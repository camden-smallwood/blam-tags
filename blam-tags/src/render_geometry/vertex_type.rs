//! [`MeshVertexType`] — a stable Rust enum covering every variant
//! the schema's `mesh_vertex_type_definition` enum (and its
//! cross-game cousins) declares.
//!
//! Different games / builds assign different integer indices to the
//! same logical format, but the option *names* are stable. The
//! canonical way to resolve a mesh's vertex type is:
//!
//! 1. Read the integer enum value off the mesh (`mesh.vertex_type`).
//! 2. Look up that value in the layout's enum-option string table
//!    (handled by [`crate::api::TagStruct::read_enum_name`]).
//! 3. Pass the resolved string to [`MeshVertexType::from_schema_name`].
//!
//! Step #2 is what makes the resolution robust to per-build index
//! shuffling: we always go through the schema's name table.
//!
//! Strides recorded here mirror the engine's per-format vertex
//! stride (TagTool's `VertexBufferFormat` size comments plus what
//! we've observed on H4 X360 monolithic dumps). They're advisory —
//! the on-disk vertex buffer descriptor's `stride` field is
//! authoritative.

/// Every mesh vertex type the engine knows about. Variant names use
/// `UpperCamelCase`; the original schema names (which mix space,
/// snake-case, and CamelCase) are recognized by
/// [`Self::from_schema_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshVertexType {
    World,
    Rigid,
    Skinned,
    ParticleModel,
    FlatWorld,
    FlatRigid,
    FlatSkinned,
    Screen,
    Debug,
    Transparent,
    Particle,
    Unused0,
    LightVolume,
    ChudSimple,
    ChudFancy,
    Decorator,
    PositionOnly,
    PatchyFog,
    Water,
    Ripple,
    ImplicitGeometry,
    Unused1,
    WorldTessellated,
    RigidTessellated,
    SkinnedTessellated,
    ShaderCache,
    StructureInstanceImposter,
    ObjectInstanceImposter,
    /// "rigid compressed" — Halo 4's tightly-packed 16-byte rigid
    /// vertex (position + normal + tangent + uv in compressed form).
    /// Dominant format in the H4 X360 corpus.
    RigidCompressed,
    /// "skinned compressed" -- a `RigidCompressed` vertex with four
    /// bone indices and four weights appended, 24 bytes. Reach's
    /// vehicles and bipeds use it where MCC's own tags use `skinned`.
    /// Measured, not derived: the build's own buffer descriptors say 24.
    SkinnedCompressed,
    SkinnedUncompressed,
    LightVolumePrecompiled,
    BlendshapeRigid,
    BlendshapeRigidBlendshaped,
    RigidBlendshaped,
    BlendshapeSkinned,
    BlendshapeSkinnedBlendshaped,
    SkinnedBlendshaped,
    VirtualGeometryHwTess,
    VirtualGeometryMemexport,
    /// Some schemas spell this `position_only` (with underscore);
    /// distinct from [`Self::PositionOnly`] only by string name.
    PositionOnlyAlt,
    VirtualGeometryDebug,
    BlendshapeRigidCompressed,
    SkinnedUncompressedBlendshaped,
    BlendshapeSkinnedCompressed,
    Tracer,
    Polyart,
    Vectorart,
    RigidBoned,
    RigidBoned2Uv,
    BlendshapeSkinned2Uv,
    BlendshapeSkinned2UvBlendshaped,
    Skinned2UvBlendshaped,
    PolyartUv,
    BlendshapeSkinnedUncompressedBlendshaped,
    Bink,
}

impl MeshVertexType {
    /// Resolve the schema option name (as it appears in
    /// `mesh_vertex_type_definition`'s `options` list) to a variant.
    /// Returns `None` for unknown names so callers can record them
    /// for future addition rather than crash.
    pub fn from_schema_name(name: &str) -> Option<Self> {
        Some(match name {
            "world" => Self::World,
            "rigid" => Self::Rigid,
            "skinned" => Self::Skinned,
            "particle_model" => Self::ParticleModel,
            "flat world" => Self::FlatWorld,
            "flat rigid" => Self::FlatRigid,
            "flat skinned" => Self::FlatSkinned,
            "screen" => Self::Screen,
            "debug" => Self::Debug,
            "transparent" => Self::Transparent,
            "particle" => Self::Particle,
            "unused0" => Self::Unused0,
            "light_volume" => Self::LightVolume,
            "chud_simple" => Self::ChudSimple,
            "chud_fancy" => Self::ChudFancy,
            "decorator" => Self::Decorator,
            "position only" => Self::PositionOnly,
            "patchy_fog" => Self::PatchyFog,
            "water" => Self::Water,
            "ripple" => Self::Ripple,
            "implicit geometry" => Self::ImplicitGeometry,
            "unused1" => Self::Unused1,
            "world_tessellated" => Self::WorldTessellated,
            "rigid_tessellated" => Self::RigidTessellated,
            "skinned_tessellated" => Self::SkinnedTessellated,
            "shader_cache" => Self::ShaderCache,
            "structure_instance_imposter" => Self::StructureInstanceImposter,
            "object_instance_imposter" => Self::ObjectInstanceImposter,
            "rigid compressed" => Self::RigidCompressed,
            "skinned compressed" => Self::SkinnedCompressed,
            "skinned uncompressed" => Self::SkinnedUncompressed,
            "light volume precompiled" => Self::LightVolumePrecompiled,
            "blendshape_rigid" => Self::BlendshapeRigid,
            "blendshape_rigid_blendshaped" => Self::BlendshapeRigidBlendshaped,
            "rigid_blendshaped" => Self::RigidBlendshaped,
            "blendshape_skinned" => Self::BlendshapeSkinned,
            "blendshape_skinned_blendshaped" => Self::BlendshapeSkinnedBlendshaped,
            "skinned_blendshaped" => Self::SkinnedBlendshaped,
            "VirtualGeometryHWtess" => Self::VirtualGeometryHwTess,
            "VirtualGeometryMemexport" => Self::VirtualGeometryMemexport,
            "position_only" => Self::PositionOnlyAlt,
            "VirtualGeometryDebug" => Self::VirtualGeometryDebug,
            "blendshapeRigidCompressed" => Self::BlendshapeRigidCompressed,
            "skinnedUncompressedBlendshaped" => Self::SkinnedUncompressedBlendshaped,
            "blendshapeSkinnedCompressed" => Self::BlendshapeSkinnedCompressed,
            "tracer" => Self::Tracer,
            "polyart" => Self::Polyart,
            "vectorart" => Self::Vectorart,
            "rigid_boned" => Self::RigidBoned,
            "rigid_boned_2uv" => Self::RigidBoned2Uv,
            "blendshape_skinned_2uv" => Self::BlendshapeSkinned2Uv,
            "blendshape_skinned_2uv_blendshaped" => Self::BlendshapeSkinned2UvBlendshaped,
            "skinned_2uv_blendshaped" => Self::Skinned2UvBlendshaped,
            "polyartUV" => Self::PolyartUv,
            "blendshape_skinned_uncompressed_blendshaped" => Self::BlendshapeSkinnedUncompressedBlendshaped,
            "bink" => Self::Bink,
            _ => return None,
        })
    }

    /// The schema option name string this variant resolves from.
    /// Inverse of [`Self::from_schema_name`].
    pub fn schema_name(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Rigid => "rigid",
            Self::Skinned => "skinned",
            Self::ParticleModel => "particle_model",
            Self::FlatWorld => "flat world",
            Self::FlatRigid => "flat rigid",
            Self::FlatSkinned => "flat skinned",
            Self::Screen => "screen",
            Self::Debug => "debug",
            Self::Transparent => "transparent",
            Self::Particle => "particle",
            Self::Unused0 => "unused0",
            Self::LightVolume => "light_volume",
            Self::ChudSimple => "chud_simple",
            Self::ChudFancy => "chud_fancy",
            Self::Decorator => "decorator",
            Self::PositionOnly => "position only",
            Self::PatchyFog => "patchy_fog",
            Self::Water => "water",
            Self::Ripple => "ripple",
            Self::ImplicitGeometry => "implicit geometry",
            Self::Unused1 => "unused1",
            Self::WorldTessellated => "world_tessellated",
            Self::RigidTessellated => "rigid_tessellated",
            Self::SkinnedTessellated => "skinned_tessellated",
            Self::ShaderCache => "shader_cache",
            Self::StructureInstanceImposter => "structure_instance_imposter",
            Self::ObjectInstanceImposter => "object_instance_imposter",
            Self::RigidCompressed => "rigid compressed",
            Self::SkinnedCompressed => "skinned compressed",
            Self::SkinnedUncompressed => "skinned uncompressed",
            Self::LightVolumePrecompiled => "light volume precompiled",
            Self::BlendshapeRigid => "blendshape_rigid",
            Self::BlendshapeRigidBlendshaped => "blendshape_rigid_blendshaped",
            Self::RigidBlendshaped => "rigid_blendshaped",
            Self::BlendshapeSkinned => "blendshape_skinned",
            Self::BlendshapeSkinnedBlendshaped => "blendshape_skinned_blendshaped",
            Self::SkinnedBlendshaped => "skinned_blendshaped",
            Self::VirtualGeometryHwTess => "VirtualGeometryHWtess",
            Self::VirtualGeometryMemexport => "VirtualGeometryMemexport",
            Self::PositionOnlyAlt => "position_only",
            Self::VirtualGeometryDebug => "VirtualGeometryDebug",
            Self::BlendshapeRigidCompressed => "blendshapeRigidCompressed",
            Self::SkinnedUncompressedBlendshaped => "skinnedUncompressedBlendshaped",
            Self::BlendshapeSkinnedCompressed => "blendshapeSkinnedCompressed",
            Self::Tracer => "tracer",
            Self::Polyart => "polyart",
            Self::Vectorart => "vectorart",
            Self::RigidBoned => "rigid_boned",
            Self::RigidBoned2Uv => "rigid_boned_2uv",
            Self::BlendshapeSkinned2Uv => "blendshape_skinned_2uv",
            Self::BlendshapeSkinned2UvBlendshaped => "blendshape_skinned_2uv_blendshaped",
            Self::Skinned2UvBlendshaped => "skinned_2uv_blendshaped",
            Self::PolyartUv => "polyartUV",
            Self::BlendshapeSkinnedUncompressedBlendshaped => "blendshape_skinned_uncompressed_blendshaped",
            Self::Bink => "bink",
        }
    }
}

/// PRT vertex sub-stream type — separate from
/// [`MeshVertexType`] because the engine ships PRT data in its own
/// vertex buffer slot. Mirrors the schema's
/// `mesh_transfer_vertex_type_definition` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeshPrtVertexType {
    NoPrt,
    PrtAmbient,
    PrtLinear,
    PrtQuadratic,
}

impl MeshPrtVertexType {
    pub fn from_schema_name(name: &str) -> Option<Self> {
        Some(match name {
            "No PRT" => Self::NoPrt,
            "PRT Ambient" => Self::PrtAmbient,
            "PRT Linear" => Self::PrtLinear,
            "PRT Quadratic" => Self::PrtQuadratic,
            _ => return None,
        })
    }

    pub fn schema_name(self) -> &'static str {
        match self {
            Self::NoPrt => "No PRT",
            Self::PrtAmbient => "PRT Ambient",
            Self::PrtLinear => "PRT Linear",
            Self::PrtQuadratic => "PRT Quadratic",
        }
    }

    /// Per-vertex stride in bytes for the PRT data buffer.
    /// `0` for `NoPrt` (no buffer slot used).
    pub fn stride(self) -> u32 {
        match self {
            Self::NoPrt => 0,
            Self::PrtAmbient | Self::PrtLinear => 4,
            Self::PrtQuadratic => 36,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rigid_compressed_round_trips_through_schema_name() {
        let name = MeshVertexType::RigidCompressed.schema_name();
        assert_eq!(name, "rigid compressed");
        assert_eq!(MeshVertexType::from_schema_name(name), Some(MeshVertexType::RigidCompressed));
    }

    #[test]
    fn position_only_vs_position_only_alt_are_distinct() {
        assert_eq!(MeshVertexType::from_schema_name("position only"), Some(MeshVertexType::PositionOnly));
        assert_eq!(MeshVertexType::from_schema_name("position_only"), Some(MeshVertexType::PositionOnlyAlt));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(MeshVertexType::from_schema_name("totally fake"), None);
    }

    #[test]
    fn prt_quadratic_stride_is_36() {
        assert_eq!(MeshPrtVertexType::PrtQuadratic.stride(), 36);
    }
}
