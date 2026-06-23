//! Shared Halo CE shader types — the lighting/material/specular structs and
//! enums common to every classic `shader_*` walker (`shader_model`,
//! `shader_environment`, `shader_transparent_*`, …).
//!
//! These mirror the shared sub-structs in `definitions/haloce_mcc/*.json`
//! 1:1 (`shader_radiosity_properties_struct`, `shader_physics_properties_struct`,
//! `shader_specular_properties_struct`) plus the enums those structs reference.
//! Each concrete shader walker imports what it needs from here and adds its own
//! tag-specific items.

use crate::api::TagStruct;
use crate::math::RealRgbColor;
use crate::typed_enums::{Enum, Flags};

// =============================================================================
// Typed enums / flags
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

pub(crate) use tag_enum;

tag_enum!(/// `flags_flags` (word_flags) on `shader_radiosity_properties`.
    RadiosityFlags: u16 {
    SimpleParameterization = 0 => "simple parameterization",
    IgnoreNormals = 1 => "ignore normals",
    TransparentLit = 2 => "transparent lit",
});

tag_enum!(/// `detail_level_enum`.
    DetailLevel: i16 {
    High = 0 => "high", Medium = 1 => "medium", Low = 2 => "low", Turd = 3 => "turd",
});

tag_enum!(/// `material_type_enum` — physics/impact classification (33 values).
    MaterialType: i16 {
    Dirt = 0 => "dirt", Sand = 1 => "sand", Stone = 2 => "stone", Snow = 3 => "snow",
    Wood = 4 => "wood", MetalHollow = 5 => "metal hollow", MetalThin = 6 => "metal thin",
    MetalThick = 7 => "metal thick", Rubber = 8 => "rubber", Glass = 9 => "glass",
    ForceField = 10 => "force field", Grunt = 11 => "grunt", HunterArmor = 12 => "hunter armor",
    HunterSkin = 13 => "hunter skin", Elite = 14 => "elite", Jackal = 15 => "jackal",
    JackalEnergyShield = 16 => "jackal energy shield", EngineerSkin = 17 => "engineer skin",
    EngineerForceField = 18 => "engineer force field", FloodCombatForm = 19 => "flood combat form",
    FloodCarrierForm = 20 => "flood carrier form", CyborgArmor = 21 => "cyborg armor",
    CyborgEnergyShield = 22 => "cyborg energy shield", HumanArmor = 23 => "human armor",
    HumanSkin = 24 => "human skin", Sentinel = 25 => "sentinel", Monitor = 26 => "monitor",
    Plastic = 27 => "plastic", Water = 28 => "water", Leaves = 29 => "leaves",
    EliteEnergyShield = 30 => "elite energy shield", Ice = 31 => "ice", HunterShield = 32 => "hunter shield",
});

tag_enum!(/// `change_color_source_enum` — selects a permutation change-color slot.
    ChangeColorSource: i16 {
    None = 0 => "none", A = 1 => "a", B = 2 => "b", C = 3 => "c", D = 4 => "d",
});

tag_enum!(/// `flags_flags_3` (word_flags) on `shader_model_self_illumination`.
    SelfIlluminationFlags: u16 {
    NoRandomPhase = 0 => "no random phase",
});

tag_enum!(/// `animation_function_enum` — periodic wave functions (12 values).
    AnimationFunction: i16 {
    One = 0 => "one", Zero = 1 => "zero", Cosine = 2 => "cosine",
    CosineVariablePeriod = 3 => "cosine variable period", DiagonalWave = 4 => "diagonal wave",
    DiagonalWaveVariablePeriod = 5 => "diagonal wave variable period", Slide = 6 => "slide",
    SlideVariablePeriod = 7 => "slide variable period", Noise = 8 => "noise",
    Jitter = 9 => "jitter", Wander = 10 => "wander", Spark = 11 => "spark",
});

tag_enum!(/// `u_animation_source_enum` — external function input for UV scroll.
    TextureAnimationSource: i16 {
    None = 0 => "none", AOut = 1 => "a out", BOut = 2 => "b out",
    COut = 3 => "c out", DOut = 4 => "d out",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_radiosity_properties_struct` — shared lighting/material header.
#[derive(Debug, Clone, Default)]
pub struct ShaderRadiosityProperties {
    pub flags: Flags<RadiosityFlags, u16>,
    pub detail_level: Enum<DetailLevel, i16>,
    pub power: f32,
    pub color_of_emitted_light: RealRgbColor,
    pub tint_color: RealRgbColor,
}

/// `shader_physics_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderPhysicsProperties {
    pub material_type: Enum<MaterialType, i16>,
    /// Schema field `type` (short_integer).
    pub type_value: i16,
}

/// `shader_specular_properties_struct` — perpendicular/parallel fresnel
/// reflection brightness + tint.
#[derive(Debug, Clone, Default)]
pub struct ShaderSpecularProperties {
    pub perpendicular_brightness: f32,
    pub perpendicular_tint_color: RealRgbColor,
    pub parallel_brightness: f32,
    pub parallel_tint_color: RealRgbColor,
}

// =============================================================================
// Readers
// =============================================================================

pub(crate) fn read_radiosity(root: &TagStruct<'_>) -> ShaderRadiosityProperties {
    let Some(s) = root.descend("radiosity") else { return Default::default() };
    ShaderRadiosityProperties {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        detail_level: s.try_read_enum("detail level").unwrap_or_default(),
        power: s.read_real("power").unwrap_or(0.0),
        color_of_emitted_light: s.read_rgb("color of emitted light"),
        tint_color: s.read_rgb("tint color"),
    }
}

pub(crate) fn read_physics(root: &TagStruct<'_>) -> ShaderPhysicsProperties {
    let Some(s) = root.descend("physics") else { return Default::default() };
    ShaderPhysicsProperties {
        material_type: s.try_read_enum("material type").unwrap_or_default(),
        type_value: s.read_int_any("type").unwrap_or(0) as i16,
    }
}

/// Read perpendicular/parallel brightness + tint from a
/// `shader_specular_properties_struct` view.
pub(crate) fn read_specular_properties(s: &TagStruct<'_>) -> ShaderSpecularProperties {
    ShaderSpecularProperties {
        perpendicular_brightness: s.read_real("perpendicular brightness").unwrap_or(0.0),
        perpendicular_tint_color: s.read_rgb("perpendicular tint color"),
        parallel_brightness: s.read_real("parallel brightness").unwrap_or(0.0),
        parallel_tint_color: s.read_rgb("parallel tint color"),
    }
}
