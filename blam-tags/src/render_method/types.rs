//! Render-method runtime types — verbatim mirror of Ares
//! `source/render_methods/render_method_definitions.h`.
//!
//! See `mod.rs` for the high-level overview. This file is pure type
//! definitions plus walkers from `&TagFile` / `&TagStruct` to the
//! corresponding Rust struct.

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::math::ArgbColor;
use crate::tag_function::TagFunction;

// =============================================================================
// Errors
// =============================================================================

#[derive(Debug)]
pub enum RenderMethodError {
    /// A required field was missing from the tag — schema mismatch
    /// or empty in the instance. Carries the dotted field path.
    MissingField(&'static str),
    /// Wrong tag group — caller passed a `TagFile` whose group_tag
    /// doesn't match the constructor's expected type. Carries the
    /// expected and actual 4-byte group tags.
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for RenderMethodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(p) => write!(f, "render_method: missing required field: {p}"),
            Self::WrongGroup { expected, actual } => write!(
                f,
                "render_method: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for RenderMethodError {}

// =============================================================================
// Enums
// =============================================================================

/// Parameter data type. Mirrors Ares `e_render_method_parameter_type`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderMethodParameterType {
    Bitmap    = 0,
    Color     = 1,
    Real      = 2,
    Int       = 3,
    Bool      = 4,
    ArgbColor = 5,
}

impl RenderMethodParameterType {
    pub fn from_index(i: i128) -> Option<Self> {
        Some(match i {
            0 => Self::Bitmap,
            1 => Self::Color,
            2 => Self::Real,
            3 => Self::Int,
            4 => Self::Bool,
            5 => Self::ArgbColor,
            _ => return None,
        })
    }

    /// Schema-name lookup. Stable across MCC schema drifts where the
    /// enum-index ordering shifts between builds — MCC tags carry the
    /// author-time index but the schema-name string is invariant.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "bitmap" => Self::Bitmap,
            "value" | "real" => Self::Real,
            "color" => Self::Color,
            "switch" | "int" => Self::Int,
            "bool" => Self::Bool,
            "argb color" | "argb_color" => Self::ArgbColor,
            _ => return None,
        })
    }
}

/// Per-channel target for an animated parameter's evaluated function
/// output. Mirrors Ares `e_render_method_animated_parameter_type` (the
/// MCC tag schema only encodes the first 9; runtime extends to 15).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderMethodAnimatedParameterType {
    Value         = 0,
    Color         = 1,
    ScaleUniform  = 2,
    ScaleX        = 3,
    ScaleY        = 4,
    TranslationX  = 5,
    TranslationY  = 6,
    FrameIndex    = 7,
    Alpha         = 8,
}

impl RenderMethodAnimatedParameterType {
    pub fn from_index(i: i128) -> Option<Self> {
        Some(match i {
            0 => Self::Value,
            1 => Self::Color,
            2 => Self::ScaleUniform,
            3 => Self::ScaleX,
            4 => Self::ScaleY,
            5 => Self::TranslationX,
            6 => Self::TranslationY,
            7 => Self::FrameIndex,
            8 => Self::Alpha,
            _ => return None,
        })
    }
}

/// Engine-bound parameter source. Mirrors Ares `e_render_method_extern`
/// — 49 H3 entries. ODST/Reach add more; use that game's enum there.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderMethodExtern {
    None                                 = 0,
    TextureGlobalTargetTexaccum          = 1,
    TextureGlobalTargetNormal            = 2,
    TextureGlobalTargetZ                 = 3,
    TextureGlobalTargetShadowBuffer1     = 4,
    TextureGlobalTargetShadowBuffer2     = 5,
    TextureGlobalTargetShadowBuffer3     = 6,
    TextureGlobalTargetShadowBuffer4     = 7,
    TextureGlobalTargetTextureCamera     = 8,
    TextureGlobalTargetReflection        = 9,
    TextureGlobalTargetRefraction        = 10,
    TextureLightprobeTexture             = 11,
    TextureDominantLightIntensityMap     = 12,
    TextureUnused1                       = 13,
    TextureUnused2                       = 14,
    ObjectChangeColorPrimary             = 15,
    ObjectChangeColorSecondary           = 16,
    ObjectChangeColorTertiary            = 17,
    ObjectChangeColorQuaternary          = 18,
    ObjectChangeColorQuinary             = 19,
    ObjectEmblemColorPrimary             = 20,
    ObjectEmblemColorSecondary           = 21,
    TextureDynamicEnvironmentMap0        = 22,
    TextureDynamicEnvironmentMap1        = 23,
    TextureCookTorranceCc0236            = 24,
    TextureCookTorranceDd0236            = 25,
    TextureCookTorranceC78d78            = 26,
    LightDir0                            = 27,
    LightColor0                          = 28,
    LightDir1                            = 29,
    LightColor1                          = 30,
    LightDir2                            = 31,
    LightColor2                          = 32,
    LightDir3                            = 33,
    LightColor3                          = 34,
    TextureUnused3                       = 35,
    TextureUnused4                       = 36,
    TextureUnused5                       = 37,
    TextureDynamicLightGel0              = 38,
    FlatEnvmapMatrixX                    = 39,
    FlatEnvmapMatrixY                    = 40,
    FlatEnvmapMatrixZ                    = 41,
    DebugTint                            = 42,
    ScreenConstants                      = 43,
    ActiveCamoDistortionTexture          = 44,
    SceneLdrTexture                      = 45,
    SceneHdrTexture                      = 46,
    WaterMemoryExportAddress             = 47,
    TreeAnimationTimer                   = 48,
}

impl RenderMethodExtern {
    pub const COUNT: usize = 49;

    /// Map by index — discriminants follow the LATEST JSON schema.
    ///
    /// **Don't use this on raw on-disk values.** Halo tag schemas drift
    /// across MCC builds, and the integer in a tag is the author-time
    /// schema's index, not the latest one. For example, grunt_armor's
    /// rmop has `source extern = 14` which the in-tag `blay` resolves
    /// as "change color primary", but the latest JSON schema (and this
    /// enum) put primary at 15. Use [`Self::from_name`] instead — names
    /// are stable across schema versions and route through the tag's
    /// own embedded string list.
    ///
    /// Suitable for: enums you constructed in code, indexes from
    /// already-upgraded sources (rmt2 routing tables that the tools
    /// rebuild on save), test fixtures.
    pub fn from_index(i: i128) -> Option<Self> {
        if !(0..Self::COUNT as i128).contains(&i) {
            return None;
        }
        // Safe: enum is `repr(u32)` with sequential 0..49 discriminants.
        Some(unsafe { std::mem::transmute::<u32, Self>(i as u32) })
    }

    /// Map from the canonical on-disk extern name (the string blam-tag
    /// resolves from the tag's own `blay` chunk — e.g.,
    /// `"change color primary"`, `"light dir 0"`).
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "none"                              => Self::None,
            "texaccum target"                   => Self::TextureGlobalTargetTexaccum,
            "normal target"                     => Self::TextureGlobalTargetNormal,
            "z target"                          => Self::TextureGlobalTargetZ,
            "shadow 1 target"                   => Self::TextureGlobalTargetShadowBuffer1,
            "shadow 2 target"                   => Self::TextureGlobalTargetShadowBuffer2,
            "shadow 3 target"                   => Self::TextureGlobalTargetShadowBuffer3,
            "shadow 4 target"                   => Self::TextureGlobalTargetShadowBuffer4,
            "texture camera target"             => Self::TextureGlobalTargetTextureCamera,
            "reflection target"                 => Self::TextureGlobalTargetReflection,
            "refraction target"                 => Self::TextureGlobalTargetRefraction,
            "lightprobe texture"                => Self::TextureLightprobeTexture,
            "dominant light intensity texture"  => Self::TextureDominantLightIntensityMap,
            "unused 1" | "unused 2"             => Self::TextureUnused1,
            "change color primary"              => Self::ObjectChangeColorPrimary,
            "change color secondary"            => Self::ObjectChangeColorSecondary,
            "change color tertiary"             => Self::ObjectChangeColorTertiary,
            "change color quaternary"           => Self::ObjectChangeColorQuaternary,
            "change color quinary"              => Self::ObjectChangeColorQuinary,
            "emblem color background"           => Self::ObjectEmblemColorPrimary,
            "emblem color primary"              => Self::ObjectEmblemColorPrimary,
            "emblem color secondary"            => Self::ObjectEmblemColorSecondary,
            "dynamic environment map 1"         => Self::TextureDynamicEnvironmentMap0,
            "dynamic environment map 2"         => Self::TextureDynamicEnvironmentMap1,
            "cook torrance cc0236"              => Self::TextureCookTorranceCc0236,
            "cook torrance dd0236"              => Self::TextureCookTorranceDd0236,
            "cook torrance c78d78"              => Self::TextureCookTorranceC78d78,
            "light dir 0"                       => Self::LightDir0,
            "light color 0"                     => Self::LightColor0,
            "light dir 1"                       => Self::LightDir1,
            "light color 1"                     => Self::LightColor1,
            "light dir 2"                       => Self::LightDir2,
            "light color 2"                     => Self::LightColor2,
            "light dir 3"                       => Self::LightDir3,
            "light color 3"                     => Self::LightColor3,
            "unused 3"                          => Self::TextureUnused3,
            "unused 4"                          => Self::TextureUnused4,
            "unused 5"                          => Self::TextureUnused5,
            "dynamic light gel 0"               => Self::TextureDynamicLightGel0,
            "flat envmap matrix x"              => Self::FlatEnvmapMatrixX,
            "flat envmap matrix y"              => Self::FlatEnvmapMatrixY,
            "flat envmap matrix z"              => Self::FlatEnvmapMatrixZ,
            "debug tint"                        => Self::DebugTint,
            "screen constants"                  => Self::ScreenConstants,
            "active camo distortion texture"    => Self::ActiveCamoDistortionTexture,
            "scene ldr texture"                 => Self::SceneLdrTexture,
            "scene hdr texture"                 => Self::SceneHdrTexture,
            "water memexport addr"              => Self::WaterMemoryExportAddress,
            "tree animation timer"              => Self::TreeAnimationTimer,
            _ => return None,
        })
    }
}

/// Rendering pass entry point. Mirrors Ares `e_entry_point` (18 H3
/// values — schema spells some with spaces, e.g., "vertex color
/// lighting").
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryPoint {
    Default                    = 0,
    Albedo                     = 1,
    StaticLightingDefault      = 2,
    StaticLightingPerPixel     = 3,
    StaticLightingPerVertex    = 4,
    StaticLightingSh           = 5,
    StaticLightingPrtAmbient   = 6,
    StaticLightingPrtLinear    = 7,
    StaticLightingPrtQuadratic = 8,
    DynamicLighting            = 9,
    ShadowGenerate             = 10,
    ShadowApply                = 11,
    ActiveCamo                 = 12,
    LightmapDebugMode          = 13,
    VertexColorLighting        = 14,
    WaterTessellation          = 15,
    WaterShading               = 16,
    DynamicLightingCinematic   = 17,
}

impl EntryPoint {
    pub const COUNT: usize = 18;

    pub fn from_index(i: i128) -> Option<Self> {
        if !(0..Self::COUNT as i128).contains(&i) {
            return None;
        }
        Some(unsafe { std::mem::transmute::<u32, Self>(i as u32) })
    }
}

/// Vertex stream layout. Mirrors Ares `e_vertex_type` (22 H3 values).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertexType {
    World            = 0,
    Rigid            = 1,
    Skinned          = 2,
    ParticleModel    = 3,
    FlatWorld        = 4,
    FlatRigid        = 5,
    FlatSkinned      = 6,
    Screen           = 7,
    Debug            = 8,
    Transparent      = 9,
    Particle         = 10,
    Contrail         = 11,
    LightVolume      = 12,
    ChudSimple       = 13,
    ChudFancy        = 14,
    Decorator        = 15,
    TinyPositionOnly = 16,
    PatchyFog        = 17,
    Water            = 18,
    Ripple           = 19,
    ImplicitGeometry = 20,
    Beam             = 21,
}

impl VertexType {
    pub const COUNT: usize = 22;

    pub fn from_index(i: i128) -> Option<Self> {
        if !(0..Self::COUNT as i128).contains(&i) {
            return None;
        }
        Some(unsafe { std::mem::transmute::<u32, Self>(i as u32) })
    }
}

/// Bitmap sampler filter mode (rmop default). Mirrors the schema's
/// `render_method_bitmap_filter_mode_enum`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BitmapFilterMode {
    #[default]
    Trilinear              = 0,
    Point                  = 1,
    Bilinear               = 2,
    Anisotropic1           = 3,
    Anisotropic2Expensive  = 4,
    Anisotropic3Expensive  = 5,
    Anisotropic4Expensive  = 6,
    LightprobeTextureArray = 7,
    ComparisonPoint        = 8,
    ComparisonBilinear     = 9,
}

impl BitmapFilterMode {
    pub fn from_index(i: i128) -> Option<Self> {
        Some(match i {
            0 => Self::Trilinear,
            1 => Self::Point,
            2 => Self::Bilinear,
            3 => Self::Anisotropic1,
            4 => Self::Anisotropic2Expensive,
            5 => Self::Anisotropic3Expensive,
            6 => Self::Anisotropic4Expensive,
            7 => Self::LightprobeTextureArray,
            8 => Self::ComparisonPoint,
            9 => Self::ComparisonBilinear,
            _ => return None,
        })
    }
}

/// Bitmap sampler address mode (rmop default). Mirrors the schema's
/// `render_method_bitmap_address_mode_enum`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BitmapAddressMode {
    #[default]
    Wrap        = 0,
    Clamp       = 1,
    Mirror      = 2,
    BlackBorder = 3,
}

impl BitmapAddressMode {
    pub fn from_index(i: i128) -> Option<Self> {
        Some(match i {
            0 => Self::Wrap,
            1 => Self::Clamp,
            2 => Self::Mirror,
            3 => Self::BlackBorder,
            _ => return None,
        })
    }
}

/// Bitmap sampler comparison function (rmop default). Mirrors the
/// schema's `render_method_bitmap_comparison_function_enum`. Used by
/// the comparison-filter sampler modes for shadow / depth fetches.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BitmapComparisonFunction {
    #[default]
    Never        = 0,
    Less         = 1,
    Equal        = 2,
    LessEqual    = 3,
    Greater      = 4,
    NotEqual     = 5,
    GreaterEqual = 6,
    Always       = 7,
}

impl BitmapComparisonFunction {
    pub fn from_index(i: i128) -> Option<Self> {
        Some(match i {
            0 => Self::Never,
            1 => Self::Less,
            2 => Self::Equal,
            3 => Self::LessEqual,
            4 => Self::Greater,
            5 => Self::NotEqual,
            6 => Self::GreaterEqual,
            7 => Self::Always,
            _ => return None,
        })
    }
}

// =============================================================================
// TagBlockIndex — bit-packed (start_index : 10, count : 6)
// =============================================================================

/// 16-bit packed `(count, start_index)` reference into another tag
/// block. The runtime spells this `s_tag_block_index` with bitfields
/// `start_index : 10; count : 6;` — equivalently
/// `count = packed >> 10`, `start = packed & 0x3FF`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TagBlockIndex(pub u16);

impl TagBlockIndex {
    pub fn new(start: u16, count: u16) -> Self {
        debug_assert!(start < 1024);
        debug_assert!(count < 64);
        Self((count << 10) | (start & 0x3FF))
    }

    pub fn start(self) -> u16 { self.0 & 0x3FF }
    pub fn count(self) -> u16 { self.0 >> 10 }

    /// Half-open range `start..start+count`, ready to index a
    /// containing `Vec`.
    pub fn range(self) -> std::ops::Range<usize> {
        let s = self.start() as usize;
        s..s + self.count() as usize
    }
}

// =============================================================================
// Routing info
// =============================================================================

/// Per-pass routing entry — maps a constant-table source into a
/// destination D3D register. Mirrors `s_render_method_routing_info`
/// (4 bytes). On-disk layout: `[dest:u16, source:u8, type_specific:u8]`.
///
/// Per the H3 schema (`render_method_routing_info_block` in
/// `definitions/halo3_mcc/render_method_template.json`):
/// - byte 0..1 = `destination index` — D3D constant register or sampler index
/// - byte 2    = `source index` — index into `rmt2.float_constants[]` (the
///               source slot in the constant table this entry pulls from)
/// - byte 3    = `type specific` — "bitmap flags or shader component mask",
///               not used by `submit_static_ps_parameters` for the real-
///               constant path
///
/// Verified against runtime dump of multiple riverworld rmt2s 2026-05-06:
/// byte 2 increments 0,1,2,3 across routing entries (= source_index per
/// schema), byte 3 is always 0 for real-constant routing. (Earlier docs
/// in this file labelled byte 2 as "overlay" — that was wrong; the engine
/// reads byte 2 as the source index.)
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderMethodRoutingInfo {
    /// D3D constant index (or sampler index for textures).
    /// Byte offset 0..1 in the routing entry.
    pub destination_index: u16,
    /// Index into `rmt2.float_constants[]` — which slot's resolved
    /// vec4 to write at this destination. Byte offset 2.
    pub source_index: u8,
    /// "type specific" — bitmap flags or shader component mask.
    /// Engine `submit_static_ps_parameters` ignores this for the
    /// real-constant path; relevant for bitmap routing variants.
    /// Byte offset 3.
    pub type_specific: u8,
}

// =============================================================================
// Pass tables
// =============================================================================

/// Per-pass index table inside a [`RenderMethodPostprocessDefinition`]
/// (6 bytes). Three `TagBlockIndex` slices into bitmaps, vertex real
/// constants, and pixel real constants — postprocess only ever needs
/// these three (other constant types are pre-baked).
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderMethodPostprocessPass {
    pub bitmaps: TagBlockIndex,
    pub vertex_real_constants: TagBlockIndex,
    pub pixel_real_constants: TagBlockIndex,
}

/// Per-pass index table inside a [`RenderMethodTemplate`] (32 bytes
/// in the MCC schema; Ares H3 is 32 bytes too but field-by-field
/// layout differs slightly — MCC stores 12 `s_tag_block_index` slots,
/// vs Ares 12 + four `u8` extern sizes + pad).
///
/// One slot per [`ParameterUsage`]-equivalent dimension; the runtime
/// dispatches to a different submit function per slot.
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderMethodTemplatePass {
    pub bitmaps:                       TagBlockIndex,
    pub vertex_real_constants:         TagBlockIndex,
    pub vertex_int_constants:          TagBlockIndex,
    pub vertex_bool_constants:         TagBlockIndex,
    pub pixel_real_constants:          TagBlockIndex,
    pub pixel_int_constants:           TagBlockIndex,
    pub pixel_bool_constants:          TagBlockIndex,
    pub extern_bitmaps:                TagBlockIndex,
    pub extern_vertex_real_constants:  TagBlockIndex,
    pub extern_vertex_int_constants:   TagBlockIndex,
    pub extern_pixel_real_constants:   TagBlockIndex,
    pub extern_pixel_int_constants:    TagBlockIndex,
    pub pixel_parameters_size:         u16,
    pub vertex_parameters_size:        u16,
    pub alpha_blend_mode:              i32,
}

// =============================================================================
// Texture / parameter records
// =============================================================================

/// Per-pass texture binding inside a postprocess definition. Mirrors
/// `s_render_method_postprocess_texture` (28 bytes in the MCC schema).
///
/// The `address_mode` byte at offset 0x12 in Ares is two 4-bit nibbles
/// (`address_mode_x : 4` low, `address_mode_y : 4` high). It is split
/// into the two enum fields here for direct use.
#[derive(Debug, Clone)]
pub struct RenderMethodPostprocessTexture {
    /// Path to the bound `bitmap` tag (empty when the slot uses an
    /// extern texture instead).
    pub bitmap_path: String,
    pub bitmap_index: i16,
    pub address_mode_x: BitmapAddressMode,
    pub address_mode_y: BitmapAddressMode,
    pub filter_mode: BitmapFilterMode,
    pub comparison_function: BitmapComparisonFunction,
    /// `None` when the slot uses the inline `bitmap_path`; `Some(extern)`
    /// when the texture is sourced from engine state.
    pub extern_texture_mode: Option<RenderMethodExtern>,
    pub texture_transform_constant_index: i8,
    pub texture_transform_overlay_indices: TagBlockIndex,
}

/// One entry in [`RenderMethod::parameters`] — a per-instance
/// parameter the artist set on this rmsh. Mirrors
/// `s_render_method_parameter` (60 bytes). Optionally carries one or
/// more animated functions that re-evaluate the value each frame.
///
/// Enum-typed fields (`bitmap_filter_mode`, `bitmap_comparison_function`,
/// `bitmap_address_mode`, `bitmap_address_mode_x`, `bitmap_address_mode_y`)
/// are declared as `_field_short_integer` in the rmsh schema rather than
/// `_field_short_enum`, but the underlying values index the same enums
/// the rmop declares — they are stored as the strong enum type here so
/// downstream code can pattern-match without re-converting from i16.
#[derive(Debug, Clone)]
pub struct RenderMethodParameter {
    pub parameter_name: String,
    pub parameter_type: Option<RenderMethodParameterType>,
    pub bitmap_path: String,
    pub real_parameter: f32,
    pub int_parameter: i32,
    pub bitmap_flags: i16,
    pub bitmap_filter_mode: BitmapFilterMode,
    pub bitmap_comparison_function: BitmapComparisonFunction,
    pub bitmap_address_mode: BitmapAddressMode,
    pub bitmap_address_mode_x: BitmapAddressMode,
    pub bitmap_address_mode_y: BitmapAddressMode,
    pub bitmap_anisotropy_amount: i16,
    /// `None` when the slot uses an inline bitmap; `Some(extern)` when
    /// the texture is sourced from engine state (e.g., scene HDR).
    pub bitmap_extern_mode: Option<RenderMethodExtern>,
    pub animated_parameters: Vec<RenderMethodAnimatedParameter>,
}

/// One entry in [`RenderMethodOption::parameters`] — a definition-time
/// parameter declared by the rmop with full default values. Mirrors
/// `s_render_method_option_parameter` (76 bytes in MCC schema).
#[derive(Debug, Clone)]
pub struct RenderMethodOptionParameter {
    pub parameter_name: String,
    pub parameter_type: Option<RenderMethodParameterType>,
    /// When non-`None`, this parameter is sourced from an engine extern
    /// rather than baked/animated values.
    pub source_extern: Option<RenderMethodExtern>,
    pub default_bitmap_path: String,
    pub default_real_value: f32,
    pub default_int_bool_value: i32,
    pub flags: i16,
    pub default_filter_mode: BitmapFilterMode,
    pub default_comparison_function: BitmapComparisonFunction,
    pub default_address_mode: BitmapAddressMode,
    pub anisotropy_amount: i16,
    pub default_color: ArgbColor,
    pub default_bitmap_scale: f32,
    pub help_text: String,
}

/// One animator wrapping a [`TagFunction`] that drives a channel of an
/// owning [`RenderMethodParameter`]. Mirrors
/// `s_render_method_animated_parameter` (36 bytes).
#[derive(Debug, Clone)]
pub struct RenderMethodAnimatedParameter {
    pub parameter_type: Option<RenderMethodAnimatedParameterType>,
    pub input_name: String,
    pub range_name: String,
    pub time_period_in_seconds: f32,
    pub function: Option<TagFunction>,
}

// =============================================================================
// Postprocess definition (the fast-path resolved cbuffer layout)
// =============================================================================

/// Pre-baked, runtime-shaped resolution of an rmsh's parameters.
/// Mirrors `s_render_method_postprocess_definition` (140 bytes).
///
/// When this is non-empty, the runtime hot path uses it directly to
/// push constants — only animated params force a re-walk through
/// [`RenderMethod::parameters`].
#[derive(Debug, Clone, Default)]
pub struct RenderMethodPostprocessDefinition {
    /// Path to the `rmt2` tag this postprocess was baked against.
    pub template_path: String,
    pub textures: Vec<RenderMethodPostprocessTexture>,
    /// Each entry is one `real_vector4d` (xyzw) ready to copy into a
    /// shader cbuffer slot — flattened as a `[f32; 4]`.
    pub real_constants: Vec<[f32; 4]>,
    pub int_constants: Vec<i32>,
    /// Bool constants are packed one bit per parameter into a single
    /// `u32` (Ares: `m_bool_constants`).
    pub bool_constants: u32,
    /// One `TagBlockIndex` per [`EntryPoint`], indexing into `passes`.
    pub entry_points: Vec<TagBlockIndex>,
    pub passes: Vec<RenderMethodPostprocessPass>,
    pub routing_info: Vec<RenderMethodRoutingInfo>,
    pub overlays: Vec<RenderMethodAnimatedParameter>,
    pub blend_mode: i32,
    pub flags: i32,
    /// `s_render_method_postprocess_definition::e_runtime_queryable_property`
    /// — 8 short indices, query helpers omitted.
    pub runtime_queryable_properties: [i16; 8],
}

// =============================================================================
// rmt2 — render_method_template
// =============================================================================

/// Compiled-shader metadata + routing tables. Mirrors
/// `c_render_method_template` (132 bytes).
///
/// In the MCC schema the top-level fields are the "current platform"
/// data (vs. the Ares C++ which exposes them via `m_current_platform`
/// nested).
#[derive(Debug, Clone)]
pub struct RenderMethodTemplate {
    pub vertex_shader_path: String,
    pub pixel_shader_path: String,
    pub available_entry_points: u32,
    /// One `TagBlockIndex` per available entry point, indexing into
    /// `passes`.
    pub entry_points: Vec<TagBlockIndex>,
    pub passes: Vec<RenderMethodTemplatePass>,
    pub routing_info: Vec<RenderMethodRoutingInfo>,
    /// Name table for float (real) constants. Index from a routing
    /// entry's `source_index` to get the parameter name.
    pub float_constants: Vec<String>,
    pub int_constants: Vec<String>,
    pub bool_constants: Vec<String>,
    pub textures: Vec<String>,
}

// =============================================================================
// rmop — render_method_option
// =============================================================================

/// One option's parameter declarations + defaults. Mirrors
/// `c_render_method_option` (12 bytes — single block of parameters).
#[derive(Debug, Clone)]
pub struct RenderMethodOption {
    pub parameters: Vec<RenderMethodOptionParameter>,
}

// =============================================================================
// rmdf — render_method_definition
// =============================================================================

/// One row of the rmdf categories block. Names a category (e.g.,
/// "albedo") and lists the option choices available for it; each
/// option links to an `rmop`.
#[derive(Debug, Clone)]
pub struct RenderMethodDefinitionCategory {
    pub category_name: String,
    pub vertex_function: String,
    pub pixel_function: String,
    pub options: Vec<RenderMethodDefinitionCategoryOption>,
}

#[derive(Debug, Clone)]
pub struct RenderMethodDefinitionCategoryOption {
    pub option_name: String,
    /// Path to the `rmop` tag declaring this option's parameters.
    pub option_path: String,
    pub vertex_function: String,
    pub pixel_function: String,
}

/// rmdf top level. Mirrors `c_render_method_definition` (92 bytes).
#[derive(Debug, Clone)]
pub struct RenderMethodDefinition {
    pub global_options_path: String,
    pub categories: Vec<RenderMethodDefinitionCategory>,
    pub shared_pixel_shaders_path: String,
    pub shared_vertex_shaders_path: String,
    pub flags: u32,
    pub version: u32,
}

// =============================================================================
// rmsh / rm** — render_method
// =============================================================================

/// Top-level render_method. Mirrors `c_render_method` (64 bytes).
///
/// Subclasses (rmsh, rmtr, rmw, etc.) prepend their own struct on the
/// outside; the `c_render_method` portion is parsed from the inner
/// `render_method` struct field. This loader takes either form.
#[derive(Debug, Clone)]
pub struct RenderMethod {
    pub definition_path: String,
    /// One option index per rmdf category (in category order). The
    /// schema stores these as `short_block` of `short`.
    pub options: Vec<i16>,
    pub parameters: Vec<RenderMethodParameter>,
    /// Empty for tags that haven't been postprocessed yet — in that
    /// case, the walker resolves params on the fly via `parameters`
    /// and the rmop defaults.
    pub postprocess_definition: Option<RenderMethodPostprocessDefinition>,
    pub flags: u16,
    pub sort_layer: i8,
    pub runtime_flags: u8,
    pub custom_fog_setting_index: i32,
    pub prediction_atom_index: i32,
    /// FOURCC of the source tag — `'rmsh'`, `'rmtr'`, `'rmw '`, etc.
    /// The runtime `render_method_submit` chain is class-blind, but
    /// shader assemblers need this to dispatch to the right WGSL
    /// fragments (terrain → 4-layer blend body, water → wave body,
    /// etc.). Zero for `from_struct` callers that don't have the
    /// outer tag context.
    /// See `reference_rmtr_runtime_distinction.md`.
    pub group_tag: u32,
}

// =============================================================================
// Parsers
// =============================================================================

const GROUP_RM:   [u8; 4] = *b"rm  ";
const GROUP_RMSH: [u8; 4] = *b"rmsh";
const GROUP_RMDF: [u8; 4] = *b"rmdf";
const GROUP_RMOP: [u8; 4] = *b"rmop";
const GROUP_RMT2: [u8; 4] = *b"rmt2";

/// Variants of `rm**` that we accept as input to [`RenderMethod::from_tag`].
/// All embed a `render_method` struct field at the top of their layout.
const RENDER_METHOD_GROUPS: &[[u8; 4]] = &[
    GROUP_RM,
    GROUP_RMSH,                     // shader
    *b"rmtr", *b"rmw ", *b"rmfl",   // terrain, water, foliage
    *b"rmd ", *b"rmhg", *b"rmsk",   // decal, halogram, skin
    *b"rmct", *b"rmcs", *b"rmp ",   // cortana, custom, particle
    *b"rmb ", *b"rmco", *b"rmlv",   // beam, contrail, light_volume
];

fn check_group(tag: &TagFile, allowed: &[[u8; 4]]) -> Result<(), RenderMethodError> {
    let actual = tag.group().tag.to_be_bytes();
    if allowed.iter().any(|g| g == &actual) {
        Ok(())
    } else {
        Err(RenderMethodError::WrongGroup { expected: allowed[0], actual })
    }
}

// ---- RenderMethod ----

impl RenderMethod {
    /// Parse an `rm**` tag (or its base `rm  `). Subclass tags (rmsh,
    /// rmtr, ...) embed the `c_render_method` portion as a nested
    /// struct field named "render_method"; we descend into it before
    /// reading.
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderMethodError> {
        check_group(tag, RENDER_METHOD_GROUPS)?;
        let group_tag = tag.group().tag;
        let root = tag.root();
        // Subclasses (rmsh etc.) wrap c_render_method as a struct field.
        let rm = root
            .descend("render_method")
            .unwrap_or(root);
        let mut out = Self::from_struct(&rm)?;
        out.group_tag = group_tag;
        Ok(out)
    }

    /// Parse from an in-place `c_render_method` struct view. Used both
    /// by [`Self::from_tag`] and by callers that already have a
    /// `TagStruct` cursor (e.g., walking an embedded render_method
    /// inside a different group).
    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let definition_path = s.read_tag_ref_path("definition")
            .or_else(|| s.read_tag_ref_path("definition*"))
            .unwrap_or_default();

        let options = s.field("options")
            .and_then(|f| f.as_block())
            .map(|b| read_short_block(&b))
            .unwrap_or_default();

        let parameters = s.field("parameters")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec(&b, RenderMethodParameter::from_struct))
            .unwrap_or_default();

        let postprocess_definition = s.field("postprocess")
            .and_then(|f| f.as_block())
            .filter(|b| !b.is_empty())
            .and_then(|b| b.element(0))
            .map(|e| RenderMethodPostprocessDefinition::from_struct(&e))
            .transpose()?;

        let flags = s.read_int_any("shader flags")
            .or_else(|| s.read_int_any("shader flags*"))
            .unwrap_or(0) as u16;
        let sort_layer = s.read_int_any("sort layer")
            .or_else(|| s.read_int_any("sort layer*"))
            .unwrap_or(0) as i8;
        let runtime_flags = s.read_int_any("runtime flags")
            .or_else(|| s.read_int_any("runtime flags!"))
            .unwrap_or(0) as u8;
        let custom_fog_setting_index = s.read_int_any("Custom fog setting index")
            .unwrap_or(0) as i32;
        let prediction_atom_index = s.read_int_any("prediction atom index")
            .or_else(|| s.read_int_any("prediction atom index!"))
            .unwrap_or(-1) as i32;

        Ok(Self {
            definition_path,
            options,
            parameters,
            postprocess_definition,
            flags,
            sort_layer,
            runtime_flags,
            custom_fog_setting_index,
            prediction_atom_index,
            group_tag: 0,  // overridden by from_tag; from_struct callers don't have outer context
        })
    }
}

// ---- RenderMethodDefinition ----

impl RenderMethodDefinition {
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderMethodError> {
        check_group(tag, &[GROUP_RMDF])?;
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let global_options_path = s.read_tag_ref_path("global options").unwrap_or_default();

        let categories = s.field("categories")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec(&b, RenderMethodDefinitionCategory::from_struct))
            .unwrap_or_default();

        let shared_pixel_shaders_path = s.read_tag_ref_path("shared pixel shaders")
            .or_else(|| s.read_tag_ref_path("shared pixel shaders*"))
            .unwrap_or_default();
        let shared_vertex_shaders_path = s.read_tag_ref_path("shared vertex shaders")
            .or_else(|| s.read_tag_ref_path("shared vertex shaders*"))
            .unwrap_or_default();

        let flags = s.read_int_any("flags").unwrap_or(0) as u32;
        let version = s.read_int_any("version").unwrap_or(0) as u32;

        Ok(Self {
            global_options_path,
            categories,
            shared_pixel_shaders_path,
            shared_vertex_shaders_path,
            flags,
            version,
        })
    }
}

impl RenderMethodDefinitionCategory {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        Ok(Self {
            category_name: s.read_string_id("category name").unwrap_or_default(),
            vertex_function: s.read_string_id("vertex function").unwrap_or_default(),
            pixel_function: s.read_string_id("pixel function").unwrap_or_default(),
            options: s.field("options")
                .and_then(|f| f.as_block())
                .map(|b| read_block_vec(&b, RenderMethodDefinitionCategoryOption::from_struct))
                .unwrap_or_default(),
        })
    }
}

impl RenderMethodDefinitionCategoryOption {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        Ok(Self {
            option_name: s.read_string_id("option name").unwrap_or_default(),
            option_path: s.read_tag_ref_path("option").unwrap_or_default(),
            vertex_function: s.read_string_id("vertex function").unwrap_or_default(),
            pixel_function: s.read_string_id("pixel function").unwrap_or_default(),
        })
    }
}

// ---- RenderMethodOption ----

impl RenderMethodOption {
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderMethodError> {
        check_group(tag, &[GROUP_RMOP])?;
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let parameters = s.field("parameters")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec(&b, RenderMethodOptionParameter::from_struct))
            .unwrap_or_default();
        Ok(Self { parameters })
    }
}

impl RenderMethodOptionParameter {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let default_color = match s.field("default color").and_then(|f| f.value()) {
            Some(crate::fields::TagFieldData::ArgbColor(c)) => c,
            _ => ArgbColor(0),
        };
        Ok(Self {
            parameter_name: s.read_string_id("parameter name").unwrap_or_default(),
            parameter_type: s.read_enum_name("parameter type")
                .as_deref()
                .and_then(RenderMethodParameterType::from_name)
                .or_else(|| {
                    s.read_int_any("parameter type")
                        .and_then(RenderMethodParameterType::from_index)
                }),
            source_extern: s.read_enum_name("source extern")
                .as_deref()
                .and_then(RenderMethodExtern::from_name),
            default_bitmap_path: s.read_tag_ref_path("default bitmap").unwrap_or_default(),
            default_real_value: s.read_real("default real value").unwrap_or(0.0),
            default_int_bool_value: s.read_int_any("default int/bool value").unwrap_or(0) as i32,
            flags: s.read_int_any("flags").unwrap_or(0) as i16,
            default_filter_mode: s.read_int_any("default filter mode")
                .and_then(BitmapFilterMode::from_index)
                .unwrap_or_default(),
            default_comparison_function: s.read_int_any("default comparison function")
                .and_then(BitmapComparisonFunction::from_index)
                .unwrap_or_default(),
            default_address_mode: s.read_int_any("default address mode")
                .and_then(BitmapAddressMode::from_index)
                .unwrap_or_default(),
            anisotropy_amount: s.read_int_any("anisotropy amount").unwrap_or(0) as i16,
            default_color,
            default_bitmap_scale: s.read_real("default bitmap scale").unwrap_or(0.0),
            help_text: s.field("help text")
                .and_then(|f| f.as_data())
                .and_then(|b| std::str::from_utf8(b).ok())
                .map(|s| s.trim_end_matches('\0').to_owned())
                .unwrap_or_default(),
        })
    }
}

// ---- RenderMethodTemplate ----

impl RenderMethodTemplate {
    pub fn from_tag(tag: &TagFile) -> Result<Self, RenderMethodError> {
        check_group(tag, &[GROUP_RMT2])?;
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        // For MCC tags, the rmt2's top-level passes/routing/constants
        // are EMPTY. The PC platform's data lives in `other platforms[0]`
        // (a `s_platform_data` struct). Try the top level first; fall
        // through to `other platforms[0]` whenever a block is empty.
        let other_platforms = s.field("other platforms").and_then(|f| f.as_block());
        let pc_platform = other_platforms
            .as_ref()
            .and_then(|b| b.element(0));

        // Pick the source for each block: prefer top-level if non-empty,
        // else fall through to other_platforms[0].
        let pick_block = |name: &str| -> Option<crate::api::TagBlock<'_>> {
            let top = s.field(name).and_then(|f| f.as_block());
            if let Some(b) = top.as_ref() {
                if b.len() > 0 {
                    return top;
                }
            }
            pc_platform.as_ref()
                .and_then(|p| p.field(name))
                .and_then(|f| f.as_block())
        };

        Ok(Self {
            vertex_shader_path: s.read_tag_ref_path("vertex shader")
                .or_else(|| pc_platform.as_ref().and_then(|p| p.read_tag_ref_path("vertex shader")))
                .unwrap_or_default(),
            pixel_shader_path: s.read_tag_ref_path("pixel shader")
                .or_else(|| pc_platform.as_ref().and_then(|p| p.read_tag_ref_path("pixel shader")))
                .unwrap_or_default(),
            available_entry_points: s.read_int_any("available entry points")
                .or_else(|| s.read_int_any("available entry points*"))
                .or_else(|| pc_platform.as_ref().and_then(|p| p.read_int_any("available entry points")))
                .or_else(|| pc_platform.as_ref().and_then(|p| p.read_int_any("available entry points*")))
                .unwrap_or(0) as u32,
            entry_points: pick_block("entry points")
                .map(|b| read_tag_block_index_block(&b))
                .unwrap_or_default(),
            passes: pick_block("passes")
                .map(|b| read_block_vec_infallible(&b, RenderMethodTemplatePass::from_struct))
                .unwrap_or_default(),
            routing_info: pick_block("routing info")
                .map(|b| read_block_vec_infallible(&b, RenderMethodRoutingInfo::from_struct))
                .unwrap_or_default(),
            float_constants: read_constant_table_or(s, pc_platform.as_ref(), "float constants"),
            int_constants: read_constant_table_or(s, pc_platform.as_ref(), "int constants"),
            bool_constants: read_constant_table_or(s, pc_platform.as_ref(), "bool constants"),
            textures: read_constant_table_or(s, pc_platform.as_ref(), "textures"),
        })
    }
}

/// Read a constant-table block, preferring the top-level field when
/// non-empty and falling through to `other platforms[0]` for MCC's
/// per-platform schema.
fn read_constant_table_or(
    top: &TagStruct<'_>,
    pc_platform: Option<&TagStruct<'_>>,
    name: &str,
) -> Vec<String> {
    let top_block = top.field(name).and_then(|f| f.as_block());
    if let Some(b) = top_block.as_ref() {
        if b.len() > 0 {
            return read_constant_table(top, name);
        }
    }
    if let Some(p) = pc_platform {
        return read_constant_table(p, name);
    }
    Vec::new()
}

impl RenderMethodTemplatePass {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            bitmaps:                       read_packed_block_index(s, "bitmaps"),
            vertex_real_constants:         read_packed_block_index(s, "vertex real constants"),
            vertex_int_constants:          read_packed_block_index(s, "vertex int constants"),
            vertex_bool_constants:         read_packed_block_index(s, "vertex bool constants"),
            pixel_real_constants:          read_packed_block_index(s, "pixel real constants"),
            pixel_int_constants:           read_packed_block_index(s, "pixel int constants"),
            pixel_bool_constants:          read_packed_block_index(s, "pixel bool constants"),
            extern_bitmaps:                read_packed_block_index(s, "extern bitmaps"),
            extern_vertex_real_constants:  read_packed_block_index(s, "extern vertex real constants"),
            extern_vertex_int_constants:   read_packed_block_index(s, "extern vertex int constants"),
            extern_pixel_real_constants:   read_packed_block_index(s, "extern pixel real constants"),
            extern_pixel_int_constants:    read_packed_block_index(s, "extern pixel int constants"),
            pixel_parameters_size:         s.read_int_any("pixel parameters size").unwrap_or(0) as u16,
            vertex_parameters_size:        s.read_int_any("vertex parameters size").unwrap_or(0) as u16,
            alpha_blend_mode:              s.read_int_any("alpha blend mode").unwrap_or(0) as i32,
        }
    }
}

impl RenderMethodRoutingInfo {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            destination_index: s.read_int_any("destination index").unwrap_or(0) as u16,
            source_index:      s.read_int_any("source index").unwrap_or(0) as u8,
            type_specific:     s.read_int_any("type specific").unwrap_or(0) as u8,
        }
    }
}

// ---- RenderMethodPostprocessDefinition ----

impl RenderMethodPostprocessDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let textures = s.field("textures")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec_infallible(&b, RenderMethodPostprocessTexture::from_struct))
            .unwrap_or_default();

        let real_constants = s.field("real vectors")
            .and_then(|f| f.as_block())
            .map(|b| read_real_vector4d_block(&b))
            .unwrap_or_default();

        let int_constants = s.field("int constants")
            .and_then(|f| f.as_block())
            .map(|b| read_int_block(&b))
            .unwrap_or_default();

        let bool_constants = s.read_int_any("bool constants").unwrap_or(0) as u32;

        let entry_points = s.field("entry points")
            .and_then(|f| f.as_block())
            .map(|b| read_tag_block_index_block(&b))
            .unwrap_or_default();

        let passes = s.field("passes")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec_infallible(&b, RenderMethodPostprocessPass::from_struct))
            .unwrap_or_default();

        let routing_info = s.field("routing info")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec_infallible(&b, RenderMethodRoutingInfo::from_struct))
            .unwrap_or_default();

        let overlays = s.field("overlays")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec(&b, RenderMethodAnimatedParameter::from_struct))
            .unwrap_or_default();

        let blend_mode = s.read_int_any("blend mode").unwrap_or(0) as i32;
        let flags = s.read_int_any("flags").unwrap_or(0) as i32;

        let mut runtime_queryable_properties = [-1i16; 8];
        if let Some(arr) = s.field("runtime queryable properties table").and_then(|f| f.as_array()) {
            for (i, slot) in runtime_queryable_properties.iter_mut().enumerate() {
                if let Some(elem) = arr.element(i) {
                    *slot = elem.read_int_any("index").unwrap_or(-1) as i16;
                }
            }
        }

        Ok(Self {
            template_path: s.read_tag_ref_path("shader template").unwrap_or_default(),
            textures,
            real_constants,
            int_constants,
            bool_constants,
            entry_points,
            passes,
            routing_info,
            overlays,
            blend_mode,
            flags,
            runtime_queryable_properties,
        })
    }
}

impl RenderMethodPostprocessPass {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            bitmaps: read_packed_block_index(s, "bitmaps"),
            vertex_real_constants: read_packed_block_index(s, "vertex real"),
            pixel_real_constants: read_packed_block_index(s, "pixel real"),
        }
    }
}

impl RenderMethodPostprocessTexture {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let texture_transform_overlay_indices = s
            .descend("texture transform overlay indices")
            .map(|inner| read_packed_block_index(&inner, "block index data"))
            .unwrap_or_default();
        // The `address mode` byte packs `address_mode_x : 4` (low) and
        // `address_mode_y : 4` (high) per Ares
        // `s_render_method_postprocess_texture` layout. Split into the
        // typed enum fields directly.
        let address_packed = s.read_int_any("address mode").unwrap_or(0) as u8;
        let address_mode_x = BitmapAddressMode::from_index((address_packed & 0x0F) as i128)
            .unwrap_or_default();
        let address_mode_y = BitmapAddressMode::from_index((address_packed >> 4) as i128)
            .unwrap_or_default();
        Self {
            bitmap_path: s.read_tag_ref_path("bitmap reference").unwrap_or_default(),
            bitmap_index: s.read_int_any("bitmap index").unwrap_or(-1) as i16,
            address_mode_x,
            address_mode_y,
            filter_mode: s.read_int_any("filter mode")
                .and_then(BitmapFilterMode::from_index)
                .unwrap_or_default(),
            comparison_function: s.read_int_any("comparison function")
                .and_then(BitmapComparisonFunction::from_index)
                .unwrap_or_default(),
            extern_texture_mode: s.read_int_any("extern texture mode")
                .and_then(RenderMethodExtern::from_index)
                .filter(|e| !matches!(e, RenderMethodExtern::None)),
            texture_transform_constant_index: s.read_int_any("texture transform constant index")
                .unwrap_or(-1) as i8,
            texture_transform_overlay_indices,
        }
    }
}

// ---- RenderMethodParameter / animated ----

impl RenderMethodParameter {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        let animated_parameters = s.field("animated parameters")
            .and_then(|f| f.as_block())
            .map(|b| read_block_vec(&b, RenderMethodAnimatedParameter::from_struct))
            .unwrap_or_default();

        Ok(Self {
            parameter_name: s.read_string_id("parameter name").unwrap_or_default(),
            parameter_type: s.read_enum_name("parameter type")
                .as_deref()
                .and_then(RenderMethodParameterType::from_name)
                .or_else(|| {
                    s.read_int_any("parameter type")
                        .and_then(RenderMethodParameterType::from_index)
                }),
            bitmap_path: s.read_tag_ref_path("bitmap").unwrap_or_default(),
            real_parameter: s.read_real("real").unwrap_or(0.0),
            int_parameter: s.read_int_any("int/bool").unwrap_or(0) as i32,
            bitmap_flags: s.read_int_any("bitmap flags").unwrap_or(0) as i16,
            bitmap_filter_mode: s.read_int_any("bitmap filter mode")
                .and_then(BitmapFilterMode::from_index)
                .unwrap_or_default(),
            bitmap_comparison_function: s.read_int_any("bitmap comparison function")
                .and_then(BitmapComparisonFunction::from_index)
                .unwrap_or_default(),
            bitmap_address_mode: s.read_int_any("bitmap address mode")
                .and_then(BitmapAddressMode::from_index)
                .unwrap_or_default(),
            bitmap_address_mode_x: s.read_int_any("bitmap address mode x")
                .and_then(BitmapAddressMode::from_index)
                .unwrap_or_default(),
            bitmap_address_mode_y: s.read_int_any("bitmap address mode y")
                .and_then(BitmapAddressMode::from_index)
                .unwrap_or_default(),
            bitmap_anisotropy_amount: s.read_int_any("bitmap anisotropy amount").unwrap_or(0) as i16,
            // `bitmap extern RTT mode` is `_field_short_integer` in the
            // rmsh schema (no enum binding) — convert via from_index to
            // match the rmop's `source extern` enum semantics. 0 = None.
            bitmap_extern_mode: s.read_int_any("bitmap extern RTT mode")
                .and_then(RenderMethodExtern::from_index)
                .filter(|e| !matches!(e, RenderMethodExtern::None)),
            animated_parameters,
        })
    }
}

impl RenderMethodAnimatedParameter {
    fn from_struct(s: &TagStruct<'_>) -> Result<Self, RenderMethodError> {
        Ok(Self {
            parameter_type: s.read_int_any("type")
                .and_then(RenderMethodAnimatedParameterType::from_index),
            input_name: s.read_string_id("input name").unwrap_or_default(),
            range_name: s.read_string_id("range name").unwrap_or_default(),
            // Schema field name is `"time period"` (no suffix). The
            // ":seconds" hint in some Halo schemas indicates the unit
            // but isn't part of the field's actual identifier.
            time_period_in_seconds: s.read_real("time period")
                .or_else(|| s.read_real("time period:seconds"))
                .unwrap_or(0.0),
            function: s.field("function")
                .and_then(|f| f.as_struct())
                .and_then(|inner| inner.field("data"))
                .and_then(|f| f.as_function()),
        })
    }
}

// =============================================================================
// Block readers (the schema wraps single-field "value blocks" — short,
// int, real_vector4d, tag_block_index — that decode 1:1 to Vec<T>).
// =============================================================================

fn read_block_vec<T, F>(block: &TagBlock<'_>, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> Result<T, RenderMethodError>,
{
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            if let Ok(v) = f(&elem) {
                out.push(v);
            }
        }
    }
    out
}

fn read_block_vec_infallible<T, F>(block: &TagBlock<'_>, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}

fn read_short_block(block: &TagBlock<'_>) -> Vec<i16> {
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(elem.read_int_any("short").unwrap_or(0) as i16);
        }
    }
    out
}

fn read_int_block(block: &TagBlock<'_>) -> Vec<i32> {
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(elem.read_int_any("int value").unwrap_or(0) as i32);
        }
    }
    out
}

fn read_tag_block_index_block(block: &TagBlock<'_>) -> Vec<TagBlockIndex> {
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            // The schema wraps each value as { struct "block index" { short_integer "block index data" } }
            let packed = elem
                .descend("block index")
                .and_then(|inner| inner.read_int_any("block index data"))
                .or_else(|| elem.read_int_any("block index data"))
                .unwrap_or(0) as u16;
            out.push(TagBlockIndex(packed));
        }
    }
    out
}

fn read_real_vector4d_block(block: &TagBlock<'_>) -> Vec<[f32; 4]> {
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            let v = elem.read_vec3("vector");
            let w = elem.read_real("vector w").unwrap_or(0.0);
            out.push([v.i, v.j, v.k, w]);
        }
    }
    out
}

fn read_packed_block_index(s: &TagStruct<'_>, name: &str) -> TagBlockIndex {
    TagBlockIndex(s.read_int_any(name).unwrap_or(0) as u16)
}

fn read_constant_table(s: &TagStruct<'_>, field_name: &str) -> Vec<String> {
    let Some(block) = s.field(field_name).and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(elem.read_string_id("parameter name").unwrap_or_default());
        }
    }
    out
}
