//! `camera_fx_settings` (`cfxs`) tag walker — per-level exposure,
//! bloom, and tone curve. Pointed at by `scenario.camera_fx_settings`.
//!
//! Halo's render path (per dllcache):
//! - `c_player_view::setup_camera_fx_parameters @ 0x180689c20` reads
//!   the scenario's camera_fx_settings and applies it to the player's
//!   `m_camera_fx_values`.
//! - `c_camera_fx_values::get_render_exposure @ 0x18068e3e0` then
//!   computes the per-frame view_exposure as:
//!   `pow(2, scripted + g_exposure_stops + exposure_boost)
//!    × tone_curve_white_point × 0.66943294`.
//! - `c_rasterizer::setup_render_target_globals_with_exposure @ 0x180670ad0`
//!   uploads `(view_exposure, pow(2, HDR_target_stops), 1, 1)` to
//!   shader cbuffer slot 0x28 (`g_exposure`).
//!
//! For now we walk only the exposure block — the rest of the tag
//! (bloom, bling, tone curve) lands when those passes go in.

use crate::api::TagStruct;
use crate::fields::TagFieldType;
use crate::file::TagFile;
use crate::math::RealRgbColor;
use crate::tag_function::TagFunction;

const CFXS_GROUP: [u8; 4] = *b"cfxs";

#[derive(Debug)]
pub enum CameraFxError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for CameraFxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "expected group '{}', got '{}'",
                std::str::from_utf8(expected).unwrap_or("?"),
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
        }
    }
}
impl std::error::Error for CameraFxError {}

/// `c_camera_fx_settings::s_real_parameter` (16B) — the parameter
/// block for every "value + max_change + blend_speed + flags"
/// authored slider in cfxs (bloom_point/inherent/intensity,
/// bling_intensity/size/angle, self_illum_*). Engine
/// `c_camera_fx_values::update @ 0x180687CB0` reads ALL these
/// fields per frame when blending the runtime `c_camera_fx_values`
/// toward the cfxs target — dropping any of them breaks faithful
/// per-frame parameter interpolation.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScalarParameter {
    /// `camera_fx_parameter_flags_no_auto_adjust`. Bit 0 = `use
    /// default (ignore these values)` — engine treats the parameter
    /// as absent when set, falling through to the default cfxs.
    pub flags: u16,
    /// Authored target value (units depend on the parameter — stops,
    /// HDR multiplier, length, angle, etc.).
    pub value: f32,
    /// Maximum delta the runtime value can move per frame.
    pub max_change: f32,
    /// Blend rate per frame (1 = instantaneous, 0.01 = slow).
    pub blend_speed: f32,
}

/// `c_camera_fx_settings::s_real_instant_parameter` (8B) — the
/// snap-to-target variant (no blending). Used by
/// `auto_exposure_sensitivity` and `auto_exposure_anti_bloom`.
#[derive(Debug, Clone, Copy, Default)]
pub struct InstantScalarParameter {
    pub flags: u16,
    pub value: f32,
}

/// `c_camera_fx_settings::s_color_parameter` (16B + flags) — the
/// per-stage bloom color overrides. The `flags` `use default`
/// bit (0x01) tells the engine to ignore the per-stage color and
/// substitute the global default (1, 1, 1).
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorParameter {
    pub flags: u16,
    pub color: RealRgbColor,
}

impl ColorParameter {
    /// Per the schema's `use default` bit (0x01) — when set, engine
    /// ignores `color` and substitutes (1, 1, 1).
    pub fn use_default(&self) -> bool {
        (self.flags & 0x01) != 0
    }

    /// The effective color the engine actually applies — `(1,1,1)`
    /// when `use_default` is set, else the authored color.
    pub fn effective_color(&self) -> RealRgbColor {
        if self.use_default() {
            RealRgbColor { red: 1.0, green: 1.0, blue: 1.0 }
        } else {
            self.color
        }
    }
}

/// `c_camera_fx_settings::s_word_parameter` (4B) — bling spike count.
#[derive(Debug, Clone, Copy, Default)]
pub struct WordParameter {
    pub flags: u16,
    pub value: u16,
}

/// Decoded `.camera_fx_settings`. Carries every authored block —
/// callers pull `.value` (or sub-fields) when they need the scalar.
#[derive(Debug, Clone, Default)]
pub struct CameraFxSettings {
    /// Exposure block — auto-exposure tuning + flags + min/max stops.
    pub exposure: ExposureBlock,
    /// `auto_exposure_sensitivity` — controls how shader 35
    /// (`exposure_downsample`) lerps between mean(log) and log(mean)
    /// average-luminance interpretations.
    pub auto_exposure_sensitivity: InstantScalarParameter,
    /// `auto_exposure_anti_bloom` — reduces overexposure bloom from
    /// elements that don't bloom at exposure_stops = 0.
    pub auto_exposure_anti_bloom: InstantScalarParameter,
    /// Bright threshold (typically 1.5 HDR units). Above this point
    /// the bloom curve kicks in heavily.
    pub bloom_point: ScalarParameter,
    /// Additional bloom-curve contribution on intensities BELOW the
    /// bloom point (riverworld: 0.1).
    pub bloom_inherent: ScalarParameter,
    /// Global bloom multiplier vs. underlying scene (riverworld: 0.1).
    pub bloom_intensity: ScalarParameter,
    /// Per-stage bloom color tints — composited over the global
    /// large/medium/small bloom result. When `use_default` is set,
    /// engine substitutes (1, 1, 1).
    pub bloom_large_color: ColorParameter,
    pub bloom_medium_color: ColorParameter,
    pub bloom_small_color: ColorParameter,
    /// Bling (sun-disc / lens spike) intensity multiplier.
    pub bling_intensity: ScalarParameter,
    /// Spike length in 1/2-res pixels.
    pub bling_size: ScalarParameter,
    /// Spike rotation, in degrees (schema-named "bling angle").
    pub bling_angle_deg: ScalarParameter,
    /// Number of spikes (typically 3 or 4).
    pub bling_count: WordParameter,
    /// Self-illumination preferred exposure stops + blend tuning.
    pub self_illum_preferred: ScalarParameter,
    /// `Self illum change` — `[0,1]` cap on how much the self-illum
    /// exposure is allowed to track the scene exposure.
    pub self_illum_scale: ScalarParameter,
    /// `ssao` sub-struct. Older tags (riverworld) have it absent — `None`.
    pub ssao: Option<SsaoBlock>,
    /// `lightshafts` sub-struct. Same shape — older tags omit it.
    pub lightshafts: Option<LightshaftsBlock>,
    /// `color grading` sub-struct. `None` if the cfxs tag predates the
    /// color-grading addition (early H3 maps including riverworld) or
    /// the field walker can't find it. Older shorter tags fall back to
    /// identity LUT (engine `update_color_grading` does the same when
    /// `(m_flags & 2) == 0`).
    pub color_grading: Option<ColorGradingBlock>,
}

/// `s_ssao_parameter` (cfxs sub-struct, schema size 16B).
/// Engine: `c_screen_postprocess::render_ssao @ 0x1806b50e0` reads
/// these per frame. `flags & 2 == 0` → SSAO disabled (return early).
#[derive(Debug, Clone, Copy, Default)]
pub struct SsaoBlock {
    /// Bit 1 (`& 2`) = enable.
    pub flags: u32,
    /// AO darkening multiplier. Tuned per-cfxs.
    pub intensity: f32,
    /// World-space sample radius. Larger = wider AO falloff.
    pub radius: f32,
    /// Z-threshold for sample acceptance — samples beyond this depth
    /// delta are treated as "different surface" and rejected. Engine
    /// passes `1/sample_z_threshold` to the SSAO shader.
    pub sample_z_threshold: f32,
}

impl SsaoBlock {
    pub fn is_enabled(&self) -> bool {
        (self.flags & 2) != 0
    }
}

/// `s_lightshafts` (cfxs sub-struct, schema size 44B).
/// Engine: `c_screen_postprocess::render_lightshafts @ 0x1806b55c0`.
/// `flags & 2 == 0` → disabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct LightshaftsBlock {
    /// Bit 1 (`& 2`) = enable.
    pub flags: u32,
    /// Light source pitch in degrees `[0, 90]` (vertical angle).
    pub pitch: f32,
    /// Light source heading in degrees `[0, 360]` (horizontal angle).
    pub heading: f32,
    /// God-ray tint color.
    pub tint: RealRgbColor,
    /// World-space depth clamp — samples beyond this are excluded.
    pub depth_clamp: f32,
    /// Intensity clamp `[0, 1]`.
    pub intensity_clamp: f32,
    /// Falloff radius `[0, 2]` (relative to screen space).
    pub falloff_radius: f32,
    /// Overall ray intensity `[0, 50]`.
    pub intensity: f32,
    /// Blur radius `[0, 20]` (post-pass blur to soften the rays).
    pub blur_radius: f32,
}

impl LightshaftsBlock {
    pub fn is_enabled(&self) -> bool {
        (self.flags & 2) != 0
    }
}

/// `s_color_grading_parameter` (cfxs sub-struct, schema size 80B).
///
/// Mirrors the `color_grading_*_block`s nested inside the `color
/// grading` field. Riverworld's tag predates this struct so it lands
/// as `None`; campaign maps and `s3d_*` Forge tags carry it. Engine
/// reads in `c_screen_postprocess::update_color_grading` (per-texel
/// bake of the 16³ LUT).
#[derive(Debug, Clone, Default)]
pub struct ColorGradingBlock {
    /// Bit 1 (`& 2`) = enable. `c_screen_postprocess::update_color_grading
    /// @ dllcache:146` checks `(pColorGrading->m_flags & 2) == 0` and
    /// falls through to the identity-LUT fast path when unset.
    pub flags: u32,
    /// Authored cross-fade duration (in seconds) when transitioning
    /// between color-grading settings. Engine: `g_fColorGradingBlendFactor`.
    pub blend_time: f32,
    /// `Curves editor` block (max_count=1). 4 TagFunctions —
    /// brightness + per-RGB curves. Mode picks which to apply.
    pub curves_editor: Option<CurvesEditorBlock>,
    /// `Brightness, contrast` block (max_count=1). `None` if absent.
    pub brightness_contrast: Option<BrightnessContrast>,
    /// `Hue, saturation, lightness, vibrance` block. NOTE: the schema
    /// labels offset 12 as "lightness" and offset 16 as "vibrance" but
    /// the engine reads them as saturation-multiplier (vibrance) and
    /// lightness-add respectively — schema labels are swapped vs
    /// `update_color_grading`'s actual math. Field names below follow
    /// the *engine* semantics.
    pub hslv: Option<HslvBlock>,
    /// `Colorize effect` block. NOTE: same swap as HSLV — schema's
    /// "saturation" slider is at the offset the engine reads as the
    /// L-target, and vice versa. Field names follow engine semantics.
    pub colorize: Option<ColorizeBlock>,
    /// `Selective color` block — per-color-zone CMYK adjust, applied
    /// post-HSL→RGB via `ProcessSelectiveColor`.
    pub selective_color: Option<SelectiveColorBlock>,
    /// `Color balance` block — shadows/midtones/highlights CMY adjust,
    /// applied as a per-channel pow remap built from `Tones[3][3]`.
    pub color_balance: Option<ColorBalanceBlock>,
}

impl ColorGradingBlock {
    /// `(m_flags & 2) != 0` — engine's gate before applying any of the
    /// per-effect blocks.
    pub fn is_enabled(&self) -> bool {
        (self.flags & 2) != 0
    }
}

/// `Curves editor` block (88B). 4 mapping curves + a mode enum.
/// Engine: `update_color_grading:226-269` — when mode == 1 (Brightness)
/// applies `brightness` to all 3 channels; otherwise applies `red`,
/// `green`, `blue` per channel. Each curve is evaluated with
/// `range = 1.0`.
#[derive(Debug, Clone, Default)]
pub struct CurvesEditorBlock {
    /// Bit 0 = enable.
    pub flags: u32,
    /// 0 = RGB (per-channel curves), 1 = Brightness (single curve).
    pub mode: u32,
    pub brightness: Option<TagFunction>,
    pub red: Option<TagFunction>,
    pub green: Option<TagFunction>,
    pub blue: Option<TagFunction>,
}

/// `Brightness, contrast` block (12B).
#[derive(Debug, Clone, Copy, Default)]
pub struct BrightnessContrast {
    /// Bit 0 = enable.
    pub flags: u32,
    /// Authored slider in [-1, 1]. Engine formula:
    /// `b≥0: c = c + (1-c)*b ; b<0: c = c * (b+1)` per channel, then
    /// `c = (contrast+1) * (c - 0.5) + 0.5`.
    pub brightness: f32,
    /// Authored slider in [-1, 1]. See `brightness` formula.
    pub contrast: f32,
}

/// `Hue, saturation, lightness, vibrance` block (20B). Field names
/// follow *engine* semantics — the schema's slider labels for offsets
/// 12 and 16 are swapped vs how the runtime reads them.
#[derive(Debug, Clone, Copy, Default)]
pub struct HslvBlock {
    /// Bit 0 = enable.
    pub flags: u32,
    /// Hue offset in degrees, `[-180, 180]`. Added directly to `hsl.H`.
    pub hue_offset_deg: f32,
    /// Saturation additive offset, `[-1, 1]`.
    pub saturation_offset: f32,
    /// Saturation multiplier (engine's "vibrance" — schema slider is
    /// labeled "lightness" but the math at offset 12 multiplies S).
    /// Effective scale = `(±0.48 or ±0.10) * vibrance + 1`, with the
    /// branch picked from current H (skin-tone protect) and current S.
    pub vibrance: f32,
    /// Lightness additive offset (schema slider is labeled "vibrance"
    /// but offset 16 adds to L).
    pub lightness_offset: f32,
}

/// `Selective color` block (148B = 4 + 9 × 16). One CMYB per
/// color-zone band. Engine `ProcessSelectiveColor` accumulates a CMYK
/// adjustment by weighting each band by the input pixel's hue / tonal
/// distance to that zone.
#[derive(Debug, Clone, Copy, Default)]
pub struct SelectiveColorBlock {
    /// Bit 0 = enable.
    pub flags: u32,
    /// Order matters — `ProcessSelectiveColor` walks them in declaration
    /// order: `dFactors[0..5]` map to reds..magentas (hue zones via
    /// 6-bin RGB ordering); `dFactors[6..8]` map to whites/neutrals/
    /// blacks (tonal zones via luminance distance).
    pub bands: [CmybBand; 9],
}

/// `Color grading CMYB` (16B). Per-zone CMYK adjust slider in `[-1, 1]`.
/// Negative `black` values are weighted 2× during accumulation
/// (asymmetric K) per `ProcessSelectiveColor:139-180`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CmybBand {
    pub cyan: f32,
    pub magenta: f32,
    pub yellow: f32,
    pub black: f32,
}

/// `Color balance` block (40B = 4 + 3 × 12). Cyan-red, magenta-green,
/// yellow-blue sliders in shadows/midtones/highlights. Engine
/// `SetColorBalanceParams` builds a `Tones[3][3]` (per-channel lo,
/// gamma, hi) which the per-texel loop applies as
/// `out = pow(remap(in, lo, hi), 1/gamma)` per RGB channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorBalanceBlock {
    /// Bit 0 = enable.
    pub flags: u32,
    pub shadows: CmyBand,
    pub midtones: CmyBand,
    pub highlights: CmyBand,
}

/// `Color grading CMY` (12B). Cyan-red, magenta-green, yellow-blue
/// sliders in `[-1, 1]`.
#[derive(Debug, Clone, Copy, Default)]
pub struct CmyBand {
    pub cyan_red: f32,
    pub magenta_green: f32,
    pub yellow_blue: f32,
}

/// `Colorize effect` block (20B). Field names follow *engine*
/// semantics; schema's "saturation" / "lightness" slider labels are
/// swapped vs the read offsets.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorizeBlock {
    /// Bit 0 = enable.
    pub flags: u32,
    /// Lerp factor in `[0, 1]`. 0 = no colorize (preserve original
    /// H/S/L), 1 = pure target. Engine: `out = target*blend + in*(1-blend)`.
    pub blendfactor: f32,
    /// Target hue in degrees, `[-180, 180]`.
    pub target_hue_deg: f32,
    /// Lightness target bias in `[-1, 1]`. The L target is the texel's
    /// luminance (R+G+B)/3 nudged toward 1 (when positive) or 0 (when
    /// negative); this slider is the nudge amount.
    pub target_lightness: f32,
    /// Saturation target in `[-1, 1]`.
    pub target_saturation: f32,
}

#[derive(Debug, Clone, Default)]
pub struct ExposureBlock {
    /// Bit 0: auto-adjust target. Bit 2: auto-adjust delay enabled.
    /// Bit 4: fixed (use `exposure` value verbatim, no auto). Bit 5:
    /// scripted.
    pub flags: u16,
    /// Exposure stops (log₂ of luminance multiplier). 0 = neutral.
    /// `view_exposure = pow(2, exposure) × tone_curve_white_point × 0.66943`.
    pub exposure: f32,
    /// Auto-exposure max delta per frame.
    pub maximum_change: f32,
    /// Auto-exposure blend speed.
    pub blend_speed: f32,
    /// Min/max stops clamp for auto-exposure.
    pub minimum: f32,
    pub maximum: f32,
    /// Target screen brightness for auto-exposure (0-1).
    pub auto_exposure_screen_brightness: f32,
    pub auto_exposure_delay: f32,
}

impl ExposureBlock {
    /// `flags & 0x10` — fixed-exposure flag.
    pub fn is_fixed(&self) -> bool {
        (self.flags & 0x10) != 0
    }

    /// Compute Halo's `view_exposure` for a given exposure_boost +
    /// tone_curve_white_point (default 1.0). Mirrors
    /// `c_camera_fx_values::get_render_exposure @ 0x18068e3e0` — we
    /// skip the scripted_exposure component (always 0 for our v1).
    pub fn view_exposure(&self, tone_curve_white_point: f32, exposure_boost: f32) -> f32 {
        // Effective stops: scripted (0) + exposure (`g_exposure_stops`
        // is the same field) + boost.
        let stops = self.exposure + exposure_boost;
        2.0_f32.powf(stops) * tone_curve_white_point * 0.66943294
    }
}

impl CameraFxSettings {
    pub fn from_tag(tag: &TagFile) -> Result<Self, CameraFxError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != CFXS_GROUP {
            return Err(CameraFxError::WrongGroup { expected: CFXS_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let exposure = s
            .field("exposure")
            .and_then(|f| f.as_struct())
            .map(|sub| ExposureBlock {
                flags: sub.read_int_any("flags").unwrap_or(0) as u16,
                exposure: sub.read_real("exposure").unwrap_or(0.0),
                maximum_change: sub.read_real("maximum change").unwrap_or(0.0),
                blend_speed: sub.read_real("blend speed (0-1)").unwrap_or(0.0),
                minimum: sub.read_real("minimum").unwrap_or(0.0),
                maximum: sub.read_real("maximum").unwrap_or(0.0),
                auto_exposure_screen_brightness: sub
                    .read_real("auto-exposure screen brightness")
                    .unwrap_or(0.5),
                auto_exposure_delay: sub
                    .read_real("auto-exposure delay")
                    .unwrap_or(0.0),
            })
            .unwrap_or_default();

        // Generic readers for the parameter shapes. Each parameter
        // sub-struct has the same shape — flags + named-value field
        // (+ max_change + blend_speed for blend-mode parameters).
        let read_scalar_param = |struct_name: &str, value_field: &str| -> ScalarParameter {
            s.field(struct_name)
                .and_then(|f| f.as_struct())
                .map(|sub| ScalarParameter {
                    flags: sub.read_int_any("flags").unwrap_or(0) as u16,
                    value: sub.read_real(value_field).unwrap_or(0.0),
                    max_change: sub.read_real("maximum change").unwrap_or(0.0),
                    blend_speed: sub.read_real("blend speed (0-1)").unwrap_or(0.0),
                })
                .unwrap_or_default()
        };
        let read_instant_param = |struct_name: &str, value_field: &str| -> InstantScalarParameter {
            s.field(struct_name)
                .and_then(|f| f.as_struct())
                .map(|sub| InstantScalarParameter {
                    flags: sub.read_int_any("flags").unwrap_or(0) as u16,
                    value: sub.read_real(value_field).unwrap_or(0.0),
                })
                .unwrap_or_default()
        };
        let read_color_param = |struct_name: &str, value_field: &str| -> ColorParameter {
            s.field(struct_name)
                .and_then(|f| f.as_struct())
                .map(|sub| ColorParameter {
                    flags: sub.read_int_any("flags").unwrap_or(0) as u16,
                    color: sub.read_rgb(value_field),
                })
                .unwrap_or_default()
        };
        let read_word_param = |struct_name: &str, value_field: &str| -> WordParameter {
            s.field(struct_name)
                .and_then(|f| f.as_struct())
                .map(|sub| WordParameter {
                    flags: sub.read_int_any("flags").unwrap_or(0) as u16,
                    value: sub.read_int_any(value_field).unwrap_or(0) as u16,
                })
                .unwrap_or_default()
        };

        let color_grading = parse_color_grading(s);
        let ssao = s.field("ssao").and_then(|f| f.as_struct()).map(|sub| SsaoBlock {
            flags: sub.read_int_any("flags").unwrap_or(0) as u32,
            intensity: sub.read_real("intensity").unwrap_or(0.0),
            radius: sub.read_real("radius").unwrap_or(0.0),
            sample_z_threshold: sub.read_real("sample z threshold").unwrap_or(0.0),
        });
        let lightshafts = s.field("lightshafts").and_then(|f| f.as_struct()).map(|sub| LightshaftsBlock {
            flags: sub.read_int_any("flags").unwrap_or(0) as u32,
            pitch: sub.read_real("pitch").unwrap_or(0.0),
            heading: sub.read_real("heading").unwrap_or(0.0),
            tint: sub.read_rgb("tint"),
            depth_clamp: sub.read_real("depth clamp").unwrap_or(0.0),
            intensity_clamp: sub.read_real("intensity clamp").unwrap_or(0.0),
            falloff_radius: sub.read_real("falloff radius").unwrap_or(0.0),
            intensity: sub.read_real("intensity").unwrap_or(0.0),
            blur_radius: sub.read_real("blur radius").unwrap_or(0.0),
        });

        Self {
            exposure,
            auto_exposure_sensitivity: read_instant_param(
                "auto_exposure_sensitivity",
                "sensitivity (0-1)",
            ),
            auto_exposure_anti_bloom: read_instant_param(
                "auto_exposure_anti_bloom",
                "anti-bloom (0-1)",
            ),
            bloom_point: read_scalar_param("bloom_point", "bloom point"),
            bloom_inherent: read_scalar_param("bloom_inherent", "inherent bloom"),
            bloom_intensity: read_scalar_param("bloom_intensity", "bloom intensity"),
            bloom_large_color: read_color_param("bloom_large_color", "large color"),
            bloom_medium_color: read_color_param("bloom_medium_color", "medium color"),
            bloom_small_color: read_color_param("bloom_small_color", "small color"),
            bling_intensity: read_scalar_param("bling_intensity", "bling intensity"),
            bling_size: read_scalar_param("bling_size", "bling length"),
            bling_angle_deg: read_scalar_param("bling_angle", "bling angle"),
            bling_count: read_word_param("bling_count", "bling spikes"),
            self_illum_preferred: read_scalar_param(
                "self_illum_preferred",
                "preferred exposure",
            ),
            self_illum_scale: read_scalar_param("self_illum_scale", "exposure change"),
            ssao,
            lightshafts,
            color_grading,
        }
    }
}

/// Walk the `color grading` struct (if present) and pull out the
/// sub-blocks the LUT bake consumes. Returns `None` for tags that
/// predate the struct (e.g. riverworld) so callers can fall through
/// to the engine's identity-LUT path.
fn parse_color_grading(s: &TagStruct<'_>) -> Option<ColorGradingBlock> {
    let cg = s.field("color grading").and_then(|f| f.as_struct())?;

    let block_first = |name: &str| -> Option<TagStruct<'_>> {
        cg.field(name)
            .and_then(|f| f.as_block())
            .and_then(|b| b.element(0))
    };

    let curves_editor = block_first("Curves editor").map(|sub| {
        let parse_curve = |name: &str| -> Option<TagFunction> {
            let curve = sub.field(name).and_then(|f| f.as_struct())?;
            // The schema defines two same-named "Mapping" fields — a
            // `custom` marker (group_tag fned) followed by the actual
            // `mapping_function` struct. `field("Mapping")` would
            // return the marker first; instead pick the field by type.
            let mapping = curve
                .fields()
                .find(|f| f.field_type() == TagFieldType::Struct)?
                .as_struct()?;
            mapping.field("data").and_then(|f| f.as_function())
        };
        CurvesEditorBlock {
            flags: sub.read_int_any("flags").unwrap_or(0) as u32,
            mode: sub.read_int_any("mode").unwrap_or(0) as u32,
            brightness: parse_curve("brightness curve"),
            red: parse_curve("red curve"),
            green: parse_curve("green curve"),
            blue: parse_curve("blue curve"),
        }
    });

    let brightness_contrast = block_first("Brightness, contrast").map(|sub| BrightnessContrast {
        flags: sub.read_int_any("flags").unwrap_or(0) as u32,
        brightness: sub.read_real("brightness").unwrap_or(0.0),
        contrast: sub.read_real("contrast").unwrap_or(0.0),
    });

    let hslv = block_first("Hue, saturation, lightness, vibrance").map(|sub| HslvBlock {
        flags: sub.read_int_any("flags").unwrap_or(0) as u32,
        hue_offset_deg: sub.read_real("hue").unwrap_or(0.0),
        saturation_offset: sub.read_real("saturation").unwrap_or(0.0),
        // Schema label "lightness" — engine reads as saturation
        // multiplier (vibrance) at offset 12.
        vibrance: sub.read_real("lightness").unwrap_or(0.0),
        // Schema label "vibrance" — engine reads as lightness offset
        // at offset 16.
        lightness_offset: sub.read_real("vibrance").unwrap_or(0.0),
    });

    let colorize = block_first("Colorize effect").map(|sub| ColorizeBlock {
        flags: sub.read_int_any("flags").unwrap_or(0) as u32,
        blendfactor: sub.read_real("blendfactor").unwrap_or(0.0),
        target_hue_deg: sub.read_real("hue").unwrap_or(0.0),
        // Schema label "saturation" — engine reads as L-target bias
        // at offset 12 (luminance lerp control).
        target_lightness: sub.read_real("saturation").unwrap_or(0.0),
        // Schema label "lightness" — engine reads as S-target at offset 16.
        target_saturation: sub.read_real("lightness").unwrap_or(0.0),
    });

    let selective_color = block_first("Selective color").map(|sub| {
        let read_band = |name: &str| -> CmybBand {
            sub.field(name)
                .and_then(|f| f.as_struct())
                .map(|b| CmybBand {
                    cyan: b.read_real("cyan").unwrap_or(0.0),
                    magenta: b.read_real("magenta").unwrap_or(0.0),
                    yellow: b.read_real("yellow").unwrap_or(0.0),
                    black: b.read_real("black").unwrap_or(0.0),
                })
                .unwrap_or_default()
        };
        SelectiveColorBlock {
            flags: sub.read_int_any("flags").unwrap_or(0) as u32,
            // Order MUST match `ProcessSelectiveColor`'s walk:
            // reds → yellows → greens → cyans → blues → magentas →
            // whites → neutrals → blacks.
            bands: [
                read_band("reds"),
                read_band("yellows"),
                read_band("greens"),
                read_band("cyans"),
                read_band("blues"),
                read_band("magentas"),
                read_band("whites"),
                read_band("neutrals"),
                read_band("blacks"),
            ],
        }
    });

    let color_balance = block_first("Color balance").map(|sub| {
        let read_cmy = |name: &str| -> CmyBand {
            sub.field(name)
                .and_then(|f| f.as_struct())
                .map(|b| CmyBand {
                    cyan_red: b.read_real("cyan - red").unwrap_or(0.0),
                    magenta_green: b.read_real("magenta - green").unwrap_or(0.0),
                    yellow_blue: b.read_real("yellow - blue").unwrap_or(0.0),
                })
                .unwrap_or_default()
        };
        ColorBalanceBlock {
            flags: sub.read_int_any("flags").unwrap_or(0) as u32,
            shadows: read_cmy("shadows"),
            midtones: read_cmy("midtones"),
            highlights: read_cmy("highlights"),
        }
    });

    Some(ColorGradingBlock {
        flags: cg.read_int_any("flags").unwrap_or(0) as u32,
        blend_time: cg.read_real("blend time").unwrap_or(0.0),
        curves_editor,
        brightness_contrast,
        hslv,
        colorize,
        selective_color,
        color_balance,
    })
}
