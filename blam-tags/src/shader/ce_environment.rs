//! Schema-faithful walker for the Halo CE `shader_environment` (senv) tag —
//! mirrors `definitions/haloce_mcc/shader_environment.json` 1:1.
//!
//! `shader_environment` is the CE BSP/world surface shader: a base map plus
//! three tiers of detail (primary / secondary / micro), a bump map, a flat
//! specular color block, a reflection cube map with a per-type reflection
//! mode, an optional lens flare, and a three-channel self-illumination
//! (primary / secondary / plasma) animator.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealPoint2d, RealRgbColor};
use crate::typed_enums::{Enum, Flags};

use super::ce_common::{
    read_physics, read_radiosity, tag_enum, AnimationFunction, ShaderPhysicsProperties,
    ShaderRadiosityProperties,
};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SENV: u32 = u32::from_be_bytes(*b"senv");

// =============================================================================
// senv-specific typed enums / flags
//
// Shared enums (`flags_flags` -> RadiosityFlags, `material_type_enum` ->
// MaterialType, `u_animation_function_enum` -> AnimationFunction, and the
// `detail_level_enum` carried inside ShaderRadiosityProperties) come from
// `ce_common`; everything below is unique to `shader_environment`.
// =============================================================================

tag_enum!(/// `flags_flags_2` (word_flags) on `shader_environment_properties`.
    ShaderEnvironmentFlags: u16 {
    AlphaTested = 0 => "alpha tested",
    BumpMapIsSpecularMask = 1 => "bump map is specular mask",
    TrueAtmosphericFog = 2 => "true atmospheric fog",
    UseAlternateBumpAttenuation = 3 => "use alternate bump attenuation",
});

tag_enum!(/// `shader_environment_type_enum` — how the base map composites.
    ShaderEnvironmentType: i16 {
    Normal = 0 => "normal",
    Blended = 1 => "blended",
    BlendedBaseSpecular = 2 => "blended base specular",
});

tag_enum!(/// `flags_flags_3` (word_flags) on `shader_environment_diffuse`.
    DiffuseFlags: u16 {
    RescaleDetailMaps = 0 => "rescale detail maps",
    RescaleBumpMap = 1 => "rescale bump map",
});

tag_enum!(/// `detail_map_function_enum` — how a detail map blends with the base.
    DetailMapFunction: i16 {
    DoubleBiasedMultiply = 0 => "double biased multiply",
    Multiply = 1 => "multiply",
    DoubleBiasedAdd = 2 => "double biased add",
});

tag_enum!(/// `flags_flags_4` (word_flags) on `shader_environment_self_illumination`.
    SelfIlluminationFlags: u16 {
    Unfiltered = 0 => "unfiltered",
});

tag_enum!(/// `flags_flags_5` (word_flags) on `shader_environment_specular`.
    SpecularFlags: u16 {
    Overbright = 0 => "overbright",
    ExtraShiny = 1 => "extra shiny",
    LightmapIsSpecular = 2 => "lightmap is specular",
});

tag_enum!(/// `flags_flags_6` (word_flags) on `shader_environment_reflection`.
    ReflectionFlags: u16 {
    DynamicMirror = 0 => "dynamic mirror",
});

tag_enum!(/// `type_enum` — reflection sampling mode.
    ReflectionType: i16 {
    BumpedCubeMap = 0 => "bumped cube map",
    FlatCubeMap = 1 => "flat cube map",
    BumpedRadiosity = 2 => "bumped radiosity",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_environment_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentProperties {
    pub flags: Flags<ShaderEnvironmentFlags, u16>,
    pub shader_environment_type: Enum<ShaderEnvironmentType, i16>,
}

/// `shader_lens_flares_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderLensFlares {
    pub lens_flare_spacing: f32,
    pub lens_flare: TagRef,
}

/// `shader_environment_diffuse_struct` — base map + the three detail tiers.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentDiffuse {
    pub flags: Flags<DiffuseFlags, u16>,
    pub base_map: TagRef,
    pub detail_map_function: Enum<DetailMapFunction, i16>,
    pub primary_detail_map_scale: f32,
    pub primary_detail_map: TagRef,
    pub secondary_detail_map_scale: f32,
    pub secondary_detail_map: TagRef,
    pub micro_detail_map_function: Enum<DetailMapFunction, i16>,
    pub micro_detail_map_scale: f32,
    pub micro_detail_map: TagRef,
    pub material_color: RealRgbColor,
}

/// `shader_environment_bump_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentBump {
    pub bump_map_scale: f32,
    pub bump_map: TagRef,
    pub bump_map_scale_xy: RealPoint2d,
}

/// `shader_environment_texture_scrolling_animation_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentTextureScrollingAnimation {
    pub u_animation_function: Enum<AnimationFunction, i16>,
    pub u_animation_period: f32,
    pub u_animation_scale: f32,
    pub v_animation_function: Enum<AnimationFunction, i16>,
    pub v_animation_period: f32,
    pub v_animation_scale: f32,
}

/// `shader_environment_self_illumination_struct` — primary / secondary / plasma
/// self-illumination channels plus a shared map.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentSelfIllumination {
    pub flags: Flags<SelfIlluminationFlags, u16>,
    pub primary_on_color: RealRgbColor,
    pub primary_off_color: RealRgbColor,
    pub primary_animation_function: Enum<AnimationFunction, i16>,
    pub primary_animation_period: f32,
    pub primary_animation_phase: f32,
    pub secondary_on_color: RealRgbColor,
    pub secondary_off_color: RealRgbColor,
    pub secondary_animation_function: Enum<AnimationFunction, i16>,
    pub secondary_animation_period: f32,
    pub secondary_animation_phase: f32,
    pub plasma_on_color: RealRgbColor,
    pub plasma_off_color: RealRgbColor,
    pub plasma_animation_function: Enum<AnimationFunction, i16>,
    pub plasma_animation_period: f32,
    pub plasma_animation_phase: f32,
    pub map_scale: f32,
    pub map: TagRef,
}

/// `shader_environment_specular_struct` — flat specular color block. (This is
/// senv's own specular struct, distinct from the shared
/// `shader_specular_properties_struct` reused by other CE shaders.)
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentSpecular {
    pub flags: Flags<SpecularFlags, u16>,
    pub brightness: f32,
    pub perpendicular_color: RealRgbColor,
    pub parallel_color: RealRgbColor,
}

/// `shader_environment_reflection_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironmentReflection {
    pub flags: Flags<ReflectionFlags, u16>,
    /// Schema field `type` (`type_enum`).
    pub type_value: Enum<ReflectionType, i16>,
    pub lightmap_brightness_scale: f32,
    pub perpendicular_brightness: f32,
    pub parallel_brightness: f32,
    pub reflection_cube_map: TagRef,
}

/// `shader_environment_block_struct` — the whole CE `shader_environment` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderEnvironment {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderEnvironmentProperties,
    pub lens_flares: ShaderLensFlares,
    pub diffuse: ShaderEnvironmentDiffuse,
    pub bump: ShaderEnvironmentBump,
    pub texture_scrolling_animation: ShaderEnvironmentTextureScrollingAnimation,
    pub self_illumination: ShaderEnvironmentSelfIllumination,
    pub specular: ShaderEnvironmentSpecular,
    pub reflection: ShaderEnvironmentReflection,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderEnvironment {
    /// Walk a parsed CE `shader_environment` (senv) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SENV {
            return Err(ShaderError::WrongGroup { expected: GROUP_SENV, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            lens_flares: read_lens_flares(&root),
            diffuse: read_diffuse(&root),
            bump: read_bump(&root),
            texture_scrolling_animation: read_tsa(&root),
            self_illumination: read_self_illumination(&root),
            specular: read_specular(&root),
            reflection: read_reflection(&root),
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderEnvironmentProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    ShaderEnvironmentProperties {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        shader_environment_type: s.try_read_enum("shader environment type").unwrap_or_default(),
    }
}

fn read_lens_flares(root: &TagStruct<'_>) -> ShaderLensFlares {
    let Some(s) = root.descend("lens flares") else { return Default::default() };
    ShaderLensFlares {
        lens_flare_spacing: s.read_real("lens flare spacing").unwrap_or(0.0),
        lens_flare: read_tag_ref(&s, "lens flare"),
    }
}

fn read_diffuse(root: &TagStruct<'_>) -> ShaderEnvironmentDiffuse {
    let Some(s) = root.descend("diffuse") else { return Default::default() };
    ShaderEnvironmentDiffuse {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        base_map: read_tag_ref(&s, "base map"),
        detail_map_function: s.try_read_enum("detail map function").unwrap_or_default(),
        primary_detail_map_scale: s.read_real("primary detail map scale").unwrap_or(0.0),
        primary_detail_map: read_tag_ref(&s, "primary detail map"),
        secondary_detail_map_scale: s.read_real("secondary detail map scale").unwrap_or(0.0),
        secondary_detail_map: read_tag_ref(&s, "secondary detail map"),
        micro_detail_map_function: s.try_read_enum("micro detail map function").unwrap_or_default(),
        micro_detail_map_scale: s.read_real("micro detail map scale").unwrap_or(0.0),
        micro_detail_map: read_tag_ref(&s, "micro detail map"),
        material_color: s.read_rgb("material color"),
    }
}

fn read_bump(root: &TagStruct<'_>) -> ShaderEnvironmentBump {
    let Some(s) = root.descend("bump") else { return Default::default() };
    ShaderEnvironmentBump {
        bump_map_scale: s.read_real("bump map scale").unwrap_or(0.0),
        bump_map: read_tag_ref(&s, "bump map"),
        bump_map_scale_xy: s.read_point2d("bump map scale xy"),
    }
}

fn read_tsa(root: &TagStruct<'_>) -> ShaderEnvironmentTextureScrollingAnimation {
    let Some(s) = root.descend("texture scrolling animation") else { return Default::default() };
    ShaderEnvironmentTextureScrollingAnimation {
        u_animation_function: s.try_read_enum("u animation function").unwrap_or_default(),
        u_animation_period: s.read_real("u animation period").unwrap_or(0.0),
        u_animation_scale: s.read_real("u animation scale").unwrap_or(0.0),
        v_animation_function: s.try_read_enum("v animation function").unwrap_or_default(),
        v_animation_period: s.read_real("v animation period").unwrap_or(0.0),
        v_animation_scale: s.read_real("v animation scale").unwrap_or(0.0),
    }
}

fn read_self_illumination(root: &TagStruct<'_>) -> ShaderEnvironmentSelfIllumination {
    let Some(s) = root.descend("self-illumination") else { return Default::default() };
    ShaderEnvironmentSelfIllumination {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        primary_on_color: s.read_rgb("primary on color"),
        primary_off_color: s.read_rgb("primary off color"),
        primary_animation_function: s.try_read_enum("primary animation function").unwrap_or_default(),
        primary_animation_period: s.read_real("primary animation period").unwrap_or(0.0),
        primary_animation_phase: s.read_real("primary animation phase").unwrap_or(0.0),
        secondary_on_color: s.read_rgb("secondary on color"),
        secondary_off_color: s.read_rgb("secondary off color"),
        secondary_animation_function: s.try_read_enum("secondary animation function").unwrap_or_default(),
        secondary_animation_period: s.read_real("secondary animation period").unwrap_or(0.0),
        secondary_animation_phase: s.read_real("secondary animation phase").unwrap_or(0.0),
        plasma_on_color: s.read_rgb("plasma on color"),
        plasma_off_color: s.read_rgb("plasma off color"),
        plasma_animation_function: s.try_read_enum("plasma animation function").unwrap_or_default(),
        plasma_animation_period: s.read_real("plasma animation period").unwrap_or(0.0),
        plasma_animation_phase: s.read_real("plasma animation phase").unwrap_or(0.0),
        map_scale: s.read_real("map scale").unwrap_or(0.0),
        map: read_tag_ref(&s, "map"),
    }
}

fn read_specular(root: &TagStruct<'_>) -> ShaderEnvironmentSpecular {
    let Some(s) = root.descend("specular") else { return Default::default() };
    ShaderEnvironmentSpecular {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        brightness: s.read_real("brightness").unwrap_or(0.0),
        perpendicular_color: s.read_rgb("perpendicular color"),
        parallel_color: s.read_rgb("parallel color"),
    }
}

fn read_reflection(root: &TagStruct<'_>) -> ShaderEnvironmentReflection {
    let Some(s) = root.descend("reflection") else { return Default::default() };
    ShaderEnvironmentReflection {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        type_value: s.try_read_enum("type").unwrap_or_default(),
        lightmap_brightness_scale: s.read_real("lightmap brightness scale").unwrap_or(0.0),
        perpendicular_brightness: s.read_real("perpendicular brightness").unwrap_or(0.0),
        parallel_brightness: s.read_real("parallel brightness").unwrap_or(0.0),
        reflection_cube_map: read_tag_ref(&s, "reflection cube map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE BSP shader_environment and print key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_a_shader_environment() {
        let path =
            "/Users/camden/Halo/haloce_mcc/tags/levels/a10/shaders/a10_floor.shader_environment";
        let Ok(tag) = TagFile::read(path) else { return };
        let se = ShaderEnvironment::from_tag(&tag).expect("parse senv");
        eprintln!(
            "senv: base={:?} primary_detail={:?} micro_detail={:?} bump={:?} \
             cube={:?} type={:?} refl_type={:?} env_type={:?}",
            se.diffuse.base_map.path,
            se.diffuse.primary_detail_map.path,
            se.diffuse.micro_detail_map.path,
            se.bump.bump_map.path,
            se.reflection.reflection_cube_map.path,
            se.properties.shader_environment_type,
            se.reflection.type_value,
            se.properties.shader_environment_type,
        );
    }
}
