//! `shield_impact` (`shit`) tag walker — global shield-rendering
//! parameters consumed by every object's shield-impact pass.
//!
//! The tag is referenced once per cache via
//! `c_rasterizer_globals.m_shield_impact_parameters` (a single
//! `s_tag_reference` returned by
//! `c_rasterizer_globals::get_shield_impact_parameters_ref @ 0x1806E59D0`).
//! Individual `model` tags may override the global via the
//! `shield_impact_parameter_override_path` field, otherwise everyone
//! shares one set.
//!
//! ## Consumer
//!
//! `c_object_renderer::render_shield_impact_mesh_part @ 0x1806E43C0`
//! binds the 2 noise textures, sets blend = ADDITIVE, and writes 11
//! shader constants derived from this struct + per-object dynamic
//! state (overshield_amount, shield_damage):
//!
//! - cb `0x4A0001` — `(0, 0, game_time_seconds, shield_damage/255)`
//! - cb `0x4A0000` — runtime per-object override block (584B)
//! - cb `0x4A0002` — `(_, _, texture_scale, scroll_speed)`
//! - cb `0x4A0003..0x4A000A` — color × intensity packed quads (overshield
//!   1/2/ambient, impact 1/2/ambient — 8 quads total)
//! - cb `0x490000` — `(_, _, plasma_sharpness1, _)`
//!
//! ## Schema
//!
//! Reference: `definitions/halo3_mcc/shield_impact.json` (4-CC `shit`).
//! Runtime struct: `s_shield_impact_parameters` (164B) — exact layout
//! captured in [reference_effect_system_dllcache_layouts_2026_05_21].

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealRgbColor;

const SHIT_GROUP: [u8; 4] = *b"shit";

#[derive(Debug)]
pub enum ShieldImpactError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for ShieldImpactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "shield_impact: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for ShieldImpactError {}

/// A `(color, intensity)` pair as packed by the engine when filling
/// constant-buffer quads (`color.rgb * intensity` is the actual scalar
/// that lands in the shader; we keep them separate at the tag layer).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColorIntensity {
    pub color: RealRgbColor,
    pub intensity: f32,
}

/// A pair of plasma layer parameters — the shader composites two
/// independent plasma noise layers (different sharpness/scale/threshold)
/// for the overshield glow effect.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PlasmaLayer {
    pub sharpness: f32,
    pub scale: f32,
    pub threshold: f32,
}

/// Walked `shield_impact` tag — 164B `s_shield_impact_parameters`.
#[derive(Debug, Clone, Default)]
pub struct ShieldImpact {
    /// `shield_impact_noise_texture_1` tag reference path (bitmap).
    pub noise_texture_1: Option<String>,
    /// `shield_impact_noise_texture_2` tag reference path (bitmap).
    pub noise_texture_2: Option<String>,

    /// Mesh-extrusion distance along normals when rendering the
    /// shield-impact pass over the source render_geometry.
    pub extrusion_distance: f32,
    /// UV scale applied to both noise textures.
    pub texture_scale: f32,
    /// UV scroll rate (per second) applied to both noise textures.
    pub scroll_speed: f32,

    /// Plasma layer 1 — composited additively over layer 2.
    pub plasma_layer_1: PlasmaLayer,
    /// Plasma layer 2 — composited additively under layer 1.
    pub plasma_layer_2: PlasmaLayer,

    /// Overshield primary color × intensity. Multiplied by the
    /// per-object `overshield_amount` byte before shader upload.
    pub overshield_1: ColorIntensity,
    /// Overshield secondary color × intensity (blends with primary
    /// based on plasma noise).
    pub overshield_2: ColorIntensity,
    /// Overshield ambient color × intensity (radiance floor — applied
    /// uniformly regardless of plasma noise threshold).
    pub overshield_ambient: ColorIntensity,

    /// Shield-impact primary color × intensity. Multiplied by the
    /// per-object `shield_damage / 255` factor before shader upload.
    pub impact_1: ColorIntensity,
    /// Shield-impact secondary color × intensity.
    pub impact_2: ColorIntensity,
    /// Shield-impact ambient color × intensity. **Note:** the schema
    /// field name is "Impact Ambient Intensity 2" (with a trailing "2"
    /// — Bungie source typo preserved in MCC).
    pub impact_ambient: ColorIntensity,
}

impl ShieldImpact {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShieldImpactError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != SHIT_GROUP {
            return Err(ShieldImpactError::WrongGroup { expected: SHIT_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        // NOTE: field names are TitleCase in this tag (not lowercase
        // like decs/sefc) — match exactly per the schema +
        // dump_shield_impact_fields example.
        let plasma_layer_1 = PlasmaLayer {
            sharpness: s.read_real("Plasma Sharpness 1").unwrap_or(0.0),
            scale: s.read_real("Plasma Scale 1").unwrap_or(0.0),
            threshold: s.read_real("Plasma Threshold 1").unwrap_or(0.0),
        };
        let plasma_layer_2 = PlasmaLayer {
            sharpness: s.read_real("Plasma Sharpness 2").unwrap_or(0.0),
            scale: s.read_real("Plasma Scale 2").unwrap_or(0.0),
            threshold: s.read_real("Plasma Threshold 2").unwrap_or(0.0),
        };

        Self {
            noise_texture_1: s.read_tag_ref_path("Shield Impact Noise Texture 1"),
            noise_texture_2: s.read_tag_ref_path("Shield Impact Noise Texture 2"),
            extrusion_distance: s.read_real("Extrusion Distance").unwrap_or(0.0),
            texture_scale: s.read_real("Texture Scale").unwrap_or(0.0),
            scroll_speed: s.read_real("Scroll Speed").unwrap_or(0.0),
            plasma_layer_1,
            plasma_layer_2,
            overshield_1: read_color_intensity(s, "Overshield Color 1", "Overshield Intensity 1"),
            overshield_2: read_color_intensity(s, "Overshield Color 2", "Overshield Intensity 2"),
            overshield_ambient: read_color_intensity(
                s,
                "Overshield Ambient Color",
                "Overshield Ambient Intensity",
            ),
            impact_1: read_color_intensity(s, "Impact Color 1", "Impact Intensity 1"),
            impact_2: read_color_intensity(s, "Impact Color 2", "Impact Intensity 2"),
            // Schema preserves the Bungie source typo: trailing "2"
            // on the ambient field with no matching "1" counterpart.
            impact_ambient: read_color_intensity(
                s,
                "Impact Ambient Color",
                "Impact Ambient Intensity 2",
            ),
        }
    }
}

fn read_color_intensity(s: &TagStruct<'_>, color_name: &str, intensity_name: &str) -> ColorIntensity {
    ColorIntensity {
        color: s.read_rgb(color_name),
        intensity: s.read_real(intensity_name).unwrap_or(0.0),
    }
}
