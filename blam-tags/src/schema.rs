//! JSON schema import — build a [`TagLayout`] from a per-group JSON
//! schema dumped by `halo3_dump_tag_definitions_json.py`.
//!
//! The result matches what `TagLayout::read` would produce from an
//! equivalent blay chunk: same string_data/string_offsets/string_lists,
//! struct_layouts/block_layouts/etc. with consistent indices, and
//! every struct's size + field offsets computed.
//!
//! The JSON's shape:
//! - Group metadata (`name`, `tag`, `parent_tag`, `version`, `flags`) +
//!   a `block` name that points at the root block.
//! - Named registries: `blocks`, `structs`, `arrays`, `enums_flags`,
//!   `datas`, `resources`, `interops`. Each map key is a definition
//!   name; each value is the body (no redundant `name` key).
//! - Fields' `definition` is either a name string into one of the
//!   registries (for struct/block/array/flags/enum/data/etc.), an
//!   integer byte-count (for pad/skip/useless_pad), a text string
//!   (for explanation), or an object `{flags, allowed}` (for
//!   tag_reference).
//!
//! Workflow: walk the registries, assign stable indices per kind
//! (alphabetical via [`BTreeMap`] for determinism), build the
//! `string_data` table dedup'd, resolve name references to indices,
//! populate every `TagLayout` table, and finally run
//! [`TagLayout::compute_struct_layout`] so every struct has its size +
//! per-field offsets set. Each computed struct size is cross-checked
//! against the JSON's dumped `size` field — mismatches bubble up as
//! [`TagSchemaError::StructSizeMismatch`] rather than silently
//! producing a broken layout.
//!
//! Inheritance: when a schema declares `parent_tag`, ancestor
//! registries are merged into the child via `merge_parent_schemas`
//! before the build, so cross-parent references (e.g. biped's
//! `biped_group` referencing `mapping_function` from object) resolve
//! transparently.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::fields::TagFieldType;
use crate::layout::{
    TagArrayLayout, TagBlockLayout, TagFieldLayout, TagFieldTypeLayout, TagInteropLayout,
    TagLayout, TagLayoutHeader, TagResourceLayout, TagStringList, TagStructLayout,
    TagTemplateHole,
};

/// Schema-import failures from [`TagLayout::from_json`]. Distinct
/// from [`crate::error::TagReadError`], which covers binary-read
/// failures.
#[derive(Debug)]
pub enum TagSchemaError {
    /// Filesystem error reading a schema JSON.
    Io(std::io::Error),
    /// Malformed JSON.
    Json(serde_json::Error),
    /// A reference to a sibling struct/array/resource definition by
    /// name didn't resolve.
    UnknownReference { kind: &'static str, name: String },
    /// A field's `definition` slot didn't match its type's expected
    /// shape (e.g. a struct field pointing at a missing struct name).
    BadFieldDefinition { field: String, ty: String },
    /// The schema named a field type the library doesn't model.
    UnknownFieldType(String),
    /// Guid string wasn't a valid 32-char hex sequence.
    BadGuid(String),
    /// Group tag string wasn't 1–4 ASCII chars.
    BadGroupTag(String),
    /// The walker computed a struct size that disagreed with the
    /// schema's declared size — caught early since downstream readers
    /// rely on the size for offset math.
    StructSizeMismatch { name: String, schema: u32, computed: usize },
}

impl std::fmt::Display for TagSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error reading schema: {e}"),
            Self::Json(e) => write!(f, "JSON parse error: {e}"),
            Self::UnknownReference { kind, name } => {
                write!(f, "schema references unknown {kind} {name:?}")
            }
            Self::BadFieldDefinition { field, ty } => {
                write!(f, "field {field:?} of type {ty:?} has invalid definition value")
            }
            Self::UnknownFieldType(s) => write!(f, "unknown field type {s:?}"),
            Self::BadGuid(s) => write!(f, "invalid guid {s:?} (expected 32 hex chars)"),
            Self::BadGroupTag(s) => write!(f, "invalid group tag {s:?} (expected 4 chars)"),
            Self::StructSizeMismatch { name, schema, computed } => write!(
                f,
                "computed size mismatch for struct {name:?}: schema says {schema}, computed {computed}"
            ),
        }
    }
}

impl std::error::Error for TagSchemaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Json(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TagSchemaError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}
impl From<serde_json::Error> for TagSchemaError {
    fn from(e: serde_json::Error) -> Self { Self::Json(e) }
}

//
// Serde shapes for the JSON schema files the dumper produces.
// Names match the library's `Tag*` convention + a `Schema` suffix.
//

#[derive(Debug, Deserialize)]
struct TagSchema {
    tag: String,
    #[serde(default)] parent_tag: Option<String>,
    version: u32,
    flags: u32,
    block: String,
    #[serde(default)] blocks: BTreeMap<String, TagBlockSchema>,
    #[serde(default)] structs: BTreeMap<String, TagStructSchema>,
    #[serde(default)] arrays: BTreeMap<String, TagArraySchema>,
    #[serde(default)] enums_flags: BTreeMap<String, TagEnumSchema>,
    #[serde(default)] datas: BTreeMap<String, TagDataSchema>,
    #[serde(default)] resources: BTreeMap<String, PageableResourceSchema>,
    #[serde(default)] interops: BTreeMap<String, ApiInteropSchema>,
    /// Classic Halo 2 only: base (latest) struct name -> { on-disk version
    /// -> variant struct name }. Present for multi-version layouts; the
    /// classic decoder selects the FieldSet matching a block/struct
    /// header's version field. Absent/empty for MCC + single-version.
    #[serde(default)] struct_versions: BTreeMap<String, BTreeMap<String, String>>,
}

impl TagSchema {
    /// Position of `name` in `self.structs` (alphabetical via
    /// `BTreeMap` iteration order). Used by the schema importer to
    /// translate name references in field `definition` slots into the
    /// `u32` indexes the binary layout records.
    fn struct_index(&self, name: &str) -> Option<u32> { index_of(&self.structs, name) }
    fn block_index(&self, name: &str) -> Option<u32> { index_of(&self.blocks, name) }
    fn array_index(&self, name: &str) -> Option<u32> { index_of(&self.arrays, name) }
    fn enum_index(&self, name: &str) -> Option<u32> { index_of(&self.enums_flags, name) }
    fn data_index(&self, name: &str) -> Option<u32> { index_of(&self.datas, name) }
    fn resource_index(&self, name: &str) -> Option<u32> { index_of(&self.resources, name) }
    fn interop_index(&self, name: &str) -> Option<u32> { index_of(&self.interops, name) }
}

fn index_of<V>(map: &BTreeMap<String, V>, name: &str) -> Option<u32> {
    map.keys().position(|k| k == name).map(|i| i as u32)
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagBlockSchema {
    max_count: u32,
    #[serde(rename = "struct")] struct_name: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagStructSchema {
    guid: String,
    size: u32,
    fields: Vec<TagFieldSchema>,
    /// Classic Halo 2 only: a 4-char group tag present when this inline
    /// struct carries a 16-byte block-style header on disk (e.g. `MAPP`).
    #[serde(default)]
    tag: Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagFieldSchema {
    #[serde(rename = "type")] ty: String,
    #[serde(default)] name: Option<String>,
    #[serde(default)] definition: serde_json::Value,
    #[serde(default)] group_tag: Option<String>,
    /// Set by [`fold_template_bases`] on a `tmpl` custom whose template's
    /// inherited base has been folded into the struct that follows it. Such a
    /// custom occupies no bytes, so the expansion pass must leave it at zero
    /// rather than widening it a second time. Never present in the JSON.
    #[serde(skip)] folded: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagArraySchema {
    count: u32,
    #[serde(rename = "struct")] struct_name: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagEnumSchema {
    options: Vec<Option<String>>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct TagDataSchema {}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct PageableResourceSchema {
    flags: u64,
    #[serde(rename = "struct")] struct_name: String,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
struct ApiInteropSchema {
    guid: String,
    #[serde(rename = "struct")] struct_name: String,
}

//
// Field-type metadata: canonical on-wire name, byte size, whether the
// type emits a sub-chunk. Each JSON field's `type` string (snake_case)
// maps to one of these rows; the (size, needs_sub_chunk) values match
// what the engine packs into each blay's `tgft` registry.
//
// A per-layout `field_types` table is then built incrementally — only
// types actually referenced by the schema get an entry, mirroring how
// real tags only carry the types they use.
//

struct FieldTypeInfo {
    ty: TagFieldType,
    canonical: &'static str,
    size: u32,
    needs_sub_chunk: u32,
}

/// JSON `"type": "..."` string → metadata. Snake-case names match what
/// the dumper emits; `canonical` is the space-separated form that goes
/// into the blay's string table (matches what `TagFieldType::from_name`
/// parses).
fn field_type_info(ty: &str) -> Option<FieldTypeInfo> {
    Some(match ty {
        "string"                   => FieldTypeInfo { ty: TagFieldType::String,                 canonical: "string",                   size: 32,  needs_sub_chunk: 0 },
        "long_string"              => FieldTypeInfo { ty: TagFieldType::LongString,             canonical: "long string",              size: 256, needs_sub_chunk: 0 },
        "string_id"                => FieldTypeInfo { ty: TagFieldType::StringId,               canonical: "string id",                size: 4,   needs_sub_chunk: 1 },
        "old_string_id"            => FieldTypeInfo { ty: TagFieldType::OldStringId,            canonical: "old string id",            size: 4,   needs_sub_chunk: 1 },
        "char_integer"             => FieldTypeInfo { ty: TagFieldType::CharInteger,            canonical: "char integer",             size: 1,   needs_sub_chunk: 0 },
        "short_integer"            => FieldTypeInfo { ty: TagFieldType::ShortInteger,           canonical: "short integer",            size: 2,   needs_sub_chunk: 0 },
        "long_integer"             => FieldTypeInfo { ty: TagFieldType::LongInteger,            canonical: "long integer",             size: 4,   needs_sub_chunk: 0 },
        "int64_integer"            => FieldTypeInfo { ty: TagFieldType::Int64Integer,           canonical: "int64 integer",            size: 8,   needs_sub_chunk: 0 },
        "byte_integer"             => FieldTypeInfo { ty: TagFieldType::ByteInteger,            canonical: "byte integer",             size: 1,   needs_sub_chunk: 0 },
        "word_integer"             => FieldTypeInfo { ty: TagFieldType::WordInteger,            canonical: "word integer",             size: 2,   needs_sub_chunk: 0 },
        "dword_integer"            => FieldTypeInfo { ty: TagFieldType::DwordInteger,           canonical: "dword integer",            size: 4,   needs_sub_chunk: 0 },
        "qword_integer"            => FieldTypeInfo { ty: TagFieldType::QwordInteger,           canonical: "qword integer",            size: 8,   needs_sub_chunk: 0 },
        "angle"                    => FieldTypeInfo { ty: TagFieldType::Angle,                  canonical: "angle",                    size: 4,   needs_sub_chunk: 0 },
        "tag"                      => FieldTypeInfo { ty: TagFieldType::Tag,                    canonical: "tag",                      size: 4,   needs_sub_chunk: 0 },
        "char_enum"                => FieldTypeInfo { ty: TagFieldType::CharEnum,               canonical: "char enum",                size: 1,   needs_sub_chunk: 0 },
        "short_enum"               => FieldTypeInfo { ty: TagFieldType::ShortEnum,              canonical: "short enum",               size: 2,   needs_sub_chunk: 0 },
        "long_enum"                => FieldTypeInfo { ty: TagFieldType::LongEnum,               canonical: "long enum",                size: 4,   needs_sub_chunk: 0 },
        "long_flags"               => FieldTypeInfo { ty: TagFieldType::LongFlags,              canonical: "long flags",               size: 4,   needs_sub_chunk: 0 },
        "word_flags"               => FieldTypeInfo { ty: TagFieldType::WordFlags,              canonical: "word flags",               size: 2,   needs_sub_chunk: 0 },
        "byte_flags"               => FieldTypeInfo { ty: TagFieldType::ByteFlags,              canonical: "byte flags",               size: 1,   needs_sub_chunk: 0 },
        "point_2d"                 => FieldTypeInfo { ty: TagFieldType::Point2d,                canonical: "point 2d",                 size: 4,   needs_sub_chunk: 0 },
        "rectangle_2d"             => FieldTypeInfo { ty: TagFieldType::Rectangle2d,            canonical: "rectangle 2d",             size: 8,   needs_sub_chunk: 0 },
        "rgb_color"                => FieldTypeInfo { ty: TagFieldType::RgbColor,               canonical: "rgb color",                size: 4,   needs_sub_chunk: 0 },
        "argb_color"               => FieldTypeInfo { ty: TagFieldType::ArgbColor,              canonical: "argb color",               size: 4,   needs_sub_chunk: 0 },
        "real"                     => FieldTypeInfo { ty: TagFieldType::Real,                   canonical: "real",                     size: 4,   needs_sub_chunk: 0 },
        "real_slider"              => FieldTypeInfo { ty: TagFieldType::RealSlider,             canonical: "real slider",              size: 4,   needs_sub_chunk: 0 },
        "real_fraction"            => FieldTypeInfo { ty: TagFieldType::RealFraction,           canonical: "real fraction",            size: 4,   needs_sub_chunk: 0 },
        "real_point_2d"            => FieldTypeInfo { ty: TagFieldType::RealPoint2d,            canonical: "real point 2d",            size: 8,   needs_sub_chunk: 0 },
        "real_point_3d"            => FieldTypeInfo { ty: TagFieldType::RealPoint3d,            canonical: "real point 3d",            size: 12,  needs_sub_chunk: 0 },
        "real_vector_2d"           => FieldTypeInfo { ty: TagFieldType::RealVector2d,           canonical: "real vector 2d",           size: 8,   needs_sub_chunk: 0 },
        "real_vector_3d"           => FieldTypeInfo { ty: TagFieldType::RealVector3d,           canonical: "real vector 3d",           size: 12,  needs_sub_chunk: 0 },
        "real_quaternion"          => FieldTypeInfo { ty: TagFieldType::RealQuaternion,         canonical: "real quaternion",          size: 16,  needs_sub_chunk: 0 },
        "real_euler_angles_2d"     => FieldTypeInfo { ty: TagFieldType::RealEulerAngles2d,      canonical: "real euler angles 2d",     size: 8,   needs_sub_chunk: 0 },
        "real_euler_angles_3d"     => FieldTypeInfo { ty: TagFieldType::RealEulerAngles3d,      canonical: "real euler angles 3d",     size: 12,  needs_sub_chunk: 0 },
        "real_plane_2d"            => FieldTypeInfo { ty: TagFieldType::RealPlane2d,            canonical: "real plane 2d",            size: 12,  needs_sub_chunk: 0 },
        "real_plane_3d"            => FieldTypeInfo { ty: TagFieldType::RealPlane3d,            canonical: "real plane 3d",            size: 16,  needs_sub_chunk: 0 },
        "real_rgb_color"           => FieldTypeInfo { ty: TagFieldType::RealRgbColor,           canonical: "real rgb color",           size: 12,  needs_sub_chunk: 0 },
        "real_argb_color"          => FieldTypeInfo { ty: TagFieldType::RealArgbColor,          canonical: "real argb color",          size: 16,  needs_sub_chunk: 0 },
        "real_hsv_color"           => FieldTypeInfo { ty: TagFieldType::RealHsvColor,           canonical: "real hsv color",           size: 12,  needs_sub_chunk: 0 },
        "real_ahsv_color"          => FieldTypeInfo { ty: TagFieldType::RealAhsvColor,          canonical: "real ahsv color",          size: 16,  needs_sub_chunk: 0 },
        "short_bounds"             => FieldTypeInfo { ty: TagFieldType::ShortIntegerBounds,     canonical: "short integer bounds",     size: 4,   needs_sub_chunk: 0 },
        "angle_bounds"             => FieldTypeInfo { ty: TagFieldType::AngleBounds,            canonical: "angle bounds",             size: 8,   needs_sub_chunk: 0 },
        "real_bounds"              => FieldTypeInfo { ty: TagFieldType::RealBounds,             canonical: "real bounds",              size: 8,   needs_sub_chunk: 0 },
        "fraction_bounds"          => FieldTypeInfo { ty: TagFieldType::FractionBounds,         canonical: "fraction bounds",          size: 8,   needs_sub_chunk: 0 },
        "tag_reference"            => FieldTypeInfo { ty: TagFieldType::TagReference,           canonical: "tag reference",            size: 16,  needs_sub_chunk: 1 },
        "block"                    => FieldTypeInfo { ty: TagFieldType::Block,                  canonical: "block",                    size: 12,  needs_sub_chunk: 1 },
        "long_block_flags"         => FieldTypeInfo { ty: TagFieldType::LongBlockFlags,         canonical: "long block flags",         size: 4,   needs_sub_chunk: 0 },
        "word_block_flags"         => FieldTypeInfo { ty: TagFieldType::WordBlockFlags,         canonical: "word block flags",         size: 2,   needs_sub_chunk: 0 },
        "byte_block_flags"         => FieldTypeInfo { ty: TagFieldType::ByteBlockFlags,         canonical: "byte block flags",         size: 1,   needs_sub_chunk: 0 },
        "char_block_index"         => FieldTypeInfo { ty: TagFieldType::CharBlockIndex,         canonical: "char block index",         size: 1,   needs_sub_chunk: 0 },
        "custom_char_block_index"  => FieldTypeInfo { ty: TagFieldType::CustomCharBlockIndex,   canonical: "custom char block index",  size: 1,   needs_sub_chunk: 0 },
        "short_block_index"        => FieldTypeInfo { ty: TagFieldType::ShortBlockIndex,        canonical: "short block index",        size: 2,   needs_sub_chunk: 0 },
        "custom_short_block_index" => FieldTypeInfo { ty: TagFieldType::CustomShortBlockIndex,  canonical: "custom short block index", size: 2,   needs_sub_chunk: 0 },
        "long_block_index"         => FieldTypeInfo { ty: TagFieldType::LongBlockIndex,         canonical: "long block index",         size: 4,   needs_sub_chunk: 0 },
        "custom_long_block_index"  => FieldTypeInfo { ty: TagFieldType::CustomLongBlockIndex,   canonical: "custom long block index",  size: 4,   needs_sub_chunk: 0 },
        "data"                     => FieldTypeInfo { ty: TagFieldType::Data,                   canonical: "data",                     size: 20,  needs_sub_chunk: 1 },
        "vertex_buffer"            => FieldTypeInfo { ty: TagFieldType::VertexBuffer,           canonical: "vertex buffer",            size: 32,  needs_sub_chunk: 0 },
        "pointer"                  => FieldTypeInfo { ty: TagFieldType::Pointer,                canonical: "pointer",                  size: 4,   needs_sub_chunk: 0 },
        "real_matrix_3x3"          => FieldTypeInfo { ty: TagFieldType::RealMatrix3x3,          canonical: "real matrix 3x3",          size: 36,  needs_sub_chunk: 0 },
        "pad"                      => FieldTypeInfo { ty: TagFieldType::Pad,                    canonical: "pad",                      size: 0,   needs_sub_chunk: 0 },
        "useless_pad"              => FieldTypeInfo { ty: TagFieldType::UselessPad,             canonical: "useless pad",              size: 0,   needs_sub_chunk: 0 },
        "skip"                     => FieldTypeInfo { ty: TagFieldType::Skip,                   canonical: "skip",                     size: 0,   needs_sub_chunk: 0 },
        "explanation"              => FieldTypeInfo { ty: TagFieldType::Explanation,            canonical: "explanation",              size: 0,   needs_sub_chunk: 0 },
        "custom"                   => FieldTypeInfo { ty: TagFieldType::Custom,                 canonical: "custom",                   size: 0,   needs_sub_chunk: 0 },
        "struct"                   => FieldTypeInfo { ty: TagFieldType::Struct,                 canonical: "struct",                   size: 0,   needs_sub_chunk: 1 },
        "array"                    => FieldTypeInfo { ty: TagFieldType::Array,                  canonical: "array",                    size: 0,   needs_sub_chunk: 0 },
        "tag_resource"             => FieldTypeInfo { ty: TagFieldType::PageableResource,       canonical: "pageable resource",        size: 8,   needs_sub_chunk: 1 },
        "tag_interop"              => FieldTypeInfo { ty: TagFieldType::ApiInterop,             canonical: "api interop",              size: 12,  needs_sub_chunk: 1 },
        "terminator"               => FieldTypeInfo { ty: TagFieldType::Terminator,             canonical: "terminator X",             size: 0,   needs_sub_chunk: 0 },
        "non_cache_runtime_value"  => FieldTypeInfo { ty: TagFieldType::NonCacheRuntimeValue,   canonical: "non-cache runtime value",  size: 4,   needs_sub_chunk: 0 },
        _ => return None,
    })
}

//
// String table builder — dedups identical strings so `name_offset`
// values in the layout point at shared bytes.
//

#[derive(Default)]
struct StringTable {
    bytes: Vec<u8>,
    offsets: std::collections::HashMap<String, u32>,
}

impl StringTable {
    fn new() -> Self {
        // An empty string at offset 0 is free and gives a canonical
        // "nameless" target for fields without a name.
        let mut me = Self::default();
        me.offsets.insert(String::new(), 0);
        me.bytes.push(0);
        me
    }
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&off) = self.offsets.get(s) {
            return off;
        }
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.bytes.push(0);
        self.offsets.insert(s.to_owned(), off);
        off
    }
}

/// Schema-side wrapper: a JSON-emitted group tag must be exactly 4
/// ASCII chars (the dumper preserves Halo's right-space padding
/// verbatim, e.g. `"rm  "`). Defers the actual u32 packing to
/// [`crate::fields::parse_group_tag`] so the byte-fiddling lives in
/// one place — only the strict length check is layout-specific.
fn parse_group_tag(s: &str) -> Result<u32, TagSchemaError> {
    if s.len() != 4 {
        return Err(TagSchemaError::BadGroupTag(s.to_owned()));
    }
    crate::fields::parse_group_tag(s).ok_or_else(|| TagSchemaError::BadGroupTag(s.to_owned()))
}

fn parse_guid(s: &str) -> Result<[u8; 16], TagSchemaError> {
    if s.len() != 32 {
        return Err(TagSchemaError::BadGuid(s.to_owned()));
    }
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
            .map_err(|_| TagSchemaError::BadGuid(s.to_owned()))?;
    }
    Ok(out)
}

/// Group-level metadata extracted from a schema JSON file. Not part
/// of `TagLayout` (blay doesn't carry it) but needed by `TagFile`
/// to populate its header.
#[derive(Debug, Clone)]
pub struct TagGroupMeta {
    pub tag: u32,
    pub version: u32,
    pub flags: u32,
    pub parent_tag: Option<u32>,
}

impl TagLayout {
    /// Build a TagLayout from a JSON schema file (per-group output of
    /// `halo3_dump_tag_definitions_json.py`). The result matches
    /// what `TagLayout::read` would produce from an equivalent blay
    /// chunk: same string_data/string_offsets/string_lists,
    /// struct_layouts/block_layouts/etc. with consistent indices, and
    /// every struct's size + field offsets computed.
    ///
    /// Returns `TagSchemaError::StructSizeMismatch` if the computed
    /// size of any struct disagrees with what the JSON's `size` field
    /// claims — that's our cross-check against `field_type_info`'s
    /// size column being wrong.
    pub fn from_json(path: impl AsRef<Path>) -> Result<Self, TagSchemaError> {
        Self::from_json_with_meta(path).map(|(l, _)| l)
    }

    /// Like [`TagLayout::from_json`] but also returns the group-level
    /// metadata (group tag, version, flags, parent_tag) that the JSON
    /// carries but blay doesn't. Needed when creating a new tag file
    /// from scratch — the file header needs `group_tag` /
    /// `group_version`.
    pub fn from_json_with_meta(
        path: impl AsRef<Path>,
    ) -> Result<(Self, TagGroupMeta), TagSchemaError> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)?;
        let mut schema: TagSchema = serde_json::from_reader(std::io::BufReader::new(file))?;
        let meta = TagGroupMeta {
            tag: parse_group_tag(&schema.tag)?,
            version: schema.version,
            flags: schema.flags,
            parent_tag: schema.parent_tag.as_deref().map(parse_group_tag).transpose()?,
        };
        // `tmpl` custom expansion sizes are resolved by loading the
        // sibling group JSONs from the same directory on demand.
        let defs_dir = path.parent().unwrap_or(Path::new("."));

        // Schemas only carry their *own* registry entries — anything
        // inherited from `parent_tag`'s chain (e.g. `biped` → `unit` →
        // `object` for shared structs like `mapping_function`) lives in
        // the ancestor JSONs. Walk the chain via `_meta.json` and merge
        // ancestor registries into the child so cross-parent references
        // resolve. Child wins on key collision (defensive — the dedupe
        // tool guarantees no overlap, but if a future override appears
        // we don't silently drop it).
        merge_parent_schemas(&mut schema, defs_dir);

        // A `tmpl` custom stands in for a template group that inherits from
        // another; the inherited fields belong to the struct the custom
        // introduces, not to the parent as padding. Fold them in before the
        // build so the layout describes them.
        fold_template_bases(&mut schema, defs_dir);

        let layout = build_layout_from_schema(schema, defs_dir)?;
        Ok((layout, meta))
    }
}

/// Walk `schema.parent_tag` recursively (via `_meta.json` for the
/// group-tag → filename mapping) and merge each ancestor's registries
/// into `schema`. Child entries take precedence; ancestor entries
/// fill in the gaps. Tolerates missing `_meta.json`, missing parent
/// files, or bogus group tags by silently treating them as "no
/// parent" — same posture as `tmpl_expansion_size`.
fn merge_parent_schemas(schema: &mut TagSchema, defs_dir: &Path) {
    let Ok(meta_bytes) = std::fs::read(defs_dir.join("_meta.json")) else { return };
    let Ok(meta_value): Result<serde_json::Value, _> = serde_json::from_slice(&meta_bytes) else {
        return;
    };
    let Some(tag_index) = meta_value.get("tag_index").and_then(|v| v.as_object()) else {
        return;
    };

    let mut current_parent = schema.parent_tag.clone();
    for _ in 0..32 {
        let Some(pt) = current_parent.take() else { break };
        let Some(name) = tag_index.get(&pt).and_then(|v| v.as_str()) else { break };
        let Ok(bytes) = std::fs::read(defs_dir.join(format!("{name}.json"))) else { break };
        let Ok(parent_schema): Result<TagSchema, _> = serde_json::from_slice(&bytes) else {
            break;
        };

        for (k, v) in parent_schema.blocks {
            schema.blocks.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.structs {
            schema.structs.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.arrays {
            schema.arrays.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.enums_flags {
            schema.enums_flags.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.datas {
            schema.datas.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.resources {
            schema.resources.entry(k).or_insert(v);
        }
        for (k, v) in parent_schema.interops {
            schema.interops.entry(k).or_insert(v);
        }

        current_parent = parent_schema.parent_tag;
    }
}

/// Fold the base a `tmpl` template inherits into the struct that carries it.
///
/// A `tmpl` custom names a *template group* — `?rmp`, `rmd `, `rmlv`, `rmb `,
/// `?rmc` — and is followed by a `struct` field holding that template's fields.
/// The template group derives from another (`?rmp` from `rm `), and on disk the
/// base's fields are part of the same struct: every shipped Halo 4 particle
/// writes `shader_particle_struct_definition` as 152 bytes beginning
/// `definition`, `reference`, `options`, `parameters`, `postprocess`, …, and so
/// does a particle ManagedBlam creates. The per-group schema carries only the
/// derived half and says so — `shader_particle_struct_definition`'s
/// `size_string` is literally
/// `sizeof(c_render_method_shader_particle)-sizeof(c_render_method)` — so the
/// base has to be read out of the ancestor group's own JSON and prepended here.
///
/// The importer used to leave the struct at its derived size and widen the
/// preceding `tmpl` custom by the base's width instead. That balances the
/// parent's declared size, which is why nothing caught it, but it describes the
/// base as opaque padding: the blocks inside it (`parameters`, `postprocess`,
/// `locked parameters`, and the ten struct definitions they bring) never reach
/// the layout, and an editing kit walking its own field list against the tag's
/// refuses to open it. Measured on a Halo 4 `particle`: 23 structs / 13 blocks /
/// 211 fields the old way against ManagedBlam's 33 / 22 / 295.
///
/// Only *ancestors* are folded — the template's own fields already live in the
/// struct being widened, which keeps that struct's name and GUID rather than
/// adopting the template group's (`material_struct`/`230d8113…` is what a
/// shipped tag writes, not `mat `'s own `material_block_struct`/`2b67f52e…`).
/// The ancestor's root struct and root block are likewise left out: they name
/// the base as a *definition*, and nothing in a shipped layout refers to it once
/// its fields are inline.
///
/// A template with no ancestors (`mat `) or one that will not resolve (`ssfx`,
/// which no `_meta.json` lists) folds nothing and is left exactly as it was.
fn fold_template_bases(schema: &mut TagSchema, defs_dir: &Path) {
    // (template group tag, name of the struct field that follows it). Collected
    // before mutating because the walk borrows `schema.structs`.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for struct_schema in schema.structs.values() {
        let mut pending: Option<&str> = None;
        for field in &struct_schema.fields {
            if field.ty == "custom" && field.group_tag.as_deref() == Some("tmpl") {
                pending = field.definition.as_str();
            } else if field.ty == "struct"
                && let Some(template) = pending.take()
                && let Some(target) = field.definition.as_str()
            {
                pairs.push((template.to_owned(), target.to_owned()));
            }
        }
    }

    let mut folded_templates: Vec<String> = Vec::new();
    let mut folded_targets: Vec<String> = Vec::new();
    for (template_tag, target_name) in pairs {
        // Two structs naming the same template each get the base; one struct
        // reached twice must not get it twice.
        if folded_targets.contains(&target_name) {
            continue;
        }
        let ancestors = template_ancestor_schemas(defs_dir, &template_tag);
        if ancestors.is_empty() {
            continue;
        }

        // Fold only when the struct is missing the base — the arithmetic says
        // which, without a per-game list. The template group's own root struct
        // is the whole thing (`?rmp` = 152); the schema being built carries
        // either the derived half (Halo 4 / Reach / ODST: 52, 4, or nothing at
        // all, and 52 + 100 == 152 asks for the base) or the whole thing
        // already (Halo 3: `shader_particle_struct_definition` is 64 and so is
        // `rm `, because H3 dumps the common shader fields straight into the
        // struct). Prepending to the second kind would count the base twice.
        let base_size: u32 = ancestors
            .iter()
            .filter_map(|ancestor| {
                let root = ancestor.blocks.get(&ancestor.block)?;
                Some(ancestor.structs.get(&root.struct_name)?.size)
            })
            .sum();
        let Some(template_root_size) = group_root_struct_size(defs_dir, &template_tag) else {
            continue;
        };
        let Some(local_size) = schema.structs.get(&target_name).map(|s| s.size) else { continue };
        if local_size.saturating_add(base_size) != template_root_size {
            continue;
        }

        // Outermost ancestor first, matching the order a C++ base contributes
        // its members: `rm `'s fields lead `shader_particle_struct_definition`.
        let mut prefix: Vec<TagFieldSchema> = Vec::new();
        let mut base_size: u32 = 0;
        let mut folded_any = false;
        for mut ancestor in ancestors {
            let Some(root_block) = ancestor.blocks.get(&ancestor.block) else { continue };
            let root_struct_name = root_block.struct_name.clone();
            if !ancestor.structs.contains_key(&root_struct_name) {
                continue;
            }
            let qualifier = ancestor.tag.clone();
            let renames = qualify_colliding_definitions(schema, &mut ancestor, &qualifier);
            let root_struct = &ancestor.structs[&root_struct_name];
            base_size = base_size.saturating_add(root_struct.size);
            // Renamed per ancestor, before joining the accumulated prefix: two
            // ancestors' rename maps are keyed by the same names, so a second
            // pass over fields the first already rewrote would rewrite them
            // again under the wrong qualifier.
            let mut base_fields: Vec<TagFieldSchema> = root_struct
                .fields
                .iter()
                .filter(|field| field.ty != "terminator")
                .cloned()
                .collect();
            rename_field_references(&mut base_fields, &renames);
            prefix.extend(base_fields);
            folded_any = true;
            merge_template_base_registries(schema, ancestor, &root_struct_name);
        }
        if !folded_any {
            continue;
        }

        let Some(target) = schema.structs.get_mut(&target_name) else { continue };
        target.size = target.size.saturating_add(base_size);
        prefix.append(&mut target.fields);
        target.fields = prefix;
        folded_templates.push(template_tag);
        folded_targets.push(target_name);
    }

    if folded_templates.is_empty() {
        return;
    }
    // The hole is gone; mark its custom so the expansion pass leaves it at zero.
    for struct_schema in schema.structs.values_mut() {
        for field in &mut struct_schema.fields {
            if field.ty == "custom"
                && field.group_tag.as_deref() == Some("tmpl")
                && field.definition.as_str().is_some_and(|t| folded_templates.iter().any(|f| f == t))
            {
                field.folded = true;
            }
        }
    }
}

/// The declared size of a group's root struct, or `None` if the group cannot be
/// resolved. What a template's *whole* struct measures, against which
/// [`fold_template_bases`] checks whether the base is already inline.
fn group_root_struct_size(defs_dir: &Path, group_tag: &str) -> Option<u32> {
    let meta_bytes = std::fs::read(defs_dir.join("_meta.json")).ok()?;
    let meta: serde_json::Value = serde_json::from_slice(&meta_bytes).ok()?;
    let name = meta.get("tag_index")?.get(group_tag)?.as_str()?;
    let bytes = std::fs::read(defs_dir.join(format!("{name}.json"))).ok()?;
    let schema: TagSchema = serde_json::from_slice(&bytes).ok()?;
    let root_block = schema.blocks.get(&schema.block)?;
    Some(schema.structs.get(&root_block.struct_name)?.size)
}

/// The schemas of every group above `target_tag` in its `parent_tag` chain,
/// outermost first. The target itself is excluded — [`fold_template_bases`]
/// folds what the template *inherits*, not what it declares. Empty when the
/// chain cannot be walked, which is the same "leave it alone" answer a template
/// with no parent gives.
fn template_ancestor_schemas(defs_dir: &Path, target_tag: &str) -> Vec<TagSchema> {
    let Ok(meta_bytes) = std::fs::read(defs_dir.join("_meta.json")) else { return Vec::new() };
    let Ok(meta): Result<serde_json::Value, _> = serde_json::from_slice(&meta_bytes) else {
        return Vec::new();
    };
    let Some(tag_index) = meta.get("tag_index").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut chain: Vec<TagSchema> = Vec::new();
    let mut cur = target_tag.to_owned();
    for _ in 0..32 {
        let Some(name) = tag_index.get(&cur).and_then(|v| v.as_str()) else { break };
        let Ok(bytes) = std::fs::read(defs_dir.join(format!("{name}.json"))) else { break };
        let Ok(schema): Result<TagSchema, _> = serde_json::from_slice(&bytes) else { break };
        let parent = schema.parent_tag.clone();
        // Skip the target itself — only its ancestors contribute.
        if cur != target_tag {
            chain.push(schema);
        }
        let Some(parent) = parent else { break };
        cur = parent;
    }
    chain.reverse();
    chain
}

/// Separator between a definition's name and the group tag that disambiguates
/// it. Chosen because no dumped definition name can contain a control
/// character, and stripped again by [`definition_display_name`] so the layout
/// still writes the plain name.
const DEFINITION_QUALIFIER: char = '\u{1}';

/// The name a definition is written under in the layout: everything before the
/// qualifier [`qualify_colliding_definitions`] may have appended.
///
/// Two definitions really can share a name. A Halo 4 particle carries a
/// `runtime_queryable_properties` array of 12 (the material's) *and* one of 28
/// (the render method's), and ManagedBlam writes both records under that one
/// name with their own counts and GUIDs. The registries here are keyed by name,
/// so the second copy needs a distinct key; the key is an implementation
/// detail and the name is what goes on disk.
fn definition_display_name(key: &str) -> &str {
    match key.split_once(DEFINITION_QUALIFIER) {
        Some((name, _)) => name,
        None => key,
    }
}

/// Which registry a field's `definition` name resolves against, for rewriting
/// references. Mirrors the dispatch in [`resolve_field_definition`] — a name
/// means nothing without the kind, and `runtime_queryable_properties` is both a
/// struct and an array in the same schema.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RegistryKind { Struct, Block, Array, EnumFlags, Data, Resource, Interop }

fn registry_kind_of(ty: TagFieldType) -> Option<RegistryKind> {
    Some(match ty {
        TagFieldType::Struct => RegistryKind::Struct,
        TagFieldType::Block
        | TagFieldType::LongBlockFlags
        | TagFieldType::WordBlockFlags
        | TagFieldType::ByteBlockFlags
        | TagFieldType::CharBlockIndex
        | TagFieldType::ShortBlockIndex
        | TagFieldType::LongBlockIndex => RegistryKind::Block,
        TagFieldType::Array => RegistryKind::Array,
        TagFieldType::CharEnum
        | TagFieldType::ShortEnum
        | TagFieldType::LongEnum
        | TagFieldType::LongFlags
        | TagFieldType::WordFlags
        | TagFieldType::ByteFlags => RegistryKind::EnumFlags,
        TagFieldType::Data => RegistryKind::Data,
        TagFieldType::PageableResource => RegistryKind::Resource,
        TagFieldType::ApiInterop => RegistryKind::Interop,
        _ => return None,
    })
}

/// A definition rename, by kind: `(kind, old name, new key)`.
type DefinitionRenames = Vec<(RegistryKind, String, String)>;

/// Re-key every definition in `ancestor` that collides with a *different*
/// definition of the same name already in `schema`, and rewrite the ancestor's
/// own references to match. Returns the renames so the caller can rewrite the
/// base's field list too.
///
/// An identical collision is left alone — `mapping_function` and `g_null_block`
/// are the same definition in both groups, and merging them is what
/// [`merge_parent_schemas`] has always done. It is the ones that differ that
/// used to be silently dropped: keeping the child's 12-element
/// `runtime_queryable_properties` for a render-method field that wants 28 is
/// how `render_method_postprocess_block` came out 32 bytes short.
///
/// Run to a fixed point, because separating one definition can separate
/// another. `int_block` is the same block in both groups until the *struct* it
/// names is re-keyed; then the two blocks disagree too, and ManagedBlam indeed
/// writes both. Rounds are bounded — each one strictly grows the rename set,
/// which is bounded by the ancestor's registry size, and eight is far past what
/// any real chain needs.
fn qualify_colliding_definitions(
    schema: &TagSchema,
    ancestor: &mut TagSchema,
    qualifier: &str,
) -> DefinitionRenames {
    let mut renames: DefinitionRenames = Vec::new();

    for _ in 0..8 {
        let mut round: DefinitionRenames = Vec::new();

        macro_rules! collect {
            ($kind:expr, $registry:ident) => {
                for (name, value) in ancestor.$registry.iter() {
                    if schema.$registry.get(name).is_some_and(|existing| existing != value) {
                        round.push((
                            $kind,
                            name.clone(),
                            format!("{name}{DEFINITION_QUALIFIER}{qualifier}"),
                        ));
                    }
                }
            };
        }
        collect!(RegistryKind::Struct, structs);
        collect!(RegistryKind::Block, blocks);
        collect!(RegistryKind::Array, arrays);
        collect!(RegistryKind::EnumFlags, enums_flags);
        collect!(RegistryKind::Data, datas);
        collect!(RegistryKind::Resource, resources);
        collect!(RegistryKind::Interop, interops);

        if round.is_empty() {
            break;
        }

        macro_rules! rekey {
            ($kind:expr, $registry:ident) => {
                for (_, old, new) in round.iter().filter(|(kind, _, _)| *kind == $kind) {
                    if let Some(value) = ancestor.$registry.remove(old) {
                        ancestor.$registry.insert(new.clone(), value);
                    }
                }
            };
        }
        rekey!(RegistryKind::Struct, structs);
        rekey!(RegistryKind::Block, blocks);
        rekey!(RegistryKind::Array, arrays);
        rekey!(RegistryKind::EnumFlags, enums_flags);
        rekey!(RegistryKind::Data, datas);
        rekey!(RegistryKind::Resource, resources);
        rekey!(RegistryKind::Interop, interops);

        // Everything in the ancestor that names a struct by hand.
        let struct_rename = |name: &mut String| {
            if let Some((_, _, new)) =
                round.iter().find(|(kind, old, _)| *kind == RegistryKind::Struct && old == name)
            {
                *name = new.clone();
            }
        };
        for block in ancestor.blocks.values_mut() { struct_rename(&mut block.struct_name); }
        for array in ancestor.arrays.values_mut() { struct_rename(&mut array.struct_name); }
        for resource in ancestor.resources.values_mut() { struct_rename(&mut resource.struct_name); }
        for interop in ancestor.interops.values_mut() { struct_rename(&mut interop.struct_name); }
        for struct_schema in ancestor.structs.values_mut() {
            rename_field_references(&mut struct_schema.fields, &round);
        }

        renames.extend(round);
    }

    renames
}

/// Point every field whose `definition` names a renamed definition at its new
/// key. Kind-aware: the same name can belong to two registries at once.
fn rename_field_references(fields: &mut [TagFieldSchema], renames: &DefinitionRenames) {
    if renames.is_empty() {
        return;
    }
    for field in fields {
        let Some(kind) = field_type_info(&field.ty).and_then(|info| registry_kind_of(info.ty))
        else {
            continue;
        };
        let Some(name) = field.definition.as_str() else { continue };
        if let Some((_, _, new)) =
            renames.iter().find(|(k, old, _)| *k == kind && old == name)
        {
            field.definition = serde_json::Value::String(new.clone());
        }
    }
}

/// Merge everything an inlined base's fields can refer to into the schema being
/// built, minus the base's own root struct and root block.
///
/// Existing entries win, as in [`merge_parent_schemas`]. By this point that is
/// safe rather than lossy: [`qualify_colliding_definitions`] has already moved
/// any entry that *disagreed* with the child's to a key of its own, so the
/// collisions left are the ones where both groups describe the same thing.
fn merge_template_base_registries(
    schema: &mut TagSchema,
    ancestor: TagSchema,
    root_struct_name: &str,
) {
    let root_block_name = ancestor.block.clone();
    for (k, v) in ancestor.blocks {
        if k == root_block_name {
            continue;
        }
        schema.blocks.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.structs {
        if k == root_struct_name {
            continue;
        }
        schema.structs.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.arrays {
        schema.arrays.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.enums_flags {
        schema.enums_flags.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.datas {
        schema.datas.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.resources {
        schema.resources.entry(k).or_insert(v);
    }
    for (k, v) in ancestor.interops {
        schema.interops.entry(k).or_insert(v);
    }
}

/// Walk a `tmpl` target's parent chain and return the cumulative
/// root-struct size. The target itself is *excluded* — its own fields
/// are serialized via the sibling `struct` field that follows the
/// tmpl custom. Returns 0 if the target can't be resolved (dead
/// templates like `ssfx` with no `_meta.json` entry).
///
/// Loads `_meta.json` to map group_tag → filename, then walks up the
/// chain reading each ancestor's JSON on demand.
fn tmpl_expansion_size(defs_dir: &Path, target_tag: &str) -> u32 {
    let Ok(meta_bytes) = std::fs::read(defs_dir.join("_meta.json")) else { return 0 };
    let Ok(meta): Result<serde_json::Value, _> = serde_json::from_slice(&meta_bytes) else {
        return 0;
    };
    let Some(tag_index) = meta.get("tag_index").and_then(|v| v.as_object()) else { return 0 };

    let mut sum: u32 = 0;
    let mut cur = target_tag.to_owned();
    for _ in 0..32 {
        let Some(name) = tag_index.get(&cur).and_then(|v| v.as_str()) else { break };
        let Ok(bytes) = std::fs::read(defs_dir.join(format!("{name}.json"))) else { break };
        let Ok(schema): Result<TagSchema, _> = serde_json::from_slice(&bytes) else { break };
        // Skip the target itself — we only add parent chain sizes.
        if cur != target_tag {
            let Some(block) = schema.blocks.get(&schema.block) else { break };
            let Some(rs) = schema.structs.get(&block.struct_name) else { break };
            sum = sum.saturating_add(rs.size);
        }
        let Some(parent) = schema.parent_tag else { break };
        cur = parent;
    }
    sum
}

fn build_layout_from_schema(
    schema: TagSchema,
    defs_dir: &Path,
) -> Result<TagLayout, TagSchemaError> {
    let _ = parse_group_tag(&schema.tag)?; // validate early

    let mut strings = StringTable::new();

    // field_types registry — grown on-demand as fields are emitted.
    let mut field_types: Vec<TagFieldTypeLayout> = Vec::new();
    let mut field_type_index_by_name: std::collections::HashMap<&'static str, u32> = Default::default();
    let mut intern_field_type = |canonical: &'static str, size: u32, needs_sub: u32,
                                 strings: &mut StringTable|
     -> u32 {
        if let Some(&i) = field_type_index_by_name.get(canonical) {
            return i;
        }
        let name_offset = strings.intern(canonical);
        let i = field_types.len() as u32;
        field_types.push(TagFieldTypeLayout {
            name_offset,
            size,
            needs_sub_chunk: needs_sub,
        });
        field_type_index_by_name.insert(canonical, i);
        i
    };

    // Build custom_block_index_search_names_offsets — one entry per
    // *distinct* search-name string seen on custom_*_block_index
    // fields. Fields' `definition` becomes the index into here.
    // (Our JSON doesn't currently carry search names, so this stays
    // empty unless the dumper starts emitting them.)
    let custom_block_index_search_names_offsets: Vec<u32> = Vec::new();

    // Build data_definition_name_offsets from `datas` keys.
    let data_definition_name_offsets: Vec<u32> = schema
        .datas
        .keys()
        .map(|n| strings.intern(definition_display_name(n)))
        .collect();

    // Build string_lists (enums/flags). Each enum's options go into
    // string_offsets contiguously; string_lists[i] points at that
    // slice.
    let mut string_offsets: Vec<u32> = Vec::new();
    let mut string_lists: Vec<TagStringList> = Vec::new();
    for (name, enum_schema) in &schema.enums_flags {
        let list_name_offset = strings.intern(definition_display_name(name));
        let first = string_offsets.len() as u32;
        for opt in &enum_schema.options {
            let off = match opt {
                Some(s) => strings.intern(s),
                None => 0, // null option slot → empty string at offset 0
            };
            string_offsets.push(off);
        }
        string_lists.push(TagStringList {
            offset: list_name_offset,
            count: enum_schema.options.len() as u32,
            first,
        });
    }

    // Helper for the four lookups below — array/resource/interop/block
    // each name a struct, and we want a uniform "unknown struct" error.
    let resolve_struct_name = |name: &str| -> Result<u32, TagSchemaError> {
        schema
            .struct_index(name)
            .ok_or_else(|| TagSchemaError::UnknownReference { kind: "struct", name: name.to_owned() })
    };

    // Build array_layouts (resolve each array's struct by name).
    let mut array_layouts: Vec<TagArrayLayout> = Vec::with_capacity(schema.arrays.len());
    for (name, array) in &schema.arrays {
        array_layouts.push(TagArrayLayout {
            name_offset: strings.intern(definition_display_name(name)),
            count: array.count,
            struct_index: resolve_struct_name(&array.struct_name)?,
        });
    }

    // Build resource_layouts.
    let mut resource_layouts: Vec<TagResourceLayout> = Vec::with_capacity(schema.resources.len());
    for (name, resource) in &schema.resources {
        resource_layouts.push(TagResourceLayout {
            name_offset: strings.intern(definition_display_name(name)),
            unknown: resource.flags as u32,
            struct_index: resolve_struct_name(&resource.struct_name)?,
        });
    }

    // Build interop_layouts.
    let mut interop_layouts: Vec<TagInteropLayout> = Vec::with_capacity(schema.interops.len());
    for (name, interop) in &schema.interops {
        interop_layouts.push(TagInteropLayout {
            name_offset: strings.intern(definition_display_name(name)),
            struct_index: resolve_struct_name(&interop.struct_name)?,
            guid: parse_guid(&interop.guid)?,
        });
    }

    // Build block_layouts.
    let mut block_layouts: Vec<TagBlockLayout> = Vec::with_capacity(schema.blocks.len());
    for (i, (name, block)) in schema.blocks.iter().enumerate() {
        block_layouts.push(TagBlockLayout {
            index: i as u32,
            name_offset: strings.intern(definition_display_name(name)),
            max_count: block.max_count,
            struct_index: resolve_struct_name(&block.struct_name)?,
        });
    }

    // Build struct_layouts + the flat `fields` array. For each struct,
    // remember its `first_field_index` before pushing its fields.
    let mut struct_layouts: Vec<TagStructLayout> = Vec::with_capacity(schema.structs.len());
    let mut fields: Vec<TagFieldLayout> = Vec::new();
    for (i, (name, struct_schema)) in schema.structs.iter().enumerate() {
        let first = fields.len() as u32;

        for field in &struct_schema.fields {
            let info = field_type_info(&field.ty)
                .ok_or_else(|| TagSchemaError::UnknownFieldType(field.ty.clone()))?;

            // An `explanation` is the schema's documentation text, and a shipped
            // layout does not carry the text — but it *does* carry the field.
            //
            // This used to drop them entirely, on the belief that tags carry none
            // at all. Measured against HREK, that is wrong: the kits keep a
            // zero-width, unnamed `custom` field at each explanation's position,
            // so `cheap_particle_emitter`'s root has 43 fields where dropping
            // them gave 36 — the seven explanations in its schema. The bytes were
            // right either way, because these occupy none; what diverged was the
            // field *list*, and an editing kit walking its own field list against
            // a tag's is entitled to refuse the tag or fall over on it.
            //
            // So they are emitted the way the kits write them: `custom`, size 0,
            // no name. Keeping them as `explanation` would align the count and
            // still disagree on the type.
            let explanation = matches!(info.ty, TagFieldType::Explanation);
            let (canonical, ty, size) = if explanation {
                ("custom", TagFieldType::Custom, 0)
            } else {
                (info.canonical, info.ty, info.size)
            };

            let type_index =
                intern_field_type(canonical, size, info.needs_sub_chunk, &mut strings);

            // Clean the field name to its bare display form (drop `:units`,
            // `#help`, `{alias}`, and trailing `*`/`!` markers) so the embedded
            // layout matches shipped tags rather than the verbose JSON schema.
            // A shipped layout carries no name for a `custom` field, whatever the
            // JSON calls it. Measured across HREK: every `custom` in a kit tag's
            // layout is unnamed — the decorators, and the `tmpl` render-method
            // hole alike — while the dump names them `types`, `mapping`,
            // `shader`. An explanation is the same case with its heading text.
            //
            // The name is metadata on a field that carries no data, so dropping
            // it moves nothing; what it buys is a field list the editing kits
            // recognise as their own, which is what `cheap_particle_emitter`
            // needed. The JSON keeps the names, so nothing is lost that a reader
            // of the schema wanted.
            let field_name_offset = match &field.name {
                Some(n) if !explanation && !matches!(ty, TagFieldType::Custom) => {
                    strings.intern(&clean_blay_field_name(n))
                }
                _ => 0,
            };

            let definition = if explanation {
                0
            } else {
                resolve_field_definition(field, info.ty, &schema)?
            };

            fields.push(TagFieldLayout {
                name_offset: field_name_offset,
                type_index,
                definition,
                field_type: ty,
                offset: 0, // computed later by compute_struct_layout
            });
        }

        struct_layouts.push(TagStructLayout {
            index: i as u32,
            guid: parse_guid(&struct_schema.guid)?,
            name_offset: strings.intern(definition_display_name(name)),
            first_field_index: first,
            size: 0, // computed later
            version: 0,
        });
    }

    // Pull root-block index. Its struct's guid/size become the layout-
    // level guid/root_data_size (matching `TagLayout::read`).
    let root_block_index = schema.block_index(&schema.block).ok_or_else(|| {
        TagSchemaError::UnknownReference { kind: "block", name: schema.block.clone() }
    })?;
    let root_struct_index = block_layouts[root_block_index as usize].struct_index as usize;
    let root_struct = &struct_layouts[root_struct_index];
    let layout_guid = root_struct.guid;
    let schema_root_size = schema.structs.iter().nth(root_struct_index).map(|(_, s)| s.size).unwrap_or(0);

    let header = TagLayoutHeader {
        tag_group_block_index: root_block_index,
        string_data_size: 0, // filled in below
        string_offset_count: string_offsets.len() as u32,
        string_list_count: string_lists.len() as u32,
        custom_block_index_search_names_count: custom_block_index_search_names_offsets.len() as u32,
        data_definition_name_count: data_definition_name_offsets.len() as u32,
        array_layout_count: array_layouts.len() as u32,
        field_type_count: field_types.len() as u32,
        field_count: fields.len() as u32,
        aggregate_layout_count: 0,
        struct_layout_count: struct_layouts.len() as u32,
        block_layout_count: block_layouts.len() as u32,
        resource_layout_count: resource_layouts.len() as u32,
        interop_layout_count: interop_layouts.len() as u32,
    };

    // Classic Halo 2 inline-struct tags (0 = no on-disk header), parallel
    // to struct_layouts order.
    let struct_tags: Vec<u32> = schema
        .structs
        .values()
        .map(|s| match &s.tag {
            Some(t) => crate::fields::parse_group_tag(t).unwrap_or(0),
            None => 0,
        })
        .collect();

    // Classic Halo 2 per-version struct variant table, parallel to
    // struct_layouts order. `Some(v)` only for a base (multi-version)
    // struct: `v[n]` is the struct index of the variant for on-disk
    // version `n`, gaps padded with the base index. `None` everywhere
    // else (single-version structs + the variant entries themselves).
    let struct_version_table: Vec<Option<Vec<u32>>> = schema
        .structs
        .keys()
        .map(|name| {
            let vmap = schema.struct_versions.get(name)?;
            let base_idx = schema.struct_index(name)?;
            let max_ver = vmap
                .keys()
                .filter_map(|k| k.parse::<usize>().ok())
                .max()
                .unwrap_or(0);
            let mut v = vec![base_idx; max_ver + 1];
            for (ver_str, variant) in vmap {
                if let (Ok(ver), Some(idx)) = (ver_str.parse::<usize>(), schema.struct_index(variant)) {
                    if ver < v.len() {
                        v[ver] = idx;
                    }
                }
            }
            Some(v)
        })
        .collect();

    let mut result = TagLayout {
        root_data_size: schema_root_size,
        guid: layout_guid,
        version: 3, // H3 MCC — layout payload version 3
        header: TagLayoutHeader {
            string_data_size: strings.bytes.len() as u32,
            ..header
        },
        string_data: strings.bytes,
        string_offsets,
        string_lists,
        custom_block_index_search_names_offsets,
        data_definition_name_offsets,
        array_layouts,
        field_types,
        fields,
        block_layouts,
        resource_layouts,
        interop_layouts,
        struct_layouts,
        struct_tags,
        struct_version_table,
        tmpl_holes: Vec::new(),
    };

    // Compute struct sizes + field offsets. First pass with tmpl
    // customs stored at 0 (no expansion) — matches how H3 schemas lay
    // out (common shader fields are inlined directly in the struct
    // field that follows the tmpl).
    //
    // Every `tmpl` custom is recorded, including the ones that expand to
    // nothing: the point of `tmpl_holes` is *which template* a hole belongs
    // to, and a dead template like `ssfx` still answers that question. Only
    // the non-zero ones go on to have their size patched into the field's
    // `definition` slot below, because only those change the arithmetic.
    //
    // A custom [`fold_template_bases`] has already handled is width zero: its
    // template's base is described by real fields in the struct that follows,
    // so widening the custom on top of that would double-count the bytes.
    // Everything reaching `tmpl_expansion_size` here is a template whose
    // ancestors could not be resolved, which sizes to nothing for the same
    // reason it could not be folded.
    let mut tmpl_holes: Vec<TagTemplateHole> = Vec::new();
    let tmpl_expansions: Vec<(usize, u32)> = {
        let mut out = Vec::new();
        let mut global_field_idx = 0usize;
        for (_, struct_schema) in schema.structs.iter() {
            for field in &struct_schema.fields {
                if field.ty == "custom"
                    && field.group_tag.as_deref() == Some("tmpl")
                    && let Some(target) = field.definition.as_str() {
                        let exp = if field.folded { 0 } else { tmpl_expansion_size(defs_dir, target) };
                        // A template whose tag is unparseable is not recorded
                        // rather than recorded as zero: an unknown identity and
                        // a known-empty one are different claims.
                        if let Ok(group_tag) = parse_group_tag(target) {
                            tmpl_holes.push(TagTemplateHole {
                                field_index: global_field_idx as u32,
                                group_tag,
                                size: exp,
                            });
                        }
                        if exp > 0 {
                            out.push((global_field_idx, exp));
                        }
                    }
                global_field_idx += 1;
            }
        }
        out
    };
    // `binary_search_by_key` in `template_hole` needs this ordering; the walk
    // above produces it, and sorting says so rather than relying on it.
    tmpl_holes.sort_by_key(|hole| hole.field_index);
    result.tmpl_holes = tmpl_holes;

    for i in 0..result.struct_layouts.len() {
        result.compute_struct_layout(i);
    }

    // Cross-check computed sizes against the schema's stated sizes.
    // If declared > computed and this struct has tmpl customs, apply
    // their expansion (Reach-style: parent-chain inlined here) and
    // recompute. If declared still doesn't match — or we're > declared
    // — it's a genuine mismatch.
    for (i, (name, struct_schema)) in schema.structs.iter().enumerate() {
        let computed = result.struct_layouts[i].size;
        let declared = struct_schema.size as usize;
        if computed == declared {
            continue;
        }
        if computed < declared {
            // Try tmpl expansion for this struct's fields.
            let first = result.struct_layouts[i].first_field_index as usize;
            let mut field_idx = first;
            let mut applied = 0usize;
            while result.fields[field_idx].field_type != TagFieldType::Terminator {
                if let Some(&(_, exp)) = tmpl_expansions.iter().find(|&&(fi, _)| fi == field_idx) {
                    result.fields[field_idx].definition = exp;
                    applied += exp as usize;
                }
                field_idx += 1;
            }
            if applied > 0 {
                // Reset the struct's size so compute_struct_layout runs again.
                result.struct_layouts[i].size = 0;
                result.compute_struct_layout(i);
            }
        }
        let computed = result.struct_layouts[i].size;
        if computed != declared {
            return Err(TagSchemaError::StructSizeMismatch {
                name: name.clone(),
                schema: struct_schema.size,
                computed,
            });
        }
    }

    // Update header size-counts that depend on final string_data size.
    result.header.string_data_size = result.string_data.len() as u32;

    // Everything above builds a layout that is *correct*. This makes it the one
    // the engine would have written: the tables re-ordered by the walk from the
    // root block, and the identifier a freshly-authored layout carries.
    reemit_in_engine_order(&mut result);
    result.version = persist_layout_version(defs_dir);
    result.root_data_size = u32::MAX;
    result.guid = new_layout_guid();

    Ok(result)
}

/// Which `blay` payload version the engine of this profile writes.
///
/// Not in the schema — it is a constant in each engine's layout writer
/// (`c_tag_layout_builder(builder, 4)` in Halo 4's), so it is read off the
/// profile name in `_meta.json`. Measured against the kits: 3,719 of 3,965
/// sampled Halo 4 tags are version 4, 3,998 of 3,999 Reach and 3,966 of 3,979
/// Halo 3 are version 3. The minority in each case are tags an older build
/// wrote and nothing has re-saved since.
///
/// Version 3 is the safe answer for a profile we have not measured: it is what
/// this importer emitted for every group before, every engine from Halo 3 on
/// reads it, and the two differ only in whether `stv4`'s per-struct version
/// field is present.
fn persist_layout_version(defs_dir: &Path) -> u32 {
    let Ok(bytes) = std::fs::read(defs_dir.join("_meta.json")) else { return 3 };
    let Ok(meta): Result<serde_json::Value, _> = serde_json::from_slice(&bytes) else { return 3 };
    match meta.get("game").and_then(|value| value.as_str()) {
        Some("halo4_mcc") | Some("halo2amp_mcc") => 4,
        _ => 3,
    }
}

/// A fresh identifier for a layout this library authored.
///
/// The engine's rule is a two-line branch in
/// `c_default_single_tag_file_layout_writer`: a *tracked* build stamps its build
/// number and leaves the guid zero, an untracked one stamps `-1` and calls
/// `system_global_unique_identifier_create`. The reader checks exactly that pair
/// — `(build != -1) XOR (guid != 0)` — and every one of 57,418 shipped Halo 3
/// and 449 Halo 4 kit tags satisfies it. We are an untracked build, so: `-1` and
/// a guid.
///
/// It is a version-4 (random) UUID because that is what the kits carry: 22,769
/// of the 22,773 Halo 4 tags with a guid have the version-4 nibble and an
/// RFC 4122 variant. The bytes come from the clock, the process id and a
/// counter rather than a random-number dependency — the guid has to be
/// *different* every time a layout is authored, not unguessable, and nothing
/// downstream derives anything from its value.
///
/// Note this is one field of a tag that cannot be reproduced by rebuilding it:
/// two saves of the same layout get different guids, which is why the kit ships
/// 1,070 prefabs with one layout and 1,070 distinct guids between them.
fn new_layout_guid() -> [u8; 16] {
    use std::hash::{BuildHasher, Hasher, RandomState};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seed = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut guid = [0u8; 16];
    for (half, chunk) in guid.chunks_mut(8).enumerate() {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(seed);
        hasher.write_u64(half as u64);
        hasher.write_u32(std::process::id());
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0),
        );
        chunk.copy_from_slice(&hasher.finish().to_le_bytes());
    }
    // RFC 4122: version 4 in the high nibble of the third group's high byte,
    // variant `10` in the top bits of the fourth group's first byte. The group
    // fields are stored little-endian, as `s_system_global_unique_identifier`
    // does.
    guid[7] = (guid[7] & 0x0F) | 0x40;
    guid[8] = (guid[8] & 0x3F) | 0x80;
    guid
}

/// Translate a field schema's `definition` value into the `u32` that
/// goes into the corresponding `TagFieldLayout`. The interpretation
/// depends on the field type:
///
/// - named-registry types (struct/block/array/flags/enum/data/
///   resource/interop): string → index into the matching table.
/// - `pad`/`useless_pad`/`skip`: integer → byte count (stored in the
///   `definition` slot verbatim).
/// - `tag_reference`: object → would normally store flags+allowed,
///   but blay only stores flags here (just flags slot).
/// - `explanation`: string → stored as a string offset into
///   string_data.
/// - primitives / `terminator`: 0.
/// Reduce a schema field name to the bare display name shipped tags store in
/// their embedded layout: cut at the first `:` (units) or `#` (help/tooltip),
/// drop `{alias}` annotations (keeping the base name, as shipped tags do), and
/// strip trailing `*`/`!` markers. Keeps `[range]` hints, matching real tags.
fn clean_blay_field_name(name: &str) -> String {
    let cut = name.find([':', '#']).unwrap_or(name.len());
    let mut s = name[..cut].to_string();
    while let (Some(open), Some(close)) = (s.find('{'), s.find('}')) {
        if open < close {
            s.replace_range(open..=close, "");
        } else {
            break;
        }
    }
    s.trim_end_matches(['*', '!', '^', ' ']).trim().to_string()
}

fn resolve_field_definition(
    field: &TagFieldSchema,
    ty: TagFieldType,
    schema: &TagSchema,
) -> Result<u32, TagSchemaError> {
    let def = &field.definition;

    // `custom` fields contribute 0 bytes by default. `tmpl`-typed
    // customs inline their target group's parent-chain size only
    // when the containing struct's declared size is larger than the
    // sum of plain field sizes — that post-hoc patch happens in
    // `build_layout_from_schema`, not here.
    if matches!(ty, TagFieldType::Custom) {
        return Ok(0);
    }

    // Primitives & no-definition types: return 0.
    if matches!(
        ty,
        TagFieldType::Unknown
            | TagFieldType::String
            | TagFieldType::LongString
            | TagFieldType::StringId
            | TagFieldType::OldStringId
            | TagFieldType::CharInteger
            | TagFieldType::ShortInteger
            | TagFieldType::LongInteger
            | TagFieldType::Int64Integer
            | TagFieldType::ByteInteger
            | TagFieldType::WordInteger
            | TagFieldType::DwordInteger
            | TagFieldType::QwordInteger
            | TagFieldType::Angle
            | TagFieldType::Tag
            | TagFieldType::Point2d
            | TagFieldType::Rectangle2d
            | TagFieldType::RgbColor
            | TagFieldType::ArgbColor
            | TagFieldType::Real
            | TagFieldType::RealSlider
            | TagFieldType::RealFraction
            | TagFieldType::RealPoint2d
            | TagFieldType::RealPoint3d
            | TagFieldType::RealVector2d
            | TagFieldType::RealVector3d
            | TagFieldType::RealQuaternion
            | TagFieldType::RealEulerAngles2d
            | TagFieldType::RealEulerAngles3d
            | TagFieldType::RealPlane2d
            | TagFieldType::RealPlane3d
            | TagFieldType::RealRgbColor
            | TagFieldType::RealArgbColor
            | TagFieldType::RealHsvColor
            | TagFieldType::RealAhsvColor
            | TagFieldType::ShortIntegerBounds
            | TagFieldType::AngleBounds
            | TagFieldType::RealBounds
            | TagFieldType::FractionBounds
            | TagFieldType::VertexBuffer
            | TagFieldType::Pointer
            | TagFieldType::RealMatrix3x3
            | TagFieldType::CustomCharBlockIndex
            | TagFieldType::CustomShortBlockIndex
            | TagFieldType::CustomLongBlockIndex
            | TagFieldType::Terminator
            | TagFieldType::NonCacheRuntimeValue,
    ) {
        return Ok(0);
    }

    // Pad/skip/useless_pad: definition is a byte count integer.
    if matches!(ty, TagFieldType::Pad | TagFieldType::UselessPad | TagFieldType::Skip) {
        return def
            .as_u64()
            .map(|v| v as u32)
            .ok_or_else(|| TagSchemaError::BadFieldDefinition {
                field: field.name.clone().unwrap_or_default(),
                ty: field.ty.clone(),
            });
    }

    // Explanation: store as 0 in the layout (blay's `definition` slot
    // holds the string offset at runtime via a separate mechanism).
    // Preserving the text in string_data is out-of-scope for now.
    if matches!(ty, TagFieldType::Explanation) {
        return Ok(0);
    }

    // tag_reference: the `definition` slot stays empty, and neither the schema's
    // `flags` nor its `allowed` list goes into blay's field record. The engine
    // keeps reference flags in its own definition and writes nothing here —
    // measured across 501 shipped Halo 4 and Reach kit tags, all 1,631
    // `tag reference` fields carry `definition == 0`, including the ones the
    // schema gives flags to (`render_method`'s `definition*` is 16 in the JSON
    // and 0 in every tag). Emitting the flags put a value in the field list that
    // no tag has; nothing in the library reads the slot back for this type.
    if matches!(ty, TagFieldType::TagReference) {
        return Ok(0);
    }

    // Named-registry types: resolve by name.
    let name = def.as_str().ok_or_else(|| TagSchemaError::BadFieldDefinition {
        field: field.name.clone().unwrap_or_default(),
        ty: field.ty.clone(),
    })?;
    let lookup = match ty {
        TagFieldType::Struct => schema.struct_index(name),
        TagFieldType::Block
        | TagFieldType::LongBlockFlags
        | TagFieldType::WordBlockFlags
        | TagFieldType::ByteBlockFlags
        | TagFieldType::CharBlockIndex
        | TagFieldType::ShortBlockIndex
        | TagFieldType::LongBlockIndex => schema.block_index(name),
        TagFieldType::Array => schema.array_index(name),
        TagFieldType::CharEnum
        | TagFieldType::ShortEnum
        | TagFieldType::LongEnum
        | TagFieldType::LongFlags
        | TagFieldType::WordFlags
        | TagFieldType::ByteFlags => schema.enum_index(name),
        TagFieldType::Data => schema.data_index(name),
        TagFieldType::PageableResource => schema.resource_index(name),
        TagFieldType::ApiInterop => schema.interop_index(name),
        _ => None,
    };
    lookup.ok_or_else(|| TagSchemaError::UnknownReference {
        kind: match ty {
            TagFieldType::Struct => "struct",
            TagFieldType::Block
            | TagFieldType::LongBlockFlags
            | TagFieldType::WordBlockFlags
            | TagFieldType::ByteBlockFlags
            | TagFieldType::CharBlockIndex
            | TagFieldType::ShortBlockIndex
            | TagFieldType::LongBlockIndex => "block",
            TagFieldType::Array => "array",
            TagFieldType::CharEnum
            | TagFieldType::ShortEnum
            | TagFieldType::LongEnum
            | TagFieldType::LongFlags
            | TagFieldType::WordFlags
            | TagFieldType::ByteFlags => "enum_or_flags",
            TagFieldType::Data => "data",
            TagFieldType::PageableResource => "resource",
            TagFieldType::ApiInterop => "interop",
            _ => "?",
        },
        name: name.to_owned(),
    })
}

/// The characters the engine truncates a persisted name at.
///
/// `find_tag_string_end` (tag_group_editing.cpp) scans for the first byte in
/// this set and cuts there, and every string that enters a `blay` goes through
/// it — field names, struct/block/array/interop names, and enum options alike.
/// Measured to agree: across 1,489 sampled Halo 4 kit tags, not one string in a
/// layout's table contains any of these.
const TAG_STRING_DELIMITERS: &[u8] = b":*#^|!{}&~";

/// A string as the engine persists it: everything before the first delimiter.
fn persisted_string(name: &str) -> &str {
    match name.bytes().position(|b| TAG_STRING_DELIMITERS.contains(&b)) {
        Some(end) => &name[..end],
        None => name,
    }
}

/// Rebuilds a layout's tables in the order the engine's tag-layout writer emits
/// them, so a schema-built layout is indistinguishable from a kit-authored one.
///
/// The importer builds its tables by walking the JSON registries alphabetically,
/// which is a fine way to get a *correct* layout and the wrong way to get the
/// engine's. `c_tag_layout_builder_context` walks the group from its root block
/// instead, and the walk decides everything: which index each definition gets,
/// which strings exist and in what order, even how many records a table has.
///
/// The rules, read out of `midnight_tag_debug`'s
/// `c_tag_layout_builder_context` / `c_tag_layout_builder`:
///
/// - **Structs** are memoized by identity and their index is reserved on the way
///   *down* (`reserve_struct_definition` before the field walk), so the struct
///   table is pre-order. Their *fields* are appended on the way back *up*
///   (`add_field_list` after the walk), so the flat field array is post-order,
///   and a struct's own name is interned after everything its fields reached.
/// - **Blocks, arrays, resources and interops** recurse into their struct first
///   and are then find-or-added *by record content*, so two references to the
///   same shape share a record and two same-named definitions that differ get
///   one each.
/// - **Field types** are interned on first use, so their order follows the walk.
/// - **A field with no bytes is not persistent** (`tag_field_is_persistent` is
///   `terminator || size > 0`): it is written with no name and the `custom`
///   type, whatever the schema called it.
/// - **Strings** are find-or-added over the raw character blob *including the
///   terminator*, so a string that is a suffix of one already present reuses its
///   offset rather than being appended again. That alone accounted for 15 of the
///   strings this importer used to emit that no kit tag has.
///
/// Validated by construction: with this pass, a Halo 4 `particle` built from
/// `definitions/halo4_mcc/particle.json` reproduces a ManagedBlam-authored one
/// table for table — 36 structs, 24 blocks, 3 arrays, 30 field types, 295
/// fields, 24 string lists, and all 7,377 bytes of string data, byte for byte.
fn reemit_in_engine_order(layout: &mut TagLayout) {
    // Classic Halo 2 layouts carry per-struct group tags and version variants in
    // arrays parallel to `struct_layouts`, and a classic tag has no `blay` on
    // disk for the order to matter to. Reordering would have to permute those
    // too, for no gain; leave them alone.
    if layout.struct_tags.iter().any(|tag| *tag != 0)
        || layout.struct_version_table.iter().any(|entry| entry.is_some())
    {
        return;
    }

    let mut out = EngineOrderBuilder::default();
    let root_block = layout.header.tag_group_block_index as usize;
    let new_root = out.add_block(layout, root_block);

    let mut header = TagLayoutHeader {
        tag_group_block_index: new_root,
        string_data_size: out.strings.len() as u32,
        string_offset_count: out.string_offsets.len() as u32,
        string_list_count: out.string_lists.len() as u32,
        custom_block_index_search_names_count: out.custom_search_names.len() as u32,
        data_definition_name_count: out.data_names.len() as u32,
        array_layout_count: out.arrays.len() as u32,
        field_type_count: out.field_types.len() as u32,
        field_count: out.fields.len() as u32,
        aggregate_layout_count: 0,
        struct_layout_count: out.structs.len() as u32,
        block_layout_count: out.blocks.len() as u32,
        resource_layout_count: out.resources.len() as u32,
        interop_layout_count: out.interops.len() as u32,
    };
    header.string_data_size = out.strings.len() as u32;

    // `tmpl_holes` indexes the flat field array, which has just been rebuilt.
    let mut holes: Vec<TagTemplateHole> = layout
        .tmpl_holes
        .iter()
        .filter_map(|hole| {
            out.field_remap
                .get(&(hole.field_index as usize))
                .map(|&new_index| TagTemplateHole { field_index: new_index as u32, ..*hole })
        })
        .collect();
    holes.sort_by_key(|hole| hole.field_index);

    let struct_count = out.structs.len();
    layout.header = header;
    layout.string_data = out.strings;
    layout.string_offsets = out.string_offsets;
    layout.string_lists = out.string_lists;
    layout.custom_block_index_search_names_offsets = out.custom_search_names;
    layout.data_definition_name_offsets = out.data_names;
    layout.array_layouts = out.arrays;
    layout.field_types = out.field_types;
    layout.fields = out.fields;
    layout.block_layouts = out.blocks;
    layout.resource_layouts = out.resources;
    layout.interop_layouts = out.interops;
    layout.struct_layouts = out.structs;
    layout.struct_tags = vec![0; struct_count];
    layout.struct_version_table = vec![None; struct_count];
    layout.tmpl_holes = holes;

    // Sizes and offsets were computed against the old indices; redo them.
    for entry in layout.struct_layouts.iter_mut() {
        entry.size = 0;
    }
    for index in 0..layout.struct_layouts.len() {
        layout.compute_struct_layout(index);
    }
}

/// Scratch state for [`reemit_in_engine_order`] — one output table per `blay`
/// section, plus the memo maps the engine keeps on its builder context.
#[derive(Default)]
struct EngineOrderBuilder {
    strings: Vec<u8>,
    string_offsets: Vec<u32>,
    string_lists: Vec<TagStringList>,
    custom_search_names: Vec<u32>,
    data_names: Vec<u32>,
    arrays: Vec<TagArrayLayout>,
    field_types: Vec<TagFieldTypeLayout>,
    fields: Vec<TagFieldLayout>,
    blocks: Vec<TagBlockLayout>,
    resources: Vec<TagResourceLayout>,
    interops: Vec<TagInteropLayout>,
    structs: Vec<TagStructLayout>,
    struct_memo: BTreeMap<usize, u32>,
    string_list_memo: BTreeMap<usize, u32>,
    field_type_memo: BTreeMap<usize, u32>,
    /// Old flat field index → new flat field index, for `tmpl_holes`.
    field_remap: BTreeMap<usize, usize>,
}

impl EngineOrderBuilder {
    /// Find-or-add over the raw blob, terminator included — so a string that is
    /// a suffix of one already stored shares its offset.
    fn add_string(&mut self, text: &str) -> u32 {
        let mut needle = persisted_string(text).as_bytes().to_vec();
        needle.push(0);
        if let Some(at) = self
            .strings
            .windows(needle.len())
            .position(|window| window == needle.as_slice())
        {
            return at as u32;
        }
        let at = self.strings.len() as u32;
        self.strings.extend_from_slice(&needle);
        at
    }

    fn string_at(layout: &TagLayout, offset: u32) -> String {
        layout.get_string(offset).unwrap_or_default().to_owned()
    }

    fn add_field_type(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        if let Some(&index) = self.field_type_memo.get(&old_index) {
            return index;
        }
        let source = &layout.field_types[old_index];
        let name = Self::string_at(layout, source.name_offset);
        let record = TagFieldTypeLayout {
            name_offset: self.add_string(&name),
            size: source.size,
            needs_sub_chunk: source.needs_sub_chunk,
        };
        let index = match self.field_types.iter().position(|existing| {
            existing.name_offset == record.name_offset
                && existing.size == record.size
                && existing.needs_sub_chunk == record.needs_sub_chunk
        }) {
            Some(index) => index as u32,
            None => {
                self.field_types.push(record);
                self.field_types.len() as u32 - 1
            }
        };
        self.field_type_memo.insert(old_index, index);
        index
    }

    fn add_block(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let source = layout.block_layouts[old_index];
        let struct_index = self.add_struct(layout, source.struct_index as usize);
        let name = Self::string_at(layout, source.name_offset);
        let name_offset = self.add_string(&name);
        match self.blocks.iter().position(|existing| {
            existing.name_offset == name_offset
                && existing.max_count == source.max_count
                && existing.struct_index == struct_index
        }) {
            Some(index) => index as u32,
            None => {
                let index = self.blocks.len() as u32;
                self.blocks.push(TagBlockLayout {
                    index,
                    name_offset,
                    max_count: source.max_count,
                    struct_index,
                });
                index
            }
        }
    }

    fn add_array(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let source = layout.array_layouts[old_index];
        let struct_index = self.add_struct(layout, source.struct_index as usize);
        let name = Self::string_at(layout, source.name_offset);
        let name_offset = self.add_string(&name);
        let record = TagArrayLayout { name_offset, count: source.count, struct_index };
        match self.arrays.iter().position(|existing| {
            existing.name_offset == record.name_offset
                && existing.count == record.count
                && existing.struct_index == record.struct_index
        }) {
            Some(index) => index as u32,
            None => {
                self.arrays.push(record);
                self.arrays.len() as u32 - 1
            }
        }
    }

    fn add_resource(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let source = layout.resource_layouts[old_index];
        let struct_index = self.add_struct(layout, source.struct_index as usize);
        let name = Self::string_at(layout, source.name_offset);
        let name_offset = self.add_string(&name);
        let record = TagResourceLayout { name_offset, unknown: source.unknown, struct_index };
        match self.resources.iter().position(|existing| {
            existing.name_offset == record.name_offset
                && existing.unknown == record.unknown
                && existing.struct_index == record.struct_index
        }) {
            Some(index) => index as u32,
            None => {
                self.resources.push(record);
                self.resources.len() as u32 - 1
            }
        }
    }

    fn add_interop(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let source = layout.interop_layouts[old_index];
        let struct_index = self.add_struct(layout, source.struct_index as usize);
        // Every interop definition in the engine answers to this name — 1,921
        // Halo 4 kit tags, 1,807 Reach and 1,896 Halo 3, and no other spelling.
        // The dumper records the C++ definition's identifier instead, which is
        // a name no tag carries.
        let name_offset = self.add_string(ENGINE_INTEROP_NAME);
        let record = TagInteropLayout { name_offset, struct_index, guid: source.guid };
        match self.interops.iter().position(|existing| {
            existing.name_offset == record.name_offset
                && existing.struct_index == record.struct_index
                && existing.guid == record.guid
        }) {
            Some(index) => index as u32,
            None => {
                self.interops.push(record);
                self.interops.len() as u32 - 1
            }
        }
    }

    fn add_data_definition(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let Some(&offset) = layout.data_definition_name_offsets.get(old_index) else {
            return 0;
        };
        let name = Self::string_at(layout, offset);
        let name_offset = self.add_string(&name);
        match self.data_names.iter().position(|&existing| existing == name_offset) {
            Some(index) => index as u32,
            None => {
                self.data_names.push(name_offset);
                self.data_names.len() as u32 - 1
            }
        }
    }

    fn add_custom_search_name(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        let Some(&offset) = layout.custom_block_index_search_names_offsets.get(old_index) else {
            return 0;
        };
        let name = Self::string_at(layout, offset);
        let name_offset = self.add_string(&name);
        match self.custom_search_names.iter().position(|&existing| existing == name_offset) {
            Some(index) => index as u32,
            None => {
                self.custom_search_names.push(name_offset);
                self.custom_search_names.len() as u32 - 1
            }
        }
    }

    /// Options first, in order, then the list's own name — `add_string_list`.
    fn add_string_list(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        if let Some(&index) = self.string_list_memo.get(&old_index) {
            return index;
        }
        let source = layout.string_lists[old_index];
        let mut offsets = Vec::with_capacity(source.count as usize);
        for entry in 0..source.count as usize {
            let option = layout
                .string_offsets
                .get(source.first as usize + entry)
                .map(|&offset| Self::string_at(layout, offset))
                .unwrap_or_default();
            offsets.push(self.add_string(&option));
        }
        let name = Self::string_at(layout, source.offset);
        let name_offset = self.add_string(&name);
        let first = self.string_offsets.len() as u32;
        self.string_offsets.extend(offsets);
        self.string_lists.push(TagStringList { offset: name_offset, count: source.count, first });
        let index = self.string_lists.len() as u32 - 1;
        self.string_list_memo.insert(old_index, index);
        index
    }

    /// Reserve the index on the way down, append the field list on the way up.
    fn add_struct(&mut self, layout: &TagLayout, old_index: usize) -> u32 {
        if let Some(&index) = self.struct_memo.get(&old_index) {
            return index;
        }
        let slot = self.structs.len();
        self.structs.push(TagStructLayout {
            index: slot as u32,
            guid: [0u8; 16],
            name_offset: 0,
            first_field_index: 0,
            size: 0,
            version: 0,
        });
        self.struct_memo.insert(old_index, slot as u32);

        let source = layout.struct_layouts[old_index];
        let mut built: Vec<(usize, TagFieldLayout)> = Vec::new();
        let mut field_index = source.first_field_index as usize;
        loop {
            let field = layout.fields[field_index];
            let terminator = field.field_type == TagFieldType::Terminator;
            // A field's width is the gap to the next field's offset; the
            // terminator sits at the struct's end and has no next.
            let width = if terminator {
                0
            } else {
                layout.fields[field_index + 1].offset.saturating_sub(field.offset)
            };
            if terminator || width > 0 {
                let name = Self::string_at(layout, field.name_offset);
                let name_offset = self.add_string(&name);
                let type_index = self.add_field_type(layout, field.type_index as usize);
                let definition = match field.field_type {
                    TagFieldType::Struct => self.add_struct(layout, field.definition as usize),
                    TagFieldType::Block
                    | TagFieldType::LongBlockFlags
                    | TagFieldType::WordBlockFlags
                    | TagFieldType::ByteBlockFlags
                    | TagFieldType::CharBlockIndex
                    | TagFieldType::ShortBlockIndex
                    | TagFieldType::LongBlockIndex => {
                        self.add_block(layout, field.definition as usize)
                    }
                    TagFieldType::Array => self.add_array(layout, field.definition as usize),
                    TagFieldType::CharEnum
                    | TagFieldType::ShortEnum
                    | TagFieldType::LongEnum
                    | TagFieldType::LongFlags
                    | TagFieldType::WordFlags
                    | TagFieldType::ByteFlags => {
                        self.add_string_list(layout, field.definition as usize)
                    }
                    TagFieldType::Data => self.add_data_definition(layout, field.definition as usize),
                    TagFieldType::PageableResource => {
                        self.add_resource(layout, field.definition as usize)
                    }
                    TagFieldType::ApiInterop => self.add_interop(layout, field.definition as usize),
                    TagFieldType::CustomCharBlockIndex
                    | TagFieldType::CustomShortBlockIndex
                    | TagFieldType::CustomLongBlockIndex => {
                        self.add_custom_search_name(layout, field.definition as usize)
                    }
                    _ => field.definition,
                };
                built.push((
                    field_index,
                    TagFieldLayout { name_offset, type_index, definition, ..field },
                ));
            } else {
                // Not persistent: no name, and the `custom` type regardless of
                // what the schema called it.
                let name_offset = self.add_string("");
                let type_index = self.add_custom_field_type(layout);
                built.push((
                    field_index,
                    TagFieldLayout {
                        name_offset,
                        type_index,
                        definition: 0,
                        field_type: TagFieldType::Custom,
                        offset: field.offset,
                    },
                ));
            }
            if terminator {
                break;
            }
            field_index += 1;
        }

        let name = Self::string_at(layout, source.name_offset);
        let name_offset = self.add_string(&name);
        let first = self.fields.len() as u32;
        for (old, record) in built {
            self.field_remap.insert(old, self.fields.len());
            self.fields.push(record);
        }
        self.structs[slot] = TagStructLayout {
            index: slot as u32,
            guid: source.guid,
            name_offset,
            first_field_index: first,
            size: 0,
            version: source.version,
        };
        slot as u32
    }

    /// The `custom` field type, interned the way a non-persistent field asks for
    /// it. Reuses the layout's own `custom` row when it has one so the type
    /// table matches; falls back to synthesizing the row.
    fn add_custom_field_type(&mut self, layout: &TagLayout) -> u32 {
        if let Some(index) = layout
            .field_types
            .iter()
            .position(|entry| layout.get_string(entry.name_offset) == Some("custom"))
        {
            return self.add_field_type(layout, index);
        }
        let name_offset = self.add_string("custom");
        match self.field_types.iter().position(|entry| entry.name_offset == name_offset) {
            Some(index) => index as u32,
            None => {
                self.field_types.push(TagFieldTypeLayout {
                    name_offset,
                    size: 0,
                    needs_sub_chunk: 0,
                });
                self.field_types.len() as u32 - 1
            }
        }
    }
}

/// What every `api interop` definition is called in a persisted layout.
const ENGINE_INTEROP_NAME: &str = "blah";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::TagLayout;

    /// The size and field list a group's `tmpl` template struct comes out with,
    /// plus the width the `tmpl` custom that introduces it occupies.
    fn template_struct(
        game: &str,
        group: &str,
        struct_name: &str,
    ) -> (usize, Vec<String>, Vec<u32>) {
        let layout =
            TagLayout::from_json(format!("../definitions/{game}/{group}.json")).unwrap();
        let index = layout
            .struct_layouts
            .iter()
            .position(|s| layout.get_string(s.name_offset) == Some(struct_name))
            .unwrap_or_else(|| panic!("{game}/{group} has no struct {struct_name}"));
        let mut names = Vec::new();
        let mut field = layout.struct_layouts[index].first_field_index as usize;
        loop {
            let record = &layout.fields[field];
            if record.field_type == TagFieldType::Terminator {
                break;
            }
            names.push(layout.get_string(record.name_offset).unwrap_or_default().to_owned());
            field += 1;
        }
        let hole_widths = layout
            .tmpl_holes
            .iter()
            .map(|hole| layout.fields[hole.field_index as usize].definition)
            .collect();
        (layout.struct_layouts[index].size, names, hole_widths)
    }

    /// A template's inherited base lands in the struct, not in the parent.
    ///
    /// Halo 4's `particle` is the case that sent us here: Bonobo refused to open
    /// a tag Baboon created, and the difference against the same tag from
    /// ManagedBlam was `shader_particle_struct_definition` — 52 bytes and six
    /// fields where the kit writes 152 and twenty, the missing hundred being
    /// `rm `'s, which the importer had been charging to a `tmpl` custom in
    /// `particle_struct_definition` as anonymous padding. The parent's size came
    /// out right either way, so only the field list catches it.
    #[test]
    fn a_templates_inherited_base_lands_in_the_struct_it_belongs_to() {
        let (size, names, holes) =
            template_struct("halo4_mcc", "particle", "shader_particle_struct_definition");
        assert_eq!(size, 152);
        assert_eq!(names.first().map(String::as_str), Some(""), "the `rm ` leading custom");
        assert_eq!(names.get(1).map(String::as_str), Some("definition"));
        assert!(names.iter().any(|n| n == "parameters"), "{names:?}");
        assert!(names.iter().any(|n| n == "postprocess"), "{names:?}");
        assert!(names.iter().any(|n| n == "locked parameters"), "{names:?}");
        assert_eq!(names.last().map(String::as_str), Some("palette"), "own fields still last");
        // The bytes moved into the struct, so the custom that named the template
        // occupies none — charging both would double-count the base.
        assert!(holes.iter().all(|&width| width == 0), "{holes:?}");
    }

    /// The Halo 3 schemas already inline the base, and must be left alone.
    ///
    /// This is the case that keeps the fold honest: `?rmp`'s whole struct is 64
    /// bytes there and so is `rm `'s, because H3's dump writes the common shader
    /// fields straight into `shader_particle_struct_definition`. Folding on the
    /// same rule that fixes Halo 4 would make it 128. Without a game that
    /// *disagrees*, "the base is inline" and "the base was added" look identical.
    #[test]
    fn a_base_the_schema_already_inlines_is_not_added_twice() {
        let (size, names, _) =
            template_struct("halo3_mcc", "particle", "shader_particle_struct_definition");
        assert_eq!(size, 64);
        assert_eq!(names.iter().filter(|n| *n == "definition").count(), 1, "{names:?}");
    }

    /// A template with no ancestors contributes nothing and renames nothing.
    ///
    /// Halo 4's `light_volume_system` names `mat `, which has no `parent_tag`.
    /// The struct that follows already is the whole material, and it keeps the
    /// group's own `material_struct` identity rather than adopting `mat `'s
    /// `material_block_struct` — which is what shipped tags carry.
    #[test]
    fn a_template_with_no_base_is_left_alone() {
        let (size, names, holes) =
            template_struct("halo4_mcc", "light_volume_system", "material_struct");
        assert_eq!(size, 68);
        assert_eq!(names.first().map(String::as_str), Some("material shader"));
        assert!(holes.iter().all(|&width| width == 0), "{holes:?}");
    }

    /// The tables come out in the order the engine writes them.
    ///
    /// Four properties that all follow from the walk and all failed before it:
    /// the root struct is reached first and so is index 0; the root *block* is
    /// added on the way back up and so is last; no persisted string carries the
    /// editor decorations the dump keeps; and an `api interop` is named the way
    /// the engine names one rather than the way the dumper does.
    #[test]
    fn the_layout_tables_come_out_in_the_order_the_engine_writes_them() {
        let layout = TagLayout::from_json("../definitions/halo4_mcc/particle.json").unwrap();

        let root_block = layout.header.tag_group_block_index as usize;
        assert_eq!(
            root_block,
            layout.block_layouts.len() - 1,
            "the root block is added after everything it reaches",
        );
        assert_eq!(
            layout.block_layouts[root_block].struct_index, 0,
            "the root struct is reached first",
        );

        let mut offset = 0usize;
        while offset < layout.string_data.len() {
            let end = layout.string_data[offset..]
                .iter()
                .position(|byte| *byte == 0)
                .map(|len| offset + len)
                .unwrap_or(layout.string_data.len());
            let text = &layout.string_data[offset..end];
            assert!(
                !text.iter().any(|byte| TAG_STRING_DELIMITERS.contains(byte)),
                "{:?} keeps a delimiter the engine cuts at",
                String::from_utf8_lossy(text),
            );
            offset = end + 1;
        }

        assert!(!layout.interop_layouts.is_empty(), "a particle carries one");
        for interop in &layout.interop_layouts {
            assert_eq!(layout.get_string(interop.name_offset), Some(ENGINE_INTEROP_NAME));
        }
    }

    /// A string that is a suffix of one already in the blob shares its offset.
    ///
    /// `find_or_add_explicit_list<char>` searches the raw character buffer for
    /// the string *and its terminator*, so `flags` costs nothing once
    /// `main flags` is there. Emitting it separately is what put fifteen strings
    /// in a Halo 4 particle that no kit tag has.
    #[test]
    fn a_string_that_is_a_suffix_of_another_shares_its_offset() {
        let layout = TagLayout::from_json("../definitions/halo4_mcc/particle.json").unwrap();
        let long = layout
            .string_data
            .windows(b"main flags\0".len())
            .position(|window| window == b"main flags\0")
            .expect("the root's `main flags` field");
        let short = layout
            .string_data
            .windows(b"flags\0".len())
            .position(|window| window == b"flags\0")
            .expect("`flags` resolves somewhere");
        assert_eq!(short, long + "main ".len(), "`flags` reuses the tail of `main flags`");
    }

    /// The identifier says what the engine's writer would say.
    ///
    /// `version_get_build_number()` is -1 for an untracked build and the guid is
    /// then generated; the reader accepts the pair only when exactly one of
    /// "build is -1" and "guid is set" holds. The payload version is the
    /// engine's per-profile constant, 4 for Halo 4 and 3 for Reach.
    #[test]
    fn the_layout_identifier_is_the_pair_the_engine_checks_for() {
        for (game, expected_version) in [("halo4_mcc", 4u32), ("haloreach_mcc", 3)] {
            let layout =
                TagLayout::from_json(format!("../definitions/{game}/particle.json")).unwrap();
            assert_eq!(layout.version, expected_version, "{game} payload version");
            assert_eq!(layout.root_data_size, u32::MAX, "{game} build stamp");
            assert_ne!(layout.guid, [0u8; 16], "{game} guid");
            assert_eq!(layout.guid[7] & 0xF0, 0x40, "{game} guid is version 4");
            assert_eq!(layout.guid[8] & 0xC0, 0x80, "{game} guid has the RFC variant");
        }
        // Two layouts authored separately must not share one.
        let first = TagLayout::from_json("../definitions/halo4_mcc/particle.json").unwrap();
        let second = TagLayout::from_json("../definitions/halo4_mcc/particle.json").unwrap();
        assert_ne!(first.guid, second.guid);
    }

    /// The layout a schema builds is the layout the kit's own tags carry.
    ///
    /// The end of the whole exercise, and the only test that can tell "correct"
    /// from "identical": every table, every index and every byte of string data
    /// compared against tags the editing kit wrote, with only the 20-byte
    /// identifier skipped — the build stamp and the guid, which are properties
    /// of the *save* rather than of the layout and are not reproducible by
    /// construction.
    ///
    /// Ignored by default — it needs a loose kit.
    ///
    /// Run with:
    ///   BLAM_TEST_H4EK=~/Halo/halo4_mcc/tags cargo test the_generated_layout -- --ignored
    #[test]
    #[ignore = "requires a loose Halo 4 tag tree; set BLAM_TEST_H4EK"]
    fn the_generated_layout_is_the_one_the_kits_tags_carry() {
        let Ok(root) = std::env::var("BLAM_TEST_H4EK") else {
            eprintln!("skipping: set BLAM_TEST_H4EK to a loose Halo 4 tags directory");
            return;
        };
        // A group whose shipped tags were all authored against the definition
        // the dump describes, so any mismatch is ours rather than drift.
        let layout = TagLayout::from_json("../definitions/halo4_mcc/prefab.json").unwrap();
        let mut ours = Vec::new();
        layout.write(&mut ours).unwrap();
        let ours = &ours[IDENTIFIER_END..];

        let mut compared = 0usize;
        for path in crate::convert::walk_files(std::path::Path::new(&root)) {
            if path.extension().is_none_or(|e| !e.eq_ignore_ascii_case("prefab")) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let Some(shipped) = shipped_layout_chunk(&bytes) else { continue };
            assert_eq!(
                &shipped[IDENTIFIER_END..],
                ours,
                "{} carries a different layout",
                path.display()
            );
            compared += 1;
            if compared == 64 {
                break;
            }
        }
        assert!(compared > 0, "no prefabs under {root}");
        eprintln!("layout reproduced on {compared} shipped prefabs");
    }

    /// Past the `blay` chunk header and the identifier it starts with.
    const IDENTIFIER_END: usize = 12 + 4 + 16;

    /// The `blay` chunk of an MCC tag file, header included.
    fn shipped_layout_chunk(bytes: &[u8]) -> Option<&[u8]> {
        if bytes.len() < 80 || &bytes[60..64] != b"MALB" {
            return None;
        }
        let read = |at: usize| -> u32 {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        if &bytes[64..68] != b"!gat" {
            return None;
        }
        let layout_at = 64 + 12;
        if bytes.get(layout_at..layout_at + 4)? != b"yalb" {
            return None;
        }
        let size = read(layout_at + 8) as usize;
        bytes.get(layout_at..layout_at + 12 + size)
    }

    /// Two definitions that share a name and differ are both kept.
    ///
    /// A Halo 4 particle carries the material's 12-element
    /// `runtime_queryable_properties` and the render method's 28-element one.
    /// Merging the base's registry by name alone dropped the second, and
    /// `render_method_postprocess_block` came out 32 bytes short — a size error
    /// rather than a wrong tag, which is the only reason it was noticed.
    #[test]
    fn a_base_definition_that_collides_by_name_keeps_its_own_shape() {
        let layout = TagLayout::from_json("../definitions/halo4_mcc/particle.json").unwrap();
        let mut counts: Vec<u32> = layout
            .array_layouts
            .iter()
            .filter(|a| layout.get_string(a.name_offset) == Some("runtime_queryable_properties"))
            .map(|a| a.count)
            .collect();
        counts.sort_unstable();
        assert_eq!(counts, vec![12, 28]);

        let postprocess = layout
            .struct_layouts
            .iter()
            .find(|s| layout.get_string(s.name_offset) == Some("render_method_postprocess_block"))
            .unwrap();
        assert_eq!(postprocess.size, 172);
    }
}
