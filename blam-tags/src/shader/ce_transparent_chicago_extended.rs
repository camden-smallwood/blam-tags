//! Schema-faithful walker for the Halo CE `shader_transparent_chicago_extended`
//! (scex) tag — mirrors
//! `definitions/haloce_mcc/shader_transparent_chicago_extended.json` 1:1.
//!
//! `shader_transparent_chicago_extended` is the Custom-Edition-only sibling of
//! `shader_transparent_chicago`: it carries the same per-stage transparent
//! pipeline but splits its maps into **two sets** — a `4 stage maps` block (used
//! when 4 texture stages are available) and a `2 stage maps` block (the fallback
//! for 2-stage hardware) — plus an `extra flags` long_flags field. Each shared
//! sub-struct (`radiosity`, `physics`) comes from [`super::ce_common`].

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealPoint2d;
use crate::typed_enums::{Enum, Flags};

use super::ce_common::{
    read_physics, read_radiosity, tag_enum, ShaderPhysicsProperties, ShaderRadiosityProperties,
};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SCEX: u32 = u32::from_be_bytes(*b"scex");

// =============================================================================
// Typed enums / flags
// =============================================================================

tag_enum!(/// `flags_flags_2` (byte_flags) on `shader_transparent_properties`.
    TransparentFlags: u8 {
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
    Map2d = 0 => "2d map",
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

tag_enum!(/// `framebuffer_fade_source_enum` — also the per-stage UV animation source.
    FramebufferFadeSource: i16 {
    None = 0 => "none", AOut = 1 => "a out", BOut = 2 => "b out",
    COut = 3 => "c out", DOut = 4 => "d out",
});

tag_enum!(/// `flags_flags_3` (word_flags) on the per-stage maps blocks.
    MapFlags: u16 {
    Unfiltered = 0 => "unfiltered",
    AlphaReplicate = 1 => "alpha replicate",
    UClamped = 2 => "u clamped",
    VClamped = 3 => "v clamped",
});

tag_enum!(/// `color_function_enum` — per-stage color/alpha combiner (13 values).
    ColorFunction: i16 {
    Current = 0 => "current",
    NextMap = 1 => "next map",
    Multiply = 2 => "multiply",
    DoubleMultiply = 3 => "double multiply",
    Add = 4 => "add",
    AddSignedCurrent = 5 => "add signed current",
    AddSignedNextMap = 6 => "add signed next map",
    SubtractCurrent = 7 => "subtract current",
    SubtractNextMap = 8 => "subtract next map",
    BlendCurrentAlpha = 9 => "blend current alpha",
    BlendCurrentAlphaInverse = 10 => "blend current alpha inverse",
    BlendNextMapAlpha = 11 => "blend next map alpha",
    BlendNextMapAlphaInverse = 12 => "blend next map alpha inverse",
});

tag_enum!(/// `u_animation_function_enum` — periodic wave functions (12 values).
    UAnimationFunction: i16 {
    One = 0 => "one", Zero = 1 => "zero", Cosine = 2 => "cosine",
    CosineVariablePeriod = 3 => "cosine variable period", DiagonalWave = 4 => "diagonal wave",
    DiagonalWaveVariablePeriod = 5 => "diagonal wave variable period", Slide = 6 => "slide",
    SlideVariablePeriod = 7 => "slide variable period", Noise = 8 => "noise",
    Jitter = 9 => "jitter", Wander = 10 => "wander", Spark = 11 => "spark",
});

tag_enum!(/// `extra_flags_flags` (long_flags) — Custom Edition extensions.
    ExtraFlags: u32 {
    DontFadeActiveCamouflage = 0 => "dont fade active camouflage",
    NumericCountdownTimer = 1 => "numeric countdown timer",
    CustomEditionBlending = 2 => "custom edition blending",
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

/// `extra_layers_block_struct` — a nested shader referenced as an extra pass.
#[derive(Debug, Clone, Default)]
pub struct ExtraLayer {
    pub shader: TagRef,
}

/// `shader_transparent_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentProperties {
    /// Schema `char_integer`.
    pub numeric_counter_limit: i8,
    pub flags: Flags<TransparentFlags, u8>,
    pub first_map_type: Enum<FirstMapType, i16>,
    pub framebuffer_blend_function: Enum<FramebufferBlendFunction, i16>,
    pub framebuffer_fade_mode: Enum<FramebufferFadeMode, i16>,
    pub framebuffer_fade_source: Enum<FramebufferFadeSource, i16>,
    pub lens_flares: ShaderLensFlares,
    pub extra_layers: Vec<ExtraLayer>,
}

/// `shader_transparent_map_parameters_struct` (shared by both map sets).
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

/// `shader_transparent_map_animation_struct` (shared by both map sets).
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMapAnimation {
    pub u_animation_source: Enum<FramebufferFadeSource, i16>,
    pub u_animation_function: Enum<UAnimationFunction, i16>,
    pub u_animation_period: f32,
    pub u_animation_phase: f32,
    pub u_animation_scale: f32,
    pub v_animation_source: Enum<FramebufferFadeSource, i16>,
    pub v_animation_function: Enum<UAnimationFunction, i16>,
    pub v_animation_period: f32,
    pub v_animation_phase: f32,
    pub v_animation_scale: f32,
    pub rotation_animation_source: Enum<FramebufferFadeSource, i16>,
    pub rotation_animation_function: Enum<UAnimationFunction, i16>,
    pub rotation_animation_period: f32,
    pub rotation_animation_phase: f32,
    pub rotation_animation_scale: f32,
    pub rotation_animation_center: RealPoint2d,
}

/// `4_stage_maps_block_struct` — one stage of the 4-stage map set.
#[derive(Debug, Clone, Default)]
pub struct FourStageMap {
    pub flags: Flags<MapFlags, u16>,
    pub color_function: Enum<ColorFunction, i16>,
    pub alpha_function: Enum<ColorFunction, i16>,
    pub parameters: ShaderTransparentMapParameters,
    pub animation: ShaderTransparentMapAnimation,
}

/// `2_stage_maps_block_struct` — one stage of the 2-stage map set.
#[derive(Debug, Clone, Default)]
pub struct TwoStageMap {
    pub flags: Flags<MapFlags, u16>,
    pub color_function: Enum<ColorFunction, i16>,
    pub alpha_function: Enum<ColorFunction, i16>,
    pub parameters: ShaderTransparentMapParameters,
    pub animation: ShaderTransparentMapAnimation,
}

/// `shader_transparent_chicago_extended_block_struct` — the whole CE
/// `shader_transparent_chicago_extended` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentChicagoExtended {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentProperties,
    pub four_stage_maps: Vec<FourStageMap>,
    pub two_stage_maps: Vec<TwoStageMap>,
    pub extra_flags: Flags<ExtraFlags, u32>,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentChicagoExtended {
    /// Walk a parsed CE `shader_transparent_chicago_extended` (scex) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SCEX {
            return Err(ShaderError::WrongGroup { expected: GROUP_SCEX, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            four_stage_maps: read_four_stage_maps(&root),
            two_stage_maps: read_two_stage_maps(&root),
            extra_flags: root.try_read_flags("extra flags").unwrap_or_default(),
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderTransparentProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    let lens_flares = match s.descend("lens flares") {
        Some(l) => ShaderLensFlares {
            lens_flare_spacing: l.read_real("lens flare spacing").unwrap_or(0.0),
            lens_flare: read_tag_ref(&l, "lens flare"),
        },
        None => Default::default(),
    };
    let mut extra_layers = Vec::new();
    if let Some(block) = s.field("extra layers").and_then(|f| f.as_block()) {
        for i in 0..block.len() {
            let e = block.element(i).unwrap();
            extra_layers.push(ExtraLayer { shader: read_tag_ref(&e, "shader") });
        }
    }
    ShaderTransparentProperties {
        numeric_counter_limit: s.read_int_any("numeric counter limit").unwrap_or(0) as i8,
        flags: s.try_read_flags("flags").unwrap_or_default(),
        first_map_type: s.try_read_enum("first map type").unwrap_or_default(),
        framebuffer_blend_function: s.try_read_enum("framebuffer blend function").unwrap_or_default(),
        framebuffer_fade_mode: s.try_read_enum("framebuffer fade mode").unwrap_or_default(),
        framebuffer_fade_source: s.try_read_enum("framebuffer fade source").unwrap_or_default(),
        lens_flares,
        extra_layers,
    }
}

fn read_parameters(parent: &TagStruct<'_>) -> ShaderTransparentMapParameters {
    let Some(p) = parent.descend("parameters") else { return Default::default() };
    ShaderTransparentMapParameters {
        map_u_scale: p.read_real("map u scale").unwrap_or(0.0),
        map_v_scale: p.read_real("map v scale").unwrap_or(0.0),
        map_u_offset: p.read_real("map u offset").unwrap_or(0.0),
        map_v_offset: p.read_real("map v offset").unwrap_or(0.0),
        map_rotation: p.read_real("map rotation").unwrap_or(0.0),
        mipmap_bias: p.read_real("mipmap bias").unwrap_or(0.0),
        map: read_tag_ref(&p, "map"),
    }
}

fn read_animation(parent: &TagStruct<'_>) -> ShaderTransparentMapAnimation {
    let Some(a) = parent.descend("animation") else { return Default::default() };
    ShaderTransparentMapAnimation {
        u_animation_source: a.try_read_enum("u animation source").unwrap_or_default(),
        u_animation_function: a.try_read_enum("u animation function").unwrap_or_default(),
        u_animation_period: a.read_real("u animation period").unwrap_or(0.0),
        u_animation_phase: a.read_real("u animation phase").unwrap_or(0.0),
        u_animation_scale: a.read_real("u animation scale").unwrap_or(0.0),
        v_animation_source: a.try_read_enum("v animation source").unwrap_or_default(),
        v_animation_function: a.try_read_enum("v animation function").unwrap_or_default(),
        v_animation_period: a.read_real("v animation period").unwrap_or(0.0),
        v_animation_phase: a.read_real("v animation phase").unwrap_or(0.0),
        v_animation_scale: a.read_real("v animation scale").unwrap_or(0.0),
        rotation_animation_source: a.try_read_enum("rotation animation source").unwrap_or_default(),
        rotation_animation_function: a.try_read_enum("rotation animation function").unwrap_or_default(),
        rotation_animation_period: a.read_real("rotation animation period").unwrap_or(0.0),
        rotation_animation_phase: a.read_real("rotation animation phase").unwrap_or(0.0),
        rotation_animation_scale: a.read_real("rotation animation scale").unwrap_or(0.0),
        rotation_animation_center: a.read_point2d("rotation animation center"),
    }
}

fn read_four_stage_maps(root: &TagStruct<'_>) -> Vec<FourStageMap> {
    let Some(block) = root.field("4 stage maps").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        out.push(FourStageMap {
            flags: m.try_read_flags("flags").unwrap_or_default(),
            color_function: m.try_read_enum("color function").unwrap_or_default(),
            alpha_function: m.try_read_enum("alpha function").unwrap_or_default(),
            parameters: read_parameters(&m),
            animation: read_animation(&m),
        });
    }
    out
}

fn read_two_stage_maps(root: &TagStruct<'_>) -> Vec<TwoStageMap> {
    let Some(block) = root.field("2 stage maps").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        out.push(TwoStageMap {
            flags: m.try_read_flags("flags").unwrap_or_default(),
            color_function: m.try_read_enum("color function").unwrap_or_default(),
            alpha_function: m.try_read_enum("alpha function").unwrap_or_default(),
            parameters: read_parameters(&m),
            animation: read_animation(&m),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE scex tag and sanity-check key fields.
    /// Skips silently when no such tag is present.
    #[test]
    fn walks_chicago_extended() {
        let path = "/Users/camden/Halo/haloce_mcc/tags/test.shader_transparent_chicago_extended";
        let Ok(tag) = TagFile::read(path) else { return };
        let sh = ShaderTransparentChicagoExtended::from_tag(&tag).expect("parse scex");
        eprintln!(
            "scex: 4stage={} 2stage={} extra_layers={} blend={:?} extra_flags={:?}",
            sh.four_stage_maps.len(),
            sh.two_stage_maps.len(),
            sh.properties.extra_layers.len(),
            sh.properties.framebuffer_blend_function,
            sh.extra_flags,
        );
    }
}
