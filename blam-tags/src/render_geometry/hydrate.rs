//! Synthesize author-format `raw vertices` / `raw indices` blocks
//! from an xsync-hydrated render_geometry api resource.
//!
//! On Halo 4 X360 monolithic builds the per-mesh vertex / index
//! buffers live in `cache_N` and are reached via a `tgxc` pageable
//! resource — the inline `per mesh temporary[i]` blocks are empty.
//! On MCC PC builds the same buffers are written inline under
//! `per mesh temporary[i]/{raw vertices, raw indices}` and there is
//! no xsync state.
//!
//! This module bridges the two: when [`hydrate`] sees a geometry
//! struct whose api resource is xsync-backed, it decodes the GPU
//! buffers and populates the author-format blocks so downstream
//! code (`crate::jms`, etc.) walks the same shape regardless of
//! origin.

use crate::api::{TagStruct, TagStructMut};
use crate::fields::TagFieldData;
use crate::monolithic::FixupAddress;
use crate::render_geometry::{
    decode_vertex_buffer, AuthorVertex, MeshVertexType, RenderGeometryResource,
};
use crate::TagFile;

/// Failure modes when populating author-format blocks from a GPU
/// resource. The hydration step never panics — every recoverable
/// shape mismatch surfaces here so callers can keep going or log.
#[derive(Debug)]
pub enum HydrateError {
    /// Resource header pointed at a buffer slice outside the cache
    /// block we received.
    OutOfRangeBuffer {
        which: &'static str,
        offset: usize,
        size: usize,
        available: usize,
    },
    /// Mesh referenced a vertex / index buffer index outside the
    /// resource's table.
    InvalidBufferIndex { kind: &'static str, index: i128, len: usize },
}

impl std::fmt::Display for HydrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRangeBuffer { which, offset, size, available } => write!(
                f,
                "{which} buffer offset 0x{offset:x}+{size} exceeds {available}-byte cache slice",
            ),
            Self::InvalidBufferIndex { kind, index, len } => write!(
                f, "{kind} buffer index {index} out of range (len={len})",
            ),
        }
    }
}

impl std::error::Error for HydrateError {}

/// Geometry-struct paths to inspect, keyed by tag group. Mirrors the
/// table used by the vertex-type survey, but stored as
/// struct-paths (where `api resource` and `meshes` siblings live)
/// rather than mesh-block paths.
fn geometry_struct_paths(group_tag: u32) -> &'static [&'static str] {
    match &group_tag.to_be_bytes() {
        b"mode" | b"sbsp" | b"pmdf" | b"iimz" => &["render geometry"],
        b"impo" => &["geometry"],
        b"Lbsp" => &[
            "imported geometry",
            "shadow geometry",
            "Dynamic Light Shadow Geometry",
        ],
        b"rmla" => &["atlas geometry"],
        _ => &[],
    }
}

/// Walk every geometry-shaped struct in `tag` (set by the tag's
/// group) and, when its api resource carries a hydrated
/// [`crate::monolithic::XSyncState`], rebuild
/// `per mesh temporary[i]/{raw vertices, raw indices}` from the GPU
/// buffers in `cache_bytes`.
///
/// Returns the number of meshes hydrated across all matched
/// geometry structs.  Tags without a matching geometry struct, or
/// whose resources are MCC-native (no xsync state), return `Ok(0)`.
pub fn hydrate(tag: &mut TagFile, cache_bytes: &[u8]) -> Result<usize, HydrateError> {
    let group_tag = tag.header.group_tag;
    let paths = geometry_struct_paths(group_tag);
    if paths.is_empty() {
        return Ok(0);
    }

    // Pass 1 — build a plan from an immutable walk.  Pass 2 mutates
    // the same tag, so they can't share borrows.
    let mut plans: Vec<GeometryPlan> = Vec::new();
    for path in paths {
        if let Some(plan) = build_plan(tag, path, cache_bytes)? {
            plans.push(plan);
        }
    }
    if plans.is_empty() {
        return Ok(0);
    }

    let mut hydrated = 0;
    for plan in &plans {
        hydrated += apply_plan(tag, plan)?;
    }
    Ok(hydrated)
}

/// The field every geometry struct keeps its GPU buffers behind.
///
/// Named here rather than spelled out at each call site because the converter
/// has to recognise a left-behind resource by it, and a literal in two modules
/// is a literal that drifts.
pub const API_RESOURCE_FIELD: &str = "api resource";

/// Whether every geometry struct this tag's group declares already carries
/// author-format vertex data.
///
/// `None` when the group declares no geometry struct, so a caller can tell
/// "this tag has no geometry" apart from "this tag's geometry went missing".
///
/// The question matters to anything moving a 360 tag to PC. There the buffers
/// arrive in the pageable cache and the inline blocks are empty; [`hydrate`]
/// fills them in on read, which is the shape an MCC PC tag stores natively. A
/// tag that answers `Some(true)` therefore has its geometry in hand, and its
/// api resource is only a description of where the 360 kept its copy.
///
/// Deliberately all-or-nothing: `Lbsp` declares three geometry structs, and
/// forgiving one struct's missing resource because a *different* struct
/// hydrated is exactly the silent loss this exists to catch.
pub fn author_geometry_populated(tag: &TagFile) -> Option<bool> {
    let paths = geometry_struct_paths(tag.header.group_tag);
    if paths.is_empty() {
        return None;
    }
    let root = tag.root();
    let mut seen = false;
    let mut all = true;
    for path in paths {
        let Some(rg) = root.field_path(path).and_then(|f| f.as_struct()) else {
            continue;
        };
        seen = true;
        let populated = rg
            .field("per mesh temporary")
            .and_then(|f| f.as_block())
            .is_some_and(|pmt| {
                (0..pmt.len()).filter_map(|i| pmt.element(i)).any(|elem| {
                    elem.field("raw vertices")
                        .and_then(|f| f.as_block())
                        .is_some_and(|rv| rv.len() > 0)
                })
            });
        all &= populated;
    }
    if seen { Some(all) } else { None }
}

/// Decoded geometry for one geometry struct.
struct GeometryPlan {
    path: &'static str,
    meshes: Vec<MeshPlanItem>,
}

/// Decoded geometry for one mesh.
struct MeshPlanItem {
    vertices: Vec<AuthorVertex>,
    indices: Vec<u32>,
    is_index32: bool,
}

fn build_plan(
    tag: &TagFile,
    path: &'static str,
    cache_bytes: &[u8],
) -> Result<Option<GeometryPlan>, HydrateError> {
    let root = tag.root();
    let Some(rg) = root.field_path(path).and_then(|f| f.as_struct()) else {
        return Ok(None);
    };
    let xsync = rg.field("api resource")
        .and_then(|f| f.as_resource())
        .and_then(|r| r.xsync_state());
    let Some(state) = xsync else {
        return Ok(None);
    };

    let fixed = state.apply_control_fixups();
    let root_addr = FixupAddress(state.header.root_address);
    let Some(resource) = RenderGeometryResource::parse(&fixed, root_addr) else {
        return Ok(None);
    };

    // Primary buffer = `[optional_location_offset ..
    // optional_location_offset + cache_location_size]`. The xsync
    // header field-name pairing is non-obvious — see
    // [`crate::monolithic::XSyncStateHeader`].
    let primary_offset = state.header.optional_location_offset as usize;
    let primary_size = state.header.cache_location_size as usize;
    let primary = cache_bytes
        .get(primary_offset..primary_offset + primary_size)
        .ok_or(HydrateError::OutOfRangeBuffer {
            which: "primary",
            offset: primary_offset,
            size: primary_size,
            available: cache_bytes.len(),
        })?;

    let Some(meshes) = rg.field("meshes").and_then(|f| f.as_block()) else {
        return Ok(Some(GeometryPlan { path, meshes: Vec::new() }));
    };

    // Pre-read per-mesh node maps in parallel with the mesh block.
    // Skinned vertex buffers store mesh-LOCAL node indices (0..local_max);
    // author-format JMS / MCC PC tags expect global skeleton indices.
    // We remap here so downstream JMS export Just Works.
    let node_maps = read_per_mesh_node_maps(&rg, meshes.len());

    let mut items = Vec::with_capacity(meshes.len());
    for (i, mesh) in meshes.iter().enumerate() {
        let node_map = node_maps.get(i).map(|v| v.as_slice()).unwrap_or(&[]);
        items.push(decode_mesh(&mesh, &resource, primary, node_map)?);
    }

    Ok(Some(GeometryPlan { path, meshes: items }))
}

/// Walk `render_geometry/per mesh node map[*]/node map[*]/node index`
/// into a `Vec<Vec<i16>>`. Outer index = mesh index; inner index =
/// the mesh-local node index from a skinned vertex's blend_indices
/// byte; value = the global skeleton node index. Returns an empty
/// vec if the block is absent (non-skinned tags).
fn read_per_mesh_node_maps(rg: &TagStruct<'_>, mesh_count: usize) -> Vec<Vec<i16>> {
    let mut out = vec![Vec::new(); mesh_count];
    let Some(pmnm) = rg.field("per mesh node map").and_then(|f| f.as_block()) else {
        return out;
    };
    for i in 0..pmnm.len().min(mesh_count) {
        let Some(elem) = pmnm.element(i) else { continue; };
        let Some(map) = elem.field("node map").and_then(|f| f.as_block()) else { continue; };
        let mut row = Vec::with_capacity(map.len());
        for j in 0..map.len() {
            let Some(e) = map.element(j) else { row.push(-1); continue; };
            let v = e.read_int_any("node index").unwrap_or(-1) as i64;
            row.push(v as i16);
        }
        out[i] = row;
    }
    out
}

fn decode_mesh(
    mesh: &TagStruct<'_>,
    resource: &RenderGeometryResource,
    primary: &[u8],
    node_map: &[i16],
) -> Result<MeshPlanItem, HydrateError> {
    let vbi0 = read_vbi_slot(mesh, 0);
    let ibi: i64 = mesh.read_int_any("index buffer index").unwrap_or(-1) as i64;

    // Mesh has no primary vertex buffer — empty mesh / instance
    // imposter / etc. Soft skip without checking vertex type.
    if vbi0 < 0 {
        return Ok(MeshPlanItem { vertices: Vec::new(), indices: Vec::new(), is_index32: false });
    }

    // Vertex type — schema enum → Rust enum via the option name. If
    // we have a buffer but can't resolve a decoder, panic so the
    // missing case surfaces loudly.
    let vt_name = mesh.read_enum_name("vertex type").unwrap_or_default();
    let vertex_type = MeshVertexType::from_schema_name(&vt_name).unwrap_or_else(|| {
        panic!(
            "unsupported mesh vertex type {vt_name:?} (vbi[0]={vbi0}, ibi={ibi}); \
             register it in MeshVertexType or extend the schema-name table",
        )
    });

    let vb = resource
        .xenon_vertex_buffers
        .get(vbi0 as usize)
        .ok_or(HydrateError::InvalidBufferIndex {
            kind: "vertex",
            index: vbi0 as i128,
            len: resource.xenon_vertex_buffers.len(),
        })?;
    let vbo = vb.data_address.offset() as usize;
    let vbsz = vb.data_size as usize;
    let vbytes = primary
        .get(vbo..vbo + vbsz)
        .ok_or(HydrateError::OutOfRangeBuffer {
            which: "vertex",
            offset: vbo,
            size: vbsz,
            available: primary.len(),
        })?;
    let mut vertices = decode_vertex_buffer(vertex_type, vb.vertex_count, vb.stride, vbytes)
        .unwrap_or_else(|e| panic!(
            "vertex decode failed for {vertex_type:?} (stride={}, count={}): {e}",
            vb.stride, vb.vertex_count,
        ));

    // For skinned formats: remap mesh-LOCAL blend_indices to global
    // skeleton indices via per_mesh_node_map. JMS / MCC PC tags carry
    // global indices in raw_vertices; doing the remap here keeps
    // downstream consumers unaware of the X360 indirection.
    if matches!(vertex_type, MeshVertexType::Skinned) && !node_map.is_empty() {
        for v in vertices.iter_mut() {
            for (local, _) in v.node_sets.iter_mut() {
                let li = *local as usize;
                *local = node_map.get(li).copied().unwrap_or(-1);
            }
        }
    }

    let (indices, is_index32) = if ibi < 0 {
        (Vec::new(), false)
    } else {
        let ib = resource
            .xenon_index_buffers
            .get(ibi as usize)
            .ok_or(HydrateError::InvalidBufferIndex {
                kind: "index",
                index: ibi as i128,
                len: resource.xenon_index_buffers.len(),
            })?;
        let ibo = ib.data_address.offset() as usize;
        let ibsz = ib.data_size as usize;
        let ibytes = primary
            .get(ibo..ibo + ibsz)
            .ok_or(HydrateError::OutOfRangeBuffer {
                which: "index",
                offset: ibo,
                size: ibsz,
                available: primary.len(),
            })?;
        let raw = if ib.is_index32 {
            decode_u32_be(ibytes)
        } else {
            decode_u16_be(ibytes)
        };
        // Pass indices through verbatim. The mesh's `index buffer
        // type` field tells the JMS exporter how to interpret them —
        // running strip→list here would double-convert when the
        // exporter also sees the strip flag.
        (raw, ib.is_index32)
    };

    Ok(MeshPlanItem { vertices, indices, is_index32 })
}

fn read_vbi_slot(mesh: &TagStruct<'_>, slot: usize) -> i64 {
    let Some(arr) = mesh.field("vertex buffer indices").and_then(|f| f.as_array()) else {
        return -1;
    };
    let Some(elem) = arr.element(slot) else { return -1; };
    elem.read_int_any("vertex buffer index").unwrap_or(-1) as i64
}

fn decode_u16_be(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]) as u32)
        .collect()
}

fn decode_u32_be(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn apply_plan(tag: &mut TagFile, plan: &GeometryPlan) -> Result<usize, HydrateError> {
    let mut root = tag.root_mut();
    let Some(mut rg_field) = root.field_path_mut(plan.path) else {
        return Ok(0);
    };
    let Some(mut rg) = rg_field.as_struct_mut() else {
        return Ok(0);
    };
    let Some(mut pmt_field) = rg.field_mut("per mesh temporary") else {
        return Ok(0);
    };
    let Some(mut pmt) = pmt_field.as_block_mut() else {
        return Ok(0);
    };

    let mut hydrated = 0;
    for (i, item) in plan.meshes.iter().enumerate() {
        while pmt.len() <= i {
            pmt.add_element();
        }
        let mut pmt_elem = pmt.element_mut(i).unwrap();
        write_raw_vertices(&mut pmt_elem, &item.vertices);
        write_raw_indices(&mut pmt_elem, &item.indices, item.is_index32);
        if !item.vertices.is_empty() {
            hydrated += 1;
        }
    }

    Ok(hydrated)
}

fn write_raw_vertices(pmt_elem: &mut TagStructMut<'_>, vertices: &[AuthorVertex]) {
    let Some(mut rv_field) = pmt_elem.field_mut("raw vertices") else { return; };
    let Some(mut rv) = rv_field.as_block_mut() else { return; };
    rv.clear();
    for v in vertices {
        let i = rv.add_element();
        let mut elem = rv.element_mut(i).unwrap();
        write_vertex(&mut elem, v);
    }
}

fn write_raw_indices(pmt_elem: &mut TagStructMut<'_>, indices: &[u32], is_index32: bool) {
    let (block_name, field_name) = if is_index32 {
        ("raw indices32", "dword")
    } else {
        ("raw indices", "word")
    };
    let Some(mut ri_field) = pmt_elem.field_mut(block_name) else { return; };
    let Some(mut ri) = ri_field.as_block_mut() else { return; };
    ri.clear();
    for &idx in indices {
        let i = ri.add_element();
        let mut elem = ri.element_mut(i).unwrap();
        let Some(mut f) = elem.field_mut(field_name) else { continue; };
        let value = if is_index32 {
            TagFieldData::LongInteger(idx as i32)
        } else {
            TagFieldData::ShortInteger(idx as i16)
        };
        let _ = f.set(value);
    }
}

fn write_vertex(elem: &mut TagStructMut<'_>, v: &AuthorVertex) {
    set_point3d(elem, "position", v.position);
    set_point3d(elem, "normal", v.normal);
    set_point3d(elem, "tangent", v.tangent);
    set_point3d(elem, "binormal", v.binormal);
    set_point2d(elem, "texcoord", v.texcoord);

    // node_indices array (4 × char/byte integer) and node_weights
    // array (4 × real). Use field_type to pick the right TagFieldData
    // variant; H3 declares the index as `char_integer` while H4
    // switched it to `byte_integer`.
    if let Some(mut idx_arr_field) = elem.field_mut("node indices") {
        if let Some(mut idx_arr) = idx_arr_field.as_array_mut() {
            for k in 0..idx_arr.len() {
                let idx_value = v.node_sets.get(k).map(|p| p.0).unwrap_or(0);
                let Some(mut row) = idx_arr.element_mut(k) else { continue; };
                set_int_first_field(&mut row, idx_value as i128);
            }
        }
    }
    if let Some(mut wt_arr_field) = elem.field_mut("node weights") {
        if let Some(mut wt_arr) = wt_arr_field.as_array_mut() {
            for k in 0..wt_arr.len() {
                let w = v.node_sets.get(k).map(|p| p.1).unwrap_or(0.0);
                let Some(mut row) = wt_arr.element_mut(k) else { continue; };
                set_real_first_field(&mut row, w);
            }
        }
    }
}

fn set_point3d(elem: &mut TagStructMut<'_>, name: &str, v: [f32; 3]) {
    let Some(mut f) = elem.field_mut(name) else { return; };
    let _ = f.set(TagFieldData::RealPoint3d(crate::math::RealPoint3d {
        x: v[0], y: v[1], z: v[2],
    }));
}

fn set_point2d(elem: &mut TagStructMut<'_>, name: &str, v: [f32; 2]) {
    let Some(mut f) = elem.field_mut(name) else { return; };
    let _ = f.set(TagFieldData::RealPoint2d(crate::math::RealPoint2d {
        x: v[0], y: v[1],
    }));
}

/// Inner-element of a 1-field array struct (e.g. `node_indices_array`
/// → `node index`). The field name varies by schema version so we
/// just take the first field and pick a [`TagFieldData`] variant
/// matching its declared type.
fn set_int_first_field(elem: &mut TagStructMut<'_>, value: i128) {
    let field_type = match elem.as_ref().fields().next() {
        Some(f) => (f.name().to_string(), f.field_type()),
        None => return,
    };
    let (name, ft) = field_type;
    let Some(mut f) = elem.field_mut(&name) else { return; };
    let data = match ft {
        crate::TagFieldType::CharInteger => TagFieldData::CharInteger(value as i8),
        crate::TagFieldType::ByteInteger => TagFieldData::ByteInteger(value as u8),
        crate::TagFieldType::ShortInteger => TagFieldData::ShortInteger(value as i16),
        crate::TagFieldType::WordInteger => TagFieldData::WordInteger(value as u16),
        crate::TagFieldType::LongInteger => TagFieldData::LongInteger(value as i32),
        crate::TagFieldType::DwordInteger => TagFieldData::DwordInteger(value as u32),
        crate::TagFieldType::Int64Integer => TagFieldData::Int64Integer(value as i64),
        crate::TagFieldType::QwordInteger => TagFieldData::QwordInteger(value as u64),
        _ => return,
    };
    let _ = f.set(data);
}

fn set_real_first_field(elem: &mut TagStructMut<'_>, value: f32) {
    let name = match elem.as_ref().fields().next() {
        Some(f) => f.name().to_string(),
        None => return,
    };
    let Some(mut f) = elem.field_mut(&name) else { return; };
    let _ = f.set(TagFieldData::Real(value));
}
