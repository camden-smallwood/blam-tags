//! Schema-faithful walker for the Halo 4 `material_shader` (mats) tag —
//! mirrors `definitions/halo4_mcc/material_shader.json` 1:1.
//!
//! A `material_shader` is the *template* side of the Halo 4 material system:
//! it carries the source HLSL/shader files, the compiled vertex/pixel/compute
//! shader entry-point tables, and — most usefully for tooling — the
//! `material parameters` block, which declares the parameters (and their
//! default values, display metadata, and animation functions) that concrete
//! `material` tags expose and override.
//!
//! STANDALONE: this module imports only the shared shader primitives
//! ([`super::TagRef`], [`super::read_tag_ref`], [`super::ShaderError`]) and the
//! generic typed-enum machinery; it does not depend on the CE shader commons.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealArgbColor, RealVector3d};
use crate::typed_enums::{Enum, Flags};

use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_MATS: u32 = u32::from_be_bytes(*b"mats");

// =============================================================================
// tag_enum! — schema-faithful enum/flags (copied verbatim from ce_common)
// =============================================================================

/// Declare a schema-faithful tag enum/flags: a `#[repr]`'d unit enum with
/// `FromPrimitive`/`ToPrimitive` (wire <-> variant) and `EnumString`/
/// `IntoStaticStr`/`VariantArray` derives (name <-> variant).
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

tag_enum!(/// `material_shader_flags` (long_flags) on the root block.
    MaterialShaderFlags: u32 {
    IsDistortion = 0 => "is_distortion",
    IsDecal = 1 => "is_decal",
    BlendedMaterials = 2 => "blended_materials",
    NoPhysicsMaterial = 3 => "no_physics_material",
    IsVolumeFog = 4 => "is_volume_fog",
    IsWater = 5 => "is_water",
    IsWaterfall = 6 => "is_waterfall",
    IsHologram = 7 => "is_hologram",
    IsBlendedHologram = 8 => "is_blended_hologram",
    IsEmblem = 9 => "is_emblem",
    BlendedMaterials2 = 10 => "blended_materials_2",
    BlendedMaterials3 = 11 => "blended_materials_3",
    IsAlphaClip = 12 => "is_alpha_clip",
    IsLightableTransparent = 13 => "is_lightable_transparent",
});

tag_enum!(/// `material_shader_parameter_type_enum` — declared parameter slot kind.
    MaterialShaderParameterType: i32 {
    Bitmap = 0 => "bitmap",
    Real = 1 => "real",
    Int = 2 => "int",
    Bool = 3 => "bool",
    Color = 4 => "color",
});

tag_enum!(/// `material_animated_parameter_type_enum` — what a function parameter drives.
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

tag_enum!(/// `materialFunctionOutputModEnum` — how the output modifier combines.
    MaterialFunctionOutputMod: i8 {
    None = 0 => " ",
    Add = 1 => "Add",
    Multiply = 2 => "Multiply",
});

tag_enum!(/// `register_set_enum` — which constant register file the param lives in.
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

// =============================================================================
// Schema structs
// =============================================================================

/// `material_shader_source_file_block` — one source shader file: its path plus
/// the raw (uncompiled) shader source bytes.
#[derive(Debug, Clone, Default)]
pub struct MaterialShaderSourceFile {
    /// Schema field `shader path^` (long_string).
    pub shader_path: String,
    /// Schema field `shader data` (`data`, definition `shader_code_block`).
    pub shader_data: Vec<u8>,
}

/// `compiled_effects_block` — a compiled-effect blob per shader platform.
#[derive(Debug, Clone, Default)]
pub struct CompiledEffects {
    /// Schema field `compiled effect data` (`data`, definition `compiled_effect_data`).
    pub compiled_effect_data: Vec<u8>,
}

/// `compiled_vertex_shader_refererence_block_struct` — (hash, index) into the
/// shader bank's compiled vertex shaders, keyed by vertex type.
#[derive(Debug, Clone, Default)]
pub struct CompiledVertexShaderReference {
    pub hash: i32,
    pub index: i32,
}

/// `material_vertex_shader_entry_point_block_struct` — per entry point, the
/// vertex-type-keyed list of compiled vertex shader references.
#[derive(Debug, Clone, Default)]
pub struct MaterialVertexShaderEntryPoint {
    pub vertex_type_shader_indices: Vec<CompiledVertexShaderReference>,
}

/// `compiled_pixel_shader_refererence_block_struct` — (hash, index) into the
/// shader bank's compiled pixel shaders, keyed by entry point.
#[derive(Debug, Clone, Default)]
pub struct CompiledPixelShaderReference {
    pub hash: i32,
    pub index: i32,
}

/// `compiled_compute_shader_refererence_block_struct` — (hash, index) into the
/// shader bank's compiled compute shaders, keyed by entry point.
#[derive(Debug, Clone, Default)]
pub struct CompiledComputeShaderReference {
    pub hash: i32,
    pub index: i32,
}

/// `mapping_function` — the `tag_function` data blob backing an animated
/// function parameter.
#[derive(Debug, Clone, Default)]
pub struct MappingFunction {
    /// Schema field `data` (`data`, definition `function_definition_data`).
    pub data: Vec<u8>,
}

/// `material_shader_function_parameter_block` — declares an animated function
/// driving one aspect of a parameter (a scroll, a color cycle, …).
#[derive(Debug, Clone, Default)]
pub struct MaterialShaderFunctionParameter {
    /// Schema field `type^` (long_enum). Renamed from `type`.
    pub type_value: Enum<MaterialAnimatedParameterType, i32>,
    pub input_name: String,
    pub range_name: String,
    /// Schema field `Output Modifier!` (char_enum).
    pub output_modifier: Enum<MaterialFunctionOutputMod, i8>,
    /// Schema field `Output Modifier Input!` (string_id).
    pub output_modifier_input: String,
    /// Schema field `time period:seconds` (real).
    pub time_period: f32,
    pub function: MappingFunction,
}

/// `material_shader_parameter_block` — one declared template parameter: its
/// name, type, default value (the appropriate one of bitmap/color/real/vector/
/// int), sampler state defaults, animation functions, and display metadata.
#[derive(Debug, Clone, Default)]
pub struct MaterialShaderParameter {
    /// Schema field `parameter name^*` (string_id).
    pub parameter_name: String,
    /// Schema field `parameter type*` (long_enum).
    pub parameter_type: Enum<MaterialShaderParameterType, i32>,
    /// Schema field `parameter index*!` (long_integer).
    pub parameter_index: i32,
    pub bitmap: TagRef,
    pub bitmap_path: String,
    /// Schema field `color` (real_argb_color).
    pub color: RealArgbColor,
    /// Schema field `real` (real).
    pub real: f32,
    /// Schema field `vector` (real_vector_3d).
    pub vector: RealVector3d,
    /// Schema field `int/bool` (long_integer).
    pub int_bool: i32,
    pub bitmap_flags: i16,
    pub bitmap_filter_mode: i16,
    pub bitmap_address_mode: i16,
    pub bitmap_address_mode_x: i16,
    pub bitmap_address_mode_y: i16,
    pub bitmap_sharpen_mode: i16,
    pub bitmap_extern_mode: i8,
    pub bitmap_min_mipmap: i8,
    pub bitmap_max_mipmap: i8,
    pub render_phases_used: i8,
    pub function_parameters: Vec<MaterialShaderFunctionParameter>,
    /// Schema field `display name*` (`data`, definition `help_text_block`).
    pub display_name: String,
    /// Schema field `display group*` (`data`, definition `help_text_block`).
    pub display_group: String,
    /// Schema field `display help text*` (`data`, definition `help_text_block`).
    pub display_help_text: String,
    pub display_minimum: f32,
    pub display_maximum: f32,
    /// Schema field `register index*!` (byte_integer).
    pub register_index: i8,
    /// Schema field `register offset*!` (byte_integer).
    pub register_offset: i8,
    /// Schema field `register count*!` (byte_integer).
    pub register_count: i8,
    /// Schema field `register set*!` (char_enum).
    pub register_set: Enum<RegisterSet, i8>,
}

/// `material_shader_block_struct` — the whole Halo 4 `material_shader` tag.
#[derive(Debug, Clone, Default)]
pub struct H4MaterialShader {
    /// Schema field `flags!` (long_flags).
    pub flags: Flags<MaterialShaderFlags, u32>,
    /// Schema field `source shader files!` (block).
    pub source_shader_files: Vec<MaterialShaderSourceFile>,
    /// Schema field `compiled effects!` (block).
    pub compiled_effects: Vec<CompiledEffects>,
    /// Schema field `source shader hash!` (long_integer).
    pub source_shader_hash: i32,
    /// Schema field `compiled shader hash!` (long_integer).
    pub compiled_shader_hash: i32,
    pub shader_bank: TagRef,
    /// Schema field `vertex shader entry points*` (block).
    pub vertex_shader_entry_points: Vec<MaterialVertexShaderEntryPoint>,
    /// Schema field `pixel shader entry points*` (block).
    pub pixel_shader_entry_points: Vec<CompiledPixelShaderReference>,
    /// Schema field `material parameters*` (block) — the declared parameters.
    pub material_parameters: Vec<MaterialShaderParameter>,
    /// Schema field `compute shader entry points*` (block).
    pub compute_shader_entry_points: Vec<CompiledComputeShaderReference>,
}

/// Convenience alias requested by the integrator. The canonical type name is
/// [`H4MaterialShader`]; `MaterialShader` is the public-facing spelling.
pub type MaterialShader = H4MaterialShader;

// =============================================================================
// Walker
// =============================================================================

impl H4MaterialShader {
    /// Walk a parsed Halo 4 `material_shader` (mats) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_MATS {
            return Err(ShaderError::WrongGroup { expected: GROUP_MATS, found });
        }
        let root = tag.root();
        Ok(Self {
            flags: root.try_read_flags("flags").unwrap_or_default(),
            source_shader_files: read_block(&root, "source shader files", read_source_file),
            compiled_effects: read_block(&root, "compiled effects", read_compiled_effects),
            source_shader_hash: read_i32(&root, "source shader hash"),
            compiled_shader_hash: read_i32(&root, "compiled shader hash"),
            shader_bank: read_tag_ref(&root, "shader bank"),
            vertex_shader_entry_points: read_block(
                &root,
                "vertex shader entry points",
                read_vertex_entry_point,
            ),
            pixel_shader_entry_points: read_block(
                &root,
                "pixel shader entry points",
                read_pixel_reference,
            ),
            material_parameters: read_block(&root, "material parameters", read_parameter),
            compute_shader_entry_points: read_block(
                &root,
                "compute shader entry points",
                read_compute_reference,
            ),
        })
    }
}

// -----------------------------------------------------------------------------
// Block element readers
// -----------------------------------------------------------------------------

fn read_source_file(s: &TagStruct<'_>) -> MaterialShaderSourceFile {
    MaterialShaderSourceFile {
        shader_path: read_string(s, "shader path"),
        shader_data: read_data(s, "shader data"),
    }
}

fn read_compiled_effects(s: &TagStruct<'_>) -> CompiledEffects {
    CompiledEffects {
        compiled_effect_data: read_data(s, "compiled effect data"),
    }
}

fn read_vertex_entry_point(s: &TagStruct<'_>) -> MaterialVertexShaderEntryPoint {
    MaterialVertexShaderEntryPoint {
        vertex_type_shader_indices: read_block(
            s,
            "vertex type shader indices",
            read_vertex_reference,
        ),
    }
}

fn read_vertex_reference(s: &TagStruct<'_>) -> CompiledVertexShaderReference {
    CompiledVertexShaderReference {
        hash: read_i32(s, "hash"),
        index: read_i32(s, "index"),
    }
}

fn read_pixel_reference(s: &TagStruct<'_>) -> CompiledPixelShaderReference {
    CompiledPixelShaderReference {
        hash: read_i32(s, "hash"),
        index: read_i32(s, "index"),
    }
}

fn read_compute_reference(s: &TagStruct<'_>) -> CompiledComputeShaderReference {
    CompiledComputeShaderReference {
        hash: read_i32(s, "hash"),
        index: read_i32(s, "index"),
    }
}

fn read_parameter(s: &TagStruct<'_>) -> MaterialShaderParameter {
    MaterialShaderParameter {
        parameter_name: read_string(s, "parameter name"),
        parameter_type: s.try_read_enum("parameter type").unwrap_or_default(),
        parameter_index: read_i32(s, "parameter index"),
        bitmap: read_tag_ref(s, "bitmap"),
        bitmap_path: read_string(s, "bitmap path"),
        color: read_argb(s, "color"),
        real: s.read_real("real").unwrap_or(0.0),
        vector: s.read_vec3("vector"),
        int_bool: read_i32(s, "int/bool"),
        bitmap_flags: read_i16(s, "bitmap flags"),
        bitmap_filter_mode: read_i16(s, "bitmap filter mode"),
        bitmap_address_mode: read_i16(s, "bitmap address mode"),
        bitmap_address_mode_x: read_i16(s, "bitmap address mode x"),
        bitmap_address_mode_y: read_i16(s, "bitmap address mode y"),
        bitmap_sharpen_mode: read_i16(s, "bitmap sharpen mode"),
        bitmap_extern_mode: read_i8(s, "bitmap extern mode"),
        bitmap_min_mipmap: read_i8(s, "bitmap min mipmap"),
        bitmap_max_mipmap: read_i8(s, "bitmap max mipmap"),
        render_phases_used: read_i8(s, "render phases used"),
        function_parameters: read_block(s, "function parameters", read_function_parameter),
        display_name: read_data_string(s, "display name"),
        display_group: read_data_string(s, "display group"),
        display_help_text: read_data_string(s, "display help text"),
        display_minimum: s.read_real("display minimum").unwrap_or(0.0),
        display_maximum: s.read_real("display maximum").unwrap_or(0.0),
        register_index: read_i8(s, "register index"),
        register_offset: read_i8(s, "register offset"),
        register_count: read_i8(s, "register count"),
        register_set: s.try_read_enum("register set").unwrap_or_default(),
    }
}

fn read_function_parameter(s: &TagStruct<'_>) -> MaterialShaderFunctionParameter {
    let function = match s.descend("function") {
        Some(f) => MappingFunction {
            data: read_data(&f, "data"),
        },
        None => MappingFunction::default(),
    };
    MaterialShaderFunctionParameter {
        type_value: s.try_read_enum("type").unwrap_or_default(),
        input_name: read_string(s, "input name"),
        range_name: read_string(s, "range name"),
        output_modifier: s.try_read_enum("Output Modifier!").unwrap_or_default(),
        output_modifier_input: read_string(s, "Output Modifier Input!"),
        time_period: s.read_real("time period:seconds").unwrap_or(0.0),
        function,
    }
}

// -----------------------------------------------------------------------------
// Small typed readers
// -----------------------------------------------------------------------------

/// Read a `block` field and map each element through `f`. Missing/empty → `[]`.
fn read_block<T>(
    parent: &TagStruct<'_>,
    name: &str,
    f: impl Fn(&TagStruct<'_>) -> T,
) -> Vec<T> {
    let Some(block) = parent.field(name).and_then(|fld| fld.as_block()) else {
        return Vec::new();
    };
    (0..block.len()).filter_map(|i| block.element(i)).map(|e| f(&e)).collect()
}

/// Read a `data` field as raw bytes (cloned). Missing → empty.
fn read_data(s: &TagStruct<'_>, name: &str) -> Vec<u8> {
    s.field(name)
        .and_then(|f| f.as_data())
        .map(|b| b.to_vec())
        .unwrap_or_default()
}

/// Read a `data` field as a NUL-trimmed UTF-8 string (lossy on bad bytes).
fn read_data_string(s: &TagStruct<'_>, name: &str) -> String {
    s.field(name)
        .and_then(|f| f.as_data())
        .map(|b| String::from_utf8_lossy(b).trim_end_matches('\0').to_owned())
        .unwrap_or_default()
}

/// Read a `string_id`, `string`, or `long_string` field. Missing → empty.
fn read_string(s: &TagStruct<'_>, name: &str) -> String {
    use crate::fields::TagFieldData;
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::StringId(sid)) | Some(TagFieldData::OldStringId(sid)) => sid.string,
        Some(TagFieldData::String(v)) | Some(TagFieldData::LongString(v)) => v,
        _ => String::new(),
    }
}

/// Read a `real_argb_color` field. Missing → default (all zero).
fn read_argb(s: &TagStruct<'_>, name: &str) -> RealArgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(crate::fields::TagFieldData::RealArgbColor(c)) => c,
        _ => RealArgbColor::default(),
    }
}

fn read_i32(s: &TagStruct<'_>, name: &str) -> i32 {
    s.read_int_any(name).unwrap_or(0) as i32
}

fn read_i16(s: &TagStruct<'_>, name: &str) -> i16 {
    s.read_int_any(name).unwrap_or(0) as i16
}

fn read_i8(s: &TagStruct<'_>, name: &str) -> i8 {
    s.read_int_any(name).unwrap_or(0) as i8
}
