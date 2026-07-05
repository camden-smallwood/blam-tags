//! Field-level types.
//!
//! [`TagFieldType`] is the resolved, dispatch-ready enum form of a tag
//! field's type-name string — computed once per field during layout read
//! and then used in every hot read/write path to avoid re-comparing
//! strings.
//!
//! [`TagFieldData`] is the typed, per-field value representation
//! produced by `deserialize_field` and consumed by `serialize_field`.
//! It sits on top of the raw-data + sub-chunks roundtrip path in
//! [`crate::data`] — sub-chunk payloads come in as `Vec<u8>` and already
//! stripped of their outer chunk header, so the parse functions here
//! operate on payload bytes rather than readers.

use crate::data::TagSubChunkContent;
use crate::io::Endian;
use crate::layout::{TagFieldLayout, TagLayout};
use crate::math;

/// Resolved type of a layout field.
///
/// Computed once per [`crate::layout::TagFieldLayout`] during
/// [`crate::layout::TagLayout::read`] by [`TagFieldType::from_name`], so
/// the hot read/write paths can dispatch on an enum instead of comparing
/// type-name strings on every field.
///
/// Variant names mirror the Halo type-name strings (e.g. `"real point 3d"
/// → RealPoint3d`). `Unknown` is the fallback for any unrecognized name
/// and also the initial value before resolution.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TagFieldType {
    Unknown,
    String,
    LongString,
    StringId,
    OldStringId,
    CharInteger,
    ShortInteger,
    LongInteger,
    Int64Integer,
    ByteInteger,
    WordInteger,
    DwordInteger,
    QwordInteger,
    Angle,
    Tag,
    CharEnum,
    ShortEnum,
    LongEnum,
    LongFlags,
    WordFlags,
    ByteFlags,
    Point2d,
    Rectangle2d,
    RgbColor,
    ArgbColor,
    Real,
    RealSlider,
    RealFraction,
    RealPoint2d,
    RealPoint3d,
    RealVector2d,
    RealVector3d,
    RealQuaternion,
    RealEulerAngles2d,
    RealEulerAngles3d,
    RealPlane2d,
    RealPlane3d,
    RealRgbColor,
    RealArgbColor,
    RealHsvColor,
    RealAhsvColor,
    ShortIntegerBounds,
    AngleBounds,
    RealBounds,
    FractionBounds,
    TagReference,
    Block,
    LongBlockFlags,
    WordBlockFlags,
    ByteBlockFlags,
    CharBlockIndex,
    CustomCharBlockIndex,
    ShortBlockIndex,
    CustomShortBlockIndex,
    LongBlockIndex,
    CustomLongBlockIndex,
    Data,
    VertexBuffer,
    /// Classic (Halo CE / H2) 4-byte cache pointer — opaque, raw-only.
    Pointer,
    /// Classic 3x3 float matrix (36 bytes) — raw-only for now.
    RealMatrix3x3,
    Pad,
    UselessPad,
    Skip,
    Explanation,
    Custom,
    Struct,
    Array,
    PageableResource,
    ApiInterop,
    Terminator,
    NonCacheRuntimeValue,
}

impl TagFieldType {
    /// Parse a field-type name string (as stored in the layout's
    /// `string_data`) into the dispatch enum. Unknown names are
    /// tolerated and returned as [`TagFieldType::Unknown`] — this is
    /// how we detect new or unhandled type names without panicking.
    pub fn from_name(name: &str) -> Self {
        match name {
            "string" => Self::String,
            "long string" => Self::LongString,
            "string id" => Self::StringId,
            "old string id" => Self::OldStringId,
            "char integer" => Self::CharInteger,
            "short integer" => Self::ShortInteger,
            "long integer" => Self::LongInteger,
            "int64 integer" => Self::Int64Integer,
            "byte integer" => Self::ByteInteger,
            "word integer" => Self::WordInteger,
            "dword integer" => Self::DwordInteger,
            "qword integer" => Self::QwordInteger,
            "angle" => Self::Angle,
            "tag" => Self::Tag,
            "char enum" => Self::CharEnum,
            "short enum" => Self::ShortEnum,
            "long enum" => Self::LongEnum,
            "long flags" => Self::LongFlags,
            "word flags" => Self::WordFlags,
            "byte flags" => Self::ByteFlags,
            "point 2d" => Self::Point2d,
            "rectangle 2d" => Self::Rectangle2d,
            "rgb color" => Self::RgbColor,
            "argb color" => Self::ArgbColor,
            "real" => Self::Real,
            "real slider" => Self::RealSlider,
            "real fraction" => Self::RealFraction,
            "real point 2d" => Self::RealPoint2d,
            "real point 3d" => Self::RealPoint3d,
            "real vector 2d" => Self::RealVector2d,
            "real vector 3d" => Self::RealVector3d,
            "real quaternion" => Self::RealQuaternion,
            "real euler angles 2d" => Self::RealEulerAngles2d,
            "real euler angles 3d" => Self::RealEulerAngles3d,
            "real plane 2d" => Self::RealPlane2d,
            "real plane 3d" => Self::RealPlane3d,
            "real rgb color" => Self::RealRgbColor,
            "real argb color" => Self::RealArgbColor,
            "real hsv color" => Self::RealHsvColor,
            "real ahsv color" => Self::RealAhsvColor,
            "short integer bounds" => Self::ShortIntegerBounds,
            "angle bounds" => Self::AngleBounds,
            "real bounds" => Self::RealBounds,
            "fraction bounds" => Self::FractionBounds,
            "tag reference" => Self::TagReference,
            "block" => Self::Block,
            "long block flags" => Self::LongBlockFlags,
            "word block flags" => Self::WordBlockFlags,
            "byte block flags" => Self::ByteBlockFlags,
            "char block index" => Self::CharBlockIndex,
            "custom char block index" => Self::CustomCharBlockIndex,
            "short block index" => Self::ShortBlockIndex,
            "custom short block index" => Self::CustomShortBlockIndex,
            "long block index" => Self::LongBlockIndex,
            "custom long block index" => Self::CustomLongBlockIndex,
            "data" => Self::Data,
            "vertex buffer" => Self::VertexBuffer,
            "pointer" => Self::Pointer,
            "real matrix 3x3" => Self::RealMatrix3x3,
            "pad" => Self::Pad,
            "useless pad" => Self::UselessPad,
            "skip" => Self::Skip,
            "explanation" => Self::Explanation,
            "custom" => Self::Custom,
            "struct" => Self::Struct,
            "array" => Self::Array,
            "pageable resource" => Self::PageableResource,
            "api interop" => Self::ApiInterop,
            "terminator X" => Self::Terminator,
            "non-cache runtime value" => Self::NonCacheRuntimeValue,
            _ => Self::Unknown,
        }
    }
}

//================================================================================
// Sub-chunk payload wrappers.
//
// These types own the *parsed* form of a sub-chunk payload — the outer
// chunk header (signature + version + size) is consumed by the data
// layer and never reaches this module. `from_bytes` takes the chunk's
// content bytes; `to_bytes` rebuilds them for serialization.
//================================================================================

/// Parsed form of a `tgrf` tag-reference chunk payload.
///
/// A null reference (payload shorter than the 4-byte group tag) is
/// represented as `None`; a resolved reference is `Some((group_tag,
/// path))`, where `path` is the UTF-8 tag path relative to the tag
/// root. No explicit terminator — the path fills the rest of the chunk.
#[derive(Debug)]
pub struct TagReferenceData {
    pub group_tag_and_name: Option<(u32, String)>,
}

impl TagReferenceData {
    /// Parse a `tgrf` payload (header already consumed). The 4-byte
    /// group tag prefix is read in the file's wire endian; the rest
    /// is the UTF-8 path. Bad UTF-8 in the path is decoded lossily
    /// (U+FFFD replacement) — preserves well-formed-input roundtrip
    /// while making the parser safe to feed corrupt bytes.
    pub fn from_bytes(payload: &[u8], endian: Endian) -> Self {
        if payload.len() < 4 {
            return Self { group_tag_and_name: None };
        }
        let b = [payload[0], payload[1], payload[2], payload[3]];
        let group_tag = match endian {
            Endian::Le => u32::from_le_bytes(b),
            Endian::Be => u32::from_be_bytes(b),
        };
        // The on-disk path is NUL-terminated; strip the terminator so the
        // stored name is the clean path (re-added by `to_bytes`). Keeps
        // displays/diffs free of an embedded `\0` while staying byte-exact.
        let name = String::from_utf8_lossy(&payload[4..])
            .trim_end_matches('\0')
            .to_owned();
        Self { group_tag_and_name: Some((group_tag, name)) }
    }

    /// Serialize back to a `tgrf` payload (caller writes the header). The
    /// group FOURCC is emitted in the file's wire `endian`, mirroring
    /// [`Self::from_bytes`], so a re-serialized reference round-trips instead
    /// of reversing the tag class. Halo CE tags are big-endian and store the
    /// reference group forward on disk (`mod2`); a hardcoded little-endian
    /// write turned it into `2dom` the moment a reference was edited, which
    /// corrupted the on-disk tag and made reverse-dependency lookups miss. The
    /// path is re-emitted NUL-terminated (the terminator stripped by
    /// [`Self::from_bytes`]).
    pub fn to_bytes(&self, endian: Endian) -> Vec<u8> {
        match &self.group_tag_and_name {
            None => Vec::new(),
            Some((group_tag, name)) => {
                let mut bytes = Vec::with_capacity(5 + name.len());
                let group = match endian {
                    Endian::Le => group_tag.to_le_bytes(),
                    Endian::Be => group_tag.to_be_bytes(),
                };
                bytes.extend_from_slice(&group);
                bytes.extend_from_slice(name.as_bytes());
                bytes.push(0);
                bytes
            }
        }
    }
}

/// Parsed form of a `ti][` api-interop chunk payload.
///
/// The 12-byte shape matches BCS's `s_tag_interop { descriptor,
/// address, definition_address }`. `address` is the runtime pointer
/// slot — BCS's writer zeroes it to `UINT_MAX` on save, so the
/// canonical on-disk reset pattern is `{ 0, 0xFFFFFFFF, 0 }`.
///
/// `raw` preserves the payload verbatim so non-12-byte variants
/// (if any exist in other games) still roundtrip byte-exactly; the
/// three named fields are convenience accessors over the common case.
/// `endian` is the wire endian the payload was read in — needed to
/// decode the `u32` accessors for X360 / BE tags.
#[derive(Debug)]
pub struct ApiInteropData {
    pub raw: Vec<u8>,
    pub endian: Endian,
}

impl ApiInteropData {
    /// Parse a `ti][` payload (header already consumed). `endian` is
    /// the file's wire endian; the raw bytes are preserved verbatim.
    pub fn from_bytes(payload: &[u8], endian: Endian) -> Self {
        Self { raw: payload.to_vec(), endian }
    }

    /// Serialize back to a `ti][` payload (caller writes the header).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.raw.clone()
    }

    /// Reset pattern BCS writes on save: `{ descriptor=0,
    /// address=UINT_MAX, definition_address=0 }`. 12 bytes, LE.
    pub fn reset() -> Self {
        let mut raw = Vec::with_capacity(12);
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&u32::MAX.to_le_bytes());
        raw.extend_from_slice(&0u32.to_le_bytes());
        Self { raw, endian: Endian::Le }
    }

    /// `descriptor` field (u32 at offset 0), if the payload has the
    /// canonical 12-byte shape.
    pub fn descriptor(&self) -> Option<u32> {
        self.u32_at(0)
    }

    /// `address` field (u32 at offset 4), if the payload has the
    /// canonical 12-byte shape.
    pub fn address(&self) -> Option<u32> {
        self.u32_at(4)
    }

    /// `definition_address` field (u32 at offset 8), if the payload
    /// has the canonical 12-byte shape.
    pub fn definition_address(&self) -> Option<u32> {
        self.u32_at(8)
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        if self.raw.len() < 12 {
            return None;
        }
        let b = [
            self.raw[off],
            self.raw[off + 1],
            self.raw[off + 2],
            self.raw[off + 3],
        ];
        Some(match self.endian {
            Endian::Le => u32::from_le_bytes(b),
            Endian::Be => u32::from_be_bytes(b),
        })
    }
}

/// Parsed form of a `tgsi` string-id chunk payload. Used for both
/// [`TagFieldType::StringId`] and [`TagFieldType::OldStringId`].
/// Empty content represents `string_id::NONE`.
#[derive(Debug)]
pub struct StringIdData {
    pub string: String,
}

impl StringIdData {
    /// Parse a `tgsi` payload (header already consumed).
    pub fn from_bytes(payload: &[u8]) -> Self {
        Self { string: String::from_utf8_lossy(payload).to_string() }
    }

    /// Serialize back to a `tgsi` payload (caller writes the header).
    pub fn to_bytes(&self) -> Vec<u8> {
        self.string.as_bytes().to_vec()
    }
}

//================================================================================
// TagFieldData
//================================================================================

/// Parsed per-field value.
///
/// Carries only *values* — things that parse to a single Rust datum
/// displayable/editable on its own. Container field types (struct,
/// block, array, pageable_resource) are deliberately absent: they're
/// navigated via the raw-data + sub-chunks tree directly, since
/// materializing them here would duplicate subtree storage.
/// `deserialize_field` returns `None` for container / pad / skip /
/// explanation / terminator / api_interop / vertex_buffer fields.
///
/// **Enum and flags variants** carry the raw integer *and* resolved
/// name(s). Names are informational only — `serialize_field` writes
/// just the integer back to `raw_data`.
///
/// **Sub-chunk-bearing variants** (string_id, old_string_id,
/// tag_reference, data) carry their parsed payload. Sub-chunk
/// outer-header handling stays in [`crate::data`].
#[derive(Debug)]
pub enum TagFieldData {
    // Strings (fixed-size, null-padded on the wire).
    /// 32-byte null-padded UTF-8 buffer.
    String(String),
    /// 256-byte null-padded UTF-8 buffer.
    LongString(String),

    // Sub-chunk leaves.
    StringId(StringIdData),
    OldStringId(StringIdData),
    TagReference(TagReferenceData),
    Data(Vec<u8>),
    ApiInterop(ApiInteropData),

    // Integers.
    CharInteger(i8),
    ShortInteger(i16),
    LongInteger(i32),
    Int64Integer(i64),
    ByteInteger(u8),
    WordInteger(u16),
    DwordInteger(u32),
    QwordInteger(u64),
    Tag(u32),

    // Enums: raw value + resolved variant name (None if out of range
    // or no matching string_list entry).
    CharEnum { value: i8, name: Option<String> },
    ShortEnum { value: i16, name: Option<String> },
    LongEnum { value: i32, name: Option<String> },

    // Flags: raw value + names of set bits (bit index + display name).
    // Unset bits are omitted; bits past the string_list's `count` are
    // preserved in `value` but have no corresponding name entry.
    ByteFlags { value: u8, names: Vec<(u32, String)> },
    WordFlags { value: u16, names: Vec<(u32, String)> },
    LongFlags { value: i32, names: Vec<(u32, String)> },

    // Block flags: value only. TODO: resolve per-bit names against the
    // referenced block's current element count at parse time.
    ByteBlockFlags(u8),
    WordBlockFlags(u16),
    LongBlockFlags(i32),

    // Block indices.
    CharBlockIndex(i8),
    CustomCharBlockIndex(i8),
    ShortBlockIndex(i16),
    CustomShortBlockIndex(i16),
    LongBlockIndex(i32),
    CustomLongBlockIndex(i32),

    // Floats.
    Angle(f32),
    Real(f32),
    RealSlider(f32),
    RealFraction(f32),

    // Math composites.
    Point2d(math::Point2d),
    Rectangle2d(math::Rectangle2d),
    RealPoint2d(math::RealPoint2d),
    RealPoint3d(math::RealPoint3d),
    RealVector2d(math::RealVector2d),
    RealVector3d(math::RealVector3d),
    RealQuaternion(math::RealQuaternion),
    RealEulerAngles2d(math::RealEulerAngles2d),
    RealEulerAngles3d(math::RealEulerAngles3d),
    RealPlane2d(math::RealPlane2d),
    RealPlane3d(math::RealPlane3d),

    // Colors.
    RgbColor(math::RgbColor),
    ArgbColor(math::ArgbColor),
    RealRgbColor(math::RealRgbColor),
    RealArgbColor(math::RealArgbColor),
    RealHsvColor(math::RealHsvColor),
    RealAhsvColor(math::RealAhsvColor),

    // Bounds.
    ShortIntegerBounds(math::ShortBounds),
    AngleBounds(math::AngleBounds),
    RealBounds(math::RealBounds),
    FractionBounds(math::FractionBounds),

    // Opaque.
    Custom(Vec<u8>),
}

impl TagFieldData {
    /// Read a single bit from a flags-shaped variant (including
    /// block-flags). Returns `None` for variants that aren't flags.
    pub fn flag_bit(&self, bit: u32) -> Option<bool> {
        let raw = match self {
            TagFieldData::ByteFlags { value, .. } => *value as u64,
            TagFieldData::WordFlags { value, .. } => *value as u64,
            TagFieldData::LongFlags { value, .. } => *value as u32 as u64,
            TagFieldData::ByteBlockFlags(v) => *v as u64,
            TagFieldData::WordBlockFlags(v) => *v as u64,
            TagFieldData::LongBlockFlags(v) => *v as u32 as u64,
            _ => return None,
        };
        Some((raw & (1u64 << bit)) != 0)
    }

    /// Set or clear a single bit on a flags-shaped variant (including
    /// block-flags). Returns `true` on success, `false` if `self`
    /// isn't a flags variant. The bit is mutated in place; the
    /// variant's `names` field (if any) is left untouched — callers
    /// re-derive it by re-parsing if they need accurate names after
    /// mutation.
    pub fn set_flag_bit(&mut self, bit: u32, on: bool) -> bool {
        let mask = 1u64 << bit;
        let apply = |raw: u64| -> u64 { if on { raw | mask } else { raw & !mask } };
        match self {
            TagFieldData::ByteFlags { value, .. } => {
                *value = apply(*value as u64) as u8;
                true
            }
            TagFieldData::WordFlags { value, .. } => {
                *value = apply(*value as u64) as u16;
                true
            }
            TagFieldData::LongFlags { value, .. } => {
                *value = apply(*value as u32 as u64) as u32 as i32;
                true
            }
            TagFieldData::ByteBlockFlags(v) => {
                *v = apply(*v as u64) as u8;
                true
            }
            TagFieldData::WordBlockFlags(v) => {
                *v = apply(*v as u64) as u16;
                true
            }
            TagFieldData::LongBlockFlags(v) => {
                *v = apply(*v as u32 as u64) as u32 as i32;
                true
            }
            _ => false,
        }
    }
}

//================================================================================
// Display
//================================================================================

//================================================================================
// Raw-data read/write helpers.
//
// Multi-byte primitives slice into `raw_data` and dispatch on `endian`
// at read time so X360 (BE) tags work without an upfront byteswap of
// raw_data. The 1-byte helpers don't take endian — endianness is
// meaningless for a single byte.
//
// Writers always emit little-endian because we never serialize a BE
// tag back to disk (Phase 1 set scope to read-only for X360). If that
// ever changes, mirror the reader dispatch in the writer.
//================================================================================

#[inline] fn read_i8(raw: &[u8], o: usize) -> i8 { raw[o] as i8 }
#[inline] fn read_u8(raw: &[u8], o: usize) -> u8 { raw[o] }
#[inline] fn read_i16(raw: &[u8], o: usize, e: Endian) -> i16 {
    let b = [raw[o], raw[o + 1]];
    match e { Endian::Le => i16::from_le_bytes(b), Endian::Be => i16::from_be_bytes(b) }
}
#[inline] fn read_u16(raw: &[u8], o: usize, e: Endian) -> u16 {
    let b = [raw[o], raw[o + 1]];
    match e { Endian::Le => u16::from_le_bytes(b), Endian::Be => u16::from_be_bytes(b) }
}
#[inline] fn read_i32(raw: &[u8], o: usize, e: Endian) -> i32 {
    let b: [u8; 4] = raw[o..o + 4].try_into().unwrap();
    match e { Endian::Le => i32::from_le_bytes(b), Endian::Be => i32::from_be_bytes(b) }
}
#[inline] fn read_u32(raw: &[u8], o: usize, e: Endian) -> u32 {
    let b: [u8; 4] = raw[o..o + 4].try_into().unwrap();
    match e { Endian::Le => u32::from_le_bytes(b), Endian::Be => u32::from_be_bytes(b) }
}
#[inline] fn read_i64(raw: &[u8], o: usize, e: Endian) -> i64 {
    let b: [u8; 8] = raw[o..o + 8].try_into().unwrap();
    match e { Endian::Le => i64::from_le_bytes(b), Endian::Be => i64::from_be_bytes(b) }
}
#[inline] fn read_u64(raw: &[u8], o: usize, e: Endian) -> u64 {
    let b: [u8; 8] = raw[o..o + 8].try_into().unwrap();
    match e { Endian::Le => u64::from_le_bytes(b), Endian::Be => u64::from_be_bytes(b) }
}
#[inline] fn read_f32(raw: &[u8], o: usize, e: Endian) -> f32 {
    let b: [u8; 4] = raw[o..o + 4].try_into().unwrap();
    match e { Endian::Le => f32::from_le_bytes(b), Endian::Be => f32::from_be_bytes(b) }
}

#[inline] fn write_i8(raw: &mut [u8], o: usize, v: i8) { raw[o] = v as u8 }
#[inline] fn write_u8(raw: &mut [u8], o: usize, v: u8) { raw[o] = v }
#[inline] fn write_i16(raw: &mut [u8], o: usize, v: i16, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 2].copy_from_slice(&b);
}
#[inline] fn write_u16(raw: &mut [u8], o: usize, v: u16, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 2].copy_from_slice(&b);
}
#[inline] fn write_i32(raw: &mut [u8], o: usize, v: i32, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 4].copy_from_slice(&b);
}
#[inline] fn write_u32(raw: &mut [u8], o: usize, v: u32, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 4].copy_from_slice(&b);
}
#[inline] fn write_i64(raw: &mut [u8], o: usize, v: i64, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 8].copy_from_slice(&b);
}
#[inline] fn write_u64(raw: &mut [u8], o: usize, v: u64, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 8].copy_from_slice(&b);
}
#[inline] fn write_f32(raw: &mut [u8], o: usize, v: f32, e: Endian) {
    let b = match e { Endian::Le => v.to_le_bytes(), Endian::Be => v.to_be_bytes() };
    raw[o..o + 4].copy_from_slice(&b);
}

/// Read a fixed-size null-padded UTF-8 string out of `bytes`. Stops at
/// the first NUL; invalid UTF-8 sequences are replaced.
fn decode_null_padded_string(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Write `s` into a fixed-size buffer, truncating to fit and zero-padding
/// the tail.
fn encode_null_padded_string(s: &str, dest: &mut [u8]) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(dest.len());
    dest[..n].copy_from_slice(&bytes[..n]);
    for slot in &mut dest[n..] {
        *slot = 0;
    }
}

//================================================================================
// Enum / flags name resolution.
//================================================================================

/// Resolve an enum value to its variant name via the layout's
/// `string_lists[field.definition]`. Returns `None` if `value` is out
/// of range or the string list / offset is missing.
fn resolve_enum_name(layout: &TagLayout, field: &TagFieldLayout, value: i64) -> Option<String> {
    let string_list = layout.string_lists.get(field.definition as usize)?;
    if value < 0 || (value as u32) >= string_list.count {
        return None;
    }
    let offset_index = (string_list.first + value as u32) as usize;
    let string_offset = *layout.string_offsets.get(offset_index)?;
    layout.get_string(string_offset).map(str::to_string)
}

/// Reverse of the name-resolution step: look up the bit index of a
/// flags value whose variant name matches `name` (case-sensitive).
/// Returns `None` if the field has no string list or no bit with that
/// name exists. Used by the `flag` CLI command to map a user-supplied
/// flag name to the bit it should set/clear.
pub(crate) fn find_flag_bit(layout: &TagLayout, field: &TagFieldLayout, name: &str) -> Option<u32> {
    let string_list = layout.string_lists.get(field.definition as usize)?;
    for bit in 0..string_list.count {
        let offset_index = (string_list.first + bit) as usize;
        let Some(&string_offset) = layout.string_offsets.get(offset_index) else { continue };
        if layout.get_string(string_offset) == Some(name) {
            return Some(bit);
        }
    }
    None
}

/// Render a 4-byte group tag (stored as a BE-packed `u32` with
/// trailing-space padding for short tags) as its ASCII form, with
/// trailing spaces and NULs stripped. Matches the convention used
/// throughout the tag-file wire format. Inverse of [`parse_group_tag`].
pub fn format_group_tag(tag: u32) -> String {
    let bytes = tag.to_be_bytes();
    String::from_utf8_lossy(&bytes)
        .trim_end_matches(['\0', ' '])
        .to_string()
}

/// Parse an ASCII group-tag string (1-4 chars, e.g. `"bipd"` or
/// `"mo"`) into the BE-packed `u32` form used on disk. Short tags are
/// right-padded with spaces to 4 bytes. Returns `None` if the input
/// is longer than 4 bytes. Inverse of [`format_group_tag`].
pub fn parse_group_tag(s: &str) -> Option<u32> {
    let bytes = s.as_bytes();
    if bytes.len() > 4 {
        return None;
    }
    let mut padded = [b' '; 4];
    padded[..bytes.len()].copy_from_slice(bytes);
    Some(u32::from_be_bytes(padded))
}

/// Iterate the option names of an enum or flags field (indexed by
/// `field.definition` into `layout.string_lists`). Yields `""` for
/// empty / missing entries. Returns an empty iterator for fields
/// whose `definition` doesn't reference a valid string list
/// (e.g. block-flags, which index into `block_layouts`).
pub(crate) fn field_option_names<'a>(
    layout: &'a TagLayout,
    field: &TagFieldLayout,
) -> impl Iterator<Item = &'a str> + 'a {
    let string_list = layout.string_lists.get(field.definition as usize);
    let range = match string_list {
        Some(sl) => sl.first..sl.first + sl.count,
        None => 0..0,
    };
    range.map(move |i| {
        layout
            .string_offsets
            .get(i as usize)
            .and_then(|&off| layout.get_string(off))
            .unwrap_or("")
    })
}

/// Resolve the names of the set bits of a flags value. Unset bits and
/// bits past the string list's `count` are omitted — the raw value in
/// `TagFieldData` preserves them regardless.
fn resolve_flag_names(
    layout: &TagLayout,
    field: &TagFieldLayout,
    value: u64,
    total_bits: u32,
) -> Vec<(u32, String)> {
    let mut names = Vec::new();
    let Some(string_list) = layout.string_lists.get(field.definition as usize) else {
        return names;
    };
    for bit in 0..total_bits {
        if (value & (1u64 << bit)) == 0 {
            continue;
        }
        if bit >= string_list.count {
            continue;
        }
        let offset_index = (string_list.first + bit) as usize;
        let Some(&string_offset) = layout.string_offsets.get(offset_index) else { continue };
        let Some(name) = layout.get_string(string_offset) else { continue };
        names.push((bit, name.to_string()));
    }
    names
}

//================================================================================
// deserialize_field / serialize_field
//================================================================================

/// Parse the value of a single field.
///
/// `raw_struct` is the slice of the enclosing block's `raw_data`
/// covering exactly this struct's bytes; primitive values slice
/// directly out of it at `field.offset`. Sub-chunk leaf fields consume
/// `sub_chunk` — the caller is responsible for locating the matching
/// [`TagSubChunkContent`] entry in the containing struct's
/// `sub_chunks`.
///
/// Returns `None` for field types that don't carry a value
/// (pad/skip/explanation/terminator, container types handled via
/// sub-chunks navigation, and the not-yet-modeled api_interop /
/// vertex_buffer). Container fields live in the sub-chunks tree
/// already — the caller walks them directly.
pub(crate) fn deserialize_field(
    layout: &TagLayout,
    field: &TagFieldLayout,
    raw_struct: &[u8],
    sub_chunk: Option<&TagSubChunkContent>,
    endian: Endian,
) -> Option<TagFieldData> {
    let offset = field.offset as usize;

    // Truncated (clamped) element: a field whose declared bytes run past
    // the on-disk element decodes as ABSENT (None) instead of panicking.
    // This mirrors the Halo 2 engine, which reads each block element at
    // its field-set's definition size and ZERO-FILLS the bytes an
    // on-disk element doesn't carry when `stored_size < field_set.size`
    // (read_recursive_tag_block / read_field_set_definition in
    // tag_group_loading.cpp — verified against MCC tool.exe). Within a
    // single field-set version the layout is fixed, so a field past the
    // on-disk tail is genuinely absent; a consumer's `unwrap_or(default)`
    // then yields the same value the engine's zero-fill would. CE is
    // unaffected (headerless — element size always equals the schema).
    let need = layout.field_types[field.type_index as usize].size as usize;
    if offset + need > raw_struct.len() {
        return None;
    }

    match field.field_type {
        // No value.
        TagFieldType::Unknown
        | TagFieldType::Pad
        | TagFieldType::UselessPad
        | TagFieldType::Skip
        | TagFieldType::Explanation
        | TagFieldType::Terminator
        | TagFieldType::NonCacheRuntimeValue => None,

        // Containers — navigated via sub_chunks directly.
        TagFieldType::Struct
        | TagFieldType::Block
        | TagFieldType::Array
        | TagFieldType::PageableResource => None,

        // Not yet modeled (raw-only — preserved in raw_data for roundtrip).
        TagFieldType::VertexBuffer | TagFieldType::Pointer | TagFieldType::RealMatrix3x3 => None,

        // Strings (null-padded in raw_data).
        TagFieldType::String => Some(TagFieldData::String(
            decode_null_padded_string(&raw_struct[offset..offset + 32]),
        )),
        TagFieldType::LongString => Some(TagFieldData::LongString(
            decode_null_padded_string(&raw_struct[offset..offset + 256]),
        )),

        // Integers.
        TagFieldType::CharInteger => Some(TagFieldData::CharInteger(read_i8(raw_struct, offset))),
        TagFieldType::ShortInteger => Some(TagFieldData::ShortInteger(read_i16(raw_struct, offset, endian))),
        TagFieldType::LongInteger => Some(TagFieldData::LongInteger(read_i32(raw_struct, offset, endian))),
        TagFieldType::Int64Integer => Some(TagFieldData::Int64Integer(read_i64(raw_struct, offset, endian))),
        TagFieldType::ByteInteger => Some(TagFieldData::ByteInteger(read_u8(raw_struct, offset))),
        TagFieldType::WordInteger => Some(TagFieldData::WordInteger(read_u16(raw_struct, offset, endian))),
        TagFieldType::DwordInteger => Some(TagFieldData::DwordInteger(read_u32(raw_struct, offset, endian))),
        TagFieldType::QwordInteger => Some(TagFieldData::QwordInteger(read_u64(raw_struct, offset, endian))),
        TagFieldType::Tag => Some(TagFieldData::Tag(read_u32(raw_struct, offset, endian))),

        // Enums.
        TagFieldType::CharEnum => {
            let value = read_i8(raw_struct, offset);
            Some(TagFieldData::CharEnum { value, name: resolve_enum_name(layout, field, value as i64) })
        }
        TagFieldType::ShortEnum => {
            let value = read_i16(raw_struct, offset, endian);
            Some(TagFieldData::ShortEnum { value, name: resolve_enum_name(layout, field, value as i64) })
        }
        TagFieldType::LongEnum => {
            let value = read_i32(raw_struct, offset, endian);
            Some(TagFieldData::LongEnum { value, name: resolve_enum_name(layout, field, value as i64) })
        }

        // Flags.
        TagFieldType::ByteFlags => {
            let value = read_u8(raw_struct, offset);
            Some(TagFieldData::ByteFlags { value, names: resolve_flag_names(layout, field, value as u64, 8) })
        }
        TagFieldType::WordFlags => {
            let value = read_u16(raw_struct, offset, endian);
            Some(TagFieldData::WordFlags { value, names: resolve_flag_names(layout, field, value as u64, 16) })
        }
        TagFieldType::LongFlags => {
            let value = read_i32(raw_struct, offset, endian);
            Some(TagFieldData::LongFlags { value, names: resolve_flag_names(layout, field, value as u32 as u64, 32) })
        }

        // Block flags — value only for now.
        TagFieldType::ByteBlockFlags => Some(TagFieldData::ByteBlockFlags(read_u8(raw_struct, offset))),
        TagFieldType::WordBlockFlags => Some(TagFieldData::WordBlockFlags(read_u16(raw_struct, offset, endian))),
        TagFieldType::LongBlockFlags => Some(TagFieldData::LongBlockFlags(read_i32(raw_struct, offset, endian))),

        // Block indices.
        TagFieldType::CharBlockIndex => Some(TagFieldData::CharBlockIndex(read_i8(raw_struct, offset))),
        TagFieldType::CustomCharBlockIndex => Some(TagFieldData::CustomCharBlockIndex(read_i8(raw_struct, offset))),
        TagFieldType::ShortBlockIndex => Some(TagFieldData::ShortBlockIndex(read_i16(raw_struct, offset, endian))),
        TagFieldType::CustomShortBlockIndex => Some(TagFieldData::CustomShortBlockIndex(read_i16(raw_struct, offset, endian))),
        TagFieldType::LongBlockIndex => Some(TagFieldData::LongBlockIndex(read_i32(raw_struct, offset, endian))),
        TagFieldType::CustomLongBlockIndex => Some(TagFieldData::CustomLongBlockIndex(read_i32(raw_struct, offset, endian))),

        // Floats.
        TagFieldType::Angle => Some(TagFieldData::Angle(read_f32(raw_struct, offset, endian))),
        TagFieldType::Real => Some(TagFieldData::Real(read_f32(raw_struct, offset, endian))),
        TagFieldType::RealSlider => Some(TagFieldData::RealSlider(read_f32(raw_struct, offset, endian))),
        TagFieldType::RealFraction => Some(TagFieldData::RealFraction(read_f32(raw_struct, offset, endian))),

        // Math composites.
        TagFieldType::Point2d => Some(TagFieldData::Point2d(math::Point2d {
            x: read_i16(raw_struct, offset, endian),
            y: read_i16(raw_struct, offset + 2, endian),
        })),
        TagFieldType::Rectangle2d => Some(TagFieldData::Rectangle2d(math::Rectangle2d {
            top: read_i16(raw_struct, offset, endian),
            left: read_i16(raw_struct, offset + 2, endian),
            bottom: read_i16(raw_struct, offset + 4, endian),
            right: read_i16(raw_struct, offset + 6, endian),
        })),
        TagFieldType::RealPoint2d => Some(TagFieldData::RealPoint2d(math::RealPoint2d {
            x: read_f32(raw_struct, offset, endian),
            y: read_f32(raw_struct, offset + 4, endian),
        })),
        TagFieldType::RealPoint3d => Some(TagFieldData::RealPoint3d(math::RealPoint3d {
            x: read_f32(raw_struct, offset, endian),
            y: read_f32(raw_struct, offset + 4, endian),
            z: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealVector2d => Some(TagFieldData::RealVector2d(math::RealVector2d {
            i: read_f32(raw_struct, offset, endian),
            j: read_f32(raw_struct, offset + 4, endian),
        })),
        TagFieldType::RealVector3d => Some(TagFieldData::RealVector3d(math::RealVector3d {
            i: read_f32(raw_struct, offset, endian),
            j: read_f32(raw_struct, offset + 4, endian),
            k: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealQuaternion => Some(TagFieldData::RealQuaternion(math::RealQuaternion {
            i: read_f32(raw_struct, offset, endian),
            j: read_f32(raw_struct, offset + 4, endian),
            k: read_f32(raw_struct, offset + 8, endian),
            w: read_f32(raw_struct, offset + 12, endian),
        })),
        TagFieldType::RealEulerAngles2d => Some(TagFieldData::RealEulerAngles2d(math::RealEulerAngles2d {
            yaw: read_f32(raw_struct, offset, endian),
            pitch: read_f32(raw_struct, offset + 4, endian),
        })),
        TagFieldType::RealEulerAngles3d => Some(TagFieldData::RealEulerAngles3d(math::RealEulerAngles3d {
            yaw: read_f32(raw_struct, offset, endian),
            pitch: read_f32(raw_struct, offset + 4, endian),
            roll: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealPlane2d => Some(TagFieldData::RealPlane2d(math::RealPlane2d {
            i: read_f32(raw_struct, offset, endian),
            j: read_f32(raw_struct, offset + 4, endian),
            d: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealPlane3d => Some(TagFieldData::RealPlane3d(math::RealPlane3d {
            i: read_f32(raw_struct, offset, endian),
            j: read_f32(raw_struct, offset + 4, endian),
            k: read_f32(raw_struct, offset + 8, endian),
            d: read_f32(raw_struct, offset + 12, endian),
        })),

        // Colors.
        TagFieldType::RgbColor => Some(TagFieldData::RgbColor(math::RgbColor(read_u32(raw_struct, offset, endian)))),
        TagFieldType::ArgbColor => Some(TagFieldData::ArgbColor(math::ArgbColor(read_u32(raw_struct, offset, endian)))),
        TagFieldType::RealRgbColor => Some(TagFieldData::RealRgbColor(math::RealRgbColor {
            red: read_f32(raw_struct, offset, endian),
            green: read_f32(raw_struct, offset + 4, endian),
            blue: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealArgbColor => Some(TagFieldData::RealArgbColor(math::RealArgbColor {
            alpha: read_f32(raw_struct, offset, endian),
            red: read_f32(raw_struct, offset + 4, endian),
            green: read_f32(raw_struct, offset + 8, endian),
            blue: read_f32(raw_struct, offset + 12, endian),
        })),
        TagFieldType::RealHsvColor => Some(TagFieldData::RealHsvColor(math::RealHsvColor {
            hue: read_f32(raw_struct, offset, endian),
            saturation: read_f32(raw_struct, offset + 4, endian),
            value: read_f32(raw_struct, offset + 8, endian),
        })),
        TagFieldType::RealAhsvColor => Some(TagFieldData::RealAhsvColor(math::RealAhsvColor {
            alpha: read_f32(raw_struct, offset, endian),
            hue: read_f32(raw_struct, offset + 4, endian),
            saturation: read_f32(raw_struct, offset + 8, endian),
            value: read_f32(raw_struct, offset + 12, endian),
        })),

        // Bounds.
        TagFieldType::ShortIntegerBounds => Some(TagFieldData::ShortIntegerBounds(math::ShortBounds {
            lower: read_i16(raw_struct, offset, endian),
            upper: read_i16(raw_struct, offset + 2, endian),
        })),
        TagFieldType::AngleBounds => Some(TagFieldData::AngleBounds(math::AngleBounds {
            lower: read_f32(raw_struct, offset, endian),
            upper: read_f32(raw_struct, offset + 4, endian),
        })),
        TagFieldType::RealBounds => Some(TagFieldData::RealBounds(math::RealBounds {
            lower: read_f32(raw_struct, offset, endian),
            upper: read_f32(raw_struct, offset + 4, endian),
        })),
        TagFieldType::FractionBounds => Some(TagFieldData::FractionBounds(math::FractionBounds {
            lower: read_f32(raw_struct, offset, endian),
            upper: read_f32(raw_struct, offset + 4, endian),
        })),

        // Custom: variable-size opaque bytes. Size comes from the
        // field_type's declared `size`.
        TagFieldType::Custom => {
            let size = layout.field_types[field.type_index as usize].size as usize;
            Some(TagFieldData::Custom(raw_struct[offset..offset + size].to_vec()))
        }

        // Sub-chunk leaves. A missing sub-chunk returns `None` to
        // match the graceful fall-back the container accessors
        // (`as_block` / `as_array` / `as_struct`) already use —
        // monolithic-hydrated resources have empty `sub_chunks`
        // lists, and we'd rather have the walker show a blank value
        // than abort the whole inspection.
        TagFieldType::StringId => sub_chunk.and_then(|c| match c {
            TagSubChunkContent::StringId(payload) => {
                Some(TagFieldData::StringId(StringIdData::from_bytes(payload)))
            }
            _ => None,
        }),
        TagFieldType::OldStringId => sub_chunk.and_then(|c| match c {
            TagSubChunkContent::OldStringId(payload) => {
                Some(TagFieldData::OldStringId(StringIdData::from_bytes(payload)))
            }
            _ => None,
        }),
        TagFieldType::TagReference => sub_chunk.and_then(|c| match c {
            TagSubChunkContent::TagReference(payload) => Some(TagFieldData::TagReference(
                TagReferenceData::from_bytes(payload, endian),
            )),
            _ => None,
        }),
        TagFieldType::Data => sub_chunk.and_then(|c| match c {
            TagSubChunkContent::Data(payload) => Some(TagFieldData::Data(payload.clone())),
            _ => None,
        }),
        TagFieldType::ApiInterop => sub_chunk.and_then(|c| match c {
            TagSubChunkContent::ApiInterop(payload) => Some(TagFieldData::ApiInterop(
                ApiInteropData::from_bytes(payload, endian),
            )),
            _ => None,
        }),
    }
}

/// Serialize a [`TagFieldData`] back to its on-disk form.
///
/// Primitive/enum/flag/string/math variants write their bytes into
/// `raw_struct` at `field.offset` and return `None`. Sub-chunk leaf
/// variants (string_id, old_string_id, tag_reference, data) produce a
/// new [`TagSubChunkContent`] that the caller swaps into the owning
/// struct's `sub_chunks`. Names in enum/flag variants are ignored —
/// only the raw value is written.
///
/// Panics if `value`'s variant doesn't match `field.field_type` — the
/// caller is responsible for only passing compatible pairs.
pub(crate) fn serialize_field(
    field: &TagFieldLayout,
    value: &TagFieldData,
    raw_struct: &mut [u8],
    endian: Endian,
) -> Option<TagSubChunkContent> {
    let offset = field.offset as usize;

    match value {
        TagFieldData::String(s) => {
            encode_null_padded_string(s, &mut raw_struct[offset..offset + 32]);
            None
        }
        TagFieldData::LongString(s) => {
            encode_null_padded_string(s, &mut raw_struct[offset..offset + 256]);
            None
        }

        TagFieldData::CharInteger(v) => { write_i8(raw_struct, offset, *v); None }
        TagFieldData::ShortInteger(v) => { write_i16(raw_struct, offset, *v, endian); None }
        TagFieldData::LongInteger(v) => { write_i32(raw_struct, offset, *v, endian); None }
        TagFieldData::Int64Integer(v) => { write_i64(raw_struct, offset, *v, endian); None }
        TagFieldData::ByteInteger(v) => { write_u8(raw_struct, offset, *v); None }
        TagFieldData::WordInteger(v) => { write_u16(raw_struct, offset, *v, endian); None }
        TagFieldData::DwordInteger(v) => { write_u32(raw_struct, offset, *v, endian); None }
        TagFieldData::QwordInteger(v) => { write_u64(raw_struct, offset, *v, endian); None }
        TagFieldData::Tag(v) => { write_u32(raw_struct, offset, *v, endian); None }

        TagFieldData::CharEnum { value, .. } => { write_i8(raw_struct, offset, *value); None }
        TagFieldData::ShortEnum { value, .. } => { write_i16(raw_struct, offset, *value, endian); None }
        TagFieldData::LongEnum { value, .. } => { write_i32(raw_struct, offset, *value, endian); None }

        TagFieldData::ByteFlags { value, .. } => { write_u8(raw_struct, offset, *value); None }
        TagFieldData::WordFlags { value, .. } => { write_u16(raw_struct, offset, *value, endian); None }
        TagFieldData::LongFlags { value, .. } => { write_i32(raw_struct, offset, *value, endian); None }

        TagFieldData::ByteBlockFlags(v) => { write_u8(raw_struct, offset, *v); None }
        TagFieldData::WordBlockFlags(v) => { write_u16(raw_struct, offset, *v, endian); None }
        TagFieldData::LongBlockFlags(v) => { write_i32(raw_struct, offset, *v, endian); None }

        TagFieldData::CharBlockIndex(v) => { write_i8(raw_struct, offset, *v); None }
        TagFieldData::CustomCharBlockIndex(v) => { write_i8(raw_struct, offset, *v); None }
        TagFieldData::ShortBlockIndex(v) => { write_i16(raw_struct, offset, *v, endian); None }
        TagFieldData::CustomShortBlockIndex(v) => { write_i16(raw_struct, offset, *v, endian); None }
        TagFieldData::LongBlockIndex(v) => { write_i32(raw_struct, offset, *v, endian); None }
        TagFieldData::CustomLongBlockIndex(v) => { write_i32(raw_struct, offset, *v, endian); None }

        TagFieldData::Angle(v) => { write_f32(raw_struct, offset, *v, endian); None }
        TagFieldData::Real(v) => { write_f32(raw_struct, offset, *v, endian); None }
        TagFieldData::RealSlider(v) => { write_f32(raw_struct, offset, *v, endian); None }
        TagFieldData::RealFraction(v) => { write_f32(raw_struct, offset, *v, endian); None }

        TagFieldData::Point2d(p) => {
            write_i16(raw_struct, offset, p.x, endian);
            write_i16(raw_struct, offset + 2, p.y, endian);
            None
        }
        TagFieldData::Rectangle2d(r) => {
            write_i16(raw_struct, offset, r.top, endian);
            write_i16(raw_struct, offset + 2, r.left, endian);
            write_i16(raw_struct, offset + 4, r.bottom, endian);
            write_i16(raw_struct, offset + 6, r.right, endian);
            None
        }
        TagFieldData::RealPoint2d(p) => {
            write_f32(raw_struct, offset, p.x, endian);
            write_f32(raw_struct, offset + 4, p.y, endian);
            None
        }
        TagFieldData::RealPoint3d(p) => {
            write_f32(raw_struct, offset, p.x, endian);
            write_f32(raw_struct, offset + 4, p.y, endian);
            write_f32(raw_struct, offset + 8, p.z, endian);
            None
        }
        TagFieldData::RealVector2d(v) => {
            write_f32(raw_struct, offset, v.i, endian);
            write_f32(raw_struct, offset + 4, v.j, endian);
            None
        }
        TagFieldData::RealVector3d(v) => {
            write_f32(raw_struct, offset, v.i, endian);
            write_f32(raw_struct, offset + 4, v.j, endian);
            write_f32(raw_struct, offset + 8, v.k, endian);
            None
        }
        TagFieldData::RealQuaternion(q) => {
            write_f32(raw_struct, offset, q.i, endian);
            write_f32(raw_struct, offset + 4, q.j, endian);
            write_f32(raw_struct, offset + 8, q.k, endian);
            write_f32(raw_struct, offset + 12, q.w, endian);
            None
        }
        TagFieldData::RealEulerAngles2d(a) => {
            write_f32(raw_struct, offset, a.yaw, endian);
            write_f32(raw_struct, offset + 4, a.pitch, endian);
            None
        }
        TagFieldData::RealEulerAngles3d(a) => {
            write_f32(raw_struct, offset, a.yaw, endian);
            write_f32(raw_struct, offset + 4, a.pitch, endian);
            write_f32(raw_struct, offset + 8, a.roll, endian);
            None
        }
        TagFieldData::RealPlane2d(p) => {
            write_f32(raw_struct, offset, p.i, endian);
            write_f32(raw_struct, offset + 4, p.j, endian);
            write_f32(raw_struct, offset + 8, p.d, endian);
            None
        }
        TagFieldData::RealPlane3d(p) => {
            write_f32(raw_struct, offset, p.i, endian);
            write_f32(raw_struct, offset + 4, p.j, endian);
            write_f32(raw_struct, offset + 8, p.k, endian);
            write_f32(raw_struct, offset + 12, p.d, endian);
            None
        }

        TagFieldData::RgbColor(c) => { write_u32(raw_struct, offset, c.0, endian); None }
        TagFieldData::ArgbColor(c) => { write_u32(raw_struct, offset, c.0, endian); None }
        TagFieldData::RealRgbColor(c) => {
            write_f32(raw_struct, offset, c.red, endian);
            write_f32(raw_struct, offset + 4, c.green, endian);
            write_f32(raw_struct, offset + 8, c.blue, endian);
            None
        }
        TagFieldData::RealArgbColor(c) => {
            write_f32(raw_struct, offset, c.alpha, endian);
            write_f32(raw_struct, offset + 4, c.red, endian);
            write_f32(raw_struct, offset + 8, c.green, endian);
            write_f32(raw_struct, offset + 12, c.blue, endian);
            None
        }
        TagFieldData::RealHsvColor(c) => {
            write_f32(raw_struct, offset, c.hue, endian);
            write_f32(raw_struct, offset + 4, c.saturation, endian);
            write_f32(raw_struct, offset + 8, c.value, endian);
            None
        }
        TagFieldData::RealAhsvColor(c) => {
            write_f32(raw_struct, offset, c.alpha, endian);
            write_f32(raw_struct, offset + 4, c.hue, endian);
            write_f32(raw_struct, offset + 8, c.saturation, endian);
            write_f32(raw_struct, offset + 12, c.value, endian);
            None
        }

        TagFieldData::ShortIntegerBounds(b) => {
            write_i16(raw_struct, offset, b.lower, endian);
            write_i16(raw_struct, offset + 2, b.upper, endian);
            None
        }
        TagFieldData::AngleBounds(b) => {
            write_f32(raw_struct, offset, b.lower, endian);
            write_f32(raw_struct, offset + 4, b.upper, endian);
            None
        }
        TagFieldData::RealBounds(b) => {
            write_f32(raw_struct, offset, b.lower, endian);
            write_f32(raw_struct, offset + 4, b.upper, endian);
            None
        }
        TagFieldData::FractionBounds(b) => {
            write_f32(raw_struct, offset, b.lower, endian);
            write_f32(raw_struct, offset + 4, b.upper, endian);
            None
        }

        TagFieldData::Custom(bytes) => {
            let size = bytes.len();
            raw_struct[offset..offset + size].copy_from_slice(bytes);
            None
        }

        // Sub-chunk leaves — produce a new TagSubChunkContent.
        TagFieldData::StringId(s) => Some(TagSubChunkContent::StringId(s.to_bytes())),
        TagFieldData::OldStringId(s) => Some(TagSubChunkContent::OldStringId(s.to_bytes())),
        TagFieldData::TagReference(r) => {
            Some(TagSubChunkContent::TagReference(r.to_bytes(endian)))
        }
        TagFieldData::Data(bytes) => Some(TagSubChunkContent::Data(bytes.clone())),
        TagFieldData::ApiInterop(i) => Some(TagSubChunkContent::ApiInterop(i.to_bytes())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A tag_reference's group FOURCC must survive a read -> edit -> write
    // round-trip in the file's wire endian. Halo CE tags are big-endian and
    // store the group forward on disk (`mod2`); serializing it little-endian
    // reversed it to `2dom` on the first edit, corrupting the tag and breaking
    // reverse-dependency lookups. H2 / modern tags are little-endian.
    #[test]
    fn tag_reference_group_round_trips_in_both_endians() {
        let mode = u32::from_be_bytes(*b"mod2");
        for (endian, on_disk_group) in
            [(Endian::Be, *b"mod2"), (Endian::Le, *b"2dom")]
        {
            // On-disk payload: group (in wire endian) + NUL-terminated path.
            let mut payload = on_disk_group.to_vec();
            payload.extend_from_slice(b"weapons\\pistol\\pistol\0");

            let parsed = TagReferenceData::from_bytes(&payload, endian);
            let (group, name) = parsed.group_tag_and_name.clone().expect("resolved");
            assert_eq!(group, mode, "group must decode to logical `mod2` ({endian:?})");
            assert_eq!(name, "weapons\\pistol\\pistol");

            // Re-serializing must reproduce the original on-disk bytes exactly.
            assert_eq!(parsed.to_bytes(endian), payload, "byte-exact round-trip ({endian:?})");
        }
    }
}
