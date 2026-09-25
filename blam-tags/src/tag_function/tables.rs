//! Engine-exact transition / periodic sub-function lookup tables.
//!
//! The MCC engine evaluates `e_transition_function` and `e_periodic_function`
//! through 1024-entry byte LUTs (one row per function type), NOT closed-form
//! curves. The decompiled evaluators are:
//!   - `transition_function_evaluate @0x180346C60` — table `@0x181114FB0`
//!     ([8][1024] u8, indexed by function_type; row 0 unused — type 0 returns
//!     the input directly).
//!   - `periodic_function_evaluate  @0x180346AC0` — table `@0x181111FB0`
//!     ([12][1024] u8, indexed by function_type; row 0 unused — type 0 returns
//!     1.0).
//!
//! [`FUNCTION_TABLES`] is the raw table bytes pulled verbatim from the
//! dllcache (transition rows 1..=7 then periodic rows 1..=11) and verified
//! byte-exact against fresh reads across every row — including the periodic
//! noise/jitter/wander/spark rows (types 8..=11), which hold *baked
//! pseudo-random data with no closed form*. That is why the LUT is the only
//! faithful source: those four functions cannot be reproduced analytically.
//!
//! Byte `b` at table index `k` represents `b / 255.0` at normalized input
//! `k / 1023` (transition) or table phase `k` (periodic).

/// Raw LUT bytes: transition rows 1..=7 (`7 * 1024`) then periodic rows
/// 1..=11 (`11 * 1024`) = 18432 bytes. Extracted from the dllcache.
pub static FUNCTION_TABLES: &[u8; 18432] = include_bytes!("function_tables.bin");

const LUT_LEN: usize = 1024;
/// Transition LUT holds function types 1..=7 (row = type - 1).
const TRANSITION_ROWS: usize = 7;
/// Periodic LUT holds function types 1..=11 (row = type - 1).
const PERIODIC_ROWS: usize = 11;
/// Byte offset where the periodic rows begin within [`FUNCTION_TABLES`].
const PERIODIC_BASE: usize = TRANSITION_ROWS * LUT_LEN; // 7168

#[inline]
pub(crate) fn lut(base: usize, k: usize) -> f32 {
    // Engine scales bytes by 0.0039215689 (= 1/255).
    FUNCTION_TABLES[base + k] as f32 * (1.0 / 255.0)
}

/// `transition_function_evaluate @0x180346C60` — verbatim.
///
/// `function_type == 0` → identity (returns the clamped input). Types 1..=7
/// index the LUT with the engine's exact `idx = (int)(x*1023 - 0.1 + 0.5)`
/// rounding and a fractional lerp between adjacent entries; the result is
/// clamped to `[0, 1]`.
pub fn transition_function_evaluate(function_index: u8, value: f32) -> f32 {
    let x = value.clamp(0.0, 1.0);
    if function_index == 0 {
        return x;
    }
    // Engine asserts type <= 7; clamp defensively rather than panic.
    let row = (function_index as usize - 1).min(TRANSITION_ROWS - 1);
    let base = row * LUT_LEN;
    let scaled = x * 1023.0;
    let frac = scaled - scaled.floor(); // fmodf(scaled, 1.0), scaled >= 0
    let i = ((scaled - 0.1) + 0.5) as i32 as usize; // (int)(scaled + 0.4)
    let result = if i >= LUT_LEN - 1 {
        lut(base, LUT_LEN - 1)
    } else {
        lut(base, i) * (1.0 - frac) + lut(base, i + 1) * frac
    };
    result.clamp(0.0, 1.0)
}

/// `periodic_function_evaluate @0x180346AC0` — verbatim.
///
/// `function_type == 0` → constant 1.0. Types 1..=11 scale `time` by the
/// engine constant `36.57143`, round-and-mask to a 1024-wide phase index, and
/// lerp adjacent entries with wraparound. Types 6 and 7 (the slide / sawtooth
/// family, mask `0xC0`) get the engine's wrap-blend so a near-1 → near-0 step
/// interpolates through the top instead of collapsing.
pub fn periodic_function_evaluate(function_index: u8, time: f32) -> f32 {
    if function_index == 0 {
        return 1.0;
    }
    // The noise / jitter / wander / spark rows (8..=11) hold baked
    // pseudo-random data and run through the same LUT below — byte-exact to
    // `periodic_function_evaluate @0x180346AC0`. These were previously stubbed
    // to a constant 0.5 to suppress a "flicker", but that flicker was an
    // artifact of the animated-param path wrongly applying `map_to_output_range`
    // on top of the periodic compact (which already bakes amp_min/amp_max),
    // amplifying the gentle amp-bounded noise into huge swings. The engine runs
    // these noise curves per-frame with NO gate or cache: verified
    // `render_method_submit_volatile_per_node @0x180683af0` →
    // `update_constants @0x180685300` → `evaluate_function @0x1806864d0`
    // (game_time, deterministic) → `evaluate_legacy` (no output map). With the
    // periodic skip-map in `TagFunction::evaluate` the noise is gently
    // amp-bounded, so it must run — it drives e.g. the mainmenu storm cloud's
    // animated self-illum.
    let row = (function_index as usize - 1).min(PERIODIC_ROWS - 1);
    let base = PERIODIC_BASE + row * LUT_LEN;
    let scaled = time * 36.57143;
    let v4 = scaled % 1.0; // fmodf(scaled, 1.0)
    let i = ((scaled - v4) + 0.5) as i32 as usize & 0x3FF;
    let v7 = lut(base, i);
    let v9 = lut(base, (i + 1) & 0x3FF);
    if (1u32 << function_index) & 0xC0 == 0 {
        (1.0 - v4) * v7 + v9 * v4
    } else {
        // Sawtooth wrap (types 6/7): lift the next sample over the seam.
        let v8 = if v7 > 0.75 && v9 < 0.25 { v9 + 1.0 } else { v9 };
        let r = (1.0 - v4) * v7 + v8 * v4;
        if r > 1.0 { r - 1.0 } else { r }
    }
}
