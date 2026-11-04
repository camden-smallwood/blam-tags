//! Halo 4 materials: `material` (mat ) over `material_shader` (mats).
//!
//! A material names its material shader and overrides some of its
//! parameters by name. Neither tag ships a baked postprocess: the game
//! builds it at load (`c_material::postprocess`, MCC tag_test
//! 0x140881FD0, X360 0x83A7BC40), and [`bake`] reproduces that.
//!
//! The shader's parameters carry the register each value lands in; a
//! material's copies of them carry zeros there, which is why the bake reads
//! the registers from the shader and the values from the material.

mod bake;
mod submit;

pub use bake::{bake, postprocess_flags, BakedBool, BakedFunction, BakedMaterial, BakedTexture, BitmapLookup, LayerBlendMode};
pub use submit::{
    bitmap_index, color_degamma, evaluate_function_output, update_constants, FunctionContext, PIXEL_REGISTERS,
    TEXTURE_TRANSFORM_REGISTERS, VERTEX_REGISTERS,
};

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::tag_function::TagFunction;

/// `material_shader_parameter_type_enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialParameterType {
    Bitmap,
    Real,
    Int,
    Bool,
    Color,
}

impl MaterialParameterType {
    pub const ALL: [Self; 5] = [Self::Bitmap, Self::Real, Self::Int, Self::Bool, Self::Color];

    fn from_index(index: i128) -> Option<Self> {
        Self::ALL.get(usize::try_from(index).ok()?).copied()
    }
}

/// `material_animated_parameter_type_enum`: what a parameter's function
/// drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialFunctionType {
    Value,
    Color,
    ScaleUniform,
    ScaleU,
    ScaleV,
    OffsetU,
    OffsetV,
    FrameIndex,
    Alpha,
}

impl MaterialFunctionType {
    const ALL: [Self; 9] = [
        Self::Value,
        Self::Color,
        Self::ScaleUniform,
        Self::ScaleU,
        Self::ScaleV,
        Self::OffsetU,
        Self::OffsetV,
        Self::FrameIndex,
        Self::Alpha,
    ];

    fn from_index(index: i128) -> Option<Self> {
        Self::ALL.get(usize::try_from(index).ok()?).copied()
    }

    /// A texture transform: scale or offset.
    pub fn is_transform(self) -> bool {
        matches!(self, Self::ScaleUniform | Self::ScaleU | Self::ScaleV | Self::OffsetU | Self::OffsetV)
    }
}

/// One `function parameters` element of a parameter.
#[derive(Debug, Clone)]
pub struct MaterialFunctionParameter {
    pub kind: MaterialFunctionType,
    pub input_name: String,
    pub range_name: String,
    /// `materialFunctionOutputModEnum`: 0 none, 1 add, 2 multiply.
    pub output_modifier: u8,
    pub output_modifier_input: String,
    pub time_period: f32,
    pub function: Option<TagFunction>,
}

impl MaterialFunctionParameter {
    fn from_struct(s: &TagStruct<'_>) -> Option<Self> {
        Some(Self {
            kind: MaterialFunctionType::from_index(s.read_int_any("type")?)?,
            input_name: s.read_string_id("input name").unwrap_or_default(),
            range_name: s.read_string_id("range name").unwrap_or_default(),
            output_modifier: s.read_int_any("Output Modifier").unwrap_or(0) as u8,
            output_modifier_input: s.read_string_id("Output Modifier Input").unwrap_or_default(),
            time_period: s.read_real("time period").or_else(|| s.read_real("time period:seconds")).unwrap_or(0.0),
            function: s
                .field("function")
                .and_then(|field| field.as_struct())
                .and_then(|inner| inner.field("data"))
                .and_then(|field| field.as_function()),
        })
    }

    /// The game queues a function to run per draw unless it is constant and
    /// unmodified (`c_material::postprocess`).
    pub fn is_dynamic(&self) -> bool {
        let constant = self.function.as_ref().is_none_or(TagFunction::is_constant);
        !constant || (self.output_modifier != 0 && !self.output_modifier_input.is_empty())
    }
}

/// One `material parameters` element, of a material or its shader.
#[derive(Debug, Clone)]
pub struct MaterialParameter {
    pub name: String,
    pub kind: MaterialParameterType,
    pub bitmap: Option<String>,
    pub bitmap_path: String,
    /// Alpha, red, green, blue, as stored.
    pub color: [f32; 4],
    pub real: f32,
    pub vector: [f32; 3],
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
    pub functions: Vec<MaterialFunctionParameter>,
    pub display_minimum: f32,
    pub display_maximum: f32,
    pub register_index: u8,
    pub register_offset: u8,
    pub register_count: u8,
    /// `register_set_enum`: 0–3 pixel (bool, int, float, sampler), 4–7 the
    /// same for the vertex stage.
    pub register_set: u8,
}

impl MaterialParameter {
    fn from_struct(s: &TagStruct<'_>) -> Option<Self> {
        let byte = |name: &str| s.read_int_any(name).unwrap_or(0) as u8;
        let word = |name: &str| s.read_int_any(name).unwrap_or(0) as u16;
        let color = match s.field("color").and_then(|field| field.value()) {
            Some(crate::fields::TagFieldData::RealArgbColor(color)) => [color.alpha, color.red, color.green, color.blue],
            _ => [0.0; 4],
        };
        let vector = s.read_vec3("vector");
        Some(Self {
            name: s.read_string_id("parameter name").unwrap_or_default(),
            kind: MaterialParameterType::from_index(s.read_int_any("parameter type")?)?,
            bitmap: s.read_tag_ref_path("bitmap").filter(|path| !path.is_empty()),
            bitmap_path: s.read_string_id("bitmap path").unwrap_or_default(),
            color,
            real: s.read_real("real").unwrap_or(0.0),
            vector: [vector.i, vector.j, vector.k],
            int_bool: s.read_int_any("int\\bool").unwrap_or(0) as i32,
            bitmap_flags: word("bitmap flags"),
            bitmap_filter_mode: word("bitmap filter mode"),
            bitmap_address_mode: word("bitmap address mode"),
            bitmap_address_mode_x: word("bitmap address mode x"),
            bitmap_address_mode_y: word("bitmap address mode y"),
            bitmap_sharpen_mode: word("bitmap sharpen mode"),
            bitmap_extern_mode: byte("bitmap extern mode"),
            bitmap_min_mipmap: byte("bitmap min mipmap"),
            bitmap_max_mipmap: byte("bitmap max mipmap"),
            render_phases_used: byte("render phases used"),
            functions: s
                .field("function parameters")
                .and_then(|field| field.as_block())
                .map(|block| block.iter().filter_map(|element| MaterialFunctionParameter::from_struct(&element)).collect())
                .unwrap_or_default(),
            display_minimum: s.read_real("display minimum").unwrap_or(0.0),
            display_maximum: s.read_real("display maximum").unwrap_or(0.0),
            register_index: byte("register index"),
            register_offset: byte("register offset"),
            register_count: byte("register count"),
            register_set: byte("register set"),
        })
    }

    /// Set 4 and up: a vertex-stage register.
    pub fn is_vertex(&self) -> bool {
        self.register_set >= 4
    }

    /// The phase mask the parameter's records carry: its `render phases
    /// used`, or every phase when that is 0.
    pub fn phase_mask(&self) -> u8 {
        if self.render_phases_used == 0 { 0xFF } else { self.render_phases_used }
    }
}

fn read_parameters(root: &TagStruct<'_>) -> Vec<MaterialParameter> {
    root.field("material parameters")
        .and_then(|field| field.as_block())
        .map(|block| block.iter().filter_map(|element| MaterialParameter::from_struct(&element)).collect())
        .unwrap_or_default()
}

/// `material_shader_flags`.
pub mod shader_flags {
    pub const IS_DISTORTION: u32 = 1 << 0;
    pub const IS_DECAL: u32 = 1 << 1;
    pub const IS_VOLUME_FOG: u32 = 1 << 4;
    pub const IS_HOLOGRAM: u32 = 1 << 7;
    pub const IS_BLENDED_HOLOGRAM: u32 = 1 << 8;
    pub const IS_EMBLEM: u32 = 1 << 9;
    pub const IS_ALPHA_CLIP: u32 = 1 << 12;
    pub const IS_LIGHTABLE_TRANSPARENT: u32 = 1 << 13;
}

/// A `material_shader`'s parameters and which vertex shaders it compiled.
#[derive(Debug, Clone, Default)]
pub struct MaterialShader {
    pub flags: u32,
    pub parameters: Vec<MaterialParameter>,
    /// `vertex shader entry points[entry].vertex type shader indices[type].hash`.
    pub vertex_shader_hashes: Vec<Vec<i32>>,
}

impl MaterialShader {
    pub fn from_tag(tag: &TagFile) -> Self {
        let root = tag.root();
        let vertex_shader_hashes = root
            .field("vertex shader entry points")
            .and_then(|field| field.as_block())
            .map(|block| {
                block
                    .iter()
                    .map(|entry| {
                        entry
                            .field("vertex type shader indices")
                            .and_then(|field| field.as_block())
                            .map(|types| types.iter().map(|ty| ty.read_int_any("hash").unwrap_or(0) as i32).collect())
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            flags: root.read_int_any("flags").unwrap_or(0) as u32,
            parameters: read_parameters(&root),
            vertex_shader_hashes,
        }
    }

    /// Whether the shader compiled a vertex shader for an entry point and
    /// vertex type (`sub_140880950`). The tag stores bank hashes; an absent
    /// shader is 0 or -1 [inferred: the bank lookup itself was not read].
    pub fn has_vertex_shader(&self, entry_point: usize, vertex_type: usize) -> bool {
        self.vertex_shader_hashes
            .get(entry_point)
            .and_then(|types| types.get(vertex_type))
            .is_some_and(|&hash| hash != 0 && hash != -1)
    }

    pub fn parameter(&self, name: &str) -> Option<&MaterialParameter> {
        self.parameters.iter().find(|parameter| parameter.name == name)
    }
}

/// A `material`: its shader and its overrides.
#[derive(Debug, Clone, Default)]
pub struct Material {
    pub shader: String,
    pub parameters: Vec<MaterialParameter>,
    pub sort_offset: f32,
    /// `alpha_blend_mode_enum`, 25 values.
    pub alpha_blend_mode: u8,
    /// `global_sort_layer_enum_defintion`: invalid, pre-pass, normal, post-pass.
    pub sort_layer: u8,
    pub flags: u8,
    pub render_flags: u8,
    pub transparent_shadow_policy: u8,
}

impl Material {
    pub fn from_tag(tag: &TagFile) -> Self {
        Self::from_struct(&tag.root())
    }

    /// From a `material` struct, as a particle's or decal's embedded one.
    pub fn from_struct(root: &TagStruct<'_>) -> Self {
        let byte = |name: &str| root.read_int_any(name).unwrap_or(0) as u8;
        Self {
            shader: root.read_tag_ref_path("material shader").unwrap_or_default(),
            parameters: read_parameters(root),
            sort_offset: root.read_real("sort offset").unwrap_or(0.0),
            alpha_blend_mode: byte("alpha blend mode"),
            sort_layer: byte("sort layer"),
            flags: byte("flags"),
            render_flags: byte("render flags"),
            transparent_shadow_policy: byte("Transparent Shadow Policy"),
        }
    }

    pub fn parameter(&self, name: &str) -> Option<&MaterialParameter> {
        self.parameters.iter().find(|parameter| parameter.name == name)
    }
}

/// `c_material_shader::GetEntrypointRenderPhaseMask` (MCC 0x140880780,
/// X360 0x83A83DE0): which of a record's phase bits an entry point submits.
/// 0 submits every record.
pub fn entry_point_phase_mask(entry_point: usize) -> u8 {
    match entry_point {
        1 => 1,
        2..=11 | 34..=37 => 2,
        21..=24 | 31..=33 => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests;
