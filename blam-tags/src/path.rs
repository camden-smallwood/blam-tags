//! Path-based navigation into a tag's data tree.
//!
//! A path is a `/`-separated sequence of segments. Each segment names
//! a field; sub-chunk-bearing fields (structs / blocks / arrays) are
//! transparently stepped into by intermediate segments, and blocks /
//! arrays accept an optional `[N]` suffix to select a specific
//! element. If a descent hits a block/array without an explicit
//! index, element 0 is chosen — matching the CLI's behavior.
//!
//! Segment grammar: `[Type:]name[#ordinal][\[index\]]`
//! - `Type:` — optional case-insensitive filter on the field-type
//!   name (e.g. `Block:regions`), used to disambiguate fields that
//!   share a name across types.
//! - `name` — case-sensitive field name, looked up against the
//!   containing struct's fields via `layout.get_string`.
//! - `#ordinal` — optional 0-based field position within the containing
//!   struct (`field_index - first_field_index`). When present, resolution
//!   targets that exact field (Foundation-style positional addressing),
//!   unambiguous even when several fields share a name/type. A stale
//!   ordinal self-heals by falling back to name resolution. See
//!   `TagField::ordinal` for building these.
//! - `[index]` — block / array element index. Ignored on the final
//!   segment (the caller decides what to do with it).
//!
//! Fields that are *not* the last segment must resolve to a container
//! (struct/block/array/resource); a same-named non-container leaf (e.g.
//! the H2 particle "Mapping" custom placeholder) is skipped on descent.
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
        let (type_filter, name, ordinal, index) = parse_segment(segment);
        let field_index =
            find_field_in_struct(layout, current_struct, name, type_filter, ordinal, true)?;
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

    let (type_filter, name, ordinal, _index) = parse_segment(final_segment);
    let field_index =
        find_field_in_struct(layout, current_struct, name, type_filter, ordinal, false)?;
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
        let (type_filter, name, ordinal, index) = parse_segment(segment);
        let field_index =
            find_field_in_struct(layout, current_struct, name, type_filter, ordinal, true)?;
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
        let (type_filter, name, ordinal, index) = parse_segment(segment);
        let field_index =
            find_field_in_struct(layout, current_struct, name, type_filter, ordinal, true)?;
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

    let (type_filter, name, ordinal, _index) = parse_segment(final_segment);
    let field_index =
        find_field_in_struct(layout, current_struct, name, type_filter, ordinal, false)?;
    Some(TagFieldCursorMut {
        struct_raw: current_raw,
        struct_data: current_struct,
        field_index,
    })
}

//================================================================================
// Segment parsing
//================================================================================

/// Parse one path segment into `(type_filter, name, ordinal, index)`.
///
/// Grammar: `[Type:]name[#ordinal][[index]]`
/// - `Type:` — optional field-type filter (case-insensitive).
/// - `#ordinal` — optional 0-based field position within the containing struct
///   (`field_index - first_field_index`). When present, resolution targets that
///   exact field (Foundation-style positional addressing) — ambiguity-free even
///   when several fields share a name/type. A stale ordinal self-heals by
///   falling back to name resolution.
/// - `[index]` — optional block/array element index.
fn parse_segment(segment: &str) -> (Option<&str>, &str, Option<usize>, Option<u32>) {
    let (type_filter, rest) = match segment.find(':') {
        Some(colon) => (Some(&segment[..colon]), &segment[colon + 1..]),
        None => (None, segment),
    };

    // Trailing `[index]` element selector.
    let (name_ord, index) = match rest.find('[') {
        Some(open) => {
            let close = rest[open..].find(']').map(|o| open + o).unwrap_or(rest.len());
            let index = rest[open + 1..close].parse::<u32>().ok();
            (&rest[..open], index)
        }
        None => (rest, None),
    };

    // `#ordinal` field-position token (after the name, before any `[index]`).
    let (name, ordinal) = match name_ord.find('#') {
        Some(hash) => (&name_ord[..hash], name_ord[hash + 1..].parse::<usize>().ok()),
        None => (name_ord, None),
    };

    (type_filter, name, ordinal, index)
}

/// Field types a path segment can descend *into*.
fn is_container_field(field_type: TagFieldType) -> bool {
    matches!(
        field_type,
        TagFieldType::Struct
            | TagFieldType::Block
            | TagFieldType::Array
            | TagFieldType::PageableResource
    )
}

fn find_field_in_struct(
    layout: &TagLayout,
    struct_data: &TagStructData,
    name: &str,
    type_filter: Option<&str>,
    ordinal: Option<usize>,
    require_container: bool,
) -> Option<usize> {
    let struct_layout = &layout.struct_layouts[struct_data.struct_index as usize];
    let first_field_index = struct_layout.first_field_index as usize;

    // Positional resolution (Foundation's `tagFields[fieldIndex]`): a `#ordinal`
    // token pins the exact field regardless of duplicate names/types. We
    // soft-verify the field's name and (for intermediate segments) container-ness
    // so a stale ordinal — e.g. a path built against a different layout — falls
    // through to name resolution and self-heals rather than editing the wrong
    // field.
    if let Some(ord) = ordinal {
        let idx = first_field_index + ord;
        if idx < layout.fields.len() {
            let field = &layout.fields[idx];
            let name_ok = field.field_type != TagFieldType::Terminator
                && crate::data::field_name_matches(layout.get_string(field.name_offset), name);
            let container_ok = !require_container || is_container_field(field.field_type);
            if name_ok && container_ok {
                return Some(idx);
            }
        }
        // else: fall through to name resolution below.
    }

    // Fast path: unfiltered final-segment lookup → first name match.
    if type_filter.is_none() && !require_container {
        return struct_data.find_field_by_name(layout, name);
    }

    // Walk the fields, accepting the first name match that also satisfies the
    // `Type:name` filter (if any) and, for intermediate segments, is a
    // container. `require_container` matters when a name is ambiguous: the H2
    // particle/effect schemas place a `custom` placeholder field *before* the
    // real struct of the same name (e.g. two "Mapping" fields), so an
    // intermediate segment must skip the leaf and resolve to the struct —
    // otherwise the descent hits a non-container and the whole path fails.
    let mut field_index = first_field_index;

    loop {
        let field = &layout.fields[field_index];
        if field.field_type == TagFieldType::Terminator {
            return None;
        }
        if crate::data::field_name_matches(layout.get_string(field.name_offset), name) {
            let type_ok = match type_filter {
                Some(filter) => {
                    let type_name_offset = layout.field_types[field.type_index as usize].name_offset;
                    layout
                        .get_string(type_name_offset)
                        .unwrap_or("")
                        .eq_ignore_ascii_case(filter)
                }
                None => true,
            };
            if type_ok && (!require_container || is_container_field(field.field_type)) {
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

#[cfg(test)]
mod tests {
    use super::parse_segment;

    #[test]
    fn parse_segment_parses_type_name_ordinal_and_index() {
        // Plain name.
        assert_eq!(parse_segment("color"), (None, "color", None, None));
        // Ordinal only.
        assert_eq!(parse_segment("Mapping#5"), (None, "Mapping", Some(5), None));
        // Element index only.
        assert_eq!(parse_segment("params[2]"), (None, "params", None, Some(2)));
        // Type + name.
        assert_eq!(parse_segment("struct:Mapping"), (Some("struct"), "Mapping", None, None));
        // Ordinal + element index.
        assert_eq!(parse_segment("params#7[2]"), (None, "params", Some(7), Some(2)));
        // Everything: type + name + ordinal + index.
        assert_eq!(
            parse_segment("block:params#7[2]"),
            (Some("block"), "params", Some(7), Some(2))
        );
        // A name containing spaces stays intact.
        assert_eq!(parse_segment("Function Type#1"), (None, "Function Type", Some(1), None));
        // Garbage ordinal is ignored (falls back to name).
        assert_eq!(parse_segment("foo#x"), (None, "foo", None, None));
    }

    /// Integration: `name#ordinal` positional resolution on a real H2 particle
    /// (two "Mapping" fields per struct — the reported bug). Skipped when the
    /// tag/definition aren't present (not in CI).
    #[test]
    fn positional_path_resolves_exact_field_h2_particle() {
        use crate::classic::read_classic_tag_file;
        use crate::layout::TagLayout;
        let tag_path = "/Users/camden/Halo/halo2_mcc/tags/effects/generic/smoke/steam.particle";
        let def = "../definitions/halo2_mcc/particle.json";
        if !std::path::Path::new(tag_path).exists() || !std::path::Path::new(def).exists() {
            eprintln!("skipping: H2 particle/definition not present");
            return;
        }
        let bytes = std::fs::read(tag_path).unwrap();
        let layout = TagLayout::from_json(def).unwrap();
        let tag = read_classic_tag_file(&bytes, layout).unwrap();
        let root = tag.root();

        // The color struct's `.field("Mapping")` is the custom leaf (ambiguity).
        let color = root.field("color").and_then(|f| f.as_struct()).unwrap();
        assert!(
            color.field("Mapping").and_then(|f| f.as_struct()).is_none(),
            "first 'Mapping' should be the non-struct custom leaf"
        );
        // Capture the struct Mapping's ordinal and resolve positionally.
        let map_ord = color
            .fields_all()
            .find(|f| f.name() == "Mapping" && f.as_struct().is_some())
            .map(|f| f.ordinal())
            .unwrap();
        let color_ord = root
            .fields_all()
            .find(|f| f.name() == "color" && f.as_struct().is_some())
            .map(|f| f.ordinal())
            .unwrap();

        // Positional path resolves the exact struct field.
        let path = format!("color#{color_ord}/Mapping#{map_ord}/Function Type");
        assert!(root.field_path(&path).is_some(), "positional path {path} failed");
        // Plain path resolves too (require_container fallback).
        assert!(root.field_path("color/Mapping/Function Type").is_some());
        // A stale ordinal self-heals via name fallback.
        assert!(root.field_path("color#99/Mapping#99/Function Type").is_some());
    }
}
