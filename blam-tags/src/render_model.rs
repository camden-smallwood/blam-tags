//! Schema-faithful walker for the `render_model` (mode) tag.
//!
//! This module mirrors the `definitions/halo3_mcc/render_model.json`
//! schema **1:1**: every block/struct/field in the schema maps to a
//! field on the public type tree below, with the same nesting. Type
//! names are PascalCase with the `_block`/`_struct`/`_definition`
//! suffixes stripped (`render_model_region_block` -> [`Region`]); field
//! names are snake_case.
//!
//! On top of the faithful tree, a **render-oriented derived layer**
//! ([`RenderMesh`] + [`RenderVertex`] + [`RawWaterData`]) is provided
//! via [`RenderModel::derive_render_meshes`] and the `extract_*` free
//! functions. The derived layer does what a renderer actually needs and
//! the raw schema tree does not: decompress vertex positions/texcoords
//! through the per-mesh compression bounds, decode triangle strips to
//! triangle lists (per-subpart when a part declares subparts), resolve
//! the water append-pool sequential indexing, extract the PRT-ambient
//! per-vertex transfer stream from `per_mesh_prt_data`, and surface the
//! `has_vertex_color` / `has_prt_vertex_stream` / `has_lightmap_uvs`
//! engine signals. The derived layer is also reusable on
//! `scenario_structure_bsp` (sbsp) and the per-BSP lightmap tag, since
//! all three share the `s_render_geometry` schema.
//!
//! Targets H3 / Reach MCC tags where every render mesh stores its
//! buffers inline under `render geometry/per mesh temporary[i]`.

use crate::api::{TagBlock, TagStruct};
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::game::Game;
use crate::geometry::{read_compression_bounds, strip_to_list, CompressionBounds};
use crate::math::{
    RealOrientation, RealPlane3d, RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d,
};
use crate::typed_enums::{Enum, Flags};

//================================================================================
// Typed enums / flags (variants superset the schema option lists).
//================================================================================

/// `render_model_flags_definition` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum RenderModelFlags {
    #[strum(serialize = "UNUSED")] Unused = 0,
    #[strum(serialize = "UNUSED2")] Unused2 = 1,
    #[strum(serialize = "has node maps")] HasNodeMaps = 2,
    // Reach/H4 union (on-disk bit comes from each tag's schema; discriminants
    // here are just canonical indices appended past the H3 set).
    #[strum(serialize = "sun as vmf light")] SunAsVmfLight = 3,
    #[strum(serialize = "this is supposed to be a sky")] ThisIsSupposedToBeASky = 4,
    #[strum(serialize = "has fur shader")] HasFurShader = 5,
    #[strum(serialize = "is hologram")] IsHologram = 6,
}

/// `render_geometry_flags` (long_flags). Runtime-only (`*!`) bits whose
/// layout diverged across engines: H3 stops at bit 2 ("version 2"), while
/// Reach/H4 use on-disk bit 2 for "has valid budgets (really)" and add bits
/// 3-5. Both names are distinct variants; on-disk bit positions come from
/// each tag's own schema (the variant discriminant is only a canonical index
/// for the typed view), so `Version2` and `HasValidBudgets` coexisting at
/// different discriminants despite sharing on-disk bit 2 is intentional.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum RenderGeometryFlags {
    #[strum(serialize = "processed")] Processed = 0,
    #[strum(serialize = "available")] Available = 1,
    #[strum(serialize = "version 2")] Version2 = 2,
    #[strum(serialize = "manual resource creation")] ManualResourceCreation = 3,
    #[strum(serialize = "keep raw geometry")] KeepRawGeometry = 4,
    #[strum(serialize = "dont use compressed vertex positions")] DontUseCompressedVertexPositions = 5,
    #[strum(serialize = "has valid budgets (really)")] HasValidBudgets = 6,
}

/// `part_flags` (byte_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum PartFlags {
    #[strum(serialize = "dislikes photons")] DislikesPhotons = 0,
    #[strum(serialize = "ignored by lightmapper")] IgnoredByLightmapper = 1,
    #[strum(serialize = "has transparent sorting plane")] HasTransparentSortingPlane = 2,
    #[strum(serialize = "is water surface")] IsWaterSurface = 3,
    #[strum(serialize = "is hologram")] IsHologram = 4,
    // Reach/H4 union (on-disk bit comes from each tag's schema; discriminants
    // here are just canonical indices appended past the H3 set). Reach and H4
    // drift on a few names ("transparent"/"is transparent", "cannot render
    // single pass render"/"cannot single pass render", etc.) — both kept.
    #[strum(serialize = "per vertex lightmap part")] PerVertexLightmapPart = 5,
    #[strum(serialize = "debug flag instance part")] DebugFlagInstancePart = 6,
    #[strum(serialize = "subparts has uberlights info")] SubpartsHasUberlightsInfo = 7,
    #[strum(serialize = "draw cull distance medium")] DrawCullDistanceMedium = 8,
    #[strum(serialize = "draw cull distance close")] DrawCullDistanceClose = 9,
    #[strum(serialize = "draw cull rendering shields")] DrawCullRenderingShields = 10,
    #[strum(serialize = "cannot render single pass render")] CannotRenderSinglePassRender = 11,
    #[strum(serialize = "transparent")] Transparent = 12,
    #[strum(serialize = "cannot render two pass")] CannotRenderTwoPass = 13,
    #[strum(serialize = "cannot single pass render")] CannotSinglePassRender = 14,
    #[strum(serialize = "is transparent")] IsTransparent = 15,
    #[strum(serialize = "cannot two pass")] CannotTwoPass = 16,
    #[strum(serialize = "transparent should output depth for DoF")] TransparentShouldOutputDepthForDof = 17,
    #[strum(serialize = "do not include in static lightmap")] DoNotIncludeInStaticLightmap = 18,
    #[strum(serialize = "do not include in PVS generation")] DoNotIncludeInPvsGeneration = 19,
    #[strum(serialize = "draw cull rendering active camo")] DrawCullRenderingActiveCamo = 20,
}

/// `e_geometry_part_type` (Ares `geometry_definitions_new.h:25`). Stored
/// per render-geometry part as a `char_integer` (not a schema enum), so
/// it's decoded value→variant here rather than via the by-name
/// `Enum<T,U>` machinery. Drives per-part render-pass routing in the
/// engine's `c_structure_renderer::submit_visibility`:
///   - `part_is_renderable @ 0x18069eb90` skips `LightmapOnly` parts
///     entirely (invisible `z_invis_lightquad` surfaces baked only to
///     feed the offline lightmapper).
///   - the remainder route by `is_transparent ? Transparent : (type & 3)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum GeometryPartType {
    #[strum(serialize = "opaque not drawn")] OpaqueNotDrawn = 0,
    #[strum(serialize = "opaque shadow only")] OpaqueShadowOnly = 1,
    #[strum(serialize = "opaque shadow casting")] OpaqueShadowCasting = 2,
    #[strum(serialize = "opaque non shadowing")] OpaqueNonShadowing = 3,
    #[strum(serialize = "transparent")] Transparent = 4,
    #[strum(serialize = "lightmap only")] LightmapOnly = 5,
}

impl GeometryPartType {
    /// Decode the raw `char_integer`. Out-of-spec values (Halo only ever
    /// emits 0..=5) fall back to `OpaqueNotDrawn` — the safe "has geometry
    /// but don't draw in the opaque passes" default.
    pub fn from_raw(v: i8) -> Self {
        <Self as num_traits::FromPrimitive>::from_i8(v).unwrap_or(Self::OpaqueNotDrawn)
    }
}

/// `mesh_flags` (byte_flags). Shared global render-geometry mesh flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum MeshFlags {
    #[strum(serialize = "mesh has vertex color")] MeshHasVertexColor = 0,
    #[strum(serialize = "use region index for sorting")] UseRegionIndexForSorting = 1,
    #[strum(serialize = "use vertex buffers for indices")] UseVertexBuffersForIndices = 2,
    #[strum(serialize = "mesh has per-instance lighting (do not modify)")] MeshHasPerInstanceLighting = 3,
    #[strum(serialize = "mesh is unindexed (do not modify)")] MeshIsUnindexed = 4,
    // Reach/H4 union (on-disk bit comes from each tag's schema; discriminants
    // here are just canonical indices appended past the H3 set).
    #[strum(serialize = "subpart were merged")] SubpartWereMerged = 5,
    #[strum(serialize = "mesh has fur")] MeshHasFur = 6,
    #[strum(serialize = "mesh has decal")] MeshHasDecal = 7,
    #[strum(serialize = "mesh doesnt use compressed position")] MeshDoesntUseCompressedPosition = 8,
    #[strum(serialize = "use uncompressed vertex format")] UseUncompressedVertexFormat = 9,
    #[strum(serialize = "mesh is PCA")] MeshIsPca = 10,
    #[strum(serialize = "mesh compression determined")] MeshCompressionDetermined = 11,
    #[strum(serialize = "mesh has authored lightmap texture coords")] MeshHasAuthoredLightmapTexcoords = 12,
    #[strum(serialize = "mesh has a useful set of second texture coords")] MeshHasUsefulSecondTexcoords = 13,
    #[strum(serialize = "mesh has no lightmap")] MeshHasNoLightmap = 14,
    #[strum(serialize = "per vertex lighting")] PerVertexLighting = 15,
}

/// `compression_flags` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum CompressionFlags {
    #[strum(serialize = "compressed position")] CompressedPosition = 0,
    #[strum(serialize = "compressed texcoord")] CompressedTexcoord = 1,
    // Reach/H4 union.
    #[strum(serialize = "compression optimized")] CompressionOptimized = 2,
}

/// `per_mesh_raw_data_flags` (long_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum PerMeshRawDataFlags {
    #[strum(serialize = "indices are triangle strips")] IndicesAreTriangleStrips = 0,
    #[strum(serialize = "indices are triangle lists")] IndicesAreTriangleLists = 1,
    #[strum(serialize = "indices are quad lists")] IndicesAreQuadLists = 2,
}

/// `mesh_vertex_type_definition` (char_enum) — full 22-option list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum MeshVertexType {
    #[default]
    #[strum(serialize = "world")] World = 0,
    #[strum(serialize = "rigid")] Rigid = 1,
    #[strum(serialize = "skinned")] Skinned = 2,
    #[strum(serialize = "particle_model")] ParticleModel = 3,
    #[strum(serialize = "flat world")] FlatWorld = 4,
    #[strum(serialize = "flat rigid")] FlatRigid = 5,
    #[strum(serialize = "flat skinned")] FlatSkinned = 6,
    #[strum(serialize = "screen")] Screen = 7,
    #[strum(serialize = "debug")] Debug = 8,
    #[strum(serialize = "transparent")] Transparent = 9,
    #[strum(serialize = "particle")] Particle = 10,
    #[strum(serialize = "contrail")] Contrail = 11,
    #[strum(serialize = "light_volume")] LightVolume = 12,
    #[strum(serialize = "chud_simple")] ChudSimple = 13,
    #[strum(serialize = "chud_fancy")] ChudFancy = 14,
    #[strum(serialize = "decorator")] Decorator = 15,
    #[strum(serialize = "position only")] PositionOnly = 16,
    #[strum(serialize = "patchy_fog")] PatchyFog = 17,
    #[strum(serialize = "water")] Water = 18,
    #[strum(serialize = "ripple")] Ripple = 19,
    #[strum(serialize = "implicit geometry")] ImplicitGeometry = 20,
    #[strum(serialize = "beam")] Beam = 21,
    // Reach/H4 union. Discriminants 22-53 match the on-disk option index; the
    // few Reach/H4 name drifts (29, 30) are aliased, and H4's `unused0`/`unused1`
    // (on-disk 11/21, where H3 has contrail/beam) get appended indices. The
    // preview never reads this value (vertices decode from the buffer format),
    // so the only requirement is that resolve-by-name finds a variant.
    #[strum(serialize = "world_tessellated")] WorldTessellated = 22,
    #[strum(serialize = "rigid_tessellated")] RigidTessellated = 23,
    #[strum(serialize = "skinned_tessellated")] SkinnedTessellated = 24,
    #[strum(serialize = "shader_cache")] ShaderCache = 25,
    #[strum(serialize = "structure_instance_imposter")] StructureInstanceImposter = 26,
    #[strum(serialize = "object_instance_imposter")] ObjectInstanceImposter = 27,
    #[strum(serialize = "rigid compressed")] RigidCompressed = 28,
    #[strum(serialize = "skinned uncompressed", serialize = "skinned compressed")] SkinnedUncompressed = 29,
    #[strum(serialize = "light volume precompiled", serialize = "light_volume_precompiled")] LightVolumePrecompiled = 30,
    #[strum(serialize = "blendshape_rigid")] BlendshapeRigid = 31,
    #[strum(serialize = "blendshape_rigid_blendshaped")] BlendshapeRigidBlendshaped = 32,
    #[strum(serialize = "rigid_blendshaped")] RigidBlendshaped = 33,
    #[strum(serialize = "blendshape_skinned")] BlendshapeSkinned = 34,
    #[strum(serialize = "blendshape_skinned_blendshaped")] BlendshapeSkinnedBlendshaped = 35,
    #[strum(serialize = "skinned_blendshaped")] SkinnedBlendshaped = 36,
    #[strum(serialize = "VirtualGeometryHWtess")] VirtualGeometryHwTess = 37,
    #[strum(serialize = "VirtualGeometryMemexport")] VirtualGeometryMemexport = 38,
    #[strum(serialize = "position_only")] PositionOnlyUnderscore = 39,
    #[strum(serialize = "VirtualGeometryDebug")] VirtualGeometryDebug = 40,
    #[strum(serialize = "blendshapeRigidCompressed")] BlendshapeRigidCompressed = 41,
    #[strum(serialize = "skinnedUncompressedBlendshaped")] SkinnedUncompressedBlendshaped = 42,
    #[strum(serialize = "blendshapeSkinnedCompressed")] BlendshapeSkinnedCompressed = 43,
    #[strum(serialize = "tracer")] Tracer = 44,
    #[strum(serialize = "polyart")] Polyart = 45,
    #[strum(serialize = "vectorart")] Vectorart = 46,
    #[strum(serialize = "rigid_boned")] RigidBoned = 47,
    #[strum(serialize = "rigid_boned_2uv")] RigidBoned2Uv = 48,
    #[strum(serialize = "blendshape_skinned_2uv")] BlendshapeSkinned2Uv = 49,
    #[strum(serialize = "blendshape_skinned_2uv_blendshaped")] BlendshapeSkinned2UvBlendshaped = 50,
    #[strum(serialize = "skinned_2uv_blendshaped")] Skinned2UvBlendshaped = 51,
    #[strum(serialize = "polyartUV")] PolyartUv = 52,
    #[strum(serialize = "blendshape_skinned_uncompressed_blendshaped")] BlendshapeSkinnedUncompressedBlendshaped = 53,
    #[strum(serialize = "unused0")] Unused0 = 54,
    #[strum(serialize = "unused1")] Unused1 = 55,
}

/// `mesh_transfer_vertex_type_definition` (char_enum). Selects which
/// PRT entry point the engine remaps to at `render_mesh_part_default @
/// 0x18069EBC0` via `entry_point_remapping_0[transfer_vector_vertex_type]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum PrtVertexType {
    #[default]
    #[strum(serialize = "No PRT")] None = 0,
    #[strum(serialize = "PRT Ambient")] Ambient = 1,
    #[strum(serialize = "PRT Linear")] Linear = 2,
    #[strum(serialize = "PRT Quadratic")] Quadratic = 3,
}

impl PrtVertexType {
    pub fn is_some(self) -> bool { !matches!(self, Self::None) }
}

/// `mesh_index_buffer_type_definition` (char_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum MeshIndexBufferType {
    #[default]
    #[strum(serialize = "DEFAULT")] Default = 0,
    #[strum(serialize = "line list")] LineList = 1,
    #[strum(serialize = "line strip")] LineStrip = 2,
    #[strum(serialize = "triangle list")] TriangleList = 3,
    #[strum(serialize = "triangle fan")] TriangleFan = 4,
    #[strum(serialize = "triangle strip")] TriangleStrip = 5,
    #[strum(serialize = "quad list")] QuadList = 6,
}

/// `geometry_material_property_type` (short_enum) — 9 options.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GeometryMaterialPropertyType {
    #[default]
    #[strum(serialize = "lightmap resolution")] LightmapResolution = 0,
    #[strum(serialize = "lightmap power")] LightmapPower = 1,
    #[strum(serialize = "lightmap half life")] LightmapHalfLife = 2,
    #[strum(serialize = "lightmap diffuse scale")] LightmapDiffuseScale = 3,
    #[strum(serialize = "lightmap photon fidelity")] LightmapPhotonFidelity = 4,
    #[strum(serialize = "lightmap translucency tint color")] LightmapTranslucencyTintColor = 5,
    #[strum(serialize = "lightmap transparency override")] LightmapTransparencyOverride = 6,
    #[strum(serialize = "lightmap additive transparency")] LightmapAdditiveTransparency = 7,
    #[strum(serialize = "lightmap ignore default res scale")] LightmapIgnoreDefaultResScale = 8,
}

//================================================================================
// Error
//================================================================================

/// Errors from render_model extraction.
#[derive(Debug)]
pub enum RenderModelError {
    /// A required field was missing from the tag — schema mismatch or
    /// the field was empty in the instance. Carries the dotted field path.
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

//================================================================================
// Schema-faithful public type tree (mirrors render_model.json 1:1).
//================================================================================

/// Root `render_model_block_struct`.
///
/// NOTE: the schema's `errors` block (`global_error_report_categories_block`)
/// is intentionally NOT mirrored here — it is authoring-only error/warning
/// reporting data with no runtime/render meaning.
#[derive(Debug, Clone, Default)]
pub struct RenderModel {
    pub name: String,
    pub flags: Flags<RenderModelFlags, u16>,
    pub regions: Vec<Region>,
    pub instance_placements: Vec<InstancePlacement>,
    pub nodes: Vec<Node>,
    pub marker_groups: Vec<MarkerGroup>,
    pub materials: Vec<Material>,
    pub render_geometry: Geometry,
    pub sky_lights: Vec<SkyLight>,
    pub default_lightprobe: Option<DefaultLightprobe>,
    pub volume_samples: Vec<VolumeSample>,
    /// `runtime node orientations!` block — tool.exe-baked bind-pose
    /// snapshot (one [`NodeOrientation`] per node). Empty for extracted
    /// tags that strip runtime blocks.
    pub default_node_orientations: Vec<NodeOrientation>,
}

/// `render_model_region_block`.
#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    pub permutations: Vec<Permutation>,
}

/// `render_model_permutation_block`.
#[derive(Debug, Clone)]
pub struct Permutation {
    pub name: String,
    pub mesh_index: i16,
    pub mesh_count: i16,
    /// `instance mask 0-31 / 32-63 / 64-95 / 96-127` long_flags packed
    /// as a 128-bit mask (each entry is one u32 word).
    pub instance_mask: [u32; 4],
}

/// `global_render_model_instance_placement_block`.
#[derive(Debug, Clone)]
pub struct InstancePlacement {
    pub name: String,
    pub node_index: i16,
    pub scale: f32,
    pub forward: RealVector3d,
    pub left: RealVector3d,
    pub up: RealVector3d,
    pub position: RealPoint3d,
}

/// `render_model_node_block`. Field order follows the schema's
/// "Old Mistakes Die Hard" explanation: the on-disk inverse layout is
/// `inverse forward / left / up / position` then `inverse scale`.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub parent_node: i16,
    pub first_child_node: i16,
    pub next_sibling_node: i16,
    pub default_translation: RealPoint3d,
    pub default_rotation: RealQuaternion,
    pub inverse_forward: RealVector3d,
    pub inverse_left: RealVector3d,
    pub inverse_up: RealVector3d,
    pub inverse_position: RealPoint3d,
    pub inverse_scale: f32,
    pub distance_from_parent: f32,
}

/// `render_model_marker_group_block`.
#[derive(Debug, Clone)]
pub struct MarkerGroup {
    pub name: String,
    pub markers: Vec<Marker>,
}

/// `render_model_marker_block`.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    pub region_index: i8,
    pub permutation_index: i8,
    pub node_index: i8,
    pub translation: RealPoint3d,
    pub rotation: RealQuaternion,
    pub scale: f32,
}

/// `global_geometry_material_block`.
#[derive(Debug, Clone)]
pub struct Material {
    /// `render method` tag_reference — Halo-style relative path (no
    /// extension). Empty when the tag_ref was null.
    pub render_method: String,
    /// Group FOURCC of the `render method` tag_reference (`rmsh`, `rmtr`,
    /// `rmw `, …). Zero when the tag_ref was null.
    pub render_method_group: u32,
    pub properties: Vec<MaterialProperty>,
    pub imported_material_index: i32,
    pub breakable_surface_index: i8,
}

impl Material {
    /// File extension matching [`Self::render_method_group`] — e.g.
    /// `"shader_terrain"` for `rmtr`. Pair with [`Self::render_method`]
    /// and `paths::resolve_tag_path` to locate the on-disk tag file.
    pub fn shader_extension(&self) -> &'static str {
        crate::paths::group_tag_to_extension(self.render_method_group).unwrap_or("shader")
    }

    /// Shader basename (filename without extension/directory). Stable
    /// for dedupe / default-material keying.
    pub fn shader_name(&self) -> String {
        std::path::Path::new(&self.render_method.replace('\\', "/"))
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("default")
            .to_owned()
    }
}

/// `global_geometry_material_property_block`.
#[derive(Debug, Clone, Copy)]
pub struct MaterialProperty {
    pub property_type: Enum<GeometryMaterialPropertyType, i16>,
    pub int_value: i16,
    pub long_value: i32,
    pub real_value: f32,
}

/// `global_render_geometry_struct`.
///
/// Schema blocks NOT mirrored here (decode skipped; see field docs):
/// - `user data` (`user_data_block`) — authoring PRT-info user-data blob.
/// - `per mesh mopp` (`per_mesh_mopp_block`) — collision MOPP codes.
/// - `per mesh subpart visibility` (`per_mesh_subpart_visibility_block`) —
///   per-subpart bounding spheres for runtime culling.
/// - `api resource` (`render_geometry_api_resource_definition`) — the
///   cache-only packed vertex/index buffer resource (loose tags carry
///   the raw data under `per mesh temporary` instead).
#[derive(Debug, Clone, Default)]
pub struct Geometry {
    pub flags: Flags<RenderGeometryFlags, u32>,
    pub meshes: Vec<Mesh>,
    pub compression_info: Vec<CompressionInfo>,
    pub part_sorting_position: Vec<SortingPosition>,
    pub per_mesh_temporary: Vec<PerMeshTemporary>,
    /// `per mesh node map` — one `Vec<u8>` (node-index list) per mesh.
    pub per_mesh_node_map: Vec<Vec<u8>>,
    pub per_mesh_prt_data: Vec<PerMeshPrtData>,
    pub per_instance_lightmap_texcoords: Vec<PerInstanceLightmapTexcoords>,
}

/// `global_mesh_block`.
///
/// `instance buckets` (`global_instance_bucket_block`) is decoded into
/// [`Self::instance_buckets`].
#[derive(Debug, Clone)]
pub struct Mesh {
    pub parts: Vec<Part>,
    pub subparts: Vec<Subpart>,
    pub vertex_buffer_indices: [u16; 8],
    pub index_buffer_index: i16,
    pub index_buffer_tessellation: i16,
    pub mesh_flags: Flags<MeshFlags, u8>,
    pub rigid_node_index: i8,
    pub vertex_type: Enum<MeshVertexType, i8>,
    pub prt_vertex_type: Enum<PrtVertexType, i8>,
    pub index_buffer_type: Enum<MeshIndexBufferType, i8>,
    pub instance_buckets: Vec<InstanceBucket>,
    pub water_indices_start: Vec<u16>,
}

/// `global_instance_bucket_block`.
#[derive(Debug, Clone)]
pub struct InstanceBucket {
    pub mesh_index: i16,
    pub definition_index: i16,
    pub instances: Vec<u16>,
}

/// `part_block`.
#[derive(Debug, Clone)]
pub struct Part {
    pub render_method_index: i16,
    pub transparent_sorting_index: i16,
    pub index_start: i16,
    pub index_count: i16,
    pub subpart_start: i16,
    pub subpart_count: i16,
    pub part_type: GeometryPartType,
    pub part_flags: Flags<PartFlags, u8>,
    pub budget_vertex_count: i16,
}

/// `subpart_block`.
#[derive(Debug, Clone, Copy)]
pub struct Subpart {
    pub index_start: i16,
    pub index_count: i16,
    pub part_index: i16,
    pub budget_vertex_count: i16,
}

/// `compression_info_block`. `position_bounds` / `texcoord_bounds` are
/// stored mislabeled in the schema as `real_point_*` pairs — the actual
/// packing is `[xmin,xmax,ymin] [ymax,zmin,zmax]` for position and
/// `[xmin,xmax] [ymin,ymax]` for texcoord (see schema WARNING). We read
/// them verbatim into the two-element arrays as authored.
#[derive(Debug, Clone)]
pub struct CompressionInfo {
    pub flags: Flags<CompressionFlags, u16>,
    pub position_bounds: [RealPoint3d; 2],
    pub texcoord_bounds: [RealPoint2d; 2],
}

/// `sorting_position_block`. `node_weights` is the schema's implicit
/// 3-element array (4th weight = 1 - sum).
#[derive(Debug, Clone, Copy)]
pub struct SortingPosition {
    pub plane: RealPlane3d,
    pub position: RealPoint3d,
    pub radius: f32,
    pub node_indices: [u8; 4],
    pub node_weights: [f32; 3],
}

/// `per_mesh_raw_data_block`.
#[derive(Debug, Clone, Default)]
pub struct PerMeshTemporary {
    pub raw_vertices: Vec<RawVertex>,
    pub raw_indices: Vec<u16>,
    pub raw_water_data: Option<RawWaterDataSchema>,
    pub parameterized_texture_width: i16,
    pub parameterized_texture_height: i16,
    pub flags: Flags<PerMeshRawDataFlags, u32>,
}

/// `raw_vertex_block`. Values are read VERBATIM (no compression-bounds
/// decompress) — the decompressed/render-ready form lives in the derived
/// [`RenderVertex`].
#[derive(Debug, Clone, Copy)]
pub struct RawVertex {
    pub position: RealPoint3d,
    pub texcoord: RealPoint2d,
    pub normal: RealVector3d,
    pub binormal: RealVector3d,
    pub tangent: RealVector3d,
    pub lightmap_texcoord: RealPoint2d,
    pub node_indices: [u8; 4],
    pub node_weights: [f32; 4],
    pub vertex_color: RealVector3d,
}

/// `raw_water_block` — schema-faithful form (parallel index + append
/// pools, unresolved). The renderer-facing resolved form is
/// [`RawWaterData`].
#[derive(Debug, Clone, Default)]
pub struct RawWaterDataSchema {
    pub indices: Vec<u16>,
    pub vertices: Vec<RawWaterAppend>,
}

/// `raw_water_append_block` (36 bytes) — three `real_point_3d` fields
/// read by the water VS.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterAppend {
    /// `local_info` — feeds foam height + paint sampling. `.x` is the
    /// scenario-wide wave amplitude scale; `.y` is per-vertex water depth.
    pub local_info: RealPoint3d,
    /// `water_velocity` — flow-direction sampling for animated waves.
    pub water_velocity: RealPoint3d,
    /// `base_texcoord` — UV for watercolor / foam / global_shape textures.
    pub base_texcoord: RealPoint3d,
}

/// `per_mesh_prt_data_block`.
///
/// The nested `per instance prt data` (`per_instance_prt_data_block`)
/// sub-block is NOT decoded — it carries the same per-instance PCA blob
/// and is unused by the render path.
#[derive(Debug, Clone, Default)]
pub struct PerMeshPrtData {
    pub mesh_pca_data: Vec<u8>,
}

/// `per_instance_lightmap_texcoords_block`.
#[derive(Debug, Clone, Default)]
pub struct PerInstanceLightmapTexcoords {
    pub texture_coordinates: Vec<RawVertex>,
    pub vertex_buffer_index: i16,
}

/// `sky_lights_block` (28 bytes). Mirrors `s_sky_gen_light`.
#[derive(Debug, Clone, Copy)]
pub struct SkyLight {
    /// World-space direction TO the light.
    pub direction: RealVector3d,
    /// Linear-space radiant intensity per channel (HDR).
    pub intensity: RealVector3d,
    /// Solid angle (steradians).
    pub solid_angle: f32,
}

/// The three `default lightprobe r/g/b` arrays (16 `default_lightprobe`
/// structs each, each holding one `coefficient` real). We keep the full
/// 16-coefficient on-disk form (the trailing 7 past SH3's 9 are zero).
#[derive(Debug, Clone)]
pub struct DefaultLightprobe {
    pub r: [f32; 16],
    pub g: [f32; 16],
    pub b: [f32; 16],
}

/// `volume_samples_block`.
#[derive(Debug, Clone)]
pub struct VolumeSample {
    pub position: RealVector3d,
    /// `radiance transfer matrix` — 9x9 = 81 reals.
    pub radiance_transfer: [f32; 81],
}

/// `default_node_orientations_block` (the `runtime node orientations!`
/// block). One parent-relative TRS per node.
#[derive(Debug, Clone, Copy)]
pub struct NodeOrientation {
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub scale: f32,
}

//================================================================================
// Render-oriented derived layer (decompressed, strip-decoded, water/PRT
// resolved). Reusable on render_model / sbsp / lightmap render geometry.
//================================================================================

/// One mesh from `render geometry/meshes[i]`, decoded for a renderer:
/// vertices decompressed through the compression bounds, strips decoded
/// to triangle lists. Index in the parent vec aligns 1:1 with the tag's
/// `meshes[i]` order.
#[derive(Debug, Clone)]
pub struct RenderMesh {
    pub vertices: Vec<RenderVertex>,
    /// Triangle-list indices into [`Self::vertices`]. For triangle-list
    /// (BSP cluster/instance) meshes this is the RAW tag index buffer in its
    /// original order, so `structure_surface_to_triangle_mapping.triangle_index`
    /// (the geometry sampler's collision-hit → render-triangle lookup) indexes
    /// it directly; [`RenderMeshPart::index_start`]/`index_count` are raw spans.
    /// For strip meshes this is the de-stripped list (no surface mapping uses
    /// it), with parts reassembled into contiguous draw ranges.
    pub indices: Vec<u32>,
    pub parts: Vec<RenderMeshPart>,
    /// For rigid meshes, the single bone all vertices bind to. `None` for
    /// skinned meshes whose vertices carry per-vertex weights.
    pub rigid_node_index: Option<i16>,
    /// Resolved water data (`None` for non-water meshes).
    pub water_data: Option<RawWaterData>,
    /// `meshes[i].PRT vertex type` — author-declared PRT variant.
    pub prt_vertex_type: Enum<PrtVertexType, i8>,
    /// True iff `vertex_buffer_indices[3] != 0xFFFF` (runtime PRT vertex
    /// buffer present).
    pub has_prt_vertex_stream: bool,
    /// Pre-baked PRT Ambient per-vertex transfer scalar (grayscale). One
    /// `f32` per vertex; from `per_mesh_prt_data[i].mesh pca data`
    /// (3 floats RGB per vertex, averaged). Empty when no PRT data.
    pub prt_ambient_stream: Vec<f32>,
    /// `mesh->flags & _mesh_has_vertex_color_bit`.
    pub has_vertex_color: bool,
    /// `mesh->flags & _mesh_use_region_index_for_sorting_bit` (MeshFlags bit 1).
    /// When set, the engine sorts this transparent mesh by REGION INDEX
    /// (constant `global_origin3d` centroid + `region*0.1` offset) instead of
    /// its authored sort centroid — keeps co-located transparent layers (e.g.
    /// a waterfall's liquid/sheet/mist) composited in a fixed, view-independent
    /// order. Engine `submit_object_mesh_parts @ 0x1806E1BF0` checks `& 2`.
    pub use_region_index_for_sorting: bool,
    /// True iff at least one vertex's lightmap UV is non-zero.
    pub has_lightmap_uvs: bool,
}

/// Per-mesh water-surface data, resolved at parse time. Each triangle's
/// 3 control points carry `(regular_idx, water_idx)` pairs already
/// de-referenced through `raw_indices` / `raw_water_indices`.
#[derive(Debug, Clone, Default)]
pub struct RawWaterData {
    /// One entry per source water triangle, ordered by source part.
    pub triangles: Vec<RawWaterTriangle>,
    /// `raw water vertices` append pool — `RawWaterControlPoint::water_idx`
    /// indexes into this.
    pub vertices: Vec<RawWaterAppend>,
    /// Per-part triangle ranges within [`Self::triangles`].
    pub parts: Vec<RawWaterPart>,
}

/// One water-flagged part's triangle range within [`RawWaterData::triangles`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterPart {
    /// Index into [`RenderMesh::parts`] — gives the rmw material.
    pub mesh_part_index: u16,
    pub triangle_start: u32,
    pub triangle_count: u32,
}

/// One source water triangle — 3 control points pulling from two pools.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterTriangle {
    pub control_points: [RawWaterControlPoint; 3],
}

/// One control point in a water triangle. `regular_idx` indexes
/// [`RenderMesh::vertices`]; `water_idx` indexes [`RawWaterData::vertices`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RawWaterControlPoint {
    pub regular_idx: u16,
    pub water_idx: u16,
}

/// Decompressed render vertex. UV is **unflipped**.
/// `node_indices`/`node_weights` are zero-padded to 4.
#[derive(Debug, Clone, Copy)]
pub struct RenderVertex {
    pub position: RealPoint3d,
    pub texcoord: RealPoint2d,
    pub normal: RealVector3d,
    pub tangent: RealVector3d,
    pub binormal: RealVector3d,
    pub node_indices: [u8; 4],
    pub node_weights: [f32; 4],
    /// `raw_vertex.lightmap texcoord`.
    pub lightmap_texcoord: RealPoint2d,
    /// `raw_vertex.vertex color` — per-vertex baked color (sky meshes).
    pub vert_color: RealVector3d,
}

/// One draw range within a [`RenderMesh`]. `material_index` indexes
/// [`RenderModel::materials`].
#[derive(Debug, Clone, Copy)]
pub struct RenderMeshPart {
    pub material_index: u16,
    pub index_start: u32,
    pub index_count: u32,
    /// `e_geometry_part_type` — drives per-part render-pass routing /
    /// the `part_is_renderable` gate. See [`GeometryPartType`].
    pub part_type: GeometryPartType,
    /// `part_block.transparent sorting index` — index into the geometry's
    /// `part sorting position` block. `-1` when the part has no authored
    /// sort position (opaque parts, or transparent parts the tool didn't
    /// emit one for). Drives the engine's back-to-front transparent sort.
    pub transparent_sorting_index: i16,
    /// Resolved [`SortingPosition`] for this part (`None` when
    /// `transparent_sorting_index < 0` or out of range). The engine feeds
    /// `position` (centroid) + `plane` to `c_transparency_renderer::add_element`
    /// as the transparent sort key. Carried here so the render-ready
    /// [`RenderMesh`] is self-contained — no second walk of the raw
    /// `Geometry.part_sorting_position` block needed.
    pub sort_position: Option<SortingPosition>,
}

/// Per-subpart triangle-list geometry, decorator-specific. Each subpart
/// is strip-decoded independently. Returned by [`extract_decorator_subparts`].
#[derive(Debug, Clone, Default)]
pub struct DecoratorSubpartGeometry {
    pub vertices: Vec<RenderVertex>,
    /// Per-subpart triangle-list (each `u32` is a vertex index).
    pub subpart_indices: Vec<Vec<u32>>,
}

//================================================================================
// from_tag — schema-faithful walk
//================================================================================

impl RenderModel {
    /// Walk a parsed `render_model` (mode) tag into the schema-faithful
    /// type tree. Use [`Self::derive_render_meshes`] for renderer-ready
    /// (decompressed, strip-decoded) geometry.
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderModelError> {
        // Halo CE is a `gbxmodel` (mod2) — a different layout (geometries/parts,
        // a `shaders` block, LOD geometry indices). Synthesize the super-type.
        if matches!(Game::of(tag), Game::Halo1) {
            return Self::from_gbxmodel_tag(tag);
        }
        let root = tag.root();
        // Halo 2 ships the same `render_model` super-type (regions / nodes /
        // marker groups / materials), but stores geometry in `sections` (not
        // `render geometry`), references shaders via `shader` (not `render
        // method`), and selects per-permutation geometry by `L1..L6 section
        // index` rather than `mesh index`/`mesh count`. Handle those three
        // deltas here; everything else reuses the H3 readers.
        let game = Game::of(tag);
        Ok(Self {
            name: root.read_string_id("name").unwrap_or_default(),
            flags: root.try_read_flags("flags").unwrap_or_default(),
            regions: read_regions(&root, game)?,
            instance_placements: read_instance_placements(&root),
            nodes: read_nodes(&root)?,
            marker_groups: read_marker_groups(&root),
            materials: read_materials(&root)?,
            // H2 has no `render geometry` block — its meshes live in `sections`
            // and are decoded by `derive_render_meshes`. Leave it empty.
            render_geometry: if matches!(game, Game::Halo2) {
                Geometry::default()
            } else {
                read_geometry(&root)?
            },
            sky_lights: read_sky_lights(&root),
            default_lightprobe: read_default_lightprobe(&root),
            volume_samples: read_volume_samples(&root),
            default_node_orientations: read_default_node_orientations(&root),
        })
    }

    /// Decode every mesh into the renderer-facing [`RenderMesh`] layer
    /// (decompressed vertices, triangle-list indices, resolved water/PRT).
    /// Uses the per-mesh `index buffer type` enum for strip-vs-list. H2 decodes
    /// from `sections` (one [`RenderMesh`] per section); H3 from `render geometry`.
    pub fn derive_render_meshes(tag: &TagFile) -> Result<Vec<RenderMesh>, RenderModelError> {
        match Game::of(tag) {
            Game::Halo1 => return derive_render_meshes_gbxmodel(tag),
            Game::Halo2 => return derive_render_meshes_h2(tag),
            Game::Halo3 => {}
        }
        let root = tag.root();
        let bounds = read_compression_bounds(&root);
        read_meshes_per_mesh(&root, |_| bounds, IndexFormatPolicy::PerMeshSchema)
    }

    /// Synthesize the [`RenderModel`] super-type from a Halo CE `gbxmodel`:
    /// regions/permutations (LOD geometry index → `mesh_index`, since
    /// `derive_render_meshes` emits one [`RenderMesh`] per geometry), materials
    /// from the `shaders` block, nodes for marker placement. Geometry lives in
    /// `geometries` (decoded by `derive_render_meshes`), so `render_geometry`
    /// stays empty. CE markers (a separate block) aren't surfaced yet.
    fn from_gbxmodel_tag(tag: &TagFile) -> Result<Self, RenderModelError> {
        let root = tag.root();
        let geometry_count = root
            .field("geometries")
            .and_then(|f| f.as_block())
            .map(|b| b.len())
            .unwrap_or(0);
        Ok(Self {
            name: root.read_string_id("name").unwrap_or_default(),
            flags: Flags::default(),
            regions: read_gbxmodel_regions(&root, geometry_count)?,
            instance_placements: Vec::new(),
            // CE nodes use a different layout (`default translation` is a
            // real_vector_3d, not real_point_3d) so the H3 `read_nodes` panics on
            // them. Nodes are only needed for markers, which CE doesn't surface in
            // the preview yet — leave empty.
            nodes: Vec::new(),
            marker_groups: Vec::new(),
            materials: read_gbxmodel_materials(&root),
            render_geometry: Geometry::default(),
            sky_lights: Vec::new(),
            default_lightprobe: None,
            volume_samples: Vec::new(),
            default_node_orientations: Vec::new(),
        })
    }

    /// Bind-pose [`RealOrientation`] per node.
    ///
    /// Returns [`Self::default_node_orientations`] verbatim when populated
    /// (cache loads carry the tool.exe-baked block), otherwise derives one
    /// entry per node from `default_translation` + `default_rotation` with
    /// `scale = 1.0`. Empty when the model has no nodes.
    pub fn node_bind_pose(&self) -> Vec<RealOrientation> {
        if !self.default_node_orientations.is_empty() {
            return self
                .default_node_orientations
                .iter()
                .map(|o| RealOrientation {
                    rotation: o.rotation,
                    translation: o.translation,
                    scale: o.scale,
                })
                .collect();
        }
        self.nodes
            .iter()
            .map(|n| RealOrientation {
                rotation: n.default_rotation,
                translation: n.default_translation,
                scale: 1.0,
            })
            .collect()
    }
}

//--------------------------------------------------------------------------------
// Small typed scalar readers (mirroring the api.rs reader style).
//--------------------------------------------------------------------------------

fn read_i16(s: &TagStruct<'_>, name: &str) -> i16 {
    s.read_int_any(name).unwrap_or(0) as i16
}

fn read_i8(s: &TagStruct<'_>, name: &str) -> i8 {
    s.read_int_any(name).unwrap_or(0) as i8
}

fn read_i32(s: &TagStruct<'_>, name: &str) -> i32 {
    s.read_int_any(name).unwrap_or(0) as i32
}

/// Read a `long_flags` field as a raw u32 word (for the 128-bit instance
/// mask, which is four parallel long_flags fields without a typed enum).
fn read_u32_flags_word(s: &TagStruct<'_>, name: &str) -> u32 {
    s.read_int_any(name).unwrap_or(0) as u32
}

/// Read a block of `indices_word_block` (one `word*` per element) into a
/// flat `Vec<u16>`.
fn read_word_block(parent: &TagStruct<'_>, field: &str) -> Vec<u16> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    (0..block.len())
        .filter_map(|k| block.element(k))
        .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
        .collect()
}

/// Read an N-element `array` of single-field elements into a Vec via a
/// per-element reader.
fn read_array_with<T>(
    parent: &TagStruct<'_>,
    field: &str,
    mut read_elem: impl FnMut(&TagStruct<'_>) -> T,
) -> Vec<T> {
    let Some(arr) = parent.field(field).and_then(|f| f.as_array()) else {
        return Vec::new();
    };
    (0..arr.len())
        .filter_map(|k| arr.element(k))
        .map(|e| read_elem(&e))
        .collect()
}

/// First scalar field of a single-field array element, as i128.
fn elem_scalar_i128(e: &TagStruct<'_>) -> i128 {
    e.fields()
        .next()
        .and_then(|f| f.value())
        .map(|v| match v {
            TagFieldData::CharInteger(c) => c as i128,
            TagFieldData::ShortInteger(s) => s as i128,
            TagFieldData::LongInteger(l) => l as i128,
            _ => 0,
        })
        .unwrap_or(0)
}

/// First scalar field of a single-field array element, as f32.
fn elem_scalar_f32(e: &TagStruct<'_>) -> f32 {
    e.fields()
        .next()
        .and_then(|f| f.value())
        .and_then(|v| if let TagFieldData::Real(r) = v { Some(r) } else { None })
        .unwrap_or(0.0)
}

//--------------------------------------------------------------------------------
// Block readers
//--------------------------------------------------------------------------------

fn read_regions(root: &TagStruct<'_>, game: Game) -> Result<Vec<Region>, RenderModelError> {
    let block = root
        .field_path("regions")
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("regions"))?;
    // H2 maps a permutation to geometry by an LOD section index, and
    // `derive_render_meshes_h2` emits one `RenderMesh` per section (section i ->
    // mesh i). For the preview we want the HIGHEST-detail section. The loose-tag
    // `Lx section index` slots are unreliable (often garbage for the high LODs —
    // e.g. rocket_launcher's L1/L2 are stale 24932/25714 while L3..L6 point at the
    // 56-vtx low LOD, with the 1458-vtx mesh referenced by nobody), so pick by
    // VERTEX COUNT instead of trusting the ordering.
    let section_verts: Vec<u32> = root
        .field("sections")
        .and_then(|f| f.as_block())
        .map(|b| {
            (0..b.len())
                .map(|s| {
                    b.element(s)
                        .and_then(|e| e.read_int_any("total vertex count"))
                        .unwrap_or(0)
                        .max(0) as u32
                })
                .collect()
        })
        .unwrap_or_default();
    let single_region = block.len() == 1;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let r = block.element(i).unwrap();
        let name = r.read_string_id("name").unwrap_or_default();
        let mut permutations = Vec::new();
        if let Some(perms) = r.field("permutations").and_then(|f| f.as_block()) {
            for j in 0..perms.len() {
                let p = perms.element(j).unwrap();
                let (mesh_index, mesh_count) = if matches!(game, Game::Halo2) {
                    (h2_permutation_section(&p, &section_verts, single_region), 1)
                } else {
                    (
                        p.read_int_any("mesh index").unwrap_or(-1) as i16,
                        p.read_int_any("mesh count").unwrap_or(0) as i16,
                    )
                };
                permutations.push(Permutation {
                    name: p.read_string_id("name").unwrap_or_default(),
                    mesh_index,
                    mesh_count,
                    instance_mask: [
                        read_u32_flags_word(&p, "instance mask 0-31"),
                        read_u32_flags_word(&p, "instance mask 32-63"),
                        read_u32_flags_word(&p, "instance mask 64-95"),
                        read_u32_flags_word(&p, "instance mask 96-127"),
                    ],
                });
            }
        }
        out.push(Region { name, permutations });
    }
    Ok(out)
}

/// Resolve an H2 permutation to its highest-detail section, by VERTEX COUNT
/// (loose-tag `Lx section index` ordering is unreliable). Candidates are the
/// permutation's in-range `L1..L6 section index` values; for a single-region
/// model the candidates are ALL sections (they're that region's LODs, and the
/// high-detail one is often referenced only by a garbage index). Returns the
/// section index (= the `RenderMesh` index from `derive_render_meshes_h2`), or
/// `-1` for an FX/empty permutation.
fn h2_permutation_section(p: &TagStruct<'_>, section_verts: &[u32], single_region: bool) -> i16 {
    let n = section_verts.len();
    let candidates: Vec<usize> = if single_region {
        (0..n).collect()
    } else {
        ["L1 section index", "L2 section index", "L3 section index",
         "L4 section index", "L5 section index", "L6 section index"]
            .iter()
            .filter_map(|f| {
                p.read_int_any(f)
                    .map(|v| v as i64)
                    .filter(|&v| v >= 0 && (v as usize) < n)
                    .map(|v| v as usize)
            })
            .collect()
    };
    candidates
        .into_iter()
        .max_by_key(|&s| section_verts[s])
        .map(|s| s as i16)
        .unwrap_or(-1)
}

/// Decode H2 geometry: one [`RenderMesh`] per `sections[i]`, so a permutation's
/// resolved section index ([`h2_permutation_section`]) directly indexes the
/// result. H2 `raw vertices` are full-precision floats in object space (the
/// per-section compression bounds are vestigial X360 metadata) — no
/// dequantization or node transform is needed for a bind-pose preview, and UVs
/// are already V-down (no flip), matching [`RenderVertex`].
fn derive_render_meshes_h2(tag: &TagFile) -> Result<Vec<RenderMesh>, RenderModelError> {
    let root = tag.root();
    let sections = root
        .field("sections")
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("sections"))?;
    Ok((0..sections.len())
        .map(|si| read_h2_section(&sections.element(si).unwrap()))
        .collect())
}

/// Decode one H2 `sections[i]` element into a [`RenderMesh`]. Returns an empty
/// mesh (no vertices/parts) when the section carries no geometry, preserving the
/// section↔mesh index alignment.
fn read_h2_section(section: &TagStruct<'_>) -> RenderMesh {
    let rigid_node = section
        .read_int_any("rigid node")
        .map(|v| v as i16)
        .filter(|&v| v >= 0);
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();

    if let Some(sd) = section
        .field("section data")
        .and_then(|f| f.as_block())
        .and_then(|b| b.element(0))
        .and_then(|e| e.field("section").and_then(|f| f.as_struct()))
    {
        if let Some(raw) = sd.field("raw vertices").and_then(|f| f.as_block()) {
            vertices.reserve(raw.len());
            for k in 0..raw.len() {
                vertices.push(read_h2_render_vertex(&raw.element(k).unwrap()));
            }
        }
        // H2 strip indices are u16 with a 0xFFFF restart sentinel between subparts.
        let strip: Vec<u16> = sd
            .field("strip indices")
            .and_then(|f| f.as_block())
            .map(|b| {
                (0..b.len())
                    .filter_map(|k| b.element(k))
                    .map(|e| e.read_int_any("index").unwrap_or(0) as u16)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(pblock) = sd.field("parts").and_then(|f| f.as_block()) {
            for pi in 0..pblock.len() {
                let part = pblock.element(pi).unwrap();
                let material_index = part.read_int_any("material").unwrap_or(0).max(0) as u16;
                let part_type =
                    GeometryPartType::from_raw(part.read_int_any("type").unwrap_or(0) as i8);
                let start = part.read_int_any("strip start index").unwrap_or(0).max(0) as usize;
                let len = part.read_int_any("strip length").unwrap_or(0).max(0) as usize;
                let index_start = indices.len() as u32;
                if start < strip.len() {
                    let end = (start + len).min(strip.len());
                    for (a, b, c) in strip_to_list(&strip[start..end]) {
                        indices.push(a as u32);
                        indices.push(b as u32);
                        indices.push(c as u32);
                    }
                }
                parts.push(RenderMeshPart {
                    material_index,
                    index_start,
                    index_count: indices.len() as u32 - index_start,
                    part_type,
                    transparent_sorting_index: -1,
                    sort_position: None,
                });
            }
        }
    }

    RenderMesh {
        vertices,
        indices,
        parts,
        rigid_node_index: rigid_node,
        water_data: None,
        prt_vertex_type: Enum::default(),
        has_prt_vertex_stream: false,
        prt_ambient_stream: Vec::new(),
        has_vertex_color: false,
        use_region_index_for_sorting: false,
        has_lightmap_uvs: false,
    }
}

/// One H2 `raw vertices[i]` element → [`RenderVertex`]. Full-float object-space
/// position; authored tangent/binormal basis; UV unflipped. Skinning indices are
/// left at defaults — the preview renders the bind pose without per-vertex skin.
fn read_h2_render_vertex(v: &TagStruct<'_>) -> RenderVertex {
    RenderVertex {
        position: v.read_point3d("position"),
        texcoord: v.read_point2d("texcoord"),
        normal: v.read_vec3("normal"),
        tangent: v.read_vec3("tangent"),
        binormal: v.read_vec3("binormal"),
        node_indices: [0; 4],
        node_weights: [1.0, 0.0, 0.0, 0.0],
        lightmap_texcoord: RealPoint2d { x: 0.0, y: 0.0 },
        vert_color: RealVector3d { i: 0.0, j: 0.0, k: 0.0 },
    }
}

/// CE `gbxmodel` regions → [`Region`]s. A permutation's geometry is the
/// highest-detail valid LOD (`super high` → `super low`); store it as
/// `mesh_index` (= the geometry index, which equals the `RenderMesh` index from
/// `derive_render_meshes_gbxmodel`), `mesh_count = 1`.
fn read_gbxmodel_regions(
    root: &TagStruct<'_>,
    geometry_count: usize,
) -> Result<Vec<Region>, RenderModelError> {
    let block = root
        .field("regions")
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("regions"))?;
    let mut out = Vec::with_capacity(block.len());
    for ri in 0..block.len() {
        let r = block.element(ri).unwrap();
        let name = r.read_string("name").unwrap_or_default();
        let mut permutations = Vec::new();
        if let Some(perms) = r.field("permutations").and_then(|f| f.as_block()) {
            for pi in 0..perms.len() {
                let p = perms.element(pi).unwrap();
                let geo = ["super high", "high", "medium", "low", "super low"]
                    .iter()
                    .find_map(|f| {
                        p.read_int_any(f)
                            .map(|v| v as i32)
                            .filter(|&v| v >= 0 && (v as usize) < geometry_count)
                    })
                    .map(|v| v as i16)
                    .unwrap_or(-1);
                permutations.push(Permutation {
                    name: p.read_string("name").unwrap_or_default(),
                    mesh_index: geo,
                    mesh_count: 1,
                    instance_mask: [0; 4],
                });
            }
        }
        out.push(Region { name, permutations });
    }
    Ok(out)
}

/// CE `gbxmodel` `shaders[]` → [`Material`]s. Material `i` ↔ `shaders[i]`; a
/// part's `shader index` is its material index directly.
fn read_gbxmodel_materials(root: &TagStruct<'_>) -> Vec<Material> {
    let Some(block) = root.field("shaders").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    (0..block.len())
        .filter_map(|i| block.element(i))
        .map(|s| {
            let (group, path) = s
                .read_tag_ref_with_group("shader")
                .unwrap_or((0, String::new()));
            Material {
                render_method: path,
                render_method_group: group,
                properties: Vec::new(),
                imported_material_index: -1,
                breakable_surface_index: -1,
            }
        })
        .collect()
}

/// Decode CE geometry: one [`RenderMesh`] per `geometries[i]`, so a permutation's
/// resolved geometry index directly indexes the result. Each `geometries[i]`
/// holds `parts[]` with their own `uncompressed vertices` pool + `triangle data`
/// (3-index chunks, `-1`/`0xFFFF` restart). CE has no authored tangent basis.
fn derive_render_meshes_gbxmodel(tag: &TagFile) -> Result<Vec<RenderMesh>, RenderModelError> {
    let root = tag.root();
    // Model-level base-map UV scale the engine multiplies into every texcoord
    // (0 → "no scale" → 1.0). Without it, tiled-UV models (warthog 2x/3x) smear.
    let uv_scale = [
        root.read_real("base map u scale").filter(|&s| s > 0.0).unwrap_or(1.0),
        root.read_real("base map v scale").filter(|&s| s > 0.0).unwrap_or(1.0),
    ];
    let geometries = root
        .field("geometries")
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("geometries"))?;
    Ok((0..geometries.len())
        .map(|gi| read_gbxmodel_geometry(&geometries.element(gi).unwrap(), uv_scale))
        .collect())
}

/// Decode one CE `geometries[i]` into a [`RenderMesh`]. Each part appends its own
/// vertex pool (offsetting indices) and contributes one [`RenderMeshPart`].
fn read_gbxmodel_geometry(geo: &TagStruct<'_>, uv_scale: [f32; 2]) -> RenderMesh {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut parts = Vec::new();

    if let Some(pblock) = geo.field("parts").and_then(|f| f.as_block()) {
        for pi in 0..pblock.len() {
            let part = pblock.element(pi).unwrap();
            let material_index = part.read_int_any("shader index").unwrap_or(0).max(0) as u16;
            let vbase = vertices.len() as u32;
            if let Some(uv) = part.field("uncompressed vertices").and_then(|f| f.as_block()) {
                for k in 0..uv.len() {
                    vertices.push(read_gbxmodel_render_vertex(&uv.element(k).unwrap(), uv_scale));
                }
            }
            // Flatten the triangle-data chunks (each holds 3 `indices`) into one
            // strip; `-1` becomes the 0xFFFF restart sentinel for strip_to_list.
            let mut strip: Vec<u16> = Vec::new();
            if let Some(td) = part.field("triangle data").and_then(|f| f.as_block()) {
                for k in 0..td.len() {
                    let t = td.element(k).unwrap();
                    for f in t.fields() {
                        if let Some(TagFieldData::ShortInteger(i)) = f.value() {
                            strip.push(i as u16);
                        }
                    }
                }
            }
            let index_start = indices.len() as u32;
            for (a, b, c) in strip_to_list(&strip) {
                // CE triangles wind opposite to their stored normals — reverse
                // (a, c, b) so winding agrees with the normals (matches the
                // General-101 toolset + the existing CE JMS path).
                indices.push(vbase + a as u32);
                indices.push(vbase + c as u32);
                indices.push(vbase + b as u32);
            }
            parts.push(RenderMeshPart {
                material_index,
                index_start,
                index_count: indices.len() as u32 - index_start,
                part_type: GeometryPartType::OpaqueNonShadowing,
                transparent_sorting_index: -1,
                sort_position: None,
            });
        }
    }

    RenderMesh {
        vertices,
        indices,
        parts,
        rigid_node_index: None,
        water_data: None,
        prt_vertex_type: Enum::default(),
        has_prt_vertex_stream: false,
        prt_ambient_stream: Vec::new(),
        has_vertex_color: false,
        use_region_index_for_sorting: false,
        has_lightmap_uvs: false,
    }
}

/// One CE `uncompressed vertices[i]` → [`RenderVertex`]. Full-float object-space
/// position; the model-level base-map UV scale folded into the texcoord; no
/// authored tangent basis (the preview derives one). UV is unflipped.
fn read_gbxmodel_render_vertex(v: &TagStruct<'_>, uv_scale: [f32; 2]) -> RenderVertex {
    let p = v.read_vec3("position");
    let uv = v.read_point2d("texture coords");
    RenderVertex {
        position: RealPoint3d { x: p.i, y: p.j, z: p.k },
        texcoord: RealPoint2d { x: uv.x * uv_scale[0], y: uv.y * uv_scale[1] },
        normal: v.read_vec3("normal"),
        tangent: RealVector3d { i: 0.0, j: 0.0, k: 0.0 },
        binormal: RealVector3d { i: 0.0, j: 0.0, k: 0.0 },
        node_indices: [0; 4],
        node_weights: [1.0, 0.0, 0.0, 0.0],
        lightmap_texcoord: RealPoint2d { x: 0.0, y: 0.0 },
        vert_color: RealVector3d { i: 0.0, j: 0.0, k: 0.0 },
    }
}

fn read_instance_placements(root: &TagStruct<'_>) -> Vec<InstancePlacement> {
    let Some(block) = root.field_path("instance placements").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(InstancePlacement {
            name: e.read_string_id("name").unwrap_or_default(),
            node_index: e.read_block_index("node_index"),
            scale: e.read_real("scale").unwrap_or(1.0),
            forward: e.read_vec3("forward"),
            left: e.read_vec3("left"),
            up: e.read_vec3("up"),
            position: e.read_point3d("position"),
        });
    }
    out
}

/// Re-sliced node inverse-bind (corrects the "Old Mistakes Die Hard"
/// 1-float shift, see [`read_nodes`]).
struct NodeInverseBind {
    scale: f32,
    fwd: RealVector3d,
    left: RealVector3d,
    up: RealVector3d,
    pos: RealPoint3d,
}

fn read_nodes(root: &TagStruct<'_>) -> Result<Vec<Node>, RenderModelError> {
    let block = root
        .field_path("nodes")
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("nodes"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        // Flatten the 13 inverse floats in labeled (= on-disk) order, then
        // re-slice past the 1-float "Old Mistakes Die Hard" shift (see below).
        let lf = n.read_vec3("inverse forward");
        let ll = n.read_vec3("inverse left");
        let lu = n.read_vec3("inverse up");
        let lp = n.read_point3d("inverse position");
        let ls = n.read_real("inverse scale").unwrap_or(0.0);
        let raw = [lf.i, lf.j, lf.k, ll.i, ll.j, ll.k, lu.i, lu.j, lu.k, lp.x, lp.y, lp.z, ls];
        let inv = NodeInverseBind {
            scale: if raw[0].abs() > 1e-6 { raw[0] } else { 1.0 },
            fwd: RealVector3d { i: raw[1], j: raw[2], k: raw[3] },
            left: RealVector3d { i: raw[4], j: raw[5], k: raw[6] },
            up: RealVector3d { i: raw[7], j: raw[8], k: raw[9] },
            pos: RealPoint3d { x: raw[10], y: raw[11], z: raw[12] },
        };
        out.push(Node {
            name: n.read_string_id("name").unwrap_or_default(),
            parent_node: n.read_block_index("parent node"),
            first_child_node: n.read_block_index("first child node"),
            next_sibling_node: n.read_block_index("next sibling node"),
            default_translation: n.read_point3d("default translation"),
            default_rotation: n.read_quat("default rotation"),
            // "Old Mistakes Die Hard" (schema explanation on this block): the
            // inverse-bind fields are stored SHIFTED by one float. The real
            // layout is [inverse_scale, inverse_forward(3), inverse_left(3),
            // inverse_up(3), inverse_position(3)] but they're LABELED
            // [forward, left, up, position, scale]. So labeled `inverse
            // forward`.i is actually inverse_scale, and labeled `inverse
            // scale` is actually inverse_position.z. Read all 13 floats in
            // labeled (= raw) order and re-slice into the true layout. The
            // engine (model_skinning_matrix_from_real_matrix4x3s) reads the
            // true layout; these inverse fields are the per-node bind-pose
            // inverse used to build the skinning palette node_world×inverse.
            inverse_forward: inv.fwd,
            inverse_left: inv.left,
            inverse_up: inv.up,
            inverse_position: inv.pos,
            inverse_scale: inv.scale,
            distance_from_parent: n.read_real("distance from parent").unwrap_or(0.0),
        });
    }
    Ok(out)
}

fn read_marker_groups(root: &TagStruct<'_>) -> Vec<MarkerGroup> {
    let Some(block) = root.field_path("marker groups").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let g = block.element(i).unwrap();
        let name = g.read_string_id("name").unwrap_or_default();
        let mut markers = Vec::new();
        if let Some(inner) = g.field("markers").and_then(|f| f.as_block()) {
            for j in 0..inner.len() {
                let m = inner.element(j).unwrap();
                markers.push(Marker {
                    region_index: m.read_int_any("region index").unwrap_or(-1) as i8,
                    permutation_index: m.read_int_any("permutation index").unwrap_or(-1) as i8,
                    node_index: m.read_int_any("node index").unwrap_or(-1) as i8,
                    translation: m.read_point3d("translation"),
                    rotation: m.read_quat("rotation"),
                    scale: m.read_real("scale").unwrap_or(1.0),
                });
            }
        }
        out.push(MarkerGroup { name, markers });
    }
    out
}

fn read_materials(root: &TagStruct<'_>) -> Result<Vec<Material>, RenderModelError> {
    let Some(block) = root.field_path("materials").and_then(|f| f.as_block()) else {
        // `materials` is `*`-optional in the schema; some tags carry none.
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        // H3 references the shader via `render method`; H2 via `shader`.
        let (group, path) = m
            .read_tag_ref_with_group("render method")
            .filter(|(_, p)| !p.is_empty())
            .or_else(|| m.read_tag_ref_with_group("shader"))
            .unwrap_or((0, String::new()));
        let mut properties = Vec::new();
        if let Some(props) = m.field("properties").and_then(|f| f.as_block()) {
            for j in 0..props.len() {
                let p = props.element(j).unwrap();
                properties.push(MaterialProperty {
                    property_type: p.try_read_enum("type").unwrap_or_default(),
                    int_value: read_i16(&p, "int-value"),
                    long_value: read_i32(&p, "long-value"),
                    real_value: p.read_real("real-value").unwrap_or(0.0),
                });
            }
        }
        out.push(Material {
            render_method: path,
            render_method_group: group,
            properties,
            imported_material_index: read_i32(&m, "imported material index"),
            breakable_surface_index: read_i8(&m, "breakable surface index"),
        });
    }
    Ok(out)
}

pub(crate) fn read_geometry(root: &TagStruct<'_>) -> Result<Geometry, RenderModelError> {
    let geo = root
        .field("render geometry")
        .and_then(|f| f.as_struct())
        .ok_or(RenderModelError::MissingField("render geometry"))?;
    Ok(read_geometry_from(&geo))
}

/// Decode a `global_render_geometry_struct` from an already-resolved
/// struct (the caller has navigated to the geometry-bearing field). Used
/// by `structure_bsp`, whose `render geometry` and `decorator instance
/// buffer` fields both carry this schema.
pub(crate) fn read_geometry_from(geo: &TagStruct<'_>) -> Geometry {
    let geo = geo;
    Geometry {
        flags: geo.try_read_flags("runtime flags").unwrap_or_default(),
        meshes: read_meshes_schema(&geo),
        compression_info: read_compression_info(&geo),
        part_sorting_position: read_sorting_positions(&geo),
        per_mesh_temporary: read_per_mesh_temporary(&geo),
        per_mesh_node_map: read_per_mesh_node_map(&geo),
        per_mesh_prt_data: read_per_mesh_prt_data(&geo),
        per_instance_lightmap_texcoords: read_per_instance_lightmap_texcoords(&geo),
    }
}

fn read_meshes_schema(geo: &TagStruct<'_>) -> Vec<Mesh> {
    let Some(block) = geo.field("meshes").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        let mut parts = Vec::new();
        if let Some(pb) = m.field("parts").and_then(|f| f.as_block()) {
            for j in 0..pb.len() {
                let p = pb.element(j).unwrap();
                parts.push(Part {
                    render_method_index: p.read_block_index("render method index"),
                    transparent_sorting_index: p.read_block_index("transparent sorting index"),
                    index_start: read_i16(&p, "index start"),
                    index_count: read_i16(&p, "index count"),
                    subpart_start: read_i16(&p, "subpart start"),
                    subpart_count: read_i16(&p, "subpart count"),
                    part_type: GeometryPartType::from_raw(read_i8(&p, "part type")),
                    part_flags: p.try_read_flags("part flags").unwrap_or_default(),
                    budget_vertex_count: read_i16(&p, "budget vertex count"),
                });
            }
        }
        let mut subparts = Vec::new();
        if let Some(sb) = m.field("subparts").and_then(|f| f.as_block()) {
            for j in 0..sb.len() {
                let s = sb.element(j).unwrap();
                subparts.push(Subpart {
                    index_start: read_i16(&s, "index start"),
                    index_count: read_i16(&s, "index count"),
                    part_index: s.read_block_index("part index"),
                    budget_vertex_count: read_i16(&s, "budget vertex count"),
                });
            }
        }
        let vbi_vec = read_array_with(&m, "vertex buffer indices", |e| elem_scalar_i128(e) as u16);
        let mut vertex_buffer_indices = [0u16; 8];
        for (k, v) in vbi_vec.iter().take(8).enumerate() {
            vertex_buffer_indices[k] = *v;
        }
        let mut instance_buckets = Vec::new();
        if let Some(ib) = m.field("instance buckets").and_then(|f| f.as_block()) {
            for j in 0..ib.len() {
                let b = ib.element(j).unwrap();
                instance_buckets.push(InstanceBucket {
                    mesh_index: read_i16(&b, "mesh index"),
                    definition_index: read_i16(&b, "definition index"),
                    instances: read_word_block(&b, "instances"),
                });
            }
        }
        out.push(Mesh {
            parts,
            subparts,
            vertex_buffer_indices,
            index_buffer_index: read_i16(&m, "index buffer index"),
            index_buffer_tessellation: read_i16(&m, "index buffer tessellation"),
            mesh_flags: m.try_read_flags("mesh flags").unwrap_or_default(),
            rigid_node_index: read_i8(&m, "rigid node index"),
            vertex_type: m.try_read_enum("vertex type").unwrap_or_default(),
            prt_vertex_type: m.try_read_enum("PRT vertex type").unwrap_or_default(),
            index_buffer_type: m.try_read_enum("index buffer type").unwrap_or_default(),
            instance_buckets,
            water_indices_start: read_word_block(&m, "water indices start"),
        });
    }
    out
}

fn read_compression_info(geo: &TagStruct<'_>) -> Vec<CompressionInfo> {
    let Some(block) = geo.field("compression info").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let c = block.element(i).unwrap();
        out.push(CompressionInfo {
            flags: c.try_read_flags("compression flags").unwrap_or_default(),
            position_bounds: [
                c.read_point3d("position bounds 0"),
                c.read_point3d("position bounds 1"),
            ],
            texcoord_bounds: [
                c.read_point2d("texcoord bounds 0"),
                c.read_point2d("texcoord bounds 1"),
            ],
        });
    }
    out
}

fn read_sorting_positions(geo: &TagStruct<'_>) -> Vec<SortingPosition> {
    let Some(block) = geo.field("part sorting position").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let s = block.element(i).unwrap();
        let idx_vec = read_array_with(&s, "node indices", |e| elem_scalar_i128(e) as u8);
        let mut node_indices = [0u8; 4];
        for (k, v) in idx_vec.iter().take(4).enumerate() {
            node_indices[k] = *v;
        }
        let wt_vec = read_array_with(&s, "node weights", elem_scalar_f32);
        let mut node_weights = [0f32; 3];
        for (k, v) in wt_vec.iter().take(3).enumerate() {
            node_weights[k] = *v;
        }
        out.push(SortingPosition {
            plane: s.read_plane3d("plane"),
            position: s.read_point3d("position"),
            radius: s.read_real("radius").unwrap_or(0.0),
            node_indices,
            node_weights,
        });
    }
    out
}

fn read_raw_vertex(v: &TagStruct<'_>) -> RawVertex {
    let idx_vec = read_array_with(v, "node indices", |e| elem_scalar_i128(e) as u8);
    let mut node_indices = [0u8; 4];
    for (k, x) in idx_vec.iter().take(4).enumerate() {
        node_indices[k] = *x;
    }
    let wt_vec = read_array_with(v, "node weights", elem_scalar_f32);
    let mut node_weights = [0f32; 4];
    for (k, x) in wt_vec.iter().take(4).enumerate() {
        node_weights[k] = *x;
    }
    RawVertex {
        position: v.read_point3d("position"),
        texcoord: v.read_point2d("texcoord"),
        normal: v.read_point3d("normal").as_vector(),
        binormal: v.read_point3d("binormal").as_vector(),
        tangent: v.read_point3d("tangent").as_vector(),
        lightmap_texcoord: v.read_point2d("lightmap texcoord"),
        node_indices,
        node_weights,
        vertex_color: v.read_point3d("vertex color").as_vector(),
    }
}

fn read_per_mesh_temporary(geo: &TagStruct<'_>) -> Vec<PerMeshTemporary> {
    let Some(block) = geo.field("per mesh temporary").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let p = block.element(i).unwrap();
        let raw_vertices = p
            .field("raw vertices")
            .and_then(|f| f.as_block())
            .map(|b| {
                (0..b.len())
                    .filter_map(|k| b.element(k))
                    .map(|v| read_raw_vertex(&v))
                    .collect()
            })
            .unwrap_or_default();
        let raw_indices = read_word_block(&p, "raw indices");
        let raw_water_data = p
            .field("raw water data")
            .and_then(|f| f.as_block())
            .filter(|b| !b.is_empty())
            .and_then(|b| b.element(0))
            .map(|e| RawWaterDataSchema {
                indices: read_word_block(&e, "raw water indices"),
                vertices: e
                    .field("raw water vertices")
                    .and_then(|f| f.as_block())
                    .map(|wb| {
                        (0..wb.len())
                            .filter_map(|k| wb.element(k))
                            .map(|w| RawWaterAppend {
                                local_info: w.read_point3d("local info"),
                                water_velocity: w.read_point3d("water velocity"),
                                base_texcoord: w.read_point3d("base texcoord"),
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        out.push(PerMeshTemporary {
            raw_vertices,
            raw_indices,
            raw_water_data,
            parameterized_texture_width: read_i16(&p, "parameterized texture width"),
            parameterized_texture_height: read_i16(&p, "parameterized texture height"),
            flags: p.try_read_flags("flags").unwrap_or_default(),
        });
    }
    out
}

fn read_per_mesh_node_map(geo: &TagStruct<'_>) -> Vec<Vec<u8>> {
    let Some(block) = geo.field("per mesh node map").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let e = block.element(i).unwrap();
        let map = e
            .field("node map")
            .and_then(|f| f.as_block())
            .map(|nb| {
                (0..nb.len())
                    .filter_map(|k| nb.element(k))
                    .map(|n| n.read_int_any("node index").unwrap_or(0) as u8)
                    .collect()
            })
            .unwrap_or_default();
        out.push(map);
    }
    out
}

fn read_per_mesh_prt_data(geo: &TagStruct<'_>) -> Vec<PerMeshPrtData> {
    let Some(block) = geo.field("per_mesh_prt_data").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let e = block.element(i).unwrap();
        let mesh_pca_data = e
            .field("mesh pca data")
            .and_then(|f| f.as_data())
            .map(|b| b.to_vec())
            .unwrap_or_default();
        out.push(PerMeshPrtData { mesh_pca_data });
    }
    out
}

fn read_per_instance_lightmap_texcoords(geo: &TagStruct<'_>) -> Vec<PerInstanceLightmapTexcoords> {
    let Some(block) = geo
        .field("per_instance_lightmap_texcoords")
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let e = block.element(i).unwrap();
        let texture_coordinates = e
            .field("texture coordinates")
            .and_then(|f| f.as_block())
            .map(|tc| {
                (0..tc.len())
                    .filter_map(|k| tc.element(k))
                    .map(|v| read_raw_vertex(&v))
                    .collect()
            })
            .unwrap_or_default();
        out.push(PerInstanceLightmapTexcoords {
            texture_coordinates,
            vertex_buffer_index: read_i16(&e, "vertex buffer index"),
        });
    }
    out
}

fn read_sky_lights(root: &TagStruct<'_>) -> Vec<SkyLight> {
    let Some(block) = root.field("sky lights").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(SkyLight {
            direction: e.read_vec3("direction"),
            intensity: e.read_vec3("intensity"),
            solid_angle: e.read_real("solid angle").unwrap_or(0.0),
        });
    }
    out
}

fn read_default_lightprobe(root: &TagStruct<'_>) -> Option<DefaultLightprobe> {
    fn read_channel(root: &TagStruct<'_>, name: &str) -> Option<[f32; 16]> {
        let arr = root.field(name)?.as_array()?;
        let mut out = [0.0f32; 16];
        for i in 0..arr.len().min(16) {
            let e = arr.element(i)?;
            out[i] = e.read_real("coefficient").unwrap_or(0.0);
        }
        Some(out)
    }
    Some(DefaultLightprobe {
        r: read_channel(root, "default lightprobe r")?,
        g: read_channel(root, "default lightprobe g")?,
        b: read_channel(root, "default lightprobe b")?,
    })
}

fn read_volume_samples(root: &TagStruct<'_>) -> Vec<VolumeSample> {
    let Some(block) = root.field("volume samples").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let e = block.element(i).unwrap();
        let rt = read_array_with(&e, "radiance transfer matrix", elem_scalar_f32);
        let mut radiance_transfer = [0f32; 81];
        for (k, v) in rt.iter().take(81).enumerate() {
            radiance_transfer[k] = *v;
        }
        out.push(VolumeSample {
            position: e.read_vec3("position"),
            radiance_transfer,
        });
    }
    out
}

fn read_default_node_orientations(root: &TagStruct<'_>) -> Vec<NodeOrientation> {
    let Some(block) = root.field("runtime node orientations").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(NodeOrientation {
            rotation: e.read_quat("rotation"),
            translation: e.read_point3d("translation"),
            scale: e.read_real("scale").unwrap_or(1.0),
        });
    }
    out
}

//================================================================================
// Derived render-geometry decode (decompress + strip-decode + water/PRT).
// Reusable on render_model / sbsp / lightmap render geometry.
//================================================================================

/// Index-buffer interpretation policy. Halo 3 sbsp `render geometry`
/// stores all index buffers as triangle lists despite the schema's
/// `index buffer type` enum sometimes claiming "triangle strip". Pass
/// `PerMeshSchema` for `render_model` (mode) tags.
#[derive(Debug, Clone, Copy)]
pub enum IndexFormatPolicy {
    /// Use the per-mesh `index buffer type` enum (correct for `mode`).
    PerMeshSchema,
    /// Force triangle list (correct for `sbsp` / lightmap geometry).
    ForceTriangleList,
}

/// Decode every mesh from the `render geometry` block of an arbitrary
/// root struct (`render_model` or `scenario_structure_bsp`). Compression
/// bounds are auto-paired: mesh `i` uses `compression info[i]` when it
/// declares compression, else identity.
pub fn extract_render_geometry_meshes(
    root: &TagStruct<'_>,
) -> Result<Vec<RenderMesh>, RenderModelError> {
    extract_render_geometry_meshes_with_bounds(root, |mi| {
        let bounds = crate::geometry::read_compression_bounds_at(root, mi);
        if bounds.pos_compressed || bounds.uv_compressed {
            bounds
        } else {
            crate::geometry::CompressionBounds::identity()
        }
    })
}

/// As [`extract_render_geometry_meshes`] but the caller picks the
/// compression bounds per mesh.
pub fn extract_render_geometry_meshes_with_bounds<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    read_meshes_per_mesh(root, bounds_for, IndexFormatPolicy::PerMeshSchema)
}

/// sbsp-specific extractor: forces triangle-list interpretation on every
/// mesh (the schema enum lies about strip-vs-list for sbsp).
pub fn extract_sbsp_render_geometry_meshes<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    read_meshes_per_mesh(root, bounds_for, IndexFormatPolicy::ForceTriangleList)
}

/// Walk the lightmap tag's `imported geometry` (vertex-aligned copy of
/// the sbsp `render geometry`, carrying the real lightmap UVs).
pub fn extract_imported_geometry_meshes<F>(
    root: &TagStruct<'_>,
    bounds_for: F,
) -> Result<Vec<RenderMesh>, RenderModelError>
where
    F: Fn(usize) -> CompressionBounds,
{
    read_meshes_at_path(root, "imported geometry", bounds_for, IndexFormatPolicy::ForceTriangleList)
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
    let pmt_block = root
        .field_path(&pmt_path)
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("render geometry/per mesh temporary"))?;
    let meshes_block = root
        .field_path(&meshes_path)
        .and_then(|f| f.as_block())
        .ok_or(RenderModelError::MissingField("render geometry/meshes"))?;

    let prt_path = format!("{path_prefix}/per_mesh_prt_data");
    let prt_data_block = root.field_path(&prt_path).and_then(|f| f.as_block());

    // Geometry-level transparent sort positions (one `SortingPosition` per
    // entry). Each transparent part references one by `transparent sorting
    // index`. Read once here so the per-part loop can resolve + attach it to
    // the render-ready `RenderMeshPart` (engine feeds these to
    // `c_transparency_renderer::add_element` as the back-to-front sort key).
    let sort_positions: Vec<SortingPosition> = root
        .field_path(path_prefix)
        .and_then(|f| f.as_struct())
        .map(|geo| read_sorting_positions(&geo))
        .unwrap_or_default();

    let count = meshes_block.len();
    let mut out = Vec::with_capacity(count);
    for mi in 0..count {
        let mesh = meshes_block.element(mi).unwrap();
        let mut bounds = bounds_for(mi);

        // Rigid meshes store the single bone via the mesh-level `rigid
        // node index`; per-vertex node arrays are typically all zero.
        // `vertex type` is a char_enum in most tags, but some (e.g. zanzibar
        // dome_light) carry an embedded schema that types it as a plain
        // char_integer — same value (1 = rigid), different TagFieldData
        // variant. Accept BOTH, else the mesh decodes to -1 and misses the
        // format-based decompression below (→ rendered at raw 0..1 scale).
        let vt = mesh.field("vertex type").and_then(|f| f.value()).map(|v| match v {
            TagFieldData::CharEnum { value, .. } => value as i32,
            TagFieldData::CharInteger(value) => value as i32,
            _ => -1,
        }).unwrap_or(-1);

        // ⭐Decompression is gated by the VERTEX FORMAT, not the
        // `compression flags` word. Verified against BOTH the dllcache and
        // Ares: `s_compression_info::compression_flags` is referenced ONLY in
        // the struct definition — never read. The CPU extract path
        // (`extract_rigid_vertex_data @0x180490610`) and the GPU submit path
        // (`render_mesh_submit_compression @0x18069DF90`) apply
        // `position_bounds`/`texcoord_bounds` whenever the vertex format is
        // rigid/skinned (positions stored normalized 0..1), and the `world`
        // format path (`extract_world_vertex_data @0x180490a10`) copies
        // full-precision floats with NO bounds. `read_compression_bounds_at`
        // derives `pos/uv_compressed` from the flag word, which is WRONG: e.g.
        // zanzibar `dome_light` is rigid with normalized verts but has
        // `compression flags = 0`, so the flag path left it un-decompressed →
        // the model rendered at raw 0..1 scale (≈5× too big) and offset to
        // +0.5 (wrong location). Override per-mesh by format: rigid(1)/
        // flat-rigid(5)/skinned(2)/flat-skinned(6) → always decompress;
        // world(0)/flat-world(4) → never (already full-precision).
        if std::env::var_os("BLAM_DIAG_DECOMPRESS").is_some() {
            eprintln!(
                "[decompress] mesh{mi} vt={vt} flag_pos_compressed={} -> override, pbounds x[{:.4},{:.4}] y[{:.4},{:.4}] z[{:.4},{:.4}]",
                bounds.pos_compressed,
                bounds.px_min, bounds.px_max, bounds.py_min, bounds.py_max, bounds.pz_min, bounds.pz_max,
            );
        }
        match vt {
            1 | 2 | 5 | 6 => { bounds.pos_compressed = true; bounds.uv_compressed = true; }
            0 | 4 => { bounds.pos_compressed = false; bounds.uv_compressed = false; }
            _ => {}
        }

        let rigid_node_index = if matches!(vt, 1 | 5) {
            mesh.read_int_any("rigid node index").map(|v| v as i16).filter(|&v| v >= 0)
        } else {
            None
        };

        let prt_vertex_type = mesh.try_read_enum("PRT vertex type").unwrap_or_default();
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

        let mesh_flags: Flags<MeshFlags, u8> = mesh.try_read_flags("mesh flags").unwrap_or_default();
        let has_vertex_color = mesh_flags.contains(MeshFlags::MeshHasVertexColor);
        let use_region_index_for_sorting =
            mesh_flags.contains(MeshFlags::UseRegionIndexForSorting);

        let empty_mesh = || RenderMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            parts: Vec::new(),
            rigid_node_index,
            water_data: None,
            prt_vertex_type,
            has_prt_vertex_stream,
            prt_ambient_stream: Vec::new(),
            has_vertex_color,
            use_region_index_for_sorting,
            has_lightmap_uvs: false,
        };
        let Some(pmt) = pmt_block.element(mi) else {
            out.push(empty_mesh());
            continue;
        };
        let Some(raw_v) = pmt.field("raw vertices").and_then(|f| f.as_block()) else {
            out.push(empty_mesh());
            continue;
        };
        let Some(raw_i) = pmt.field("raw indices").and_then(|f| f.as_block()) else {
            out.push(empty_mesh());
            continue;
        };

        let mut vertices: Vec<RenderVertex> = Vec::with_capacity(raw_v.len());
        for k in 0..raw_v.len() {
            let v = raw_v.element(k).unwrap();
            vertices.push(decode_render_vertex(&v, &bounds, rigid_node_index));
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

        let parts_block = mesh
            .field("parts")
            .and_then(|f| f.as_block())
            .ok_or(RenderModelError::MissingField("meshes[i]/parts"))?;
        // Decode per-subpart when a part declares them (matching ass.rs /
        // the H3 toolset rule); part-level `index start/count` is only a
        // summary and decodes WRONG for multi-subpart parts. Fall back to
        // the part's own range when subparts are absent.
        let subparts_block = mesh.field("subparts").and_then(|f| f.as_block());

        let mut indices: Vec<u32> = Vec::new();
        let mut parts: Vec<RenderMeshPart> = Vec::with_capacity(parts_block.len());
        // Normalize an `index start` that may be a wrapped i16 (H3 short).
        let norm_start = |start_i: i128| -> usize {
            if start_i < 0 { (start_i as i16 as u16) as usize } else { start_i as usize }
        };

        if !is_strip {
            // Triangle-list (BSP cluster/instance) meshes: keep the RAW index
            // buffer untouched. `structure_surface_to_triangle_mapping
            // .triangle_index` — which the geometry sampler uses to map a
            // collision hit → render triangle — is a position in THIS buffer,
            // so reordering it (the old per-subpart reassembly) made the sampler
            // resolve wrong triangles. Each part's draw range is its subparts'
            // contiguous raw span; sum the subpart counts as u32 because the
            // part-level `index count` is an i16 that overflows on large meshes
            // (the bug the 2026-06-07 per-subpart pass worked around).
            indices = raw_index_list.iter().map(|&i| i as u32).collect();
            let n = indices.len() as u32;
            for pi in 0..parts_block.len() {
                let part = parts_block.element(pi).unwrap();
                let material_index = part.read_int_any("render method index").unwrap_or(0).max(0) as u16;
                let part_type = GeometryPartType::from_raw(part.read_int_any("part type").unwrap_or(0) as i8);
                let sub_start = part.read_int_any("subpart start").unwrap_or(0);
                let sub_count = part.read_int_any("subpart count").unwrap_or(0);
                let (mut index_start, mut index_count) = (0u32, 0u32);
                let mut from_subparts = false;
                if let Some(sps) = subparts_block.as_ref() {
                    if sub_count > 0 {
                        let mut start = usize::MAX;
                        let mut total = 0usize;
                        for off in 0..sub_count as usize {
                            let Some(sp) = sps.element(sub_start as usize + off) else { break };
                            let s = norm_start(sp.read_int_any("index start").unwrap_or(0));
                            let c = sp.read_int_any("index count").unwrap_or(0).max(0) as usize;
                            start = start.min(s);
                            total += c;
                        }
                        if start != usize::MAX {
                            index_start = start as u32;
                            index_count = total as u32;
                            from_subparts = true;
                        }
                    }
                }
                if !from_subparts {
                    index_start = norm_start(part.read_int_any("index start").unwrap_or(0)) as u32;
                    index_count = part.read_int_any("index count").unwrap_or(0).max(0) as u32;
                }
                if index_start > n { index_start = n; }
                if index_start + index_count > n { index_count = n - index_start; }
                let transparent_sorting_index = part.read_block_index("transparent sorting index");
                let sort_position = if transparent_sorting_index >= 0 {
                    sort_positions.get(transparent_sorting_index as usize).copied()
                } else { None };
                parts.push(RenderMeshPart {
                    material_index, index_start, index_count, part_type,
                    transparent_sorting_index, sort_position,
                });
            }
        } else {
            // Triangle-strip (object render_model) meshes: de-strip per subpart
            // into a fresh triangle-list buffer. These carry no structure-surface
            // mapping, so the reassembled order is never indexed by the sampler.
            let emit_range = |start_i: i128, count_i: i128, indices: &mut Vec<u32>| {
                if count_i <= 0 { return; }
                let start = norm_start(start_i);
                let count = count_i as usize;
                if start >= raw_index_list.len() { return; }
                let end = (start + count).min(raw_index_list.len());
                for (a, b, c) in strip_to_list(&raw_index_list[start..end]) {
                    indices.push(a as u32);
                    indices.push(b as u32);
                    indices.push(c as u32);
                }
            };
            for pi in 0..parts_block.len() {
                let part = parts_block.element(pi).unwrap();
                let material_index = part.read_int_any("render method index").unwrap_or(0).max(0) as u16;
                let part_type = GeometryPartType::from_raw(part.read_int_any("part type").unwrap_or(0) as i8);
                let part_index_start = indices.len() as u32;
                let sub_start = part.read_int_any("subpart start").unwrap_or(0);
                let sub_count = part.read_int_any("subpart count").unwrap_or(0);
                let mut used_subparts = false;
                if let Some(sps) = subparts_block.as_ref() {
                    if sub_count > 0 {
                        for off in 0..sub_count as usize {
                            let Some(sp) = sps.element(sub_start as usize + off) else { break };
                            let s = sp.read_int_any("index start").unwrap_or(0);
                            let c = sp.read_int_any("index count").unwrap_or(0);
                            emit_range(s, c, &mut indices);
                        }
                        used_subparts = true;
                    }
                }
                if !used_subparts {
                    let start_i = part.read_int_any("index start").unwrap_or(0);
                    let count_i = part.read_int_any("index count").unwrap_or(0);
                    emit_range(start_i, count_i, &mut indices);
                }
                let part_index_count = indices.len() as u32 - part_index_start;
                let transparent_sorting_index = part.read_block_index("transparent sorting index");
                let sort_position = if transparent_sorting_index >= 0 {
                    sort_positions.get(transparent_sorting_index as usize).copied()
                } else { None };
                parts.push(RenderMeshPart {
                    material_index,
                    index_start: part_index_start,
                    index_count: part_index_count,
                    part_type,
                    transparent_sorting_index,
                    sort_position,
                });
            }
        }

        let water_data = read_raw_water_data(&mesh, &pmt, &raw_index_list, &parts_block);

        // PRT Ambient bake: `per_mesh_prt_data[mi].mesh pca data` = 3
        // little-endian floats RGB per vertex, averaged to grayscale.
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

        let has_lightmap_uvs = vertices
            .iter()
            .any(|v| v.lightmap_texcoord.x != 0.0 || v.lightmap_texcoord.y != 0.0);

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
            use_region_index_for_sorting,
            has_lightmap_uvs,
        });
    }
    Ok(out)
}

/// Decorator-specific extractor: per-subpart triangle-lists from
/// `render geometry/meshes[0]`.
pub fn extract_decorator_subparts(tag: &TagFile) -> Option<DecoratorSubpartGeometry> {
    let root = tag.root();
    let bounds = read_compression_bounds(&root);

    let pmt = root.field_path("render geometry/per mesh temporary").and_then(|f| f.as_block())?;
    let meshes = root.field_path("render geometry/meshes").and_then(|f| f.as_block())?;
    if meshes.is_empty() || pmt.is_empty() {
        return None;
    }

    let mesh = meshes.element(0)?;
    let pmt0 = pmt.element(0)?;
    let raw_v = pmt0.field("raw vertices").and_then(|f| f.as_block())?;
    let raw_i = pmt0.field("raw indices").and_then(|f| f.as_block())?;

    let mut vertices: Vec<RenderVertex> = Vec::with_capacity(raw_v.len());
    for k in 0..raw_v.len() {
        let v = raw_v.element(k)?;
        vertices.push(decode_render_vertex(&v, &bounds, None));
    }
    let raw_index_list: Vec<u16> = (0..raw_i.len())
        .filter_map(|k| raw_i.element(k))
        .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
        .collect();

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
            let tris = strip_to_list(strip);
            let mut flat = Vec::with_capacity(tris.len() * 3);
            for (a, b, c) in tris {
                flat.push(a as u32);
                flat.push(b as u32);
                flat.push(c as u32);
            }
            subpart_indices.push(flat);
        }
    } else {
        let tris = strip_to_list(&raw_index_list);
        let mut flat = Vec::with_capacity(tris.len() * 3);
        for (a, b, c) in tris {
            flat.push(a as u32);
            flat.push(b as u32);
            flat.push(c as u32);
        }
        subpart_indices.push(flat);
    }

    Some(DecoratorSubpartGeometry { vertices, subpart_indices })
}

/// Per-instance lightmap UV streams from the lightmap tag's
/// `imported geometry/per_instance_lightmap_texcoords[]`.
#[derive(Debug, Clone)]
pub struct PerInstanceLightmapUvs {
    pub block_index: usize,
    pub uvs: Vec<RealPoint2d>,
}

/// Walk the lightmap tag's `per_instance_lightmap_texcoords` block; only
/// the `lightmap texcoord` field of each `texture coordinates` entry is read.
pub fn extract_per_instance_lightmap_uvs(root: &TagStruct<'_>) -> Vec<PerInstanceLightmapUvs> {
    let Some(block) = root
        .field_path("imported geometry/per_instance_lightmap_texcoords")
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(elem) = block.element(i) else { continue };
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

/// Walk water-flagged parts and produce already-resolved per-triangle
/// `(regular_idx, water_idx)` control-point pairs. Mirrors the cache-build
/// walk in `?create_mesh_water_vertex_buffer @ 0x82e094e8`.
fn read_raw_water_data(
    mesh: &TagStruct<'_>,
    pmt: &TagStruct<'_>,
    raw_index_list: &[u16],
    parts_block: &TagBlock<'_>,
) -> Option<RawWaterData> {
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

    let mut raw_water_indices: Vec<u16> = Vec::with_capacity(water_indices_block.len());
    for k in 0..water_indices_block.len() {
        let Some(e) = water_indices_block.element(k) else { continue };
        raw_water_indices.push(e.read_int_any("word").unwrap_or(0) as u16);
    }

    let mut vertices: Vec<RawWaterAppend> = Vec::with_capacity(vertices_block.len());
    for k in 0..vertices_block.len() {
        let Some(e) = vertices_block.element(k) else { continue };
        vertices.push(RawWaterAppend {
            local_info: e.read_point3d("local info"),
            water_velocity: e.read_point3d("water velocity"),
            base_texcoord: e.read_point3d("base texcoord"),
        });
    }

    let water_starts_block = mesh.field("water indices start").and_then(|f| f.as_block())?;
    let mut water_indices_start: Vec<u16> = Vec::with_capacity(water_starts_block.len());
    for k in 0..water_starts_block.len() {
        let Some(e) = water_starts_block.element(k) else { continue };
        water_indices_start.push(e.read_int_any("word").unwrap_or(0) as u16);
    }
    if water_indices_start.is_empty() {
        return None;
    }

    let mut triangles: Vec<RawWaterTriangle> = Vec::new();
    let mut parts: Vec<RawWaterPart> = Vec::new();
    for p in 0..parts_block.len() {
        let Some(part) = parts_block.element(p) else { continue };
        let part_flags: Flags<PartFlags, u8> = part.try_read_flags("part flags").unwrap_or_default();
        if !part_flags.contains(PartFlags::IsWaterSurface) {
            continue;
        }
        let regular_base = part.read_int_any("index start").unwrap_or(0);
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

fn decode_render_vertex(
    v: &TagStruct<'_>,
    bounds: &CompressionBounds,
    rigid_node_index: Option<i16>,
) -> RenderVertex {
    let raw_pos = v.read_point3d("position");
    let position = bounds.decompress_position(raw_pos);
    let normal = v.read_point3d("normal").as_vector();
    let tangent = v.read_point3d("tangent").as_vector();
    let binormal = v.read_point3d("binormal").as_vector();
    let raw_uv = v.read_point2d("texcoord");
    let texcoord = bounds.decompress_texcoord(raw_uv);
    let lightmap_texcoord = v.read_point2d("lightmap texcoord");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::{RealOrientation, RealPoint3d, RealQuaternion};


    fn node(tx: RealPoint3d, rot: RealQuaternion) -> Node {
        Node {
            name: "root".into(),
            parent_node: -1,
            first_child_node: -1,
            next_sibling_node: -1,
            default_translation: tx,
            default_rotation: rot,
            inverse_forward: RealVector3d { i: 1.0, j: 0.0, k: 0.0 },
            inverse_left: RealVector3d { i: 0.0, j: 1.0, k: 0.0 },
            inverse_up: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
            inverse_position: RealPoint3d::ZERO,
            inverse_scale: 1.0,
            distance_from_parent: 0.0,
        }
    }

    /// `node_bind_pose` returns the tag block verbatim when populated.
    #[test]
    fn node_bind_pose_prefers_tag_block_when_present() {
        let baked = NodeOrientation {
            rotation: RealQuaternion { i: 0.1, j: 0.2, k: 0.3, w: 0.4 },
            translation: RealPoint3d { x: 1.0, y: 2.0, z: 3.0 },
            scale: 0.5,
        };
        let rm = RenderModel {
            nodes: vec![node(RealPoint3d::ZERO, RealQuaternion::IDENTITY)],
            default_node_orientations: vec![baked],
            ..Default::default()
        };
        let pose = rm.node_bind_pose();
        assert_eq!(pose.len(), 1);
        assert_eq!(pose[0].rotation, baked.rotation);
        assert_eq!(pose[0].translation, baked.translation);
        assert_eq!(pose[0].scale, baked.scale);
    }

    /// `node_bind_pose` derives from nodes when the tag block is empty.
    #[test]
    fn node_bind_pose_derives_from_nodes_when_tag_block_empty() {
        let tx = RealPoint3d { x: 4.0, y: 5.0, z: 6.0 };
        let rot = RealQuaternion { i: 0.1, j: 0.2, k: 0.3, w: 0.9 };
        let rm = RenderModel {
            nodes: vec![node(tx, rot)],
            default_node_orientations: Vec::new(),
            ..Default::default()
        };
        let pose = rm.node_bind_pose();
        assert_eq!(pose.len(), 1);
        assert_eq!(pose[0].rotation, rot);
        assert_eq!(pose[0].translation, tx);
        assert_eq!(pose[0].scale, 1.0, "derived bind-pose scale defaults to 1.0");
    }

    /// `RealOrientation` import is used by the derived bind-pose path.
    #[test]
    fn bind_pose_returns_real_orientation() {
        let rm = RenderModel::default();
        let pose: Vec<RealOrientation> = rm.node_bind_pose();
        assert!(pose.is_empty());
    }
}
