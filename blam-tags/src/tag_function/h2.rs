//! Halo 2 classic function — the byte-block `c_function_definition` encoding
//! (`mapping_function` v1, Family B).
//!
//! Unlike the H3+ blob ([`super::BlobFunction`], a 32-byte header + typed
//! compacts), a Halo 2 function on disk is a flat byte-block that the engine
//! reads and edits in place:
//!
//! ```text
//!   byte 0     function_type       (shared FunctionType enum, 0..10)
//!   byte 1     flags               bit 0 = RANGE, bits 4-7 = color graph type
//!   byte 2     function 1          periodic/transition index, or point count
//!   byte 3     function 2            for spline/multi types (graph 1)
//!   bytes 4-19 union               scalar: clamp_range min (4), max (8)
//!                                   color:  four ARGB slots (4, 8, 12, 16)
//!   bytes 20.. graph data          two graphs of `floats_per_graph(type)` f32s
//!                                   each; graph 1 starts at 20 + 4*n
//! ```
//!
//! The block is `20 + 8 * floats_per_graph(type)` bytes (room for both graphs,
//! ranged or not). Everything here is ported from the MCC Halo 2 tool
//! (`halo2_mcc_tool.exe`, `prjh2a2/original/source/math/function_definitions.cpp`):
//! `evaluate` @0x80EC50, `map_to_output_range` @0x810500, `evaluate_color`
//! @0x80F670, `is_constant` @0x810020, the type initializer @0x80EA20, the
//! postprocess @0x810650 and the setters beside them. The 2003 Xbox build kept
//! the graph data in a separate tag block, so its offsets do not apply to MCC.
//!
//! [`H2Function::to_bytes`] returns the block as it stands: an unedited
//! function round-trips byte-identically, and an edit is exactly the engine's
//! in-place write.

use super::editor::color_slots;
use super::tables::FUNCTION_TABLES;
use super::{ColorGraphType, FunctionType};

/// Size of the fixed header + union that precedes the graph data.
pub const HEADER_SIZE: usize = 20;

/// Flag bits at header byte 1 (distinct from the H3+ `FunctionFlags` layout).
pub mod flags {
    /// A second (range) graph is present, blended by the second eval input.
    pub const RANGE: u8 = 1 << 0;
    /// The color graph type occupies the top four bits.
    pub const COLOR_GRAPH_TYPE_SHIFT: u8 = 4;
}

/// `g_constant_count_by_function_type` (0xDD58AC): f32s per graph.
const FLOATS_PER_GRAPH: [usize; 11] = [0, 1, 2, 4, 6, 20, 32, 12, 4, 3, 12];

/// Default control-point count by type (0xDD58A0).
const DEFAULT_POINT_COUNT: [usize; 11] = [0, 0, 0, 0, 2, 4, 16, 4, 16, 0, 4];

/// Periodic functions whose table wraps (the engine blends across the wrap
/// rather than through it): "slide" and "slide (variable period)".
const PERIODIC_SLIDE: u8 = 6;
const PERIODIC_SLIDE_VARIABLE_PERIOD: u8 = 7;

const EPSILON: f32 = 0.000_1;
const LUT_LEN: usize = 1024;
const TRANSITION_ROWS: usize = 7;
const PERIODIC_BASE: usize = TRANSITION_ROWS * LUT_LEN;
const PERIODIC_ROWS: usize = 11;

/// Every function type, in Guerilla's picker order (the MCC tool's string list
/// @0x101A4F0 names them in type order).
pub const FUNCTION_TYPES: [FunctionType; 11] = [
    FunctionType::Identity,
    FunctionType::Constant,
    FunctionType::Transition,
    FunctionType::Periodic,
    FunctionType::Linear,
    FunctionType::LinearKey,
    FunctionType::MultiLinearKey,
    FunctionType::Spline,
    FunctionType::MultiSpline,
    FunctionType::Exponent,
    FunctionType::Spline2,
];

/// Guerilla's name for a function type.
pub fn function_type_name(function_type: FunctionType) -> &'static str {
    match function_type {
        FunctionType::Identity => "identity",
        FunctionType::Constant => "constant",
        FunctionType::Transition => "transition",
        FunctionType::Periodic => "periodic",
        FunctionType::Linear => "linear",
        FunctionType::LinearKey => "linear key",
        FunctionType::MultiLinearKey => "multi linear key",
        FunctionType::Spline => "spline",
        FunctionType::MultiSpline => "multi spline",
        FunctionType::Exponent => "exponent",
        FunctionType::Spline2 => "spline2",
    }
}

/// Guerilla's name for a color graph type (the same string list). A single
/// color is "constant".
pub fn color_graph_type_name(color_graph_type: ColorGraphType) -> &'static str {
    match color_graph_type {
        ColorGraphType::Scalar => "scalar (intensity)",
        ColorGraphType::OneColor => "constant",
        ColorGraphType::TwoColor => "2-color",
        ColorGraphType::ThreeColor => "3-color",
        ColorGraphType::FourColor => "4-color",
    }
}

/// Periodic function names by index (MCC tool @0x1019208; the same twelve as
/// the H3+ table).
pub const PERIODIC_FUNCTION_NAMES: [&str; 12] = super::editor::PERIODIC_FUNCTIONS;

/// Transition function names by index, following the periodic names in the
/// same list. Halo 2 offers all eight.
pub const TRANSITION_FUNCTION_NAMES: [&str; 8] =
    ["linear", "early", "very early", "late", "very late", "cosine", "one", "zero"];

/// f32s per graph for a function type.
pub fn floats_per_graph(function_type: FunctionType) -> usize {
    FLOATS_PER_GRAPH[function_type as usize]
}

/// The byte-block size the engine keeps for a function type.
pub fn block_size(function_type: FunctionType) -> usize {
    HEADER_SIZE + 8 * floats_per_graph(function_type)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H2FunctionError {
    /// Fewer than the 4-byte header.
    TooShort { len: usize },
    /// Byte 0 is not a known [`FunctionType`].
    BadFunctionType { byte: u8 },
    /// The flags' color graph type is not one the engine accepts (it asserts
    /// below 5).
    BadColorGraphType { value: u8 },
    /// An edit the engine asserts against (wrong type, index out of range).
    InvalidEdit(&'static str),
}

impl std::fmt::Display for H2FunctionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "h2 function data too short: {len} bytes (need >= 4)"),
            Self::BadFunctionType { byte } => write!(f, "unknown h2 function type byte {byte}"),
            Self::BadColorGraphType { value } => write!(f, "unknown h2 color graph type {value}"),
            Self::InvalidEdit(m) => write!(f, "invalid h2 function edit: {m}"),
        }
    }
}

impl std::error::Error for H2FunctionError {}

/// A Halo 2 classic `c_function_definition` byte-block.
#[derive(Debug, Clone)]
pub struct H2Function {
    data: Vec<u8>,
}

impl H2Function {
    /// Take ownership of a byte-block, checking only the header's type byte.
    pub fn parse(data: &[u8]) -> Result<Self, H2FunctionError> {
        if data.len() < 4 {
            return Err(H2FunctionError::TooShort { len: data.len() });
        }
        FunctionType::from_byte(data[0]).ok_or(H2FunctionError::BadFunctionType { byte: data[0] })?;
        let color_graph_type = data[1] >> flags::COLOR_GRAPH_TYPE_SHIFT;
        ColorGraphType::from_byte(color_graph_type).ok_or(H2FunctionError::BadColorGraphType { value: color_graph_type })?;
        Ok(Self { data: data.to_vec() })
    }

    /// A new function of `function_type`, built the way the engine builds one:
    /// every setter first grows an empty block to a zeroed 20-byte header
    /// (an identity function), and `set_function_type` then sizes and seeds it.
    pub fn new(function_type: FunctionType) -> Self {
        let mut f = Self { data: vec![0; HEADER_SIZE] };
        f.set_function_type(function_type);
        f
    }

    /// A new constant scalar `value`. Setting both graphs' constant gives the
    /// shape most shipped constants have (16,958 of 37,995 in halo2_mcc):
    /// flags 0, clamp min == max == value, graph tail [1, 1].
    pub fn new_constant(value: f32) -> Self {
        let mut f = Self::new(FunctionType::Constant);
        f.put_f32(4, value);
        f.put_f32(8, value);
        f
    }

    /// A new constant color. Shipped color constants are two-color (Guerilla's
    /// default), and a constant only ever reads the first color, so only that
    /// slot is written (a shape 221 shipped constants have).
    pub fn new_constant_color(argb: u32) -> Self {
        let mut f = Self::new(FunctionType::Constant);
        f.set_color_graph_type(ColorGraphType::TwoColor);
        f.put_u32(4, argb);
        f
    }

    /// True when the block is the size the engine keeps for its type.
    pub fn has_engine_size(&self) -> bool {
        self.data.len() == block_size(self.function_type())
    }

    /// The byte-block as it stands (byte-identical to the input until edited).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    pub fn function_type(&self) -> FunctionType {
        FunctionType::from_byte(self.data[0]).expect("checked at parse and on every type change")
    }

    /// True when the RANGE flag is set: a second graph blended by the second
    /// evaluation input.
    pub fn is_ranged(&self) -> bool {
        self.data[1] & flags::RANGE != 0
    }

    /// Scalar, or how many colors (the flags' high nibble; checked at parse).
    pub fn color_graph_type(&self) -> ColorGraphType {
        ColorGraphType::from_byte(self.data[1] >> flags::COLOR_GRAPH_TYPE_SHIFT)
            .expect("checked at parse and on every change")
    }

    /// Periodic/transition index (or point count, for spline/multi types) of
    /// `graph` (0 or 1).
    pub fn function_index(&self, graph: usize) -> u8 {
        self.data[2 + graph.min(1)]
    }

    /// The f32 at byte `offset`; 0.0 past the end of a short block.
    fn f32_at(&self, offset: usize) -> f32 {
        f32::from_bits(self.u32_at(offset))
    }

    fn u32_at(&self, offset: usize) -> u32 {
        self.data
            .get(offset..offset + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0)
    }

    fn put_u32(&mut self, offset: usize, value: u32) {
        if self.data.len() < offset + 4 {
            self.data.resize(offset + 4, 0);
        }
        self.data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_f32(&mut self, offset: usize, value: f32) {
        self.put_u32(offset, value.to_bits());
    }

    /// Byte offset of float `k` of `graph`.
    fn graph_offset(&self, graph: usize, k: usize) -> usize {
        HEADER_SIZE + 4 * (graph * floats_per_graph(self.function_type()) + k)
    }

    /// Float `k` of `graph`'s parameters.
    pub fn graph_value(&self, graph: usize, k: usize) -> f32 {
        self.f32_at(self.graph_offset(graph, k))
    }

    /// `get_clamp_range_min` (0x80FC60): 0 for color graphs.
    pub fn clamp_range_min(&self) -> f32 {
        if self.color_graph_type() != ColorGraphType::Scalar { 0.0 } else { self.f32_at(4) }
    }

    /// `get_clamp_range_max` (0x80FC00): 1 for color graphs.
    pub fn clamp_range_max(&self) -> f32 {
        if self.color_graph_type() != ColorGraphType::Scalar { 1.0 } else { self.f32_at(8) }
    }

    /// ARGB of logical color `index` (0..color graph type) through the slot
    /// remap; `None` for a scalar function or an index past the color count.
    pub fn color(&self, index: usize) -> Option<u32> {
        let slot = color_slots(self.color_graph_type()).get(index)?;
        Some(self.u32_at(4 + 4 * slot))
    }

    /// Number of control points of `graph` (`get_control_point_count`, 0x80FDC0).
    pub fn control_point_count(&self, graph: usize) -> usize {
        match self.function_type() {
            FunctionType::Linear => 2,
            FunctionType::LinearKey | FunctionType::Spline | FunctionType::Spline2 => 4,
            FunctionType::MultiLinearKey | FunctionType::MultiSpline => self.function_index(graph) as usize,
            _ => 0,
        }
    }

    /// Whether control point `point` of `graph` lies inside the block. The
    /// multi types take their point count from the header, which can name more
    /// points than their block holds; those are never read or written.
    fn point_in_block(&self, graph: usize, point: usize) -> bool {
        graph < 2
            && point < self.control_point_count(graph)
            && self.graph_offset(graph, 2 * point + 1) + 4 <= self.data.len()
    }

    /// Control point `point` of `graph` (`get_control_point`, 0x80FD20).
    pub fn control_point(&self, graph: usize, point: usize) -> Option<(f32, f32)> {
        self.point_in_block(graph, point)
            .then(|| (self.graph_value(graph, 2 * point), self.graph_value(graph, 2 * point + 1)))
    }

    /// The x range control point `point` of `graph` may move within, or `None`
    /// when its x is fixed. Guerilla's `set_control_point_x` (2003 build
    /// @0x498910): only interior points move; a linear key's are held between
    /// their neighbours, a spline's anywhere between its end points. The 2003
    /// build predates spline2; shipped MCC spline2 points behave like spline's
    /// (interior x moved in 353 of 360 graphs, crossing in 37).
    pub fn control_point_x_range(&self, graph: usize, point: usize) -> Option<(f32, f32)> {
        if !self.point_in_block(graph, point) || point == 0 || point + 1 >= self.control_point_count(graph) {
            return None;
        }
        let x = |p: usize| self.graph_value(graph, 2 * p);
        let bounds = |lo: usize, hi: usize| {
            (self.point_in_block(graph, lo) && self.point_in_block(graph, hi)).then(|| (x(lo), x(hi)))
        };
        match self.function_type() {
            FunctionType::LinearKey | FunctionType::MultiLinearKey => bounds(point - 1, point + 1),
            FunctionType::Spline | FunctionType::Spline2 => bounds(0, 3),
            FunctionType::MultiSpline => {
                let start = 3 * (point / 3);
                bounds(start, start + 3)
            }
            _ => None,
        }
    }

    // -- Evaluation -----------------------------------------------------------

    /// The normalized curve at (`input`, `range`), clamped to `[0, 1]`: MCC
    /// `c_function_definition::evaluate` (0x80EC50). Multi-linear-key and
    /// multi-spline evaluate to 0 there, and so here.
    pub fn evaluate(&self, input: f32, range: f32) -> f32 {
        let x = input;
        let ranged = self.is_ranged();
        let f = |offset: usize| self.f32_at(offset);
        let blend = |first: f32, second: f32| first * (1.0 - range) + second * range;
        let v = match self.function_type() {
            FunctionType::Identity => x,
            FunctionType::Constant => {
                if ranged { range } else { 0.0 }
            }
            FunctionType::Transition => {
                let t = transition_evaluate(self.function_index(0), x);
                let first = (f(24) - f(20)) * t + f(20);
                if ranged {
                    let t = transition_evaluate(self.function_index(1), x);
                    blend(first, (f(32) - f(28)) * t + f(28))
                } else {
                    first
                }
            }
            FunctionType::Periodic => {
                let p = periodic_evaluate(self.function_index(0), f(20) * x + f(24));
                let first = (f(32) - f(28)) * p + f(28);
                if ranged {
                    let p = periodic_evaluate(self.function_index(1), f(36) * x + f(40));
                    blend(first, (f(48) - f(44)) * p + f(44))
                } else {
                    first
                }
            }
            FunctionType::Linear => {
                let first = f(36) * x + f(40);
                if ranged { blend(first, f(60) * x + f(64)) } else { first }
            }
            FunctionType::LinearKey => {
                let graph = |base: usize| {
                    let t = |i: usize| saturate((x - f(base + 36 + 4 * i)) * f(base + 52 + 4 * i));
                    ((f(base + 68) * t(0) + f(base + 64)) + f(base + 72) * t(1)) + f(base + 76) * t(2)
                };
                let first = graph(HEADER_SIZE);
                if ranged { blend(first, graph(HEADER_SIZE + 80)) } else { first }
            }
            FunctionType::Spline | FunctionType::Spline2 => {
                let warp = self.function_type() == FunctionType::Spline2;
                let t0 = if warp { self.spline2_warp(0, x, x) } else { x };
                let cubic = |t: f32, c: usize| {
                    let t2 = t * t;
                    ((f(c + 4) * t2 + t2 * t * f(c)) + f(c + 8) * t) + f(c + 12)
                };
                let first = cubic(t0, 52);
                if ranged {
                    let t1 = if warp { self.spline2_warp(1, x, t0) } else { x };
                    blend(first, cubic(t1, 100))
                } else {
                    first
                }
            }
            FunctionType::Exponent => {
                let graph = |min: f32, max: f32, exponent: f32| {
                    if exponent.abs() < EPSILON || (exponent < 0.0 && x.abs() < EPSILON) {
                        1.0
                    } else {
                        ((x as f64).powf(exponent as f64) as f32) * (max - min) + min
                    }
                };
                let first = graph(f(20), f(24), f(28));
                if ranged { blend(first, graph(f(32), f(36), f(40))) } else { first }
            }
            FunctionType::MultiLinearKey | FunctionType::MultiSpline => 0.0,
        };
        clamp_tail(v)
    }

    /// The x-warp spline2 applies before its cubic: `x` raised to roughly
    /// |p0 - p1| / |p2 - p3|, computed with the engine's bit-trick square root
    /// and one Newton step of reciprocal square root. `fallback` is what the
    /// engine keeps when the warp is degenerate (graph 0's result, for graph 1).
    fn spline2_warp(&self, graph: usize, x: f32, fallback: f32) -> f32 {
        let p = |i: usize| (self.graph_value(graph, 2 * i), self.graph_value(graph, 2 * i + 1));
        let (p0, p1, p2, p3) = (p(0), p(1), p(2), p(3));
        let far = (p2.0 - p3.0) * (p2.0 - p3.0) + (p2.1 - p3.1) * (p2.1 - p3.1);
        if !(far > EPSILON) {
            return fallback;
        }
        let near = (p0.0 - p1.0) * (p0.0 - p1.0) + (p0.1 - p1.1) * (p0.1 - p1.1);
        if EPSILON > near.abs() {
            return 1.0;
        }
        if EPSILON > (near - far).abs() {
            return fallback;
        }
        let sqrt_bits = |v: f32| ((v.to_bits() as i32) >> 1).wrapping_add(0x1FC0_0000);
        let sqrt_near = f32::from_bits(sqrt_bits(near) as u32);
        let sqrt_far = sqrt_bits(far);
        let reciprocal = if sqrt_far == 0 {
            0.0
        } else {
            let guess = f32::from_bits(0x7F00_0000i32.wrapping_sub(sqrt_far) as u32);
            (2.0 - guess * f32::from_bits(sqrt_far as u32)) * guess
        };
        clamp_tail((x as f64).powf((reciprocal * sqrt_near) as f64) as f32)
    }

    /// `map_to_output_range` (0x810500): scalar functions lerp the normalized
    /// value through the clamp range; color functions pass it through.
    pub fn map_to_output_range(&self, normalized: f32) -> f32 {
        if self.color_graph_type() != ColorGraphType::Scalar {
            return normalized;
        }
        let (min, max) = (self.clamp_range_min(), self.clamp_range_max());
        (max - min) * saturate(normalized) + min
    }

    /// The final scalar value: [`Self::evaluate`] mapped through the clamp
    /// range (MCC `evaluate_scalar`, 0x80F500).
    pub fn evaluate_scalar(&self, input: f32, range: f32) -> f32 {
        self.map_to_output_range(self.evaluate(input, range))
    }

    /// ARGB at an already-evaluated normalized position (`evaluate_color`,
    /// 0x80F670), including its fixed-point channel lerp.
    pub fn evaluate_color(&self, normalized: f32) -> u32 {
        let cgt = self.color_graph_type();
        let at_most_one_color = matches!(cgt, ColorGraphType::Scalar | ColorGraphType::OneColor);
        if self.function_type() == FunctionType::Constant && (!self.is_ranged() || at_most_one_color) {
            return self.u32_at(4);
        }
        let t = saturate(normalized);
        match cgt {
            ColorGraphType::Scalar => {
                let g = ((t * 255.0).round_ties_even() as i32 as u32) & 0xFF;
                g | (g << 8) | (g << 16) | 0xFF00_0000
            }
            ColorGraphType::OneColor => self.u32_at(4),
            ColorGraphType::TwoColor => lerp_argb(self.u32_at(4), self.u32_at(16), t),
            ColorGraphType::ThreeColor => {
                let scaled = t * 2.0;
                let mut n = scaled.floor() as i32;
                let frac = if n >= 2 {
                    n = 1;
                    1.0
                } else {
                    scaled - n as f32
                };
                let (a, b) = if n != 0 { (8, 16) } else { (4, 8) };
                lerp_argb(self.u32_at(a), self.u32_at(b), frac)
            }
            ColorGraphType::FourColor => {
                let scaled = t * 3.0;
                let mut n = scaled.floor() as i32;
                let frac = if n >= 3 {
                    n = 2;
                    1.0
                } else {
                    scaled - n as f32
                };
                let slot = 4 + 4 * n as usize;
                lerp_argb(self.u32_at(slot), self.u32_at(slot + 4), frac)
            }
        }
    }

    /// `is_constant` (0x810020): the output cannot vary with the inputs.
    pub fn is_constant(&self) -> bool {
        let f = |offset: usize| self.f32_at(offset);
        let flat = |a: f32, b: f32| (a - b).abs() < EPSILON;
        let ranged = self.is_ranged();
        let curve = match self.function_type() {
            FunctionType::Constant => !ranged,
            FunctionType::Transition => {
                let first = flat(f(24), f(20));
                if ranged { first && flat(f(32), f(28)) && flat(f(32), f(24)) } else { first }
            }
            FunctionType::Periodic => {
                let trivial = self.function_index(0) <= 1;
                let first = flat(f(32), f(28));
                if ranged {
                    (first && flat(f(48), f(44)) && flat(f(48), f(32))) || (trivial && self.function_index(1) <= 1)
                } else {
                    first || trivial
                }
            }
            FunctionType::Linear => {
                let first = flat(f(36), 0.0);
                if ranged { first && flat(f(60), 0.0) && flat(f(40), f(64)) } else { first }
            }
            FunctionType::LinearKey => {
                let first = flat(f(88), 0.0) && flat(f(92), 0.0) && flat(f(96), 0.0);
                if ranged {
                    first && flat(f(168), 0.0) && flat(f(172), 0.0) && flat(f(176), 0.0) && flat(f(84), f(164))
                } else {
                    first
                }
            }
            FunctionType::Exponent => {
                let first = flat(f(24), f(20));
                if ranged { first && flat(f(36), f(32)) && flat(f(36), f(24)) } else { first }
            }
            _ => false,
        };
        let u = |offset: usize| self.u32_at(offset);
        match self.color_graph_type() {
            ColorGraphType::Scalar => curve || flat(f(4), f(8)),
            ColorGraphType::OneColor => true,
            ColorGraphType::TwoColor => curve || u(4) == u(16),
            ColorGraphType::ThreeColor => curve || (u(4) == u(16) && u(4) == u(8)),
            ColorGraphType::FourColor => curve || (u(4) == u(16) && u(4) == u(8) && u(4) == u(12)),
        }
    }

    // -- Editing (the engine's setters, writing the block in place) -----------

    /// The engine grows any block shorter than the header before an edit.
    fn ensure_header(&mut self) {
        if self.data.len() < HEADER_SIZE {
            self.data.resize(HEADER_SIZE, 0);
        }
    }

    fn check_graph(graph: usize) -> Result<(), H2FunctionError> {
        if graph < 2 { Ok(()) } else { Err(H2FunctionError::InvalidEdit("graph index out of range")) }
    }

    /// `set_function_type` (0x811120): retype, resize to the type's block size
    /// and seed that type's defaults. A no-op when both already match.
    pub fn set_function_type(&mut self, function_type: FunctionType) {
        self.ensure_header();
        let size = block_size(function_type);
        if self.function_type() == function_type && self.data.len() == size {
            return;
        }
        self.data[0] = function_type as u8;
        self.data.resize(size, 0);
        self.initialize_graphs();
    }

    /// The type initializer (0x80EA20), then [`Self::postprocess`].
    fn initialize_graphs(&mut self) {
        let function_type = self.function_type();
        for graph in 0..2 {
            let at = |k: usize| HEADER_SIZE + 4 * (graph * floats_per_graph(function_type) + k);
            let defaults: &[f32] = match function_type {
                FunctionType::Constant => &[1.0],
                FunctionType::Transition => &[0.0, 1.0],
                FunctionType::Periodic => &[1.0, 0.0, 0.0, 1.0],
                FunctionType::Spline | FunctionType::Spline2 => {
                    &[0.0, 1.0, 0.333_333_34, 0.0, 0.666_666_7, 0.0, 1.0, 1.0]
                }
                FunctionType::Exponent => &[0.0, 1.0, 5.0],
                _ => &[],
            };
            for (k, &v) in defaults.iter().enumerate() {
                self.put_f32(at(k), v);
            }
            match function_type {
                FunctionType::Linear | FunctionType::LinearKey => {
                    let points = DEFAULT_POINT_COUNT[function_type as usize];
                    let step = 1.0 / (points as f32 - 1.0);
                    for i in 0..points {
                        self.put_f32(at(2 * i), i as f32 * step);
                        self.put_f32(at(2 * i + 1), 1.0);
                    }
                }
                FunctionType::Spline | FunctionType::Spline2 => self.data[2 + graph] = 4,
                FunctionType::Transition
                | FunctionType::Periodic
                | FunctionType::MultiLinearKey
                | FunctionType::MultiSpline
                | FunctionType::Exponent => self.data[2 + graph] = 0,
                _ => {}
            }
        }
        self.postprocess();
    }

    /// The postprocess (0x810650): rebuild each graph's derived data from its
    /// control points: linear slope/offset, linear-key knot tables, and the
    /// spline/spline2 cubic (Hermite) coefficients.
    pub fn postprocess(&mut self) {
        let function_type = self.function_type();
        let n = floats_per_graph(function_type);
        for graph in 0..2 {
            let at = |k: usize| HEADER_SIZE + 4 * (graph * n + k);
            let f = |this: &Self, k: usize| this.f32_at(at(k));
            match function_type {
                FunctionType::Linear => {
                    let (y0, y1) = (f(self, 1), f(self, 3));
                    self.put_f32(at(5), y0);
                    self.put_f32(at(4), y1 - y0);
                }
                FunctionType::LinearKey => {
                    let (x1, x2) = (f(self, 2), f(self, 4));
                    let (y0, y1, y2, y3) = (f(self, 1), f(self, 3), f(self, 5), f(self, 7));
                    self.put_f32(at(8), -1.0);
                    self.put_f32(at(9), 0.0);
                    self.put_f32(at(10), x1);
                    self.put_f32(at(11), x2);
                    self.put_f32(at(12), 1.0);
                    self.put_f32(at(13), if x1 <= 0.0 { 0.0 } else { 1.0 / x1 });
                    self.put_f32(at(14), if x2 <= x1 { 0.0 } else { 1.0 / (x2 - x1) });
                    self.put_f32(at(15), if x2 >= 1.0 { 0.0 } else { 1.0 / (1.0 - x2) });
                    self.put_f32(at(16), y0);
                    self.put_f32(at(17), y1 - y0);
                    self.put_f32(at(18), y2 - y1);
                    self.put_f32(at(19), y3 - y2);
                }
                FunctionType::Spline | FunctionType::Spline2 => {
                    let (x1, y0, y1) = (f(self, 2), f(self, 1), f(self, 3));
                    let (x2, y2, y3) = (f(self, 4), f(self, 5), f(self, 7));
                    let start = if x1 <= EPSILON { 0.0 } else { (y1 - y0) / x1 };
                    let span = 1.0 - x2;
                    let end = if span <= EPSILON { 0.0 } else { (y3 - y2) / span };
                    self.put_f32(at(10), start);
                    self.put_f32(at(11), y0);
                    self.put_f32(at(8), (((y0 + y0) - (y3 + y3)) + start) + end);
                    self.put_f32(at(9), (((y3 * 3.0) - (y0 * 3.0)) - (start + start)) - end);
                }
                _ => {}
            }
        }
    }

    /// `set_ranged` (0x811200).
    pub fn set_ranged(&mut self, ranged: bool) {
        self.ensure_header();
        if ranged {
            self.data[1] |= flags::RANGE;
        } else {
            self.data[1] &= !flags::RANGE;
        }
    }

    /// `set_color_graph_type` (0x810CD0).
    pub fn set_color_graph_type(&mut self, color_graph_type: ColorGraphType) {
        self.ensure_header();
        self.data[1] = (self.data[1] & 0x0F) | ((color_graph_type as u8) << flags::COLOR_GRAPH_TYPE_SHIFT);
    }

    /// `set_color` (0x810C00): logical color `index` → its physical slot.
    pub fn set_color(&mut self, index: usize, argb: u32) -> Result<(), H2FunctionError> {
        self.ensure_header();
        let slot = *color_slots(self.color_graph_type())
            .get(index)
            .ok_or(H2FunctionError::InvalidEdit("color index out of range"))?;
        self.put_u32(4 + 4 * slot, argb);
        Ok(())
    }

    /// `set_clamp_range_min` / `_max` (0x810BC0 / 0x810B80): scalar only; a
    /// color function's union holds colors, so the engine skips these.
    pub fn set_clamp_range(&mut self, min: f32, max: f32) -> Result<(), H2FunctionError> {
        self.ensure_header();
        if self.color_graph_type() != ColorGraphType::Scalar {
            return Err(H2FunctionError::InvalidEdit("a color function has no clamp range"));
        }
        self.put_f32(4, min);
        self.put_f32(8, max);
        Ok(())
    }

    /// `set_constant` (0x810D90): a constant's per-graph value, stored in the
    /// clamp-range union (graph 0 at byte 4, graph 1 at byte 8).
    pub fn set_constant(&mut self, graph: usize, value: f32) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        self.ensure_header();
        if self.function_type() != FunctionType::Constant {
            return Err(H2FunctionError::InvalidEdit("set_constant needs a constant function"));
        }
        self.put_f32(4 + 4 * graph, value);
        Ok(())
    }

    /// `set_function_index` (0x810F80): transition 0..7, periodic 0..11.
    pub fn set_function_index(&mut self, graph: usize, index: u8) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        self.ensure_header();
        let limit = match self.function_type() {
            FunctionType::Transition => 8,
            FunctionType::Periodic => 12,
            _ => return Err(H2FunctionError::InvalidEdit("function index needs a transition or periodic function")),
        };
        if index >= limit {
            return Err(H2FunctionError::InvalidEdit("function index out of range"));
        }
        self.data[2 + graph] = index;
        Ok(())
    }

    /// Byte offset of `graph`'s amplitude minimum (the maximum follows at +4),
    /// for the types `set_amplitude_range_min/max` (0x810A50 / 0x810910) accept.
    fn amplitude_offset(&self, graph: usize) -> Result<usize, H2FunctionError> {
        Self::check_graph(graph)?;
        match self.function_type() {
            FunctionType::Transition => Ok(20 + 8 * graph),
            FunctionType::Periodic => Ok(28 + 16 * graph),
            FunctionType::Exponent => Ok(20 + 12 * graph),
            _ => Err(H2FunctionError::InvalidEdit("amplitude needs a transition, periodic or exponent function")),
        }
    }

    /// `graph`'s `(amplitude min, amplitude max)`.
    pub fn amplitude_range(&self, graph: usize) -> Option<(f32, f32)> {
        let at = self.amplitude_offset(graph).ok()?;
        Some((self.f32_at(at), self.f32_at(at + 4)))
    }

    pub fn set_amplitude_range(&mut self, graph: usize, min: f32, max: f32) -> Result<(), H2FunctionError> {
        self.ensure_header();
        let at = self.amplitude_offset(graph)?;
        self.put_f32(at, min);
        self.put_f32(at + 4, max);
        Ok(())
    }

    /// A periodic graph's `(frequency, phase)`, at the offsets `evaluate` reads.
    pub fn periodic_frequency_phase(&self, graph: usize) -> Option<(f32, f32)> {
        (graph < 2 && self.function_type() == FunctionType::Periodic)
            .then(|| (self.f32_at(20 + 16 * graph), self.f32_at(24 + 16 * graph)))
    }

    /// Set a periodic graph's frequency and phase. The engine has no setter for
    /// these (Guerilla writes the fields); the offsets are the ones `evaluate`
    /// reads.
    pub fn set_periodic_frequency_phase(&mut self, graph: usize, frequency: f32, phase: f32) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        if self.function_type() != FunctionType::Periodic {
            return Err(H2FunctionError::InvalidEdit("frequency/phase needs a periodic function"));
        }
        self.put_f32(20 + 16 * graph, frequency);
        self.put_f32(24 + 16 * graph, phase);
        Ok(())
    }

    /// An exponent graph's exponent, at the offset `evaluate` reads.
    pub fn exponent(&self, graph: usize) -> Option<f32> {
        (graph < 2 && self.function_type() == FunctionType::Exponent).then(|| self.f32_at(28 + 12 * graph))
    }

    /// Set an exponent graph's exponent. Like frequency/phase this has no
    /// engine setter; the offset is the one `evaluate` reads.
    pub fn set_exponent(&mut self, graph: usize, exponent: f32) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        if self.function_type() != FunctionType::Exponent {
            return Err(H2FunctionError::InvalidEdit("exponent needs an exponent function"));
        }
        self.put_f32(28 + 12 * graph, exponent);
        Ok(())
    }

    /// `set_control_point_y` (0x810E90), then [`Self::postprocess`] so the
    /// derived data stays consistent with the points.
    pub fn set_control_point_y(&mut self, graph: usize, point: usize, y: f32) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        self.ensure_header();
        if !self.point_in_block(graph, point) {
            return Err(H2FunctionError::InvalidEdit("control point index out of range"));
        }
        let at = self.graph_offset(graph, 2 * point + 1);
        self.put_f32(at, y);
        self.postprocess();
        Ok(())
    }

    /// `set_control_point_x`, then [`Self::postprocess`]. The x is held to
    /// [`Self::control_point_x_range`] with Guerilla's comparisons (below the
    /// range takes the low bound, above takes the high); a point whose x is
    /// fixed refuses.
    pub fn set_control_point_x(&mut self, graph: usize, point: usize, x: f32) -> Result<(), H2FunctionError> {
        Self::check_graph(graph)?;
        let (lo, hi) = self
            .control_point_x_range(graph, point)
            .ok_or(H2FunctionError::InvalidEdit("this control point's x is fixed"))?;
        let x = if x >= lo {
            if x <= hi { x } else { hi }
        } else {
            lo
        };
        let at = self.graph_offset(graph, 2 * point);
        self.put_f32(at, x);
        self.postprocess();
        Ok(())
    }
}

/// `v >= 0 ? min(1, v) : 0` — NaN goes to 0.
fn saturate(v: f32) -> f32 {
    if v >= 0.0 { v.min(1.0) } else { 0.0 }
}

/// The SSE tail `0 > v ? 0 : minss(1, v)` — NaN passes through.
fn clamp_tail(v: f32) -> f32 {
    if 0.0 > v {
        0.0
    } else if 1.0 < v {
        1.0
    } else {
        v
    }
}

#[inline]
fn lut(base: usize, k: usize) -> f32 {
    FUNCTION_TABLES[base + k] as f32 * (1.0 / 255.0)
}

/// MCC `transition_function_evaluate` (0x80CC70). The table rows match the
/// H3+ ones byte for byte; the index arithmetic differs (it truncates).
pub fn transition_evaluate(index: u8, value: f32) -> f32 {
    let x = saturate(value);
    if index == 0 {
        return x;
    }
    let base = (index.min(TRANSITION_ROWS as u8) as usize - 1) * LUT_LEN;
    let scaled = x * 1023.0;
    let frac = scaled % 1.0;
    let i = ((scaled as f64 - 0.1) as f32) as i32 as usize;
    let mut v = lut(base, i);
    if i != LUT_LEN - 1 {
        v = v * (1.0 - frac) + lut(base, i + 1) * frac;
    }
    saturate(v)
}

/// MCC `periodic_function_evaluate` (0x80CAE0).
pub fn periodic_evaluate(index: u8, value: f32) -> f32 {
    if index == 0 {
        return 1.0;
    }
    let base = PERIODIC_BASE + (index.min(PERIODIC_ROWS as u8) as usize - 1) * LUT_LEN;
    let scaled = (value * 36.571_43) % 1024.0;
    let frac = scaled % 1.0;
    let i = ((scaled - frac) as i32 & 0x3FF) as usize;
    let a = lut(base, i);
    let mut b = lut(base, (i + 1) & 0x3FF);
    if !matches!(index, PERIODIC_SLIDE | PERIODIC_SLIDE_VARIABLE_PERIOD) {
        return (1.0 - frac) * a + b * frac;
    }
    if a > 0.75 && b < 0.25 {
        b += 1.0;
    }
    let v = (1.0 - frac) * a + b * frac;
    if v > 1.0 { v - 1.0 } else { v }
}

/// The engine's MMX channel lerp: each byte moves by `round((b - a) * t)` in
/// 1/16384 fixed point, saturated to a byte.
fn lerp_argb(a: u32, b: u32, t: f32) -> u32 {
    let weight = (t * 16384.0).round_ties_even() as i32 as i16 as i32;
    let mut out = 0u32;
    for shift in [0, 8, 16, 24] {
        let from = ((a >> shift) & 0xFF) as i16;
        let to = ((b >> shift) & 0xFF) as i16;
        let delta = to.wrapping_sub(from).wrapping_shl(3) as i32;
        let high = ((delta * weight) >> 16) as i16;
        let step = high.wrapping_add(1) >> 1;
        out |= (from.wrapping_add(step).clamp(0, 255) as u32) << shift;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block of `function_type`'s engine size: the header, the union (clamp
    /// min/max or colors), then graph floats from byte 20.
    fn block(function_type: FunctionType, flags: u8, fns: [u8; 2], union: [u32; 4], graph: &[f32]) -> Vec<u8> {
        let mut b = vec![function_type as u8, flags, fns[0], fns[1]];
        for u in union {
            b.extend_from_slice(&u.to_le_bytes());
        }
        for v in graph {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.resize(block_size(function_type).max(b.len()), 0);
        b
    }

    fn range(min: f32, max: f32) -> [u32; 4] {
        [min.to_bits(), max.to_bits(), 0, 0]
    }

    #[test]
    fn real_constant_particle_functions() {
        // iac_engine_fire.effect "emission rate": constant, 28 bytes. The
        // value is the clamp range (bytes 4/8); byte 20 is the initializer's 1.0.
        let bytes = block(FunctionType::Constant, 0, [4, 4], range(1.0, 1.0), &[1.0, 1.0]);
        assert_eq!(bytes.len(), 28);
        let f = H2Function::parse(&bytes).unwrap();
        assert_eq!(f.evaluate(0.3, 0.0), 0.0, "a constant is normalized 0 unranged");
        assert_eq!(f.evaluate_scalar(0.3, 0.0), 1.0);
        assert!(f.is_constant());
        assert_eq!(f.to_bytes(), bytes);

        // "particle velocity": 0.01 to 0.0 (min above max), unranged.
        let f = H2Function::parse(&block(FunctionType::Constant, 0, [0, 0], range(0.01, 0.0), &[1.0, 1.0])).unwrap();
        assert_eq!(f.evaluate_scalar(0.7, 0.0), 0.01);
    }

    #[test]
    fn color_graph_type_is_the_high_nibble() {
        // "particle tint": flags 0x20 = two-color; colors in slots 0 and 3.
        let bytes = block(FunctionType::Constant, 0x20, [4, 4], [0xFFFF_FFFF, 1.0f32.to_bits(), 0, 0xFFFF_FFFF], &[1.0, 1.0]);
        let f = H2Function::parse(&bytes).unwrap();
        assert_eq!(f.color_graph_type(), ColorGraphType::TwoColor);
        assert_eq!(f.color(0), Some(0xFFFF_FFFF));
        assert_eq!(f.color(1), Some(0xFFFF_FFFF), "logical 1 of two-color is physical slot 3");
        assert_eq!(f.color(2), None, "two colors");
        assert_eq!(f.clamp_range_min(), 0.0, "color functions have no clamp range");
        assert_eq!(f.evaluate_color(0.5), 0xFFFF_FFFF);
    }

    #[test]
    fn periodic_reads_its_graph_after_the_union() {
        // freq 2, phase 0.25, amp 0.1..0.9 at byte 20; index 0 = "one" (P == 1).
        let f = H2Function::parse(&block(FunctionType::Periodic, 0, [0, 0], range(0.0, 1.0), &[2.0, 0.25, 0.1, 0.9])).unwrap();
        assert!((f.evaluate(0.5, 0.0) - 0.9).abs() < 1e-6);
        assert_eq!(f.periodic_frequency_phase(0), Some((2.0, 0.25)));
        assert_eq!(f.amplitude_range(0), Some((0.1, 0.9)));
    }

    #[test]
    fn linear_uses_its_postprocessed_slope_and_offset() {
        let mut f = H2Function::parse(&block(FunctionType::Linear, 0, [0, 0], range(0.0, 10.0), &[])).unwrap();
        f.set_control_point_y(0, 0, 0.2).unwrap();
        f.set_control_point_y(0, 1, 0.7).unwrap();
        assert!((f.evaluate(0.4, 0.0) - (0.2 + 0.4 * 0.5)).abs() < 1e-6);
        assert!((f.evaluate_scalar(0.4, 0.0) - 4.0).abs() < 1e-5);
    }

    #[test]
    fn set_function_type_seeds_engine_defaults() {
        let mut f = H2Function::parse(&block(FunctionType::Identity, 0, [0, 0], range(0.0, 1.0), &[])).unwrap();
        f.set_function_type(FunctionType::Spline2);
        assert_eq!(f.to_bytes().len(), block_size(FunctionType::Spline2));
        assert_eq!(f.function_index(0), 4);
        assert_eq!(f.control_point(0, 1), Some((0.333_333_34, 0.0)));
        // The seeded curve runs (0,1) → (1,1) through a dip.
        assert!((f.evaluate(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((f.evaluate(1.0, 0.0) - 1.0).abs() < 1e-5);
        assert!(f.evaluate(0.5, 0.0) < 0.5);

        f.set_function_type(FunctionType::Exponent);
        assert_eq!(f.exponent(0), Some(5.0));
        assert!((f.evaluate(0.5, 0.0) - 0.5f32.powi(5)).abs() < 1e-6);
    }

    #[test]
    fn two_color_lerp_is_the_engine_fixed_point() {
        let f = H2Function::parse(&block(FunctionType::Linear, 0x20, [0, 0], [0xFF00_0000, 0, 0, 0xFFFF_FFFF], &[])).unwrap();
        assert_eq!(f.evaluate_color(0.0), 0xFF00_0000);
        assert_eq!(f.evaluate_color(1.0), 0xFFFF_FFFF);
        // Halfway, each channel moves by (255*8 * 8192 >> 16) + 1 >> 1 = 128.
        assert_eq!(f.evaluate_color(0.5), 0xFF80_8080);
    }

    #[test]
    fn edits_the_engine_refuses() {
        let mut f = H2Function::parse(&block(FunctionType::Linear, 0, [0, 0], range(0.0, 1.0), &[])).unwrap();
        assert!(f.set_constant(0, 1.0).is_err());
        assert!(f.set_function_index(0, 1).is_err());
        assert!(matches!(
            H2Function::parse(&block(FunctionType::Constant, 5 << 4, [0, 0], range(0.0, 1.0), &[])),
            Err(H2FunctionError::BadColorGraphType { value: 5 })
        ));
        assert!(f.set_control_point_y(0, 2, 0.5).is_err(), "linear has two points");
        f.set_function_type(FunctionType::Transition);
        assert!(f.set_function_index(0, 8).is_err());
        assert!(f.set_function_index(1, 7).is_ok());
        f.set_color_graph_type(ColorGraphType::TwoColor);
        assert!(f.set_clamp_range(0.0, 1.0).is_err(), "a color function's union is colors");
    }

    #[test]
    fn new_constants_match_shipped_shapes() {
        // The dominant shipped scalar constant, byte for byte.
        let expected = block(FunctionType::Constant, 0, [0, 0], range(0.25, 0.25), &[1.0, 1.0]);
        let f = H2Function::new_constant(0.25);
        assert_eq!(f.to_bytes(), expected);
        assert_eq!(f.evaluate_scalar(0.7, 0.3), 0.25);
        assert!(f.has_engine_size());

        let f = H2Function::new_constant_color(0xFF11_2233);
        let expected = block(FunctionType::Constant, 0x20, [0, 0], [0xFF11_2233, 0, 0, 0], &[1.0, 1.0]);
        assert_eq!(f.to_bytes(), expected);
        assert_eq!(f.evaluate_color(f.evaluate(0.5, 0.0)), 0xFF11_2233);

        assert_eq!(H2Function::new(FunctionType::Identity).to_bytes(), vec![0; HEADER_SIZE]);
    }

    #[test]
    fn control_point_x_follows_guerillas_rules() {
        let mut f = H2Function::new(FunctionType::Linear);
        assert_eq!(f.control_point_x_range(0, 0), None, "linear x is fixed");
        assert!(f.set_control_point_x(0, 1, 0.5).is_err());

        // Linear key: interior points held between their neighbours.
        let mut f = H2Function::new(FunctionType::LinearKey);
        let x = |f: &H2Function, p: usize| f.control_point(0, p).unwrap().0;
        assert_eq!(f.control_point_x_range(0, 0), None, "end points are fixed");
        f.set_control_point_x(0, 1, 0.9).unwrap();
        assert_eq!(x(&f, 1), x(&f, 2), "cannot pass the next point");
        f.set_control_point_x(0, 2, 0.1).unwrap();
        assert!(x(&f, 2) >= x(&f, 1), "cannot pass the previous point");

        // Spline: interior points may cross, held to the end points.
        let mut f = H2Function::new(FunctionType::Spline2);
        f.set_control_point_x(0, 1, 0.9).unwrap();
        assert_eq!(x(&f, 1), 0.9, "may pass point 2");
        f.set_control_point_x(0, 2, -1.0).unwrap();
        assert_eq!(x(&f, 2), 0.0, "held to p0.x");
        // The cubic was rebuilt from the moved points.
        let before = f.clone();
        f.set_control_point_x(0, 1, 0.5).unwrap();
        assert_ne!(f.to_bytes()[52..68], before.to_bytes()[52..68]);

        // Multi types name their point count in the header; points past the
        // block are never touched, so the block never grows.
        let mut f = H2Function::new(FunctionType::MultiSpline);
        f.data[2] = 16;
        let size = f.to_bytes().len();
        assert!(f.control_point(0, 15).is_none());
        assert!(f.set_control_point_y(0, 15, 0.5).is_err());
        assert_eq!(f.to_bytes().len(), size);
    }

    #[test]
    fn rejects_short_and_bad_type() {
        assert!(matches!(H2Function::parse(&[1, 0, 0]), Err(H2FunctionError::TooShort { len: 3 })));
        assert!(matches!(H2Function::parse(&[99, 0, 0, 0]), Err(H2FunctionError::BadFunctionType { byte: 99 })));
    }
}
