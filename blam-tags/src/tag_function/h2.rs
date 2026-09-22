//! Halo 2 classic function — the byte-block `c_function_definition` encoding
//! (`mapping_function` v1, Family B).
//!
//! Unlike the H3+ blob ([`super::TagFunction`], a 32-byte header + typed
//! compacts), a Halo 2 function on disk is a **4-byte header** followed by a
//! flat `f32` array:
//!
//! ```text
//!   byte 0  function_type   (shared FunctionType enum, 0..10)
//!   byte 1  flags           (bit0 = RANGE; bits 5-7 = color count)
//!   byte 2  function 1       (periodic/transition selector, graph 0)
//!   byte 3  function 2       (periodic/transition selector, graph 1 / range)
//!   +4..    f32 values[]     (per-type parameters; values[i] at byte 4+4*i)
//! ```
//!
//! The per-type value layout and evaluation are ported verbatim from the engine
//! `c_function_definition::evaluate` (halo2symbols.xbe.i64 @ 0x30a940); the
//! periodic/transition selectors index the SAME 12/4-entry tables the H3+ path
//! uses ([`super::periodic_function_evaluate`] /
//! [`super::transition_function_evaluate`]).
//!
//! Byte-identical round-trip: the exact parsed bytes are retained in [`raw`]
//! and returned verbatim by [`H2Function::to_bytes`] until an edit sets `dirty`
//! (mirrors [`super::TagFunction`]).

use super::{periodic_function_evaluate, transition_function_evaluate, FunctionType};

/// Flag bits at header byte 1 (distinct from the H3+ `FunctionFlags` layout).
pub mod flags {
    /// A second (range) graph is present, blended by the second eval input.
    pub const RANGE: u8 = 1 << 0;
    /// Color count occupies the top three bits.
    pub const COLOR_COUNT_SHIFT: u8 = 5;
    pub const COLOR_COUNT_MASK: u8 = 0b111;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum H2FunctionError {
    /// Fewer than the 4-byte header.
    TooShort { len: usize },
    /// Byte 0 is not a known [`FunctionType`].
    BadFunctionType { byte: u8 },
}

impl std::fmt::Display for H2FunctionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { len } => write!(f, "h2 function data too short: {len} bytes (need >= 4)"),
            Self::BadFunctionType { byte } => write!(f, "unknown h2 function type byte {byte}"),
        }
    }
}

impl std::error::Error for H2FunctionError {}

/// A Halo 2 classic `c_function_definition`, decoded from its byte-block form.
#[derive(Debug, Clone)]
pub struct H2Function {
    /// Exact on-disk bytes; returned verbatim by [`Self::to_bytes`] until `dirty`.
    raw: Vec<u8>,
    dirty: bool,
    function_type: FunctionType,
    flags: u8,
    function_1: u8,
    function_2: u8,
}

impl H2Function {
    /// Decode the 4-byte header and retain the raw bytes. The `f32` value array
    /// is read on demand from `raw` (see [`Self::value`]).
    pub fn parse(data: &[u8]) -> Result<Self, H2FunctionError> {
        if data.len() < 4 {
            return Err(H2FunctionError::TooShort { len: data.len() });
        }
        let function_type = FunctionType::from_byte(data[0])
            .ok_or(H2FunctionError::BadFunctionType { byte: data[0] })?;
        Ok(Self {
            raw: data.to_vec(),
            dirty: false,
            function_type,
            flags: data[1],
            function_1: data[2],
            function_2: data[3],
        })
    }

    /// Serialize to the on-disk byte-block. Byte-identical to the parsed input
    /// until an edit dirties the function.
    pub fn to_bytes(&self) -> Vec<u8> {
        // No mutators re-serialize yet; unedited functions are byte-identical.
        debug_assert!(!self.dirty, "h2 function re-serialization not yet implemented");
        self.raw.clone()
    }

    pub fn function_type(&self) -> FunctionType {
        self.function_type
    }

    /// True when the RANGE flag is set — a second graph blended by the second
    /// evaluation input.
    pub fn is_ranged(&self) -> bool {
        self.flags & flags::RANGE != 0
    }

    /// Number of color graphs (0 = scalar), from flag bits 5-7.
    pub fn color_count(&self) -> usize {
        ((self.flags >> flags::COLOR_COUNT_SHIFT) & flags::COLOR_COUNT_MASK) as usize
    }

    /// Periodic/transition selector for graph 0 (indexes the shared tables).
    pub fn function_1(&self) -> u8 {
        self.function_1
    }

    /// Periodic/transition selector for graph 1 (range graph).
    pub fn function_2(&self) -> u8 {
        self.function_2
    }

    /// `values[i]` — the i-th `f32` after the 4-byte header. Out-of-range reads
    /// return `0.0` (matches the engine's tolerance of short data).
    pub fn value(&self, i: usize) -> f32 {
        let off = 4 + i * 4;
        self.raw
            .get(off..off + 4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .unwrap_or(0.0)
    }

    /// Evaluate at `input` (primary) and `range` (blend for the second graph).
    ///
    /// A direct port of `c_function_definition::evaluate` (0x30a940). The result
    /// is clamped to `[0, 1]` exactly as the engine does. Curve types (LinearKey/
    /// Spline/Spline2) are not yet evaluated here — they return `input`
    /// (identity) pending the curve-editor port; every other type is exact.
    pub fn evaluate(&self, input: f32, range: f32) -> f32 {
        let v = |i: usize| self.value(i);
        let ranged = self.is_ranged();
        let mut out: f32;
        match self.function_type {
            FunctionType::Identity => out = input,
            FunctionType::Constant => {
                out = v(0);
                if ranged {
                    out = (1.0 - range) * out + range * v(1);
                }
            }
            FunctionType::Transition => {
                let t = transition_function_evaluate(self.function_1, input);
                out = (v(1) - v(0)) * t + v(0);
                if ranged {
                    let t2 = transition_function_evaluate(self.function_2, input);
                    let g2 = (v(3) - v(2)) * t2 + v(2);
                    out = (1.0 - range) * out + g2 * range;
                }
            }
            FunctionType::Periodic => {
                let x = input * v(0) + v(1);
                let p = periodic_function_evaluate(self.function_1, x);
                out = (v(3) - v(2)) * p + v(2);
                if ranged {
                    // graph 1 parameters begin at values[4].
                    let x2 = input * v(4) + v(5);
                    let p2 = periodic_function_evaluate(self.function_2, x2);
                    let g2 = (v(7) - v(6)) * p2 + v(6);
                    out = (1.0 - range) * out + g2 * range;
                }
            }
            FunctionType::Linear => {
                out = input * v(4) + v(5);
                if ranged {
                    out = (1.0 - range) * out + (input * v(10) + v(11)) * range;
                }
            }
            // Curve family — byte-identity holds, but shape eval is pending the
            // curve-editor port (Phase 1b). Approximate as identity for preview.
            FunctionType::LinearKey
            | FunctionType::MultiLinearKey
            | FunctionType::Spline
            | FunctionType::MultiSpline
            | FunctionType::Exponent
            | FunctionType::Spline2 => out = input,
        }
        out.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a byte-block: 4-byte header + LE f32 values.
    fn blob(ty: u8, flags: u8, fn1: u8, fn2: u8, values: &[f32]) -> Vec<u8> {
        let mut b = vec![ty, flags, fn1, fn2];
        for v in values {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b
    }

    #[test]
    fn parses_real_constant_particle_functions() {
        // iac_engine_fire.effect particle "emission rate": constant, fn1=fn2=4,
        // 28 bytes = 4-byte header + 6 f32s (subagent corpus dump).
        let emission = blob(1, 0, 4, 4, &[1.0, 1.0, 0.0, 0.0, 1.0, 1.0]);
        assert_eq!(emission.len(), 28);
        let f = H2Function::parse(&emission).unwrap();
        assert_eq!(f.function_type(), FunctionType::Constant);
        assert!(!f.is_ranged());
        assert_eq!(f.function_1(), 4);
        assert_eq!(f.value(0), 1.0);
        // Constant, not ranged: output is values[0] regardless of input.
        assert_eq!(f.evaluate(0.3, 0.0), 1.0);
        // Byte-identical round-trip.
        assert_eq!(f.to_bytes(), emission);

        // "particle velocity": constant 0.01.
        let velocity = blob(1, 0, 0, 0, &[0.01, 0.0, 0.0, 0.0, 1.0, 1.0]);
        let f = H2Function::parse(&velocity).unwrap();
        assert_eq!(f.function_type(), FunctionType::Constant);
        assert!((f.evaluate(0.7, 0.0) - 0.01).abs() < 1e-6);
    }

    #[test]
    fn decodes_color_and_curve_headers() {
        // "particle tint": constant color function, flags 0x20 (color bit set).
        let tint = blob(1, 0x20, 4, 4, &[f32::from_bits(0xFFFFFFFF), 1.0, 0.0, f32::from_bits(0xFFFFFFFF), 1.0, 1.0]);
        let f = H2Function::parse(&tint).unwrap();
        assert_eq!(f.function_type(), FunctionType::Constant);
        assert_ne!(f.color_count(), 0, "color bit should be seen");
        assert_eq!(f.to_bytes(), tint);

        // "particle alpha": a curve (Spline2 = type 10), 116 bytes.
        let mut alpha = blob(10, 0, 4, 4, &[0.0, 1.0]);
        alpha.resize(116, 0);
        let f = H2Function::parse(&alpha).unwrap();
        assert_eq!(f.function_type(), FunctionType::Spline2);
        assert_eq!(f.to_bytes(), alpha); // byte-identity holds even for unported eval
    }

    #[test]
    fn periodic_eval_matches_the_engine_formula() {
        // Periodic (type 3): x = input*freq + phase; out = (ampMax-ampMin)*P(fn1,x) + ampMin.
        // values = [freq, phase, ampMin, ampMax]. fn1 = 0 = "one" (P(x) == 1.0),
        // so out == ampMax regardless of x.
        let f = H2Function::parse(&blob(3, 0, 0, 0, &[2.0, 0.25, 0.1, 0.9])).unwrap();
        assert_eq!(f.function_type(), FunctionType::Periodic);
        let out = f.evaluate(0.5, 0.0);
        assert!((out - 0.9).abs() < 1e-3, "P('one')==1 -> ampMax=0.9, got {out}");
    }

    #[test]
    fn linear_eval_uses_slope_offset_at_values_4_5() {
        // Linear (type 4): out = input*values[4] + values[5].
        let f = H2Function::parse(&blob(4, 0, 0, 0, &[0.0, 0.0, 0.0, 0.0, 0.5, 0.2])).unwrap();
        assert_eq!(f.function_type(), FunctionType::Linear);
        assert!((f.evaluate(0.4, 0.0) - (0.4 * 0.5 + 0.2)).abs() < 1e-6);
    }

    #[test]
    fn rejects_short_and_bad_type() {
        assert!(matches!(H2Function::parse(&[1, 0, 0]), Err(H2FunctionError::TooShort { len: 3 })));
        assert!(matches!(H2Function::parse(&[99, 0, 0, 0]), Err(H2FunctionError::BadFunctionType { byte: 99 })));
    }
}
