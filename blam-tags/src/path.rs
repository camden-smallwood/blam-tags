//! Path-based navigation into a tag's data tree.
//!
//! A path is a `/`-separated sequence of segments. Each segment names
//! a field; sub-chunk-bearing fields (structs / blocks / arrays) are
//! transparently stepped into by intermediate segments, and blocks /
//! arrays accept an optional `[N]` suffix to select a specific
//! element. If a descent hits a block/array without an explicit
//! index, element 0 is chosen — matching the CLI's behavior.
//!
//! Segment grammar: `[Type:]name[\[index\]]`
//! - `Type:` — optional case-insensitive filter on the field-type
//!   name (e.g. `Block:regions`), used to disambiguate fields that
//!   share a name across types.
//! - `name` — case-sensitive field name, looked up against the
//!   containing struct's fields via `layout.get_string`.
//! - `[index]` — block / array element index. Ignored on the final
//!   segment (the caller decides what to do with it).
//!
//! Lookups return a [`TagFieldCursor`] / [`TagFieldCursorMut`] bundling the
//! enclosing block's `raw_data` slice, the containing
//! [`crate::data::TagStructData`], and the final field's index. Read
//! primitives via [`TagFieldCursor::parse`]; write via
//! [`TagFieldCursorMut::set`].

use crate::data::{TagBlockData, TagResourceChunk, TagStructData, TagSubChunkContent};
use crate::fields::TagFieldType;
use crate::layout::TagLayout;

/// Immutable cursor at a resolved field. `struct_raw` is the slice
/// from the enclosing block's `raw_data` covering exactly the
/// containing struct's bytes; `field_index` indexes into
/// `layout.fields`.
pub(crate) struct TagFieldCursor<'a> {
    pub struct_raw: &'a [u8],
    pub struct_data: &'a TagStructData,
    pub field_index: usize,
}

/// Mutable cursor. Same shape as [`TagFieldCursor`] but with mutable
/// references, so the facade's write-side handles (`TagStructMut`,
/// `TagFieldMut`, …) can split-borrow into it to mutate in place.
pub(crate) struct TagFieldCursorMut<'a> {
    pub struct_raw: &'a mut [u8],
    pub struct_data: &'a mut TagStructData,
    pub field_index: usize,
}

/// Resolve `path` against a starting struct + raw slice and return a
/// [`TagFieldCursor`] at the final field. Used by the facade's
/// [`crate::api::TagStruct::field_path`]; callers at the tag root
/// pass `tag.tag_stream.data.elements[0]` and `element_raw(0)`.
pub(crate) fn lookup_from_struct<'a>(
    layout: &'a TagLayout,
    start_struct: &'a TagStructData,
    start_raw: &'a [u8],
    path: &str,
) -> Option<TagFieldCursor<'a>> {
    let segments: Vec<&str> = path.split('/').collect();
    let (final_segment, preceding) = segments.split_last()?;

    let mut current_raw: &[u8] = start_raw;
    let mut current_struct: &TagStructData = start_struct;

    for segment in preceding {
        let (type_filter, name, index) = parse_segment(segment);
        let field_index = find_field_in_struct(layout, current_struct, name, type_filter)?;
        let field = &layout.fields[field_index];

        match field.field_type {
            TagFieldType::Struct => {
                let nested_def = &layout.struct_layouts[field.definition as usize];
                let offset = field.offset as usize;
                if offset >= current_raw.len() {
                    return None;
                }
                let end = (offset + nested_def.size).min(current_raw.len());
                current_raw = &current_raw[offset..end];
                current_struct = descend_struct(current_struct, field_index)?;
            }
            TagFieldType::Block => {
                let block = descend_block_data(current_struct, field_index)?;
                let block_def = &layout.block_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[block_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                // Bounds check: a block can legitimately be empty
                // (X360 monolithic tags with pageable resources push
                // their element data into the cache partition and
                // leave the on-disk block at 0 elements).
                let start = element_index * element_def.size;
                let end = start + element_def.size;
                if end > block.raw_data.len() {
                    return None;
                }
                current_raw = &block.raw_data[start..end];
                current_struct = block.elements.get(element_index)?;
            }
            TagFieldType::Array => {
                let array_def = &layout.array_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[array_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                let start = field.offset as usize + element_index * element_def.size;
                let end = start + element_def.size;
                if end > current_raw.len() {
                    return None;
                }
                current_raw = &current_raw[start..end];
                let elements = descend_array(current_struct, field_index)?;
                current_struct = elements.get(element_index)?;
            }
            TagFieldType::PageableResource => {
                let (nested, nested_raw) = descend_resource(layout, current_struct, field_index)?;
                current_raw = nested_raw;
                current_struct = nested;
            }
            _ => return None,
        }
    }

    let (type_filter, name, _index) = parse_segment(final_segment);
    let field_index = find_field_in_struct(layout, current_struct, name, type_filter)?;
    Some(TagFieldCursor {
        struct_raw: current_raw,
        struct_data: current_struct,
        field_index,
    })
}

/// Walk `path` treating every `/`-separated segment as an intermediate
/// descent (struct sub-chunk / block element at `[i]` / array element
/// at `[i]`). Returns the struct + raw slice at the terminal position.
///
/// Unlike [`lookup_from_struct`], there's no "final-segment lookup" —
/// the caller just wants to *reach* a struct, not the field that
/// points to it. Used by the facade's [`crate::api::TagStruct::descend`].
pub(crate) fn descend_from_struct<'a>(
    layout: &'a TagLayout,
    start_struct: &'a TagStructData,
    start_raw: &'a [u8],
    path: &str,
) -> Option<(&'a TagStructData, &'a [u8])> {
    let mut current_raw: &[u8] = start_raw;
    let mut current_struct: &TagStructData = start_struct;

    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let (type_filter, name, index) = parse_segment(segment);
        let field_index = find_field_in_struct(layout, current_struct, name, type_filter)?;
        let field = &layout.fields[field_index];

        match field.field_type {
            TagFieldType::Struct => {
                let nested_def = &layout.struct_layouts[field.definition as usize];
                let offset = field.offset as usize;
                if offset >= current_raw.len() {
                    return None;
                }
                let end = (offset + nested_def.size).min(current_raw.len());
                current_raw = &current_raw[offset..end];
                current_struct = descend_struct(current_struct, field_index)?;
            }
            TagFieldType::Block => {
                let block = descend_block_data(current_struct, field_index)?;
                let block_def = &layout.block_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[block_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                // Bounds check: a block can legitimately be empty
                // (X360 monolithic tags with pageable resources push
                // their element data into the cache partition and
                // leave the on-disk block at 0 elements).
                let start = element_index * element_def.size;
                let end = start + element_def.size;
                if end > block.raw_data.len() {
                    return None;
                }
                current_raw = &block.raw_data[start..end];
                current_struct = block.elements.get(element_index)?;
            }
            TagFieldType::Array => {
                let array_def = &layout.array_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[array_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                let start = field.offset as usize + element_index * element_def.size;
                let end = start + element_def.size;
                if end > current_raw.len() {
                    return None;
                }
                current_raw = &current_raw[start..end];
                let elements = descend_array(current_struct, field_index)?;
                current_struct = elements.get(element_index)?;
            }
            TagFieldType::PageableResource => {
                let (nested, nested_raw) = descend_resource(layout, current_struct, field_index)?;
                current_raw = nested_raw;
                current_struct = nested;
            }
            _ => return None,
        }
    }

    Some((current_struct, current_raw))
}

/// Mutable counterpart to [`lookup_from_struct`]. Descends through
/// disjoint field splits to maintain simultaneous `&mut` access to
/// the enclosing block's `raw_data` slice and the containing
/// `TagStructData`.
pub(crate) fn lookup_mut_from_struct<'a>(
    layout: &'a TagLayout,
    start_struct: &'a mut TagStructData,
    start_raw: &'a mut [u8],
    path: &str,
) -> Option<TagFieldCursorMut<'a>> {
    let segments: Vec<&str> = path.split('/').collect();
    let (final_segment, preceding) = segments.split_last()?;

    let mut current_raw: &mut [u8] = start_raw;
    let mut current_struct: &mut TagStructData = start_struct;

    for segment in preceding {
        let (type_filter, name, index) = parse_segment(segment);
        let field_index = find_field_in_struct(layout, current_struct, name, type_filter)?;
        let field = &layout.fields[field_index];

        match field.field_type {
            TagFieldType::Struct => {
                let nested_def = &layout.struct_layouts[field.definition as usize];
                let offset = field.offset as usize;
                let size = nested_def.size;
                let new_raw = &mut current_raw[offset..offset + size];
                let new_struct = descend_struct_mut(current_struct, field_index)?;
                current_raw = new_raw;
                current_struct = new_struct;
            }
            TagFieldType::Block => {
                let block = descend_block_data_mut(current_struct, field_index)?;
                let block_def = &layout.block_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[block_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                let element_size = element_def.size;
                let start = element_index * element_size;
                let end = start + element_size;
                if end > block.raw_data.len() {
                    return None;
                }
                let new_raw = &mut block.raw_data[start..end];
                let new_struct = block.elements.get_mut(element_index)?;
                current_raw = new_raw;
                current_struct = new_struct;
            }
            TagFieldType::Array => {
                let array_def = &layout.array_layouts[field.definition as usize];
                let element_def = &layout.struct_layouts[array_def.struct_index as usize];
                let element_index = index.unwrap_or(0) as usize;
                let offset = field.offset as usize + element_index * element_def.size;
                let size = element_def.size;
                if offset + size > current_raw.len() {
                    return None;
                }
                let new_raw = &mut current_raw[offset..offset + size];
                let elements = descend_array_mut(current_struct, field_index)?;
                let new_struct = elements.get_mut(element_index)?;
                current_raw = new_raw;
                current_struct = new_struct;
            }
            TagFieldType::PageableResource => {
                let (new_struct, new_raw) = descend_resource_mut(layout, current_struct, field_index)?;
                current_raw = new_raw;
                current_struct = new_struct;
            }
            _ => return None,
        }
    }

    let (type_filter, name, _index) = parse_segment(final_segment);
    let field_index = find_field_in_struct(layout, current_struct, name, type_filter)?;
    Some(TagFieldCursorMut {
        struct_raw: current_raw,
        struct_data: current_struct,
        field_index,
    })
}

//================================================================================
// Segment parsing
//================================================================================

fn parse_segment(segment: &str) -> (Option<&str>, &str, Option<u32>) {
    let (type_filter, rest) = match segment.find(':') {
        Some(colon) => (Some(&segment[..colon]), &segment[colon + 1..]),
        None => (None, segment),
    };

    let (name, index) = match rest.find('[') {
        Some(open) => {
            let close = rest[open..].find(']').map(|o| open + o).unwrap_or(rest.len());
            let index = rest[open + 1..close].parse::<u32>().ok();
            (&rest[..open], index)
        }
        None => (rest, None),
    };

    (type_filter, name, index)
}

fn find_field_in_struct(
    layout: &TagLayout,
    struct_data: &TagStructData,
    name: &str,
    type_filter: Option<&str>,
) -> Option<usize> {
    // No type filter → delegate to the by-name helper.
    let Some(filter) = type_filter else {
        return struct_data.find_field_by_name(layout, name);
    };

    // Filtered walk: accept the first name match whose field-type
    // name matches the filter case-insensitively.
    let struct_layout = &layout.struct_layouts[struct_data.struct_index as usize];
    let mut field_index = struct_layout.first_field_index as usize;

    loop {
        let field = &layout.fields[field_index];
        if field.field_type == TagFieldType::Terminator {
            return None;
        }
        if crate::data::field_name_matches(layout.get_string(field.name_offset), name) {
            let type_name_offset = layout.field_types[field.type_index as usize].name_offset;
            let type_name = layout.get_string(type_name_offset).unwrap_or("");
            if type_name.eq_ignore_ascii_case(filter) {
                return Some(field_index);
            }
        }
        field_index += 1;
    }
}

fn descend_struct(struct_data: &TagStructData, field_index: usize) -> Option<&TagStructData> {
    let entry = struct_data
        .sub_chunks
        .iter()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &entry.content {
        TagSubChunkContent::Struct(nested) => Some(nested),
        _ => None,
    }
}

fn descend_struct_mut(struct_data: &mut TagStructData, field_index: usize) -> Option<&mut TagStructData> {
    let entry = struct_data
        .sub_chunks
        .iter_mut()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &mut entry.content {
        TagSubChunkContent::Struct(nested) => Some(nested),
        _ => None,
    }
}

fn descend_block_data(struct_data: &TagStructData, field_index: usize) -> Option<&TagBlockData> {
    let entry = struct_data
        .sub_chunks
        .iter()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &entry.content {
        TagSubChunkContent::Block(block) => Some(block),
        _ => None,
    }
}

fn descend_block_data_mut(
    struct_data: &mut TagStructData,
    field_index: usize,
) -> Option<&mut TagBlockData> {
    let entry = struct_data
        .sub_chunks
        .iter_mut()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &mut entry.content {
        TagSubChunkContent::Block(block) => Some(block),
        _ => None,
    }
}

fn descend_array(struct_data: &TagStructData, field_index: usize) -> Option<&[TagStructData]> {
    let entry = struct_data
        .sub_chunks
        .iter()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &entry.content {
        TagSubChunkContent::Array(elements) => Some(elements),
        _ => None,
    }
}

fn descend_array_mut(
    struct_data: &mut TagStructData,
    field_index: usize,
) -> Option<&mut [TagStructData]> {
    let entry = struct_data
        .sub_chunks
        .iter_mut()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    match &mut entry.content {
        TagSubChunkContent::Array(elements) => Some(elements),
        _ => None,
    }
}

/// Step into a `PageableResource` field. The resource's header struct
/// lives in the `tgdt` payload bytes (the leading `struct_size` of
/// them); `struct_data` is the parsed sub-chunk tree. Returns `None`
/// for null / xsync resources, where there's no parsed struct.
fn descend_resource<'a>(
    layout: &'a TagLayout,
    struct_data: &'a TagStructData,
    field_index: usize,
) -> Option<(&'a TagStructData, &'a [u8])> {
    let entry = struct_data
        .sub_chunks
        .iter()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    let TagSubChunkContent::Resource(TagResourceChunk::Exploded { struct_data, exploded, .. }) =
        &entry.content
    else {
        return None;
    };
    let struct_size = layout.struct_layouts[struct_data.struct_index as usize].size;
    let header_raw = exploded.get(..struct_size)?;
    Some((struct_data, header_raw))
}

fn descend_resource_mut<'a>(
    layout: &'a TagLayout,
    struct_data: &'a mut TagStructData,
    field_index: usize,
) -> Option<(&'a mut TagStructData, &'a mut [u8])> {
    let entry = struct_data
        .sub_chunks
        .iter_mut()
        .find(|entry| entry.field_index == Some(field_index as u32))?;
    let TagSubChunkContent::Resource(TagResourceChunk::Exploded { struct_data, exploded, .. }) =
        &mut entry.content
    else {
        return None;
    };
    let struct_size = layout.struct_layouts[struct_data.struct_index as usize].size;
    let header_raw = exploded.get_mut(..struct_size)?;
    Some((struct_data, header_raw))
}
