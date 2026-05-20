//! Structure BSP tag (`sbsp`) types — author-time tag format.
//!
//! Captures the rendering-relevant subset:
//! - Clusters (spatial partitions, each with one mesh + portals + sky)
//! - Materials (per-mesh-part render_method bindings)
//! - Instanced geometry instances + per-instance lightmap policy
//! - Markers + sky_owner_cluster mapping
//! - Render geometry mesh metadata (parts → render_method index)
//!
//! Field names follow the **MCC tag schema**. Mesh DATA (vertex / index
//! buffers) is read via the same render_model mesh extraction path —
//! see [`crate::render_model::RenderModel::from_tag`] for the
//! algorithm; protomorph drives this directly when uploading a BSP.
//!
//! Reference: `Ares/source/structures/structure_bsp_definitions.h:102`
//! and `Ares/source/structures/instanced_geometry_definitions.h:33`.

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::fields::TagFieldData;
use crate::math::{RealBounds, RealPlane2d, RealPlane3d, RealPoint3d, RealVector3d};

const SBSP_GROUP: [u8; 4] = *b"sbsp";

#[derive(Debug)]
pub enum StructureBspError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for StructureBspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "structure_bsp: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for StructureBspError {}

// =============================================================================
// Top-level
// =============================================================================

/// Structure BSP tag (`sbsp`) — root of one BSP's geometry / clusters /
/// instances / materials. A scenario references one or more.
#[derive(Debug, Clone, Default)]
pub struct StructureBsp {
    pub flags: u32,
    pub world_bounds_x: RealBounds,
    pub world_bounds_y: RealBounds,
    pub world_bounds_z: RealBounds,

    /// Per-mesh-part shaders. `materials[i].render_method` is the tag
    /// path; mesh.parts[k].render_method_index indexes here.
    pub materials: Vec<BspMaterial>,

    /// Per-collision-surface shaders (separate list from `materials`).
    pub collision_materials: Vec<BspCollisionMaterial>,

    /// `leaves*` (offset 0x30) — one per BSP3D leaf node. Each entry
    /// holds a single `cluster` index (i8, -1 = no cluster). The
    /// BSP3D collision tree walks down to a leaf via plane tests
    /// (`bsp3d_test_point @ 0x1803342E0`); the leaf's `cluster` field
    /// then maps position → cluster_index. Phase C2 of the visibility
    /// port (`scenario_location_from_point @ 0x18017BFE0`) needs this.
    pub leaves: Vec<BspLeaf>,

    /// `collision bsp*` (block of `global_collision_bsp_block`, max 1).
    /// The collision/BSP3D tree used for camera→leaf→cluster lookup.
    /// `None` if absent (some BSPs have no collision data). Schema
    /// also exposes `large collision bsp*` and `render bsp*`; we
    /// surface only the standard one here.
    pub collision_bsp: Option<Bsp3d>,

    pub clusters: Vec<BspCluster>,

    /// `instanced geometry instances[i]` — placement. Definition is
    /// implicit via `mesh_index` (each instance defs a mesh in render
    /// geometry, but the actual definition table is built at runtime).
    pub instanced_geometry_instances: Vec<BspInstance>,

    /// `cluster portals[i]` — connectivity between clusters for PVS
    /// + portal-frustum culling.
    pub cluster_portals: Vec<BspClusterPortal>,

    /// `sky owner cluster[i]` — which cluster index owns each sky in
    /// the scenario. `[i]` = scenario sky index.
    pub sky_owner_clusters: Vec<i16>,

    /// Mesh geometry metadata — parts and material indices. Vertex/index
    /// data is decoded separately (see render_model's mesh reader).
    pub meshes_metadata: Vec<BspMeshMetadata>,

    /// Markers (named anchor points within the BSP; e.g. "sky_anchor").
    pub markers: Vec<BspMarker>,

    /// `resource interface/raw_resources[0]/raw_items/instanced geometries
    /// definitions` — one entry per unique instance definition. Instance
    /// placements (`instanced_geometry_instances[i].definition_index`)
    /// reference these. Each def carries `mesh index` (which mesh in
    /// `render_geometry/meshes[]`) and `compression index` (which
    /// `compression_info[]` entry to use for that mesh's vertex
    /// decompression).
    pub instance_definitions: Vec<BspInstanceDefinition>,

    /// `atmosphere palette[i]` — per-BSP atmosphere palette indirection.
    /// Each entry maps a name + index into the scenario's
    /// `sky_atm_parameters.atmosphere_settings[]`. `BspCluster::atmosphere_index`
    /// indexes this table; the resolved entry's `atmosphere_setting_index`
    /// then indexes the global atmosphere settings. Engine
    /// `c_atmosphere_fog_interface::get_atmosphere_setting @ 0x1803AFBA0`.
    pub atmosphere_palette: Vec<BspAtmospherePaletteEntry>,

    /// `camera fx palette[i]` — per-BSP camera-fx palette. Each entry
    /// is a `structure_camera_fx_palette_entry` (a cfxs tag-ref + per-
    /// field overrides keyed on `flags`). `BspCluster::camera_fx_index`
    /// indexes this table. Engine `c_camera_fx_values::update @
    /// 0x180687CB0:47-101` resolves this each frame for the camera's
    /// current cluster and applies the overrides to the inherited
    /// scenario-level cfxs.
    pub camera_fx_palette: Vec<BspCameraFxPaletteEntry>,

    /// `weather palette[i]` — per-BSP weather palette. Engine: weather
    /// is a normal particle effect with the `_effect_weather_bit` flag
    /// (see `effect_new_weather @ 0x18053D720` per the plan). The
    /// palette entries carry per-effect wind direction/magnitude/scale
    /// function, indexed by per-cluster activation in the scenario's
    /// `scenario_cluster_weather_properties` block. NO separate weather
    /// renderer — particle effects render through standard transparency.
    pub weather_palette: Vec<BspWeatherPaletteEntry>,
}

impl StructureBsp {
    pub fn from_tag(tag: &TagFile) -> Result<Self, StructureBspError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != SBSP_GROUP {
            return Err(StructureBspError::WrongGroup { expected: SBSP_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            flags: s.read_int_any("flags").unwrap_or(0) as u32,
            world_bounds_x: s.read_real_bounds("world bounds x"),
            world_bounds_y: s.read_real_bounds("world bounds y"),
            world_bounds_z: s.read_real_bounds("world bounds z"),

            materials: read_block(s, "materials", BspMaterial::from_struct),
            collision_materials: read_block(
                s,
                "collision materials",
                BspCollisionMaterial::from_struct,
            ),
            leaves: read_block(s, "leaves", BspLeaf::from_struct),
            collision_bsp: Bsp3d::from_collision_block(s),
            clusters: read_block(s, "clusters", BspCluster::from_struct),
            instanced_geometry_instances: read_block(
                s,
                "instanced geometry instances",
                BspInstance::from_struct,
            ),
            cluster_portals: read_block(s, "cluster portals", BspClusterPortal::from_struct),
            sky_owner_clusters: s
                .field("sky owner cluster")
                .and_then(|f| f.as_block())
                .map(|b| {
                    let mut v = Vec::with_capacity(b.len());
                    for i in 0..b.len() {
                        if let Some(e) = b.element(i) {
                            v.push(e.read_block_index("cluster"));
                        }
                    }
                    v
                })
                .unwrap_or_default(),
            meshes_metadata: s
                .field("render geometry")
                .and_then(|f| f.as_struct())
                .and_then(|rg| rg.field("meshes").and_then(|f| f.as_block()))
                .map(|b| read_block_vec(&b, BspMeshMetadata::from_struct))
                .unwrap_or_default(),
            markers: read_block(s, "markers", BspMarker::from_struct),
            instance_definitions: read_instance_definitions(s),
            atmosphere_palette: read_block(
                s,
                "atmosphere palette",
                BspAtmospherePaletteEntry::from_struct,
            ),
            camera_fx_palette: read_block(
                s,
                "camera fx palette",
                BspCameraFxPaletteEntry::from_struct,
            ),
            weather_palette: read_block(
                s,
                "weather palette",
                BspWeatherPaletteEntry::from_struct,
            ),
        }
    }
}

/// One BSP-side weather palette entry. Schema
/// `structure_bsp_weather_palette_block` (120B). Each entry's named
/// effect-tag-ref + wind parameters drive a particle system; the entry
/// itself is a static palette slot referenced by per-cluster weather
/// activation in the scenario's `scenario_cluster_weather_properties`
/// block.
#[derive(Debug, Clone, Default)]
pub struct BspWeatherPaletteEntry {
    /// `name^` — palette entry author name.
    pub name: String,
    /// `wind direction` — world-space direction the wind blows (toward).
    pub wind_direction: RealVector3d,
    /// `wind magnitude` — per-effect wind speed scale.
    pub wind_magnitude: f32,
    /// `wind scale function` — string id of the scenario function that
    /// modulates wind magnitude over time. Empty when no animation.
    pub wind_scale_function: String,
}

impl BspWeatherPaletteEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            wind_direction: s.read_vec3("wind direction"),
            wind_magnitude: s.read_real("wind magnitude").unwrap_or(0.0),
            wind_scale_function: s.read_string_id("wind scale function").unwrap_or_default(),
        }
    }
}

/// One BSP-side atmosphere palette entry. Schema
/// `structure_bsp_atmosphere_palette_block` (8B). Per-BSP indirection
/// from `BspCluster::atmosphere_index` → `atmosphere_setting_index`,
/// which indexes the scenario's `sky_atm_parameters.atmosphere_settings[]`.
#[derive(Debug, Clone, Default)]
pub struct BspAtmospherePaletteEntry {
    /// `name^` (string_id) — author-friendly name.
    pub name: String,
    /// `Atmosphere Setting Index` (i16) — index into the scenario's
    /// `sky_atm_parameters.atmosphere_settings[]`. -1 = no setting.
    pub atmosphere_setting_index: i16,
}

impl BspAtmospherePaletteEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            atmosphere_setting_index: s
                .read_int_any("Atmosphere Setting Index")
                .unwrap_or(-1) as i16,
        }
    }
}

/// One BSP-side camera-fx palette entry. Schema
/// `structure_bsp_camera_fx_palette_block`. Per-BSP indirection from
/// `BspCluster::camera_fx_index`. Engine reads via
/// `c_camera_fx_values::update @ 0x180687CB0:47-101` —
/// `cluster_palette_entry` arg. When set, individual fields can
/// override (per `flags` bits) the scenario-level cfxs:
///   bit 0 → `forced_exposure` overrides exposure target (clears AUTO_BIT).
///   bit 1 → `forced_auto_exposure_brightness` overrides AUTO target (sets AUTO_BIT).
///   bit 2 → `exposure_min` / `exposure_max` override clamp range.
///   bit 3 → `inherent_bloom` + `bloom_intensity` override bloom params.
///
/// Engine struct layout (`structure_camera_fx_palette_entry`, 48 B):
///   `name` (i32 string_id, 4) + `camera_fx_tag` (TagRef, 16) +
///   `flags` (u8) + 3B pad + 6×f32 values.
#[derive(Debug, Clone, Default)]
pub struct BspCameraFxPaletteEntry {
    /// `name^` (string_id) — author-friendly name.
    pub name: String,
    /// `flags` byte (engine `structure_camera_fx_palette_entry.flags`).
    pub flags: u8,
    /// `forced exposure` (stops). Active when `flags & 0x01`.
    pub forced_exposure: f32,
    /// `forced auto exposure brightness` (stops). Active when `flags & 0x02`.
    pub forced_auto_exposure_brightness: f32,
    /// `exposure min` (stops). Active when `flags & 0x04`.
    pub exposure_min: f32,
    /// `exposure max` (stops). Active when `flags & 0x04`.
    pub exposure_max: f32,
    /// `inherent bloom`. Active when `flags & 0x08`.
    pub inherent_bloom: f32,
    /// `bloom intensity`. Active when `flags & 0x08`.
    pub bloom_intensity: f32,
}

impl BspCameraFxPaletteEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        // Field names verified against definitions/halo3_mcc/
        // scenario_structure_bsp.json `structure_bsp_camera_fx_palette_block`
        // (2026-05-20). The bit-1 override path field is
        // `forced auto-exposure screen brightness` (NOT
        // `forced auto exposure brightness` — missing hyphen and
        // "screen" causes silent unwrap_or(0.0)). The bloom field
        // names are plain `inherent bloom` / `bloom intensity`; the
        // schema's `override inherent bloom` / `override bloom intensity`
        // are FLAG-BIT LABELS in camera_fx_palette_flags, not field
        // names.
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            flags: s.read_int_any("camera_fx_palette_flags").unwrap_or(0) as u8,
            forced_exposure: s.read_real("forced exposure").unwrap_or(0.0),
            forced_auto_exposure_brightness: s
                .read_real("forced auto-exposure screen brightness")
                .unwrap_or(0.0),
            exposure_min: s.read_real("exposure min").unwrap_or(0.0),
            exposure_max: s.read_real("exposure max").unwrap_or(0.0),
            inherent_bloom: s.read_real("inherent bloom").unwrap_or(0.0),
            bloom_intensity: s.read_real("bloom intensity").unwrap_or(0.0),
        }
    }
}

fn read_instance_definitions(root: &TagStruct<'_>) -> Vec<BspInstanceDefinition> {
    // Path: resource interface/raw_resources[0]/raw_items/instanced geometries definitions
    let Some(ri) = root.field("resource interface").and_then(|f| f.as_struct()) else {
        return Vec::new();
    };
    let Some(rr) = ri.field("raw_resources").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let Some(elem0) = rr.element(0) else {
        return Vec::new();
    };
    let Some(items) = elem0.field("raw_items").and_then(|f| f.as_struct()) else {
        return Vec::new();
    };
    let Some(defs) = items
        .field("instanced geometries definitions")
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(defs.len());
    for i in 0..defs.len() {
        if let Some(d) = defs.element(i) {
            out.push(BspInstanceDefinition::from_struct(&d));
        }
    }
    out
}

// =============================================================================
// Sub-blocks
// =============================================================================

/// One material in `materials[]` — a render_method tag reference. Mesh
/// part's `render method index` indexes here.
#[derive(Debug, Clone, Default)]
pub struct BspMaterial {
    /// `.shader` / `.material` / etc tag path. NO file extension —
    /// caller composes via [`Self::render_method_extension`].
    pub render_method: String,
    /// FOURCC of the referenced render_method group — `rmsh` (regular
    /// shader), `rmtr` (terrain), `rmw ` (water), `rmfl` (foliage),
    /// etc. Riverworld carries a mix; missing this turns terrain
    /// shader paths into invalid `.shader` lookups.
    pub render_method_group_tag: u32,
    /// `imported material index` (debug / editor metadata).
    pub imported_material_index: i32,
    /// `breakable surface index` (-1 if not breakable).
    pub breakable_surface_index: i8,
}

impl BspMaterial {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let (render_method_group_tag, render_method) = s
            .read_tag_ref_with_group("render method")
            .unwrap_or((0, String::new()));
        Self {
            render_method,
            render_method_group_tag,
            imported_material_index: s.read_int_any("imported material index").unwrap_or(-1) as i32,
            breakable_surface_index: s.read_int_any("breakable surface index").unwrap_or(-1) as i8,
        }
    }

    /// File extension matching [`Self::render_method_group_tag`] —
    /// e.g. `"shader_terrain"` for `rmtr`. Pair with `render_method`
    /// + `paths::resolve_tag_path` to locate the on-disk tag.
    pub fn render_method_extension(&self) -> &'static str {
        crate::paths::group_tag_to_extension(self.render_method_group_tag).unwrap_or("shader")
    }
}

/// One collision material — tag ref + indexes into other tables.
/// Distinct from `materials` (which is the render-mesh material list).
#[derive(Debug, Clone, Default)]
pub struct BspCollisionMaterial {
    pub render_method: String,
    pub runtime_global_material_index: i16,
    pub conveyor_surface_index: i16,
    pub seam_mapping_index: i16,
}

impl BspCollisionMaterial {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            render_method: s.read_tag_ref_path("render method").unwrap_or_default(),
            runtime_global_material_index: s
                .read_int_any("runtime global material index")
                .unwrap_or(0) as i16,
            conveyor_surface_index: s.read_block_index("conveyor surface index"),
            seam_mapping_index: s.read_block_index("seam mapping index"),
        }
    }
}

// =============================================================================
// BSP3D — collision/visibility tree (Ares `physics/bsp3d.h`)
// =============================================================================

/// One BSP3D node — schema `bsp3d_nodes_block_struct` (8B per entry,
/// stored as a single `int64_integer` "node data designator!" in the
/// tag schema). Bit-packed engine layout (`bsp3d_node` in
/// `physics/bsp3d.h:39-52`):
///
/// ```text
///   bits  0-15  plane_index           (signed 16-bit)
///   bits 16-39  below_child_index     (24-bit; bit 23 = leaf bit)
///   bits 40-63  above_child_index     (24-bit; bit 23 = leaf bit)
/// ```
///
/// Child encoding: bit 23 of the 24-bit value is the leaf bit. When
/// set, the lower 23 bits are a leaf index into
/// [`StructureBsp::leaves`]. When clear, the lower 23 bits are a
/// child node index.
///
/// `bsp3d_test_point @ 0x1803342E0` walks down via plane tests until
/// it lands on a leaf-flagged child.
/// Canonical large-format encoding (Reach `s_large_bsp3d_types`,
/// verified against `bsp3d_child_index_is_node @ 0x8271B4A8` /
/// `bsp3d_child_index_from_leaf_index @ 0x82F902B8` /
/// `bsp3d_leaf_index_from_child_index @ 0x8271B4C0`):
///   - `child >= 0`              → child is another node, index = child
///   - `child <  0` && `child != -1` → child is a leaf,
///                                     leaf_index = `child & 0x7FFFFFFF`
///   - `child == -1`             → walker bails (sentinel)
/// Both small (8B-packed) and large (3 × i32) tag variants are
/// unpacked into this form at parse time so the runtime walker only
/// knows one convention.
#[derive(Debug, Clone, Copy, Default)]
pub struct Bsp3dNode {
    pub plane_index: i32,
    pub below_child: i32,
    pub above_child: i32,
}

impl Bsp3dNode {
    pub const NULL_CHILD: i32 = -1;
    /// Sign bit — `child < 0` ⇔ leaf.
    pub const LEAF_FLAG: u32 = 0x8000_0000;
    /// Mask for the leaf-index payload (bits 0-30).
    pub const LEAF_INDEX_MASK: i32 = 0x7FFF_FFFF;

    pub fn child_is_leaf(child: i32) -> bool {
        child < 0
    }

    pub fn child_leaf_index(child: i32) -> i32 {
        child & Self::LEAF_INDEX_MASK
    }

    pub fn plane_index(self) -> i32 {
        self.plane_index
    }
    pub fn below_child_index(self) -> i32 {
        self.below_child
    }
    pub fn above_child_index(self) -> i32 {
        self.above_child
    }
}

/// `collision_bsp` — schema `global_collision_bsp_struct/_block`
/// (sizeof=96). Engine `physics/collision_bsp_definitions.h`. Holds
/// the full collision tree: BSP3D nodes (kd-tree) + planes + leaves
/// (with bsp2d references) + bsp2d nodes (per-leaf surface kd-tree)
/// + surface polygons (with edges + vertices).
///
/// Two readers:
/// - `bsp3d_test_point` walks `nodes`/`planes` to find the leaf
///   containing a world point.
/// - `collision_bsp_test_vector_recursive @ 0x180513f80` walks the
///   same nodes for a ray, then `collision_leaf_test_vector @
///   0x180514460` uses each leaf's `bsp2d_references` to find which
///   surface polygon the ray hits.
#[derive(Debug, Clone, Default)]
pub struct Bsp3d {
    pub nodes: Vec<Bsp3dNode>,
    pub planes: Vec<RealPlane3d>,
    /// `leaves*` — one per BSP3D leaf the recursive walker can reach.
    pub leaves: Vec<CollisionLeaf>,
    /// `bsp2d references*` — per-leaf surface-tree roots. Each leaf
    /// addresses `[first_bsp2d_reference ..
    /// first_bsp2d_reference + bsp2d_reference_count)` of this array.
    pub bsp2d_references: Vec<CollisionBsp2dReference>,
    /// `bsp2d nodes*` — kd-tree nodes for per-surface ray-in-polygon
    /// testing. Leaf indices into `surfaces`.
    pub bsp2d_nodes: Vec<CollisionBsp2dNode>,
    /// `surfaces*` — collision polygons (one per BSP face).
    pub surfaces: Vec<CollisionSurface>,
    /// `edges*` — half-edge graph linking surfaces via shared edges
    /// (the surface-adjacency table the decal fragment walker uses).
    pub edges: Vec<CollisionEdge>,
    /// `vertices*` — collision vertices indexed by edges.
    pub vertices: Vec<CollisionVertex>,
}

/// `collision_leaf_struct` (sizeof=8). One per leaf of the BSP3D
/// collision tree. `bsp2d_reference_count` consecutive entries in
/// `Bsp3d::bsp2d_references` (starting at `first_bsp2d_reference`)
/// describe which surface trees this leaf intersects.
///
/// `flags` is `u16` to cover both schema variants: SMALL uses
/// `byte_flags` (u8); LARGE uses `word_flags` (u16). Stored canonical
/// at `u16` so a single walker reads it.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionLeaf {
    /// `flags*`. bit 0 = "contains double-sided surfaces" per the
    /// engine `collision_bsp_test_vector_recursive` contents logic.
    pub flags: u16,
    /// `bsp2d reference count*`.
    pub bsp2d_reference_count: i16,
    /// `first bsp2d reference*` — block index into
    /// `Bsp3d::bsp2d_references`.
    pub first_bsp2d_reference: i32,
}

/// `bsp2d_references_block` (sizeof=4 SMALL, sizeof=8 LARGE). Maps a
/// leaf to a per-plane surface kd-tree root. Both fields are stored
/// as `i32` regardless of source format — parser normalizes SMALL
/// bit-15 high-bit flags to the canonical bit-31 form so a single
/// walker reads either schema.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionBsp2dReference {
    /// `plane*` — plane_designator. Low 31 bits index into
    /// `Bsp3d::planes`; bit 31 = negate.
    pub plane_designator: i32,
    /// `bsp2d node*` — root node index into `Bsp3d::bsp2d_nodes`.
    /// Bit 31 set = leaf (surface index = value & 0x7FFF_FFFF).
    pub bsp2d_node: i32,
}

/// `bsp2d_nodes_block` (sizeof=16). A node in the per-leaf surface
/// kd-tree. Left/right children carry a sign-bit-leaf encoding —
/// negative = leaf (`child & 0x7FFF_FFFF` is the surface index),
/// non-negative = interior-node index.
///
/// Children are stored as `i32` regardless of source format. The two
/// schema variants disagree on field width:
///   - SMALL `collision bsp` schema: `short integer` (i16). Engine
///     uses bit 15 as the leaf flag; the parser remaps to the
///     canonical bit-31 form so a single walker handles both.
///   - LARGE `large collision bsp` schema: `long integer` (i32). Bit
///     31 is the leaf flag natively.
///
/// **Why this matters.** Storing as `i16` and reading LARGE values
/// with `as i16` TRUNCATES bit 31, turning a leaf encoded as
/// `0x80003D56` into an out-of-bounds positive interior pointer
/// (`0x3D56 = 15702`). On powerhouse (LARGE), that produced a tight
/// 24k-iteration cycle in `bsp2d_test_point` — the exact CPU-pegging
/// hang the previous-session watchdogs were masking.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionBsp2dNode {
    pub plane: RealPlane2d,
    pub left_child: i32,
    pub right_child: i32,
}

/// `surfaces_block` (sizeof=12 SMALL, sizeof=16 LARGE). One per
/// collision polygon (the engine calls these "surfaces"; each is a
/// planar face described as an edge ring).
///
/// `plane_designator` and `first_edge` are stored as `i32` regardless
/// of source format — LARGE schema fields are natively i32, SMALL i16
/// are sign-extended at parse time; `plane_designator`'s high-bit
/// negate flag is normalized to bit 31.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionSurface {
    /// `plane*` — plane_designator. Low 31 bits = plane index;
    /// bit 31 = negate.
    pub plane_designator: i32,
    /// `first edge*` — entry into `Bsp3d::edges` for the edge ring.
    /// Walk via `CollisionEdge::forward_edge` until you return to
    /// `first_edge`.
    pub first_edge: i32,
    /// `material*` — index into `StructureBsp::collision_materials`.
    pub material: i16,
    /// `breakable surface set*` — index into per-BSP breakable
    /// surface set table (unused outside breakable physics).
    pub breakable_surface_set: i16,
    /// `breakable surface*` — index into the breakable set.
    pub breakable_surface: i16,
    /// `flags*` (byte_flags `surface_flags`). The decal walker reads
    /// bits 1 and 3 to filter (bit 1: invisible/sky, bit 3: two-sided).
    pub flags: u8,
    /// `best plane calculation vertex index *!` — i8, runtime
    /// optimization hint; ignored by the decal port.
    pub best_plane_vertex_index: i8,
}

/// `edges_block` (sizeof=12 SMALL, sizeof=24 LARGE). Each edge is
/// shared by EXACTLY TWO surfaces (left + right). `forward_edge`
/// follows the edge ring around `left_surface`; `reverse_edge`
/// follows the ring around `right_surface` (with start/end vertices
/// swapped semantically).
///
/// Fields stored as `i32` regardless of source format. LARGE schema
/// is natively i32; SMALL i16 fields are sign-extended at parse time.
/// Powerhouse has 33020 edges — outside i16 range — so the LARGE
/// schema is mandatory for any map with >32k edges.
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionEdge {
    pub start_vertex: i32,
    pub end_vertex: i32,
    pub forward_edge: i32,
    pub reverse_edge: i32,
    pub left_surface: i32,
    pub right_surface: i32,
}

/// `vertices_block` (sizeof=16). Collision vertex with a back-pointer
/// to one of its edges (used for vertex-graph operations the decal
/// port doesn't exercise).
#[derive(Debug, Clone, Copy, Default)]
pub struct CollisionVertex {
    pub point: RealPoint3d,
    pub first_edge: i32,
}

impl Bsp3d {
    /// Read the BSP3D node + plane tables. In MCC the collision data
    /// is paged through the structure_bsp's resource interface, not
    /// stored at the top-level tag. Both the small (`collision bsp`,
    /// 8-byte packed nodes) and large (`large collision bsp`, 12-byte
    /// 3-int nodes) variants are tried in order; whichever has data
    /// wins. Returns `None` only if neither variant carries any nodes.
    pub fn from_collision_block(s: &TagStruct<'_>) -> Option<Self> {
        const SMALL_PATH: &str =
            "resource interface/raw_resources[0]/raw_items/collision bsp";
        const LARGE_PATH: &str =
            "resource interface/raw_resources[0]/raw_items/large collision bsp";

        if let Some(block) = s.field_path(SMALL_PATH).and_then(|f| f.as_block()) {
            if let Some(entry) = block.element(0) {
                let parsed = parse_small_bsp3d(&entry);
                if !parsed.nodes.is_empty() {
                    return Some(parsed);
                }
            }
        }
        if let Some(block) = s.field_path(LARGE_PATH).and_then(|f| f.as_block()) {
            if let Some(entry) = block.element(0) {
                let parsed = parse_large_bsp3d(&entry);
                if !parsed.nodes.is_empty() {
                    return Some(parsed);
                }
            }
        }
        None
    }

    /// Parse a `collision_bsp` carried inline as a sub-struct (rather
    /// than referenced through a resource-interface block). Tag schema
    /// labels this field `collision info` inside each
    /// `instanced geometries definitions[i]` entry — see Ares
    /// `structures/instanced_geometry_definitions.h:38` (the
    /// `collision_bsp bsp` field of `structure_instanced_geometry_definition`).
    /// The shape matches a `collision bsp` block element so the same
    /// small/large node parsers apply.
    pub fn from_inline_struct(entry: &TagStruct<'_>) -> Option<Self> {
        // Small-bsp shape uses `bsp3d nodes` with `node data designator`
        // (64-bit packed). Large-bsp shape uses `bsp3d nodes` with
        // `plane`/`back child`/`front child`. Try small first; fall
        // back to large.
        let small = parse_small_bsp3d(entry);
        if !small.nodes.is_empty() {
            return Some(small);
        }
        let large = parse_large_bsp3d(entry);
        if !large.nodes.is_empty() {
            return Some(large);
        }
        None
    }
}

fn parse_small_bsp3d(entry: &TagStruct<'_>) -> Bsp3d {
    // 64-bit packed: bits 0-15 plane, 16-39 below (24b, bit 23 = leaf),
    // 40-63 above (24b, bit 23 = leaf). Re-encode into canonical
    // sign-bit-leaf form: leaf_index → `leaf_index | 0x8000_0000`.
    let to_canonical = |raw24: u32| -> i32 {
        if raw24 == 0x00FF_FFFF {
            -1 // engine sentinel: walker bails
        } else if (raw24 & 0x0080_0000) != 0 {
            let leaf_idx = raw24 & 0x007F_FFFF;
            (leaf_idx | Bsp3dNode::LEAF_FLAG) as i32
        } else {
            (raw24 & 0x007F_FFFF) as i32
        }
    };
    let nodes = entry
        .field("bsp3d nodes")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    let raw = e
                        .read_int_any("node data designator")
                        .unwrap_or(0) as u64;
                    let plane_index = (raw & 0xFFFF) as u16 as i16 as i32;
                    let below_raw24 = ((raw >> 16) & 0x00FF_FFFF) as u32;
                    let above_raw24 = ((raw >> 40) & 0x00FF_FFFF) as u32;
                    out.push(Bsp3dNode {
                        plane_index,
                        below_child: to_canonical(below_raw24),
                        above_child: to_canonical(above_raw24),
                    });
                }
            }
            out
        })
        .unwrap_or_default();
    let planes = read_planes(entry);
    let mut out = Bsp3d { nodes, planes, ..Bsp3d::default() };
    populate_collision_subblocks(entry, &mut out, true);
    out
}

fn parse_large_bsp3d(entry: &TagStruct<'_>) -> Bsp3d {
    // 3 × i32: plane / back_child / front_child. Engine convention:
    // child >= 0 = node index, child < 0 with bit 31 set = leaf
    // (leaf_index = child & 0x7FFFFFFF). back = below.
    let nodes = entry
        .field("bsp3d nodes")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    let plane_index = e.read_int_any("plane").unwrap_or(0) as i32;
                    let below_child = e.read_int_any("back child").unwrap_or(-1) as i32;
                    let above_child = e.read_int_any("front child").unwrap_or(-1) as i32;
                    out.push(Bsp3dNode { plane_index, below_child, above_child });
                }
            }
            out
        })
        .unwrap_or_default();
    let planes = read_planes(entry);
    let mut out = Bsp3d { nodes, planes, ..Bsp3d::default() };
    populate_collision_subblocks(entry, &mut out, false);
    out
}

fn populate_collision_subblocks(entry: &TagStruct<'_>, bsp: &mut Bsp3d, is_small_format: bool) {
    // Normalize a high-bit-flag value (designator: bit-15-negate in
    // SMALL, bit-31-negate in LARGE, OR child: bit-15-leaf / bit-31-
    // leaf) into canonical bit-31 form. `0xFFFF` / `-1` are the
    // engine miss sentinels and preserved as i32 `-1`. Fixes the
    // i16-truncation hang that powered the s3d_powerhouse decal
    // freeze — see `CollisionBsp2dNode` for context.
    let to_canonical_31 = |raw: i128| -> i32 {
        if is_small_format {
            let v16 = raw as i16;
            if v16 == -1 {
                -1
            } else if (v16 as u16) & 0x8000 != 0 {
                let idx = (v16 as u16 & 0x7FFF) as u32;
                (idx | 0x8000_0000) as i32
            } else {
                v16 as i32
            }
        } else {
            raw as i32
        }
    };

    bsp.leaves = entry
        .field("leaves")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(CollisionLeaf {
                        flags: e.read_int_any("flags").unwrap_or(0) as u16,
                        bsp2d_reference_count: e
                            .read_int_any("bsp2d reference count")
                            .unwrap_or(0) as i16,
                        first_bsp2d_reference: e
                            .read_int_any("first bsp2d reference")
                            .unwrap_or(0) as i32,
                    });
                }
            }
            out
        })
        .unwrap_or_default();

    bsp.bsp2d_references = entry
        .field("bsp2d references")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(CollisionBsp2dReference {
                        plane_designator: to_canonical_31(
                            e.read_int_any("plane").unwrap_or(0),
                        ),
                        bsp2d_node: to_canonical_31(
                            e.read_int_any("bsp2d node").unwrap_or(0),
                        ),
                    });
                }
            }
            out
        })
        .unwrap_or_default();

    bsp.bsp2d_nodes = entry
        .field("bsp2d nodes")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    let plane = match e.field("plane").and_then(|f| f.value()) {
                        Some(TagFieldData::RealPlane2d(p)) => p,
                        _ => RealPlane2d::default(),
                    };
                    out.push(CollisionBsp2dNode {
                        plane,
                        left_child: to_canonical_31(e.read_int_any("left child").unwrap_or(0)),
                        right_child: to_canonical_31(e.read_int_any("right child").unwrap_or(0)),
                    });
                }
            }
            out
        })
        .unwrap_or_default();

    bsp.surfaces = entry
        .field("surfaces")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(CollisionSurface {
                        plane_designator: to_canonical_31(
                            e.read_int_any("plane").unwrap_or(0),
                        ),
                        first_edge: e.read_int_any("first edge").unwrap_or(0) as i32,
                        material: e.read_int_any("material").unwrap_or(-1) as i16,
                        breakable_surface_set: e
                            .read_int_any("breakable surface set")
                            .unwrap_or(-1) as i16,
                        breakable_surface: e
                            .read_int_any("breakable surface")
                            .unwrap_or(-1) as i16,
                        flags: e.read_int_any("flags").unwrap_or(0) as u8,
                        best_plane_vertex_index: e
                            .read_int_any("best plane calculation vertex index ")
                            .unwrap_or(0) as i8,
                    });
                }
            }
            out
        })
        .unwrap_or_default();

    bsp.edges = entry
        .field("edges")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(CollisionEdge {
                        start_vertex: e.read_int_any("start vertex").unwrap_or(0) as i32,
                        end_vertex: e.read_int_any("end vertex").unwrap_or(0) as i32,
                        forward_edge: e.read_int_any("forward edge").unwrap_or(0) as i32,
                        reverse_edge: e.read_int_any("reverse edge").unwrap_or(0) as i32,
                        left_surface: e.read_int_any("left surface").unwrap_or(-1) as i32,
                        right_surface: e.read_int_any("right surface").unwrap_or(-1) as i32,
                    });
                }
            }
            out
        })
        .unwrap_or_default();

    bsp.vertices = entry
        .field("vertices")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    let point = e.read_point3d("point");
                    out.push(CollisionVertex {
                        point,
                        first_edge: e.read_int_any("first edge").unwrap_or(0) as i32,
                    });
                }
            }
            out
        })
        .unwrap_or_default();
}

fn read_planes(entry: &TagStruct<'_>) -> Vec<RealPlane3d> {
    entry
        .field("planes")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    let plane = match e.field("plane").and_then(|f| f.value()) {
                        Some(TagFieldData::RealPlane3d(p)) => p,
                        _ => RealPlane3d::default(),
                    };
                    out.push(plane);
                }
            }
            out
        })
        .unwrap_or_default()
}

/// One BSP3D leaf node entry — schema
/// `structure_bsp_leaf_block` (1B per entry). The BSP3D collision
/// tree's leaves index into this table; the entry's `cluster` field
/// is the cluster index a world-position falling into that leaf
/// belongs to.
///
/// Engine: `c_structure_bsp_leaf` in `structure_bsp_definitions.h`.
/// Used by `scenario_location_from_point @ 0x18017BFE0` to convert
/// camera position → `s_cluster_reference`.
#[derive(Debug, Clone, Copy, Default)]
pub struct BspLeaf {
    /// `cluster*` — block index into `StructureBsp::clusters` (i8,
    /// -1 = leaf is outside any cluster, e.g. solid space).
    pub cluster: i8,
}

impl BspLeaf {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cluster: s.read_int_any("cluster").unwrap_or(-1) as i8,
        }
    }
}

/// One cluster — a spatial partition of the BSP. Each cluster has one
/// opaque mesh in the BSP's render_geometry (indexed by `mesh_index`).
#[derive(Debug, Clone, Default)]
pub struct BspCluster {
    pub bounds_x: RealBounds,
    pub bounds_y: RealBounds,
    pub bounds_z: RealBounds,
    /// `scenario sky index` — i8 — which scenario sky covers this
    /// cluster. -1 if no sky.
    pub scenario_sky_index: i8,
    /// `atmosphere index` — fog/atmosphere reference; -1 if none.
    pub atmosphere_index: i8,
    /// `camera fx index` — camera FX overlay; -1 if none.
    pub camera_fx_index: i8,
    /// `mesh index` — into [`StructureBsp::meshes_metadata`].
    pub mesh_index: i16,
    /// `flags` (cluster runtime flags).
    pub flags: u16,
    /// Portal block indices into [`StructureBsp::cluster_portals`].
    pub portals: Vec<i16>,
}

impl BspCluster {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            bounds_x: s.read_real_bounds("bounds x"),
            bounds_y: s.read_real_bounds("bounds y"),
            bounds_z: s.read_real_bounds("bounds z"),
            scenario_sky_index: s.read_int_any("scenario sky index").unwrap_or(-1) as i8,
            atmosphere_index: s.read_int_any("atmosphere index").unwrap_or(-1) as i8,
            camera_fx_index: s.read_int_any("camera fx index").unwrap_or(-1) as i8,
            mesh_index: s.read_int_any("mesh index").unwrap_or(-1) as i16,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            portals: s
                .field("portals")
                .and_then(|f| f.as_block())
                .map(|b| {
                    let mut out = Vec::with_capacity(b.len());
                    for i in 0..b.len() {
                        if let Some(e) = b.element(i) {
                            out.push(e.read_int_any("portal index").unwrap_or(-1) as i16);
                        }
                    }
                    out
                })
                .unwrap_or_default(),
        }
    }
}

/// One instanced-geometry instance — one placement of a reusable mesh.
/// World transform stored as scale + 3-column orthonormal basis +
/// position. The mesh referenced is `definition_index → render geometry
/// meshes[def.mesh_index]`.
#[derive(Debug, Clone, Default)]
pub struct BspInstance {
    pub scale: f32,
    pub forward: RealVector3d,
    pub left: RealVector3d,
    pub up: RealVector3d,
    pub position: RealPoint3d,
    /// `instance definition` block index — see runtime documentation
    /// for how this maps to render-geometry meshes.
    pub definition_index: i16,
    pub flags: u16,
    /// `lightmap texcoord block index` — into per_instance_lightmap_texcoords.
    pub lightmap_texcoord_block_index: i16,
    pub world_bounding_sphere_center: RealPoint3d,
    pub world_bounding_sphere_radius: f32,
    /// `name` — string_id, for debugging / identification.
    pub name: String,
    /// `pathfinding policy` enum index.
    pub pathfinding_policy: i16,
    /// `lightmapping policy` enum: 0 = per_pixel, 1 = per_vertex,
    /// 2 = single_probe, 3 = per_pixel_shared, 4 = no_lightmaps,
    /// 5 = ... (numbering differs across MCC builds — check by name).
    pub lightmapping_policy: i16,
    pub lightmap_resolution_scale: f32,
}

impl BspInstance {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            scale: s.read_real("scale").unwrap_or(1.0),
            forward: s.read_vec3("forward"),
            left: s.read_vec3("left"),
            up: s.read_vec3("up"),
            position: s.read_point3d("position"),
            definition_index: s.read_block_index("instance definition"),
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            lightmap_texcoord_block_index: s
                .read_int_any("lightmap texcoord block index")
                .unwrap_or(-1) as i16,
            world_bounding_sphere_center: s.read_point3d("world bounding sphere center"),
            world_bounding_sphere_radius: s
                .read_real("world bounding sphere radius")
                .unwrap_or(0.0),
            name: s.read_string_id("name").unwrap_or_default(),
            pathfinding_policy: s.read_int_any("pathfinding policy").unwrap_or(0) as i16,
            lightmapping_policy: s.read_int_any("lightmapping policy").unwrap_or(0) as i16,
            lightmap_resolution_scale: s.read_real("lightmap resolution scale").unwrap_or(1.0),
        }
    }
}

/// One cluster-portal — connectivity between two clusters. Schema
/// `structure_bsp_cluster_portal_block` (40B). Polygon vertices live
/// in the inline `vertices*` sub-block (each entry is one
/// `real_point_3d`, 12B). Engine reads the polygon for portal-frustum
/// clipping in `visibility_build_region_from_projections @ 0x180508520`
/// → `transform_portal @ 0x180508FB0`.
#[derive(Debug, Clone, Default)]
pub struct BspClusterPortal {
    /// `back cluster*` — block index into `StructureBsp::clusters`.
    pub back_cluster: i16,
    /// `front cluster*` — block index into `StructureBsp::clusters`.
    pub front_cluster: i16,
    /// `plane index*` — index into the BSP's planes block (sign bit
    /// indicates plane direction, like Halo's `plane_designator`).
    pub plane_index: i32,
    /// `centroid*` — average of vertex positions; used for portal
    /// activation distance + initial cull tests.
    pub centroid: RealPoint3d,
    /// `bounding radius*` — max distance from centroid to any vertex;
    /// fast pre-cull bound for portal visibility.
    pub bounding_radius: f32,
    pub flags: u32,
    /// Portal polygon (3-or-more vertices, 5 max in practice). Order
    /// is wound CCW when viewed from the front cluster.
    pub vertices: Vec<RealPoint3d>,
}

impl BspClusterPortal {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            back_cluster: s.read_block_index("back cluster"),
            front_cluster: s.read_block_index("front cluster"),
            plane_index: s.read_int_any("plane index").unwrap_or(-1) as i32,
            centroid: s.read_point3d("centroid"),
            bounding_radius: s.read_real("bounding radius").unwrap_or(0.0),
            flags: s.read_int_any("flags").unwrap_or(0) as u32,
            vertices: s
                .field("vertices")
                .and_then(|f| f.as_block())
                .map(|b| {
                    let mut out = Vec::with_capacity(b.len());
                    for i in 0..b.len() {
                        if let Some(e) = b.element(i) {
                            out.push(e.read_point3d("point"));
                        }
                    }
                    out
                })
                .unwrap_or_default(),
        }
    }
}

/// One mesh's metadata in `render geometry/meshes[i]`. Parts within
/// store a `render_method_index` into [`StructureBsp::materials`].
/// Vertex / index data is decoded separately.
#[derive(Debug, Clone, Default)]
pub struct BspMeshMetadata {
    pub parts: Vec<BspMeshPart>,
    /// `vertex type` enum: 1 = rigid, 2 = skinned, 3 = ambient_prt,
    /// 4 = linear_prt, 5 = quadratic_prt, 6 = static_prt, ... (varies).
    pub vertex_type: i32,
    pub mesh_flags: u8,
    pub rigid_node_index: i8,
    /// `index buffer type`: 3 = triangle list, 0 = triangle strip.
    pub index_buffer_type: i32,
}

impl BspMeshMetadata {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            parts: read_block(s, "parts", BspMeshPart::from_struct),
            vertex_type: s.read_int_any("vertex type").unwrap_or(1) as i32,
            mesh_flags: s.read_int_any("mesh flags").unwrap_or(0) as u8,
            rigid_node_index: s.read_int_any("rigid node index").unwrap_or(-1) as i8,
            index_buffer_type: s.read_int_any("index buffer type").unwrap_or(3) as i32,
        }
    }
}

/// One part of a BSP mesh — a draw-call range. `render_method_index`
/// indexes into [`StructureBsp::materials`].
#[derive(Debug, Clone, Default)]
pub struct BspMeshPart {
    pub render_method_index: i16,
    pub transparent_sorting_index: i16,
    pub index_start: u16,
    pub index_count: u16,
    pub subpart_start: u16,
    pub subpart_count: u16,
    pub part_type: i8,
    pub part_flags: u8,
    pub budget_vertex_count: u16,
}

impl BspMeshPart {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            render_method_index: s.read_block_index("render method index"),
            transparent_sorting_index: s.read_block_index("transparent sorting index"),
            index_start: s.read_int_any("index start").unwrap_or(0) as u16,
            index_count: s.read_int_any("index count").unwrap_or(0) as u16,
            subpart_start: s.read_int_any("subpart start").unwrap_or(0) as u16,
            subpart_count: s.read_int_any("subpart count").unwrap_or(0) as u16,
            part_type: s.read_int_any("part type").unwrap_or(0) as i8,
            part_flags: s.read_int_any("part flags").unwrap_or(0) as u8,
            budget_vertex_count: s.read_int_any("budget vertex count").unwrap_or(0) as u16,
        }
    }
}

/// One instance definition — mesh + compression bounds reference for
/// reusable instanced geometry. Placements (`BspInstance::definition_index`)
/// reference these by index.
///
/// Path: `resource interface/raw_resources[0]/raw_items/instanced
/// geometries definitions[i]`.
#[derive(Debug, Clone, Default)]
pub struct BspInstanceDefinition {
    pub checksum: i32,
    pub bounding_sphere_center: RealPoint3d,
    pub bounding_sphere_radius: f32,
    /// Which mesh in `render_geometry/meshes[]` this def's geometry uses.
    pub mesh_index: i16,
    /// Which `render_geometry/compression_info[]` entry decompresses
    /// this def's vertex positions + texcoords.
    pub compression_index: i16,
    pub global_lightmap_resolution_scale: f32,
    /// Inline `collision_bsp` for this definition — schema field
    /// `collision info`. Ares
    /// `structure_instanced_geometry_definition::bsp` at offset 0x14.
    /// Used by `instanced_geometry_test_vector_internal @ 0x180400170`
    /// to raycast against per-instance geometry in instance-local
    /// space.
    pub bsp: Option<Bsp3d>,

    /// `render bsp[i]` count — Ares
    /// `structure_instanced_geometry_definition::render_bsp` (`s_tag_block`
    /// at offset 0x74). When non-zero the definition is "render-only":
    /// instances of this def carry geometry but no collision data.
    /// Engine `s_decal_collision_result::decalable_surface() const @
    /// 0x18039FE50` rejects decal hits on such instances. We only need
    /// the count for that predicate, so we don't materialize the
    /// contents.
    pub render_bsp_count: usize,
}

impl BspInstanceDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let bsp = s
            .field("collision info")
            .and_then(|f| f.as_struct())
            .and_then(|cs| Bsp3d::from_inline_struct(&cs));
        let render_bsp_count = s
            .field("render bsp")
            .and_then(|f| f.as_block())
            .map(|b| b.len())
            .unwrap_or(0);
        Self {
            checksum: s.read_int_any("checksum").unwrap_or(0) as i32,
            bounding_sphere_center: s.read_point3d("bounding sphere center"),
            bounding_sphere_radius: s.read_real("bounding sphere radius").unwrap_or(0.0),
            mesh_index: s.read_int_any("mesh index").unwrap_or(-1) as i16,
            compression_index: s.read_int_any("compression index").unwrap_or(-1) as i16,
            global_lightmap_resolution_scale: s
                .read_real("global lightmap resolution scale")
                .unwrap_or(1.0),
            bsp,
            render_bsp_count,
        }
    }
}

/// One marker placed in the BSP — name + position + node ref.
#[derive(Debug, Clone, Default)]
pub struct BspMarker {
    pub name: String,
    pub node_index: i16,
    pub position: RealPoint3d,
}

impl BspMarker {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            node_index: s.read_block_index("node index"),
            position: s.read_point3d("position"),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|b| read_block_vec(&b, f))
        .unwrap_or_default()
}

fn read_block_vec<T, F>(block: &TagBlock<'_>, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}
