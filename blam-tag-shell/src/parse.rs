//! Parse CLI-flavored strings into [`TagFieldData`] values.
//!
//! All textual parsing of tag-field values lives here — the library
//! offers only typed mutation via [`blam_tags::TagFieldMut::set`]. The shell
//! reads what the user typed, parses it into the right `TagFieldData`
//! variant for the field's schema type, and hands it to `set`.
//!
//! Conventions (CLI shell-only — not part of the on-disk format):
//! - `none` (case-insensitive) on `tag_reference`/`*_block_index`/
//!   `api_interop` means the canonical empty/sentinel value.
//! - Integer masks on `*_flags` / `*_block_flags` accept `0x…` hex or
//!   plain decimal.
//! - Enums accept either a variant name (case-insensitive) or a raw
//!   integer.
//! - `tag_reference` accepts the canonical filename form
//!   `<path>.<group_name>` (e.g. `objects/characters/elite/elite.biped`)
//!   or the legacy `GROUP:path` form (e.g. `hlmt:objects/...`).
//! - `api_interop` accepts `reset`/`none` or a `desc,addr,def_addr`
//!   triple of u32 values (decimal or `0x…` hex).
//!
//! Entry point: [`parse_field_value`] turns a string into a typed
//! [`TagFieldData`]. Callers do `field.set(value)` themselves once
//! they're on the mutable side — splitting the two halves keeps the
//! borrow checker happy when the caller is also working with
//! `&mut CliContext`.

use std::fmt;

use blam_tags::{
    parse_group_tag, ApiInteropData, StringIdData, TagField, TagFieldData, TagFieldType,
    TagOptions, TagReferenceData, TagSetError,
};

use crate::context::CliContext;

/// Errors emitted by the shell-side parser. The library's
/// [`TagSetError`] is wrapped here for the typed-mutation tail; raw
/// parse failures get their own variant.
#[derive(Debug)]
pub enum ParseError {
    /// The input string didn't match the expected shape for this
    /// field's schema type.
    Bad(String),
    /// The library rejected the typed value (container field type, or
    /// a type mismatch we should never see from this parser).
    Set(TagSetError),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bad(msg) => f.write_str(msg),
            Self::Set(TagSetError::NotAssignable) => {
                f.write_str("cannot set container field types directly")
            }
            Self::Set(TagSetError::TypeMismatch { expected, got }) => {
                write!(f, "type mismatch: expected {expected}, got {got}")
            }
        }
    }
}

impl std::error::Error for ParseError {}

impl From<TagSetError> for ParseError {
    fn from(e: TagSetError) -> Self { Self::Set(e) }
}

fn bad(msg: impl Into<String>) -> ParseError { ParseError::Bad(msg.into()) }

/// Parse `input` into the [`TagFieldData`] variant matching this
/// field's schema type, without committing. Useful for `set --dry-run`
/// and any other "validate before mutating" workflow. `ctx` provides
/// the [`crate::tag_index::TagIndex`] used to resolve `<path>.<group_name>`
/// tag references.
pub fn parse_field_value(
    ctx: &CliContext,
    field: &TagField<'_>,
    input: &str,
) -> Result<TagFieldData, ParseError> {
    parse_value(ctx, field, input)
}

fn parse_value(
    ctx: &CliContext,
    field: &TagField<'_>,
    input: &str,
) -> Result<TagFieldData, ParseError> {
    match field.field_type() {
        TagFieldType::CharInteger => Ok(TagFieldData::CharInteger(
            input.parse().map_err(|_| bad("expected i8"))?,
        )),
        TagFieldType::ShortInteger => Ok(TagFieldData::ShortInteger(
            input.parse().map_err(|_| bad("expected i16"))?,
        )),
        TagFieldType::LongInteger => Ok(TagFieldData::LongInteger(
            input.parse().map_err(|_| bad("expected i32"))?,
        )),
        TagFieldType::Int64Integer => Ok(TagFieldData::Int64Integer(
            input.parse().map_err(|_| bad("expected i64"))?,
        )),
        TagFieldType::ByteInteger => Ok(TagFieldData::ByteInteger(
            input.parse().map_err(|_| bad("expected u8"))?,
        )),
        TagFieldType::WordInteger => Ok(TagFieldData::WordInteger(
            input.parse().map_err(|_| bad("expected u16"))?,
        )),
        TagFieldType::DwordInteger => Ok(TagFieldData::DwordInteger(
            input.parse().map_err(|_| bad("expected u32"))?,
        )),
        TagFieldType::QwordInteger => Ok(TagFieldData::QwordInteger(
            input.parse().map_err(|_| bad("expected u64"))?,
        )),
        TagFieldType::Tag => Ok(TagFieldData::Tag(
            parse_group_tag(input).ok_or_else(|| bad("group tag must be 1..=4 ASCII chars"))?,
        )),

        TagFieldType::Angle => Ok(TagFieldData::Angle(
            input.parse().map_err(|_| bad("expected f32"))?,
        )),
        TagFieldType::Real => Ok(TagFieldData::Real(
            input.parse().map_err(|_| bad("expected f32"))?,
        )),
        TagFieldType::RealSlider => Ok(TagFieldData::RealSlider(
            input.parse().map_err(|_| bad("expected f32"))?,
        )),
        TagFieldType::RealFraction => Ok(TagFieldData::RealFraction(
            input.parse().map_err(|_| bad("expected f32"))?,
        )),

        TagFieldType::CharEnum => Ok(TagFieldData::CharEnum {
            value: parse_enum_value(field, input)? as i8,
            name: None,
        }),
        TagFieldType::ShortEnum => Ok(TagFieldData::ShortEnum {
            value: parse_enum_value(field, input)? as i16,
            name: None,
        }),
        TagFieldType::LongEnum => Ok(TagFieldData::LongEnum {
            value: parse_enum_value(field, input)?,
            name: None,
        }),

        TagFieldType::ByteFlags => Ok(TagFieldData::ByteFlags {
            value: parse_int_mask(input)? as u8,
            names: Vec::new(),
        }),
        TagFieldType::WordFlags => Ok(TagFieldData::WordFlags {
            value: parse_int_mask(input)? as u16,
            names: Vec::new(),
        }),
        TagFieldType::LongFlags => Ok(TagFieldData::LongFlags {
            value: parse_int_mask(input)? as i32,
            names: Vec::new(),
        }),

        TagFieldType::ByteBlockFlags => Ok(TagFieldData::ByteBlockFlags(parse_int_mask(input)? as u8)),
        TagFieldType::WordBlockFlags => Ok(TagFieldData::WordBlockFlags(parse_int_mask(input)? as u16)),
        TagFieldType::LongBlockFlags => Ok(TagFieldData::LongBlockFlags(parse_int_mask(input)? as i32)),

        TagFieldType::CharBlockIndex => Ok(TagFieldData::CharBlockIndex(parse_block_index(input)? as i8)),
        TagFieldType::CustomCharBlockIndex => Ok(TagFieldData::CustomCharBlockIndex(parse_block_index(input)? as i8)),
        TagFieldType::ShortBlockIndex => Ok(TagFieldData::ShortBlockIndex(parse_block_index(input)? as i16)),
        TagFieldType::CustomShortBlockIndex => Ok(TagFieldData::CustomShortBlockIndex(parse_block_index(input)? as i16)),
        TagFieldType::LongBlockIndex => Ok(TagFieldData::LongBlockIndex(parse_block_index(input)?)),
        TagFieldType::CustomLongBlockIndex => Ok(TagFieldData::CustomLongBlockIndex(parse_block_index(input)?)),

        TagFieldType::String => Ok(TagFieldData::String(input.to_string())),
        TagFieldType::LongString => Ok(TagFieldData::LongString(input.to_string())),

        TagFieldType::StringId => Ok(TagFieldData::StringId(StringIdData { string: input.to_string() })),
        TagFieldType::OldStringId => Ok(TagFieldData::OldStringId(StringIdData { string: input.to_string() })),

        TagFieldType::TagReference => Ok(TagFieldData::TagReference(parse_tag_reference(ctx, input)?)),

        TagFieldType::Data => Err(bad("parsing 'data' fields from a string is not supported")),

        TagFieldType::Struct
        | TagFieldType::Block
        | TagFieldType::Array
        | TagFieldType::PageableResource => Err(ParseError::Set(TagSetError::NotAssignable)),

        TagFieldType::ApiInterop => Ok(TagFieldData::ApiInterop(parse_api_interop(input)?)),

        TagFieldType::VertexBuffer => {
            Err(bad("parsing vertex_buffer fields is not supported"))
        }

        ty => Err(bad(format!(
            "parsing field type {:?} from a string is not supported",
            ty,
        ))),
    }
}

fn parse_enum_value(field: &TagField<'_>, input: &str) -> Result<i32, ParseError> {
    if let Ok(n) = input.parse::<i32>() {
        return Ok(n);
    }
    if let Some(TagOptions::Enum { names, .. }) = field.options() {
        for (i, name) in names.iter().enumerate() {
            if name.eq_ignore_ascii_case(input) {
                return Ok(i as i32);
            }
        }
    }
    Err(bad(format!("enum option '{}' not found", input)))
}

fn parse_int_mask(s: &str) -> Result<i64, ParseError> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).map_err(|_| bad("expected hex integer"))
    } else {
        s.parse::<i64>().map_err(|_| bad("expected integer"))
    }
}

fn parse_block_index(s: &str) -> Result<i32, ParseError> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(-1);
    }
    s.parse().map_err(|_| bad("expected integer or 'none'"))
}

/// Parse an api_interop payload.
///
/// - `reset` / `none` → BCS's canonical reset pattern
///   (`{ 0, UINT_MAX, 0 }`). The usual way to scrub runtime handles
///   out of a tag before committing it.
/// - `0xDESCRIPTOR,0xADDRESS,0xDEFINITION_ADDRESS` → verbatim triple
///   (each field a 32-bit integer, decimal or `0x` hex).
fn parse_api_interop(s: &str) -> Result<ApiInteropData, ParseError> {
    let trimmed = s.trim();
    if trimmed.eq_ignore_ascii_case("reset") || trimmed.eq_ignore_ascii_case("none") {
        return Ok(ApiInteropData::reset());
    }

    let parts: Vec<&str> = trimmed.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(bad(
            "api_interop format: 'reset', or 'descriptor,address,definition_address' (each u32, decimal or 0x…)",
        ));
    }
    let one = |p: &str| -> Result<u32, ParseError> {
        let (radix, body) = match p.strip_prefix("0x").or_else(|| p.strip_prefix("0X")) {
            Some(hex) => (16, hex),
            None => (10, p),
        };
        u32::from_str_radix(body, radix)
            .map_err(|_| bad("expected u32 (decimal or 0x hex)"))
    };
    let descriptor = one(parts[0])?;
    let address = one(parts[1])?;
    let definition_address = one(parts[2])?;

    let mut raw = Vec::with_capacity(12);
    raw.extend_from_slice(&descriptor.to_le_bytes());
    raw.extend_from_slice(&address.to_le_bytes());
    raw.extend_from_slice(&definition_address.to_le_bytes());
    Ok(ApiInteropData { raw, endian: blam_tags::Endian::Le })
}

fn parse_tag_reference(ctx: &CliContext, s: &str) -> Result<TagReferenceData, ParseError> {
    if s.eq_ignore_ascii_case("none") || s.is_empty() {
        return Ok(TagReferenceData { group_tag_and_name: None });
    }

    // Preferred form: `<path>.<group_name>` — e.g.
    // `objects/characters/elite/elite.biped`. Split on the *last* `.`
    // so paths with literal `.` characters still work, then resolve
    // the trailing identifier to a group tag via the loaded TagIndex.
    if let Some((path, name)) = s.rsplit_once('.')
        && let Some(group_tag) = ctx.tag_index.group_tag_for(name) {
            return Ok(TagReferenceData { group_tag_and_name: Some((group_tag, path.to_string())) });
        }

    // Legacy form: `<group_tag>:<path>`. Kept as a fallback for
    // scripts that still use the wire-style group-tag-prefixed form.
    if let Some((group_str, path)) = s.split_once(':') {
        let group_tag = parse_group_tag(group_str)
            .ok_or_else(|| bad("group tag must be 1..=4 ASCII chars"))?;
        return Ok(TagReferenceData { group_tag_and_name: Some((group_tag, path.to_string())) });
    }

    Err(bad(
        "tag reference format: <path>.<group_name> (e.g. objects/characters/elite/elite.biped), \
         or legacy <group_tag>:<path>, or 'none'",
    ))
}
