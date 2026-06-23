//! Strongly-typed, schema-faithful walkers for the **classic** shader tags
//! (Halo CE `shader_*`, Halo 2 `shader`) and the **Halo 4** `material` tag —
//! the shading analogue of [`crate::render_model`]. The Gen3 (H3/ODST/Reach)
//! `render_method` family is handled by [`crate::render_method`].
//!
//! Each walker mirrors its `definitions/<game>/<tag>.json` schema 1:1: every
//! struct/field maps to a public type/field with the same nesting, type names
//! PascalCase with the `_struct`/`_block` suffix stripped, field names
//! snake_case. Typed enums/flags use [`crate::typed_enums`] exactly as
//! `render_model` does.

// Shared CE scaffolding (tag_enum! macro + radiosity/physics/specular structs).
pub mod ce_common;
// One walker per tag, namespaced (subtypes declare same-named local enums like
// `FirstMapType`/`ColorFunction`, so flattening would collide).
pub mod ce_model;
pub mod ce_environment;
pub mod ce_transparent_glass;
pub mod ce_transparent_chicago;
pub mod ce_transparent_chicago_extended;
pub mod ce_transparent_generic;
pub mod ce_transparent_meter;
pub mod ce_transparent_plasma;
pub mod ce_transparent_water;
pub mod h2_shader;
pub mod h4_material;
pub mod h4_material_shader;

use crate::api::TagStruct;

/// A `tag_reference` resolved to its Halo-style relative path + group FOURCC.
/// Empty path / zero group when the reference was null.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagRef {
    pub path: String,
    pub group: u32,
}

impl TagRef {
    /// `true` when the reference was null (no path).
    pub fn is_null(&self) -> bool {
        self.path.is_empty()
    }
}

/// Errors from walking a classic shader / H4 material tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderError {
    /// The tag's group FOURCC isn't the one this walker parses.
    WrongGroup { expected: u32, found: u32 },
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShaderError::WrongGroup { expected, found } => write!(
                f,
                "wrong group: expected {}, found {}",
                fourcc(*expected),
                fourcc(*found)
            ),
        }
    }
}

impl std::error::Error for ShaderError {}

fn fourcc(g: u32) -> String {
    String::from_utf8_lossy(&g.to_be_bytes()).trim().to_owned()
}

/// Read a `tag_reference` field as a [`TagRef`] (null → default).
pub(crate) fn read_tag_ref(s: &TagStruct<'_>, name: &str) -> TagRef {
    match s.read_tag_ref_with_group(name) {
        Some((group, path)) => TagRef { path, group },
        None => TagRef::default(),
    }
}
