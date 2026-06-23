//! Schema-faithful walker for the Halo 2 `shader` (shad) tag — mirrors
//! `definitions/halo2_mcc/shader.json` 1:1.
//!
//! The Halo 2 `shader` is a *template-driven* material: it references a
//! `shader_template` ([`H2Shader::template`]) and supplies a flat list of
//! named [`H2ShaderParameter`]s (bitmap / value / color, each optionally
//! animated) that fill the template's slots. Model resolvers consume that
//! `parameters` block to find the diffuse / bump / specular bitmaps and the
//! tint colors — so it is mirrored exhaustively here, by name.
//!
//! Below the parameters sit the cached, compiler-emitted `postprocess
//! definition` and `postprocess properties` blocks (the baked GPU state:
//! bitmaps, pixel/vertex constants, levels of detail, render/texture-stage
//! state, etc.). They are mirrored faithfully but are runtime caches rather
//! than authored data.
//!
//! Standalone: Halo 2 shaders do **not** share the Halo CE `ce_common`
//! lighting/specular structs, so every type here is H2-local.

use crate::api::{TagStruct, TagField};
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::math::{ArgbColor, RealArgbColor, RealRgbColor, RealVector3d, RgbColor};
use crate::typed_enums::{Enum, Flags};

use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SHAD: u32 = u32::from_be_bytes(*b"shad");

// =============================================================================
// tag_enum! — local copy of the canonical schema-enum/flags declarator.
// =============================================================================

/// Declare a schema-faithful tag enum/flags: a `#[repr]`'d unit enum with
/// `FromPrimitive`/`ToPrimitive` (wire <-> variant) and `EnumString`/
/// `IntoStaticStr`/`VariantArray` (name <-> variant) derives. For enums the
/// discriminant is the option index; for word/byte flags it is the bit index.
macro_rules! tag_enum {
    ($(#[$m:meta])* $name:ident : $repr:ty {
        $first:ident = $fidx:expr => $fs:literal
        $(, $var:ident = $idx:expr => $s:literal)* $(,)?
    }) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
                 num_derive::FromPrimitive, num_derive::ToPrimitive,
                 strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
        #[strum(ascii_case_insensitive)]
        #[repr($repr)]
        pub enum $name {
            #[default]
            #[strum(serialize = $fs)] $first = $fidx,
            $(#[strum(serialize = $s)] $var = $idx,)*
        }
    };
}

// =============================================================================
// Typed enums / flags
// =============================================================================

tag_enum!(/// `flags_flags` (word_flags) on `shader_block_struct`.
    H2ShaderFlags: u16 {
    Water = 0 => "water", SortFirst = 1 => "sort first", NoActiveCamo = 2 => "no active camo",
});

tag_enum!(/// `type_enum` — kind of a `global_shader_parameter_block` value.
    /// `switch` (index 3) is rarely authored but kept for fidelity.
    H2ShaderParameterType: i16 {
    Bitmap = 0 => "bitmap", Value = 1 => "value", Color = 2 => "color", Switch = 3 => "switch",
});

tag_enum!(/// `type_enum_2` — which template input a shader animation property drives.
    H2ShaderAnimationPropertyType: i16 {
    BitmapScaleUniform = 0 => "bitmap scale uniform",
    BitmapScaleX = 1 => "bitmap scale x", BitmapScaleY = 2 => "bitmap scale y",
    BitmapScaleZ = 3 => "bitmap scale z",
    BitmapTranslationX = 4 => "bitmap translation x", BitmapTranslationY = 5 => "bitmap translation y",
    BitmapTranslationZ = 6 => "bitmap translation z",
    BitmapRotationAngle = 7 => "bitmap rotation angle",
    BitmapRotationAxisX = 8 => "bitmap rotation axis x", BitmapRotationAxisY = 9 => "bitmap rotation axis y",
    BitmapRotationAxisZ = 10 => "bitmap rotation axis z",
    Value = 11 => "value", Color = 12 => "color", BitmapIndex = 13 => "bitmap index",
});

tag_enum!(/// `flags_flags_2` (byte_flags) on `mapping_function__v0`.
    H2MappingFunctionFlags: u8 {
    Range = 0 => "Range",
    Unused0 = 1 => "Unused 0", Unused1 = 2 => "Unused 1", Unused2 = 3 => "Unused 2",
    ColorBit0 = 4 => "Color Bit 0", ColorBit1 = 5 => "Color Bit 1",
    ColorBit2 = 6 => "Color Bit 2", ColorBit3 = 7 => "Color Bit 3",
});

tag_enum!(/// `type_enum_3` — `predicted_resource_block` resource kind.
    H2PredictedResourceType: i16 {
    Bitmap = 0 => "bitmap", Sound = 1 => "sound",
    RenderModelGeometry = 2 => "render model geometry", ClusterGeometry = 3 => "cluster geometry",
    ClusterInstancedGeometry = 4 => "cluster instanced geometry",
    LightmapGeometryObjectBuckets = 5 => "lightmap geometry object buckets",
    LightmapGeometryInstanceBuckets = 6 => "lightmap geometry instance buckets",
    LightmapClusterBitmaps = 7 => "lightmap cluster bitmaps",
    LightmapInstanceBitmaps = 8 => "lightmap instance bitmaps",
});

tag_enum!(/// `shader_lod_bias_enum`.
    H2ShaderLodBias: i16 {
    None = 0 => "none", FourTimesSize = 1 => "4 times size", TwoTimesSize = 2 => "2 times size",
    HalfSize = 3 => "1/2 size", QuarterSize = 4 => "1/4 size", Never = 5 => "never",
    Cinematic = 6 => "cinematic", Lowest = 7 => "lowest",
});

tag_enum!(/// `specular_type_enum`.
    H2SpecularType: i16 {
    None = 0 => "none", Default = 1 => "default", Dull = 2 => "dull", Shiny = 3 => "shiny",
});

tag_enum!(/// `lightmap_type_enum`.
    H2LightmapType: i16 {
    Diffuse = 0 => "diffuse", DefaultSpecular = 1 => "default specular",
    DullSpecular = 2 => "dull specular", ShinySpecular = 3 => "shiny specular",
});

// =============================================================================
// Schema structs (top of tree)
// =============================================================================

/// `shader_block_struct` — the whole Halo 2 `shader` (shad) tag.
#[derive(Debug, Clone, Default)]
pub struct H2Shader {
    /// `template` (tag_reference) — the `shader_template` (`stem`) this shader fills.
    pub template: TagRef,
    /// `material name` (string_id).
    pub material_name: String,
    /// `runtime properties` block — cached lightmap/diffuse properties.
    pub runtime_properties: Vec<H2ShaderProperties>,
    pub flags: Flags<H2ShaderFlags, u16>,
    /// `parameters` block — the authored, named template-slot fills
    /// (bitmaps / values / colors). Consumed by model resolvers.
    pub parameters: Vec<H2ShaderParameter>,
    /// `postprocess definition` block — compiler-baked GPU state.
    pub postprocess_definition: Vec<H2ShaderPostprocessDefinitionNew>,
    pub predicted_resources: Vec<H2PredictedResource>,
    pub light_response: TagRef,
    pub shader_lod_bias: Enum<H2ShaderLodBias, i16>,
    pub specular_type: Enum<H2SpecularType, i16>,
    pub lightmap_type: Enum<H2LightmapType, i16>,
    pub lightmap_specular_brightness: f32,
    pub lightmap_ambient_bias: f32,
    /// `postprocess properties` block (`long_block`) — bitmap group indices.
    pub postprocess_properties: Vec<H2Long>,
    /// `Added depth bias offset` (real) — schema name retained.
    pub added_depth_bias_offset: f32,
    /// `Added depth bias slope scale` (real) — schema name retained.
    pub added_depth_bias_slope_scale: f32,
}

/// `shader_properties_block` — cached runtime lightmap/diffuse properties.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderProperties {
    pub diffuse_map: TagRef,
    pub lightmap_emissive_map: TagRef,
    pub lightmap_emissive_color: RealRgbColor,
    pub lightmap_emissive_power: f32,
    pub lightmap_resolution_scale: f32,
    pub lightmap_half_life: f32,
    pub lightmap_diffuse_scale: f32,
    pub alphatest_map: TagRef,
    pub translucent_map: TagRef,
    pub lightmap_transparent_color: RealRgbColor,
    pub lightmap_transparent_alpha: f32,
    pub lightmap_foliage_scale: f32,
}

/// `global_shader_parameter_block` — one named template-slot fill.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderParameter {
    /// `name` (string_id) — matches a `shader_template` parameter name.
    pub name: String,
    /// `type` (short_enum) — schema field literally named `type`.
    pub type_value: Enum<H2ShaderParameterType, i16>,
    /// `bitmap` (tag_reference) — used when `type_value` is `Bitmap`.
    pub bitmap: TagRef,
    /// `const value` (real) — used when `type_value` is `Value`.
    pub const_value: f32,
    /// `const color` (real_rgb_color) — used when `type_value` is `Color`.
    pub const_color: RealRgbColor,
    pub animation_properties: Vec<H2ShaderAnimationProperty>,
}

/// `shader_animation_property_block` — drives one template input from a function.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderAnimationProperty {
    /// `type` (short_enum) — schema field literally named `type`.
    pub type_value: Enum<H2ShaderAnimationPropertyType, i16>,
    pub input_name: String,
    pub range_name: String,
    pub time_period: f32,
    // `animation function` is a `custom` field (editor-only); skipped.
    /// `function` (struct `mapping_function`) — the baked function bytes.
    pub function: H2MappingFunction,
}

/// `mapping_function` (v1) — the in-tag function carrier: a flat byte blob.
///
/// The schema versions this struct: v0 (`MAPP`, fully-expanded, the
/// `mapping_function__v0` layout below) and v1 (the compact `data` byte
/// block carrying the same function serialized). The on-disk struct is v1;
/// [`H2MappingFunctionV0`] mirrors the v0 schema for completeness.
#[derive(Debug, Clone, Default)]
pub struct H2MappingFunction {
    /// `data` (byte_block) — serialized function bytes.
    pub data: Vec<u8>,
}

/// `mapping_function__v0` — fully-expanded function schema (v0). Not the
/// on-disk layout (that's [`H2MappingFunction`] v1), kept for fidelity.
#[derive(Debug, Clone, Default)]
pub struct H2MappingFunctionV0 {
    /// `Function Type` (char_integer).
    pub function_type: i8,
    pub flags: Flags<H2MappingFunctionFlags, u8>,
    /// `Function 1` (char_integer).
    pub function_1: i8,
    /// `Function 2` (char_integer).
    pub function_2: i8,
    /// `Color 0` (rgb_color, packed).
    pub color_0: RgbColor,
    pub color_1: RgbColor,
    pub color_2: RgbColor,
    pub color_3: RgbColor,
    /// `Values` (real_block).
    pub values: Vec<f32>,
}

// =============================================================================
// Postprocess definition (compiler-baked GPU state) — "new" tree.
// =============================================================================

/// `shader_postprocess_definition_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessDefinitionNew {
    pub shader_template_index: i32,
    pub bitmaps: Vec<H2ShaderPostprocessBitmapNew>,
    /// `pixel constants` (pixel32_block) — packed ARGB constants.
    pub pixel_constants: Vec<ArgbColor>,
    pub vertex_constants: Vec<H2RealVector4d>,
    pub levels_of_detail: Vec<H2ShaderPostprocessLevelOfDetailNew>,
    /// `layers` (tag_block_index_block).
    pub layers: Vec<i16>,
    /// `passes` (tag_block_index_block).
    pub passes: Vec<i16>,
    pub implementations: Vec<H2ShaderPostprocessImplementationNew>,
    pub overlays: Vec<H2ShaderPostprocessOverlayNew>,
    pub overlay_references: Vec<H2ShaderPostprocessOverlayReferenceNew>,
    pub animated_parameters: Vec<H2ShaderPostprocessAnimatedParameterNew>,
    pub animated_parameter_references: Vec<H2ShaderPostprocessAnimatedParameterReferenceNew>,
    pub bitmap_properties: Vec<H2ShaderPostprocessBitmapProperty>,
    pub color_properties: Vec<H2ShaderPostprocessColorProperty>,
    pub value_properties: Vec<H2ShaderPostprocessValueProperty>,
    pub old_levels_of_detail: Vec<H2ShaderPostprocessLevelOfDetail>,
}

/// `shader_postprocess_bitmap_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessBitmapNew {
    pub bitmap_group: i32,
    pub bitmap_index: i32,
    pub log_bitmap_dimension: f32,
}

/// `real_vector4d_block`.
#[derive(Debug, Clone, Default)]
pub struct H2RealVector4d {
    pub vector3: RealVector3d,
    pub w: f32,
}

/// `shader_postprocess_level_of_detail_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessLevelOfDetailNew {
    pub available_layer_flags: i32,
    /// `layers` (struct `tag_block_index_struct`).
    pub layers: i16,
}

/// `shader_postprocess_implementation_new_block` — all fields are
/// `tag_block_index_struct` (a single packed `short_integer`).
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessImplementationNew {
    pub bitmap_transforms: i16,
    pub render_states: i16,
    pub texture_states: i16,
    pub pixel_constants: i16,
    pub vertex_constants: i16,
}

/// `shader_postprocess_overlay_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessOverlayNew {
    pub input_name: String,
    pub range_name: String,
    pub time_period_in_seconds: f32,
    /// `function` (struct `scalar_function_struct`).
    pub function: H2ScalarFunction,
}

/// `scalar_function_struct` (`SCFN`) — a `mapping_function` wrapper.
#[derive(Debug, Clone, Default)]
pub struct H2ScalarFunction {
    pub function: H2MappingFunction,
}

/// `color_function_struct` (`CLFN`) — a `mapping_function` wrapper.
#[derive(Debug, Clone, Default)]
pub struct H2ColorFunction {
    pub function: H2MappingFunction,
}

/// `shader_postprocess_overlay_reference_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessOverlayReferenceNew {
    pub overlay_index: i16,
    pub transform_index: i16,
}

/// `shader_postprocess_animated_parameter_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessAnimatedParameterNew {
    /// `overlay references` (struct `tag_block_index_struct`).
    pub overlay_references: i16,
}

/// `shader_postprocess_animated_parameter_reference_new_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessAnimatedParameterReferenceNew {
    pub parameter_index: i8,
}

/// `shader_postprocess_bitmap_property_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessBitmapProperty {
    pub bitmap_index: i16,
    pub animated_parameter_index: i16,
}

/// `shader_postprocess_color_property_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessColorProperty {
    pub color: RealRgbColor,
}

/// `shader_postprocess_value_property_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessValueProperty {
    pub value: f32,
}

// =============================================================================
// Postprocess definition (old, baked-per-LOD GPU state) tree.
// =============================================================================

/// `shader_postprocess_level_of_detail_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessLevelOfDetail {
    pub projected_height_percentage: f32,
    pub available_layers: i32,
    pub layers: Vec<H2ShaderPostprocessLayer>,
    pub passes: Vec<H2ShaderPostprocessPass>,
    pub implementations: Vec<H2ShaderPostprocessImplementation>,
    pub bitmaps: Vec<H2ShaderPostprocessBitmap>,
    pub bitmap_transforms: Vec<H2ShaderPostprocessBitmapTransform>,
    pub values: Vec<H2ShaderPostprocessValue>,
    pub colors: Vec<H2ShaderPostprocessColor>,
    pub bitmap_transform_overlays: Vec<H2ShaderPostprocessBitmapTransformOverlay>,
    pub value_overlays: Vec<H2ShaderPostprocessValueOverlay>,
    pub color_overlays: Vec<H2ShaderPostprocessColorOverlay>,
    pub vertex_shader_constants: Vec<H2ShaderPostprocessVertexShaderConstant>,
    /// `GPU state` (struct `shader_gpu_state_struct`).
    pub gpu_state: H2ShaderGpuState,
}

/// `shader_postprocess_layer_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessLayer {
    /// `passes` (struct `tag_block_index_struct`).
    pub passes: i16,
}

/// `shader_postprocess_pass_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessPass {
    pub shader_pass: TagRef,
    /// `implementations` (struct `tag_block_index_struct`).
    pub implementations: i16,
}

/// `shader_postprocess_implementation_block` — all fields are nested structs.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessImplementation {
    pub gpu_constant_state: H2ShaderGpuStateReference,
    pub gpu_volatile_state: H2ShaderGpuStateReference,
    /// `bitmap parameters` (struct `tag_block_index_struct`).
    pub bitmap_parameters: i16,
    pub bitmap_transforms: i16,
    pub value_parameters: i16,
    pub color_parameters: i16,
    pub bitmap_transform_overlays: i16,
    pub value_overlays: i16,
    pub color_overlays: i16,
    pub vertex_shader_constants: i16,
}

/// `shader_gpu_state_reference_struct` (`GPUR`) — packed `tag_block_index`es.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderGpuStateReference {
    pub render_states: i16,
    pub texture_stage_states: i16,
    pub render_state_parameters: i16,
    pub texture_stage_parameters: i16,
    pub textures: i16,
    /// `Vn constants` (struct `tag_block_index_struct`).
    pub vn_constants: i16,
    /// `Cn constants` (struct `tag_block_index_struct`).
    pub cn_constants: i16,
}

/// `shader_postprocess_bitmap_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessBitmap {
    pub parameter_index: i8,
    pub flags: i8,
    pub bitmap_group_index: i32,
    pub log_bitmap_dimension: f32,
}

/// `shader_postprocess_bitmap_transform_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessBitmapTransform {
    pub parameter_index: i8,
    pub bitmap_transform_index: i8,
    pub value: f32,
}

/// `shader_postprocess_value_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessValue {
    pub parameter_index: i8,
    pub value: f32,
}

/// `shader_postprocess_color_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessColor {
    pub parameter_index: i8,
    pub color: RealRgbColor,
}

/// `shader_postprocess_bitmap_transform_overlay_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessBitmapTransformOverlay {
    pub parameter_index: i8,
    pub transform_index: i8,
    pub animation_property_type: i8,
    pub input_name: String,
    pub range_name: String,
    pub time_period_in_seconds: f32,
    /// `function` (struct `scalar_function_struct`).
    pub function: H2ScalarFunction,
}

/// `shader_postprocess_value_overlay_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessValueOverlay {
    pub parameter_index: i8,
    pub input_name: String,
    pub range_name: String,
    pub time_period_in_seconds: f32,
    /// `function` (struct `color_function_struct`).
    pub function: H2ColorFunction,
}

/// `shader_postprocess_color_overlay_block`.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessColorOverlay {
    pub parameter_index: i8,
    pub input_name: String,
    pub range_name: String,
    pub time_period_in_seconds: f32,
    /// `function` (struct `color_function_struct`).
    pub function: H2ColorFunction,
}

/// `shader_postprocess_vertex_shader_constant_block` — 4 `data` reals (schema
/// names them all `data`; surfaced as an array).
#[derive(Debug, Clone, Default)]
pub struct H2ShaderPostprocessVertexShaderConstant {
    pub register_index: i8,
    pub register_bank: i8,
    /// `data` x4 (the schema lists four `data` reals in order).
    pub data: [f32; 4],
}

/// `shader_gpu_state_struct` (`GPUS`) — the baked Direct3D state tables.
#[derive(Debug, Clone, Default)]
pub struct H2ShaderGpuState {
    pub render_states: Vec<H2RenderState>,
    pub texture_stage_states: Vec<H2TextureStageState>,
    pub render_state_parameters: Vec<H2RenderStateParameter>,
    pub texture_stage_parameters: Vec<H2TextureStageStateParameter>,
    pub textures: Vec<H2Texture>,
    /// `Vn constants` (vertex_shader_constant_block).
    pub vn_constants: Vec<H2VertexShaderConstant>,
    /// `Cn constants` (vertex_shader_constant_block).
    pub cn_constants: Vec<H2VertexShaderConstant>,
}

/// `render_state_block`.
#[derive(Debug, Clone, Default)]
pub struct H2RenderState {
    pub state_index: i8,
    pub state_value: i32,
}

/// `texture_stage_state_block`.
#[derive(Debug, Clone, Default)]
pub struct H2TextureStageState {
    pub state_index: i8,
    pub stage_index: i8,
    pub state_value: i32,
}

/// `render_state_parameter_block`.
#[derive(Debug, Clone, Default)]
pub struct H2RenderStateParameter {
    pub parameter_index: i8,
    pub parameter_type: i8,
    pub state_index: i8,
}

/// `texture_stage_state_parameter_block`.
#[derive(Debug, Clone, Default)]
pub struct H2TextureStageStateParameter {
    pub parameter_index: i8,
    pub parameter_type: i8,
    pub state_index: i8,
    pub stage_index: i8,
}

/// `texture_block`.
#[derive(Debug, Clone, Default)]
pub struct H2Texture {
    pub stage_index: i8,
    pub parameter_index: i8,
    pub global_texture_index: i8,
    pub flags: i8,
}

/// `vertex_shader_constant_block`.
#[derive(Debug, Clone, Default)]
pub struct H2VertexShaderConstant {
    pub register_index: i8,
    pub parameter_index: i8,
    pub destination_mask: i8,
    pub scale_by_texture_stage: i8,
}

/// `predicted_resource_block`.
#[derive(Debug, Clone, Default)]
pub struct H2PredictedResource {
    /// `type` (short_enum) — schema field literally named `type`.
    pub type_value: Enum<H2PredictedResourceType, i16>,
    pub resource_index: i16,
    pub tag_index: i32,
}

/// `long_block` — the `postprocess properties` element (a bitmap group index).
#[derive(Debug, Clone, Default)]
pub struct H2Long {
    pub bitmap_group_index: i32,
}

// =============================================================================
// Walker
// =============================================================================

impl H2Shader {
    /// Walk a parsed Halo 2 `shader` (shad) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SHAD {
            return Err(ShaderError::WrongGroup { expected: GROUP_SHAD, found });
        }
        let root = tag.root();
        Ok(Self {
            template: read_tag_ref(&root, "template"),
            material_name: root.read_string_id("material name").unwrap_or_default(),
            runtime_properties: read_block(&root, "runtime properties", read_properties),
            flags: root.try_read_flags("flags").unwrap_or_default(),
            parameters: read_block(&root, "parameters", read_parameter),
            postprocess_definition: read_block(&root, "postprocess definition", read_pp_def),
            predicted_resources: read_block(&root, "predicted resources", read_predicted),
            light_response: read_tag_ref(&root, "light response"),
            shader_lod_bias: root.try_read_enum("shader LOD bias").unwrap_or_default(),
            specular_type: root.try_read_enum("specular type").unwrap_or_default(),
            lightmap_type: root.try_read_enum("lightmap type").unwrap_or_default(),
            lightmap_specular_brightness: real(&root, "lightmap specular brightness"),
            lightmap_ambient_bias: real(&root, "lightmap ambient bias"),
            postprocess_properties: read_block(&root, "postprocess properties", |s| H2Long {
                bitmap_group_index: int(s, "bitmap group index") as i32,
            }),
            added_depth_bias_offset: real(&root, "Added depth bias offset"),
            added_depth_bias_slope_scale: real(&root, "Added depth bias slope scale"),
        })
    }
}

// -----------------------------------------------------------------------------
// Block / scalar helpers
// -----------------------------------------------------------------------------

/// Iterate a `block` field, mapping each element with `f`. Empty when absent.
fn read_block<T>(parent: &TagStruct<'_>, name: &str, f: impl Fn(&TagStruct<'_>) -> T) -> Vec<T> {
    let Some(block) = parent.field(name).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    (0..block.len())
        .filter_map(|i| block.element(i))
        .map(|e| f(&e))
        .collect()
}

/// Read a `real`, defaulting to 0.
fn real(s: &TagStruct<'_>, name: &str) -> f32 {
    s.read_real(name).unwrap_or(0.0)
}

/// Read any integer (defaulting to 0) as `i128`; callers narrow with `as`.
fn int(s: &TagStruct<'_>, name: &str) -> i128 {
    s.read_int_any(name).unwrap_or(0)
}

/// Read a `string_id`, defaulting to empty.
fn sid(s: &TagStruct<'_>, name: &str) -> String {
    s.read_string_id(name).unwrap_or_default()
}

/// Read a `real_vector_3d` field (defaulting to zero).
fn vector3d(s: &TagStruct<'_>, name: &str) -> RealVector3d {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealVector3d(v)) => v,
        _ => RealVector3d::default(),
    }
}

/// Read a packed `argb_color` (pixel32) field (defaulting to zero).
fn argb(s: &TagStruct<'_>, name: &str) -> ArgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::ArgbColor(c)) => c,
        _ => ArgbColor::default(),
    }
}

/// Read a packed `rgb_color` field (defaulting to zero). Used by the v0
/// `mapping_function` view (not the on-disk v1 layout).
#[allow(dead_code)]
fn rgb_packed(s: &TagStruct<'_>, name: &str) -> RgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RgbColor(c)) => c,
        _ => RgbColor::default(),
    }
}

/// Read a `real_argb_color`. NOTE: there is no dedicated reader; pulled out of
/// the raw field value (`read_rgb` would drop alpha), defaulting to zero.
#[allow(dead_code)]
fn real_argb(s: &TagStruct<'_>, name: &str) -> RealArgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealArgbColor(c)) => c,
        _ => RealArgbColor::default(),
    }
}

/// Read a `byte_block` (`mapping_function`'s `data`) into a `Vec<u8>`.
fn read_byte_block(parent: &TagStruct<'_>, name: &str) -> Vec<u8> {
    read_block(parent, name, |e| int(e, "Value") as u8)
}

/// Read the `function` (`mapping_function`, v1) inside a struct field.
fn read_mapping_function(parent: &TagStruct<'_>, field: &str) -> H2MappingFunction {
    let Some(s) = parent.descend(field) else { return Default::default() };
    H2MappingFunction { data: read_byte_block(&s, "data") }
}

/// Read a `scalar_function_struct` (its inner `function` mapping_function).
fn read_scalar_function(parent: &TagStruct<'_>, field: &str) -> H2ScalarFunction {
    let Some(s) = parent.descend(field) else { return Default::default() };
    H2ScalarFunction { function: read_mapping_function(&s, "function") }
}

/// Read a `color_function_struct` (its inner `function` mapping_function).
fn read_color_function(parent: &TagStruct<'_>, field: &str) -> H2ColorFunction {
    let Some(s) = parent.descend(field) else { return Default::default() };
    H2ColorFunction { function: read_mapping_function(&s, "function") }
}

/// Read a `tag_block_index_struct` (a single packed `short_integer`).
fn block_index(parent: &TagStruct<'_>, field: &str) -> i16 {
    parent
        .descend(field)
        .map(|s| int(&s, "block index data") as i16)
        .unwrap_or(0)
}

// -----------------------------------------------------------------------------
// Element readers
// -----------------------------------------------------------------------------

fn read_properties(s: &TagStruct<'_>) -> H2ShaderProperties {
    H2ShaderProperties {
        diffuse_map: read_tag_ref(s, "diffuse map"),
        lightmap_emissive_map: read_tag_ref(s, "lightmap emissive map"),
        lightmap_emissive_color: s.read_rgb("lightmap emissive color"),
        lightmap_emissive_power: real(s, "lightmap emissive power"),
        lightmap_resolution_scale: real(s, "lightmap resolution scale"),
        lightmap_half_life: real(s, "lightmap half life"),
        lightmap_diffuse_scale: real(s, "lightmap diffuse scale"),
        alphatest_map: read_tag_ref(s, "alphatest map"),
        translucent_map: read_tag_ref(s, "translucent map"),
        lightmap_transparent_color: s.read_rgb("lightmap transparent color"),
        lightmap_transparent_alpha: real(s, "lightmap transparent alpha"),
        lightmap_foliage_scale: real(s, "lightmap foliage scale"),
    }
}

fn read_parameter(s: &TagStruct<'_>) -> H2ShaderParameter {
    H2ShaderParameter {
        name: sid(s, "name"),
        type_value: s.try_read_enum("type").unwrap_or_default(),
        bitmap: read_tag_ref(s, "bitmap"),
        const_value: real(s, "const value"),
        const_color: s.read_rgb("const color"),
        animation_properties: read_block(s, "animation properties", read_animation_property),
    }
}

fn read_animation_property(s: &TagStruct<'_>) -> H2ShaderAnimationProperty {
    H2ShaderAnimationProperty {
        type_value: s.try_read_enum("type").unwrap_or_default(),
        input_name: sid(s, "input name"),
        range_name: sid(s, "range name"),
        time_period: real(s, "time period"),
        function: read_mapping_function(s, "function"),
    }
}

fn read_predicted(s: &TagStruct<'_>) -> H2PredictedResource {
    H2PredictedResource {
        type_value: s.try_read_enum("type").unwrap_or_default(),
        resource_index: int(s, "resource index") as i16,
        tag_index: int(s, "tag index") as i32,
    }
}

// --- postprocess definition (new tree) ---

fn read_pp_def(s: &TagStruct<'_>) -> H2ShaderPostprocessDefinitionNew {
    H2ShaderPostprocessDefinitionNew {
        shader_template_index: int(s, "shader template index") as i32,
        bitmaps: read_block(s, "bitmaps", |b| H2ShaderPostprocessBitmapNew {
            bitmap_group: int(b, "bitmap group") as i32,
            bitmap_index: int(b, "bitmap index") as i32,
            log_bitmap_dimension: real(b, "log bitmap dimension"),
        }),
        pixel_constants: read_block(s, "pixel constants", |b| argb(b, "color")),
        vertex_constants: read_block(s, "vertex constants", |b| H2RealVector4d {
            vector3: vector3d(b, "vector3"),
            w: real(b, "w"),
        }),
        levels_of_detail: read_block(s, "levels of detail", |b| {
            H2ShaderPostprocessLevelOfDetailNew {
                available_layer_flags: int(b, "available layer flags") as i32,
                layers: block_index(b, "layers"),
            }
        }),
        layers: read_block(s, "layers", |b| block_index(b, "indices")),
        passes: read_block(s, "passes", |b| block_index(b, "indices")),
        implementations: read_block(s, "implementations", |b| {
            H2ShaderPostprocessImplementationNew {
                bitmap_transforms: block_index(b, "bitmap transforms"),
                render_states: block_index(b, "render states"),
                texture_states: block_index(b, "texture states"),
                pixel_constants: block_index(b, "pixel constants"),
                vertex_constants: block_index(b, "vertex constants"),
            }
        }),
        overlays: read_block(s, "overlays", |b| H2ShaderPostprocessOverlayNew {
            input_name: sid(b, "input name"),
            range_name: sid(b, "range name"),
            time_period_in_seconds: real(b, "time period in seconds"),
            function: read_scalar_function(b, "function"),
        }),
        overlay_references: read_block(s, "overlay references", |b| {
            H2ShaderPostprocessOverlayReferenceNew {
                overlay_index: int(b, "overlay index") as i16,
                transform_index: int(b, "transform index") as i16,
            }
        }),
        animated_parameters: read_block(s, "animated parameters", |b| {
            H2ShaderPostprocessAnimatedParameterNew {
                overlay_references: block_index(b, "overlay references"),
            }
        }),
        animated_parameter_references: read_block(s, "animated parameter references", |b| {
            H2ShaderPostprocessAnimatedParameterReferenceNew {
                parameter_index: int(b, "parameter index") as i8,
            }
        }),
        bitmap_properties: read_block(s, "bitmap properties", |b| {
            H2ShaderPostprocessBitmapProperty {
                bitmap_index: int(b, "bitmap index") as i16,
                animated_parameter_index: int(b, "animated parameter index") as i16,
            }
        }),
        color_properties: read_block(s, "color properties", |b| H2ShaderPostprocessColorProperty {
            color: b.read_rgb("color"),
        }),
        value_properties: read_block(s, "value properties", |b| H2ShaderPostprocessValueProperty {
            value: real(b, "value"),
        }),
        old_levels_of_detail: read_block(s, "old levels of detail", read_pp_lod),
    }
}

// --- postprocess definition (old, per-LOD tree) ---

fn read_pp_lod(s: &TagStruct<'_>) -> H2ShaderPostprocessLevelOfDetail {
    H2ShaderPostprocessLevelOfDetail {
        projected_height_percentage: real(s, "projected height percentage"),
        available_layers: int(s, "available layers") as i32,
        layers: read_block(s, "layers", |b| H2ShaderPostprocessLayer {
            passes: block_index(b, "passes"),
        }),
        passes: read_block(s, "passes", |b| H2ShaderPostprocessPass {
            shader_pass: read_tag_ref(b, "shader pass"),
            implementations: block_index(b, "implementations"),
        }),
        implementations: read_block(s, "implementations", read_pp_impl),
        bitmaps: read_block(s, "bitmaps", |b| H2ShaderPostprocessBitmap {
            parameter_index: int(b, "parameter index") as i8,
            flags: int(b, "flags") as i8,
            bitmap_group_index: int(b, "bitmap group index") as i32,
            log_bitmap_dimension: real(b, "log bitmap dimension"),
        }),
        bitmap_transforms: read_block(s, "bitmap transforms", |b| {
            H2ShaderPostprocessBitmapTransform {
                parameter_index: int(b, "parameter index") as i8,
                bitmap_transform_index: int(b, "bitmap transform index") as i8,
                value: real(b, "value"),
            }
        }),
        values: read_block(s, "values", |b| H2ShaderPostprocessValue {
            parameter_index: int(b, "parameter index") as i8,
            value: real(b, "value"),
        }),
        colors: read_block(s, "colors", |b| H2ShaderPostprocessColor {
            parameter_index: int(b, "parameter index") as i8,
            color: b.read_rgb("color"),
        }),
        bitmap_transform_overlays: read_block(s, "bitmap transform overlays", |b| {
            H2ShaderPostprocessBitmapTransformOverlay {
                parameter_index: int(b, "parameter index") as i8,
                transform_index: int(b, "transform index") as i8,
                animation_property_type: int(b, "animation property type") as i8,
                input_name: sid(b, "input name"),
                range_name: sid(b, "range name"),
                time_period_in_seconds: real(b, "time period in seconds"),
                function: read_scalar_function(b, "function"),
            }
        }),
        value_overlays: read_block(s, "value overlays", |b| H2ShaderPostprocessValueOverlay {
            parameter_index: int(b, "parameter index") as i8,
            input_name: sid(b, "input name"),
            range_name: sid(b, "range name"),
            time_period_in_seconds: real(b, "time period in seconds"),
            function: read_color_function(b, "function"),
        }),
        color_overlays: read_block(s, "color overlays", |b| H2ShaderPostprocessColorOverlay {
            parameter_index: int(b, "parameter index") as i8,
            input_name: sid(b, "input name"),
            range_name: sid(b, "range name"),
            time_period_in_seconds: real(b, "time period in seconds"),
            function: read_color_function(b, "function"),
        }),
        vertex_shader_constants: read_block(s, "vertex shader constants", read_pp_vsc),
        gpu_state: read_gpu_state(s, "GPU state"),
    }
}

fn read_pp_impl(s: &TagStruct<'_>) -> H2ShaderPostprocessImplementation {
    H2ShaderPostprocessImplementation {
        gpu_constant_state: read_gpu_state_ref(s, "GPU constant state"),
        gpu_volatile_state: read_gpu_state_ref(s, "GPU volatile state"),
        bitmap_parameters: block_index(s, "bitmap parameters"),
        bitmap_transforms: block_index(s, "bitmap transforms"),
        value_parameters: block_index(s, "value parameters"),
        color_parameters: block_index(s, "color parameters"),
        bitmap_transform_overlays: block_index(s, "bitmap transform overlays"),
        value_overlays: block_index(s, "value overlays"),
        color_overlays: block_index(s, "color overlays"),
        vertex_shader_constants: block_index(s, "vertex shader constants"),
    }
}

fn read_gpu_state_ref(parent: &TagStruct<'_>, field: &str) -> H2ShaderGpuStateReference {
    let Some(s) = parent.descend(field) else { return Default::default() };
    H2ShaderGpuStateReference {
        render_states: block_index(&s, "render states"),
        texture_stage_states: block_index(&s, "texture stage states"),
        render_state_parameters: block_index(&s, "render state parameters"),
        texture_stage_parameters: block_index(&s, "texture stage parameters"),
        textures: block_index(&s, "textures"),
        vn_constants: block_index(&s, "Vn constants"),
        cn_constants: block_index(&s, "Cn constants"),
    }
}

fn read_pp_vsc(s: &TagStruct<'_>) -> H2ShaderPostprocessVertexShaderConstant {
    // The schema lists four reals all literally named `data`; the reader keys
    // by name and would resolve them to the first match, so pull the packed
    // group out of the raw struct in field order via the four `data` fields.
    let data = read_repeated_reals(s, "data", 4);
    H2ShaderPostprocessVertexShaderConstant {
        register_index: int(s, "register index") as i8,
        register_bank: int(s, "register bank") as i8,
        data: [data[0], data[1], data[2], data[3]],
    }
}

/// Read up to `count` reals from same-named consecutive fields. Name-keyed
/// readers collapse duplicate names, so walk the struct's fields in order.
fn read_repeated_reals(s: &TagStruct<'_>, name: &str, count: usize) -> Vec<f32> {
    let mut out: Vec<f32> = s
        .fields()
        .filter(|f: &TagField<'_>| f.name() == name)
        .filter_map(|f| match f.value() {
            Some(TagFieldData::Real(v)) => Some(v),
            _ => None,
        })
        .take(count)
        .collect();
    out.resize(count, 0.0);
    out
}

fn read_gpu_state(parent: &TagStruct<'_>, field: &str) -> H2ShaderGpuState {
    let Some(s) = parent.descend(field) else { return Default::default() };
    H2ShaderGpuState {
        render_states: read_block(&s, "render states", |b| H2RenderState {
            state_index: int(b, "state index") as i8,
            state_value: int(b, "state value") as i32,
        }),
        texture_stage_states: read_block(&s, "texture stage states", |b| H2TextureStageState {
            state_index: int(b, "state index") as i8,
            stage_index: int(b, "stage index") as i8,
            state_value: int(b, "state value") as i32,
        }),
        render_state_parameters: read_block(&s, "render state parameters", |b| {
            H2RenderStateParameter {
                parameter_index: int(b, "parameter index") as i8,
                parameter_type: int(b, "parameter type") as i8,
                state_index: int(b, "state index") as i8,
            }
        }),
        texture_stage_parameters: read_block(&s, "texture stage parameters", |b| {
            H2TextureStageStateParameter {
                parameter_index: int(b, "parameter index") as i8,
                parameter_type: int(b, "parameter type") as i8,
                state_index: int(b, "state index") as i8,
                stage_index: int(b, "stage index") as i8,
            }
        }),
        textures: read_block(&s, "textures", |b| H2Texture {
            stage_index: int(b, "stage index") as i8,
            parameter_index: int(b, "parameter index") as i8,
            global_texture_index: int(b, "global texture index") as i8,
            flags: int(b, "flags") as i8,
        }),
        vn_constants: read_block(&s, "Vn constants", read_vsc),
        cn_constants: read_block(&s, "Cn constants", read_vsc),
    }
}

fn read_vsc(b: &TagStruct<'_>) -> H2VertexShaderConstant {
    H2VertexShaderConstant {
        register_index: int(b, "register index") as i8,
        parameter_index: int(b, "parameter index") as i8,
        destination_mask: int(b, "destination mask") as i8,
        scale_by_texture_stage: int(b, "scale by texture stage") as i8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real H2 shader and dump its parameters. Skips silently
    /// when the tag isn't present.
    #[test]
    fn walks_h2_shader() {
        let path = "/Users/camden/Halo/halo2_mcc/tags/scenarios/objects/\
                    multi/shaders/test.shader";
        let Ok(tag) = TagFile::read(path) else { return };
        let sh = H2Shader::from_tag(&tag).expect("parse shad");
        eprintln!("template={:?} material={:?}", sh.template.path, sh.material_name);
        for p in &sh.parameters {
            eprintln!("  param {:?} type={:?} bitmap={:?} value={} color={:?}",
                p.name, p.type_value, p.bitmap.path, p.const_value, p.const_color);
        }
    }
}
