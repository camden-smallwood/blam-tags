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
use crate::math::{RealBounds, RealPoint3d, RealVector3d};

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

/// One cluster-portal — connectivity between two clusters.
#[derive(Debug, Clone, Default)]
pub struct BspClusterPortal {
    pub front_cluster: i16,
    pub back_cluster: i16,
    /// Plane normal + offset (pre-classified plane).
    pub plane_index: i32,
    pub flags: u32,
    pub vertex_count: i16,
}

impl BspClusterPortal {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            front_cluster: s.read_block_index("front cluster"),
            back_cluster: s.read_block_index("back cluster"),
            plane_index: s.read_int_any("plane index").unwrap_or(-1) as i32,
            flags: s.read_int_any("flags").unwrap_or(0) as u32,
            vertex_count: s.read_int_any("vertex count").unwrap_or(0) as i16,
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
}

impl BspInstanceDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            checksum: s.read_int_any("checksum").unwrap_or(0) as i32,
            bounding_sphere_center: s.read_point3d("bounding sphere center"),
            bounding_sphere_radius: s.read_real("bounding sphere radius").unwrap_or(0.0),
            mesh_index: s.read_int_any("mesh index").unwrap_or(-1) as i16,
            compression_index: s.read_int_any("compression index").unwrap_or(-1) as i16,
            global_lightmap_resolution_scale: s
                .read_real("global lightmap resolution scale")
                .unwrap_or(1.0),
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
