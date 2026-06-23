//! Schema-faithful walker for the Halo CE `shader_transparent_plasma` (spla)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_plasma.json` 1:1.
//!
//! `shader_transparent_plasma` is a fully procedural transparent shader: there
//! is no base/diffuse texture. Instead it sums two scrolling noise maps
//! (primary + secondary, each with their own animation period / direction /
//! scale) into a scalar "plasma" field, then maps that field through an
//! intensity and offset transfer function and a perpendicular/parallel
//! fresnel tint to produce the final emissive color. Classic uses include
//! the plasma pistol/rifle bolts, shield-impact flares, and energy fields.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealVector3d;
use crate::typed_enums::Enum;

use super::ce_common::{
    read_physics, read_radiosity, read_specular_properties, ShaderPhysicsProperties,
    ShaderRadiosityProperties, ShaderSpecularProperties,
};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SPLA: u32 = u32::from_be_bytes(*b"spla");

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

tag_enum!(/// `intensity_source_enum` — selects the external animation function
    /// (`a`/`b`/`c`/`d` out) driving both intensity and offset.
    IntensitySource: i16 {
    None = 0 => "none", AOut = 1 => "a out", BOut = 2 => "b out",
    COut = 3 => "c out", DOut = 4 => "d out",
});

tag_enum!(/// `tint_color_source_enum` — selects which permutation/animation slot
    /// supplies the tint color.
    TintColorSource: i16 {
    None = 0 => "none", A = 1 => "a", B = 2 => "b", C = 3 => "c", D = 4 => "d",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_transparent_plasma_intensity_struct` — maps the plasma field to
/// emissive intensity.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentPlasmaIntensity {
    pub intensity_source: Enum<IntensitySource, i16>,
    pub intensity_exponent: f32,
}

/// `shader_transparent_plasma_offset_struct` — shifts/biases the plasma field
/// before the intensity transfer.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentPlasmaOffset {
    pub offset_source: Enum<IntensitySource, i16>,
    pub offset_amount: f32,
    pub offset_exponent: f32,
}

/// `shader_transparent_plasma_color_struct` — perpendicular/parallel fresnel
/// brightness + tint plus the tint color source selector.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentPlasmaColor {
    pub specular_properties: ShaderSpecularProperties,
    pub tint_color_source: Enum<TintColorSource, i16>,
}

/// `shader_transparent_plasma_noise_map_struct` (and `_struct_2`) — one
/// scrolling noise layer. The primary and secondary layers share this layout.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentPlasmaNoiseMap {
    pub animation_period: f32,
    pub animation_direction: RealVector3d,
    pub noise_map_scale: f32,
    pub noise_map: TagRef,
}

/// `shader_transparent_plasma_block_struct` — the whole CE
/// `shader_transparent_plasma` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentPlasma {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub intensity: ShaderTransparentPlasmaIntensity,
    pub offset: ShaderTransparentPlasmaOffset,
    pub color: ShaderTransparentPlasmaColor,
    pub primary_noise_map: ShaderTransparentPlasmaNoiseMap,
    pub secondary_noise_map: ShaderTransparentPlasmaNoiseMap,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentPlasma {
    /// Walk a parsed CE `shader_transparent_plasma` (spla) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SPLA {
            return Err(ShaderError::WrongGroup { expected: GROUP_SPLA, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            intensity: read_intensity(&root),
            offset: read_offset(&root),
            color: read_color(&root),
            primary_noise_map: read_noise_map(&root, "primary noise map"),
            secondary_noise_map: read_noise_map(&root, "secondary noise map"),
        })
    }
}

fn read_intensity(root: &TagStruct<'_>) -> ShaderTransparentPlasmaIntensity {
    let Some(s) = root.descend("intensity") else { return Default::default() };
    ShaderTransparentPlasmaIntensity {
        intensity_source: s.try_read_enum("intensity source").unwrap_or_default(),
        intensity_exponent: s.read_real("intensity exponent").unwrap_or(0.0),
    }
}

fn read_offset(root: &TagStruct<'_>) -> ShaderTransparentPlasmaOffset {
    let Some(s) = root.descend("offset") else { return Default::default() };
    ShaderTransparentPlasmaOffset {
        offset_source: s.try_read_enum("offset source").unwrap_or_default(),
        offset_amount: s.read_real("offset amount").unwrap_or(0.0),
        offset_exponent: s.read_real("offset exponent").unwrap_or(0.0),
    }
}

fn read_color(root: &TagStruct<'_>) -> ShaderTransparentPlasmaColor {
    let Some(s) = root.descend("color") else { return Default::default() };
    let specular_properties = match s.descend("specular properties") {
        Some(c) => read_specular_properties(&c),
        None => Default::default(),
    };
    ShaderTransparentPlasmaColor {
        specular_properties,
        tint_color_source: s.try_read_enum("tint color source").unwrap_or_default(),
    }
}

fn read_noise_map(root: &TagStruct<'_>, name: &str) -> ShaderTransparentPlasmaNoiseMap {
    let Some(s) = root.descend(name) else { return Default::default() };
    ShaderTransparentPlasmaNoiseMap {
        animation_period: s.read_real("animation period").unwrap_or(0.0),
        animation_direction: s.read_vec3("animation direction"),
        noise_map_scale: s.read_real("noise map scale").unwrap_or(0.0),
        noise_map: read_tag_ref(&s, "noise map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE plasma shader and print key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_plasma() {
        let path =
            "/Users/camden/Halo/haloce_mcc/tags/weapons/plasma rifle/fx/plasma bolt.shader_transparent_plasma";
        let Ok(tag) = TagFile::read(path) else { return };
        let spla = ShaderTransparentPlasma::from_tag(&tag).expect("parse spla");
        eprintln!(
            "plasma: intensity_src={:?} intensity_exp={} offset_amt={} \
             tint_src={:?} prim_noise={:?} sec_noise={:?} perp_bright={}",
            spla.intensity.intensity_source,
            spla.intensity.intensity_exponent,
            spla.offset.offset_amount,
            spla.color.tint_color_source,
            spla.primary_noise_map.noise_map.path,
            spla.secondary_noise_map.noise_map.path,
            spla.color.specular_properties.perpendicular_brightness,
        );
    }
}
