//! Runtime-shaped extraction of `render_model` (mode) tag geometry.
//!
//! Sibling to [`crate::jms`], targeting renderer / engine consumers
//! rather than the JMS text format. Differences from `jms::JmsFile`:
//!
//! - **Per-mesh, not material-flattened.** Each `meshes[i]` becomes
//!   one [`RenderMesh`] with its own vertex+index buffer plus a
//!   `parts` list. Renderers want one draw call per part; the JMS
//!   path collapses everything into a single per-material vertex pool.
//! - **Native units, no ×100.** Positions stay in Halo world units;
//!   the consumer scales to whatever its scene units are.
//! - **Triangle list, not strip.** Strips are decoded once at
//!   extraction time so consumers don't carry the restart-sentinel
//!   logic.
//! - **Unflipped UVs.** V is left as-stored. Engines using either
//!   convention can flip (or not) at upload time.
//! - **Local-space node transforms.** Parent-relative TRS is preserved
//!   so the consumer can either chain-to-world for a static bind pose
//!   or feed the locals into a runtime animation system.
//! - **Fixed-size 4-bone skin.** `node_indices`/`node_weights` are
//!   `[u8; 4]`/`[f32; 4]` zero-padded — what GPU vertex layouts
//!   universally expect.
//! - **Variant/permutation selection deferred.** All meshes are
//!   extracted; the consumer filters via [`RenderRegion`] +
//!   [`RenderPermutation`] (or via the `.model` (hlmt) variant block).
//!
//! Targets H3 / Reach MCC tags where every render mesh stores its
//! buffers inline under `render geometry/per mesh temporary[i]`. Cache
//! map files would need a different code path.

use crate::api::{TagBlock, TagStruct};
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::geometry::{read_compression_bounds, strip_to_list, CompressionBounds};
use crate::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};

/// Errors from runtime render_model extraction.
#[derive(Debug)]
pub enum RenderModelError {
    /// A required field was missing from the tag — schema mismatch
    /// or the field was empty in the instance. Carries the dotted
    /// field path.
    MissingField(&'static str),
}

impl std::fmt::Display for RenderModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(p) => write!(f, "render_model is missing required field: {p}"),
        }
    }
}

impl std::error::Error for RenderModelError {}

/// One bone in the render_model skeleton, in **parent-relative**
/// (local) bind-pose. Forward-chain through `parent_index` to get
/// world-space if you need it. `parent_index = -1` for roots.
#[derive(Debug, Clone)]
pub struct RenderNode {
    pub name: String,
    pub parent_index: i16,
    pub default_translation: RealPoint3d,
    pub default_rotation: RealQuaternion,
}

/// One entry from the `materials` block. v1 consumers stub a default
/// material per [`Self::shader_name`]; later passes can resolve
/// [`Self::shader_path`] to load the real `render_method` tag.
#[derive(Debug, Clone)]
pub struct RenderMaterial {
    /// Shader basename (filename without extension). Stable enough for
    /// dedupe / default-material keying.
    pub shader_name: String,
    /// Full Halo-style relative path to the shader tag (e.g.
    /// `objects\foo\shaders\foo_diffuse`). Empty if the tag_ref was
    /// null. NO file extension — caller composes via [`Self::shader_extension`].
    pub shader_path: String,
    /// Group tag FOURCC of the referenced shader — `rmsh` (regular
    /// shader), `rmtr` (terrain), `rmw ` (water), `rmfl` (foliage),
    /// etc. Determines which file extension to append to
    /// `shader_path` and which schema to expect when parsing.
    /// Zero when the tag_ref was null.
    pub shader_group_tag: u32,
}

impl RenderMaterial {
    /// File extension matching [`Self::shader_group_tag`] — e.g.
    /// `"shader_terrain"` for `rmtr`. Pair with `shader_path` and
    /// `paths::resolve_tag_path` to locate the on-disk tag file.
    pub fn shader_extension(&self) -> &'static str {
        crate::paths::group_tag_to_extension(self.shader_group_tag).unwrap_or("shader")
    }
}

/// Region — collection of permutations sharing a name (`body`,
/// `head`, etc.). Variant selection in `.model` (hlmt) picks one
/// permutation per region; v1 consumers can pick permutation 0.
#[derive(Debug, Clone)]
pub struct RenderRegion {
    pub name: String,
    pub permutations: Vec<RenderPermutation>,
}

/// One choice within a region (intact / damaged / color variant /
/// etc.). Resolves to a contiguous slice of meshes via
/// `[mesh_index .. mesh_index + mesh_count)`.
#[derive(Debug, Clone)]
pub struct RenderPermutation {
    pub name: String,
    pub mesh_index: i16,
    pub mesh_count: i16,
}

/// `mesh_transfer_vertex_type_definition` (schema enum on `meshes[i]`,
/// field name `PRT vertex type*`). Selects which PRT entry point the
/// engine remaps to at `render_mesh_part_default @ 0x18069EBC0` via
/// `entry_point_remapping_0[transfer_vector_vertex_type]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum PrtVertexType {
    #[default]
    None = 0,
    Ambient = 1,
    Linear = 2,
    Quadratic = 3,
}

impl PrtVertexType {
    pub fn from_raw(v: i32) -> Self {
        match v {
            1 => Self::Ambient,
            2 => Self::Linear,
            3 => Self::Quadratic,
            _ => Self::None,
        }
    }
    pub fn is_some(self) -> bool { !matches!(self, Self::None) }
}

/// One mesh from `render geometry/meshes[i]`. Index in the parent
/// [`RenderModel::meshes`] vec matches the `mesh_index` stored in
/// permutations.
#[derive(Debug, Clone)]
pub struct RenderMesh {
    pub vertices: Vec<RenderVertex>,
    /// Triangle-list indices into [`Self::vertices`]. Strips are
    /// already decoded.
    pub indices: Vec<u32>,
    pub parts: Vec<RenderMeshPart>,
    /// For rigid meshes (`vertex type = rigid` / `rigid_boned`), the
    /// single bone all vertices are bound to. `None` for skinned
    /// meshes whose vertices carry their own per-vertex weights.
    pub rigid_node_index: Option<i16>,
    /// `s_per_mesh_raw_data.raw_water_data` — per-mesh extra data for
    /// water surfaces. `Some` when the mesh contains at least one part
    /// with `_part_is_water_surface` set; `None` for non-water meshes.
    /// Per-vertex `local_info` + `base_texcoord` are appended onto the
    /// regular `vertices` pool (sequential indexing — see
    /// [`RawWaterData::indices`]).
    pub water_data: Option<RawWaterData>,
    /// `meshes[i].PRT vertex type` — author-declared PRT variant. Only
    /// `Ambient` appears in the sampled MCC H3 corpus.
    pub prt_vertex_type: PrtVertexType,
    /// True iff `meshes[i].vertex_buffer_indices[3] != 0xFFFF`, i.e. a
    /// runtime PRT vertex buffer is present. Mirrors the engine check
    /// at `select_instance_entry_point @ 0x180691340` and
    /// `render_mesh_part_default @ 0x18069EBC0`. Required (alongside
    /// `structure_instance.lightmapping_policy == 2`) to activate the
    /// PRT entry-point path. See
    /// `project_research_per_mesh_prt_2026_05_11.md`.
    pub has_prt_vertex_stream: bool,
    /// Pre-baked PRT Ambient per-vertex transfer scalar (grayscale).
    /// One `f32` per vertex; matches MCC PC vertex declaration
    /// `transfer_prt_ambient_only_elements` (`R32_FLOAT` `BLENDWEIGHT1`
    /// slot 2; Ares `rasterizer_resource_definitions.cpp:46`). Empty
    /// when the mesh declares no PRT or its `mesh pca data` blob is
    /// missing / size-mismatched.
    ///
    /// Source: `per_mesh_prt_data[i].mesh_pca_data` carries 3 floats
    /// per vertex (RGB transfer); Reach `create_prt_vertex_buffer @
    /// 0x82E080F0` averages to grayscale (Reach quantizes to 1 byte;
    /// MCC keeps the float). When this stream is non-empty AND
    /// `lightmapping_policy == 2` on the instance, the engine routes
    /// the draw via `_entry_point_static_lighting_prt_quadratic`
    /// (remapped to the actual variant by `render_mesh_part_default`).
    pub prt_ambient_stream: Vec<f32>,
    /// `mesh->flags & _mesh_has_vertex_color_bit` (Ares
    /// `geometry_definitions.h:15`). Engine signal that the mesh
    /// carries the `_vertex_buffer_usage_vert_color` stream; consumed
    /// by `render_mesh_part_default @ 0x18069EBC0` to remap the entry
    /// point from `_entry_point_static_lighting_prt_quadratic` to
    /// `_entry_point_vertex_color_lighting` (idx=14, sky shader path).
    /// Tag field is `s_mesh.flags` at offset 0x2C.
    pub has_vertex_color: bool,
}

/// Per-mesh water-surface data, fully resolved at parse time. Each
/// triangle's 3 control points carry (regular_idx, water_idx) pairs
/// already de-referenced through `raw_indices` and `raw_water_indices`.
/// Mirrors the cache-build walk in
/// `?create_mesh_water_vertex_buffer @ 0x82e094e8` (Reach XEX) — see
/// `reference_water_vertex_buffer_build.md`.
///
/// At runtime, control point N's:
/// - position / texcoord / tangent / binormal / lightmap_uv comes from
///   `RenderMesh::vertices[control_point.regular_idx]`.
/// - local_info / water_velocity / base_texcoord comes from
///   `RawWaterData::vertices[control_point.water_idx]`.
#[derive(Debug, Clone, Default)]
pub struct RawWaterData {
    /// One entry per source water triangle. Ordered by source part —
    /// each part's triangles are contiguous (see [`Self::parts`]).
    pub triangles: Vec<RawWaterTriangle>,
    /// `raw water vertices` — per-water-vertex append pool.
    /// `RawWaterControlPoint::water_idx` indexes into this.
    pub vertices: Vec<RawWaterAppend>,
    /// Per-part triangle ranges within [`Self::triangles`]. Each entry
    /// indexes into [`RenderMesh::parts`] and gives the
    /// `(triangle_start, triangle_count)` slice — used by the renderer
    /// to dispatch per-rmw-material draws (different water parts on a
    /// mesh can carry different rmw materials with different option
    /// vectors → different pipelines). Engine equivalent: each part is
    /// its own iteration of `c_water_renderer::render_water_part`.
    pub parts: Vec<RawWaterPart>,
}

/// One water-flagged part's triangle range within [`RawWaterData::triangles`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterPart {
    /// Index into [`RenderMesh::parts`] — gives the rmw material.
    pub mesh_part_index: u16,
    /// Start triangle in `RawWaterData::triangles` (inclusive).
    pub triangle_start: u32,
    /// Number of triangles in this part.
    pub triangle_count: u32,
}

/// One source water triangle — 3 control points each pulling from
/// two parallel pools.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterTriangle {
    pub control_points: [RawWaterControlPoint; 3],
}

/// One control point in a water triangle. The two indices reference
/// parallel pools per the Reach `create_mesh_water_vertex_buffer`
/// walk: `regular_idx = raw_indices[part.index_start + j]`,
/// `water_idx = raw_water_indices[mesh.water_indices_start[part_idx] + j]`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterControlPoint {
    pub regular_idx: u16,
    pub water_idx: u16,
}

/// `s_raw_water_append` (36 bytes on disk) — extra per-vertex data for
/// water surfaces. Three `real_point_3d` fields read by the water VS:
/// - `local_info` → `s_water_render_vertex.local_info` — feeds foam
///   height + paint sampling.
/// - `water_velocity` → flow-direction sampling for animated wave
///   displacement (Phase A7).
/// - `base_texcoord` → `s_water_render_vertex.base_tex` — UV for the
///   watercolor / foam / global_shape textures.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterAppend {
    pub local_info: RealPoint3d,
    pub water_velocity: RealPoint3d,
    pub base_texcoord: RealPoint3d,
}

/// Decompressed vertex from `raw_vertex_block`. UV is **unflipped**
/// (caller decides V convention). `node_indices`/`node_weights` are
/// zero-padded to 4; sum of weights is `1.0` for skinned vertices,
/// or zero-weighted with [`RenderMesh::rigid_node_index`] carrying
/// the bone for rigid meshes.
///
/// `tangent` and `binormal` come from raw_vertex's same-named fields
/// (`real point 3d`). Both are zero on tags that lack tangent-space
/// data — callers needing a normal-mapping basis should fall back to
/// an orthogonal stand-in when this happens.
#[derive(Debug, Clone, Copy)]
pub struct RenderVertex {
    pub position: RealPoint3d,
    pub texcoord: RealPoint2d,
    pub normal: RealVector3d,
    pub tangent: RealVector3d,
    pub binormal: RealVector3d,
    pub node_indices: [u8; 4],
    pub node_weights: [f32; 4],
    /// `raw_vertex.lightmap texcoord` — the per-vertex lightmap UV.
    /// Zero in `scenario_structure_bsp` (`render geometry`) — the SBSP
    /// tag's vertices have this slot present but un-set. The actual
    /// lightmap UVs live in the per-BSP **lightmap** tag's parallel
    /// `imported geometry/per_mesh_temporary[i]/raw_vertices[k]`,
    /// vertex-aligned 1:1 with the SBSP. Callers needing real lightmap
    /// UVs should walk the lightmap tag's geometry and zip with the
    /// SBSP vertices on `(mesh_index, vertex_index)`.
    pub lightmap_texcoord: RealPoint2d,
    /// `raw_vertex.vert_color` (Ares `RawVertex.vert_color @ 0x54`,
    /// `render_geometry.rs:155`). Per-vertex baked color used by sky
    /// `.render_model` meshes — engine binds this as the
    /// `_vertex_buffer_usage_vert_color` stream
    /// (geometry_definitions.h:27) for the
    /// `_entry_point_vertex_color_lighting` (idx=14) draw path. Zero on
    /// meshes that don't have `_mesh_has_vertex_color_bit` set.
    pub vert_color: RealVector3d,
}

/// One draw range within a [`RenderMesh`]. `material_index` indexes
/// into [`RenderModel::materials`].
#[derive(Debug, Clone, Copy)]
pub struct RenderMeshPart {
    pub material_index: u16,
    pub index_start: u32,
    pub index_count: u32,
    /// `e_geometry_part_type` enum (Ares
    /// `geometry_definitions_new.h:25`):
    /// 0=opaque_not_drawn, 1=opaque_shadow_only,
    /// 2=opaque_shadow_casting, 3=opaque_non_shadowing,
    /// 4=transparent, 5=lightmap_only.
    pub part_type: i8,
}

/// One entry in a `render_model`'s `instance placements` block — a
/// named transform that the engine resolves by string-id from the
/// owning content (e.g., `s_decorator_set::render_model_instance_names`
/// → match name → instance_placement → node_index + per-subpart
/// transform). Decorator render_models use this to map per-type
/// `decorator_types[k].mesh` (block index into the decorator_set's
/// names list) to which subpart of the single concatenated mesh to
/// draw for that type.
///
/// Index in the parent `instance_placements` vec aligns with subpart
/// index in `meshes[0].parts[0].subparts[]` — i.e.,
/// `instance_placements[i]` describes subpart `i`'s transform.
#[derive(Debug, Clone)]
pub struct RenderInstancePlacement {
    pub name: String,
    pub node_index: i16,
    pub scale: f32,
    pub forward: RealVector3d,
    pub left: RealVector3d,
    pub up: RealVector3d,
    pub position: RealPoint3d,
}

/// One sub-strip within a `render_model.meshes[i].parts[j]`. The engine
/// uses these for decorator multi-type rendering: each subpart is a
/// triangle-strip slice of the part's index pool, drawn for one
/// decorator type. Slice = `raw_indices[index_start..index_start +
/// index_count]`. `budget_vertex_count` is the number of unique
/// vertices the strip references — diagnostic only at runtime.
#[derive(Debug, Clone, Copy)]
pub struct RenderMeshSubpart {
    pub index_start: u32,
    pub index_count: u32,
    pub budget_vertex_count: u16,
}

/// Per-mesh raw-strip + per-subpart slices, as read straight from the
/// tag without the per-part `strip_to_list` decoding that
/// `extract_render_geometry_meshes` applies. Used by decorator loaders
/// that need per-subpart triangle-list slices (each subpart = one
/// decorator type's geometry). The vertex pool is shared across all
/// subparts; `subpart_indices[k]` is a triangle-list decoded from
/// the strip slice for subpart `k`.
///
/// Returned by [`extract_decorator_subparts`].
#[derive(Debug, Clone, Default)]
pub struct DecoratorSubpartGeometry {
    pub vertices: Vec<RenderVertex>,
    /// Per-subpart triangle-list (each tuple is 3 vertex indices into
    /// `vertices`). Length = number of subparts = `instance_placements.len()`.
    pub subpart_indices: Vec<Vec<u32>>,
}

/// One marker (attachment point). `region_index`/`permutation_index`
/// are `-1` when the marker is unconstrained. Transform is in
/// node-local space (relative to [`Self::node_index`]).
#[derive(Debug, Clone)]
pub struct RenderMarker {
    pub name: String,
    pub region_index: i8,
    pub permutation_index: i8,
    pub node_index: i8,
    pub translation: RealPoint3d,
    pub rotation: RealQuaternion,
    pub scale: f32,
}

/// Decoded render_model in the shape a renderer consumes. Index in
/// [`Self::meshes`] aligns 1:1 with `mode/render geometry/meshes[i]`,
/// so [`RenderPermutation::mesh_index`] is a direct slice into it.
#[derive(Debug, Clone, Default)]
pub struct RenderModel {
    pub nodes: Vec<RenderNode>,
    pub materials: Vec<RenderMaterial>,
    pub regions: Vec<RenderRegion>,
    pub meshes: Vec<RenderMesh>,
    pub markers: Vec<RenderMarker>,
    /// `instance placements` block — named transforms that decorator
    /// render_models use as the resolution target for their per-type
    /// `decorator_types[k].mesh` block index (which goes through
    /// `s_decorator_set::render_model_instance_names` → string_id →
    /// match on `instance_placements[i].name`). Empty for non-decorator
    /// render_models. Index aligns 1:1 with the subparts of
    /// `meshes[0].parts[0]`.
    pub instance_placements: Vec<RenderInstancePlacement>,
    /// `sky lights` block — area-light samples used by sky-tag
    /// render_models. The LAST entry is conventionally the dominant
    /// sun (`get_sun_constants_from_sky @ 0x1803adcb0` reads
    /// `lightgen_lights[count-1]`). Empty for non-sky models.
    pub sky_lights: Vec<SkyLight>,
    /// `default lightprobe r/g/b` — SH3 coefficients (9 floats per
    /// channel; on-disk array is 16, zero-padded). Halo's
    /// `setup_default_lighting` reads this when the per-instance
    /// lightmap chain misses. Empty (or all-zero) for non-sky models.
    pub default_lightprobe: Option<DefaultLightprobe>,
}

/// One entry from the render_model's `sky lights` block. 28 bytes on
/// disk: direction (12) + intensity (12) + solid_angle (4). Mirrors
/// `s_sky_gen_light` in dllcache.
#[derive(Debug, Clone, Copy)]
pub struct SkyLight {
    /// World-space direction TO the light.
    pub direction: RealVector3d,
    /// Linear-space radiant intensity per channel (HDR — sun entries
    /// can be tens of thousands).
    pub intensity: RealVector3d,
    /// Light's solid angle (steradians). Halo's runtime multiplies
    /// `intensity * solid_angle * 0.2 * g_render_light_intensity` to
    /// get the rendered sun radiance.
    pub solid_angle: f32,
}

/// `default lightprobe r/g/b` — three 9-float SH3 coefficient sets
/// (the on-disk arrays are 16 floats; we drop the trailing zero pad).
/// Read by `setup_default_lighting` as the deepest sky-probe fallback.
#[derive(Debug, Clone, Default)]
pub struct DefaultLightprobe {
    pub r: [f32; 9],
    pub g: [f32; 9],
    pub b: [f32; 9],
}

impl RenderModel {
    /// Walk a parsed `render_model` (mode) tag and decode every mesh,
    /// node, material, region, and marker. Variant filtering is the
    /// caller's job — see [`RenderRegion`] and the `.model` (hlmt)
    /// variant block.
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderModelError> {
        let root = tag.root();
        let bounds = read_compression_bounds(&root);
        Ok(Self {
            nodes: read_nodes(&root)?,
            materials: read_materials(&root)?,
            regions: read_regions(&root)?,
            meshes: read_meshes(&root, &bounds)?,
            markers: read_markers(&root)?,
            instance_placements: read_instance_placements(&root),
            sky_lights: read_sky_lights(&root),
            default_lightprobe: read_default_lightprobe(&root),
        })
    }
}

/// Walk `instance placements` block. Empty for non-decorator
/// render_models. Each entry's `name` matches a `string_id` in the
/// owning decorator_set's `render_model_instance_names` block — that's
/// the resolution chain `decorator_types[k].mesh → name → instance_placements`.
fn read_instance_placements(root: &TagStruct<'_>) -> Vec<RenderInstancePlacement> {
    let Some(block) = root.field_path("instance placements").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(elem) = block.element(i) else { continue };
        out.push(RenderInstancePlacement {
            name: elem.read_string_id("name").unwrap_or_default(),
            node_index: elem.read_block_index("node_index"),
            scale: elem.read_real("scale").unwrap_or(1.0),
            forward: read_real_vector3d(&elem, "forward").unwrap_or(RealVector3d { i: 1.0, j: 0.0, k: 0.0 }),
            left: read_real_vector3d(&elem, "left").unwrap_or(RealVector3d { i: 0.0, j: 1.0, k: 0.0 }),
            up: read_real_vector3d(&elem, "up").unwrap_or(RealVector3d { i: 0.0, j: 0.0, k: 1.0 }),
            position: elem.read_point3d("position"),
        });
    }
    out
}

/// Decorator-specific extractor: walk `render geometry/meshes[0]/parts[0]/subparts[]`
/// and produce per-subpart triangle-lists (each strip-decoded
/// independently of the others, so degenerate-stitching triangles
/// between subparts don't pollute any one subpart's list). Each
/// triangle is 3 indices into the shared vertex pool.
///
/// Engine equivalent: `c_structure_renderer::render_decorators @
/// 0x1806901A0`'s per-subpart draw loop. The engine actually does an
/// unindexed strip draw against a cache-built expanded vertex buffer
/// (`start_vertex = s × subpart[0].index_count`), but for an indexed
/// pipeline we slice the index pool by subpart and let the
/// rasterizer fetch the full vertex buffer through normal indexing.
///
/// Returns `None` if the tag isn't shaped like a decorator
/// render_model (no per-mesh-temporary, no parts, no subparts, etc).
pub fn extract_decorator_subparts(
    tag: &TagFile,
) -> Option<DecoratorSubpartGeometry> {
    let root = tag.root();
    let bounds = read_compression_bounds(&root);

    let pmt = root.field_path("render geometry/per mesh temporary").and_then(|f| f.as_block())?;
    let meshes = root.field_path("render geometry/meshes").and_then(|f| f.as_block())?;
    if meshes.is_empty() || pmt.is_empty() {
        return None;
    }

    // Decorator render_models conventionally have one mesh + one part.
    // We only walk meshes[0]/parts[0]; other shapes are caller error.
    let mesh = meshes.element(0)?;
    let pmt0 = pmt.element(0)?;

    let raw_v = pmt0.field("raw vertices").and_then(|f| f.as_block())?;
    let raw_i = pmt0.field("raw indices").and_then(|f| f.as_block())?;

    // Decode every raw vertex once (same path as `read_meshes_per_mesh`).
    let mut vertices: Vec<RenderVertex> = Vec::with_capacity(raw_v.len());
    for k in 0..raw_v.len() {
        let v = raw_v.element(k)?;
        // Decorators are unskinned (single-bone via instance_placements
        // node_index); per-vertex node_indices are zero — pass None.
        vertices.push(read_vertex(&v, &bounds, None));
    }
    // Flatten raw indices into a u16 pool.
    let raw_index_list: Vec<u16> = (0..raw_i.len())
        .filter_map(|k| raw_i.element(k))
        .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
        .collect();

    // Walk the mesh's `subparts` block. NOTE: in `render geometry/
    // meshes[i]`, `subparts` is a SIBLING of `parts` (not nested under
    // `parts[0]`) — the per-part `subpart_start` / `subpart_count`
    // fields slice into this mesh-level block. For decorators (one
    // part per mesh) the whole `subparts` block belongs to that part.
    // Falls back to one synthetic subpart spanning the whole index
    // pool when the field is absent (e.g., non-decorator render_models).
    let mut subpart_indices: Vec<Vec<u32>> = Vec::new();
    if let Some(subparts) = mesh.field("subparts").and_then(|f| f.as_block()) {
        for k in 0..subparts.len() {
            let Some(sp) = subparts.element(k) else { continue };
            let start = sp.read_int_any("index start").unwrap_or(0) as i32;
            let count = sp.read_int_any("index count").unwrap_or(0) as i32;
            let start = (start as i16 as u16) as usize;
            let count = count.max(0) as usize;
            if count == 0 {
                subpart_indices.push(Vec::new());
                continue;
            }
            let end = (start + count).min(raw_index_list.len());
            let strip = &raw_index_list[start..end];
            let tris = crate::geometry::strip_to_list(strip);
            let mut flat = Vec::with_capacity(tris.len() * 3);
            for (a, b, c) in tris {
                flat.push(a as u32);
                flat.push(b as u32);
                flat.push(c as u32);
            }
            subpart_indices.push(flat);
        }
    } else {
        // Single synthetic subpart spanning all indices.
        let tris = crate::geometry::strip_to_list(&raw_index_list);
        let mut flat = Vec::with_capacity(tris.len() * 3);
        for (a, b, c) in tris {
            flat.push(a as u32); flat.push(b as u32); flat.push(c as u32);
        }
        subpart_indices.push(flat);
    }

    Some(DecoratorSubpartGeometry { vertices, subpart_indices })
}

/// Walk the `sky lights` block. Field name has a space in the tag
/// schema; mirrors the `s_sky_gen_light` runtime struct (28 bytes).
fn read_sky_lights(root: &TagStruct<'_>) -> Vec<SkyLight> {
    let Some(block) = root.field("sky lights").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(elem) = block.element(i) else { continue };
        let direction = read_real_vector3d(&elem, "direction").unwrap_or(RealVector3d { i: 0.0, j: 0.0, k: 1.0 });
        let intensity = read_real_vector3d(&elem, "intensity").unwrap_or(RealVector3d { i: 0.0, j: 0.0, k: 0.0 });
        let solid_angle = elem.read_real("solid angle").unwrap_or(0.0);
        out.push(SkyLight { direction, intensity, solid_angle });
    }
    out
}

/// Read the `default lightprobe r/g/b` arrays, returning `None` when
/// any channel is missing or empty. The on-disk format is a 16-element
/// `array` of structs each containing one `coefficient: real` field; we
/// extract the first 9 and discard the trailing zero pad.
fn read_default_lightprobe(root: &TagStruct<'_>) -> Option<DefaultLightprobe> {
    fn read_channel(root: &TagStruct<'_>, name: &str) -> Option<[f32; 9]> {
        let arr = root.field(name)?.as_array()?;
        let mut out = [0.0f32; 9];
        let n = arr.len().min(9);
        for i in 0..n {
            let elem = arr.element(i)?;
            out[i] = elem.read_real("coefficient").unwrap_or(0.0);
        }
        Some(out)
    }
    let r = read_channel(root, "default lightprobe r")?;
    let g = read_channel(root, "default lightprobe g")?;
    let b = read_channel(root, "default lightprobe b")?;
    Some(DefaultLightprobe { r, g, b })
}

fn read_real_vector3d(s: &TagStruct<'_>, name: &str) -> Option<RealVector3d> {
    match s.field(name)?.value()? {
        TagFieldData::RealVector3d(v) => Some(v),
        _ => None,
    }
}

/// Decode every mesh from the `render geometry` block of an arbitrary
/// root struct — works on `render_model` (mode) tags AND on
/// `scenario_structure_bsp` (sbsp) tags, since both share the
/// `s_render_geometry` schema. Returns one [`RenderMesh`] per
/// `render geometry/meshes[i]`.
///
/// Compression bounds are auto-paired: mesh `i` uses
/// `render geometry/compression info[i]` if it exists, else falls
/// back to `compression info[0]`. This works for render_model tags
/// (which generally have one or more bounds entries) and for sbsp
/// instance meshes (paired 1:1 with their definition's
/// compression_info entry). For sbsp **cluster** meshes (mesh_index
/// >= compression_info.len()), use
/// [`extract_render_geometry_meshes_with_bounds`] and supply the
/// BSP's `world_bounds_x/y/z` as the cluster mesh bounds.
pub fn extract_render_geometry_meshes(
    root: &TagStruct<'_>,
) -> Result<Vec<RenderMesh>, RenderModelError> {
    extract_render_geometry_meshes_with_bounds(root, |mi| {
        // compression_info[mi] when in range, else identity. sbsp
        // cluster meshes that fall through here will be wrong — use
        // the per-mesh-bounds API instead.
        let bounds = crate::geometry::read_compression_bounds_at(root, mi);
        if bounds.pos_compressed || bounds.uv_compressed {
            bounds
        } else {
            crate::geometry::CompressionBounds::identity()
        }
    })
}

/// Same as [`extract_render_geometry_meshes`], but the caller picks
/// the compression bounds per mesh via a closure. Used by sbsp loaders
/// to apply `compression_info[i]` to instance meshes (i < N) and the
/// BSP's `world_bounds_x/y/z` to cluster meshes (i >= N).
pub fn extract_render_geometry_meshes_with_bounds<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> crate::geometry::CompressionBounds,
{
    read_meshes_per_mesh(root, bounds_for, IndexFormatPolicy::PerMeshSchema)
}

/// Index-buffer interpretation policy. Halo 3 sbsp `render geometry`
/// stores all index buffers as triangle lists despite the schema's
/// `index buffer type` enum sometimes claiming "triangle strip" — this
/// is empirically verified by the H3 Blender Toolset's `_mesh_decoder.py`
/// (face-normal correlation 1.000 for list, ~0.50 for strip on Guardian).
/// Render_model meshes (mode tags) DO use the schema enum; pass
/// `PerMeshSchema` for those.
#[derive(Debug, Clone, Copy)]
pub enum IndexFormatPolicy {
    /// Use the per-mesh `index buffer type` enum to choose strip vs list.
    /// Correct for `render_model` (mode) tags.
    PerMeshSchema,
    /// Force triangle list regardless of the schema enum. Correct for
    /// `scenario_structure_bsp` (sbsp) `render geometry` meshes.
    ForceTriangleList,
}

/// sbsp-specific extractor: forces triangle-list interpretation on every
/// mesh (the schema enum lies about strip-vs-list for sbsp). Caller
/// supplies per-mesh bounds — `compression_info[def.compression_index]`
/// for instance defs (mesh_idx < compression_info.len()) and identity
/// for cluster meshes (mesh_idx >= compression_info.len()).
pub fn extract_sbsp_render_geometry_meshes<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> crate::geometry::CompressionBounds,
{
    read_meshes_per_mesh(root, bounds_for, IndexFormatPolicy::ForceTriangleList)
}

fn read_nodes(root: &TagStruct<'_>) -> Result<Vec<RenderNode>, RenderModelError> {
    let block = root.field_path("nodes").and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("nodes"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        out.push(RenderNode {
            name: n.read_string_id("name").unwrap_or_default(),
            parent_index: n.read_block_index("parent node"),
            default_translation: n.read_point3d("default translation"),
            default_rotation: n.read_quat("default rotation"),
        });
    }
    Ok(out)
}

fn read_materials(root: &TagStruct<'_>) -> Result<Vec<RenderMaterial>, RenderModelError> {
    let block = root.field_path("materials").and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("materials"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        let (shader_group_tag, path) = m
            .read_tag_ref_with_group("render method")
            .unwrap_or((0, String::new()));
        let shader_name = std::path::Path::new(&path.replace('\\', "/"))
            .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned();
        out.push(RenderMaterial { shader_name, shader_path: path, shader_group_tag });
    }
    Ok(out)
}

fn read_regions(root: &TagStruct<'_>) -> Result<Vec<RenderRegion>, RenderModelError> {
    let block = root.field_path("regions").and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("regions"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let r = block.element(i).unwrap();
        let name = r.read_string_id("name").unwrap_or_default();
        let perms_block = r.field("permutations").and_then(|f| f.as_block());
        let mut permutations = Vec::new();
        if let Some(perms) = perms_block {
            for j in 0..perms.len() {
                let p = perms.element(j).unwrap();
                permutations.push(RenderPermutation {
                    name: p.read_string_id("name").unwrap_or_default(),
                    mesh_index: p.read_int_any("mesh index").unwrap_or(-1) as i16,
                    mesh_count: p.read_int_any("mesh count").unwrap_or(0) as i16,
                });
            }
        }
        out.push(RenderRegion { name, permutations });
    }
    Ok(out)
}

fn read_markers(root: &TagStruct<'_>) -> Result<Vec<RenderMarker>, RenderModelError> {
    let Some(block) = root.field_path("marker groups").and_then(|f| f.as_block()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for i in 0..block.len() {
        let g = block.element(i).unwrap();
        let group_name = g.read_string_id("name").unwrap_or_default();
        let inner = match g.field("markers").and_then(|f| f.as_block()) {
            Some(b) => b, None => continue,
        };
        for j in 0..inner.len() {
            let m = inner.element(j).unwrap();
            out.push(RenderMarker {
                name: group_name.clone(),
                region_index: m.read_int_any("region index").unwrap_or(-1) as i8,
                permutation_index: m.read_int_any("permutation index").unwrap_or(-1) as i8,
                node_index: m.read_int_any("node index").unwrap_or(-1) as i8,
                translation: m.read_point3d("translation"),
                rotation: m.read_quat("rotation"),
                scale: m.read_real("scale").unwrap_or(1.0),
            });
        }
    }
    Ok(out)
}

fn read_meshes(
    root: &TagStruct<'_>,
    bounds: &CompressionBounds,
) -> Result<Vec<RenderMesh>, RenderModelError> {
    read_meshes_per_mesh(root, |_| *bounds, IndexFormatPolicy::PerMeshSchema)
}

fn read_meshes_per_mesh<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
    index_format: IndexFormatPolicy,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    read_meshes_at_path(root, "render geometry", bounds_for, index_format)
}

/// Walk a parallel render-geometry block at a configurable path.
/// `mode`/`sbsp` tags use `"render geometry"`; the per-BSP **lightmap**
/// tag (`scenario_lightmap_bsp_data`) puts a vertex-aligned 1:1 copy
/// at `"imported geometry"` — same schema, different field name.
/// The lightmap copy is what carries non-zero `lightmap texcoord` values.
pub fn extract_imported_geometry_meshes<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    read_meshes_at_path(root, "imported geometry", bounds_for, IndexFormatPolicy::ForceTriangleList)
}

/// Per-instance lightmap UV streams. One entry per
/// `s_per_instance_lightmap_texcoords` block in the LIGHTMAP tag's
/// `imported geometry`. `block_index` is the structure_instance's
/// `lightmap_texcoord_block_index` (sbsp), `uvs` is per-vertex
/// lightmap UVs aligned with the corresponding instance-definition
/// mesh's raw_vertices in the same lightmap tag.
///
/// In the loose tag, only the `lightmap texcoord` field of each
/// `texture coordinates` entry is meaningful — position/normal/etc.
/// are all zero (it's a UV-only stream). Cache builds repackage these
/// into a per-instance vertex buffer indexed via
/// `per_instance_lightmap_texcoords[i].vertex_buffer_index` — that
/// runtime form is what `select_instance_entry_point @ 0x180691340`
/// reads via `mesh_get_vertex_buffer(_vertex_buffer_usage_lightmap_uv)`.
#[derive(Debug, Clone)]
pub struct PerInstanceLightmapUvs {
    pub block_index: usize,
    pub uvs: Vec<RealPoint2d>,
}

/// Walk the lightmap tag's
/// `imported geometry/per_instance_lightmap_texcoords[]` block. Each
/// entry's `texture coordinates` block is one UV stream; only the
/// `lightmap texcoord` field is read.
pub fn extract_per_instance_lightmap_uvs(
    root: &TagStruct<'_>,
) -> Vec<PerInstanceLightmapUvs> {
    let Some(block) = root
        .field_path("imported geometry/per_instance_lightmap_texcoords")
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let elem = match block.element(i) {
            Some(e) => e,
            None => continue,
        };
        let Some(tc) = elem.field("texture coordinates").and_then(|f| f.as_block()) else {
            out.push(PerInstanceLightmapUvs { block_index: i, uvs: Vec::new() });
            continue;
        };
        let mut uvs = Vec::with_capacity(tc.len());
        for k in 0..tc.len() {
            let Some(v) = tc.element(k) else {
                uvs.push(RealPoint2d::default());
                continue;
            };
            uvs.push(v.read_point2d("lightmap texcoord"));
        }
        out.push(PerInstanceLightmapUvs { block_index: i, uvs });
    }
    out
}

fn read_meshes_at_path<F>(
    root: &TagStruct<'_>,
    path_prefix: &str,
    bounds_for: F,
    index_format: IndexFormatPolicy,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    let pmt_path = format!("{path_prefix}/per mesh temporary");
    let meshes_path = format!("{path_prefix}/meshes");
    let pmt_block = root.field_path(&pmt_path)
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("render geometry/per mesh temporary"))?;
    let meshes_block = root.field_path(&meshes_path)
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("render geometry/meshes"))?;

    // Parallel `per mesh prt data` block — one entry per mesh, holding
    // the author-time PCA codebook (`mesh pca data*` = 3 floats RGB per
    // vertex). Reach `create_prt_vertex_buffer @ 0x82E080F0` averages
    // these to grayscale to produce the runtime slot-2 vertex stream.
    // Not present on every tag (render_model tags typically empty);
    // None falls back to "no PRT data on any mesh".
    let prt_path = format!("{path_prefix}/per_mesh_prt_data");
    let prt_data_block = root.field_path(&prt_path).and_then(|f| f.as_block());

    let count = meshes_block.len();
    let mut out = Vec::with_capacity(count);
    for mi in 0..count {
        let mesh = meshes_block.element(mi).unwrap();
        let bounds = bounds_for(mi);
        // Rigid meshes (`vertex type` enum 1=rigid or 5=rigid_boned)
        // store skin weights only via the mesh-level `rigid node
        // index`; per-vertex node arrays are typically all zero.
        let vt = mesh.field("vertex type").and_then(|f| f.value()).map(|v| match v {
            TagFieldData::CharEnum { value, .. } => value as i32, _ => -1,
        }).unwrap_or(-1);
        let rigid_node_index = if matches!(vt, 1 | 5) {
            mesh.read_int_any("rigid node index").map(|v| v as i16).filter(|&v| v >= 0)
        } else { None };

        // `PRT vertex type` enum + slot-3 (`_vertex_buffer_usage_prt`)
        // population. The picker in `select_instance_entry_point @
        // 0x180691340` activates PRT only when both the instance has
        // `lightmapping_policy == 2 (single-probe)` AND
        // `vertex_buffer_indices[3] != 0xFFFF`. We surface the per-mesh
        // half here; the policy bit comes from sbsp at the caller.
        let prt_vertex_type = mesh
            .field("PRT vertex type")
            .and_then(|f| f.value())
            .map(|v| match v {
                TagFieldData::CharEnum { value, .. } => value as i32,
                _ => 0,
            })
            .map(PrtVertexType::from_raw)
            .unwrap_or(PrtVertexType::None);
        let has_prt_vertex_stream = mesh
            .field("vertex buffer indices")
            .and_then(|f| f.as_array())
            .and_then(|arr| arr.element(3))
            .and_then(|e| e.fields().next())
            .and_then(|f| f.value())
            .map(|v| match v {
                TagFieldData::ShortInteger(s) => (s as u16) != 0xFFFF,
                _ => false,
            })
            .unwrap_or(false);

        // `s_mesh.flags` byte_flags at offset 0x2C (Ares
        // `geometry_definitions.h:13-21`). Bit 0 =
        // `_mesh_has_vertex_color_bit`. Tag schema name: `"mesh flags"`.
        // Engine `render_mesh_part_default @ 0x18069EBC0` reads this
        // bit to remap `_entry_point_static_lighting_prt_quadratic` →
        // `_entry_point_vertex_color_lighting` at draw time.
        let mesh_flags = mesh.read_int_any("mesh flags").unwrap_or(0) as u32;
        let has_vertex_color = (mesh_flags & 1) != 0;

        // No raw_vertex / raw_indices means no inline geometry — emit
        // an empty mesh placeholder so indexing into `meshes` still
        // matches the tag's `meshes[i]` order. PRT eligibility is kept
        // from the schema fields above (the placeholder still reflects
        // what `meshes[i]` declares).
        let empty_with_prt = || RenderMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            parts: Vec::new(),
            rigid_node_index,
            water_data: None,
            prt_vertex_type,
            has_prt_vertex_stream,
            prt_ambient_stream: Vec::new(),
            has_vertex_color,
        };
        let Some(pmt) = pmt_block.element(mi) else {
            out.push(empty_with_prt());
            continue;
        };
        let Some(raw_v) = pmt.field("raw vertices").and_then(|f| f.as_block()) else {
            out.push(empty_with_prt());
            continue;
        };
        let Some(raw_i) = pmt.field("raw indices").and_then(|f| f.as_block()) else {
            out.push(empty_with_prt());
            continue;
        };

        // Decode every raw vertex once (parts will share the pool).
        let mut vertices: Vec<RenderVertex> = Vec::with_capacity(raw_v.len());
        for k in 0..raw_v.len() {
            let v = raw_v.element(k).unwrap();
            vertices.push(read_vertex(&v, &bounds, rigid_node_index));
        }

        let raw_index_list: Vec<u16> = (0..raw_i.len())
            .filter_map(|k| raw_i.element(k))
            .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
            .collect();

        let is_strip = match index_format {
            IndexFormatPolicy::ForceTriangleList => false,
            IndexFormatPolicy::PerMeshSchema => mesh
                .field("index buffer type")
                .and_then(|f| f.value())
                .map(|v| matches!(v, TagFieldData::CharEnum { name: Some(n), .. } if n == "triangle strip"))
                .unwrap_or(true),
        };

        let parts_block = mesh.field("parts").and_then(|f| f.as_block())
            .ok_or(RenderModelError::MissingField("meshes[i]/parts"))?;

        let mut indices: Vec<u32> = Vec::new();
        let mut parts: Vec<RenderMeshPart> = Vec::with_capacity(parts_block.len());
        for pi in 0..parts_block.len() {
            let part = parts_block.element(pi).unwrap();
            let material_index = part.read_int_any("render method index").unwrap_or(0).max(0) as u16;
            let part_type = part.read_int_any("part type").unwrap_or(0) as i8;
            // `index start` / `index count` are schema-typed `short
            // integer` (i16) but functionally u16 — strips spanning
            // more than 32 767 indices wrap into negative i16. The
            // low-16-bit reinterpret recovers the real offset.
            let start_i = part.read_int_any("index start").unwrap_or(0);
            let count_i = part.read_int_any("index count").unwrap_or(0);
            if count_i <= 0 {
                parts.push(RenderMeshPart {
                    material_index, index_start: indices.len() as u32, index_count: 0, part_type,
                });
                continue;
            }
            let start = (start_i as i16 as u16) as usize;
            let count = count_i as usize;
            if start >= raw_index_list.len() {
                parts.push(RenderMeshPart {
                    material_index, index_start: indices.len() as u32, index_count: 0, part_type,
                });
                continue;
            }
            let end = (start + count).min(raw_index_list.len());
            let part_indices = &raw_index_list[start..end];

            let part_index_start = indices.len() as u32;
            if is_strip {
                for (a, b, c) in strip_to_list(part_indices) {
                    indices.push(a as u32);
                    indices.push(b as u32);
                    indices.push(c as u32);
                }
            } else {
                for chunk in part_indices.chunks_exact(3) {
                    indices.push(chunk[0] as u32);
                    indices.push(chunk[1] as u32);
                    indices.push(chunk[2] as u32);
                }
            }
            let part_index_count = indices.len() as u32 - part_index_start;
            parts.push(RenderMeshPart {
                material_index,
                index_start: part_index_start,
                index_count: part_index_count,
                part_type,
            });
        }

        let water_data = read_raw_water_data(&mesh, &pmt, &raw_index_list, &parts_block);

        // PRT Ambient bake. Source: `per_mesh_prt_data[mi].mesh pca
        // data` = 3 little-endian floats RGB per vertex. Output: one
        // `f32` per vertex = `(R + G + B) / 3`. Mirrors Reach's
        // `create_prt_vertex_buffer @ 0x82E080F0` but skips the
        // X360-only `* 255 + 0.5` byte quantization (MCC PC declaration
        // is `R32_FLOAT`, see Ares
        // `rasterizer_resource_definitions.cpp:46`).
        let prt_ambient_stream: Vec<f32> = prt_data_block
            .as_ref()
            .and_then(|blk| blk.element(mi))
            .and_then(|e| e.field("mesh pca data").and_then(|f| f.as_data()))
            .filter(|bytes| !bytes.is_empty() && bytes.len() == 12 * vertices.len())
            .map(|bytes| {
                let mut out = Vec::with_capacity(vertices.len());
                for v in 0..vertices.len() {
                    let off = v * 12;
                    let r = f32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
                    let g = f32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap());
                    let b = f32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap());
                    out.push((r + g + b) * (1.0 / 3.0));
                }
                out
            })
            .unwrap_or_default();

        out.push(RenderMesh {
            vertices,
            indices,
            parts,
            rigid_node_index,
            water_data,
            prt_vertex_type,
            has_prt_vertex_stream,
            prt_ambient_stream,
            has_vertex_color,
        });
    }
    Ok(out)
}

/// Walk water-flagged parts and produce already-resolved per-triangle
/// `(regular_idx, water_idx)` control-point pairs. Mirrors the cache-build
/// walk in `?create_mesh_water_vertex_buffer @ 0x82e094e8` (Reach XEX) —
/// see `reference_water_vertex_buffer_build.md`.
///
/// Schema:
/// - `per_mesh_temporary[i].raw water data` (1-element block) =
///   `s_raw_water_data` with `raw water indices` + `raw water vertices`.
/// - `meshes[i].water indices start` (per-part u16 offsets into the
///   water-index pool — one entry per part).
/// - `meshes[i].parts[p].part flags` bit 3 = `_part_is_water_surface`.
/// - `meshes[i].parts[p].index_start` / `index_count` reference
///   `raw indices` (the regular pool).
fn read_raw_water_data(
    mesh: &TagStruct<'_>,
    pmt: &TagStruct<'_>,
    raw_index_list: &[u16],
    parts_block: &TagBlock<'_>,
) -> Option<RawWaterData> {
    // 1. raw_water_data block (1 element if water-bearing).
    let block = pmt.field("raw water data").and_then(|f| f.as_block())?;
    if block.is_empty() {
        return None;
    }
    let elem = block.element(0)?;
    let water_indices_block = elem.field("raw water indices").and_then(|f| f.as_block())?;
    let vertices_block = elem.field("raw water vertices").and_then(|f| f.as_block())?;
    if water_indices_block.is_empty() && vertices_block.is_empty() {
        return None;
    }

    // 2. Decode raw_water_indices into a flat u16 array.
    let mut raw_water_indices: Vec<u16> = Vec::with_capacity(water_indices_block.len());
    for k in 0..water_indices_block.len() {
        let Some(e) = water_indices_block.element(k) else { continue };
        raw_water_indices.push(e.read_int_any("word").unwrap_or(0) as u16);
    }

    // 3. Decode raw_water_vertices (the append pool). Per
    // `s_raw_water_append` schema: 36 bytes total = local_info(rp3d)
    // + water_velocity(rp3d) + base_texcoord(rp3d). For BSP geometry
    // the canonical source is the lightmap tag's
    // `scenario_lightmap_bsp_data.raw_water_append_block` (identical
    // 36-byte layout to render_model's version).
    //
    // Empirical riverworld values (1500+ verts across 11 BSPs):
    //   local_info.x = 3.178 (constant across ALL vertices) — engine
    //                  line 447 `displacement *= IN.local_info.x`, so
    //                  this is the scenario-wide wave amplitude scale
    //                  (NOT a per-vertex shore taper as initially
    //                  speculated). Another scenario could pick a
    //                  different constant.
    //   local_info.y = water_depth, varies [0, 8.168]. Drives
    //                  `misc_info.w` → `bank_alpha` shore color fade.
    //   local_info.z = 0 (unused in PC water_shading_fx).
    let mut vertices: Vec<RawWaterAppend> = Vec::with_capacity(vertices_block.len());
    for k in 0..vertices_block.len() {
        let Some(e) = vertices_block.element(k) else { continue };
        vertices.push(RawWaterAppend {
            local_info: e.read_point3d("local info"),
            water_velocity: e.read_point3d("water velocity"),
            base_texcoord: e.read_point3d("base texcoord"),
        });
    }

    // 4. mesh.water_indices_start — per-part u16 base offsets.
    let water_starts_block = mesh
        .field("water indices start")
        .and_then(|f| f.as_block())?;
    let mut water_indices_start: Vec<u16> = Vec::with_capacity(water_starts_block.len());
    for k in 0..water_starts_block.len() {
        let Some(e) = water_starts_block.element(k) else { continue };
        water_indices_start.push(e.read_int_any("word").unwrap_or(0) as u16);
    }
    if water_indices_start.is_empty() {
        return None;
    }

    // 5. Walk parts; for each water-flagged one, emit triangles AND
    //    record the part's (start, count) range. Per Reach
    //    `create_mesh_water_vertex_buffer`:
    //      regular_idx[j] = raw_indices[part.index_start + j]
    //      water_idx[j]   = raw_water_indices[water_indices_start[p] + j]
    //    Triangles formed by chunking j in 0..part.index_count by 3.
    //    Per-part ranges let the renderer dispatch a separate water
    //    draw per part (different rmw materials on the same mesh =
    //    different shader pipelines + cbuffers).
    let mut triangles: Vec<RawWaterTriangle> = Vec::new();
    let mut parts: Vec<RawWaterPart> = Vec::new();
    for p in 0..parts_block.len() {
        let Some(part) = parts_block.element(p) else { continue };
        let part_flags = part.read_int_any("part flags").unwrap_or(0) as u32;
        // Bit 3 = `_part_is_water_surface` per `e_part_flags`
        // (`Ares/source/geometry/geometry_definitions.h:44-51`).
        if (part_flags & 0x08) == 0 {
            continue;
        }
        let regular_base = part.read_int_any("index start").unwrap_or(0);
        // `index_start` is schema-typed `short integer` (i16) but
        // functionally u16 — strips spanning >32767 wrap. Reinterpret
        // matches the existing protomorph behavior in render_model.
        let regular_base = (regular_base as i16 as u16) as usize;
        let count = part.read_int_any("index count").unwrap_or(0) as usize;
        if count == 0 || count % 3 != 0 {
            continue;
        }
        let Some(&water_base) = water_indices_start.get(p) else { continue };
        let water_base = water_base as usize;
        if water_base + count > raw_water_indices.len() {
            continue;
        }
        if regular_base + count > raw_index_list.len() {
            continue;
        }
        let triangles_in_part = count / 3;
        let triangle_start = triangles.len() as u32;
        for tri in 0..triangles_in_part {
            let mut control_points = [RawWaterControlPoint::default(); 3];
            for j in 0..3 {
                let off = tri * 3 + j;
                control_points[j] = RawWaterControlPoint {
                    regular_idx: raw_index_list[regular_base + off],
                    water_idx: raw_water_indices[water_base + off],
                };
            }
            triangles.push(RawWaterTriangle { control_points });
        }
        parts.push(RawWaterPart {
            mesh_part_index: p as u16,
            triangle_start,
            triangle_count: triangles_in_part as u32,
        });
    }

    if triangles.is_empty() && vertices.is_empty() {
        return None;
    }

    Some(RawWaterData { triangles, vertices, parts })
}

fn read_vertex(
    v: &TagStruct<'_>,
    bounds: &CompressionBounds,
    rigid_node_index: Option<i16>,
) -> RenderVertex {
    let raw_pos = v.read_point3d("position");
    let position = bounds.decompress_position(raw_pos);
    let normal = v.read_point3d("normal").as_vector();
    // raw_vertex stores both tangent + binormal directly (rather than
    // a packed sign), so we keep both here. Tags without tangent-space
    // data leave the fields zero — callers should detect that and
    // synthesize a basis themselves.
    let tangent = v.read_point3d("tangent").as_vector();
    let binormal = v.read_point3d("binormal").as_vector();
    let raw_uv = v.read_point2d("texcoord");
    let texcoord = bounds.decompress_texcoord(raw_uv);
    // Lightmap UV is stored as a separate field in raw_vertex. SBSP's
    // copy is zero; only the lightmap tag's parallel geometry has the
    // real values. Read whatever's here verbatim — caller decides
    // whether to source from sbsp or lightmap.
    let lightmap_texcoord = v.read_point2d("lightmap texcoord");
    // `raw_vertex.vert_color` — per-vertex baked color (sky meshes).
    // Stored as a `real_point3d`; read directly (no compression bounds).
    let vert_color = v.read_point3d("vertex color").as_vector();

    let mut node_indices = [0u8; 4];
    let mut node_weights = [0f32; 4];
    let mut filled = 0usize;
    if let (Some(idx_arr), Some(wt_arr)) = (
        v.field("node indices").and_then(|f| f.as_array()),
        v.field("node weights").and_then(|f| f.as_array()),
    ) {
        for k in 0..idx_arr.len().min(wt_arr.len()).min(4) {
            let idx = idx_arr.element(k).unwrap().fields().next()
                .and_then(|f| f.value())
                .and_then(|v| if let TagFieldData::CharInteger(c) = v { Some(c) } else { None })
                .unwrap_or(0);
            let wt = wt_arr.element(k).unwrap().fields().next()
                .and_then(|f| f.value())
                .and_then(|v| if let TagFieldData::Real(r) = v { Some(r) } else { None })
                .unwrap_or(0.0);
            if wt > 0.0 {
                node_indices[filled] = idx.max(0) as u8;
                node_weights[filled] = wt;
                filled += 1;
            }
        }
    }
    // Rigid-mesh fallback: zero per-vertex weights but a valid
    // mesh-level `rigid node index` means "every vertex bound to that
    // bone at weight 1.0".
    if filled == 0 {
        if let Some(node) = rigid_node_index {
            if node >= 0 {
                node_indices[0] = node as u8;
                node_weights[0] = 1.0;
            }
        }
    }

    RenderVertex {
        position,
        texcoord,
        normal,
        tangent,
        binormal,
        node_indices,
        node_weights,
        lightmap_texcoord,
        vert_color,
    }
}
