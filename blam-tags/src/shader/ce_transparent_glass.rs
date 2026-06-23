//! Schema-faithful walker for the Halo CE `shader_transparent_glass` (sgla)
//! tag — mirrors `definitions/haloce_mcc/shader_transparent_glass.json` 1:1.
//!
//! `shader_transparent_glass` is the classic CE glass/window shader: a
//! background tint map (multiplicative tint behind the glass) + a reflection
//! cube map (bumped / flat / dynamic-mirror) with perpendicular/parallel
//! fresnel properties + a bump map, plus a diffuse map (with diffuse detail)
//! and a specular map (with specular detail) layered over the surface.

use crate::api::TagStruct;
use crate::file::TagFile;

use super::ce_common::{
    read_physics, read_radiosity, read_specular_properties, tag_enum, ShaderPhysicsProperties,
    ShaderRadiosityProperties, ShaderSpecularProperties,
};
use super::{read_tag_ref, ShaderError, TagRef};

use crate::typed_enums::{Enum, Flags};

const GROUP_SGLA: u32 = u32::from_be_bytes(*b"sgla");

// =============================================================================
// Typed enums / flags
// =============================================================================

tag_enum!(/// `shader_transparent_glass_flags_flags` (word_flags) on the base struct.
    ShaderTransparentGlassFlags: u16 {
    AlphaTested = 0 => "alpha tested",
    Decal = 1 => "decal",
    TwoSided = 2 => "two sided",
    BumpMapIsSpecularMask = 3 => "bump map is specular mask",
});

tag_enum!(/// `reflection_type_enum` — how the reflection cube map is sampled.
    ReflectionType: i16 {
    BumpedCubeMap = 0 => "bumped cube map",
    FlatCubeMap = 1 => "flat cube map",
    DynamicMirror = 2 => "dynamic mirror",
});

// =============================================================================
// Schema structs
// =============================================================================

/// `shader_transparent_glass_base_struct` — top-level glass flags.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlassBase {
    pub shader_transparent_glass_flags: Flags<ShaderTransparentGlassFlags, u16>,
}

/// `shader_transparent_glass_background_tint_struct` — multiplicative tint
/// applied behind the glass surface.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlassBackgroundTint {
    pub background_tint_color: crate::math::RealRgbColor,
    pub background_tint_map_scale: f32,
    pub background_tint_map: TagRef,
}

/// `shader_transparent_glass_reflection_struct` — reflection cube map +
/// fresnel reflection properties + bump map.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlassReflection {
    pub reflection_type: Enum<ReflectionType, i16>,
    pub reflection_map_properties: ShaderSpecularProperties,
    pub reflection_map: TagRef,
    pub bump_map_scale: f32,
    pub bump_map: TagRef,
}

/// `shader_transparent_glass_diffuse_struct` — diffuse map + diffuse detail.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlassDiffuse {
    pub diffuse_map_scale: f32,
    pub diffuse_map: TagRef,
    pub diffuse_detail_map_scale: f32,
    pub diffuse_detail_map: TagRef,
}

/// `shader_transparent_glass_specular_struct` — specular map + specular detail.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlassSpecular {
    pub specular_map_scale: f32,
    pub specular_map: TagRef,
    pub specular_detail_map_scale: f32,
    pub specular_detail_map: TagRef,
}

/// `shader_transparent_glass_block_struct` — the whole CE
/// `shader_transparent_glass` tag.
#[derive(Debug, Clone, Default)]
pub struct ShaderTransparentGlass {
    pub radiosity: ShaderRadiosityProperties,
    pub physics: ShaderPhysicsProperties,
    pub properties: ShaderTransparentGlassBase,
    pub background_tint: ShaderTransparentGlassBackgroundTint,
    pub reflection: ShaderTransparentGlassReflection,
    pub diffuse: ShaderTransparentGlassDiffuse,
    pub specular: ShaderTransparentGlassSpecular,
}

// =============================================================================
// Walker
// =============================================================================

impl ShaderTransparentGlass {
    /// Walk a parsed CE `shader_transparent_glass` (sgla) tag.
    pub fn from_tag(tag: &TagFile) -> Result<Self, ShaderError> {
        let found = tag.group().tag;
        if found != GROUP_SGLA {
            return Err(ShaderError::WrongGroup { expected: GROUP_SGLA, found });
        }
        let root = tag.root();
        Ok(Self {
            radiosity: read_radiosity(&root),
            physics: read_physics(&root),
            properties: read_properties(&root),
            background_tint: read_background_tint(&root),
            reflection: read_reflection(&root),
            diffuse: read_diffuse(&root),
            specular: read_specular(&root),
        })
    }
}

fn read_properties(root: &TagStruct<'_>) -> ShaderTransparentGlassBase {
    let Some(s) = root.descend("properties") else { return Default::default() };
    ShaderTransparentGlassBase {
        shader_transparent_glass_flags: s
            .try_read_flags("shader transparent glass flags")
            .unwrap_or_default(),
    }
}

fn read_background_tint(root: &TagStruct<'_>) -> ShaderTransparentGlassBackgroundTint {
    let Some(s) = root.descend("background tint") else { return Default::default() };
    ShaderTransparentGlassBackgroundTint {
        background_tint_color: s.read_rgb("background tint color"),
        background_tint_map_scale: s.read_real("background tint map scale").unwrap_or(0.0),
        background_tint_map: read_tag_ref(&s, "background tint map"),
    }
}

fn read_reflection(root: &TagStruct<'_>) -> ShaderTransparentGlassReflection {
    let Some(s) = root.descend("reflection") else { return Default::default() };
    ShaderTransparentGlassReflection {
        reflection_type: s.try_read_enum("reflection type").unwrap_or_default(),
        reflection_map_properties: s
            .descend("reflection map properties")
            .map(|p| read_specular_properties(&p))
            .unwrap_or_default(),
        reflection_map: read_tag_ref(&s, "reflection map"),
        bump_map_scale: s.read_real("bump map scale").unwrap_or(0.0),
        bump_map: read_tag_ref(&s, "bump map"),
    }
}

fn read_diffuse(root: &TagStruct<'_>) -> ShaderTransparentGlassDiffuse {
    let Some(s) = root.descend("diffuse") else { return Default::default() };
    ShaderTransparentGlassDiffuse {
        diffuse_map_scale: s.read_real("diffuse map scale").unwrap_or(0.0),
        diffuse_map: read_tag_ref(&s, "diffuse map"),
        diffuse_detail_map_scale: s.read_real("diffuse detail map scale").unwrap_or(0.0),
        diffuse_detail_map: read_tag_ref(&s, "diffuse detail map"),
    }
}

fn read_specular(root: &TagStruct<'_>) -> ShaderTransparentGlassSpecular {
    let Some(s) = root.descend("specular") else { return Default::default() };
    ShaderTransparentGlassSpecular {
        specular_map_scale: s.read_real("specular map scale").unwrap_or(0.0),
        specular_map: read_tag_ref(&s, "specular map"),
        specular_detail_map_scale: s.read_real("specular detail map scale").unwrap_or(0.0),
        specular_detail_map: read_tag_ref(&s, "specular detail map"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opt-in: walk a real CE glass shader and check key fields.
    /// Skips silently when the tag isn't present.
    #[test]
    fn walks_glass() {
        let path =
            "/Users/camden/Halo/haloce_mcc/tags/scenarios/bitmaps/solo/a30/glass.shader_transparent_glass";
        let Ok(tag) = TagFile::read(path) else { return };
        let g = ShaderTransparentGlass::from_tag(&tag).expect("parse sgla");
        eprintln!(
            "glass: bg_tint={:?} refl_type={:?} refl_map={:?} bump={:?} \
             diffuse={:?} specular={:?} flags={:?}",
            g.background_tint.background_tint_map.path,
            g.reflection.reflection_type,
            g.reflection.reflection_map.path,
            g.reflection.bump_map.path,
            g.diffuse.diffuse_map.path,
            g.specular.specular_map.path,
            g.properties.shader_transparent_glass_flags,
        );
    }
}
