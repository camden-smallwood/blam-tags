//! Schema-faithful walker for the Halo 4 `material` (`mat `) tag —
//! mirrors `definitions/halo4_mcc/material.json` 1:1.
//!
//! The H4 `material` is the analogue of the Gen3 `render_method`: a tag
//! reference to a `material_shader` (`mats`) plus a flat list of
//! **material parameters** (the authored bitmap/real/color/bool/int
//! values that feed the shader's registers) and a baked **postprocess
//! definition** (the runtime-ready texture/constant/function tables the
//! engine actually binds at draw time).
//!
//! Type names are PascalCase with the `_struct`/`_block` suffix stripped;
//! field names are snake_case. The public root type is [`Material`], also
//! re-exported as `H4Material` to avoid clashing with
//! [`crate::render_model::Material`]. A field literally named `type` in the
//! schema is mirrored as `type_value`.
//!
//! STANDALONE — does not depend on `ce_common`. The `tag_enum!` macro is
//! copied in verbatim (matching `ce_common`'s idiom).

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::math::{RealArgbColor, RealVector3d};
use crate::typed_enums::{Enum, Flags};

use super::{read_tag_ref, ShaderError, TagRef};

/// FOURCC of the H4 `material` group — note the trailing space.
const GROUP_MAT: u32 = u32::from_be_bytes(*b"mat ");

// =============================================================================
// tag_enum! — schema-faithful typed enum/flags (copied from ce_common).
// =============================================================================

/// Declare a schema-faithful tag enum/flags: a `#[repr]`'d unit enum with
/// `FromPrimitive`/`ToPrimitive` (wire <-> variant) and `EnumString`/
/// `IntoStaticStr`/`VariantArray` (name <-> variant) derives.
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

tag_enum!(/// `material_shader_parameter_type_enum` (long_enum) — which of the
    /// per-parameter value fields (`color`/`real`/`vector`/`int/bool`/
    /// `bitmap`) is authoritative for a given parameter.
    H4MaterialParameterType: i32 {
    Bitmap = 0 => "bitmap",
    Real = 1 => "real",
    Int = 2 => "int",
    Bool = 3 => "bool",
    Color = 4 => "color",
});

tag_enum!(/// `material_animated_parameter_type_enum` (long_enum) — what a
    /// function parameter animates.
    MaterialAnimatedParameterType: i32 {
    Value = 0 => "value",
    Color = 1 => "color",
    ScaleUniform = 2 => "scale uniform",
    ScaleU = 3 => "scale u",
    ScaleV = 4 => "scale v",
    OffsetU = 5 => "offset u",
    OffsetV = 6 => "offset v",
    FrameIndex = 7 => "frame index",
    Alpha = 8 => "alpha",
});

tag_enum!(/// `materialFunctionOutputModEnum` (char_enum).
    MaterialFunctionOutputMod: i8 {
    None = 0 => " ",
    Add = 1 => "Add",
    Multiply = 2 => "Multiply",
});

tag_enum!(/// `alpha_blend_mode_enum` (char_enum) — root + postprocess blend mode.
    AlphaBlendMode: i8 {
    Opaque = 0 => "opaque",
    Additive = 1 => "additive",
    Multiply = 2 => "multiply",
    AlphaBlend = 3 => "alpha_blend",
    DoubleMultiply = 4 => "double_multiply",
    PreMultipliedAlpha = 5 => "pre_multiplied_alpha",
    Maximum = 6 => "maximum",
    MultiplyAdd = 7 => "multiply_add",
    AddSrcTimesDstalpha = 8 => "add_src_times_dstalpha",
    AddSrcTimesSrcalpha = 9 => "add_src_times_srcalpha",
    InvAlphaBlend = 10 => "inv_alpha_blend",
    MotionBlurStatic = 11 => "motion_blur_static",
    MotionBlurInhibit = 12 => "motion_blur_inhibit",
    ApplyShadowIntoShadowMask = 13 => "apply_shadow_into_shadow_mask",
    AlphaBlendConstant = 14 => "alpha_blend_constant",
    OverdrawApply = 15 => "overdraw_apply",
    WetScreenEffect = 16 => "wet_screen_effect",
    Minimum = 17 => "minimum",
    Revsubtract = 18 => "revsubtract",
    ForgeLightmap = 19 => "forge_lightmap",
    ForgeLightmapInv = 20 => "forge_lightmap_inv",
    ReplaceAllChannels = 21 => "replace_all_channels",
    AlphaBlendMax = 22 => "alpha_blend_max",
    OpaqueAlphaBlend = 23 => "opaque_alpha_blend",
    AlphaBlendAdditiveTransparent = 24 => "alpha_blend_additive_transparent",
});

tag_enum!(/// `layerBlendModeEnum` (char_enum).
    LayerBlendMode: i8 {
    None = 0 => "none",
    Blended = 1 => "blended",
    Layered = 2 => "layered",
});

tag_enum!(/// `global_sort_layer_enum_defintion` (char_enum). (`sort layer*`)
    GlobalSortLayer: i8 {
    Invalid = 0 => "invalid",
    PrePass = 1 => "pre-pass",
    Normal = 2 => "normal",
    PostPass = 3 => "post-pass",
});

tag_enum!(/// `MaterialTransparentShadowPolicyEnum` (char_enum).
    MaterialTransparentShadowPolicy: i8 {
    None = 0 => "none",
    RenderAsDecalCheap = 1 => "render as decal (cheap)",
    RenderWithMaterialExpensive = 2 => "render with material (expensive)",
});

tag_enum!(/// `materialFlags` (byte_flags) — `flags!` on the root.
    MaterialFlags: u8 {
    ConvertedFromShader = 0 => "converted from shader",
    DecalPostLighting = 1 => "decal post lighting",
});

tag_enum!(/// `materialRenderFlags` (byte_flags) — `render flags!` on the root.
    MaterialRenderFlags: u8 {
    ResolveScreenBeforeRendering = 0 => "resolve screen before rendering",
});

tag_enum!(/// `materialPostprocessFlags` (word_flags) on the postprocess block.
    MaterialPostprocessFlags: u16 {
    WireframeOutline = 0 => "wireframe outline",
    ForceSinglePass = 1 => "force single pass",
    HasPixelConstantFunctions = 2 => "has pixel constant functions",
    HasVertexConstantFunctions = 3 => "has vertex constant functions",
    HasTextureTransformFunctions = 4 => "has texture transform functions",
    HasTextureFrameFunctions = 5 => "has texture frame functions",
    ResolveScreenBeforeRendering = 6 => "resolve screen before rendering",
    DisableAtmosphereFog = 7 => "disable atmosphere fog",
    UsesDepthCamera = 8 => "uses depth camera",
    MaterialIsVariable = 9 => "material is variable",
});

// =============================================================================
// Schema structs (mirror material.json 1:1)
// =============================================================================

/// `material_shader_function_parameter_block` — an animated/function-driven
/// parameter. The nested `function` struct (`mapping_function`) is a
/// `function_definition_data` blob (custom-mapped function curve); only its
/// presence/animation metadata is mirrored here, not the curve bytes.
#[derive(Debug, Clone, Default)]
pub struct H4MaterialFunctionParameter {
    /// Schema field name: `type^`.
    pub type_value: Enum<MaterialAnimatedParameterType, i32>,
    pub input_name: String,
    pub range_name: String,
    /// Schema field name: `Output Modifier!`.
    pub output_modifier: Enum<MaterialFunctionOutputMod, i8>,
    /// Schema field name: `Output Modifier Input!`.
    pub output_modifier_input: String,
    /// Schema field name: `time period:seconds`.
    pub time_period_seconds: f32,
}

/// `material_shader_parameter_block` — one authored material parameter.
///
/// `parameter_type` selects which of the value fields below the engine
/// reads: `Bitmap` → [`bitmap`]/[`bitmap_path`] + the sampler fields,
/// `Color` → [`color`], `Real` → [`real`], `Bool`/`Int` → [`int_bool`].
/// The [`vector`] field carries packed vector params (e.g. tint + scalar).
///
/// [`bitmap`]: Self::bitmap
/// [`bitmap_path`]: Self::bitmap_path
/// [`color`]: Self::color
/// [`real`]: Self::real
/// [`int_bool`]: Self::int_bool
/// [`vector`]: Self::vector
#[derive(Debug, Clone, Default)]
pub struct H4MaterialParameter {
    /// Schema field name: `parameter name^*`.
    pub parameter_name: String,
    /// Schema field name: `parameter type*`.
    pub parameter_type: Enum<H4MaterialParameterType, i32>,
    /// Schema field name: `parameter index*!`.
    pub parameter_index: i32,
    pub bitmap: TagRef,
    pub bitmap_path: String,
    pub color: RealArgbColor,
    pub real: f32,
    pub vector: RealVector3d,
    /// Schema field name: `int/bool`.
    pub int_bool: i32,
    pub bitmap_flags: u16,
    pub bitmap_filter_mode: u16,
    pub bitmap_address_mode: u16,
    pub bitmap_address_mode_x: u16,
    pub bitmap_address_mode_y: u16,
    pub bitmap_sharpen_mode: u16,
    pub bitmap_extern_mode: u8,
    pub bitmap_min_mipmap: u8,
    pub bitmap_max_mipmap: u8,
    pub render_phases_used: u8,
    pub function_parameters: Vec<H4MaterialFunctionParameter>,
    pub display_minimum: f32,
    pub display_maximum: f32,
    /// Schema field name: `register index*!`.
    pub register_index: u8,
    /// Schema field name: `register offset*!`.
    pub register_offset: u8,
    /// Schema field name: `register count*!`.
    pub register_count: u8,
    /// Schema field name: `register set*!`.
    pub register_set: Enum<RegisterSet, i8>,
}

tag_enum!(/// `register_set_enum` (char_enum).
    RegisterSet: i8 {
    Bool = 0 => "bool",
    Int = 1 => "int",
    Float = 2 => "float",
    Sampler = 3 => "sampler",
    VertexBool = 4 => "vertex_bool",
    VertexInt = 5 => "vertex_int",
    VertexFloat = 6 => "vertex_float",
    VertexSampler = 7 => "vertex_sampler",
});

/// `material_postprocess_texture_block` — one bound texture in the baked
/// postprocess definition.
#[derive(Debug, Clone, Default)]
pub struct H4MaterialPostprocessTexture {
    pub bitmap_reference: TagRef,
    pub address_mode: u8,
    pub filter_mode: u8,
    pub frame_index_parameter: u8,
    pub sampler_index: u8,
    pub level_of_smallest_mipmap_to_use: i8,
    pub level_of_largest_mipmap_to_use: i8,
    pub render_phase_mask: u8,
}

/// `real_vector4d_block` — a `real_vector_3d` + a `w` real, used for the
/// `texture constants` / `real vectors` / `real vertex vectors` tables.
#[derive(Debug, Clone, Default)]
pub struct RealVector4d {
    pub vector: RealVector3d,
    /// Schema field name: `vector w`.
    pub vector_w: f32,
}

/// `int_block` — one element of the `int constants` table.
#[derive(Debug, Clone, Default)]
pub struct IntConstant {
    /// Schema field name: `int value`.
    pub int_value: i32,
}

/// `functionParameterBlock` — postprocess `function parameters` table entry.
#[derive(Debug, Clone, Default)]
pub struct FunctionParameter {
    pub function_index: u8,
    pub render_phase_mask: u8,
    pub register_index: u8,
    pub register_offset: u8,
}

/// `externParameterBlock` — postprocess `extern parameters` table entry.
#[derive(Debug, Clone, Default)]
pub struct ExternParameter {
    pub extern_index: u8,
    pub extern_register: u8,
    pub extern_offset: u8,
    pub render_phase_mask: u8,
}

/// `material_type_struct` — `physics material N!` slot.
#[derive(Debug, Clone, Default)]
pub struct MaterialType {
    pub global_material_index: i16,
}

/// `material_postprocess_block` — the baked, runtime-ready `postprocess
/// definition!`: the texture/constant/function tables the engine binds.
#[derive(Debug, Clone, Default)]
pub struct H4MaterialPostprocess {
    pub textures: Vec<H4MaterialPostprocessTexture>,
    pub texture_constants: Vec<RealVector4d>,
    pub real_vectors: Vec<RealVector4d>,
    pub real_vertex_vectors: Vec<RealVector4d>,
    pub int_constants: Vec<IntConstant>,
    pub bool_constants: i32,
    pub used_bool_constants: i32,
    pub bool_render_phase_mask: i32,
    pub vertex_bool_constants: i32,
    pub used_vertex_bool_constants: i32,
    pub vertex_bool_render_phase_mask: i32,
    pub functions: Vec<H4MaterialFunctionParameter>,
    pub function_parameters: Vec<FunctionParameter>,
    pub extern_parameters: Vec<ExternParameter>,
    pub alpha_blend_mode: Enum<AlphaBlendMode, i8>,
    pub layer_blend_mode: Enum<LayerBlendMode, i8>,
    pub flags: Flags<MaterialPostprocessFlags, u16>,
    /// `physics material 0!`.
    pub physics_material_0: MaterialType,
    /// `physics material 1!`.
    pub physics_material_1: MaterialType,
    /// `physics material 2!`.
    pub physics_material_2: MaterialType,
    /// `physics material 3!`.
    pub physics_material_3: MaterialType,
}

/// Root `material_block_struct` — the whole H4 `material` (`mat `) tag.
///
/// Re-exported as `H4Material` to avoid clashing with
/// [`crate::render_model::Material`].
#[derive(Debug, Clone, Default)]
pub struct Material {
    pub material_shader: TagRef,
    /// `material parameters` block.
    pub material_parameters: Vec<H4MaterialParameter>,
    /// `postprocess definition!` block. The schema allows >1, but exactly
    /// one is authored in practice; the whole list is mirrored.
    pub postprocess_definition: Vec<H4MaterialPostprocess>,
    pub physics_material_name: String,
    pub physics_material_name_2: String,
    pub physics_material_name_3: String,
    pub physics_material_name_4: String,
    pub sort_offset: f32,
    pub alpha_blend_mode: Enum<AlphaBlendMode, i8>,
    /// Schema field name: `sort layer*`.
    pub sort_layer: Enum<GlobalSortLayer, i8>,
    /// Schema field name: `flags!`.
    pub flags: Flags<MaterialFlags, u8>,
    /// Schema field name: `render flags!`.
    pub render_flags: Flags<MaterialRenderFlags, u8>,
    /// Schema field name: `Transparent Shadow Policy`.
    pub transparent_shadow_policy: Enum<MaterialTransparentShadowPolicy, i8>,
}

/// Alias for [`Material`] avoiding the `render_model::Material` name clash.
pub type H4Material = Material;

// =============================================================================
// Walker
// =============================================================================

impl Material {
    /// Walk a parsed H4 `material` (`mat `) tag into the schema-faithful tree.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_MAT {
            return Err(ShaderError::WrongGroup { expected: GROUP_MAT, found });
        }
        let root = tag.root();
        Ok(Self {
            material_shader: read_tag_ref(&root, "material shader"),
            material_parameters: read_parameters(&root),
            postprocess_definition: read_postprocess(&root),
            physics_material_name: root.read_string_id("physics material name").unwrap_or_default(),
            physics_material_name_2: root.read_string_id("physics material name 2").unwrap_or_default(),
            physics_material_name_3: root.read_string_id("physics material name 3").unwrap_or_default(),
            physics_material_name_4: root.read_string_id("physics material name 4").unwrap_or_default(),
            sort_offset: root.read_real("sort offset").unwrap_or(0.0),
            alpha_blend_mode: root.try_read_enum("alpha blend mode").unwrap_or_default(),
            sort_layer: root.try_read_enum("sort layer").unwrap_or_default(),
            flags: root.try_read_flags("flags").unwrap_or_default(),
            render_flags: root.try_read_flags("render flags").unwrap_or_default(),
            transparent_shadow_policy: root
                .try_read_enum("Transparent Shadow Policy")
                .unwrap_or_default(),
        })
    }
}

// -----------------------------------------------------------------------------
// Small typed scalar readers (mirroring the api.rs reader style).
// -----------------------------------------------------------------------------

fn read_i32(s: &TagStruct<'_>, name: &str) -> i32 {
    s.read_int_any(name).unwrap_or(0) as i32
}

fn read_i16(s: &TagStruct<'_>, name: &str) -> i16 {
    s.read_int_any(name).unwrap_or(0) as i16
}

fn read_u16(s: &TagStruct<'_>, name: &str) -> u16 {
    s.read_int_any(name).unwrap_or(0) as u16
}

fn read_u8(s: &TagStruct<'_>, name: &str) -> u8 {
    s.read_int_any(name).unwrap_or(0) as u8
}

fn read_i8(s: &TagStruct<'_>, name: &str) -> i8 {
    s.read_int_any(name).unwrap_or(0) as i8
}

/// Read a `real_argb_color` field. There is no `read_argb` on `TagStruct`
/// (unlike `read_rgb`), so we read the typed field value directly. Returns
/// the default (all zeros) when missing or of a different type.
fn read_argb(s: &TagStruct<'_>, name: &str) -> RealArgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealArgbColor(c)) => c,
        _ => RealArgbColor::default(),
    }
}

// -----------------------------------------------------------------------------
// Block readers
// -----------------------------------------------------------------------------

fn read_parameters(root: &TagStruct<'_>) -> Vec<H4MaterialParameter> {
    let Some(block) = root.field("material parameters").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(p) = block.element(i) else { continue };
        out.push(H4MaterialParameter {
            parameter_name: p.read_string_id("parameter name").unwrap_or_default(),
            parameter_type: p.try_read_enum("parameter type").unwrap_or_default(),
            parameter_index: read_i32(&p, "parameter index"),
            bitmap: read_tag_ref(&p, "bitmap"),
            bitmap_path: p.read_string_id("bitmap path").unwrap_or_default(),
            color: read_argb(&p, "color"),
            real: p.read_real("real").unwrap_or(0.0),
            vector: p.read_vec3("vector"),
            int_bool: read_i32(&p, "int/bool"),
            bitmap_flags: read_u16(&p, "bitmap flags"),
            bitmap_filter_mode: read_u16(&p, "bitmap filter mode"),
            bitmap_address_mode: read_u16(&p, "bitmap address mode"),
            bitmap_address_mode_x: read_u16(&p, "bitmap address mode x"),
            bitmap_address_mode_y: read_u16(&p, "bitmap address mode y"),
            bitmap_sharpen_mode: read_u16(&p, "bitmap sharpen mode"),
            bitmap_extern_mode: read_u8(&p, "bitmap extern mode"),
            bitmap_min_mipmap: read_u8(&p, "bitmap min mipmap"),
            bitmap_max_mipmap: read_u8(&p, "bitmap max mipmap"),
            render_phases_used: read_u8(&p, "render phases used"),
            function_parameters: read_function_parameters(&p, "function parameters"),
            display_minimum: p.read_real("display minimum").unwrap_or(0.0),
            display_maximum: p.read_real("display maximum").unwrap_or(0.0),
            register_index: read_u8(&p, "register index"),
            register_offset: read_u8(&p, "register offset"),
            register_count: read_u8(&p, "register count"),
            register_set: p.try_read_enum("register set").unwrap_or_default(),
        });
    }
    out
}

fn read_function_parameters(
    parent: &TagStruct<'_>,
    field: &str,
) -> Vec<H4MaterialFunctionParameter> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(f) = block.element(i) else { continue };
        out.push(H4MaterialFunctionParameter {
            type_value: f.try_read_enum("type").unwrap_or_default(),
            input_name: f.read_string_id("input name").unwrap_or_default(),
            range_name: f.read_string_id("range name").unwrap_or_default(),
            output_modifier: f.try_read_enum("Output Modifier!").unwrap_or_default(),
            output_modifier_input: f.read_string_id("Output Modifier Input!").unwrap_or_default(),
            time_period_seconds: f.read_real("time period:seconds").unwrap_or(0.0),
        });
    }
    out
}

fn read_postprocess(root: &TagStruct<'_>) -> Vec<H4MaterialPostprocess> {
    let Some(block) = root.field("postprocess definition").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(pp) = block.element(i) else { continue };
        out.push(H4MaterialPostprocess {
            textures: read_postprocess_textures(&pp, "textures"),
            texture_constants: read_vector4d_block(&pp, "texture constants"),
            real_vectors: read_vector4d_block(&pp, "real vectors"),
            real_vertex_vectors: read_vector4d_block(&pp, "real vertex vectors"),
            int_constants: read_int_constants(&pp, "int constants"),
            bool_constants: read_i32(&pp, "bool constants"),
            used_bool_constants: read_i32(&pp, "used bool constants"),
            bool_render_phase_mask: read_i32(&pp, "bool render phase mask"),
            vertex_bool_constants: read_i32(&pp, "vertex bool constants"),
            used_vertex_bool_constants: read_i32(&pp, "used vertex bool constants"),
            vertex_bool_render_phase_mask: read_i32(&pp, "vertex bool render phase mask"),
            functions: read_function_parameters(&pp, "functions"),
            function_parameters: read_function_parameter_block(&pp, "function parameters"),
            extern_parameters: read_extern_parameter_block(&pp, "extern parameters"),
            alpha_blend_mode: pp.try_read_enum("alpha blend mode").unwrap_or_default(),
            layer_blend_mode: pp.try_read_enum("layer blend mode").unwrap_or_default(),
            flags: pp.try_read_flags("flags").unwrap_or_default(),
            physics_material_0: read_material_type(&pp, "physics material 0"),
            physics_material_1: read_material_type(&pp, "physics material 1"),
            physics_material_2: read_material_type(&pp, "physics material 2"),
            physics_material_3: read_material_type(&pp, "physics material 3"),
        });
    }
    out
}

fn read_postprocess_textures(
    parent: &TagStruct<'_>,
    field: &str,
) -> Vec<H4MaterialPostprocessTexture> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(t) = block.element(i) else { continue };
        out.push(H4MaterialPostprocessTexture {
            bitmap_reference: read_tag_ref(&t, "bitmap reference"),
            address_mode: read_u8(&t, "address mode"),
            filter_mode: read_u8(&t, "filter mode"),
            frame_index_parameter: read_u8(&t, "frame index parameter"),
            sampler_index: read_u8(&t, "sampler index"),
            level_of_smallest_mipmap_to_use: read_i8(&t, "level of smallest mipmap to use"),
            level_of_largest_mipmap_to_use: read_i8(&t, "level of largest mipmap to use"),
            render_phase_mask: read_u8(&t, "render phase mask"),
        });
    }
    out
}

fn read_vector4d_block(parent: &TagStruct<'_>, field: &str) -> Vec<RealVector4d> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(v) = block.element(i) else { continue };
        out.push(RealVector4d {
            vector: v.read_vec3("vector"),
            vector_w: v.read_real("vector w").unwrap_or(0.0),
        });
    }
    out
}

fn read_int_constants(parent: &TagStruct<'_>, field: &str) -> Vec<IntConstant> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(IntConstant { int_value: read_i32(&e, "int value") });
    }
    out
}

fn read_function_parameter_block(
    parent: &TagStruct<'_>,
    field: &str,
) -> Vec<FunctionParameter> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(FunctionParameter {
            function_index: read_u8(&e, "function index"),
            render_phase_mask: read_u8(&e, "render phase mask"),
            register_index: read_u8(&e, "register index"),
            register_offset: read_u8(&e, "register offset"),
        });
    }
    out
}

fn read_extern_parameter_block(parent: &TagStruct<'_>, field: &str) -> Vec<ExternParameter> {
    let Some(block) = parent.field(field).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(ExternParameter {
            extern_index: read_u8(&e, "extern index"),
            extern_register: read_u8(&e, "extern register"),
            extern_offset: read_u8(&e, "extern offset"),
            render_phase_mask: read_u8(&e, "render phase mask"),
        });
    }
    out
}

fn read_material_type(parent: &TagStruct<'_>, field: &str) -> MaterialType {
    let Some(s) = parent.descend(field) else { return MaterialType::default() };
    MaterialType { global_material_index: read_i16(&s, "global material index") }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real H4 material and dump key fields. Skips silently
    /// when the tag isn't present.
    #[test]
    fn walks_material() {
        let path = "/Users/camden/Halo/halo4_mcc/tags/objects/characters/masterchief/shaders/mc_armor.material";
        let Ok(tag) = TagFile::read(path) else { return };
        let m = Material::from_tag(&tag).expect("parse mat ");
        eprintln!(
            "material: shader={:?} params={} postprocess={} sort_offset={} blend={:?}",
            m.material_shader.path,
            m.material_parameters.len(),
            m.postprocess_definition.len(),
            m.sort_offset,
            m.alpha_blend_mode,
        );
        for p in m.material_parameters.iter().take(8) {
            eprintln!(
                "  param {:?} type={:?} bitmap={:?} color={:?} real={} vec={:?}",
                p.parameter_name, p.parameter_type, p.bitmap.path, p.color, p.real, p.vector,
            );
        }
    }
}
