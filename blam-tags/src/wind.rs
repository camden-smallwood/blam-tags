//! `.wind` (wind_settings) tag walker — drives decorator + foliage
//! per-frame wind animation.
//!
//! Schema (`definitions/halo3_mcc/wind.json`, root struct `wind_block`,
//! 120 B `sizeof(s_wind_definition)`):
//!
//! | Tag offset | Type | Tag field name | Notes |
//! |-----------|------|----------------|-------|
//! | 0..20 | `wind_scalar_function_struct` | `direction` | Function output in degrees [0..360] |
//! | 20..40 | `wind_scalar_function_struct` | `speed` | Function output in MPH [0..200] |
//! | 40..60 | `wind_scalar_function_struct` | `bend` | Input is normalized speed [0..1]; output [0..10] |
//! | 60..80 | `wind_scalar_function_struct` | `oscillation` | Input is normalized speed |
//! | 80..100 | `wind_scalar_function_struct` | `frequency` | Input is normalized speed |
//! | 100..104 | `real` | `gust size:world units` | Spatial period in world units |
//! | 104..120 | `tag_reference` | `gust noise bitmap` | bitm tag-ref (default `random4_warp.bitmap`) |
//!
//! Each `wind_scalar_function_struct` wraps a single `mapping_function`
//! which carries the standard `c_function_definition` byte-blob
//! (`TagFunction::parse(&blob)`).
//!
//! Engine consumers:
//! - `s_wind_definition::update @ 0x180907F80` evaluates the 5 functions
//!   per frame and integrates a 20-bit ring buffer.
//! - `setup_wind_constants @ 0x180908290` binds the noise bitmap as
//!   vertex sampler 0 and uploads `wind_data` / `wind_data2` to slots
//!   `0x570000` / `0x570001`.
//! - `get_wind_definition @ 0x1809081E0` resolves the active wind tag
//!   per camera position (per-bsp; falls back to globals.default_wind).

use crate::api::TagStruct;
use crate::fields::TagFieldType;
use crate::file::TagFile;
use crate::tag_function::TagFunction;

#[derive(Debug)]
pub enum WindError {
    MissingField(&'static str),
}

impl std::fmt::Display for WindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(p) => write!(f, ".wind missing required field: {p}"),
        }
    }
}

impl std::error::Error for WindError {}

/// Resolved `.wind` tag — 5 evaluable functions + gust knobs.
#[derive(Debug, Clone)]
pub struct Wind {
    /// `direction` function — output in degrees [0..360]. Driven by
    /// game-clock seconds. Only `evaluate(t, 0.5)`.
    pub direction: TagFunction,
    /// `speed` function — output in MPH [0..200]. Driven by game-clock.
    pub speed: TagFunction,
    /// `bend` function — output [0..10]. Driven by **normalized speed**
    /// (= speed_mph / 1363.6).
    pub bend: TagFunction,
    /// `oscillation` function — driven by normalized speed.
    pub oscillation: TagFunction,
    /// `frequency` function — driven by normalized speed. Drives the
    /// per-frame ring-buffer drift rate.
    pub frequency: TagFunction,
    /// `gust size:world units` — spatial period of the noise pattern,
    /// world units. Reciprocal goes into `wind_data.z`. Riverworld =
    /// 30.0; default_wind = 1.0.
    pub gust_size: f32,
    /// `gust noise bitmap` — Halo-style relative path to the bitm tag.
    /// Empty when null. Default fallback (engine):
    /// `shaders/default_bitmaps/bitmaps/random4_warp.bitmap`.
    pub gust_noise_bitmap_path: String,
}

impl Wind {
    pub fn from_tag(tag: &TagFile) -> Result<Self, WindError> {
        let root = tag.root();
        Ok(Self {
            direction: read_wind_function(&root, "direction")?,
            speed: read_wind_function(&root, "speed")?,
            bend: read_wind_function(&root, "bend")?,
            oscillation: read_wind_function(&root, "oscillation")?,
            frequency: read_wind_function(&root, "frequency")?,
            // Schema field name = "gust size:world units" — colon and
            // unit suffix included in the tag-side name.
            gust_size: root.read_real("gust size").unwrap_or(1.0),
            gust_noise_bitmap_path: root
                .read_tag_ref_path("gust noise bitmap")
                .unwrap_or_default(),
        })
    }
}

/// Step into a `wind_scalar_function_struct` and pull the
/// `mapping_function::data` blob as a parsed `TagFunction`. The schema
/// declares TWO same-named "Mapping" fields (a `custom` marker and the
/// real `mapping_function` struct); walk by type instead of name to
/// land on the inner struct. Mirrors `light.rs::inner_mapping_function`.
fn read_wind_function(
    root: &TagStruct<'_>,
    field_name: &str,
) -> Result<TagFunction, WindError> {
    // Outer function-wrapper struct (e.g. "direction", "speed", ...).
    let outer = root
        .field(field_name)
        .and_then(|f| f.as_struct())
        .ok_or(WindError::MissingField(boxed_static(field_name)))?;
    // Inner mapping struct — the FIRST nested Struct field beneath the
    // outer wrapper. Skips the `Mapping` custom marker that precedes it.
    let mapping = outer
        .fields()
        .find(|f| f.field_type() == TagFieldType::Struct)
        .and_then(|f| f.as_struct())
        .ok_or(WindError::MissingField(boxed_static(field_name)))?;
    mapping
        .field("data")
        .and_then(|f| f.as_function())
        .ok_or(WindError::MissingField(boxed_static(field_name)))
}

/// Returns a `&'static str` for an arbitrary runtime &str — needed
/// because `WindError::MissingField` carries a `&'static str` (no
/// allocation on the hot path). For our small fixed set of field names
/// the leak is bounded; for unknown names we fall back to a generic.
fn boxed_static(name: &str) -> &'static str {
    match name {
        "direction" => "direction",
        "speed" => "speed",
        "bend" => "bend",
        "oscillation" => "oscillation",
        "frequency" => "frequency",
        _ => "wind_function",
    }
}
