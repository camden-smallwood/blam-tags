//! `chocolate_mountain_new` (`chmt`) tag walker — per-object-type HDR
//! "minimum luminance" floor (despite the spelling, the engine
//! source file is `chocalate_mountain_definitions.cpp`).
//!
//! Pointed at by `scenario.chocolate_mountain_settings`. The engine
//! looks up `chmt[object_type].min_luminance` to compute a
//! `mountain_scale` multiplier that boosts an object's lighting when
//! its render_exposure dips below the floor — keeps characters from
//! going fully black under shadow probes.
//!
//! ## Engine usage (IDA-verified)
//!
//! `c_chocalate_moutain::apply_chocalate_mountain_lighting @ 0x1806f9ee0`:
//! ```text
//! *mountain_scale = -1.0;                                  // default
//! if (debug_use_chocolate_mountain) {
//!     // Index by object_type into per_object_min_luminance[].
//!     float min_lum = chmt[object_type].min_luminance;
//!     if (min_lum > 0.0) {
//!         *mountain_scale = sqrt(min_lum / current_render_exposure);
//!     }
//! }
//! ```
//!
//! Note that the engine **does not modify** the SH coefficients —
//! it only writes `mountain_scale`. Caller (`object_update_cached_render_lighting
//! @ 0x180697430`) takes the scale from there.
//!
//! ## Schema (H3 MCC)
//!
//! ```text
//! chocolate_mountain_new_struct_definition  size=12B
//!   block  per object type settings  → per_object_type_relative_min_luminance_block
//!
//! per_object_type_relative_min_luminance_block  size=4B
//!   real  min luminance
//! ```
//!
//! Per-element is a single `real` indexed positionally by the
//! `e_object_type` enum value. Reach 360 ships an extended 5-field
//! version (max_contrast, bounce_to_ambient, etc.); H3 MCC has only
//! the single float.

use crate::api::TagStruct;
use crate::file::TagFile;

const CHMT_GROUP: [u8; 4] = *b"chmt";

#[derive(Debug)]
pub enum ChocolateMountainError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for ChocolateMountainError {
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

impl std::error::Error for ChocolateMountainError {}

/// Decoded `.chocolate_mountain_new` tag.
///
/// `per_object_min_luminance[object_type]` — the per-object-type
/// minimum-luminance floor. Indexed positionally by the
/// `e_object_type` enum value. Empty when the scenario doesn't
/// reference a chmt tag (engine returns mountain_scale=-1.0).
#[derive(Debug, Clone, Default)]
pub struct ChocolateMountain {
    pub per_object_min_luminance: Vec<f32>,
}

impl ChocolateMountain {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ChocolateMountainError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != CHMT_GROUP {
            return Err(ChocolateMountainError::WrongGroup {
                expected: CHMT_GROUP,
                actual,
            });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let per_object_min_luminance = s
            .field("per object type settings")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(elem.read_real("min luminance").unwrap_or(0.0));
                    }
                }
                out
            })
            .unwrap_or_default();

        Self {
            per_object_min_luminance,
        }
    }

    /// Look up a per-object-type minimum luminance. Returns 0.0 for
    /// out-of-range indices — engine treats `min_lum > 0.0` as the
    /// gate, so a zero or missing entry is the no-op path.
    pub fn min_luminance_for(&self, object_type: usize) -> f32 {
        self.per_object_min_luminance
            .get(object_type)
            .copied()
            .unwrap_or(0.0)
    }

    /// `c_chocalate_moutain::apply_chocalate_mountain_lighting @
    /// 0x1806f9ee0` — verbatim port. Returns the mountain_scale the
    /// engine writes into the cached render_lighting (-1.0 when no
    /// boost applies, `sqrt(min_lum / render_exposure)` otherwise).
    pub fn compute_mountain_scale(&self, object_type: usize, render_exposure: f32) -> f32 {
        let min_lum = self.min_luminance_for(object_type);
        if min_lum > 0.0 && render_exposure > 0.0 {
            (min_lum / render_exposure).sqrt()
        } else {
            -1.0
        }
    }
}
