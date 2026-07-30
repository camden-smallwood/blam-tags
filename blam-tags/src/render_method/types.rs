//! Render-method runtime types — verbatim mirror of Ares
//! `source/render_methods/render_method_definitions.h`.
//!
//! See `mod.rs` for the high-level overview. This file is pure type
//! definitions plus walkers from `&TagFile` / `&TagStruct` to the
//! corresponding Rust struct.

use crate::api::{TagBlock, TagStruct};
use crate::typed_enums::{Enum, Flags};

/// `entry_points_flags` (long_flags, rmt2 `available entry points*`) —
/// bitmask of which engine entry points the compiled shader supports.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum EntryPointFlags {
    #[strum(serialize = "default")] Default = 0,
    #[strum(serialize = "albedo")] Albedo = 1,
    #[strum(serialize = "static_default")] StaticDefault = 2,
    #[strum(serialize = "static_per_pixel")] StaticPerPixel = 3,
    #[strum(serialize = "static_per_vertex")] StaticPerVertex = 4,
    #[strum(serialize = "static_sh")] StaticSh = 5,
    #[strum(serialize = "static_prt_ambient")] StaticPrtAmbient = 6,
    #[strum(serialize = "static_prt_linear")] StaticPrtLinear = 7,
    #[strum(serialize = "static_prt_quadratic")] StaticPrtQuadratic = 8,
    #[strum(serialize = "dynamic_light")] DynamicLight = 9,
    #[strum(serialize = "shadow_generate")] ShadowGenerate = 10,
    #[strum(serialize = "shadow_apply")] ShadowApply = 11,
    #[strum(serialize = "active_camo")] ActiveCamo = 12,
    #[strum(serialize = "lightmap_debug_mode")] LightmapDebugMode = 13,
    #[strum(serialize = "vertex color lighting")] VertexColorLighting = 14,
    #[strum(serialize = "water tessellation")] WaterTessellation = 15,
    #[strum(serialize = "water shading")] WaterShading = 16,
    #[strum(serialize = "dynamic_light_cinematic")] DynamicLightCinematic = 17,
}

/// `global_render_method_flags_defintion` (word_flags, schema `shader
/// flags*`). Readonly/tool-derived render-state hints.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum GlobalRenderMethodFlags {
    #[strum(serialize = "don't fog me")] DontFogMe = 0,
    #[strum(serialize = "use custom setting")] UseCustomSetting = 1,
    #[strum(serialize = "calculate Z camera")] CalculateZCamera = 2,
}

/// `global_sort_layer_enum_defintion` (char_enum, schema `sort layer*`).
/// Transparent draw-order bucket. Ordered by discriminant.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum GlobalSortLayer {
    #[default]
    #[strum(serialize = "invalid")] Invalid = 0,
    #[strum(serialize = "pre-pass")] PrePass = 1,
    #[strum(serialize = "normal")] Normal = 2,
    #[strum(serialize = "post-pass")] PostPass = 3,
}

/// `global_render_method_runtime_flags_defintion` (byte_flags, schema
/// `runtime flags!`). Runtime-resolved; 0 in source tags.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum GlobalRenderMethodRuntimeFlags {
    #[strum(serialize = "use VS with misc")] UseVsWithMisc = 0,
}
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

/// Parameter data type (`render_method_parameter_type_enum`). Mirrors
/// Ares `e_render_method_parameter_type`. Resolved by embedded schema
/// name; `value`/`switch` are cross-build aliases for `real`/`int`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum RenderMethodParameterType {
    #[strum(serialize = "bitmap")]                       Bitmap    = 0,
    #[strum(serialize = "color")]                        Color     = 1,
    #[strum(serialize = "real", serialize = "value")]    Real      = 2,
    #[strum(serialize = "int", serialize = "switch")]    Int       = 3,
    #[strum(serialize = "bool")]                         Bool      = 4,
    #[strum(serialize = "argb color")]                   ArgbColor = 5,
}

/// Per-channel target for an animated parameter's evaluated function
/// output. Mirrors Ares `e_render_method_animated_parameter_type` (the
/// MCC tag schema only encodes the first 9; runtime extends to 15).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum RenderMethodAnimatedParameterType {
    #[strum(serialize = "value")]         Value         = 0,
    #[strum(serialize = "color")]         Color         = 1,
    #[strum(serialize = "scale uniform")] ScaleUniform  = 2,
    #[strum(serialize = "scale x")]       ScaleX        = 3,
    #[strum(serialize = "scale y")]       ScaleY        = 4,
    #[strum(serialize = "translation x")] TranslationX  = 5,
    #[strum(serialize = "translation y")] TranslationY  = 6,
    #[strum(serialize = "frame index")]   FrameIndex    = 7,
    #[strum(serialize = "alpha")]         Alpha         = 8,
}

/// Engine-bound parameter source. Mirrors Ares `e_render_method_extern`
/// — 49 H3 entries — then the Gen3 remaster additions, then the names the
/// shipped H3 tags turn out to offer that Ares' list does not.
///
/// The set is the union across games on purpose: `source extern` resolves by the
/// name embedded in the tag, so a name with no variant here is a panic, not a
/// fallback. Add rather than reorder.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum RenderMethodExtern {
    #[strum(serialize = "none")]                             None                                 = 0,
    #[strum(serialize = "texaccum target")]                  TextureGlobalTargetTexaccum          = 1,
    #[strum(serialize = "normal target")]                    TextureGlobalTargetNormal            = 2,
    #[strum(serialize = "z target")]                         TextureGlobalTargetZ                 = 3,
    #[strum(serialize = "shadow 1 target")]                  TextureGlobalTargetShadowBuffer1     = 4,
    #[strum(serialize = "shadow 2 target")]                  TextureGlobalTargetShadowBuffer2     = 5,
    #[strum(serialize = "shadow 3 target")]                  TextureGlobalTargetShadowBuffer3     = 6,
    #[strum(serialize = "shadow 4 target")]                  TextureGlobalTargetShadowBuffer4     = 7,
    #[strum(serialize = "texture camera target")]            TextureGlobalTargetTextureCamera     = 8,
    #[strum(serialize = "reflection target")]                TextureGlobalTargetReflection        = 9,
    #[strum(serialize = "refraction target")]                TextureGlobalTargetRefraction        = 10,
    #[strum(serialize = "lightprobe texture")]               TextureLightprobeTexture             = 11,
    #[strum(serialize = "dominant light intensity texture")] TextureDominantLightIntensityMap     = 12,
    #[strum(serialize = "unused 1")]                         TextureUnused1                       = 13,
    #[strum(serialize = "unused 2")]                         TextureUnused2                       = 14,
    #[strum(serialize = "change color primary")]             ObjectChangeColorPrimary             = 15,
    #[strum(serialize = "change color secondary")]           ObjectChangeColorSecondary           = 16,
    #[strum(serialize = "change color tertiary")]            ObjectChangeColorTertiary            = 17,
    #[strum(serialize = "change color quaternary")]          ObjectChangeColorQuaternary          = 18,
    #[strum(serialize = "change color quinary")]             ObjectChangeColorQuinary             = 19,
    #[strum(serialize = "emblem color background")]          ObjectEmblemColorBackground          = 20,
    #[strum(serialize = "emblem color primary")]             ObjectEmblemColorPrimary             = 21,
    #[strum(serialize = "emblem color secondary")]           ObjectEmblemColorSecondary           = 22,
    #[strum(serialize = "dynamic environment map 1")]        TextureDynamicEnvironmentMap0        = 23,
    #[strum(serialize = "dynamic environment map 2")]        TextureDynamicEnvironmentMap1        = 24,
    #[strum(serialize = "cook torrance cc0236")]             TextureCookTorranceCc0236            = 25,
    #[strum(serialize = "cook torrance dd0236")]             TextureCookTorranceDd0236            = 26,
    #[strum(serialize = "cook torrance c78d78")]             TextureCookTorranceC78d78            = 27,
    #[strum(serialize = "light dir 0")]                      LightDir0                            = 28,
    #[strum(serialize = "light color 0")]                    LightColor0                          = 29,
    #[strum(serialize = "light dir 1")]                      LightDir1                            = 30,
    #[strum(serialize = "light color 1")]                    LightColor1                          = 31,
    #[strum(serialize = "light dir 2")]                      LightDir2                            = 32,
    #[strum(serialize = "light color 2")]                    LightColor2                          = 33,
    #[strum(serialize = "light dir 3")]                      LightDir3                            = 34,
    #[strum(serialize = "light color 3")]                    LightColor3                          = 35,
    #[strum(serialize = "unused 3")]                         TextureUnused3                       = 36,
    #[strum(serialize = "unused 4")]                         TextureUnused4                       = 37,
    #[strum(serialize = "unused 5")]                         TextureUnused5                       = 38,
    #[strum(serialize = "dynamic light gel 0")]              TextureDynamicLightGel0              = 39,
    #[strum(serialize = "flat envmap matrix x")]             FlatEnvmapMatrixX                    = 40,
    #[strum(serialize = "flat envmap matrix y")]             FlatEnvmapMatrixY                    = 41,
    #[strum(serialize = "flat envmap matrix z")]             FlatEnvmapMatrixZ                    = 42,
    #[strum(serialize = "debug tint")]                       DebugTint                            = 43,
    #[strum(serialize = "screen constants")]                 ScreenConstants                      = 44,
    #[strum(serialize = "active camo distortion texture")]   ActiveCamoDistortionTexture          = 45,
    #[strum(serialize = "scene ldr texture")]                SceneLdrTexture                      = 46,
    #[strum(serialize = "scene hdr texture")]                SceneHdrTexture                      = 47,
    #[strum(serialize = "water memexport addr")]             WaterMemoryExportAddress             = 48,
    #[strum(serialize = "tree animation timer")]             TreeAnimationTimer                   = 49,
    // Gen3 remaster externs (ODST/Reach/H4/H2A). Appended rather than inserted:
    // the rmop `source extern` decode matches by strum NAME, so these only need
    // unique indices here — the per-game schema index order differs and is the
    // index-only fields' concern, not this name-bearing one.
    #[strum(serialize = "depth constants")]                  DepthConstants                       = 50,
    #[strum(serialize = "camera forward")]                   CameraForward                        = 51,
    #[strum(serialize = "shadow mask")]                      ShadowMask                           = 52,
    #[strum(serialize = "dualvmf direction ps")]             DualVmfDirectionPs                   = 53,
    #[strum(serialize = "dualvmf intensity ps")]             DualVmfIntensityPs                   = 54,
    #[strum(serialize = "dualvmf direction vs")]             DualVmfDirectionVs                   = 55,
    #[strum(serialize = "dualvmf intensity vs")]             DualVmfIntensityVs                   = 56,
    #[strum(serialize = "gel texture of analytical light")]  GelTextureOfAnalyticalLight          = 57,
    #[strum(serialize = "cook torrance array")]              CookTorranceArray                    = 58,
    #[strum(serialize = "vmf diffuse table")]                VmfDiffuseTable                      = 59,
    #[strum(serialize = "vmf diffuse table vs")]             VmfDiffuseTableVs                    = 60,
    #[strum(serialize = "direction lut")]                    DirectionLut                         = 61,
    #[strum(serialize = "zonal rotation table")]             ZonalRotationTable                   = 62,
    #[strum(serialize = "phong specular table")]             PhongSpecularTable                   = 63,
    #[strum(serialize = "diffuse power specular table")]     DiffusePowerSpecularTable            = 64,
    #[strum(serialize = "wrinkle weights a")]                WrinkleWeightsA                      = 65,
    #[strum(serialize = "wrinkle weights b")]                WrinkleWeightsB                      = 66,
    #[strum(serialize = "static lighting previs")]           StaticLightingPrevis                 = 67,
    #[strum(serialize = "emblem bitmaps and data")]          EmblemBitmapsAndData                 = 68,
    // Names the *shipped H3 tags* offer that the Ares list above does not, taken
    // from the option lists embedded in the H3 editing kit's
    // `render_method_option` tags. Those are a different vintage of the schema: H3
    // predates the lightprobe/dominant-light externs and binds lightmaps and
    // irradiance maps instead, and its change-color set has animated variants.
    //
    // Appended, like the block above, because the decode matches by name. Missing
    // one is not a degraded read but a panic in `Enum::resolve`: three H3 option
    // tags select `bounding sphere` / `change color primary anim` / `change color
    // secondary anim`, and reading any of them took the caller down.
    #[strum(serialize = "unused 0")]                         TextureUnused0                       = 69,
    #[strum(serialize = "lightmap")]                         TextureLightmap                      = 70,
    #[strum(serialize = "irradiance map red")]               TextureIrradianceMapRed              = 71,
    #[strum(serialize = "irradiance map green")]             TextureIrradianceMapGreen            = 72,
    #[strum(serialize = "irradiance map blue")]              TextureIrradianceMapBlue             = 73,
    #[strum(serialize = "irradiance map extra red")]         TextureIrradianceMapExtraRed         = 74,
    #[strum(serialize = "irradiance map extra green")]       TextureIrradianceMapExtraGreen       = 75,
    #[strum(serialize = "irradiance map extra blue")]        TextureIrradianceMapExtraBlue        = 76,
    #[strum(serialize = "ibr target")]                       TextureGlobalTargetIbr               = 77,
    #[strum(serialize = "cheap texture camera Z")]            TextureGlobalTargetCheapTextureCameraZ = 78,
    #[strum(serialize = "emblem clan chest texture")]        TextureEmblemClanChest               = 79,
    #[strum(serialize = "emblem player shoulder texture")]   TextureEmblemPlayerShoulder          = 80,
    #[strum(serialize = "bounding sphere")]                  ObjectBoundingSphere                 = 81,
    #[strum(serialize = "change color primary anim")]        ObjectChangeColorPrimaryAnim         = 82,
    #[strum(serialize = "change color secondary anim")]      ObjectChangeColorSecondaryAnim       = 83,
}

impl RenderMethodExtern {
    pub const COUNT: usize = 84;

    /// Map by index. **Only for raw integer fields that carry no embedded
    /// name** — the rmsh `bitmap extern RTT mode` / postprocess `extern
    /// texture mode` are plain `short_integer`/`char_integer` (already-
    /// upgraded runtime indices, rebuilt by the tools on save), so the
    /// index is the current-schema index. The name-bearing rmop `source
    /// extern` field is resolved by name instead (drift-immune) via the
    /// typed [`Enum`] wrapper. Discriminants 0..49 match the JSON schema
    /// (including `emblem color background` at 20); everything above that is
    /// appended name-only space, which an index-only field never reaches.
    pub fn from_index(i: i128) -> Option<Self> {
        if !(0..Self::COUNT as i128).contains(&i) {
            return None;
        }
        // Safe: enum is `repr(u32)` with sequential discriminants from 0.
        Some(unsafe { std::mem::transmute::<u32, Self>(i as u32) })
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

/// Bitmap sampler filter mode. The schema enum
/// `render_method_bitmap_filter_mode_enum` (rmop `default filter mode`,
/// name-resolved); the rmsh per-instance `bitmap filter mode` is a raw
/// `short_integer` (no embedded names) decoded positionally via
/// [`Self::from_index`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum BitmapFilterMode {
    #[default]
    #[strum(serialize = "trilinear")]                  Trilinear              = 0,
    #[strum(serialize = "point")]                      Point                  = 1,
    #[strum(serialize = "bilinear")]                   Bilinear               = 2,
    #[strum(serialize = "anisotropic (1)")]            Anisotropic1           = 3,
    #[strum(serialize = "anisotropic (2) expensive")]  Anisotropic2Expensive  = 4,
    #[strum(serialize = "anisotropic (3) Expensive")]  Anisotropic3Expensive  = 5,
    #[strum(serialize = "anisotropic (4) EXPENSIVE")]  Anisotropic4Expensive  = 6,
    #[strum(serialize = "lightprobe texture array")]   LightprobeTextureArray = 7,
    #[strum(serialize = "comparison point")]           ComparisonPoint        = 8,
    #[strum(serialize = "comparison bilinear")]        ComparisonBilinear     = 9,
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

/// Bitmap sampler address mode. The schema enum
/// `render_method_bitmap_address_mode_enum` (rmop `default address mode`,
/// name-resolved); the rmsh per-instance `bitmap address mode[ x/y]` and
/// the packed postprocess `address mode` nibbles are raw integers decoded
/// positionally via [`Self::from_index`].
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum BitmapAddressMode {
    #[default]
    #[strum(serialize = "wrap")]         Wrap        = 0,
    #[strum(serialize = "clamp")]        Clamp       = 1,
    #[strum(serialize = "mirror")]       Mirror      = 2,
    #[strum(serialize = "black border")] BlackBorder = 3,
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

/// Bitmap sampler comparison function. The schema enum
/// `render_method_bitmap_comparison_function_enum` (rmop `default
/// comparison function`, name-resolved); the rmsh per-instance forms are
/// raw integers decoded positionally via [`Self::from_index`]. Used by
/// the comparison-filter sampler modes for shadow / depth fetches.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum BitmapComparisonFunction {
    #[default]
    #[strum(serialize = "never")]         Never        = 0,
    #[strum(serialize = "less")]          Less         = 1,
    #[strum(serialize = "equal")]         Equal        = 2,
    #[strum(serialize = "less equal")]    LessEqual    = 3,
    #[strum(serialize = "greater")]       Greater      = 4,
    #[strum(serialize = "not equal")]     NotEqual     = 5,
    #[strum(serialize = "greater equal")] GreaterEqual = 6,
    #[strum(serialize = "always")]        Always       = 7,
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

/// Runtime rasterizer alpha-blend mode — Bungie `e_alpha_blend_mode`
/// (`rasterizer.h`). This is the value the rasterizer switches on in
/// `c_rasterizer::set_alpha_blend_mode_no_cache @ dllcache 0x18066E930`
/// (decompile-verified 2026-06-10), **not** the authored `blend_mode`
/// shader-category index — the tool BAKES the category choice into this
/// enum at the `rmt2` pass `alpha blend mode` field and the rmsh
/// postprocess `blend mode` field.
///
/// The strum names match the `blend_mode` render-method category's
/// option names, so [`super::RenderMethodChoices::blend_mode`] resolves
/// the authored choice to this same enum **by name** (drift-proof),
/// while [`Self::from_index`] decodes the baked `i32`.
///
/// Value order cross-checked against the dllcache packed-state words:
/// options 5 and 7 (`pre_multiplied_alpha`, `multiply_add`) share one
/// D3D blend word (`1016612034`); 6 (`maximum`) is distinct
/// (`1049121858`) — confirming `maximum` sits between them at value 6,
/// not the other way around.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
pub enum AlphaBlendMode {
    #[default]
    #[strum(serialize = "opaque")]
    Opaque = 0,
    #[strum(serialize = "additive")]
    Additive = 1,
    #[strum(serialize = "multiply")]
    Multiply = 2,
    #[strum(serialize = "alpha_blend", serialize = "alpha blend")]
    AlphaBlend = 3,
    #[strum(serialize = "double_multiply", serialize = "double multiply")]
    DoubleMultiply = 4,
    #[strum(serialize = "pre_multiplied_alpha", serialize = "pre multiplied alpha")]
    PreMultipliedAlpha = 5,
    #[strum(serialize = "maximum")]
    Maximum = 6,
    #[strum(serialize = "multiply_add", serialize = "multiply add")]
    MultiplyAdd = 7,
    #[strum(serialize = "add_src_times_dstalpha", serialize = "add src times dstalpha")]
    AddSrcTimesDstAlpha = 8,
    #[strum(serialize = "add_src_times_srcalpha", serialize = "add src times srcalpha")]
    AddSrcTimesSrcAlpha = 9,
    #[strum(serialize = "inv_alpha_blend", serialize = "inv alpha blend")]
    InvAlphaBlend = 10,
    #[strum(serialize = "motion_blur_static", serialize = "motion blur static")]
    MotionBlurStatic = 11,
    #[strum(serialize = "motion_blur_inhibit", serialize = "motion blur inhibit")]
    MotionBlurInhibit = 12,
}

impl AlphaBlendMode {
    pub const COUNT: usize = 13;

    /// Decode the BAKED runtime index (rmt2 pass `alpha blend mode` /
    /// postprocess `blend mode` — plain integers, no embedded name).
    /// For the AUTHORED category choice, resolve by name via
    /// [`std::str::FromStr`] / [`super::RenderMethodChoices::blend_mode`]
    /// instead.
    pub fn from_index(i: i128) -> Option<Self> {
        if !(0..Self::COUNT as i128).contains(&i) {
            return None;
        }
        // Safe: enum is `repr(u32)` with sequential 0..13 discriminants.
        Some(unsafe { std::mem::transmute::<u32, Self>(i as u32) })
    }
}

/// The concrete `rm**` subclass. A typed discriminator that replaces
/// raw-fourcc dispatch on [`RenderMethod::group_tag`].
///
/// Standalone-present in the shipped H3 tag set (have on-disk files):
/// Shader/Terrain/Water/Decal/Halogram/Cortana/Custom/Foliage + base.
/// Embedded-only (zero standalone tags across H3/Reach/H4 — carried
/// inside their container tag): Skin/Beam/LightVolume/Particle/Contrail.
/// The last two have only *nominal* `?rmp`/`?rmc` schema-registry slots
/// (a `0x3f` sentinel first byte) that never appear on disk; they get
/// distinct discriminants here instead of being aliased onto another
/// group's fourcc (the old code stamped Contrail's embedded shader as
/// `rmct`, colliding with Cortana).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RenderMethodClass {
    #[default]
    Base, // rm
    Shader,      // rmsh
    Terrain,     // rmtr
    Water,       // rmw
    Decal,       // rmd
    Halogram,    // rmhg
    Cortana,     // rmct
    Custom,      // rmcs
    Foliage,     // rmfl
    Skin,        // rmsk (embedded)
    Beam,        // rmb  (embedded)
    LightVolume, // rmlv (embedded)
    Particle,    // embedded (nominal ?rmp)
    Contrail,    // embedded (nominal ?rmc)
}

impl RenderMethodClass {
    /// Resolve from a standalone tag's group fourcc. Returns `None` for
    /// the embedded-only nominal slots (`?rmp`/`?rmc`) and anything
    /// unrecognized — embedded subclasses are set by their container
    /// walker, not by a disk group.
    pub fn from_group_tag(tag: [u8; 4]) -> Option<Self> {
        Some(match &tag {
            b"rm  " => Self::Base,
            b"rmsh" => Self::Shader,
            b"rmtr" => Self::Terrain,
            b"rmw " => Self::Water,
            b"rmd " => Self::Decal,
            b"rmhg" => Self::Halogram,
            b"rmct" => Self::Cortana,
            b"rmcs" => Self::Custom,
            b"rmfl" => Self::Foliage,
            b"rmsk" => Self::Skin,
            b"rmb " => Self::Beam,
            b"rmlv" => Self::LightVolume,
            _ => return None,
        })
    }

    /// The standalone on-disk group fourcc when this subclass exists as a
    /// file; `None` for embedded-only subclasses (Skin/Beam/LightVolume/
    /// Particle/Contrail).
    pub fn standalone_group(self) -> Option<[u8; 4]> {
        Some(match self {
            Self::Base => *b"rm  ",
            Self::Shader => *b"rmsh",
            Self::Terrain => *b"rmtr",
            Self::Water => *b"rmw ",
            Self::Decal => *b"rmd ",
            Self::Halogram => *b"rmhg",
            Self::Cortana => *b"rmct",
            Self::Custom => *b"rmcs",
            Self::Foliage => *b"rmfl",
            Self::Skin | Self::Beam | Self::LightVolume | Self::Particle | Self::Contrail => {
                return None
            }
        })
    }

    /// How many `sted` `material name` slots this subclass's outer struct
    /// carries: 4 for terrain (one per blend channel), 1 for the
    /// material-bearing shaders, 0 for the rest.
    fn material_name_count(self) -> usize {
        match self {
            Self::Terrain => 4,
            Self::Shader | Self::Halogram | Self::Cortana | Self::Custom | Self::Foliage
            | Self::Skin => 1,
            _ => 0,
        }
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
    pub parameter_type: Option<Enum<RenderMethodParameterType, i32>>,
    pub bitmap_path: String,
    pub real_parameter: f32,
    pub int_parameter: i32,
    /// The per-instance authored color (ARGB). Falls back to opaque-black
    /// when the slot has no `color` field.
    pub color_parameter: ArgbColor,
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
    pub parameter_type: Option<Enum<RenderMethodParameterType, i32>>,
    /// When non-`None`, this parameter is sourced from an engine extern
    /// rather than baked/animated values.
    pub source_extern: Option<Enum<RenderMethodExtern, i32>>,
    pub default_bitmap_path: String,
    pub default_real_value: f32,
    pub default_int_bool_value: i32,
    pub flags: i16,
    pub default_filter_mode: Enum<BitmapFilterMode, i16>,
    pub default_comparison_function: Enum<BitmapComparisonFunction, i16>,
    pub default_address_mode: Enum<BitmapAddressMode, i16>,
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
    pub parameter_type: Option<Enum<RenderMethodAnimatedParameterType, i32>>,
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
    pub available_entry_points: Flags<EntryPointFlags, u32>,
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
    pub flags: Flags<GlobalRenderMethodFlags, u16>,
    pub sort_layer: Enum<GlobalSortLayer, i8>,
    pub runtime_flags: Flags<GlobalRenderMethodRuntimeFlags, u8>,
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
    /// Typed subclass discriminator. Resolved from `group_tag` by
    /// [`Self::from_tag`]; [`RenderMethodClass::Base`] for `from_struct`
    /// callers (embedded walkers set it explicitly). Prefer this over
    /// matching `group_tag` — it is collision-free where the raw fourccs
    /// alias (Contrail vs Cortana).
    pub class: RenderMethodClass,
    /// `sted` global-material name(s) from the subclass's OUTER struct:
    /// 4 for terrain (one per blend channel 0-3), 1 for shader/halogram/
    /// cortana/custom/foliage/skin, empty otherwise. Only populated by
    /// [`Self::from_tag`] (the outer fields live above the embedded
    /// `c_render_method` and aren't reachable from `from_struct`).
    pub material_names: Vec<String>,
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
///
/// These are the REAL group fourccs (cross-checked against the shipped
/// tag set + `_meta.json`). The five embedded-only subclasses
/// (Skin/Beam/LightVolume/Particle/Contrail — zero standalone files in
/// H3/Reach/H4) are kept here for `from_tag` robustness; particle and
/// contrail use their nominal `?rmp`/`?rmc` schema slots (a `0x3f`
/// leading byte) rather than the previous bogus `rmp `/`rmco` stand-ins
/// (`rmco` is not a real tag, and the old list aliased contrail onto a
/// nonexistent group while `contrail_system` stamped it as `rmct` =
/// cortana). Standalone files of these never exist; the container
/// walkers build them via `from_struct` and set `class` directly.
const RENDER_METHOD_GROUPS: &[[u8; 4]] = &[
    GROUP_RM,
    GROUP_RMSH,                     // rmsh shader
    *b"rmtr", *b"rmw ", *b"rmfl",   // terrain, water, foliage
    *b"rmd ", *b"rmhg", *b"rmsk",   // decal, halogram, skin
    *b"rmct", *b"rmcs",             // cortana, custom
    *b"rmb ", *b"rmlv",             // beam, light_volume (embedded-only)
    *b"\x3frmp", *b"\x3frmc",       // particle, contrail — nominal ?rmp/?rmc
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
        out.class = RenderMethodClass::from_group_tag(group_tag.to_be_bytes())
            .unwrap_or_default();
        // Read the subclass's OUTER `sted` material name(s) — they live
        // on `root` (the subclass struct), siblings of the embedded
        // `render_method` field, so they're only reachable here (not in
        // `from_struct`). Terrain carries 4 (one per blend channel),
        // the material-bearing shaders 1; others none. Field names are
        // stored clean (no `#doc` suffix), verified against the H3 tags.
        let count = out.class.material_name_count();
        if count == 1 {
            if let Some(name) = root.read_string_id("material name") {
                out.material_names.push(name);
            }
        } else if count == 4 {
            for i in 0..4 {
                let name = root
                    .read_string_id(&format!("material name {i}"))
                    .unwrap_or_default();
                out.material_names.push(name);
            }
        }
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

        // Field-name lookups are markup-insensitive (field_name_matches cleans
        // both sides), so the bare name matches whether a tag stored the clean
        // form or kept a `*`/`!` marker — no need to try both forms.
        let flags = s.try_read_flags("shader flags").unwrap_or_default();
        let sort_layer = s.try_read_enum("sort layer").unwrap_or_default();
        let runtime_flags = s.try_read_flags("runtime flags").unwrap_or_default();
        let custom_fog_setting_index = s.read_int_any("Custom fog setting index")
            .unwrap_or(0) as i32;
        let prediction_atom_index = s.read_int_any("prediction atom index")
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
            class: RenderMethodClass::Base, // overridden by from_tag / embedded walkers
            material_names: Vec::new(),      // outer-struct fields: from_tag only
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
            parameter_type: s.try_read_enum("parameter type"),
            source_extern: s.try_read_enum("source extern"),
            default_bitmap_path: s.read_tag_ref_path("default bitmap").unwrap_or_default(),
            default_real_value: s.read_real("default real value").unwrap_or(0.0),
            default_int_bool_value: s.read_int_any("default int/bool value").unwrap_or(0) as i32,
            flags: s.read_int_any("flags").unwrap_or(0) as i16,
            // rmop defaults are true schema enums — resolve by embedded
            // name (drift-immune).
            default_filter_mode: s.try_read_enum("default filter mode").unwrap_or_default(),
            default_comparison_function: s
                .try_read_enum("default comparison function")
                .unwrap_or_default(),
            default_address_mode: s.try_read_enum("default address mode").unwrap_or_default(),
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
            available_entry_points: s.try_read_flags("available entry points")
                .or_else(|| s.try_read_flags("available entry points*"))
                .or_else(|| pc_platform.as_ref().and_then(|p| p.try_read_flags("available entry points")))
                .or_else(|| pc_platform.as_ref().and_then(|p| p.try_read_flags("available entry points*")))
                .unwrap_or_default(),
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
            parameter_type: s.try_read_enum("parameter type"),
            bitmap_path: s.read_tag_ref_path("bitmap").unwrap_or_default(),
            real_parameter: s.read_real("real").unwrap_or(0.0),
            int_parameter: s.read_int_any("int/bool").unwrap_or(0) as i32,
            color_parameter: match s.field("color").and_then(|f| f.value()) {
                Some(crate::fields::TagFieldData::ArgbColor(c)) => c,
                Some(crate::fields::TagFieldData::RgbColor(c)) => ArgbColor(c.0 | 0xFF00_0000),
                _ => ArgbColor(0),
            },
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
            parameter_type: s.try_read_enum("type"),
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

#[cfg(test)]
mod alpha_blend_mode_tests {
    use super::AlphaBlendMode;
    use std::str::FromStr;

    /// The runtime `e_alpha_blend_mode` value order (dllcache
    /// `set_alpha_blend_mode_no_cache @ 0x18066E930`). Locks `maximum`
    /// at 6 between `pre_multiplied_alpha` (5) and `multiply_add` (7).
    #[test]
    fn runtime_enum_value_order() {
        for (i, expected) in [
            (0, AlphaBlendMode::Opaque),
            (1, AlphaBlendMode::Additive),
            (2, AlphaBlendMode::Multiply),
            (3, AlphaBlendMode::AlphaBlend),
            (4, AlphaBlendMode::DoubleMultiply),
            (5, AlphaBlendMode::PreMultipliedAlpha),
            (6, AlphaBlendMode::Maximum),
            (7, AlphaBlendMode::MultiplyAdd),
            (8, AlphaBlendMode::AddSrcTimesDstAlpha),
            (9, AlphaBlendMode::AddSrcTimesSrcAlpha),
            (10, AlphaBlendMode::InvAlphaBlend),
            (11, AlphaBlendMode::MotionBlurStatic),
            (12, AlphaBlendMode::MotionBlurInhibit),
        ] {
            assert_eq!(AlphaBlendMode::from_index(i), Some(expected), "index {i}");
        }
        assert_eq!(AlphaBlendMode::from_index(13), None);
        assert_eq!(AlphaBlendMode::from_index(-1), None);
    }

    /// Names resolve to the runtime value regardless of where the
    /// authored category lists them — the crux of why positional decode
    /// is wrong. In `particle.rmdf`, `pre_multiplied_alpha` is the
    /// 11th `blend_mode` option (category index 10), but it is runtime
    /// value 5; a positional read would mis-decode it as `inv_alpha_blend`
    /// (runtime 10). Name resolution must give `PreMultipliedAlpha`.
    #[test]
    fn name_beats_category_index() {
        assert_eq!(
            AlphaBlendMode::from_str("pre_multiplied_alpha"),
            Ok(AlphaBlendMode::PreMultipliedAlpha)
        );
        // The value at that category index, had we (wrongly) decoded
        // positionally, is a DIFFERENT mode — proving the divergence.
        assert_eq!(AlphaBlendMode::from_index(10), Some(AlphaBlendMode::InvAlphaBlend));
        assert_ne!(
            AlphaBlendMode::from_str("pre_multiplied_alpha").unwrap(),
            AlphaBlendMode::from_index(10).unwrap()
        );
        // Same story for `maximum` (category 5 in particle.rmdf, runtime 6).
        assert_eq!(AlphaBlendMode::from_str("maximum"), Ok(AlphaBlendMode::Maximum));
        assert_eq!(AlphaBlendMode::from_index(5), Some(AlphaBlendMode::PreMultipliedAlpha));
    }

    /// Both underscore (tag option_name) and space (schema) forms parse,
    /// case-insensitively.
    #[test]
    fn name_aliases() {
        assert_eq!(AlphaBlendMode::from_str("add_src_times_dstalpha"), Ok(AlphaBlendMode::AddSrcTimesDstAlpha));
        assert_eq!(AlphaBlendMode::from_str("add src times dstalpha"), Ok(AlphaBlendMode::AddSrcTimesDstAlpha));
        assert_eq!(AlphaBlendMode::from_str("ALPHA_BLEND"), Ok(AlphaBlendMode::AlphaBlend));
        assert!(AlphaBlendMode::from_str("nonsense").is_err());
    }
}

#[cfg(test)]
mod render_method_extern_tests {
    use super::RenderMethodExtern;

    /// Every `source extern` name the shipped H3 editing kit's option tags offer.
    ///
    /// Collected from the option lists embedded in all 176 `render_method_option`
    /// and `render_method_definition` tags in the H3 kit. This is not decoration:
    /// `Enum::resolve` panics on a name with no variant, so a missing one takes
    /// down whatever was reading the tag. Three of these are selected by shipped
    /// tags today; the rest are one user edit away from being selected.
    #[test]
    fn every_h3_source_extern_name_resolves() {
        use std::str::FromStr;
        const H3_EMBEDDED_NAMES: &[&str] = &[
            "none", "texaccum target", "z target", "shadow 1 target", "shadow 2 target",
            "shadow 3 target", "shadow 4 target", "texture camera target", "reflection target",
            "refraction target", "lightmap", "irradiance map red", "irradiance map green",
            "irradiance map blue", "change color primary", "change color secondary",
            "change color tertiary", "change color quaternary", "dynamic environment map 1",
            "dynamic environment map 2", "cook torrance cc0236", "cook torrance dd0236",
            "cook torrance c78d78", "light dir 0", "light color 0", "light dir 1",
            "light color 1", "light dir 2", "light color 2", "light dir 3", "light color 3",
            "irradiance map extra red", "irradiance map extra green",
            "irradiance map extra blue", "dynamic light gel 0", "flat envmap matrix x",
            "flat envmap matrix y", "flat envmap matrix z", "debug tint", "screen constants",
            // Offered by the longer lists other option tags in the same kit embed.
            "unused 0", "ibr target", "cheap texture camera Z", "emblem clan chest texture",
            "emblem player shoulder texture", "bounding sphere", "change color primary anim",
            "change color secondary anim",
        ];
        for name in H3_EMBEDDED_NAMES {
            assert!(
                RenderMethodExtern::from_str(name).is_ok(),
                "no RenderMethodExtern variant for the shipped H3 name {name:?}"
            );
        }
    }

    /// The appended name-only space must not disturb index-only decoding, which
    /// reads the same enum by position.
    #[test]
    fn appended_externs_leave_the_indexed_range_alone() {
        assert_eq!(RenderMethodExtern::from_index(0), Some(RenderMethodExtern::None));
        assert_eq!(
            RenderMethodExtern::from_index(20),
            Some(RenderMethodExtern::ObjectEmblemColorBackground)
        );
        assert_eq!(
            RenderMethodExtern::from_index(68),
            Some(RenderMethodExtern::EmblemBitmapsAndData)
        );
        assert_eq!(RenderMethodExtern::from_index(RenderMethodExtern::COUNT as i128), None);
    }
}
