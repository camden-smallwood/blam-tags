//! Building the `D3D_SHADER_MACRO[]` define list, matching `sub_140C587A0`
//! (base macros) and `sub_140C589E0` (category/option macros).
//!
//! The final array is **category/option macros first, then base macros**
//! (spec §4). A `MacroList` owns every name/value `CString`, so the raw
//! `D3D_SHADER_MACRO` array it produces stays valid as long as the list does.

use super::d3d::D3D_SHADER_MACRO;
use super::entry::{Stage, DEFORM_MACRO, ENTRY_POINT_MACRO, VERTEX_TYPE_MACRO};
use std::ffi::CString;

/// Which platform's `#define`s to emit. Both PC and Durango compile at SM5;
/// they differ only in these defines (spec §4b step 6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    /// Legacy DX9 define set (`DX_VERSION=9`). Not used by the live SM5 driver;
    /// kept because the table distinguishes it.
    Dx9,
    /// PC: `pc=1`, `DX_VERSION=11`.
    Pc,
    /// Durango: `pc=1`, `durango=1`, `DX_VERSION=11`.
    Durango,
}

/// An ordered, owned macro list. `Clone` so the PARAM_ALLOC path can fork it and
/// append its `PARAM_ALLOC_*` sentinels.
#[derive(Clone, Default)]
pub struct MacroList {
    /// `(name, Some(value))`; every value is present (the engine never emits a
    /// value-less define here).
    entries: Vec<(CString, CString)>,
}

impl MacroList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `#define name value`. A duplicate name is allowed (the last one
    /// wins at the preprocessor, matching how the engine overwrites the
    /// PARAM_ALLOC sentinel slot).
    pub fn define(&mut self, name: &str, value: &str) {
        self.entries.push((
            CString::new(name).expect("macro name has interior NUL"),
            CString::new(value).expect("macro value has interior NUL"),
        ));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if a define with this name is present.
    pub fn contains(&self, name: &str) -> bool {
        self.entries.iter().any(|(n, _)| n.as_bytes() == name.as_bytes())
    }

    /// Produce the raw, NUL-terminated `D3D_SHADER_MACRO[]`. The pointers borrow
    /// `self`, so keep `self` alive across the `D3DCompile`/`D3DPreprocess` call.
    pub fn as_raw(&self) -> Vec<D3D_SHADER_MACRO> {
        let mut raw: Vec<D3D_SHADER_MACRO> = self
            .entries
            .iter()
            .map(|(n, v)| D3D_SHADER_MACRO {
                Name: n.as_ptr(),
                Definition: v.as_ptr(),
            })
            .collect();
        raw.push(D3D_SHADER_MACRO {
            Name: std::ptr::null(),
            Definition: std::ptr::null(),
        });
        raw
    }

    /// Human-readable dump (`/Dname=value`), for the diagnostic log line.
    pub fn to_defines_string(&self) -> String {
        self.entries
            .iter()
            .map(|(n, v)| format!("/D{}={}", n.to_string_lossy(), v.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Extra base-macro toggles (`sub_140C587A0` args `a5`/`a7`/`a8`). Defaults off.
#[derive(Clone, Copy, Default)]
pub struct BaseMacroFlags {
    pub maybe_calc_albedo: bool,
    pub disable_register_reorder: bool,
    pub use_bool_constants_for_gamma2: bool,
}

/// Append the base macros in the engine's exact order (`sub_140C587A0`).
///
/// `entry` and `vertex_type` are ordinals into the tables in [`super::entry`].
pub fn append_base_macros(
    list: &mut MacroList,
    entry: usize,
    vertex_type: usize,
    stage: Stage,
    platform: Platform,
    flags: BaseMacroFlags,
) {
    list.define("entry_point", ENTRY_POINT_MACRO[entry]);
    list.define("vertex_type", VERTEX_TYPE_MACRO[vertex_type]);
    list.define("deform", DEFORM_MACRO[vertex_type]);
    list.define("SHADER_30", "1");
    list.define(stage.stage_macro(), "1");
    match platform {
        Platform::Dx9 => {
            list.define("DX_VERSION", "9");
        }
        Platform::Pc => {
            list.define("pc", "1");
            list.define("DX_VERSION", "11");
        }
        Platform::Durango => {
            list.define("pc", "1");
            list.define("durango", "1");
            list.define("DX_VERSION", "11");
        }
    }
    if flags.maybe_calc_albedo {
        list.define("maybe_calc_albedo", "1");
    }
    if flags.disable_register_reorder && platform != Platform::Dx9 {
        list.define("disable_register_reorder", "1");
    }
    if flags.use_bool_constants_for_gamma2 {
        list.define("USE_BOOL_CONSTANTS_FOR_GAMMA2", "1");
    }
}
