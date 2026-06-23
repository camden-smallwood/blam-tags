//! Schema-faithful walker for the Halo CE `shader_transparent_meter` (smet)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_meter.json` 1:1.
//!
//! `shader_transparent_meter` draws a "meter" (e.g. health/shield bars,
//! loading gauges): a single map whose value is colored by interpolating
//! between a gradient min/max color, composited over a background color, with
//! an optional flash color and overall meter tint. Brightness/value/gradient
//! inputs can be driven by external (animation) function outputs.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::RealRgbColor;
use crate::typed_enums::{Enum, Flags};

use super::ce_common::{read_physics, read_radiosity, ShaderPhysicsProperties, ShaderRadiosityProperties};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SMET: u32 = u32::from_be_bytes(*b"smet");

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

tag_enum!(/// `flags_flags_2` (word_flags) on `shader_transparent_meter_properties`.
    ShaderTransparentMeterFlags: u16 {
    Decal = 0 => "decal",
    TwoSided = 1 => "two sided",
    FlashColorIsNegative = 2 => "flash color is negative",
    TintMode2 = 3 => "tint mode 2",
    Unfiltered = 4 => "unfiltered",
});

tag_enum!(/// `meter_brightness_source_enum` — selects an external animation
    /// function output (a/b/c/d) as the input for a meter quantity.
    MeterBrightnessSource: i16 {
    None = 0 => "none",
    AOut = 1 => "a out",
    BOut = 2 => "b out",
    COut = 3 => "c out",
    DOut = 4 => "d out",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_transparent_meter_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMeterProperties {
    pub flags: Flags<ShaderTransparentMeterFlags, u16>,
    pub map: TagRef,
}

/// `shader_transparent_meter_colors_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMeterColors {
    pub gradient_min_color: RealRgbColor,
    pub gradient_max_color: RealRgbColor,
    pub background_color: RealRgbColor,
    pub flash_color: RealRgbColor,
    pub meter_tint_color: RealRgbColor,
    pub meter_transparency: f32,
    pub background_transparency: f32,
}

/// `shader_transparent_meter_external_function_sources_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMeterExternalFunctionSources {
    pub meter_brightness_source: Enum<MeterBrightnessSource, i16>,
    pub flash_brightness_source: Enum<MeterBrightnessSource, i16>,
    pub value_source: Enum<MeterBrightnessSource, i16>,
    pub gradient_source: Enum<MeterBrightnessSource, i16>,
    pub flash_extension_source: Enum<MeterBrightnessSource, i16>,
}

/// `shader_transparent_meter_block_struct` — the whole CE
/// `shader_transparent_meter` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentMeter {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentMeterProperties,
    pub colors: ShaderTransparentMeterColors,
    pub external_function_sources: ShaderTransparentMeterExternalFunctionSources,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentMeter {
    /// Walk a parsed CE `shader_transparent_meter` (smet) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SMET {
            return Err(ShaderError::WrongGroup { expected: GROUP_SMET, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            colors: read_colors(&root),
            external_function_sources: read_external_function_sources(&root),
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderTransparentMeterProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    ShaderTransparentMeterProperties {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        map: read_tag_ref(&s, "map"),
    }
}

fn read_colors(root: &TagStruct<'_>) -> ShaderTransparentMeterColors {
    let Some(s) = root.descend("colors") else { return Default::default() };
    ShaderTransparentMeterColors {
        gradient_min_color: s.read_rgb("gradient min color"),
        gradient_max_color: s.read_rgb("gradient max color"),
        background_color: s.read_rgb("background color"),
        flash_color: s.read_rgb("flash color"),
        meter_tint_color: s.read_rgb("meter tint color"),
        meter_transparency: s.read_real("meter transparency").unwrap_or(0.0),
        background_transparency: s.read_real("background transparency").unwrap_or(0.0),
    }
}

fn read_external_function_sources(
    root: &TagStruct<'_>,
) -> ShaderTransparentMeterExternalFunctionSources {
    let Some(s) = root.descend("external function sources") else { return Default::default() };
    ShaderTransparentMeterExternalFunctionSources {
        meter_brightness_source: s.try_read_enum("meter brightness source").unwrap_or_default(),
        flash_brightness_source: s.try_read_enum("flash brightness source").unwrap_or_default(),
        value_source: s.try_read_enum("value source").unwrap_or_default(),
        gradient_source: s.try_read_enum("gradient source").unwrap_or_default(),
        flash_extension_source: s.try_read_enum("flash extension source").unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE shader_transparent_meter and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_meter() {
        let path =
            "/Users/camden/Halo/haloce_mcc/tags/ui/shell/bitmaps/white.shader_transparent_meter";
        let Ok(tag) = TagFile::read(path) else { return };
        let sm = ShaderTransparentMeter::from_tag(&tag).expect("parse smet");
        eprintln!(
            "meter: map={:?} grad_min={:?} grad_max={:?} flags={:?} \
             meter_transparency={} value_source={:?}",
            sm.properties.map.path,
            sm.colors.gradient_min_color,
            sm.colors.gradient_max_color,
            sm.properties.flags,
            sm.colors.meter_transparency,
            sm.external_function_sources.value_source,
        );
    }
}
