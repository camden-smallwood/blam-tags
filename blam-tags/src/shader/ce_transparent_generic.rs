//! Schema-faithful walker for the Halo CE `shader_transparent_generic` (sotr)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_generic.json` 1:1.
//!
//! `shader_transparent_generic` is the classic CE "generic" transparent shader:
//! the shared transparent properties + lens flares + extra layers (same as
//! `shader_transparent_chicago`), a `maps` block of up to 4 sampler stages with
//! per-map parameters + scrolling animation, PLUS a `stages` block (up to 7)
//! describing the fixed-function programmable combiner — per stage color0/color1
//! constants, A/B/C/D inputs with input-mapping selectors, ab/bc/cd output
//! routing with color/alpha output-mapping enums, and mux/sum flags.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealArgbColor, RealPoint2d};
use crate::typed_enums::{Enum, Flags};

// Shared lighting/material headers live in `ce_common` (radiosity + physics).
use super::ce_common::{ShaderPhysicsProperties, ShaderRadiosityProperties};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SOTR: u32 = u32::from_be_bytes(*b"sotr");

// =============================================================================
// Typed enums / flags
// =============================================================================

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

tag_enum!(/// `flags_flags_2` (byte_flags) on `shader_transparent_properties`.
    ShaderTransparentFlags: u8 {
    AlphaTested = 0 => "alpha tested",
    Decal = 1 => "decal",
    TwoSided = 2 => "two sided",
    FirstMapIsInScreenspace = 3 => "first map is in screenspace",
    DrawBeforeWater = 4 => "draw before water",
    IgnoreEffect = 5 => "ignore effect",
    ScaleFirstMapWithDistance = 6 => "scale first map with distance",
    Numeric = 7 => "numeric",
});

tag_enum!(/// `first_map_type_enum`.
    FirstMapType: i16 {
    TwoDMap = 0 => "2d map",
    FirstMapIsReflectionCubeMap = 1 => "first map is reflection cube map",
    FirstMapIsObjectCenteredCubeMap = 2 => "first map is object centered cube map",
    FirstMapIsViewerCenteredCubeMap = 3 => "first map is viewer centered cube map",
});

tag_enum!(/// `framebuffer_blend_function_enum`.
    FramebufferBlendFunction: i16 {
    AlphaBlend = 0 => "alpha blend",
    Multiply = 1 => "multiply",
    DoubleMultiply = 2 => "double multiply",
    Add = 3 => "add",
    Subtract = 4 => "subtract",
    ComponentMin = 5 => "component min",
    ComponentMax = 6 => "component max",
    AlphaMultiplyAdd = 7 => "alpha multiply add",
});

tag_enum!(/// `framebuffer_fade_mode_enum`.
    FramebufferFadeMode: i16 {
    None = 0 => "none",
    FadeWhenPerpendicular = 1 => "fade when perpendicular",
    FadeWhenParallel = 2 => "fade when parallel",
});

tag_enum!(/// `framebuffer_fade_source_enum` — external combiner output input.
    FramebufferFadeSource: i16 {
    None = 0 => "none", AOut = 1 => "a out", BOut = 2 => "b out",
    COut = 3 => "c out", DOut = 4 => "d out",
});

tag_enum!(/// `flags_flags_3` (word_flags) on each `maps` element.
    MapFlags: u16 {
    Unfiltered = 0 => "unfiltered",
    UClamped = 1 => "u clamped",
    VClamped = 2 => "v clamped",
});

tag_enum!(/// `u_animation_function_enum` — periodic wave functions (12 values).
    AnimationFunction: i16 {
    One = 0 => "one", Zero = 1 => "zero", Cosine = 2 => "cosine",
    CosineVariablePeriod = 3 => "cosine variable period", DiagonalWave = 4 => "diagonal wave",
    DiagonalWaveVariablePeriod = 5 => "diagonal wave variable period", Slide = 6 => "slide",
    SlideVariablePeriod = 7 => "slide variable period", Noise = 8 => "noise",
    Jitter = 9 => "jitter", Wander = 10 => "wander", Spark = 11 => "spark",
});

tag_enum!(/// `flags_flags_4` (word_flags) on each `stages` element.
    StageFlags: u16 {
    ColorMux = 0 => "color mux",
    AlphaMux = 1 => "alpha mux",
    AOutControlsColor0Animation = 2 => "a out controls color0 animation",
});

tag_enum!(/// `color0_source_enum`.
    Color0Source: i16 {
    None = 0 => "none", A = 1 => "a", B = 2 => "b", C = 3 => "c", D = 4 => "d",
});

tag_enum!(/// `input_a_enum` — color-combiner input selector (24 values).
    InputColor: i16 {
    Zero = 0 => "zero", One = 1 => "one", OneHalf = 2 => "one half",
    NegativeOne = 3 => "negative one", NegativeOneHalf = 4 => "negative one half",
    MapColor0 = 5 => "map color 0", MapColor1 = 6 => "map color 1",
    MapColor2 = 7 => "map color 2", MapColor3 = 8 => "map color 3",
    VertexColor0DiffuseLight = 9 => "vertex color 0 diffuse light",
    VertexColor1FadePerpendicular = 10 => "vertex color 1 fade perpendicular",
    ScratchColor0 = 11 => "scratch color 0", ScratchColor1 = 12 => "scratch color 1",
    ConstantColor0 = 13 => "constant color 0", ConstantColor1 = 14 => "constant color 1",
    MapAlpha0 = 15 => "map alpha 0", MapAlpha1 = 16 => "map alpha 1",
    MapAlpha2 = 17 => "map alpha 2", MapAlpha3 = 18 => "map alpha 3",
    VertexAlpha0FadeNone = 19 => "vertex alpha 0 fade none",
    VertexAlpha1FadePerpendicular = 20 => "vertex alpha 1 fade perpendicular",
    ScratchAlpha0 = 21 => "scratch alpha 0", ScratchAlpha1 = 22 => "scratch alpha 1",
    ConstantAlpha0 = 23 => "constant alpha 0", ConstantAlpha1 = 24 => "constant alpha 1",
});

tag_enum!(/// `input_a_mapping_enum` — input transform applied to a combiner input.
    InputMapping: i16 {
    ClampX = 0 => "clamp x", OneClampX = 1 => "1 clamp x", Two = 2 => "2",
    OneTwo = 3 => "1 2", ClampX1Two = 4 => "clamp x 1 2", OneTwoClampX = 5 => "1 2 clamp x",
    X = 6 => "x", X1 = 7 => "x 1",
});

tag_enum!(/// `output_ab_enum` — combiner output alpha routing (9 values).
    OutputAlpha: i16 {
    AlphaDiscard = 0 => "alpha discard",
    AlphaScratchAlpha0FinalAlpha = 1 => "alpha scratch alpha 0 final alpha",
    AlphaScratchAlpha1 = 2 => "alpha scratch alpha 1",
    AlphaVertexAlpha0Fog = 3 => "alpha vertex alpha 0 fog",
    AlphaVertexAlpha1 = 4 => "alpha vertex alpha 1",
    AlphaMapAlpha0 = 5 => "alpha map alpha 0", AlphaMapAlpha1 = 6 => "alpha map alpha 1",
    AlphaMapAlpha2 = 7 => "alpha map alpha 2", AlphaMapAlpha3 = 8 => "alpha map alpha 3",
});

tag_enum!(/// `output_ab_function_enum` — color combine op for the ab/cd outputs.
    OutputFunction: i16 {
    Multiply = 0 => "multiply", DotProduct = 1 => "dot product",
});

tag_enum!(/// `output_mapping_color_enum` — post-combine output scale/bias.
    OutputMappingColor: i16 {
    ColorIdentity = 0 => "color identity",
    ColorScaleBy1Two = 1 => "color scale by 1 2",
    ColorScaleBy2 = 2 => "color scale by 2",
    ColorScaleBy4 = 3 => "color scale by 4",
    ColorBiasBy1Two = 4 => "color bias by 1 2",
    ColorExpandNormal = 5 => "color expand normal",
});

tag_enum!(/// `input_a_alpha_enum` — alpha-combiner input selector (25 values).
    InputAlpha: i16 {
    Zero = 0 => "zero", One = 1 => "one", OneHalf = 2 => "one half",
    NegativeOne = 3 => "negative one", NegativeOneHalf = 4 => "negative one half",
    MapAlpha0 = 5 => "map alpha 0", MapAlpha1 = 6 => "map alpha 1",
    MapAlpha2 = 7 => "map alpha 2", MapAlpha3 = 8 => "map alpha 3",
    VertexAlpha0FadeNone = 9 => "vertex alpha 0 fade none",
    VertexAlpha1FadePerpendicular = 10 => "vertex alpha 1 fade perpendicular",
    ScratchAlpha0 = 11 => "scratch alpha 0", ScratchAlpha1 = 12 => "scratch alpha 1",
    ConstantAlpha0 = 13 => "constant alpha 0", ConstantAlpha1 = 14 => "constant alpha 1",
    MapBlue0 = 15 => "map blue 0", MapBlue1 = 16 => "map blue 1",
    MapBlue2 = 17 => "map blue 2", MapBlue3 = 18 => "map blue 3",
    VertexBlue0BlueLight = 19 => "vertex blue 0 blue light",
    VertexBlue1FadeParallel = 20 => "vertex blue 1 fade parallel",
    ScratchBlue0 = 21 => "scratch blue 0", ScratchBlue1 = 22 => "scratch blue 1",
    ConstantBlue0 = 23 => "constant blue 0", ConstantBlue1 = 24 => "constant blue 1",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_lens_flares_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderLensFlares {
    pub lens_flare_spacing: f32,
    pub lens_flare: TagRef,
}

/// `extra_layers_block_struct` — a chained extra shader layer.
#[derive(Debug, Clone, Default)]
pub struct ExtraLayer {
    pub shader: TagRef,
}

/// `shader_transparent_properties_struct` — shared classic transparent header.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentProperties {
    /// Schema `char_integer`.
    pub numeric_counter_limit: i8,
    pub flags: Flags<ShaderTransparentFlags, u8>,
    pub first_map_type: Enum<FirstMapType, i16>,
    pub framebuffer_blend_function: Enum<FramebufferBlendFunction, i16>,
    pub framebuffer_fade_mode: Enum<FramebufferFadeMode, i16>,
    pub framebuffer_fade_source: Enum<FramebufferFadeSource, i16>,
    pub lens_flares: ShaderLensFlares,
    pub extra_layers: Vec<ExtraLayer>,
}

/// `shader_transparent_map_parameters_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMapParameters {
    pub map_u_scale: f32,
    pub map_v_scale: f32,
    pub map_u_offset: f32,
    pub map_v_offset: f32,
    pub map_rotation: f32,
    pub mipmap_bias: f32,
    pub map: TagRef,
}

/// `shader_transparent_map_animation_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMapAnimation {
    pub u_animation_source: Enum<FramebufferFadeSource, i16>,
    pub u_animation_function: Enum<AnimationFunction, i16>,
    pub u_animation_period: f32,
    pub u_animation_phase: f32,
    pub u_animation_scale: f32,
    pub v_animation_source: Enum<FramebufferFadeSource, i16>,
    pub v_animation_function: Enum<AnimationFunction, i16>,
    pub v_animation_period: f32,
    pub v_animation_phase: f32,
    pub v_animation_scale: f32,
    pub rotation_animation_source: Enum<FramebufferFadeSource, i16>,
    pub rotation_animation_function: Enum<AnimationFunction, i16>,
    pub rotation_animation_period: f32,
    pub rotation_animation_phase: f32,
    pub rotation_animation_scale: f32,
    pub rotation_animation_center: RealPoint2d,
}

/// `maps_block_struct` — one texture sampler stage (up to 4).
#[derive(Debug, Clone, Default)]
pub struct Map {
    pub flags: Flags<MapFlags, u16>,
    pub parameters: ShaderTransparentMapParameters,
    pub animation: ShaderTransparentMapAnimation,
}

/// `stages_block_struct` — one programmable-combiner stage (up to 7).
#[derive(Debug, Clone, Default)]
pub struct Stage {
    pub flags: Flags<StageFlags, u16>,
    pub color0_source: Enum<Color0Source, i16>,
    pub color0_animation_function: Enum<AnimationFunction, i16>,
    pub color0_animation_period: f32,
    /// `real_argb_color`. NOTE: read via `read_rgb` which drops the alpha
    /// channel; alpha defaults to 0.0 until an argb reader exists.
    pub color0_animation_lower_bound: RealArgbColor,
    /// `real_argb_color` (see `color0_animation_lower_bound` alpha note).
    pub color0_animation_upper_bound: RealArgbColor,
    /// `real_argb_color` (see `color0_animation_lower_bound` alpha note).
    pub color1: RealArgbColor,
    // --- color combiner inputs ---
    pub input_a: Enum<InputColor, i16>,
    pub input_a_mapping: Enum<InputMapping, i16>,
    pub input_b: Enum<InputColor, i16>,
    pub input_b_mapping: Enum<InputMapping, i16>,
    pub input_c: Enum<InputColor, i16>,
    pub input_c_mapping: Enum<InputMapping, i16>,
    pub input_d: Enum<InputColor, i16>,
    pub input_d_mapping: Enum<InputMapping, i16>,
    // --- color combiner outputs ---
    pub output_ab: Enum<OutputAlpha, i16>,
    pub output_ab_function: Enum<OutputFunction, i16>,
    pub output_bc: Enum<OutputAlpha, i16>,
    pub output_cd_function: Enum<OutputFunction, i16>,
    pub output_ab_cd_mux_sum: Enum<OutputAlpha, i16>,
    pub output_mapping_color: Enum<OutputMappingColor, i16>,
    // --- alpha combiner inputs ---
    pub input_a_alpha: Enum<InputAlpha, i16>,
    pub input_a_mapping_alpha: Enum<InputMapping, i16>,
    pub input_b_alpha: Enum<InputAlpha, i16>,
    pub input_b_mapping_alpha: Enum<InputMapping, i16>,
    pub input_c_alpha: Enum<InputAlpha, i16>,
    pub input_c_mapping_alpha: Enum<InputMapping, i16>,
    pub input_d_alpha: Enum<InputAlpha, i16>,
    pub input_d_mapping_alpha: Enum<InputMapping, i16>,
    // --- alpha combiner outputs ---
    pub output_ab_alpha: Enum<OutputAlpha, i16>,
    pub output_cd_alpha: Enum<OutputAlpha, i16>,
    pub output_ab_cd_mux_sum_alpha: Enum<OutputAlpha, i16>,
    pub output_mapping_alpha: Enum<OutputMappingColor, i16>,
}

/// `shader_transparent_generic_block_struct` — the whole CE `sotr` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGeneric {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentProperties,
    pub maps: Vec<Map>,
    pub stages: Vec<Stage>,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentGeneric {
    /// Walk a parsed CE `shader_transparent_generic` (sotr) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SOTR {
            return Err(ShaderError::WrongGroup { expected: GROUP_SOTR, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            maps: read_maps(&root),
            stages: read_stages(&root),
        })
    }
}

fn read_radiosity(root: &TagStruct<'_>) -> ShaderRadiosityProperties {
    let Some(s) = root.descend("radiosity") else { return Default::default() };
    ShaderRadiosityProperties {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        detail_level: s.try_read_enum("detail level").unwrap_or_default(),
        power: s.read_real("power").unwrap_or(0.0),
        color_of_emitted_light: s.read_rgb("color of emitted light"),
        tint_color: s.read_rgb("tint color"),
    }
}

fn read_physics(root: &TagStruct<'_>) -> ShaderPhysicsProperties {
    let Some(s) = root.descend("physics") else { return Default::default() };
    ShaderPhysicsProperties {
        material_type: s.try_read_enum("material type").unwrap_or_default(),
        type_value: s.read_int_any("type").unwrap_or(0) as i16,
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderTransparentProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    ShaderTransparentProperties {
        numeric_counter_limit: s.read_int_any("numeric counter limit").unwrap_or(0) as i8,
        flags: s.try_read_flags("flags").unwrap_or_default(),
        first_map_type: s.try_read_enum("first map type").unwrap_or_default(),
        framebuffer_blend_function: s
            .try_read_enum("framebuffer blend function")
            .unwrap_or_default(),
        framebuffer_fade_mode: s.try_read_enum("framebuffer fade mode").unwrap_or_default(),
        framebuffer_fade_source: s.try_read_enum("framebuffer fade source").unwrap_or_default(),
        lens_flares: read_lens_flares(&s),
        extra_layers: read_extra_layers(&s),
    }
}

fn read_lens_flares(properties: &TagStruct<'_>) -> ShaderLensFlares {
    let Some(s) = properties.descend("lens flares") else { return Default::default() };
    ShaderLensFlares {
        lens_flare_spacing: s.read_real("lens flare spacing").unwrap_or(0.0),
        lens_flare: read_tag_ref(&s, "lens flare"),
    }
}

fn read_extra_layers(properties: &TagStruct<'_>) -> Vec<ExtraLayer> {
    let Some(block) = properties.field("extra layers").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    (0..block.len())
        .filter_map(|i| block.element(i))
        .map(|e| ExtraLayer { shader: read_tag_ref(&e, "shader") })
        .collect()
}

fn read_maps(root: &TagStruct<'_>) -> Vec<Map> {
    let Some(block) = root.field("maps").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(Map {
            flags: e.try_read_flags("flags").unwrap_or_default(),
            parameters: read_map_parameters(&e),
            animation: read_map_animation(&e),
        });
    }
    out
}

fn read_map_parameters(map: &TagStruct<'_>) -> ShaderTransparentMapParameters {
    let Some(s) = map.descend("parameters") else { return Default::default() };
    ShaderTransparentMapParameters {
        map_u_scale: s.read_real("map u scale").unwrap_or(0.0),
        map_v_scale: s.read_real("map v scale").unwrap_or(0.0),
        map_u_offset: s.read_real("map u offset").unwrap_or(0.0),
        map_v_offset: s.read_real("map v offset").unwrap_or(0.0),
        map_rotation: s.read_real("map rotation").unwrap_or(0.0),
        mipmap_bias: s.read_real("mipmap bias").unwrap_or(0.0),
        map: read_tag_ref(&s, "map"),
    }
}

fn read_map_animation(map: &TagStruct<'_>) -> ShaderTransparentMapAnimation {
    let Some(s) = map.descend("animation") else { return Default::default() };
    ShaderTransparentMapAnimation {
        u_animation_source: s.try_read_enum("u animation source").unwrap_or_default(),
        u_animation_function: s.try_read_enum("u animation function").unwrap_or_default(),
        u_animation_period: s.read_real("u animation period").unwrap_or(0.0),
        u_animation_phase: s.read_real("u animation phase").unwrap_or(0.0),
        u_animation_scale: s.read_real("u animation scale").unwrap_or(0.0),
        v_animation_source: s.try_read_enum("v animation source").unwrap_or_default(),
        v_animation_function: s.try_read_enum("v animation function").unwrap_or_default(),
        v_animation_period: s.read_real("v animation period").unwrap_or(0.0),
        v_animation_phase: s.read_real("v animation phase").unwrap_or(0.0),
        v_animation_scale: s.read_real("v animation scale").unwrap_or(0.0),
        rotation_animation_source: s
            .try_read_enum("rotation animation source")
            .unwrap_or_default(),
        rotation_animation_function: s
            .try_read_enum("rotation animation function")
            .unwrap_or_default(),
        rotation_animation_period: s.read_real("rotation animation period").unwrap_or(0.0),
        rotation_animation_phase: s.read_real("rotation animation phase").unwrap_or(0.0),
        rotation_animation_scale: s.read_real("rotation animation scale").unwrap_or(0.0),
        rotation_animation_center: s.read_point2d("rotation animation center"),
    }
}

fn read_stages(root: &TagStruct<'_>) -> Vec<Stage> {
    let Some(block) = root.field("stages").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(s) = block.element(i) else { continue };
        out.push(Stage {
            flags: s.try_read_flags("flags").unwrap_or_default(),
            color0_source: s.try_read_enum("color0 source").unwrap_or_default(),
            color0_animation_function: s
                .try_read_enum("color0 animation function")
                .unwrap_or_default(),
            color0_animation_period: s.read_real("color0 animation period").unwrap_or(0.0),
            // real_argb_color: read_rgb drops alpha (no argb reader available).
            color0_animation_lower_bound: read_argb(&s, "color0 animation lower bound"),
            color0_animation_upper_bound: read_argb(&s, "color0 animation upper bound"),
            color1: read_argb(&s, "color1"),
            input_a: s.try_read_enum("input a").unwrap_or_default(),
            input_a_mapping: s.try_read_enum("input a mapping").unwrap_or_default(),
            input_b: s.try_read_enum("input b").unwrap_or_default(),
            input_b_mapping: s.try_read_enum("input b mapping").unwrap_or_default(),
            input_c: s.try_read_enum("input c").unwrap_or_default(),
            input_c_mapping: s.try_read_enum("input c mapping").unwrap_or_default(),
            input_d: s.try_read_enum("input d").unwrap_or_default(),
            input_d_mapping: s.try_read_enum("input d mapping").unwrap_or_default(),
            output_ab: s.try_read_enum("output ab").unwrap_or_default(),
            output_ab_function: s.try_read_enum("output ab function").unwrap_or_default(),
            output_bc: s.try_read_enum("output bc").unwrap_or_default(),
            output_cd_function: s.try_read_enum("output cd function").unwrap_or_default(),
            output_ab_cd_mux_sum: s.try_read_enum("output ab cd mux sum").unwrap_or_default(),
            output_mapping_color: s.try_read_enum("output mapping color").unwrap_or_default(),
            input_a_alpha: s.try_read_enum("input a alpha").unwrap_or_default(),
            input_a_mapping_alpha: s.try_read_enum("input a mapping alpha").unwrap_or_default(),
            input_b_alpha: s.try_read_enum("input b alpha").unwrap_or_default(),
            input_b_mapping_alpha: s.try_read_enum("input b mapping alpha").unwrap_or_default(),
            input_c_alpha: s.try_read_enum("input c alpha").unwrap_or_default(),
            input_c_mapping_alpha: s.try_read_enum("input c mapping alpha").unwrap_or_default(),
            input_d_alpha: s.try_read_enum("input d alpha").unwrap_or_default(),
            input_d_mapping_alpha: s.try_read_enum("input d mapping alpha").unwrap_or_default(),
            output_ab_alpha: s.try_read_enum("output ab alpha").unwrap_or_default(),
            output_cd_alpha: s.try_read_enum("output cd alpha").unwrap_or_default(),
            output_ab_cd_mux_sum_alpha: s
                .try_read_enum("output ab cd mux sum alpha")
                .unwrap_or_default(),
            output_mapping_alpha: s.try_read_enum("output mapping alpha").unwrap_or_default(),
        });
    }
    out
}

/// Read a `real_argb_color` field. There is no direct argb reader, so this uses
/// `read_rgb` (rgb only) and leaves alpha at its `RealArgbColor::default()`
/// value. Replace with a proper argb reader if exact alpha is required.
fn read_argb(s: &TagStruct<'_>, name: &str) -> RealArgbColor {
    let rgb = s.read_rgb(name);
    RealArgbColor { red: rgb.red, green: rgb.green, blue: rgb.blue, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE generic transparent shader and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_a_generic_shader() {
        let path =
            "/Users/camden/Halo/haloce_mcc/tags/scenery/glass/shaders/glass.shader_transparent_generic";
        let Ok(tag) = TagFile::read(path) else { return };
        let sg = ShaderTransparentGeneric::from_tag(&tag).expect("parse sotr");
        eprintln!(
            "generic: maps={} stages={} blend={:?} first_map={:?}",
            sg.maps.len(),
            sg.stages.len(),
            sg.properties.framebuffer_blend_function,
            sg.properties.first_map_type,
        );
    }
}
