//! Schema-faithful walker for the Halo CE `shader_model` (soso) tag —
//! mirrors `definitions/haloce_mcc/shader_model.json` 1:1.
//!
//! `shader_model` is the dominant CE object/character shader: diffuse +
//! a packed `multipurpose map` (reflection/self-illum/change-color/aux
//! masks) + detail + a flat reflection cube. It has no bump map.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealPoint2d, RealRgbColor};
use crate::typed_enums::{Enum, Flags};

use super::ce_common::{
    read_physics, read_radiosity, read_specular_properties, tag_enum, AnimationFunction,
    ChangeColorSource, SelfIlluminationFlags, ShaderPhysicsProperties, ShaderRadiosityProperties,
    ShaderSpecularProperties, TextureAnimationSource,
};
use super::{read_tag_ref, ShaderError, TagRef};

const GROUP_SOSO: u32 = u32::from_be_bytes(*b"soso");

// =============================================================================
// Typed enums / flags (model-specific)
// =============================================================================

tag_enum!(/// `flags_flags_2` (word_flags) on `shader_model_properties`.
    ShaderModelFlags: u16 {
    DetailAfterReflection = 0 => "detail after reflection",
    TwoSided = 1 => "two sided", NotAlphaTested = 2 => "not alpha tested",
    AlphaBlendedDecal = 3 => "alpha blended decal", TrueAtmosphericFog = 4 => "true atmospheric fog",
    DisableTwoSidedCulling = 5 => "disable two sided culling",
    UseXboxMultipurposeChannelOrder = 6 => "use xbox multipurpose channel order",
});

tag_enum!(/// `detail_function_enum`.
    DetailFunction: i16 {
    DoubleBiasedMultiply = 0 => "double biased multiply",
    Multiply = 1 => "multiply", DoubleBiasedAdd = 2 => "double biased add",
});

tag_enum!(/// `detail_mask_enum` — which multipurpose channel gates the detail map.
    DetailMask: i16 {
    None = 0 => "none",
    ReflectionMaskInverse = 1 => "reflection mask inverse", ReflectionMask = 2 => "reflection mask",
    SelfIlluminationMaskInverse = 3 => "self illumination mask inverse",
    SelfIlluminationMask = 4 => "self illumination mask",
    ChangeColorMaskInverse = 5 => "change color mask inverse", ChangeColorMask = 6 => "change color mask",
    AuxiliaryMaskInverse = 7 => "auxiliary mask inverse", AuxiliaryMask = 8 => "auxiliary mask",
});

// =============================================================================
// Schema structs (model-specific)
// =============================================================================

/// `shader_model_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelProperties {
    pub flags: Flags<ShaderModelFlags, u16>,
    pub translucency: f32,
}

/// `shader_model_change_color_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelChangeColor {
    pub change_color_source: Enum<ChangeColorSource, i16>,
}

/// `shader_model_self_illumination_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelSelfIllumination {
    pub flags: Flags<SelfIlluminationFlags, u16>,
    pub color_source: Enum<ChangeColorSource, i16>,
    pub animation_function: Enum<AnimationFunction, i16>,
    pub animation_period: f32,
    pub animation_color_lower_bound: RealRgbColor,
    pub animation_color_upper_bound: RealRgbColor,
}

/// `shader_model_maps_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelMaps {
    pub map_u_scale: f32,
    pub map_v_scale: f32,
    pub base_map: TagRef,
    pub multipurpose_map: TagRef,
    pub detail_function: Enum<DetailFunction, i16>,
    pub detail_mask: Enum<DetailMask, i16>,
    pub detail_map_scale: f32,
    pub detail_map: TagRef,
    pub detail_map_v_scale: f32,
}

/// `shader_model_texture_scrolling_animation_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelTextureScrollingAnimation {
    pub u_animation_source: Enum<TextureAnimationSource, i16>,
    pub u_animation_function: Enum<AnimationFunction, i16>,
    pub u_animation_period: f32,
    pub u_animation_phase: f32,
    pub u_animation_scale: f32,
    pub v_animation_source: Enum<TextureAnimationSource, i16>,
    pub v_animation_function: Enum<AnimationFunction, i16>,
    pub v_animation_period: f32,
    pub v_animation_phase: f32,
    pub v_animation_scale: f32,
    pub rotation_animation_source: Enum<TextureAnimationSource, i16>,
    pub rotation_animation_function: Enum<AnimationFunction, i16>,
    pub rotation_animation_period: f32,
    pub rotation_animation_phase: f32,
    pub rotation_animation_scale: f32,
    pub rotation_animation_center: RealPoint2d,
}

/// `shader_model_reflection_struct`.
#[derive(Debug, Clone, Default)]
pub struct ShaderModelReflection {
    pub falloff_distance: f32,
    pub cutoff_distance: f32,
    pub cube_map_properties: ShaderSpecularProperties,
    /// Flat reflection cube map.
    pub cube_map: TagRef,
}

/// `shader_model_bullshit_struct` — legacy unused real (sic).
#[derive(Debug, Clone, Default)]
pub struct ShaderModelBullshit {
    pub bullshit: f32,
}

/// `shader_model_block_struct` — the whole CE `shader_model` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderModel {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderModelProperties,
    pub change_color: ShaderModelChangeColor,
    pub self_illumination: ShaderModelSelfIllumination,
    pub maps: ShaderModelMaps,
    pub texture_scrolling_animation: ShaderModelTextureScrollingAnimation,
    pub reflection: ShaderModelReflection,
    pub bullshit: ShaderModelBullshit,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderModel {
    /// Walk a parsed CE `shader_model` (soso) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SOSO {
            return Err(ShaderError::WrongGroup { expected: GROUP_SOSO, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            change_color: read_change_color(&root),
            self_illumination: read_self_illumination(&root),
            maps: read_maps(&root),
            texture_scrolling_animation: read_tsa(&root),
            reflection: read_reflection(&root),
            bullshit: ShaderModelBullshit {
                bullshit: descend_real(&root, "bullshit", "bullshit"),
            },
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderModelProperties {
    let Some(s) = root.descend("properties") else { return Default::default() };
    ShaderModelProperties {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        translucency: s.read_real("translucency").unwrap_or(0.0),
    }
}

fn read_change_color(root: &TagStruct<'_>) -> ShaderModelChangeColor {
    let Some(s) = root.descend("change color") else { return Default::default() };
    ShaderModelChangeColor {
        change_color_source: s.try_read_enum("change color source").unwrap_or_default(),
    }
}

fn read_self_illumination(root: &TagStruct<'_>) -> ShaderModelSelfIllumination {
    let Some(s) = root.descend("self-illumination") else { return Default::default() };
    ShaderModelSelfIllumination {
        flags: s.try_read_flags("flags").unwrap_or_default(),
        color_source: s.try_read_enum("color source").unwrap_or_default(),
        animation_function: s.try_read_enum("animation function").unwrap_or_default(),
        animation_period: s.read_real("animation period").unwrap_or(0.0),
        animation_color_lower_bound: s.read_rgb("animation color lower bound"),
        animation_color_upper_bound: s.read_rgb("animation color upper bound"),
    }
}

fn read_maps(root: &TagStruct<'_>) -> ShaderModelMaps {
    let Some(s) = root.descend("maps") else { return Default::default() };
    ShaderModelMaps {
        map_u_scale: s.read_real("map u scale").unwrap_or(0.0),
        map_v_scale: s.read_real("map v scale").unwrap_or(0.0),
        base_map: read_tag_ref(&s, "base map"),
        multipurpose_map: read_tag_ref(&s, "multipurpose map"),
        detail_function: s.try_read_enum("detail function").unwrap_or_default(),
        detail_mask: s.try_read_enum("detail mask").unwrap_or_default(),
        detail_map_scale: s.read_real("detail map scale").unwrap_or(0.0),
        detail_map: read_tag_ref(&s, "detail map"),
        detail_map_v_scale: s.read_real("detail map v scale").unwrap_or(0.0),
    }
}

fn read_tsa(root: &TagStruct<'_>) -> ShaderModelTextureScrollingAnimation {
    let Some(s) = root.descend("texture scrolling animation") else { return Default::default() };
    ShaderModelTextureScrollingAnimation {
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
        rotation_animation_source: s.try_read_enum("rotation animation source").unwrap_or_default(),
        rotation_animation_function: s.try_read_enum("rotation animation function").unwrap_or_default(),
        rotation_animation_period: s.read_real("rotation animation period").unwrap_or(0.0),
        rotation_animation_phase: s.read_real("rotation animation phase").unwrap_or(0.0),
        rotation_animation_scale: s.read_real("rotation animation scale").unwrap_or(0.0),
        rotation_animation_center: s.read_point2d("rotation animation center"),
    }
}

fn read_reflection(root: &TagStruct<'_>) -> ShaderModelReflection {
    let Some(s) = root.descend("reflection") else { return Default::default() };
    let cube_map_properties = match s.descend("cube map properties") {
        Some(c) => read_specular_properties(&c),
        None => Default::default(),
    };
    ShaderModelReflection {
        falloff_distance: s.read_real("falloff distance").unwrap_or(0.0),
        cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
        cube_map_properties,
        cube_map: read_tag_ref(&s, "cube map"),
    }
}

/// Read a `real` field out of a nested struct, defaulting to 0.
fn descend_real(root: &TagStruct<'_>, struct_name: &str, field: &str) -> f32 {
    root.descend(struct_name)
        .and_then(|s| s.read_real(field))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE cyborg shader_model and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_cyborg_armor() {
        let path = "/Users/camden/Halo/haloce_mcc/tags/characters/cyborg/shaders/armor.shader_model";
        let Ok(tag) = TagFile::read(path) else { return };
        let sm = ShaderModel::from_tag(&tag).expect("parse soso");
        eprintln!(
            "cyborg armor: base={:?} multipurpose={:?} cube={:?} props.flags={:?} \
             refl.perp_bright={} detail_fn={:?}",
            sm.maps.base_map.path,
            sm.maps.multipurpose_map.path,
            sm.reflection.cube_map.path,
            sm.properties.flags,
            sm.reflection.cube_map_properties.perpendicular_brightness,
            sm.maps.detail_function,
        );
        assert!(!sm.maps.base_map.is_null(), "base map should be populated");
        assert_eq!(sm.maps.base_map.group, u32::from_be_bytes(*b"bitm"));
    }
}
