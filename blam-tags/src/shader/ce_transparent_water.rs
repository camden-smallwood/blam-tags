//! Schema-faithful walker for the Halo CE `shader_transparent_water` (swat)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_water.json` 1:1.
//!
//! `shader_transparent_water` is the classic water-surface shader: a base map
//! (alpha/color modulating reflection/background), a flat reflection cube map
//! with perpendicular/parallel specular properties, and a set of animated
//! "ripple" bump maps (up to four ripple maps, each scrolling at its own
//! angle/velocity, with per-ripple contribution/offset/repeat parameters).

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealPoint2d;
use crate::typed_enums::Flags;

use super::ce_common::{
    read_physics, read_radiosity, read_specular_properties, tag_enum, ShaderPhysicsProperties,
    ShaderRadiosityProperties, ShaderSpecularProperties,
};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SWAT: u32 = u32::from_be_bytes(*b"swat");

// =============================================================================
// Typed enums / flags (water-specific)
// =============================================================================

tag_enum!(/// `water_flags_flags` (word_flags) on `shader_transparent_water_properties`.
    WaterFlags: u16 {
    BaseMapAlphaModulatesReflection = 0 => "base map alpha modulates reflection",
    BaseMapColorModulatesBackground = 1 => "base map color modulates background",
    AtmosphericFog = 2 => "atmospheric fog",
    DrawBeforeFog = 3 => "draw before fog",
});

// =============================================================================
// Schema structs (water-specific)
// =============================================================================

/// `shader_transparent_water_properties_struct` — base/reflection maps plus
/// the reflection cube map's specular brightness/tint.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentWaterProperties {
    pub water_flags: Flags<WaterFlags, u16>,
    pub base_map: TagRef,
    pub reflection_map_properties: ShaderSpecularProperties,
    /// Flat reflection cube map.
    pub reflection_map: TagRef,
}

/// `ripples_block_struct` — one animated ripple layer (up to 4 per shader).
#[derive(Debug, Clone, Default)]
pub struct Ripple {
    pub contribution_factor: f32,
    /// `angle` (radians).
    pub animation_angle: f32,
    pub animation_velocity: f32,
    pub map_offset: RealPoint2d,
    pub map_repeats: i16,
    pub map_index: i16,
}

/// `shader_transparent_water_ripples_struct` — global ripple-animation params
/// plus the per-layer `ripples` block.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentWaterRipples {
    /// `angle` (radians).
    pub animation_angle: f32,
    pub animation_velocity: f32,
    pub scale: f32,
    /// Ripple (bump) maps texture.
    pub maps: TagRef,
    pub mipmap_levels: i16,
    pub mipmap_fade_factor: f32,
    pub mipmap_detail_bias: f32,
    pub ripples: Vec<Ripple>,
}

/// `shader_transparent_water_block_struct` — the whole CE
/// `shader_transparent_water` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentWater {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentWaterProperties,
    pub ripples: ShaderTransparentWaterRipples,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentWater {
    /// Walk a parsed CE `shader_transparent_water` (swat) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SWAT {
            return Err(ShaderError::WrongGroup { expected: GROUP_SWAT, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            ripples: read_ripples(&root),
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderTransparentWaterProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    let reflection_map_properties = match s.descend("reflection map properties") {
        Some(c) => read_specular_properties(&c),
        None => Default::default(),
    };
    ShaderTransparentWaterProperties {
        water_flags: s.try_read_flags("water flags").unwrap_or_default(),
        base_map: read_tag_ref(&s, "base map"),
        reflection_map_properties,
        reflection_map: read_tag_ref(&s, "reflection map"),
    }
}

fn read_ripples(root: &TagStruct<'_>) -> ShaderTransparentWaterRipples {
    let Some(s) = root.descend("ripples") else { return Default::default() };
    let ripples = match s.field("ripples").and_then(|f| f.as_block()) {
        Some(block) => (0..block.len())
            .filter_map(|i| block.element(i))
            .map(|r| read_ripple(&r))
            .collect(),
        None => Vec::new(),
    };
    ShaderTransparentWaterRipples {
        animation_angle: s.read_real("animation angle").unwrap_or(0.0),
        animation_velocity: s.read_real("animation velocity").unwrap_or(0.0),
        scale: s.read_real("scale").unwrap_or(0.0),
        maps: read_tag_ref(&s, "maps"),
        mipmap_levels: s.read_int_any("mipmap levels").unwrap_or(0) as i16,
        mipmap_fade_factor: s.read_real("mipmap fade factor").unwrap_or(0.0),
        mipmap_detail_bias: s.read_real("mipmap detail bias").unwrap_or(0.0),
        ripples,
    }
}

fn read_ripple(r: &TagStruct<'_>) -> Ripple {
    Ripple {
        contribution_factor: r.read_real("contribution factor").unwrap_or(0.0),
        animation_angle: r.read_real("animation angle").unwrap_or(0.0),
        animation_velocity: r.read_real("animation velocity").unwrap_or(0.0),
        map_offset: r.read_point2d("map offset"),
        map_repeats: r.read_int_any("map repeats").unwrap_or(0) as i16,
        map_index: r.read_int_any("map index").unwrap_or(0) as i16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE `shader_transparent_water` and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_water_shader() {
        let path = "/Users/camden/Halo/haloce_mcc/tags/levels/a30/shaders/water.shader_transparent_water";
        let Ok(tag) = TagFile::read(path) else { return };
        let sw = ShaderTransparentWater::from_tag(&tag).expect("parse swat");
        eprintln!(
            "water: base={:?} reflection={:?} maps={:?} water_flags={:?} ripples={}",
            sw.properties.base_map.path,
            sw.properties.reflection_map.path,
            sw.ripples.maps.path,
            sw.properties.water_flags,
            sw.ripples.ripples.len(),
        );
    }
}
