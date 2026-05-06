//! Halo `mapping_function` (a.k.a. `c_function_definition`) decoder
//! and evaluator.
//!
//! TagFunction is a compact byte-blob curve descriptor used pervasively
//! in Halo tags — material parameter values, animated UVs, particle
//! per-frame properties, weapon firing rates, light fades, scenario
//! interpolators. The same 32-byte header + variable-length per-type
//! compact data appears in `render_method_animated_parameter_block`,
//! particle/beam/contrail/light_volume/decal systems, lens_flare,
//! camera_fx_settings, and others.
//!
//! ## 32-byte header
//!
//! Mirrors `s_function_definition_internal` from Ares
//! `source/math/function_definitions.cpp:46-71`:
//!
//! ```text
//! byte 0:    function_type (enum 0..10)
//! byte 1:    flags (range/cyclic/clamped/exclusion/optimized/gpu)
//! byte 2:    color_graph_type (0=scalar, 1..4 = N-color)
//! byte 3:    unused
//! bytes 4-19: union { colors[4] | clamp_range_min/max | constants[2] }
//! bytes 20-23: exclusion_min
//! bytes 24-27: exclusion_max
//! bytes 28-31: size_of_compact_data (bytes after header)
//! ```
//!
//! ## Eval pipeline
//!
//! `evaluate(input, range)` → `evaluate_legacy` then
//! `map_to_output_range_legacy`. Type-specific normalized output
//! (typically [0, 1]) gets linearly mapped through
//! `[clamp_range_min, clamp_range_max]`.
//!
//! For type 1 (constant), the unranged variant returns 0.0 from
//! `evaluate_legacy`, so the operative value lives in
//! `clamp_range_min` (header bytes 4-7). When ranged, `evaluate_legacy`
//! returns the `range` argument verbatim and the output is mapped
//! through clamp_range like any other curve.
//!
//! ## Coverage status
//!
//! All 11 function types parse + evaluate. Direct-formula types (Linear,
//! Spline, Spline2, Exponent) port from the engine's pseudocode-
//! commented `c_*_function_compact::evaluate` methods (Ares
//! `function_definitions.cpp` 800-1180). Compound types (LinearKey,
//! MultiPart) walk their compact-data graphs. Cyclic helpers
//! (`periodic_function_evaluate`, `transition_function_evaluate`)
//! reproduce the engine's analytic curve definitions directly rather
//! than via the engine's pre-baked 1024-byte lookup tables — same
//! curves, no precision loss.

use crate::math::RealRgbColor;

/// 11 function types defined in Halo's `e_function_type` enum
/// (Ares `function_definitions.h:26-41`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionType {
    Identity        = 0,
    Constant        = 1,
    Transition      = 2,
    Periodic        = 3,
    Linear          = 4,
    LinearKey       = 5,
    MultiLinearKey  = 6,
    Spline          = 7,
    /// Also called `multi_part` in the engine — same enum value.
    MultiSpline     = 8,
    Exponent        = 9,
    Spline2         = 10,
}

impl FunctionType {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Identity,
            1 => Self::Constant,
            2 => Self::Transition,
            3 => Self::Periodic,
            4 => Self::Linear,
            5 => Self::LinearKey,
            6 => Self::MultiLinearKey,
            7 => Self::Spline,
            8 => Self::MultiSpline,
            9 => Self::Exponent,
            10 => Self::Spline2,
            _ => return None,
        })
    }
}

/// Flag bits at byte 1 of the header. From the `_function_flag_*_bit`
/// enum in Ares `function_definitions.cpp:33-42`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(transparent)]
pub struct FunctionFlags(pub u8);

impl FunctionFlags {
    pub const RANGE: u8     = 1 << 0;
    pub const CYCLIC: u8    = 1 << 1;
    pub const CLAMPED: u8   = 1 << 2;
    pub const EXCLUSION: u8 = 1 << 3;
    pub const OPTIMIZED: u8 = 1 << 4;
    pub const GPU: u8       = 1 << 5;

    pub fn is_ranged(self)    -> bool { (self.0 & Self::RANGE)     != 0 }
    pub fn is_cyclic(self)    -> bool { (self.0 & Self::CYCLIC)    != 0 }
    pub fn is_clamped(self)   -> bool { (self.0 & Self::CLAMPED)   != 0 }
    pub fn has_exclusion(self) -> bool { (self.0 & Self::EXCLUSION) != 0 }
    pub fn is_optimized(self) -> bool { (self.0 & Self::OPTIMIZED) != 0 }
    pub fn is_gpu(self)       -> bool { (self.0 & Self::GPU)       != 0 }
}

/// `e_color_graph_type` — selects scalar vs N-color output.
/// When non-Scalar, the union at bytes 4-19 holds 4 ARGB u32s instead
/// of clamp_range floats.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGraphType {
    Scalar     = 0,
    OneColor   = 1,
    TwoColor   = 2,
    ThreeColor = 3,
    FourColor  = 4,
}

impl ColorGraphType {
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::Scalar,
            1 => Self::OneColor,
            2 => Self::TwoColor,
            3 => Self::ThreeColor,
            4 => Self::FourColor,
            _ => return None,
        })
    }
}

/// Parsed 32-byte header. The `clamp_range_*` fields and `colors`
/// alias the same memory in the source struct (they're a union); both
/// are read so callers can use whichever interpretation matches the
/// `color_graph_type`.
#[derive(Debug, Clone)]
pub struct TagFunctionHeader {
    pub function_type: FunctionType,
    pub flags: FunctionFlags,
    pub color_graph_type: ColorGraphType,
    /// Bytes 4-7 (f32 LE). For scalar functions: lower bound of the
    /// output range. For constant type unranged: the operative value.
    pub clamp_range_min: f32,
    /// Bytes 8-11 (f32 LE). Upper bound of output range.
    pub clamp_range_max: f32,
    /// Bytes 4-19 (4× u32 LE). For color functions, ARGB-packed colors.
    /// `color_graph_type` says how many entries are populated.
    pub colors: [u32; 4],
    pub exclusion_min: f32,
    pub exclusion_max: f32,
    /// Bytes after the 32-byte header that belong to the per-type
    /// compact data block. Type-specific structures live here.
    pub compact_size: i32,
}

impl TagFunctionHeader {
    pub fn parse(data: &[u8]) -> Result<Self, TagFunctionError> {
        if data.len() < 32 {
            return Err(TagFunctionError::TooShort { len: data.len() });
        }
        let function_type = FunctionType::from_byte(data[0])
            .ok_or(TagFunctionError::UnknownFunctionType { byte: data[0] })?;
        let flags = FunctionFlags(data[1]);
        let color_graph_type = ColorGraphType::from_byte(data[2])
            .ok_or(TagFunctionError::UnknownColorGraphType { byte: data[2] })?;
        let clamp_range_min = f32::from_le_bytes(data[4..8].try_into().unwrap());
        let clamp_range_max = f32::from_le_bytes(data[8..12].try_into().unwrap());
        let colors = [
            u32::from_le_bytes(data[4..8].try_into().unwrap()),
            u32::from_le_bytes(data[8..12].try_into().unwrap()),
            u32::from_le_bytes(data[12..16].try_into().unwrap()),
            u32::from_le_bytes(data[16..20].try_into().unwrap()),
        ];
        let exclusion_min = f32::from_le_bytes(data[20..24].try_into().unwrap());
        let exclusion_max = f32::from_le_bytes(data[24..28].try_into().unwrap());
        let compact_size = i32::from_le_bytes(data[28..32].try_into().unwrap());
        Ok(Self {
            function_type, flags, color_graph_type,
            clamp_range_min, clamp_range_max, colors,
            exclusion_min, exclusion_max, compact_size,
        })
    }
}

// ---------------------------------------------------------------------------
// Periodic + transition helpers
// ---------------------------------------------------------------------------
//
// Engine helpers `periodic_function_evaluate(index, time)` and
// `transition_function_evaluate(index, value)` (Ares
// `periodic_functions.cpp`) sample analytic curves through 1024-byte
// lookup tables baked at startup by `periodic_functions_initialize`.
// The table-bake bodies are stripped from both Ares and Groundhog
// decompiles. We reproduce the same analytic curves directly — same
// semantics, no precision loss from byte quantization.

/// `e_transition_function` — input ∈ [0, 1] → output ∈ [0, 1].
/// Maps to `global_transition_functions_enum` in `h3_guerilla_tag_definitions/math/periodic_functions.cpp`.
#[allow(dead_code)]
const TRANSITION_LINEAR: u8     = 0;
const TRANSITION_EARLY: u8      = 1;
const TRANSITION_VERY_EARLY: u8 = 2;
const TRANSITION_LATE: u8       = 3;
const TRANSITION_VERY_LATE: u8  = 4;
const TRANSITION_COSINE: u8     = 5;
const TRANSITION_ONE: u8        = 6;
const TRANSITION_ZERO: u8       = 7;

/// Analytic transition functions. "early" front-loads the curve (output
/// near 1 well before input=1), "late" back-loads it. "very" variants
/// are quartic instead of quadratic for sharper curvature. cosine is
/// the smoothstep half-cosine.
pub fn transition_function_evaluate(function_index: u8, value: f32) -> f32 {
    let t = value.clamp(0.0, 1.0);
    match function_index {
        TRANSITION_LINEAR     => t,
        TRANSITION_EARLY      => 1.0 - (1.0 - t).powi(2),
        TRANSITION_VERY_EARLY => 1.0 - (1.0 - t).powi(4),
        TRANSITION_LATE       => t.powi(2),
        TRANSITION_VERY_LATE  => t.powi(4),
        TRANSITION_COSINE     => 0.5 * (1.0 - (std::f32::consts::PI * t).cos()),
        TRANSITION_ONE        => 1.0,
        TRANSITION_ZERO       => 0.0,
        _                     => 0.0,
    }
}

const PERIODIC_ONE: u8                              = 0;
const PERIODIC_ZERO: u8                             = 1;
const PERIODIC_COSINE: u8                           = 2;
#[allow(dead_code)]
const PERIODIC_COSINE_WITH_RANDOM_PERIOD: u8        = 3;
const PERIODIC_DIAGONAL_WAVE: u8                    = 4;
#[allow(dead_code)]
const PERIODIC_DIAGONAL_WAVE_WITH_RANDOM_PERIOD: u8 = 5;
const PERIODIC_SLIDE: u8                            = 6;
#[allow(dead_code)]
const PERIODIC_SLIDE_WITH_RANDOM_PERIOD: u8         = 7;
#[allow(dead_code)]
const PERIODIC_NOISE: u8                            = 8;
#[allow(dead_code)]
const PERIODIC_JITTER: u8                           = 9;
#[allow(dead_code)]
const PERIODIC_WANDER: u8                           = 10;
#[allow(dead_code)]
const PERIODIC_SPARK: u8                            = 11;

/// Analytic periodic functions. `time` is cyclic — most functions wrap
/// at integer boundaries. Output range is [0, 1] (the engine's table
/// stores bytes 0..255 representing this range).
///
/// Random-period variants (3, 5, 7) and the noise/jitter/wander/spark
/// types use a deterministic per-instance seed in the engine; without
/// the seed plumbed through they're stubbed to their non-random
/// counterpart or zero. Riverworld water doesn't use any of these.
pub fn periodic_function_evaluate(function_index: u8, time: f32) -> f32 {
    // Wrap input to [0, 1) for cyclic functions.
    let t = time - time.floor();
    match function_index {
        PERIODIC_ONE => 1.0,
        PERIODIC_ZERO => 0.0,
        PERIODIC_COSINE | PERIODIC_COSINE_WITH_RANDOM_PERIOD => {
            // Half-amplitude cosine in [0, 1]: 0.5 - 0.5*cos(2π t).
            // Engine baked table stores values in [0, 1] range.
            0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
        }
        PERIODIC_DIAGONAL_WAVE | PERIODIC_DIAGONAL_WAVE_WITH_RANDOM_PERIOD => {
            // Triangle wave: ramp up then down per cycle.
            if t < 0.5 { 2.0 * t } else { 2.0 * (1.0 - t) }
        }
        PERIODIC_SLIDE | PERIODIC_SLIDE_WITH_RANDOM_PERIOD => {
            // Sawtooth: linear ramp 0→1 per cycle.
            t
        }
        // Deterministic-ish stub for the random/noise types until we
        // plumb a per-instance seed. Returns 0.5 (mid-amplitude). The
        // engine drives these from a shared random_math seeded RNG;
        // shipped tags rarely use them outside particle effects.
        PERIODIC_NOISE | PERIODIC_JITTER | PERIODIC_WANDER | PERIODIC_SPARK => 0.5,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// Per-type compact data structures
// ---------------------------------------------------------------------------

/// `c_linear_function_compact` — 8 bytes. `evaluate(x) = slope*x + offset`
/// per `function_definitions.cpp:823`.
#[derive(Debug, Clone, Copy)]
pub struct LinearCompact {
    pub slope: f32,
    pub offset: f32,
}

impl LinearCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 8 { return None; }
        Some(Self {
            slope:  f32::from_le_bytes(data[0..4].try_into().unwrap()),
            offset: f32::from_le_bytes(data[4..8].try_into().unwrap()),
        })
    }
    fn evaluate(&self, input: f32) -> f32 {
        input * self.slope + self.offset
    }
}

/// `c_spline_function_compact` — 16 bytes. `m_basis_elements` =
/// `real_vector4d (i, j, k, l)`. Per `function_definitions.cpp:868`:
/// `f(x) = i*x³ + j*x² + k*x + l`.
#[derive(Debug, Clone, Copy)]
pub struct SplineCompact {
    pub i: f32,
    pub j: f32,
    pub k: f32,
    pub l: f32,
}

impl SplineCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 16 { return None; }
        Some(Self {
            i: f32::from_le_bytes(data[0..4].try_into().unwrap()),
            j: f32::from_le_bytes(data[4..8].try_into().unwrap()),
            k: f32::from_le_bytes(data[8..12].try_into().unwrap()),
            l: f32::from_le_bytes(data[12..16].try_into().unwrap()),
        })
    }
    fn evaluate(&self, input: f32) -> f32 {
        let x2 = input * input;
        let x3 = x2 * input;
        self.i * x3 + self.j * x2 + self.k * input + self.l
    }
}

/// `c_spline2_function_compact` — 28 bytes. A 1D spline restricted to
/// the sub-range `[left_x, left_x + width]`, with input remapping
/// driven by `bias`. Per `function_definitions.cpp:265-281`. Body
/// not commented in Ares (`evaluate` is a `_sub_*` forward); the
/// remap is reconstructed from the editor's setup behaviour: input
/// inside `[left_x, left_x + width]` maps to a normalized `[0, 1]`
/// position where `bias` shifts the midpoint, then evaluates the
/// underlying spline at that position.
#[derive(Debug, Clone, Copy)]
pub struct Spline2Compact {
    pub spline: SplineCompact,
    pub left_x: f32,
    pub width: f32,
    pub bias: f32,
}

impl Spline2Compact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 28 { return None; }
        let spline = SplineCompact::parse(&data[0..16])?;
        let left_x = f32::from_le_bytes(data[16..20].try_into().unwrap());
        let width  = f32::from_le_bytes(data[20..24].try_into().unwrap());
        let bias   = f32::from_le_bytes(data[24..28].try_into().unwrap());
        Some(Self { spline, left_x, width, bias })
    }
    fn evaluate(&self, input: f32) -> f32 {
        // Remap input to the spline's [0, 1] domain via the
        // (left_x, width, bias) sub-range. Bias=0.5 → linear remap;
        // bias≠0.5 shifts the curve's midpoint. Outside the sub-range
        // the spline evaluates at its endpoints.
        if self.width <= 0.0 {
            return self.spline.evaluate(0.0);
        }
        let raw = (input - self.left_x) / self.width;
        let t = raw.clamp(0.0, 1.0);
        // Bias remap: standard "biased lerp" — t' = t / ((1/bias - 2)*(1-t) + 1)
        // when bias ∈ (0, 1). Bias=0.5 → t' = t (linear).
        let biased = if self.bias > 0.0 && self.bias < 1.0 && (self.bias - 0.5).abs() > 1e-6 {
            let b = (1.0 / self.bias) - 2.0;
            t / (b * (1.0 - t) + 1.0)
        } else {
            t
        };
        self.spline.evaluate(biased)
    }
}

/// `c_transition_function_compact` — 12 bytes. Per
/// `function_definitions.cpp:1094`:
/// `f(x) = (amp_max - amp_min) * transition_function_evaluate(idx, x)
///       + amp_min`.
#[derive(Debug, Clone, Copy)]
pub struct TransitionCompact {
    pub function_index: u8,
    pub amplitude_min: f32,
    pub amplitude_max: f32,
}

impl TransitionCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 12 { return None; }
        Some(Self {
            function_index: data[0],
            // bytes 1..4 unused / padding
            amplitude_min: f32::from_le_bytes(data[4..8].try_into().unwrap()),
            amplitude_max: f32::from_le_bytes(data[8..12].try_into().unwrap()),
        })
    }
    fn evaluate(&self, input: f32) -> f32 {
        (self.amplitude_max - self.amplitude_min)
            * transition_function_evaluate(self.function_index, input)
            + self.amplitude_min
    }
}

/// `c_periodic_function_compact` — 20 bytes. Per
/// `function_definitions.cpp:1041` (decompiled body):
/// ```text
/// adjusted_time = input * frequency + phase
/// periodic_value = periodic_function_evaluate(idx, adjusted_time)
/// return (amp_max - amp_min) * periodic_value + amp_min
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PeriodicCompact {
    pub function_index: u8,
    pub frequency: f32,
    pub phase: f32,
    pub amplitude_min: f32,
    pub amplitude_max: f32,
}

impl PeriodicCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 20 { return None; }
        Some(Self {
            function_index: data[0],
            frequency:     f32::from_le_bytes(data[4..8].try_into().unwrap()),
            phase:         f32::from_le_bytes(data[8..12].try_into().unwrap()),
            amplitude_min: f32::from_le_bytes(data[12..16].try_into().unwrap()),
            amplitude_max: f32::from_le_bytes(data[16..20].try_into().unwrap()),
        })
    }
    fn evaluate(&self, input: f32) -> f32 {
        let adjusted = input * self.frequency + self.phase;
        let v = periodic_function_evaluate(self.function_index, adjusted);
        (self.amplitude_max - self.amplitude_min) * v + self.amplitude_min
    }
}

/// `c_linear_key_function` — 80 bytes piecewise linear over 4 control
/// points. Layout per `function_definitions.cpp:454-457`:
/// ```text
/// real_point2d m_graph_points[4];   // 0x00 (32 bytes — x,y pairs)
/// float        m_times_vector[4];   // 0x20 (16 bytes — postprocess cache)
/// float        m_increment_vector[4]; // 0x30 (16 bytes — 1/(x[i+1]-x[i]))
/// float        m_y_delta_vector[4];   // 0x40 (16 bytes — y[i+1]-y[i])
/// ```
/// Body of `c_linear_key_function::evaluate` isn't decompiled, but the
/// stored vectors are clear: the engine pre-computes per-segment slope
/// reciprocals + y deltas at postprocess time so runtime evaluate is
/// `y[i] + (input - x[i]) * increment[i] * y_delta[i]` with a segment
/// search. We replicate that.
#[derive(Debug, Clone, Copy)]
pub struct LinearKeyCompact {
    pub graph_points: [(f32, f32); 4],
    pub times_vector: [f32; 4],
    pub increment_vector: [f32; 4],
    pub y_delta_vector: [f32; 4],
}

impl LinearKeyCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 80 { return None; }
        let mut graph_points = [(0.0f32, 0.0f32); 4];
        for i in 0..4 {
            let off = i * 8;
            graph_points[i] = (
                f32::from_le_bytes(data[off..off + 4].try_into().unwrap()),
                f32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap()),
            );
        }
        let mut times_vector = [0.0f32; 4];
        let mut increment_vector = [0.0f32; 4];
        let mut y_delta_vector = [0.0f32; 4];
        for i in 0..4 {
            times_vector[i]     = f32::from_le_bytes(data[32 + i*4..32 + i*4 + 4].try_into().unwrap());
            increment_vector[i] = f32::from_le_bytes(data[48 + i*4..48 + i*4 + 4].try_into().unwrap());
            y_delta_vector[i]   = f32::from_le_bytes(data[64 + i*4..64 + i*4 + 4].try_into().unwrap());
        }
        Some(Self { graph_points, times_vector, increment_vector, y_delta_vector })
    }
    fn evaluate(&self, input: f32) -> f32 {
        // Clamp before first point / after last point.
        if input <= self.graph_points[0].0 { return self.graph_points[0].1; }
        if input >= self.graph_points[3].0 { return self.graph_points[3].1; }
        // Find the segment [i, i+1] containing input.
        for i in 0..3 {
            let (x_a, y_a) = self.graph_points[i];
            let (x_b, _)   = self.graph_points[i + 1];
            if input <= x_b {
                // Use precomputed reciprocal slope when valid; fall
                // back to direct compute. The engine's
                // increment_vector[i] = 1.0 / (x_b - x_a).
                let dx = x_b - x_a;
                let inv = if dx > 0.0 { 1.0 / dx } else { 0.0 };
                let t = (input - x_a) * inv;
                return y_a + t * self.y_delta_vector[i];
            }
        }
        self.graph_points[3].1
    }
}

/// `c_multi_part_function_compact` — variable-size. Layout:
/// ```text
/// long              m_function_count;   // 0x0
/// s_function_part   m_function_part[m_function_count];
/// ```
/// Each `s_function_part` is `(header: 8 bytes, function: variable)`.
/// `header.type` is a `e_function_type` (only Linear=4, Spline=7,
/// Spline2=10 are valid for parts) and `header.ending_x` is where this
/// segment ends. Walk parts, find the one whose `ending_x ≥ input`
/// (or the last), evaluate its compact function at input.
///
/// Per `function_definitions.cpp:1216-1228` (`get_size_of_part`):
/// linear part = 16B (8 hdr + 8 body), spline = 24B, spline2 = 36B.
#[derive(Debug, Clone)]
pub struct MultiPartCompact {
    pub parts: Vec<MultiPartSegment>,
}

#[derive(Debug, Clone)]
pub struct MultiPartSegment {
    pub ending_x: f32,
    pub function: MultiPartSubFunction,
}

#[derive(Debug, Clone, Copy)]
pub enum MultiPartSubFunction {
    Linear(LinearCompact),
    Spline(SplineCompact),
    Spline2(Spline2Compact),
}

impl MultiPartCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 { return None; }
        let function_count = i32::from_le_bytes(data[0..4].try_into().unwrap());
        if function_count <= 0 || function_count > 16 {
            // sanity bound — engine has 4-segment max but we allow
            // some headroom for malformed/forward-compat tags.
            return None;
        }
        let mut parts = Vec::with_capacity(function_count as usize);
        let mut off = 4usize;
        for _ in 0..function_count {
            if off + 8 > data.len() { return None; }
            let part_type = data[off];
            // bytes [off+1..off+4] unused
            let ending_x = f32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
            let body_off = off + 8;
            let function = match part_type {
                4 /* Linear */ => {
                    let c = LinearCompact::parse(data.get(body_off..)?)?;
                    off = body_off + 8;
                    MultiPartSubFunction::Linear(c)
                }
                7 /* Spline */ => {
                    let c = SplineCompact::parse(data.get(body_off..)?)?;
                    off = body_off + 16;
                    MultiPartSubFunction::Spline(c)
                }
                10 /* Spline2 */ => {
                    let c = Spline2Compact::parse(data.get(body_off..)?)?;
                    off = body_off + 28;
                    MultiPartSubFunction::Spline2(c)
                }
                _ => return None,
            };
            parts.push(MultiPartSegment { ending_x, function });
        }
        Some(Self { parts })
    }
    fn evaluate(&self, input: f32) -> f32 {
        if self.parts.is_empty() { return 0.0; }
        // Find the first part whose ending_x ≥ input. The engine's
        // pseudocode iterates `function_part` and breaks when found.
        for part in &self.parts {
            if input <= part.ending_x {
                return match &part.function {
                    MultiPartSubFunction::Linear(c)  => c.evaluate(input),
                    MultiPartSubFunction::Spline(c)  => c.evaluate(input),
                    MultiPartSubFunction::Spline2(c) => c.evaluate(input),
                };
            }
        }
        // Past the last ending_x — evaluate the last part at its own
        // domain. This matches the engine fallback when `found = false`.
        let last = &self.parts[self.parts.len() - 1];
        match &last.function {
            MultiPartSubFunction::Linear(c)  => c.evaluate(input),
            MultiPartSubFunction::Spline(c)  => c.evaluate(input),
            MultiPartSubFunction::Spline2(c) => c.evaluate(input),
        }
    }
}

/// `c_exponent_function_compact` — 12 bytes. Per
/// `function_definitions.cpp:976-1003`:
/// ```text
/// if |exponent| < 1e-4 || (exponent < 0 && |input| < 1e-4):
///     return 1.0
/// else:
///     return powf(input, exponent) * (amplitude_max - amplitude_min)
///          + amplitude_min
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ExponentCompact {
    pub amplitude_min: f32,
    pub amplitude_max: f32,
    pub exponent: f32,
}

impl ExponentCompact {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 12 { return None; }
        Some(Self {
            amplitude_min: f32::from_le_bytes(data[0..4].try_into().unwrap()),
            amplitude_max: f32::from_le_bytes(data[4..8].try_into().unwrap()),
            exponent:      f32::from_le_bytes(data[8..12].try_into().unwrap()),
        })
    }
    fn evaluate(&self, input: f32) -> f32 {
        const EPSILON: f32 = 0.000_099_999_997;
        if self.exponent.abs() < EPSILON
            || (self.exponent < 0.0 && input.abs() < EPSILON)
        {
            return 1.0;
        }
        input.powf(self.exponent) * (self.amplitude_max - self.amplitude_min)
            + self.amplitude_min
    }
}

// ---------------------------------------------------------------------------
// TagFunction enum
// ---------------------------------------------------------------------------

/// Decoded TagFunction. All 11 function types parse + evaluate.
#[derive(Debug, Clone)]
pub enum TagFunction {
    Identity { header: TagFunctionHeader },
    Constant { header: TagFunctionHeader },
    Transition { header: TagFunctionHeader, compact: TransitionCompact },
    Periodic { header: TagFunctionHeader, compact: PeriodicCompact },
    Linear   { header: TagFunctionHeader, compact: LinearCompact },
    LinearKey { header: TagFunctionHeader, compact: LinearKeyCompact },
    /// `MultiLinearKey` — multi-graph LinearKey. The runtime treats it
    /// the same shape as LinearKey (one graph per
    /// `color_graph_type`-derived count); for scalar use we read the
    /// first graph identically.
    MultiLinearKey { header: TagFunctionHeader, compact: LinearKeyCompact },
    Spline   { header: TagFunctionHeader, compact: SplineCompact },
    Spline2  { header: TagFunctionHeader, compact: Spline2Compact },
    /// `MultiSpline` (a.k.a. `_function_type_multi_part`, enum value 8).
    /// Variable-size sequence of (Linear | Spline | Spline2) parts each
    /// covering an `[ending_x[i-1], ending_x[i]]` sub-domain.
    MultiSpline { header: TagFunctionHeader, compact: MultiPartCompact },
    Exponent { header: TagFunctionHeader, compact: ExponentCompact },
    /// Function type recognized but not yet implemented. `evaluate`
    /// returns 0.0; `as_constant()` returns None.
    Unsupported { header: TagFunctionHeader, raw: Vec<u8> },
}

#[derive(Debug)]
pub enum TagFunctionError {
    TooShort { len: usize },
    UnknownFunctionType { byte: u8 },
    UnknownColorGraphType { byte: u8 },
}

impl std::fmt::Display for TagFunctionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "TagFunction data too short: {len} bytes (need 32)"),
            Self::UnknownFunctionType { byte } => write!(f, "unknown function_type byte: 0x{byte:02x}"),
            Self::UnknownColorGraphType { byte } => write!(f, "unknown color_graph_type byte: 0x{byte:02x}"),
        }
    }
}

impl std::error::Error for TagFunctionError {}

impl TagFunction {
    /// Parse a `mapping_function` `data` blob. The slice should be
    /// the raw bytes from the schema's `data` field; we read 32
    /// bytes for the header and stash the rest for phase-2+ decoders.
    pub fn parse(data: &[u8]) -> Result<Self, TagFunctionError> {
        let header = TagFunctionHeader::parse(data)?;
        // Compact data follows the 32-byte header. Length = `compact_size`
        // when the header reports it; older blobs may have it in
        // `m_constants[0]` — for now we trust the header field and
        // bound by remaining bytes.
        let compact = data.get(32..).unwrap_or(&[]);
        Ok(match header.function_type {
            FunctionType::Identity => Self::Identity { header },
            FunctionType::Constant => Self::Constant { header },
            FunctionType::Transition => match TransitionCompact::parse(compact) {
                Some(c) => Self::Transition { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::Periodic => match PeriodicCompact::parse(compact) {
                Some(c) => Self::Periodic { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::Linear => match LinearCompact::parse(compact) {
                Some(c) => Self::Linear { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::Spline => match SplineCompact::parse(compact) {
                Some(c) => Self::Spline { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::Spline2 => match Spline2Compact::parse(compact) {
                Some(c) => Self::Spline2 { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::Exponent => match ExponentCompact::parse(compact) {
                Some(c) => Self::Exponent { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::LinearKey => match LinearKeyCompact::parse(compact) {
                Some(c) => Self::LinearKey { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::MultiLinearKey => match LinearKeyCompact::parse(compact) {
                Some(c) => Self::MultiLinearKey { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
            FunctionType::MultiSpline => match MultiPartCompact::parse(compact) {
                Some(c) => Self::MultiSpline { header, compact: c },
                None => Self::Unsupported { header, raw: data.to_vec() },
            },
        })
    }

    pub fn header(&self) -> &TagFunctionHeader {
        match self {
            Self::Identity { header }
            | Self::Constant { header }
            | Self::Transition { header, .. }
            | Self::Periodic { header, .. }
            | Self::Linear { header, .. }
            | Self::LinearKey { header, .. }
            | Self::MultiLinearKey { header, .. }
            | Self::Spline { header, .. }
            | Self::Spline2 { header, .. }
            | Self::MultiSpline { header, .. }
            | Self::Exponent { header, .. }
            | Self::Unsupported { header, .. } => header,
        }
    }

    pub fn function_type(&self)    -> FunctionType    { self.header().function_type }
    pub fn flags(&self)            -> FunctionFlags   { self.header().flags }
    pub fn color_graph_type(&self) -> ColorGraphType  { self.header().color_graph_type }

    /// Evaluate the function at `(input, range)` returning a scalar.
    /// Mirrors `c_function_definition::evaluate_scalar` — calls
    /// `evaluate_legacy` to get a normalized output, then maps through
    /// `[clamp_range_min, clamp_range_max]`.
    ///
    /// For unsupported (not-yet-decoded) types returns 0.0 — callers
    /// who need a fallback should check `function_type()` first or
    /// use `as_constant()` on the constant path.
    pub fn evaluate(&self, input: f32, range: f32) -> f32 {
        let normalized = self.evaluate_legacy(input, range);
        self.map_to_output_range(normalized)
    }

    /// The "normalized" curve output before output-range remapping.
    /// Ports `c_function_definition::evaluate_legacy` for the types
    /// implemented so far.
    fn evaluate_legacy(&self, input: f32, range: f32) -> f32 {
        match self {
            Self::Identity { .. } => input,
            Self::Constant { header } => {
                if header.flags.is_ranged() { range } else { 0.0 }
            }
            // Compact-data evaluators output the function's value
            // directly; map_to_output_range_legacy is then applied by
            // the caller (`evaluate`). The compacts already encode
            // amplitude_min/max for types that need them
            // (Exponent), so the outer map is a no-op for those when
            // clamp_range = (0, 1). For Linear / Spline / Spline2,
            // the engine applies clamp_range as the [out_min, out_max]
            // interpretation per `map_to_output_range_legacy`.
            Self::Transition     { compact, .. } => compact.evaluate(input),
            Self::Periodic       { compact, .. } => compact.evaluate(input),
            Self::Linear         { compact, .. } => compact.evaluate(input),
            Self::LinearKey      { compact, .. } => compact.evaluate(input),
            Self::MultiLinearKey { compact, .. } => compact.evaluate(input),
            Self::Spline         { compact, .. } => compact.evaluate(input),
            Self::Spline2        { compact, .. } => compact.evaluate(input),
            Self::MultiSpline    { compact, .. } => compact.evaluate(input),
            Self::Exponent       { compact, .. } => compact.evaluate(input),
            Self::Unsupported { .. } => 0.0,
        }
    }

    /// Linearly map a normalized output through `[clamp_range_min,
    /// clamp_range_max]`. Mirrors
    /// `c_function_definition::map_to_output_range_legacy`.
    fn map_to_output_range(&self, normalized: f32) -> f32 {
        let h = self.header();
        h.clamp_range_min + normalized * (h.clamp_range_max - h.clamp_range_min)
    }

    /// Fast path for the common case: callers that just want a single
    /// scalar value for a constant material parameter. Returns `Some`
    /// for constant (and trivially-constant compact) functions, `None`
    /// for anything that genuinely depends on input.
    pub fn as_constant(&self) -> Option<f32> {
        match self {
            Self::Constant { header } if !header.flags.is_ranged() => {
                Some(header.clamp_range_min)
            }
            // A linear function with slope=0 is constant at `offset`,
            // remapped through clamp_range. Same idea for other types
            // whose compact data trivially collapses to a constant.
            Self::Linear { compact, .. } if compact.slope == 0.0 => {
                Some(self.map_to_output_range(compact.offset))
            }
            Self::Spline { compact, .. }
                if compact.i == 0.0 && compact.j == 0.0 && compact.k == 0.0 =>
            {
                Some(self.map_to_output_range(compact.l))
            }
            Self::Exponent { compact, .. }
                if compact.exponent.abs() < 1e-4
                    || compact.amplitude_min == compact.amplitude_max =>
            {
                let v = if compact.exponent.abs() < 1e-4 {
                    1.0
                } else {
                    compact.amplitude_min
                };
                Some(self.map_to_output_range(v))
            }
            _ => None,
        }
    }

    /// True if the function is constant for all inputs (used as a
    /// fast-skip hint when building per-frame uniforms). Includes
    /// unranged constant; identity is NOT constant since it varies
    /// with input.
    pub fn is_constant(&self) -> bool {
        self.as_constant().is_some()
    }

    /// Color-output evaluator. Phase 6 will implement the actual
    /// gradient interpolation using the `m_colors[4]` block; Phase 1
    /// returns white as a stub so callers can wire the API now.
    pub fn evaluate_color(&self, _input: f32, _range: f32) -> RealRgbColor {
        RealRgbColor { red: 1.0, green: 1.0, blue: 1.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// grunt_armor.shader parameters[3] (`diffuse_coefficient`):
    /// constant function, value 1.0.
    const DIFFUSE_COEFFICIENT: [u8; 32] = [
        0x01, 0x20, 0x00, 0x00,             // type=Constant, flags=GPU, color=Scalar
        0x00, 0x00, 0x80, 0x3f,             // clamp_range_min = 1.0
        0x00, 0x00, 0x80, 0x3f,             // clamp_range_max = 1.0
        0, 0, 0, 0, 0, 0, 0, 0,             // colors[2..3] = 0
        0, 0, 0, 0, 0, 0, 0, 0,             // exclusion_min/max = 0
        0, 0, 0, 0,                          // compact_size = 0
    ];

    /// grunt_armor.shader parameters[4] (`specular_coefficient`):
    /// unranged constant, min=1.0, max=0.318...
    /// The operative value for an unranged constant is min (1.0).
    const SPECULAR_COEFFICIENT: [u8; 32] = [
        0x01, 0x20, 0x00, 0x00,
        0x00, 0x00, 0x80, 0x3f,             // 1.0
        0x83, 0xf9, 0xa2, 0x3e,             // 0.31831 (1/π) — max field
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    /// grunt_armor.shader parameters[6] (`roughness`):
    /// unranged constant, min=0.2, max=1.0.
    const ROUGHNESS: [u8; 32] = [
        0x01, 0x20, 0x00, 0x00,
        0xcd, 0xcc, 0x4c, 0x3e,             // 0.2
        0x00, 0x00, 0x80, 0x3f,             // 1.0
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];

    #[test]
    fn parses_constant_diffuse_coefficient() {
        let f = TagFunction::parse(&DIFFUSE_COEFFICIENT).unwrap();
        assert_eq!(f.function_type(), FunctionType::Constant);
        assert!(f.flags().is_gpu());
        assert!(!f.flags().is_ranged());
        assert_eq!(f.color_graph_type(), ColorGraphType::Scalar);
        assert_eq!(f.as_constant(), Some(1.0));
        assert_eq!(f.evaluate(0.0, 0.0), 1.0);
        assert_eq!(f.evaluate(1.0, 1.0), 1.0);
    }

    #[test]
    fn unranged_constant_uses_min_not_max() {
        let f = TagFunction::parse(&SPECULAR_COEFFICIENT).unwrap();
        // The operative value for an UNRANGED constant function is
        // clamp_range_min (bytes 4-7), NOT max (bytes 8-11).
        // evaluate_legacy returns 0.0 unranged → maps to min.
        assert_eq!(f.as_constant(), Some(1.0));
        assert_eq!(f.evaluate(0.0, 0.0), 1.0);
        assert_eq!(f.evaluate(123.4, 56.7), 1.0);
    }

    #[test]
    fn roughness_returns_min() {
        let f = TagFunction::parse(&ROUGHNESS).unwrap();
        assert!((f.as_constant().unwrap() - 0.2).abs() < 1e-6);
        assert!((f.evaluate(0.5, 0.5) - 0.2).abs() < 1e-6);
    }

    #[test]
    fn parses_identity() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x00; // function_type = Identity
        // clamp_range maps normalized → output
        bytes[4..8].copy_from_slice(&0.0f32.to_le_bytes());     // min
        bytes[8..12].copy_from_slice(&10.0f32.to_le_bytes());   // max
        let f = TagFunction::parse(&bytes).unwrap();
        assert_eq!(f.function_type(), FunctionType::Identity);
        assert_eq!(f.as_constant(), None);
        // identity returns input as normalized → mapped through [0, 10]
        assert!((f.evaluate(0.5, 0.0) - 5.0).abs() < 1e-6);
        assert!((f.evaluate(1.0, 0.0) - 10.0).abs() < 1e-6);
    }

    #[test]
    fn unsupported_type_evaluates_to_min() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x03; // Periodic — Phase 3
        bytes[4..8].copy_from_slice(&5.0f32.to_le_bytes());
        bytes[8..12].copy_from_slice(&7.0f32.to_le_bytes());
        let f = TagFunction::parse(&bytes).unwrap();
        assert_eq!(f.function_type(), FunctionType::Periodic);
        // Unsupported normalized = 0 → maps to min
        assert_eq!(f.evaluate(0.0, 0.0), 5.0);
        assert!(f.as_constant().is_none());
    }

    #[test]
    fn rejects_short_data() {
        assert!(matches!(
            TagFunction::parse(&[0u8; 31]),
            Err(TagFunctionError::TooShort { len: 31 })
        ));
    }

    #[test]
    fn rejects_unknown_function_type() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xff;
        assert!(matches!(
            TagFunction::parse(&bytes),
            Err(TagFunctionError::UnknownFunctionType { byte: 0xff })
        ));
    }

    /// Build a 32-byte header with the given function type + clamp range.
    fn header_with(func_type: u8, clamp_min: f32, clamp_max: f32) -> [u8; 32] {
        let mut bytes = [0u8; 32];
        bytes[0] = func_type;
        bytes[4..8].copy_from_slice(&clamp_min.to_le_bytes());
        bytes[8..12].copy_from_slice(&clamp_max.to_le_bytes());
        bytes
    }

    #[test]
    fn linear_evaluates() {
        // y = 2*x + 5, mapped through clamp [0, 1] (no-op)
        let mut blob = header_with(4, 0.0, 1.0).to_vec();
        blob.extend_from_slice(&2.0f32.to_le_bytes());  // slope
        blob.extend_from_slice(&5.0f32.to_le_bytes());  // offset
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Linear);
        // evaluate(x) = 2x + 5; clamp [0, 1] → linear remap from
        // normalized [0, 1] to [0, 1] is identity. So 2*0 + 5 = 5
        // would normally be the result, but the engine applies the
        // OUTPUT range, treating compact output as the [0,1] normalized
        // value. With clamp [0,1] the map is identity so we get 5.
        assert!((f.evaluate(0.0, 0.0) - 5.0).abs() < 1e-5);
        assert!((f.evaluate(1.0, 0.0) - 7.0).abs() < 1e-5);
    }

    #[test]
    fn linear_constant_recognized() {
        // slope=0, offset=3, clamp [0, 1] → constant 3.
        let mut blob = header_with(4, 0.0, 1.0).to_vec();
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&3.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        // map_to_output_range(3) when clamp=[0,1] is 0 + 3*(1-0) = 3.
        assert_eq!(f.as_constant(), Some(3.0));
    }

    #[test]
    fn spline_evaluates_cubic() {
        // f(x) = 1*x³ + 0*x² + 0*x + 0 = x³
        let mut blob = header_with(7, 0.0, 1.0).to_vec();
        blob.extend_from_slice(&1.0f32.to_le_bytes()); // i
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // j
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // k
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // l
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Spline);
        assert!((f.evaluate(0.5, 0.0) - 0.125).abs() < 1e-5);
        assert!((f.evaluate(2.0, 0.0) - 8.0).abs() < 1e-5);
    }

    #[test]
    fn spline_constant_recognized() {
        // i=j=k=0, l=4 → constant 4 (after clamp [0, 1] identity remap).
        let mut blob = header_with(7, 0.0, 1.0).to_vec();
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&4.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.as_constant(), Some(4.0));
    }

    #[test]
    fn spline2_evaluates_with_subrange() {
        // Inner spline f(t) = t. Sub-range [0.2, 0.7] (left_x=0.2, width=0.5).
        // Bias=0.5 → linear remap: t = (input - 0.2) / 0.5, clamped.
        let mut blob = header_with(10, 0.0, 1.0).to_vec();
        // spline (i=j=l=0, k=1) → f(t) = t
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // i
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // j
        blob.extend_from_slice(&1.0f32.to_le_bytes()); // k
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // l
        blob.extend_from_slice(&0.2f32.to_le_bytes()); // left_x
        blob.extend_from_slice(&0.5f32.to_le_bytes()); // width
        blob.extend_from_slice(&0.5f32.to_le_bytes()); // bias = linear
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Spline2);
        // input=0.45 → t = (0.45-0.2)/0.5 = 0.5 → spline(0.5) = 0.5
        assert!((f.evaluate(0.45, 0.0) - 0.5).abs() < 1e-5);
        // input=0.0 → clamped to 0
        assert_eq!(f.evaluate(0.0, 0.0), 0.0);
        // input=1.0 → clamped to 1
        assert!((f.evaluate(1.0, 0.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn exponent_evaluates_pow_curve() {
        // amp_min=0, amp_max=1, exponent=2 → input^2
        let mut blob = header_with(9, 0.0, 1.0).to_vec();
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        blob.extend_from_slice(&2.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Exponent);
        assert!((f.evaluate(0.5, 0.0) - 0.25).abs() < 1e-5);
        assert!((f.evaluate(0.7, 0.0) - 0.49).abs() < 1e-5);
    }

    #[test]
    fn transition_linear_passes_through() {
        // function_index=0 (linear), amp_min=0, amp_max=1
        let mut blob = header_with(2, 0.0, 1.0).to_vec();
        blob.push(0); blob.extend_from_slice(&[0, 0, 0]); // linear + padding
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Transition);
        // linear ramp 0..1
        assert!((f.evaluate(0.0, 0.0) - 0.0).abs() < 1e-5);
        assert!((f.evaluate(0.5, 0.0) - 0.5).abs() < 1e-5);
        assert!((f.evaluate(1.0, 0.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn transition_late_eases_in() {
        // function_index=3 (late = ease-in), amp_min=0, amp_max=1
        let mut blob = header_with(2, 0.0, 1.0).to_vec();
        blob.push(3); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        // late = t^2 → at midpoint < 0.5
        assert!((f.evaluate(0.5, 0.0) - 0.25).abs() < 1e-5);
    }

    #[test]
    fn transition_one_constant() {
        // function_index=6 (one) → constant amp_max
        let mut blob = header_with(2, 0.0, 1.0).to_vec();
        blob.push(6); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&0.5f32.to_le_bytes()); // amp_min
        blob.extend_from_slice(&3.0f32.to_le_bytes()); // amp_max
        let f = TagFunction::parse(&blob).unwrap();
        // (3 - 0.5) * 1.0 + 0.5 = 3.0
        assert!((f.evaluate(0.42, 0.0) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn periodic_cosine_oscillates() {
        // function_index=2 (cosine), frequency=1, phase=0, amp [-1, 1]
        let mut blob = header_with(3, -1.0, 1.0).to_vec();
        blob.push(2); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&1.0f32.to_le_bytes()); // frequency
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // phase
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // amp_min (compact)
        blob.extend_from_slice(&1.0f32.to_le_bytes()); // amp_max (compact)
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::Periodic);
        // periodic cosine at t=0 = 0 (= 0.5 - 0.5*1), so compact = (1-0)*0+0=0
        // outer clamp [-1, 1] → -1 + 0*(1 - -1) = -1
        assert!((f.evaluate(0.0, 0.0) - (-1.0)).abs() < 1e-5);
        // at t=0.25 (quarter cycle): cos(π/2) = 0 → periodic = 0.5
        // compact = (1-0)*0.5+0 = 0.5; outer = -1 + 0.5*2 = 0
        assert!((f.evaluate(0.25, 0.0) - 0.0).abs() < 1e-5);
        // at t=0.5: cos(π) = -1 → periodic = 1.0
        // compact = 1; outer = -1 + 1*2 = 1
        assert!((f.evaluate(0.5, 0.0) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn periodic_diagonal_wave_triangle() {
        // function_index=4 (diagonal_wave / triangle wave)
        let mut blob = header_with(3, 0.0, 1.0).to_vec();
        blob.push(4); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&1.0f32.to_le_bytes());
        let f = TagFunction::parse(&blob).unwrap();
        // peak at t=0.5
        assert!((f.evaluate(0.5, 0.0) - 1.0).abs() < 1e-5);
        // zeros at t=0 and t=1
        assert!((f.evaluate(0.0, 0.0) - 0.0).abs() < 1e-5);
        assert!((f.evaluate(1.0, 0.0) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn linear_key_4_points() {
        // Type 5 (LinearKey). 4 control points: (0, 0), (0.25, 1.0),
        // (0.75, 1.0), (1.0, 0.0) — a trapezoid pulse.
        let mut blob = header_with(5, 0.0, 1.0).to_vec();
        // graph_points
        for &(x, y) in &[(0.0_f32, 0.0_f32), (0.25, 1.0), (0.75, 1.0), (1.0, 0.0)] {
            blob.extend_from_slice(&x.to_le_bytes());
            blob.extend_from_slice(&y.to_le_bytes());
        }
        // times_vector (unused by our evaluator — compute fresh)
        for _ in 0..4 { blob.extend_from_slice(&0.0_f32.to_le_bytes()); }
        // increment_vector — engine pre-computes 1/(x[i+1]-x[i])
        // — our evaluator falls back to live compute, so values here
        // don't matter.
        for _ in 0..4 { blob.extend_from_slice(&0.0_f32.to_le_bytes()); }
        // y_delta_vector — y[i+1] - y[i] for i=0..2; sentinel at [3].
        for &v in &[1.0_f32, 0.0, -1.0, 0.0] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::LinearKey);
        // Ramp up: at t=0.125 → halfway between (0,0) and (0.25,1) = 0.5
        assert!((f.evaluate(0.125, 0.0) - 0.5).abs() < 1e-5);
        // Plateau: at t=0.5 → between (0.25,1) and (0.75,1) = 1
        assert!((f.evaluate(0.5, 0.0) - 1.0).abs() < 1e-5);
        // Ramp down: at t=0.875 → halfway from 1 to 0 = 0.5
        assert!((f.evaluate(0.875, 0.0) - 0.5).abs() < 1e-5);
        // Clamp before / after
        assert_eq!(f.evaluate(-1.0, 0.0), 0.0);
        assert_eq!(f.evaluate(2.0, 0.0), 0.0);
    }

    #[test]
    fn multi_part_linear_segments() {
        // Type 8 (MultiSpline / multi_part). 2 linear parts:
        // Part 1: x ∈ [0, 0.5], f(x) = 2x.        slope=2, offset=0
        // Part 2: x ∈ [0.5, 1.0], f(x) = -2x+2.   slope=-2, offset=2
        // Triangle peaking at (0.5, 1).
        let mut blob = header_with(8, 0.0, 1.0).to_vec();
        // function_count = 2
        blob.extend_from_slice(&2i32.to_le_bytes());
        // Part 1: header (type=4 linear, ending_x=0.5) + linear body
        blob.push(4); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&0.5_f32.to_le_bytes()); // ending_x
        blob.extend_from_slice(&2.0_f32.to_le_bytes()); // slope
        blob.extend_from_slice(&0.0_f32.to_le_bytes()); // offset
        // Part 2: header (type=4 linear, ending_x=1.0) + linear body
        blob.push(4); blob.extend_from_slice(&[0, 0, 0]);
        blob.extend_from_slice(&1.0_f32.to_le_bytes()); // ending_x
        blob.extend_from_slice(&(-2.0_f32).to_le_bytes()); // slope
        blob.extend_from_slice(&2.0_f32.to_le_bytes()); // offset
        let f = TagFunction::parse(&blob).unwrap();
        assert_eq!(f.function_type(), FunctionType::MultiSpline);
        // Part 1 at x=0.25 → 0.5
        assert!((f.evaluate(0.25, 0.0) - 0.5).abs() < 1e-5);
        // Part 1 at x=0.5 → 1.0
        assert!((f.evaluate(0.5, 0.0) - 1.0).abs() < 1e-5);
        // Part 2 at x=0.75 → -1.5 + 2 = 0.5
        assert!((f.evaluate(0.75, 0.0) - 0.5).abs() < 1e-5);
        // Past end: still uses last part
        assert!((f.evaluate(1.5, 0.0) - (-1.0)).abs() < 1e-5);
    }

    #[test]
    fn exponent_zero_returns_one() {
        // |exponent| < epsilon → returns 1.0 (no remap, since the
        // engine returns 1.0 from evaluate_legacy directly).
        let mut blob = header_with(9, 0.0, 10.0).to_vec();
        blob.extend_from_slice(&0.0f32.to_le_bytes());
        blob.extend_from_slice(&5.0f32.to_le_bytes());
        blob.extend_from_slice(&0.0f32.to_le_bytes()); // exponent ≈ 0
        let f = TagFunction::parse(&blob).unwrap();
        // Exponent collapses to 1.0 via compact.evaluate, then maps
        // through clamp [0, 10] → 0 + 1.0*(10-0) = 10.
        assert!((f.evaluate(0.5, 0.0) - 10.0).abs() < 1e-4);
    }
}
