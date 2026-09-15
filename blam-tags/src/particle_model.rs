//! `particle_model` → JMI + per-object JMS reconstruction.
//!
//! A `particle_model` tag is, in the words of its own schema
//! explanation field, "only a shell for containing imported particle
//! geometry data". Tool builds it by merging every object listed in a
//! [`JMI`](crate::jmi) manifest into a single mesh, so extracting the
//! source means splitting that mesh back apart.
//!
//! # Where the object boundaries live
//!
//! **Gen3 (`pmdf` — Halo 3 / Reach / Halo 4).** All 266 shipped tags
//! across the three kits share one shape: exactly 1 mesh, 0 parts, no
//! materials, no nodes, `vertex type = particle_model`, compression
//! flags `0x0003`, geometry inline under `per mesh temporary[0]`.
//! Per-object index ranges live in `m_gpu_data/m_variants`, a
//! `real_vector4d[]` where
//!
//! - `[0]` is a header: `(object_count, longest_index_run, 0, 0)`
//! - `[1 + k]` is object `k`'s inclusive index range: `(first, last)`
//!
//! (Confirmed against Tool's importer and the symbolled Reach build's
//! `c_particle_model_definition::s_gpu_data::get_variants`. Verified on
//! all 266 tags: header count matches entry count and the ranges tile
//! `[0..index_count)` contiguously with no gaps.)
//!
//! **Halo 2 (`PRTM`).** A different tag entirely — a full particle
//! system definition that happens to carry geometry — and a strictly
//! richer source. Its `models[]` block stores each object's
//! `(model name, index start, index count)`, so Halo 2 recovers the
//! **original JMI object names**; gen3 discards them.
//!
//! # Two traps
//!
//! 1. **The index buffer is a triangle strip even when the schema says
//!    otherwise.** 224 of the 266 gen3 tags declare
//!    `index buffer type = DEFAULT` rather than `triangle strip`, but
//!    ~60% of tags have index counts that are not divisible by 3, which
//!    rules out a list outright. Strip is hardcoded for gen3. Halo 2,
//!    by contrast, genuinely is a triangle **list** — every model's
//!    `index count` divides by 3.
//!
//! 2. **The strip must be cut at variant boundaries.** There is no
//!    `0xFFFF` restart sentinel between objects; the variant ranges
//!    *are* the restarts. Decoding the whole buffer as one strip
//!    fabricates triangles bridging unrelated objects. Face-normal
//!    correlation against the stored vertex normals, per-variant vs.
//!    whole-buffer vs. list, over a spread of tags:
//!
//!    | tag | objs | per-variant | whole-buffer | list |
//!    |---|---|---|---|---|
//!    | `ice_shards` | 4 | **+0.884** | -0.053 | -0.028 |
//!    | `glass_fragments` | 7 | **+0.994** | +0.015 | +0.108 |
//!    | `generic_shards` | 8 | **+0.987** | +0.043 | -0.048 |
//!    | `emp_hull` | 1 | **+0.978** | +0.978 | +0.044 |
//!
//! # What cannot be recovered
//!
//! Gen3 tags carry no object names, no materials (`parts` is empty in
//! all 266) and no nodes. Names are synthesized from the tag stem and
//! a placeholder material/root node is emitted so the JMS is valid and
//! re-importable. The shipped shader is reachable only from the
//! *referencing* `particle` tag, which is outside this tag's reach —
//! except on Halo 2, whose `PRTM` owns a `shader` reference directly.

use std::collections::HashMap;

use crate::geometry::{read_compression_bounds, strip_to_list_u32, CompressionBounds, SCALE};
use crate::jmi::JmiFile;
use crate::jms::read_point_or_vec;
use crate::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};
use crate::{JmsError, JmsFile, JmsMaterial, JmsNode, JmsTriangle, JmsVertex, TagFile};
use crate::api::TagStruct;

/// A decoded particle-model vertex, in **tag space**: world units,
/// decompressed through the compression bounds, UVs unflipped. This is
/// the shared currency between the JMS exporter (which scales to
/// centimetres and flips V) and renderers (which want it as-is).
#[derive(Debug, Clone, Copy)]
pub struct ParticleVertex {
    pub position: RealPoint3d,
    pub normal: RealVector3d,
    pub texcoord: RealPoint2d,
}

/// One source object's geometry, compacted to its own vertex set.
///
/// The tag stores every object's vertices in one shared buffer; each
/// object here carries only the vertices its triangles actually
/// reference, with `indices` remapped accordingly. That keeps a
/// multi-object preview from uploading the full buffer once per object.
#[derive(Debug, Clone)]
pub struct ParticleObjectMesh {
    /// Object directory name — the JMI line, and the JMS basename.
    pub name: String,
    /// `true` when `name` was read out of the tag (Halo 2 `model name`)
    /// rather than synthesized from the tag stem (gen3, which stores no
    /// object names).
    pub name_is_authentic: bool,
    pub vertices: Vec<ParticleVertex>,
    /// Triangle-list indices into [`Self::vertices`]. Already
    /// de-stripped on gen3.
    pub indices: Vec<u32>,
    /// Shader basename, when the tag actually carries one. Halo 2's
    /// `PRTM` owns a `shader` reference; gen3's `pmdf` has no materials
    /// at all, so this stays `None` there rather than inventing a name.
    pub material: Option<String>,
}

impl ParticleObjectMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// One reconstructed source object: the name it takes in the JMI and
/// the JMS holding its geometry.
#[derive(Debug)]
pub struct ParticleModelObject {
    /// Object directory name — the JMI line, and the JMS basename.
    pub name: String,
    /// `true` when `name` was read out of the tag (Halo 2 `model name`)
    /// rather than synthesized from the tag stem (gen3, which stores no
    /// object names).
    pub name_is_authentic: bool,
    pub jms: JmsFile,
}

/// The full reconstruction: the manifest plus each object's geometry.
#[derive(Debug)]
pub struct ParticleModelSource {
    pub jmi: JmiFile,
    pub objects: Vec<ParticleModelObject>,
}

impl ParticleModelSource {
    /// `true` when every object name came out of the tag.
    pub fn names_are_authentic(&self) -> bool {
        self.objects.iter().all(|o| o.name_is_authentic)
    }
}

/// Split a `particle_model` tag back into its source objects.
///
/// `stem` is the tag's basename, used to synthesize object names on
/// gen3 (`<stem>` when there is exactly one object, `<stem>_1`…
/// `<stem>_N` otherwise). Halo 2 ignores it — those names are stored.
pub fn read_particle_model(
    tag: &TagFile,
    stem: &str,
) -> Result<ParticleModelSource, JmsError> {
    let meshes = particle_model_meshes(tag, stem)?;
    let jmi = JmiFile::new(meshes.iter().map(|m| m.name.clone()).collect());
    let objects = meshes
        .into_iter()
        .map(|m| ParticleModelObject {
            jms: mesh_to_jms(&m),
            name: m.name,
            name_is_authentic: m.name_is_authentic,
        })
        .collect();
    Ok(ParticleModelSource { jmi, objects })
}

/// Decode a `particle_model` into per-object meshes in tag space.
///
/// This is the shared decode: [`read_particle_model`] layers the JMS
/// conventions (centimetre scale, flipped V, de-indexed corners) on top,
/// while a renderer can consume these directly. `stem` supplies
/// synthesized object names on gen3 — see [`read_particle_model`].
pub fn particle_model_meshes(
    tag: &TagFile,
    stem: &str,
) -> Result<Vec<ParticleObjectMesh>, JmsError> {
    match &tag.header.group_tag.to_be_bytes() {
        b"PRTM" => read_halo2(tag),
        b"pmdf" => read_gen3(tag, stem),
        other => Err(JmsError::Unsupported(format!(
            "not a particle_model tag — group `{}`",
            String::from_utf8_lossy(other),
        ))),
    }
}

/// `true` when `group_tag` is a particle_model in either engine family.
pub fn is_particle_model_group(group_tag: u32) -> bool {
    matches!(&group_tag.to_be_bytes(), b"pmdf" | b"PRTM")
}

//================================================================================
// Gen3 — pmdf (Halo 3 / Reach / Halo 4)
//================================================================================

/// Inclusive `(first, last)` index range per object, read from
/// `m_gpu_data/m_variants[1..]`. Returns `None` when the block is
/// absent or degenerate so the caller can fall back to one object
/// spanning the whole buffer.
fn read_variant_ranges(root: &TagStruct<'_>) -> Option<Vec<(usize, usize)>> {
    let variants = root.field_path("m_gpu_data/m_variants").and_then(|f| f.as_block())?;
    if variants.len() < 2 {
        return None;
    }
    // Each element is a real_vector4d surfaced as a 4-wide array of
    // `runtime gpu_real`. Element 0 is the header; 1.. are the ranges.
    let component = |index: usize, k: usize| -> Option<f32> {
        let arr = variants.element(index)?.field("runtime m_count")?.as_array()?;
        let e = arr.element(k)?;
        match e.fields().next().and_then(|f| f.value()) {
            Some(crate::TagFieldData::Real(r)) => Some(r),
            _ => None,
        }
    };
    let mut ranges = Vec::with_capacity(variants.len() - 1);
    for i in 1..variants.len() {
        let first = component(i, 0)?;
        let last = component(i, 1)?;
        if first < 0.0 || last < first {
            return None;
        }
        ranges.push((first as usize, last as usize));
    }
    Some(ranges)
}

fn read_gen3(tag: &TagFile, stem: &str) -> Result<Vec<ParticleObjectMesh>, JmsError> {
    let root = tag.root();
    let bounds = read_compression_bounds(&root);

    let pmt = root
        .field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
        .and_then(|b| b.element(0))
        .ok_or(JmsError::MissingField("render geometry/per mesh temporary[0]"))?;

    let raw_v = pmt
        .field("raw vertices")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("per mesh temporary[0]/raw vertices"))?;

    // `raw indices` is u16; `raw indices32` is the parallel wide slot.
    // Widen both to u32 — JmsTriangle indexes with u32 already.
    let indices: Vec<u32> = match (
        pmt.field("raw indices").and_then(|f| f.as_block()).filter(|b| !b.is_empty()),
        pmt.field("raw indices32").and_then(|f| f.as_block()).filter(|b| !b.is_empty()),
    ) {
        (Some(b), _) => (0..b.len())
            .filter_map(|k| b.element(k))
            .map(|e| e.read_int_any("word").unwrap_or(0) as u32 & 0xFFFF)
            .collect(),
        (_, Some(b)) => (0..b.len())
            .filter_map(|k| b.element(k))
            .map(|e| e.read_int_any("dword").unwrap_or(0) as u32)
            .collect(),
        _ => return Err(JmsError::MissingField("per mesh temporary[0]/raw indices")),
    };

    // No variants (or a malformed block) → treat the whole buffer as a
    // single object rather than dropping the geometry on the floor.
    let ranges = read_variant_ranges(&root)
        .unwrap_or_else(|| vec![(0, indices.len().saturating_sub(1))]);

    let names = synthesize_names(stem, ranges.len());

    let mut out = Vec::with_capacity(ranges.len());
    for ((first, last), name) in ranges.into_iter().zip(names) {
        if first >= indices.len() {
            continue;
        }
        let end = (last + 1).min(indices.len());
        // Gen3 is always a strip — the `index buffer type` enum reads
        // DEFAULT on most tags and cannot be trusted (see module docs).
        let tris = strip_to_list_u32(&indices[first..end]);
        out.push(compact(name, false, &tris, |vi| {
            raw_v.element(vi as usize).map(|v| read_gen3_vertex(&v, &bounds))
        }));
    }
    Ok(out)
}

fn read_gen3_vertex(v: &TagStruct<'_>, bounds: &CompressionBounds) -> ParticleVertex {
    ParticleVertex {
        position: bounds.decompress_position(read_point_or_vec(v, "position")),
        // Gen3 declares `normal` as real_point_3d despite it being a
        // direction; Halo 2 declares the same field as real_vector_3d.
        // The tolerant reader covers both so neither panics on the
        // other's schema.
        normal: read_point_or_vec(v, "normal").as_vector(),
        texcoord: bounds.decompress_texcoord(v.read_point2d("texcoord")),
    }
}

/// Gen3 stores no object names. One object takes the tag stem; several
/// get `_1`…`_N` suffixes, matching the shipped Halo 2 naming style
/// (`can_1`, `alder_2`).
fn synthesize_names(stem: &str, count: usize) -> Vec<String> {
    if count == 1 {
        vec![stem.to_owned()]
    } else {
        (1..=count).map(|i| format!("{stem}_{i}")).collect()
    }
}

//================================================================================
// Halo 2 — PRTM
//================================================================================

fn read_halo2(tag: &TagFile) -> Result<Vec<ParticleObjectMesh>, JmsError> {
    let root = tag.root();

    let models = root
        .field("models")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("models"))?;
    let raw_v = root
        .field("raw vertices")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("raw vertices"))?;
    let idx_block = root
        .field("indices")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("indices"))?;

    let indices: Vec<u32> = (0..idx_block.len())
        .filter_map(|k| idx_block.element(k))
        .map(|e| e.read_int_any("index").unwrap_or(0) as u32 & 0xFFFF)
        .collect();

    // PRTM owns its shader reference directly, so the material slot can
    // carry a real name here — unlike gen3, where it is a placeholder.
    let shader = root
        .read_tag_ref_path("shader")
        .and_then(|p| {
            p.replace('\\', "/")
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "default".to_owned());

    let mut out = Vec::with_capacity(models.len());
    let mut seen: HashMap<String, usize> = HashMap::new();
    for mi in 0..models.len() {
        let m = models.element(mi).unwrap();
        let start = m.read_int_any("index start").unwrap_or(0).max(0) as usize;
        let count = m.read_int_any("index count").unwrap_or(0).max(0) as usize;
        if count == 0 || start >= indices.len() {
            continue;
        }
        let end = (start + count).min(indices.len());

        let raw_name = m.read_string_id("model name").unwrap_or_default();
        let authentic = !raw_name.is_empty();
        let base = if authentic { raw_name } else { format!("model_{}", mi + 1) };
        // Two models sharing a name would collide on disk (same
        // directory, same JMS) and silently lose geometry.
        let name = match seen.entry(base.clone()) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                *e.get_mut() += 1;
                format!("{base}_{}", e.get())
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(1);
                base
            }
        };

        // Halo 2 indices are a triangle LIST — every shipped model's
        // `index count` divides by 3, and list decoding scores a 0.966
        // mean face-normal correlation across all 40 tags.
        let tris: Vec<(u32, u32, u32)> = indices[start..end]
            .chunks_exact(3)
            .map(|c| (c[0], c[1], c[2]))
            .collect();

        let mut mesh = compact(name, authentic, &tris, |vi| {
            raw_v.element(vi as usize).map(|v| read_halo2_vertex(&v))
        });
        mesh.material = Some(shader.clone());
        out.push(mesh);
    }
    Ok(out)
}

/// Halo 2 `raw vertices[]` are already world-unit floats — the tag has
/// no compression bounds to undo.
fn read_halo2_vertex(v: &TagStruct<'_>) -> ParticleVertex {
    ParticleVertex {
        position: read_point_or_vec(v, "position"),
        normal: read_point_or_vec(v, "normal").as_vector(),
        texcoord: v.read_point2d("texcoord"),
    }
}

//================================================================================
// Compaction + JMS assembly
//================================================================================

/// Build one object's mesh from a triangle list plus a vertex reader
/// over the tag's **shared** vertex buffer, keeping only the vertices
/// this object references and remapping indices to the compact set.
///
/// A triangle whose corner is out of range is dropped whole — a partial
/// triangle would render as garbage.
fn compact<F>(
    name: String,
    name_is_authentic: bool,
    tris: &[(u32, u32, u32)],
    mut vertex: F,
) -> ParticleObjectMesh
where
    F: FnMut(u32) -> Option<ParticleVertex>,
{
    let mut vertices: Vec<ParticleVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::with_capacity(tris.len() * 3);
    let mut remap: HashMap<u32, u32> = HashMap::new();

    for &(a, b, c) in tris {
        let mut corners = [0u32; 3];
        let mut ok = true;
        for (slot, source) in corners.iter_mut().zip([a, b, c]) {
            match remap.get(&source) {
                Some(&local) => *slot = local,
                None => {
                    let Some(v) = vertex(source) else {
                        ok = false;
                        break;
                    };
                    let local = vertices.len() as u32;
                    vertices.push(v);
                    remap.insert(source, local);
                    *slot = local;
                }
            }
        }
        if ok {
            indices.extend_from_slice(&corners);
        }
    }

    ParticleObjectMesh { name, name_is_authentic, vertices, indices, material: None }
}

/// Convert a decoded object into a JMS, applying the format's
/// conventions: centimetre scale, flipped V, and de-indexed corners
/// (the same per-corner shape the render_model exporter emits).
///
/// A particle model has no skeleton, so a single root `frame` node is
/// synthesized; gen3 has no materials either, so its slot falls back to
/// `default`. Both sections are mandatory for the JMS to parse.
fn mesh_to_jms(mesh: &ParticleObjectMesh) -> JmsFile {
    let nodes = vec![JmsNode {
        name: "frame".to_owned(),
        parent: -1,
        rotation: RealQuaternion::IDENTITY,
        translation: RealPoint3d::ZERO,
    }];
    let materials = vec![JmsMaterial {
        name: mesh.material.clone().unwrap_or_else(|| "default".to_owned()),
        material_name: format!("(1) {}", mesh.name),
    }];

    let to_jms_vertex = |v: &ParticleVertex| JmsVertex {
        position: v.position * SCALE,
        normal: v.normal,
        node_sets: vec![(0, 1.0)],
        tangent: None,
        binormal: None,
        uvs: vec![RealPoint2d { x: v.texcoord.x, y: 1.0 - v.texcoord.y }],
    };

    let mut vertices: Vec<JmsVertex> = Vec::with_capacity(mesh.indices.len());
    let mut triangles: Vec<JmsTriangle> = Vec::with_capacity(mesh.triangle_count());
    for corners in mesh.indices.chunks_exact(3) {
        let base = vertices.len() as u32;
        for &i in corners {
            // `compact` guarantees every index is in range.
            vertices.push(to_jms_vertex(&mesh.vertices[i as usize]));
        }
        triangles.push(JmsTriangle { material: 0, v: [base, base + 1, base + 2], region: 0 });
    }

    JmsFile { nodes, materials, vertices, triangles, ..Default::default() }
}
