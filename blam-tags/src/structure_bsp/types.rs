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
use crate::math::{RealBounds, RealPlane2d, RealPlane3d, RealPoint3d, RealQuaternion, RealVector3d};
use crate::typed_enums::{Enum, Flags};

/// `structure_bsp_flags_definition` (long_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum StructureBspFlags {
    #[strum(serialize = "lightmap compressed")] LightmapCompressed = 0,
}

/// `structure_cluster_flags` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum StructureClusterFlags {
    #[strum(serialize = "one way portal")] OneWayPortal = 0,
    #[strum(serialize = "door portal")] DoorPortal = 1,
    #[strum(serialize = "postprocessed geometry")] PostprocessedGeometry = 2,
    #[strum(serialize = "is the sky")] IsTheSky = 3,
    #[strum(serialize = "decorators are lit")] DecoratorsAreLit = 4,
}

/// `camera_fx_palette_flags` (byte_flags) — per-cluster cfxs override
/// gates on `structure_camera_fx_palette_entry`. Verified against
/// `c_camera_fx_values::update @ dllcache 0x180687CB0`: bit 3
/// (`OverrideInherentBloom`) gates BOTH the inherent-bloom and
/// bloom-intensity target overrides in that engine rev, and bit 4
/// (`OverrideBloomIntensity`) is defined in the schema but NOT consumed.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum CameraFxPaletteFlags {
    #[strum(serialize = "force exposure")] ForceExposure = 0,
    #[strum(serialize = "force auto-exposure")] ForceAutoExposure = 1,
    #[strum(serialize = "override exposure bounds")] OverrideExposureBounds = 2,
    #[strum(serialize = "override inherent bloom")] OverrideInherentBloom = 3,
    #[strum(serialize = "override bloom intensity")] OverrideBloomIntensity = 4,
}

/// `surface_flags` (byte_flags) — per collision surface. Verified against
/// `collision_bsp_test_vector_recursive @ dllcache 0x180513f80`: the
/// raycast filters on `Invisible` (bit 1) and `Breakable` (bit 3); the
/// decal "decalable" test rejects the `0x3B` set (TwoSided / Invisible /
/// Breakable / Invalid / Conveyor). NOTE: bit 0 is `TwoSided`, NOT bit 3 —
/// older comments calling bit 3 "two-sided" were wrong (it is `Breakable`).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum CollisionSurfaceFlags {
    #[strum(serialize = "two sided")] TwoSided = 0,
    #[strum(serialize = "invisible")] Invisible = 1,
    #[strum(serialize = "climbable")] Climbable = 2,
    #[strum(serialize = "breakable")] Breakable = 3,
    #[strum(serialize = "invalid")] Invalid = 4,
    #[strum(serialize = "conveyor")] Conveyor = 5,
    #[strum(serialize = "slip")] Slip = 6,
}

/// `leaf_flags` (byte_flags SMALL / word_flags LARGE). Bit 0 is the
/// "contains double-sided surfaces" content marker the engine reads in
/// `collision_bsp_test_vector_recursive` to build a leaf's contents code.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum CollisionLeafFlags {
    #[strum(serialize = "contains double-sided surfaces")] ContainsDoubleSidedSurfaces = 0,
}

/// `structure_bsp_cluster_portal_flags_definition` (long_flags) — per
/// cluster portal. Drives portal visibility / AI sound occlusion.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum StructureBspClusterPortalFlags {
    #[strum(serialize = "ai can't hear through this shit")] AiCantHearThroughThis = 0,
    #[strum(serialize = "one-way")] OneWay = 1,
    #[strum(serialize = "door")] Door = 2,
    #[strum(serialize = "no-way")] NoWay = 3,
    #[strum(serialize = "one-way-reversed")] OneWayReversed = 4,
    #[strum(serialize = "no one can hear through this")] NoOneCanHearThroughThis = 5,
}

/// `instanced_geometry_flags` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum InstancedGeometryFlags {
    #[strum(serialize = "not in lightprobes")] NotInLightprobes = 0,
    #[strum(serialize = "render only")] RenderOnly = 1,
    #[strum(serialize = "does not block aoe damage")] DoesNotBlockAoeDamage = 2,
    #[strum(serialize = "collidable")] Collidable = 3,
    #[strum(serialize = "decal spacing")] DecalSpacing = 4,
    #[strum(serialize = "not for render")] NotForRender = 5,
}

/// `instanced_geometry_pathfinding_policy_enum` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum InstancedGeometryPathfindingPolicy {
    #[default]
    #[strum(serialize = "cut-out")] CutOut = 0,
    #[strum(serialize = "static")] Static = 1,
    #[strum(serialize = "none")] None = 2,
}

/// `instanced_geometry_lightmapping_policy_enum` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum InstancedGeometryLightmappingPolicy {
    #[default]
    #[strum(serialize = "per-pixel seperate")] PerPixelSeparate = 0,
    #[strum(serialize = "per-vertex")] PerVertex = 1,
    #[strum(serialize = "single-probe")] SingleProbe = 2,
    #[strum(serialize = "per-pixel shared")] PerPixelShared = 3,
}

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
    pub flags: Flags<StructureBspFlags, u32>,
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

    /// `structure surfaces[i]` — one per collision polygon. Each entry
    /// references a contiguous range of [`Self::structure_surface_to_triangle_mappings`]
    /// via `first_mapping_index` + `mapping_count`. The runtime
    /// engine's `c_geometry_sampler::geometry_test_collision_result @
    /// 0x18048c620` reads this to map a BSP3D collision hit
    /// (`collision_surface_index`) back to the render geometry's
    /// triangle index range, then interpolates the lightmap UV from
    /// the render-vertex buffer.
    ///
    /// Ares: `structure_bsp_definitions.h:173`. 4 bytes per entry.
    pub structure_surfaces: Vec<StructureSurface>,

    /// `structure surface to triangle mapping[j]` — runs of triangle
    /// index ranges per mesh (= `section_index`). Each `structure_surface`
    /// owns a contiguous range of these. `last_index` is the END index
    /// in the mesh's index buffer for this run; the START is the
    /// previous mapping's `last_index` (or 0 for the surface's first
    /// mapping). `section_index` selects which render-geometry mesh
    /// the run belongs to.
    ///
    /// Ares: `structure_bsp_definitions.h:180`. 4 bytes per entry.
    pub structure_surface_to_triangle_mappings: Vec<StructureSurfaceTriangleMapping>,

    // ---- maximal coverage: the remaining root blocks ----
    pub import_info_checksum: i32,
    pub import_version: i32,
    pub visible_name: String,
    pub seam_identifiers: Vec<super::StructureSeamMapping>,
    pub edge_to_seam_edges: Vec<super::EdgeToSeamEdge>,
    pub large_structure_surfaces: Vec<super::StructureSurfaceLarge>,
    pub weather_polyhedra: Vec<super::WeatherPolyhedron>,
    pub detail_objects: Vec<super::DetailObjectData>,
    pub conveyor_surfaces: Vec<super::ConveyorSurface>,
    pub breakable_surface_sets: Vec<super::BreakableSurfaceSet>,
    pub pathfinding_data: Vec<super::PathfindingData>,
    /// `pathfinding edges` — packed midpoint bytes.
    pub pathfinding_edges: Vec<i8>,
    pub acoustics_palette: Vec<super::AcousticsPalette>,
    pub background_sound_palette: Vec<super::BackgroundSoundPalette>,
    pub sound_environment_palette: Vec<super::SoundEnvironmentPalette>,
    /// `sound PAS data` — opaque encoded cluster PAS blob.
    pub sound_pas_data: Vec<u8>,
    pub marker_light_palette: Vec<super::MarkerLightPalette>,
    /// `marker light palette index` — per-marker palette indices.
    pub marker_light_palette_indices: Vec<i16>,
    pub runtime_decals: Vec<super::RuntimeDecal>,
    pub environment_object_palette: Vec<super::EnvironmentObjectPalette>,
    pub environment_objects: Vec<super::EnvironmentObject>,
    pub leaf_map_leaves: Vec<super::MapLeaf>,
    pub leaf_map_connections: Vec<super::LeafConnection>,
    pub errors: Vec<super::ErrorReportCategory>,
    /// `decorator sets` — referenced `decorator_set` (dctr) tag paths.
    pub decorator_sets: Vec<String>,
    pub acoustics_sound_clusters: Vec<super::SoundCluster>,
    pub ambience_sound_clusters: Vec<super::SoundCluster>,
    pub reverb_sound_clusters: Vec<super::SoundCluster>,
    pub transparent_planes: Vec<super::TransparentPlane>,
    /// `debug info` — 0-or-1 element (block, max_count 1).
    pub debug_info: Vec<super::DebugInfo>,
    /// `audibility` — 0-or-1 element (block).
    pub audibility: Vec<super::Audibility>,
    /// `object fake lightprobes` — placed-object identifiers.
    pub fake_lightprobes: Vec<super::ScenarioObjectId>,
    /// `widget references` — (marker index, widget tag path) pairs.
    pub widget_references: Vec<(i16, String)>,
    /// `structure_physics` — Havok world MOPP + breakable-surface data.
    pub structure_physics: super::StructurePhysics,
    /// `render geometry` — the full renderable mesh data (vertex/index
    /// buffers + compression info), reusing the `render_model` reader.
    /// `meshes_metadata` is the lightweight per-mesh subset of this.
    pub render_geometry: Option<crate::render_model::Geometry>,
    /// `decorator instance buffer` — packed decorator (grass) instance
    /// geometry, same `global_render_geometry_struct` schema.
    pub decorator_instance_buffer: Option<crate::render_model::Geometry>,
    /// `use resource items` — resource-interface paging flag.
    pub use_resource_items: i32,
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
            flags: s.try_read_flags("flags").unwrap_or_default(),
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
            structure_surfaces: read_block(
                s,
                "structure surfaces",
                StructureSurface::from_struct,
            ),
            structure_surface_to_triangle_mappings: read_block(
                s,
                "structure surface to triangle mapping",
                StructureSurfaceTriangleMapping::from_struct,
            ),

            import_info_checksum: s.read_int_any("import info checksum").unwrap_or(0) as i32,
            import_version: s.read_int_any("import version").unwrap_or(0) as i32,
            visible_name: s.read_string_id("visible name").unwrap_or_default(),
            seam_identifiers: read_block(s, "seam identifiers", super::StructureSeamMapping::from_struct),
            edge_to_seam_edges: read_block(s, "edge to seam edge", super::EdgeToSeamEdge::from_struct),
            large_structure_surfaces: read_block(s, "large structure surfaces", super::StructureSurfaceLarge::from_struct),
            weather_polyhedra: read_block(s, "weather polyhedra", super::WeatherPolyhedron::from_struct),
            detail_objects: read_block(s, "detail objects", super::DetailObjectData::from_struct),
            conveyor_surfaces: read_block(s, "conveyor surfaces", super::ConveyorSurface::from_struct),
            breakable_surface_sets: read_block(s, "breakable surface sets", super::BreakableSurfaceSet::from_struct),
            pathfinding_data: read_block(s, "pathfinding data", super::PathfindingData::from_struct),
            pathfinding_edges: read_block(s, "pathfinding edges", |e| e.read_int_any("midpoint").unwrap_or(0) as i8),
            acoustics_palette: read_block(s, "acoustics palette", super::AcousticsPalette::from_struct),
            background_sound_palette: read_block(s, "background sound palette", super::BackgroundSoundPalette::from_struct),
            sound_environment_palette: read_block(s, "sound environment palette", super::SoundEnvironmentPalette::from_struct),
            sound_pas_data: super::common::read_data(s, "sound PAS data"),
            marker_light_palette: read_block(s, "marker light palette", super::MarkerLightPalette::from_struct),
            marker_light_palette_indices: read_block(s, "marker light palette index", |e| e.read_int_any("palette index").unwrap_or(-1) as i16),
            runtime_decals: read_block(s, "runtime decals", super::RuntimeDecal::from_struct),
            environment_object_palette: read_block(s, "environment object palette", super::EnvironmentObjectPalette::from_struct),
            environment_objects: read_block(s, "environment objects", super::EnvironmentObject::from_struct),
            leaf_map_leaves: read_block(s, "leaf map leaves", super::MapLeaf::from_struct),
            leaf_map_connections: read_block(s, "leaf map connections", super::LeafConnection::from_struct),
            errors: read_block(s, "errors", super::ErrorReportCategory::from_struct),
            decorator_sets: read_block(s, "decorator sets", |e| e.read_tag_ref_path("decorator set reference").unwrap_or_default()),
            acoustics_sound_clusters: read_block(s, "acoustics sound clusters", super::SoundCluster::from_struct),
            ambience_sound_clusters: read_block(s, "ambience sound clusters", super::SoundCluster::from_struct),
            reverb_sound_clusters: read_block(s, "reverb sound clusters", super::SoundCluster::from_struct),
            transparent_planes: read_block(s, "transparent planes", super::TransparentPlane::from_struct),
            debug_info: read_block(s, "debug info", super::DebugInfo::from_struct),
            audibility: read_block(s, "audibility", super::Audibility::from_struct),
            fake_lightprobes: read_block(s, "object fake lightprobes", |e| {
                super::common::read_struct(e, "object identifier", super::ScenarioObjectId::from_struct)
            }),
            widget_references: read_block(s, "widget references", |e| {
                (
                    e.read_int_any("marker index").unwrap_or(-1) as i16,
                    e.read_tag_ref_path("widget ref").unwrap_or_default(),
                )
            }),
            structure_physics: super::common::read_struct(
                s,
                "structure_physics",
                super::StructurePhysics::from_struct,
            ),
            render_geometry: s
                .field("render geometry")
                .and_then(|f| f.as_struct())
                .map(|rg| crate::render_model::read_geometry_from(&rg)),
            decorator_instance_buffer: s
                .field("decorator instance buffer")
                .and_then(|f| f.as_struct())
                .map(|rg| crate::render_model::read_geometry_from(&rg)),
            use_resource_items: s
                .field_path("resource interface/use resource items")
                .and_then(|f| f.value())
                .and_then(|v| match v {
                    crate::fields::TagFieldData::LongInteger(n) => Some(n),
                    _ => None,
                })
                .unwrap_or(0),
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
///   `ForceExposure` → `forced_exposure` overrides exposure target (clears AutoAdjustTarget).
///   `ForceAutoExposure` → `forced_auto_exposure_brightness` overrides AUTO target (sets AutoAdjustTarget).
///   `OverrideExposureBounds` → `exposure_min` / `exposure_max` override clamp range.
///   `OverrideInherentBloom` → `inherent_bloom` + `bloom_intensity` override bloom params
///                             (engine 0x180687CB0 gates BOTH on this single bit).
///
/// Engine struct layout (`structure_camera_fx_palette_entry`, 48 B):
///   `name` (i32 string_id, 4) + `camera_fx_tag` (TagRef, 16) +
///   `flags` (u8) + 3B pad + 6×f32 values.
#[derive(Debug, Clone, Default)]
pub struct BspCameraFxPaletteEntry {
    /// `name^` (string_id) — author-friendly name.
    pub name: String,
    /// `flags` (engine `structure_camera_fx_palette_entry.flags`),
    /// schema-name-resolved into [`CameraFxPaletteFlags`].
    pub flags: Flags<CameraFxPaletteFlags, u8>,
    /// `forced exposure` (stops). Active on `ForceExposure`.
    pub forced_exposure: f32,
    /// `forced auto exposure brightness` (stops). Active on `ForceAutoExposure`.
    pub forced_auto_exposure_brightness: f32,
    /// `exposure min` (stops). Active on `OverrideExposureBounds`.
    pub exposure_min: f32,
    /// `exposure max` (stops). Active on `OverrideExposureBounds`.
    pub exposure_max: f32,
    /// `inherent bloom`. Active on `OverrideInherentBloom`.
    pub inherent_bloom: f32,
    /// `bloom intensity`. Active on `OverrideInherentBloom` (same bit).
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
            // Field is NAMED "flags" (def `camera_fx_palette_flags`). The
            // prior `read_int_any("camera_fx_palette_flags")` read the def
            // name, not the field name, so it always missed → flags == 0
            // and the whole cluster-override path was dead. try_read_flags
            // resolves the real "flags" field by name.
            flags: s.try_read_flags("flags").unwrap_or_default(),
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
// structure_surface / structure_surface_to_triangle_mapping
// =============================================================================
//
// Bridge between BSP3D collision surfaces and render-geometry
// triangles. Engine `c_geometry_sampler::geometry_test_collision_result @
// dllcache 0x18048c620` uses this chain to map a collision raycast hit
// back to a render triangle's lightmap UV.
//
// Lookup flow per surface_index:
//   ss = structure_surfaces[surface_index]
//   for j in 0..ss.mapping_count:
//     m = structure_surface_to_triangle_mappings[ss.first_mapping_index + j]
//     // Render mesh m.section_index has an index run ending at m.last_index;
//     // its start is the previous mapping's last_index (or 0 for first).
//     // Each triangle in that run is a candidate; pick the closest hit.

/// One collision-polygon → render-triangle-range descriptor. Owns a
/// contiguous run of [`StructureBsp::structure_surface_to_triangle_mappings`]
/// entries describing which render triangles (potentially across
/// multiple meshes) cover this collision polygon. Ares
/// `structure_bsp_definitions.h:173`. 4 bytes per entry.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureSurface {
    /// `first_structure_surface_to_triangle_mapping_index` — block
    /// index into [`StructureBsp::structure_surface_to_triangle_mappings`].
    pub first_mapping_index: u16,
    /// `structure_surface_to_triangle_mapping_count` — number of
    /// consecutive entries in that block owned by this surface.
    pub mapping_count: u16,
}

impl StructureSurface {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        // MCC schema uses underscored field names for this block
        // (vs. spaced for most other blocks). Verified 2026-05-26
        // against ghosttown.scenario_structure_bsp via fields()
        // dump. Ares header has the same names.
        Self {
            first_mapping_index: s
                .read_int_any("first_structure_surface_to_triangle_mapping_index")
                .unwrap_or(0) as u16,
            mapping_count: s
                .read_int_any("structure_surface_to_triangle_mapping_count")
                .unwrap_or(0) as u16,
        }
    }
}

/// One entry in the per-surface triangle list. Each mapping is a
/// DIRECT triangle reference (NOT an end-of-run indicator, despite the
/// Ares field name `last_index`). `triangle_index` is the index of the
/// triangle within `section_index`'s render mesh — specifically, the
/// base of a 3-index triangle in that mesh's index buffer
/// (i.e., index buffer positions `triangle_index..triangle_index+3`).
/// Verified 2026-05-26 against ghosttown.scenario_structure_bsp
/// (mapping[0] = `triangle_index=28925, section_index=40`).
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureSurfaceTriangleMapping {
    /// Index into the mesh's index buffer pointing at the first
    /// vertex of a 3-vertex triangle.
    pub triangle_index: u16,
    /// Block index into `render_geometry.meshes` — which render mesh
    /// the triangle lives in.
    pub section_index: u16,
}

impl StructureSurfaceTriangleMapping {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            triangle_index: s.read_int_any("triangle_index").unwrap_or(0) as u16,
            section_index: s.read_int_any("section_index").unwrap_or(0) as u16,
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
#[derive(Debug, Clone, Default)]
pub struct CollisionLeaf {
    /// `flags*`. `ContainsDoubleSidedSurfaces` (bit 0) per the engine
    /// `collision_bsp_test_vector_recursive` contents logic.
    pub flags: Flags<CollisionLeafFlags, u16>,
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
#[derive(Debug, Clone, Default)]
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
    /// `flags*` (byte_flags `surface_flags`). The collision raycast
    /// filters on `Invisible` (bit 1) and `Breakable` (bit 3); the decal
    /// "decalable" test rejects the `0x3B` set.
    pub flags: Flags<CollisionSurfaceFlags, u8>,
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

    // Collision-BSP topology indices (`first_edge`, `forward/reverse_edge`,
    // `start/end_vertex`, `left/right_surface`) are `u16` with `0xFFFF` = NONE —
    // the engine reads them as `unsigned __int16` (e.g. `build_mesh_fragment_
    // recursive @ 0x18039C2D0`). `read_int_any` returns the schema's SIGNED
    // int16, so any index >= 0x8000 (BSPs with > 32767 edges, e.g. s3d_turf's
    // 47814) comes out negative and indexes out of bounds — collapsing the
    // decal-mesh edge-ring walk to an empty polygon (the s3d_turf bunker snow
    // that fell back to a flat quad). Reinterpret the low 16 bits as unsigned;
    // `0xFFFF` stays NONE (`-1`). Small BSPs (cyberdyne, 10216 edges) are
    // unaffected — no index reaches the sign bit — so existing tests hold.
    let collision_index = |raw: Option<i128>| -> i32 {
        let v = raw.unwrap_or(0xFFFF) as u16;
        if v == 0xFFFF { -1 } else { v as i32 }
    };

    bsp.leaves = entry
        .field("leaves")
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(CollisionLeaf {
                        flags: e.try_read_flags("flags").unwrap_or_default(),
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
                        first_edge: collision_index(e.read_int_any("first edge")),
                        material: e.read_int_any("material").unwrap_or(-1) as i16,
                        breakable_surface_set: e
                            .read_int_any("breakable surface set")
                            .unwrap_or(-1) as i16,
                        breakable_surface: e
                            .read_int_any("breakable surface")
                            .unwrap_or(-1) as i16,
                        flags: e.try_read_flags("flags").unwrap_or_default(),
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
                        start_vertex: collision_index(e.read_int_any("start vertex")),
                        end_vertex: collision_index(e.read_int_any("end vertex")),
                        forward_edge: collision_index(e.read_int_any("forward edge")),
                        reverse_edge: collision_index(e.read_int_any("reverse edge")),
                        left_surface: collision_index(e.read_int_any("left surface")),
                        right_surface: collision_index(e.read_int_any("right surface")),
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
                        first_edge: collision_index(e.read_int_any("first edge")),
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
    pub flags: Flags<StructureClusterFlags, u16>,
    /// Portal block indices into [`StructureBsp::cluster_portals`].
    pub portals: Vec<i16>,

    // ---- maximal coverage: cluster sub-fields ----
    /// `acoustics` — block index into [`StructureBsp::acoustics_palette`].
    pub acoustics: i16,
    pub acoustics_sound_cluster_index: i16,
    /// `background sound` — index into [`StructureBsp::background_sound_palette`].
    pub background_sound: i16,
    /// `sound environment` — index into [`StructureBsp::sound_environment_palette`].
    pub sound_environment: i16,
    /// `weather` — index into [`StructureBsp::weather_palette`].
    pub weather: i16,
    pub background_sound_sound_cluster_index: i16,
    pub reverb_sound_cluster_index: i16,
    pub runtime_first_decal_index: i16,
    /// `runtime decal cound` (sic — schema typo).
    pub runtime_decal_count: i16,
    pub collision_instanced_geometry: super::ClusterCollisionInstancedGeometry,
    /// `seam indices`.
    pub seam_indices: Vec<i8>,
    pub decorator_groups: Vec<super::DecoratorRuntimeCluster>,
    pub pvs_bound_object_identifiers: Vec<super::ScenarioObjectId>,
    pub pvs_bound_object_references: Vec<super::ScenarioObjectReference>,
    pub cluster_cubemaps: Vec<super::ClusterCubemap>,
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
            flags: s.try_read_flags("flags").unwrap_or_default(),
            portals: read_block(s, "portals", |e| e.read_int_any("portal index").unwrap_or(-1) as i16),

            acoustics: s.read_block_index("acoustics"),
            acoustics_sound_cluster_index: s.read_int_any("acoustics sound cluster index").unwrap_or(-1) as i16,
            background_sound: s.read_block_index("background sound"),
            sound_environment: s.read_block_index("sound environment"),
            weather: s.read_block_index("weather"),
            background_sound_sound_cluster_index: s.read_int_any("background sound sound cluster index").unwrap_or(-1) as i16,
            reverb_sound_cluster_index: s.read_int_any("reverb sound cluster index").unwrap_or(-1) as i16,
            runtime_first_decal_index: s.read_int_any("runtime first decal index").unwrap_or(-1) as i16,
            runtime_decal_count: s.read_int_any("runtime decal cound").unwrap_or(0) as i16,
            collision_instanced_geometry: super::common::read_struct(
                s,
                "collision instanced geometry",
                super::ClusterCollisionInstancedGeometry::from_struct,
            ),
            seam_indices: read_block(s, "seam indices", |e| e.read_int_any("seam index").unwrap_or(-1) as i8),
            decorator_groups: read_block(s, "decorator groups", super::DecoratorRuntimeCluster::from_struct),
            pvs_bound_object_identifiers: read_block(s, "pvs bound object identifiers", |e| {
                super::common::read_struct(e, "object ID", super::ScenarioObjectId::from_struct)
            }),
            pvs_bound_object_references: read_block(s, "pvs bound object references", |e| {
                super::common::read_struct(e, "scenario object reference", super::ScenarioObjectReference::from_struct)
            }),
            cluster_cubemaps: read_block(s, "cluster cubemaps", super::ClusterCubemap::from_struct),
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
    pub flags: Flags<InstancedGeometryFlags, u16>,
    /// `lightmap texcoord block index` — into per_instance_lightmap_texcoords.
    pub lightmap_texcoord_block_index: i16,
    pub world_bounding_sphere_center: RealPoint3d,
    pub world_bounding_sphere_radius: f32,
    /// `name` — string_id, for debugging / identification.
    pub name: String,
    /// `pathfinding policy` enum.
    pub pathfinding_policy: Enum<InstancedGeometryPathfindingPolicy, i16>,
    /// `lightmapping policy` enum.
    pub lightmapping_policy: Enum<InstancedGeometryLightmappingPolicy, i16>,
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
            flags: s.try_read_flags("flags").unwrap_or_default(),
            lightmap_texcoord_block_index: s
                .read_int_any("lightmap texcoord block index")
                .unwrap_or(-1) as i16,
            world_bounding_sphere_center: s.read_point3d("world bounding sphere center"),
            world_bounding_sphere_radius: s
                .read_real("world bounding sphere radius")
                .unwrap_or(0.0),
            name: s.read_string_id("name").unwrap_or_default(),
            pathfinding_policy: s.try_read_enum("pathfinding policy").unwrap_or_default(),
            lightmapping_policy: s.try_read_enum("lightmapping policy").unwrap_or_default(),
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
    /// `flags*` (`structure_bsp_cluster_portal_flags_definition`) —
    /// one-way / door / no-way / AI sound occlusion.
    pub flags: Flags<StructureBspClusterPortalFlags, u32>,
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
            flags: s.try_read_flags("flags").unwrap_or_default(),
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
    /// at offset 0x74).
    pub render_bsp_count: usize,

    /// `render bsp[0]` decoded as a full collision `Bsp3d` — the DETAILED
    /// per-definition BSP whose `surfaces` are PARALLEL to this definition's
    /// [`Self::structure_surfaces`] (same count/order). This is distinct from
    /// [`Self::bsp`] (`collision info`), which is the simplified physics
    /// collision (often a 6-surface bounding box).
    ///
    /// ⭐The lighting/geometry sampler MUST raycast against THIS bsp, not
    /// `collision info`: engine `instanced_geometry_test_vector_internal @
    /// 0x180400170` switches to `render_bsp` whenever the collision test flag
    /// bit `0x10` (RENDER_ONLY_BSPS) is set — which `c_geometry_sampler::
    /// geometry_test_vector` always sets (flags 0x4811/0x4819). Its bsp2d
    /// leaves return surface indices into the 95-element structure-surface
    /// space; the `collision info` box returns 0..5, which mis-index
    /// `structure_surfaces` → wrong surface → black per-object lighting.
    pub render_bsp: Option<Bsp3d>,

    /// `poopie cutter collision` — the SECOND inline `collision_bsp` in the
    /// tag-file instanced-geometry-definition struct (the cache strips it).
    /// Bungie's "cutter" collision, distinct from `collision info` (`bsp`).
    pub poopie_cutter_bsp: Option<Bsp3d>,

    /// Per-instance-definition surface descriptors (schema field
    /// `surfaces*`, `structure_surface_small_block`, 4 B each). Same shape
    /// as the top-level [`StructureBsp::structure_surfaces`] but scoped
    /// to this definition's geometry. Engine
    /// `c_geometry_sampler::geometry_test_collision_result` instance branch
    /// reads these to map a collision surface index → triangle range.
    ///
    /// The MCC schema has BOTH `surfaces*` (small, u16 indices) AND
    /// `large surfaces*` (u32 indices). Tools author ONE or the other per
    /// definition — some s3d_turf defs populate only `large surfaces`
    /// (e.g. def 15: 484 large surfaces, 0 small). Prefer whichever is
    /// non-empty via [`Self::surface_mapping_range`] / [`Self::surface_count`].
    pub structure_surfaces: Vec<StructureSurface>,

    /// `long`-indexed (large) variant of [`Self::structure_surfaces`], schema
    /// field `large surfaces*`. Same `(first_mapping_index, mapping_count)`
    /// shape with `i32` widths. Populated when the small block is empty.
    pub structure_large_surfaces: Vec<super::StructureSurfaceLarge>,

    /// Per-instance-definition surface → triangle mapping table (schema
    /// `surface to triangle mapping*`, 4 B each). Mirrors the top-level
    /// [`StructureBsp::structure_surface_to_triangle_mappings`] shape.
    /// `triangle_index` indexes into this definition's render mesh's
    /// index buffer; `section_index` is unused here (instance defs map
    /// to a single mesh, not multiple clusters). Indexed by BOTH the small
    /// and large surface tables.
    pub structure_surface_to_triangle_mappings: Vec<StructureSurfaceTriangleMapping>,
}

impl BspInstanceDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let bsp = s
            .field("collision info")
            .and_then(|f| f.as_struct())
            .and_then(|cs| Bsp3d::from_inline_struct(&cs));
        let render_bsp_block = s.field("render bsp").and_then(|f| f.as_block());
        let render_bsp_count = render_bsp_block.as_ref().map(|b| b.len()).unwrap_or(0);
        // Decode render_bsp[0] as a full collision Bsp3d (the detailed,
        // structure-surface-parallel BSP used by the geometry/lighting sampler).
        let render_bsp = render_bsp_block
            .as_ref()
            .and_then(|b| b.element(0))
            .and_then(|e| Bsp3d::from_inline_struct(&e));
        let poopie_cutter_bsp = s
            .field("poopie cutter collision")
            .and_then(|f| f.as_struct())
            .and_then(|cs| Bsp3d::from_inline_struct(&cs));
        let structure_surfaces = read_block_named(
            s,
            "surfaces",
            StructureSurface::from_struct,
        );
        let structure_large_surfaces = read_block_named(
            s,
            "large surfaces",
            super::StructureSurfaceLarge::from_struct,
        );
        let structure_surface_to_triangle_mappings = read_block_named(
            s,
            "surface to triangle mapping",
            StructureSurfaceTriangleMapping::from_struct,
        );
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
            render_bsp,
            poopie_cutter_bsp,
            structure_surfaces,
            structure_large_surfaces,
            structure_surface_to_triangle_mappings,
        }
    }

    /// Number of effective structure surfaces — the small `surfaces` block
    /// when populated, else the `large surfaces` (u32-index) block.
    pub fn surface_count(&self) -> usize {
        if !self.structure_surfaces.is_empty() {
            self.structure_surfaces.len()
        } else {
            self.structure_large_surfaces.len()
        }
    }

    /// `(first_mapping_index, mapping_count)` for effective surface `index`,
    /// preferring the small `surfaces` block and falling back to `large
    /// surfaces` when it's empty. Both index the same
    /// [`Self::structure_surface_to_triangle_mappings`]. Mirrors the engine's
    /// runtime small-vs-large selection (`c_collision_bsp_reference`).
    pub fn surface_mapping_range(&self, index: usize) -> Option<(usize, usize)> {
        if !self.structure_surfaces.is_empty() {
            let s = self.structure_surfaces.get(index)?;
            Some((s.first_mapping_index as usize, s.mapping_count as usize))
        } else {
            let s = self.structure_large_surfaces.get(index)?;
            if s.first_mapping_index < 0 || s.mapping_count < 0 {
                return None;
            }
            Some((s.first_mapping_index as usize, s.mapping_count as usize))
        }
    }
}

fn read_block_named<T, F: Fn(&TagStruct<'_>) -> T>(
    s: &TagStruct<'_>,
    name: &str,
    f: F,
) -> Vec<T> {
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(f(&e));
                }
            }
            out
        })
        .unwrap_or_default()
}

/// One marker placed in the BSP — name + position + node ref.
#[derive(Debug, Clone, Default)]
pub struct BspMarker {
    pub name: String,
    pub node_index: i16,
    /// `rotation` (quaternion) — the marker's orientation. Marker LIGHTS use it
    /// for the spawned light's forward (`q·X`) / up (`q·Y`) basis
    /// (`lights_add_structure_bsp_marker_lights`).
    pub rotation: RealQuaternion,
    pub position: RealPoint3d,
}

impl BspMarker {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            node_index: s.read_block_index("node index"),
            rotation: s.read_quat("rotation"),
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

#[cfg(test)]
mod maximal_walker_tests {
    use super::StructureBsp;
    use crate::TagFile;

    /// Parse a real H3 BSP end-to-end and assert the maximal-coverage
    /// blocks populate as expected. Validates that the schema-derived
    /// field names resolve against an actual tag.
    #[test]
    fn parses_s3d_lockout_bsp() {
        let path = "/Users/camden/Halo/halo3_mcc/tags/levels/multi/\
                    s3d_lockout/s3d_lockout.scenario_structure_bsp";
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present");
            return;
        }
        let tag = TagFile::read(path).expect("read sbsp");
        let bsp = StructureBsp::from_struct(&tag.root());

        // populated blocks (counts from a live parse)
        assert_eq!(bsp.clusters.len(), 18, "lockout has 18 clusters");
        assert_eq!(bsp.weather_polyhedra.len(), 10, "lockout weather polyhedra");
        assert_eq!(bsp.large_structure_surfaces.len(), 11271);
        assert_eq!(bsp.errors.len(), 8);
        // loose tags leave per-cluster atmosphere unresolved (= -1)
        assert_eq!(bsp.clusters[0].atmosphere_index, -1);

        // weather polyhedra carry real geometry (sphere + bounding planes)
        let poly = &bsp.weather_polyhedra[0];
        assert!(poly.bounding_sphere_radius > 0.0);
        assert!(!poly.planes.is_empty());

        // full render geometry resolves (vertex/index buffers + meshes)
        let geom = bsp.render_geometry.as_ref().expect("render geometry present");
        assert!(!geom.meshes.is_empty(), "render geometry has meshes");
        // the lightweight metadata mirror has one entry per render mesh
        assert_eq!(bsp.meshes_metadata.len(), geom.meshes.len());
    }

    /// A content-rich campaign BSP exercises the environment-object reader.
    #[test]
    fn parses_campaign_environment_objects() {
        let path = "/Users/camden/Halo/halo3_mcc/tags/levels/solo/\
                    120_halo/120_bsp_110.scenario_structure_bsp";
        if !std::path::Path::new(path).exists() {
            eprintln!("skipping: {path} not present");
            return;
        }
        let tag = TagFile::read(path).expect("read sbsp");
        let bsp = StructureBsp::from_struct(&tag.root());
        assert_eq!(bsp.clusters.len(), 29);
        assert_eq!(bsp.environment_objects.len(), 466);
        // each env object resolves a non-empty name + a palette index
        assert!(bsp.environment_objects.iter().all(|e| e.palette_index >= 0));
    }
}
