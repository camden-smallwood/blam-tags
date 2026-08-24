//! Reading a monolithic build's resource control data into a tag's own blocks.
//!
//! It owns the walk from a resource's flat, address-linked control buffer into
//! the block tree a loose tag holds inline; which resources are worth walking,
//! and what to do with what comes out, is the converter's business.
//!
//! A `tgxc` resource is the engine's own memory image: struct bytes laid out
//! big-endian, with every pointer stubbed to zero and a fixup list that turns
//! the stubs back into `(tier, offset)` addresses. A loose tag says the same
//! thing as a tree of chunks. The two agree about the *shape* -- both are the
//! schema's structs -- so this walks the target's own field declarations and,
//! for each one, reads the matching bytes out of the control buffer:
//!
//! - a scalar is at the field's offset, in the source's byte order;
//! - a block is a 12-byte `(count, address, runtime)` triple, and its elements
//!   sit end to end at that address;
//! - a `data` field is a 20-byte descriptor whose address names a buffer;
//! - a nested struct or an inline array is where the schema says it is.
//!
//! Nothing here interprets what it copies. A field the schema does not describe
//! as one of those is counted and reported rather than guessed at.

use crate::api::TagStructMut;
use crate::fields::TagFieldType;

use super::{FixupAddress, FixupTier};

/// Why a control-data walk stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlReadError {
    /// A struct or block element reached past the end of its buffer.
    OutOfRange { at: usize, size: usize, available: usize },
    /// An address named a buffer this walk was not given.
    UnreadableTier { tier: FixupTier },
    /// The tree nested deeper than any real one does.
    TooDeep,
    /// A block declared more elements than the schema permits.
    ImplausibleCount { field: String, count: u32 },
}

impl std::fmt::Display for ControlReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OutOfRange { at, size, available } => write!(
                f,
                "a {size}-byte read at {at} runs past the {available} bytes there are",
            ),
            Self::UnreadableTier { tier } => {
                write!(f, "an address points into the {tier:?} buffer, which is not here")
            }
            Self::TooDeep => write!(f, "the struct tree nests deeper than this will follow"),
            Self::ImplausibleCount { field, count } => {
                write!(f, "{field} declares {count} elements, which its schema does not allow")
            }
        }
    }
}

/// How deep a real resource tree goes, with room to spare.
const MAX_DEPTH: usize = 16;

/// What was copied, for the report.
#[derive(Debug, Default, Clone, Copy)]
pub struct ControlReadTally {
    pub structs: usize,
    pub block_elements: usize,
    pub data_bytes: usize,
    /// Fields whose shape this walk has no reading for. None have been seen in
    /// the corpus; counted so a build that has one says so instead of writing a
    /// silent zero.
    pub skipped: usize,
}

/// Copy one struct out of the control data into `target`.
///
/// `at` is the struct's offset within `control`; `primary` is the resource's
/// always-resident buffer, which is where `data` payloads live.
pub fn read_struct_into(
    control: &[u8],
    primary: &[u8],
    at: usize,
    target: &mut TagStructMut<'_>,
    tally: &mut ControlReadTally,
) -> Result<(), ControlReadError> {
    read_struct_at(control, primary, control, at, target, tally, 0)
}

fn read_struct_at(
    control: &[u8],
    primary: &[u8],
    // Which buffer *this* struct's own bytes are in. A block's address names
    // the buffer its elements live in, and for a BSP the big arrays -- the
    // collision nodes, the surfaces -- are in the primary one while the structs
    // pointing at them stay in the control data.
    here: &[u8],
    at: usize,
    target: &mut TagStructMut<'_>,
    tally: &mut ControlReadTally,
    depth: usize,
) -> Result<(), ControlReadError> {
    if depth > MAX_DEPTH {
        return Err(ControlReadError::TooDeep);
    }
    let size = target.as_ref().definition().size();
    let raw = here
        .get(at..at + size)
        .ok_or(ControlReadError::OutOfRange { at, size, available: here.len() })?;
    tally.structs += 1;

    // Scalars first, straight off the raw bytes in the source's order. A view
    // with no sub-chunk tree reads exactly the fields that live in the bytes
    // and returns nothing for the ones that do not, which is the split this
    // needs anyway.
    let names: Vec<String> = target.as_ref().field_names().map(str::to_owned).collect();
    let values: Vec<(String, crate::fields::TagFieldData)> = {
        let view = target.as_ref().over_raw(raw);
        names
            .iter()
            .filter_map(|name| {
                view.field(name)
                    .and_then(|field| field.value())
                    .map(|value| (name.clone(), value))
            })
            .collect()
    };
    for (name, value) in values {
        if let Some(mut field) = target.field_mut(&name) {
            let _ = field.set(value);
        }
    }

    // Then the fields that are somewhere else. Driven by the declaration
    // rather than by the names read above: those skip padding, and pairing the
    // two by position would read every field after the first pad at the wrong
    // offset.
    let declared: Vec<(String, usize, TagFieldType)> = target
        .as_ref()
        .definition()
        .fields()
        .map(|field| {
            // The declared name carries the schema's own markers -- a trailing
            // `*`, a `{alias}`, a `!` -- and a lookup wants the name a reader
            // sees.
            (
                crate::field_name::clean_field_name(field.name()).into_owned(),
                field.offset() as usize,
                field.field_type(),
            )
        })
        .collect();
    for (index, (name, offset, field_type)) in declared.into_iter().enumerate() {
        let definition = target.as_ref().definition().fields().nth(index).unwrap();
        match field_type {
            TagFieldType::Block => {
                let Some(block_definition) = definition.as_block() else { continue };
                let name = name.as_str();
                let element_size = block_definition.struct_definition().size();
                let limit = block_definition.max_count();
                let (count, address) = read_tag_block(here, at + offset)?;
                if count == 0 {
                    continue;
                }
                if limit > 0 && count > limit {
                    return Err(ControlReadError::ImplausibleCount {
                        field: name.to_owned(),
                        count,
                    });
                }
                let elements = buffer_for(address, control, primary)?;
                let base = address.offset() as usize;
                let Some(mut field) = target.field_mut(&name) else { continue };
                let Some(mut block) = field.as_block_mut() else { continue };
                block.clear();
                for element in 0..count as usize {
                    let index = block.add_element();
                    let Some(mut element_target) = block.element_mut(index) else { continue };
                    tally.block_elements += 1;
                    read_struct_at(
                        control,
                        primary,
                        elements,
                        base + element * element_size,
                        &mut element_target,
                        tally,
                        depth + 1,
                    )?;
                }
            }
            TagFieldType::Struct => {
                let Some(mut field) = target.field_mut(&name) else { continue };
                let Some(mut nested) = field.as_struct_mut() else { continue };
                read_struct_at(control, primary, here, at + offset, &mut nested, tally, depth + 1)?;
            }
            TagFieldType::Array => {
                let Some(array_definition) = definition.as_array() else { continue };
                let element_size = array_definition.struct_definition().size();
                let Some(mut field) = target.field_mut(&name) else { continue };
                let Some(mut array) = field.as_array_mut() else { continue };
                for element in 0..array.len() {
                    let Some(mut element_target) = array.element_mut(element) else { continue };
                    read_struct_at(
                        control,
                        primary,
                        here,
                        at + offset + element * element_size,
                        &mut element_target,
                        tally,
                        depth + 1,
                    )?;
                }
            }
            TagFieldType::Data => {
                let (length, address) = read_tag_data(here, at + offset)?;
                let bytes = if length == 0 {
                    Vec::new()
                } else {
                    let base = address.offset() as usize;
                    let source = buffer_for(address, control, primary)?;
                    source
                        .get(base..base + length as usize)
                        .ok_or(ControlReadError::OutOfRange {
                            at: base,
                            size: length as usize,
                            available: source.len(),
                        })?
                        .to_vec()
                };
                tally.data_bytes += bytes.len();
                if let Some(mut field) = target.field_mut(&name) {
                    let _ = field.set(crate::fields::TagFieldData::Data(bytes));
                }
            }
            // Nothing in the observed corpus, and a wrong guess would be worse
            // than an honest count.
            TagFieldType::TagReference
            | TagFieldType::StringId
            | TagFieldType::OldStringId
            | TagFieldType::PageableResource
            | TagFieldType::ApiInterop => tally.skipped += 1,
            _ => {}
        }
    }
    Ok(())
}

/// Which buffer an address names, and where in it.
fn buffer_for<'a>(
    address: FixupAddress,
    control: &'a [u8],
    primary: &'a [u8],
) -> Result<&'a [u8], ControlReadError> {
    match address.tier() {
        FixupTier::Control => Ok(control),
        FixupTier::Primary => Ok(primary),
        tier => Err(ControlReadError::UnreadableTier { tier }),
    }
}

/// `s_tag_block`: a count, an address, and a word the engine fills in.
fn read_tag_block(bytes: &[u8], at: usize) -> Result<(u32, FixupAddress), ControlReadError> {
    let raw = bytes.get(at..at + 12).ok_or(ControlReadError::OutOfRange {
        at,
        size: 12,
        available: bytes.len(),
    })?;
    Ok((be_u32(raw, 0), FixupAddress(be_u32(raw, 4))))
}

/// `s_tag_data`: a size, two words the engine fills in, then the address.
fn read_tag_data(bytes: &[u8], at: usize) -> Result<(u32, FixupAddress), ControlReadError> {
    let raw = bytes.get(at..at + 20).ok_or(ControlReadError::OutOfRange {
        at,
        size: 20,
        available: bytes.len(),
    })?;
    Ok((be_u32(raw, 0), FixupAddress(be_u32(raw, 12))))
}

fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
