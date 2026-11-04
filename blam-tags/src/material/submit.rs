//! What the game does with a baked material's functions at each draw
//! (`material_submit.cpp`): re-run each dynamic function on the object's
//! function values and write it over the static constant the bake left.
//!
//! X360 `midnight_tag_debug`: `MaterialSubmitUpdateConstants` 0x83B06240,
//! `EvaluateFunctionOutput` 0x83B06628, `EvaluateFunction` 0x83B068C8 and
//! its input lambda 0x83B06B48. MCC `halo4_tag_test`: 0x1408CAE80 and
//! 0x1408C9E60, which agree.

use super::{BakedMaterial, MaterialFunctionParameter, MaterialFunctionType};
use crate::tag_function::FunctionType;

/// What a material's functions read: the drawn object's function values by
/// name, the game's clock, and the object.
pub trait FunctionContext {
    /// `object_get_function_value` for the drawn object.
    fn object_function(&self, name: &str) -> f32;
    /// `game_time_get_safe_in_seconds`.
    fn time(&self) -> f32;
    /// The low 12 bits of the object's handle, which `unique_id` spreads
    /// objects apart by.
    fn object_index(&self) -> u32;
}

/// `unique_id`: each object's own value.
const UNIQUE_ID: &str = "unique_id";
/// How far apart `unique_id` sets neighbouring objects.
const UNIQUE_ID_STEP: f32 = 1.37317;

/// The register ranges `SetMaterial*Constants` hand to
/// [`update_constants`] (MCC 0x1408CA980): the transforms by sampler, then
/// the pixel and vertex constants by register.
pub const TEXTURE_TRANSFORM_REGISTERS: (u8, u8) = (0, 12);
pub const PIXEL_REGISTERS: (u8, u8) = (160, 24);
pub const VERTEX_REGISTERS: (u8, u8) = (240, 8);

fn is_periodic(function: &MaterialFunctionParameter) -> bool {
    function.function.as_ref().is_some_and(|curve| curve.function_type() == FunctionType::Periodic)
}

/// The input lambda: what a function's input, range or modifier name reads.
/// No name, `unique_id` and a scenario interpolator run on the clock, over
/// the function's period, wrapped to 0..1 unless the curve is periodic; any
/// other name is the object's value. The preview has no scenario, so no
/// name is an interpolator's.
fn input(function: &MaterialFunctionParameter, name: &str, context: &impl FunctionContext) -> f32 {
    let unique = name == UNIQUE_ID;
    if !name.is_empty() && !unique {
        return context.object_function(name);
    }
    let periodic = is_periodic(function);
    let mut value = context.time() / function.time_period;
    if unique {
        let spread = (context.object_index() & 0xFFF) as f32 * UNIQUE_ID_STEP;
        value = if periodic { value + spread / function.time_period } else { spread };
    }
    if periodic { value } else { value % 1.0 }
}

/// `EvaluateFunction`: the curve's normalized output, and the scale and
/// offset an `Output Modifier` puts on the mapped value. A modifier with an
/// input name scales (multiply) or offsets (add) the output by that input; a
/// ranged curve with none instead takes its range curve's value at the range
/// input onto the input, and is then read at range 0.
fn evaluate(function: &MaterialFunctionParameter, context: &impl FunctionContext) -> Option<(f32, f32, f32, f32)> {
    let curve = function.function.as_ref()?;
    let mut value = input(function, &function.input_name, context);
    let mut range = input(function, &function.range_name, context);
    let (mut scale, mut offset) = (1.0, 0.0);
    if function.output_modifier != 0 {
        let modifier = if !function.output_modifier_input.is_empty() {
            Some((input(function, &function.output_modifier_input, context), false))
        } else if curve.is_ranged() {
            let onto_input = curve.evaluate_legacy(range, 1.0);
            range = 0.0;
            Some((onto_input, true))
        } else {
            None
        };
        match (function.output_modifier, modifier) {
            (1, Some((by, true))) => value += by,
            (1, Some((by, false))) => offset = by,
            (2, Some((by, true))) => value *= by,
            (2, Some((by, false))) => scale = by,
            _ => {}
        }
    }
    Some((value, range, scale, offset))
}

/// `color_degamma` (MCC `sub_14041A4E0`): clamped to 0..1, then x^2.2.
pub fn color_degamma(value: f32) -> f32 {
    value.clamp(0.0, 1.0).powf(2.2)
}

/// `EvaluateFunctionOutput`: write the function into `lanes`, which start at
/// its register's lane. A colour fills three lanes; a scalar goes by what it
/// drives, and a kind with no lane of its own (value, alpha, frame index)
/// writes the first.
pub fn evaluate_function_output(function: &MaterialFunctionParameter, context: &impl FunctionContext, lanes: &mut [f32]) {
    let Some(curve) = function.function.as_ref() else { return };
    let Some((value, range, scale, offset)) = evaluate(function, context) else { return };
    let mut write = |lane: usize, value: f32| {
        if let Some(slot) = lanes.get_mut(lane) {
            *slot = value;
        }
    };
    if function.kind == MaterialFunctionType::Color {
        let color = curve.evaluate_color(value, range);
        for (lane, channel) in [color.red, color.green, color.blue].into_iter().enumerate() {
            write(lane, color_degamma(channel) * scale + offset);
        }
        return;
    }
    let value = curve.evaluate(value, range) * scale + offset;
    match function.kind {
        MaterialFunctionType::ScaleUniform => {
            write(0, value);
            write(1, value);
        }
        MaterialFunctionType::ScaleV => write(1, value),
        MaterialFunctionType::OffsetU => write(2, value),
        MaterialFunctionType::OffsetV => write(3, value),
        _ => write(0, value),
    }
}

/// `MaterialSubmitGetBitmapIndex` (X360 0x83B07060): which of a bitmap's
/// `image_count` images a frame-index function picks,
/// `floor(fmod(mapped · scale + offset, count))` (the game adds `sgn · 0.5`
/// before truncating, which changes nothing for a whole number). A bitmap of
/// one image, or a function of another kind, picks the first. Negative when
/// the value is.
pub fn bitmap_index(function: &MaterialFunctionParameter, context: &impl FunctionContext, image_count: usize) -> i32 {
    if image_count <= 1 || function.kind != MaterialFunctionType::FrameIndex {
        return 0;
    }
    let Some(curve) = function.function.as_ref() else { return 0 };
    let Some((value, range, scale, offset)) = evaluate(function, context) else { return 0 };
    let mapped = curve.evaluate(value, range) * scale + offset;
    (mapped % image_count as f32).floor() as i32
}

/// `MaterialSubmitUpdateConstants`: every queued function whose register
/// falls in `start..start + count` and whose phases meet `phase_mask` (0
/// meets all) rewrites `constants`, which hold that range from `start`. The
/// number run.
pub fn update_constants(
    baked: &BakedMaterial,
    phase_mask: u8,
    (start, count): (u8, u8),
    constants: &mut [[f32; 4]],
    context: &impl FunctionContext,
) -> usize {
    let mut run = 0;
    for function in &baked.functions {
        let Some(slot) = function.register_index.checked_sub(start).filter(|slot| *slot < count) else { continue };
        if phase_mask != 0 && function.phase_mask & phase_mask == 0 {
            continue;
        }
        let lanes = constants.as_flattened_mut();
        let from = usize::from(slot) * 4 + usize::from(function.register_offset);
        if let Some(lanes) = lanes.get_mut(from..) {
            evaluate_function_output(&function.function, context, lanes);
        }
        run += 1;
    }
    run
}
