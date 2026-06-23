//! Schema-faithful walker for the Halo CE `shader_transparent_chicago` (schi)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_chicago.json` 1:1.
//!
//! `shader_transparent_chicago` is the classic multi-layer transparent shader:
//! a numeric/framebuffer-blend header plus an ordered list of texture `maps`
//! (each with its own UV transform, per-map flags, color/alpha combiner and
//! UV-scroll animation) and an optional chain of `extra layers` (shaders drawn
//! on top). It shares the radiosity + physics headers with the other CE
//! shaders via [`super::ce_common`].

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealPoint2d;
use crate::typed_enums::{Enum, Flags};

use super::ce_common::{ShaderPhysicsProperties, ShaderRadiosityProperties};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SCHI: u32 = u32::from_be_bytes(*b"schi");

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

tag_enum!(/// `framebuffer_fade_source_enum` — also the UV-animation source enum.
    FramebufferFadeSource: i16 {
    None = 0 => "none",
    AOut = 1 => "a out",
    BOut = 2 => "b out",
    COut = 3 => "c out",
    DOut = 4 => "d out",
});

tag_enum!(/// `flags_flags_3` (word_flags) on `maps_block`.
    MapFlags: u16 {
    Unfiltered = 0 => "unfiltered",
    AlphaReplicate = 1 => "alpha replicate",
    UClamped = 2 => "u clamped",
    VClamped = 3 => "v clamped",
});

tag_enum!(/// `color_function_enum` — the color/alpha combiner op for a map layer.
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
    AnimationFunction: i16 {
    One = 0 => "one",
    Zero = 1 => "zero",
    Cosine = 2 => "cosine",
    CosineVariablePeriod = 3 => "cosine variable period",
    DiagonalWave = 4 => "diagonal wave",
    DiagonalWaveVariablePeriod = 5 => "diagonal wave variable period",
    Slide = 6 => "slide",
    SlideVariablePeriod = 7 => "slide variable period",
    Noise = 8 => "noise",
    Jitter = 9 => "jitter",
    Wander = 10 => "wander",
    Spark = 11 => "spark",
});

tag_enum!(/// `extra_flags_flags` (long_flags) on the root block.
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

/// `extra_layers_block_struct` — one shader drawn on top, per element.
#[derive(Debug, Clone, Default)]
pub struct ExtraLayer {
    pub shader: TagRef,
}

/// `shader_transparent_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentProperties {
    pub numeric_counter_limit: i8,
    pub flags: Flags<ShaderTransparentFlags, u8>,
    pub first_map_type: Enum<FirstMapType, i16>,
    pub framebuffer_blend_function: Enum<FramebufferBlendFunction, i16>,
    pub framebuffer_fade_mode: Enum<FramebufferFadeMode, i16>,
    pub framebuffer_fade_source: Enum<FramebufferFadeSource, i16>,
    pub lens_flares: ShaderLensFlares,
    pub extra_layers: Vec<ExtraLayer>,
}

/// `shader_transparent_map_parameters_struct` — per-map UV transform + ref.
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

/// `shader_transparent_map_animation_struct` — per-map UV-scroll animation.
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

/// `maps_block_struct` — one texture layer.
#[derive(Debug, Clone, Default)]
pub struct Maps {
    pub flags: Flags<MapFlags, u16>,
    pub color_function: Enum<ColorFunction, i16>,
    pub alpha_function: Enum<ColorFunction, i16>,
    pub parameters: ShaderTransparentMapParameters,
    pub animation: ShaderTransparentMapAnimation,
}

/// `shader_transparent_chicago_block_struct` — the whole CE schi tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentChicago {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentProperties,
    pub maps: Vec<Maps>,
    pub extra_flags: Flags<ExtraFlags, u32>,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentChicago {
    /// Walk a parsed CE `shader_transparent_chicago` (schi) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SCHI {
            return Err(ShaderError::WrongGroup { expected: GROUP_SCHI, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            maps: read_maps(&root),
            extra_flags: root.try_read_flags("extra flags").unwrap_or_default(),
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
            let Some(e) = block.element(i) else { continue };
            extra_layers.push(ExtraLayer {
                shader: read_tag_ref(&e, "shader"),
            });
        }
    }
    ShaderTransparentProperties {
        numeric_counter_limit: s.read_int_any("numeric counter limit").unwrap_or(0) as i8,
        flags: s.try_read_flags("flags").unwrap_or_default(),
        first_map_type: s.try_read_enum("first map type").unwrap_or_default(),
        framebuffer_blend_function: s
            .try_read_enum("framebuffer blend function")
            .unwrap_or_default(),
        framebuffer_fade_mode: s.try_read_enum("framebuffer fade mode").unwrap_or_default(),
        framebuffer_fade_source: s
            .try_read_enum("framebuffer fade source")
            .unwrap_or_default(),
        lens_flares,
        extra_layers,
    }
}

fn read_maps(root: &TagStruct<'_>) -> Vec<Maps> {
    let Some(block) = root.field("maps").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(m) = block.element(i) else { continue };
        let parameters = match m.descend("parameters") {
            Some(p) => ShaderTransparentMapParameters {
                map_u_scale: p.read_real("map u scale").unwrap_or(0.0),
                map_v_scale: p.read_real("map v scale").unwrap_or(0.0),
                map_u_offset: p.read_real("map u offset").unwrap_or(0.0),
                map_v_offset: p.read_real("map v offset").unwrap_or(0.0),
                map_rotation: p.read_real("map rotation").unwrap_or(0.0),
                mipmap_bias: p.read_real("mipmap bias").unwrap_or(0.0),
                map: read_tag_ref(&p, "map"),
            },
            None => Default::default(),
        };
        let animation = match m.descend("animation") {
            Some(a) => read_animation(&a),
            None => Default::default(),
        };
        out.push(Maps {
            flags: m.try_read_flags("flags").unwrap_or_default(),
            color_function: m.try_read_enum("color function").unwrap_or_default(),
            alpha_function: m.try_read_enum("alpha function").unwrap_or_default(),
            parameters,
            animation,
        });
    }
    out
}

fn read_animation(a: &TagStruct<'_>) -> ShaderTransparentMapAnimation {
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
        rotation_animation_source: a
            .try_read_enum("rotation animation source")
            .unwrap_or_default(),
        rotation_animation_function: a
            .try_read_enum("rotation animation function")
            .unwrap_or_default(),
        rotation_animation_period: a.read_real("rotation animation period").unwrap_or(0.0),
        rotation_animation_phase: a.read_real("rotation animation phase").unwrap_or(0.0),
        rotation_animation_scale: a.read_real("rotation animation scale").unwrap_or(0.0),
        rotation_animation_center: a.read_point2d("rotation animation center"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE transparent-chicago shader and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_a_chicago_shader() {
        let path = "/Users/camden/Halo/haloce_mcc/tags/scenery/rocks/rock screen/shaders/rock screen.shader_transparent_chicago";
        let Ok(tag) = TagFile::read(path) else { return };
        let sh = ShaderTransparentChicago::from_tag(&tag).expect("parse schi");
        eprintln!(
            "schi: maps={} extra_layers={} first_map_type={:?} blend={:?} extra_flags={:?}",
            sh.maps.len(),
            sh.properties.extra_layers.len(),
            sh.properties.first_map_type,
            sh.properties.framebuffer_blend_function,
            sh.extra_flags,
        );
        if let Some(first) = sh.maps.first() {
            eprintln!(
                "  map[0]: ref={:?} color_fn={:?} alpha_fn={:?} u_scale={}",
                first.parameters.map.path,
                first.color_function,
                first.alpha_function,
                first.parameters.map_u_scale,
            );
        }
    }
}
