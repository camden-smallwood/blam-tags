//! `lens_flare` (`lens`) tag walker — sun / muzzle-flash / corona
//! authoring. Attached to light tags via `light.lens_flare_settings`
//! and rendered as a screen-space sprite stack via
//! `c_player_view::render_lens_flares` (Tier 11 of the effects port).
//!
//! ## Schema shape
//!
//! - Root carries 25+ fields: falloff/cutoff angles, occlusion config,
//!   near/far fade distances, the corona bitmap reference, runtime
//!   flags, rotation function, falloff function, then 6 animation
//!   blocks (time × age × {brightness, color, rotation}).
//! - `reflections[]` (`s_lens_flare_reflection`, 48B each) — one per
//!   ghost / halo / etc. in the sprite stack. Each reflection carries
//!   its own bitmap slice + position curve + 4 mapping functions
//!   (radius, scale_x, scale_y, brightness) interpolated by external
//!   input.
//! - Color animations use a separate `color_function_struct` with
//!   string_id inputs (resolved at runtime, NOT the char_enum
//!   indices used by particle physics).
//!
//! Schema: `definitions/halo3_mcc/lens_flare.json`. Engine: 152B
//! `s_lens_flare_definition` (per Ares header).

use crate::api::TagStruct;
use crate::fields::{TagFieldData, TagFieldType};
use crate::file::TagFile;
use crate::math::{FractionBounds, RealBounds, RealRgbColor};
use crate::tag_function::TagFunction;

const LENS_GROUP: [u8; 4] = *b"lens";

#[derive(Debug)]
pub enum LensFlareError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for LensFlareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "lens_flare: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for LensFlareError {}

/// One reflection / ghost in the lens-flare sprite stack.
#[derive(Debug, Clone, Default)]
pub struct LensFlareReflection {
    pub flags: u16,
    /// Index into the parent's `bitmap` tag_reference sprite atlas.
    pub bitmap_index: i16,
    /// Optional bitmap override (separate atlas for this reflection).
    pub bitmap_override: Option<String>,
    /// Per-frame static rotation offset in degrees.
    pub rotation_offset_degrees: f32,
    /// Axis offset as a fraction of the corona-to-center vector
    /// (-1 = primary screen edge, 0 = corona, 1 = opposite edge).
    pub axis_offset: f32,
    /// Clamp bounds for axis-offset * corona-offset product.
    pub offset_bounds: RealBounds,
    pub radius_world_units: RealBounds,
    pub brightness: FractionBounds,
    /// Curves interpolated by external input (occlusion result, etc.).
    pub radius_curve: Option<TagFunction>,
    pub scale_curve_x: Option<TagFunction>,
    pub scale_curve_y: Option<TagFunction>,
    pub brightness_curve: Option<TagFunction>,
    /// Per-reflection tint applied multiplicatively.
    pub color: RealRgbColor,
    /// `modulation factor` (0..1) — controls strength of tinting.
    pub modulation_factor: f32,
    /// `tint power` (0.1..16) — exponent applied to tint before blend.
    pub tint_power: f32,
}

/// `color_function_struct` — color-animation node. Inputs are
/// runtime-resolved string_ids (NOT char_enums like particle_physics).
#[derive(Debug, Clone, Default)]
pub struct ColorFunction {
    pub input_variable: String,
    pub range_variable: String,
    pub output_modifier: i16,
    pub output_modifier_input: String,
    /// `lens flare color mapping` curve payload.
    pub mapping: Option<TagFunction>,
}

/// Walked `lens_flare` tag.
#[derive(Debug, Clone, Default)]
pub struct LensFlare {
    pub falloff_angle_degrees: f32,
    pub cutoff_angle_degrees: f32,

    // ---- OCCLUSION ----
    pub occlusion_reflection_index: i32,
    pub occlusion_offset_distance: f32,
    pub occlusion_offset_direction: i16,
    pub occlusion_inner_radius_scale: i16,

    // ---- FADE ----
    pub near_fade_begin_distance: f32,
    pub near_fade_end_distance: f32,
    pub near_fade_distance: f32,
    pub far_fade_distance: f32,

    // ---- BITMAP ----
    pub bitmap: Option<String>,
    pub flags: u16,
    pub runtime_flags: i16,

    // ---- ROTATION / FALLOFF ----
    pub rotation_function: i16,
    pub rotation_function_scale_degrees: f32,
    pub falloff_function: i16,

    // ---- REFLECTIONS ----
    pub reflections: Vec<LensFlareReflection>,

    // ---- ANIMATIONS ----
    pub animation_flags: u16,
    /// `time brightness` curves — sampled by elapsed seconds since
    /// the light became visible (occlusion gate).
    pub time_brightness: Vec<TagFunction>,
    /// `age brightness` curves — sampled by total age of the light.
    pub age_brightness: Vec<TagFunction>,
    /// Color over time / age. Each entry is a full ColorFunction
    /// struct (not just a scalar TagFunction).
    pub time_color: Vec<ColorFunction>,
    pub age_color: Vec<ColorFunction>,
    pub time_rotation: Vec<TagFunction>,
    pub age_rotation: Vec<TagFunction>,
}

impl LensFlare {
    pub fn from_tag(tag: &TagFile) -> Result<Self, LensFlareError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != LENS_GROUP {
            return Err(LensFlareError::WrongGroup { expected: LENS_GROUP, actual });
        }
        let mut out = Self::from_struct(&tag.root());
        out.apply_engine_postprocess();
        Ok(out)
    }

    /// Mirror tool.exe `sub_1403E5440`'s tag-group postprocess: clamp the
    /// occlusion-cone angles to `[0, π]` with `cutoff ≥ falloff`, and
    /// remove reflections flagged for debug-only suppression.
    ///
    /// Engine source:
    /// ```c
    /// v4 = clamp(*(float*)(a2 + 0), 0.0, π);     // falloff
    /// v5 = clamp(*(float*)(a2 + 4), v4,  π);     // cutoff (ordered ≥ falloff)
    /// *(float*)(a2 + 0) = v4;
    /// *(float*)(a2 + 4) = v5;
    /// *(u16*)(a2 + 54) |= 1;                      // post-processed marker
    /// for r in reflections:
    ///     if (r.flags & 0x100) remove(r);         // bit 8 = "disabled for debugging"
    /// ```
    ///
    /// Real-tag evidence: `activate.lens_flare` authors
    /// `cutoff angle = 2π (360°)` — engine clamps it to π (180°).
    ///
    /// Note: protomorph's field names suffix `_degrees` is a misnomer —
    /// the schema field type is `angle`, which Halo's tag system stores
    /// in **radians**. Both clamp bounds and the underlying storage are
    /// radians; the `_degrees` suffix is just leftover labelling.
    pub fn apply_engine_postprocess(&mut self) {
        let pi = std::f32::consts::PI;
        let falloff = self.falloff_angle_degrees.clamp(0.0, pi);
        let cutoff = self.cutoff_angle_degrees.max(falloff).min(pi);
        self.falloff_angle_degrees = falloff;
        self.cutoff_angle_degrees = cutoff;

        // Reflection-flags bit 8 = "disabled for debugging" — engine
        // strips these from the runtime list so they never render.
        self.reflections.retain(|r| (r.flags & 0x100) == 0);
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            falloff_angle_degrees: s.read_real("falloff angle").unwrap_or(0.0),
            cutoff_angle_degrees: s.read_real("cutoff angle").unwrap_or(0.0),

            occlusion_reflection_index: s.read_int_any("occlusion reflection index").unwrap_or(0) as i32,
            occlusion_offset_distance: s.read_real("occlusion offset distance").unwrap_or(0.0),
            occlusion_offset_direction: s.read_int_any("occlusion offset direction").unwrap_or(0) as i16,
            occlusion_inner_radius_scale: s.read_int_any("occlusion inner radius scale").unwrap_or(0) as i16,

            near_fade_begin_distance: s.read_real("near fade begin distance").unwrap_or(0.0),
            near_fade_end_distance: s.read_real("near fade end distance").unwrap_or(0.0),
            near_fade_distance: s.read_real("near fade distance").unwrap_or(0.0),
            far_fade_distance: s.read_real("far fade distance").unwrap_or(0.0),

            bitmap: s.read_tag_ref_path("bitmap"),
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            runtime_flags: s.read_int_any("runtime flags").unwrap_or(0) as i16,

            rotation_function: s.read_int_any("rotation function").unwrap_or(0) as i16,
            rotation_function_scale_degrees: s.read_real("rotation function scale").unwrap_or(0.0),
            falloff_function: s.read_int_any("falloff function").unwrap_or(0) as i16,

            reflections: read_block(s, "reflections", LensFlareReflection::from_struct),

            animation_flags: s.read_int_any("animation flags").unwrap_or(0) as u16,
            time_brightness: read_scalar_animation_block(s, "time brightness"),
            age_brightness: read_scalar_animation_block(s, "age brightness"),
            time_color: read_color_animation_block(s, "time color"),
            age_color: read_color_animation_block(s, "age color"),
            time_rotation: read_scalar_animation_block(s, "time rotation"),
            age_rotation: read_scalar_animation_block(s, "age rotation"),
        }
    }
}

impl LensFlareReflection {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            bitmap_index: s.read_int_any("bitmap index").unwrap_or(0) as i16,
            bitmap_override: s.read_tag_ref_path("bitmapOverride"),
            rotation_offset_degrees: s.read_real("rotation offset").unwrap_or(0.0),
            axis_offset: s.read_real("axis offset").unwrap_or(0.0),
            offset_bounds: s.read_real_bounds("offset bounds"),
            radius_world_units: s.read_real_bounds("radius"),
            brightness: s.read_fraction_bounds("brightness"),
            radius_curve: read_scalar_function(s, "radius curve"),
            scale_curve_x: read_scalar_function(s, "scale curve X"),
            scale_curve_y: read_scalar_function(s, "scale curve Y"),
            brightness_curve: read_scalar_function(s, "brightness curve"),
            color: s.read_rgb("color"),
            modulation_factor: s.read_real("modulation factor").unwrap_or(0.0),
            tint_power: s.read_real("tint power").unwrap_or(0.0),
        }
    }
}

impl ColorFunction {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let mapping = s
            .field("lens flare color mapping")
            .and_then(|f| f.as_struct())
            .and_then(|inner| inner.field("data").and_then(|f| f.as_function()));
        Self {
            input_variable: s.read_string_id("Input Variable").unwrap_or_default(),
            range_variable: s.read_string_id("Range Variable").unwrap_or_default(),
            output_modifier: s.read_int_any("Output Modifier").unwrap_or(0) as i16,
            output_modifier_input: s.read_string_id("Output Modifier Input").unwrap_or_default(),
            mapping,
        }
    }
}

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    let block = match s.field(name).and_then(|fld| fld.as_block()) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}

/// Walk a `scalar_function_named_struct` (the corona/reflection curve
/// wrapper) — same shape as area_screen_effect falloffs: `custom`
/// marker + named `function` sub-struct + inner `data` field.
fn read_scalar_function(parent: &TagStruct<'_>, name: &str) -> Option<TagFunction> {
    let outer = parent.field(name).and_then(|f| f.as_struct())?;
    let inner = outer
        .fields()
        .find(|f| f.field_type() == TagFieldType::Struct)?
        .as_struct()?;
    inner.field("data").and_then(|f| f.as_function())
}

/// A `lens_flare_scalar_animation_block` carries one `function` field
/// per entry — same wrapper as `scalar_function_named_struct`.
fn read_scalar_animation_block(s: &TagStruct<'_>, name: &str) -> Vec<TagFunction> {
    let mut out = Vec::new();
    let block = match s.field(name).and_then(|f| f.as_block()) {
        Some(b) => b,
        None => return out,
    };
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            if let Some(fun) = read_scalar_function(&elem, "function") {
                out.push(fun);
            }
        }
    }
    out
}

/// `lens_flare_color_animation_block` carries one `color animation`
/// (color_function_struct) per entry.
fn read_color_animation_block(s: &TagStruct<'_>, name: &str) -> Vec<ColorFunction> {
    let mut out = Vec::new();
    let block = match s.field(name).and_then(|f| f.as_block()) {
        Some(b) => b,
        None => return out,
    };
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            if let Some(inner) = elem.field("color animation").and_then(|f| f.as_struct()) {
                out.push(ColorFunction::from_struct(&inner));
            }
        }
    }
    out
}

// Silence the unused-import warning when read_fraction_bounds is added
// to api.rs only — keep the convenience for future colour-bound walks.
#[allow(dead_code)]
fn _force_link(_: TagFieldData) {}
