//! Tag layout: the schema attached to every tag file. A layout describes
//! the structs, blocks, fields, and field types used to interpret the
//! tag's payload bytes. It lives in the `blay` chunk (wrapping the `tgly`
//! chunk for v2/v3; flat for v1).
//!
//! Everything here is *schema*, not instance data. Per-tag values live in
//! [`crate::data`], which dispatches on the [`TagFieldType`] resolved on
//! each [`TagFieldLayout`] during layout read.

use std::io::{Read, Seek, Write};

use crate::error::TagReadError;
use crate::fields::TagFieldType;
use crate::io::*;

/// An `sz[]` entry: a named list of strings, represented as a slice
/// into [`TagLayout::string_offsets`]. Used for enum/flags value names.
#[derive(Debug)]
pub struct TagStringList {
    /// `name_offset`-style index into [`TagLayout::string_data`] — the
    /// display name of this list (e.g. an enum type name).
    pub offset: u32,
    /// Number of entries in this list.
    pub count: u32,
    /// Index into [`TagLayout::string_offsets`] of the first entry.
    /// Entries `[first .. first + count]` are this list's strings.
    pub first: u32,
}

/// An `arr!` entry: a fixed-count inline array of a struct. Array
/// elements have no wrapping `tgst` — their raw bytes live inline in
/// the parent struct's `raw_data`, and their sub-chunks flow inline
/// into the parent's `tgst` content.
#[derive(Debug)]
pub struct TagArrayLayout {
    /// Offset into [`TagLayout::string_data`] of the array's name.
    pub name_offset: u32,
    /// Number of elements.
    pub count: u32,
    /// Index into [`TagLayout::struct_layouts`] of the element struct.
    pub struct_index: u32,
}

/// A `tgft` entry: a field-type registry record. Indexed by
/// [`TagFieldLayout::type_index`].
#[derive(Debug)]
pub struct TagFieldTypeLayout {
    /// Offset into [`TagLayout::string_data`] of the canonical type name
    /// (e.g. `"real point 3d"`), resolved at read time via
    /// [`TagFieldType::from_name`].
    pub name_offset: u32,
    /// Raw-data byte footprint of a field of this type. Used to advance
    /// the struct offset for primitives that don't need a sub-chunk.
    pub size: u32,
    /// Nonzero if fields of this type emit a sub-chunk inside their
    /// containing `tgst`. Used by [`TagLayout::get_struct_expected_children`]
    /// and as the bug-catching assertion in `read_sub_chunks`.
    pub needs_sub_chunk: u32,
}

/// A `gras` entry: one field within a struct definition. Serialized
/// form is 12 bytes (`name_offset` + `type_index` + `definition`); the
/// derived `field_type` and `offset` are computed at read time and are
/// not on the wire.
#[derive(Debug)]
pub struct TagFieldLayout {
    /// Offset into [`TagLayout::string_data`] of the field name.
    pub name_offset: u32,
    /// Index into [`TagLayout::field_types`].
    pub type_index: u32,
    /// Type-specific "definition" payload. For struct/block/array/
    /// pageable_resource/api_interop fields, indexes the matching
    /// definition table; for pad/skip, the byte count; for enum/flags,
    /// the `string_lists` index.
    pub definition: u32,
    /// Dispatch-ready resolved type. Set once during layout read; not
    /// serialized.
    pub field_type: TagFieldType,
    /// Byte offset of this field's raw data within its containing struct. Set
    /// after the layout is fully parsed (after struct sizes are known).
    pub offset: u32,
}

/// A `blv2` entry (v2/v3) or half of a v1 `agro` record: names a block
/// whose elements are instances of a struct.
#[derive(Debug)]
pub struct TagBlockLayout {
    /// Position in [`TagLayout::block_layouts`]. Tracked so
    /// `crate::data::TagBlockData` can remember which block it came from.
    pub index: u32,
    /// Offset into [`TagLayout::string_data`] of the block name.
    pub name_offset: u32,
    /// Element-count cap from the schema. Not enforced here — preserved
    /// for roundtrip.
    pub max_count: u32,
    /// Index into [`TagLayout::struct_layouts`] of the element struct.
    pub struct_index: u32,
}

/// An `rcv2` entry: declares a pageable-resource field's shape.
#[derive(Debug)]
pub struct TagResourceLayout {
    /// Offset into [`TagLayout::string_data`] of the resource name.
    pub name_offset: u32,
    /// Unknown purpose; preserved verbatim.
    pub unknown: u32,
    /// Index into [`TagLayout::struct_layouts`] of the struct that
    /// wraps the resource payload when it's in
    /// `crate::data::TagResourceChunk::Exploded` form.
    pub struct_index: u32,
}

/// A `]==[` entry (v3 only): declares an api-interop field — an opaque
/// runtime-only pointer slot. Not parsed.
#[derive(Debug)]
pub struct TagInteropLayout {
    pub name_offset: u32,
    pub struct_index: u32,
    /// Stable identifier for the interop type across versions.
    pub guid: [u8; 16],
}

/// An `stv2` entry (v2/v3) or half of a v1 `agro` record: names a
/// struct and points at its first field. Size is derived at read time
/// by [`TagLayout::compute_struct_layout`] walking fields until the
/// terminator.
#[derive(Debug)]
pub struct TagStructLayout {
    /// Position in [`TagLayout::struct_layouts`]. Tracked so
    /// `crate::data::TagStructData` can remember which struct it came from.
    pub index: u32,
    /// Stable identifier for the struct type across layout versions.
    pub guid: [u8;16],
    /// Offset into [`TagLayout::string_data`] of the struct name.
    pub name_offset: u32,
    /// Index into [`TagLayout::fields`] of the first field in this
    /// struct; fields continue until a `Terminator`-typed field.
    pub first_field_index: u32,
    /// Derived field-packed size in bytes. Zero until
    /// [`TagLayout::compute_struct_layout`] runs. Not serialized.
    pub size: usize,
    /// Schema-version tag for the struct. Only present in V4 layouts
    /// (serialized as the trailing `u32` of an `stv4` record). Zero
    /// for V1/V2/V3 layouts.
    pub version: u32,
}

/// Counts at the top of the `blay` payload; each field here is the
/// length of the correspondingly named `Vec` on [`TagLayout`]. Which
/// fields are present depends on the block-layout version: v1 flattens
/// structs + blocks + fields into `aggregate_layout_count` records;
/// v2/v3 split them into the modern separate tables, and v3 adds
/// interop definitions.
#[derive(Debug)]
pub struct TagLayoutHeader {
    /// Index into [`TagLayout::block_layouts`] of the root (tag
    /// group) block. The tag's root `bdat` interprets its elements
    /// through this block's struct. v2/v3 only.
    pub tag_group_block_index: u32,
    pub string_data_size: u32,
    pub string_offset_count: u32,
    pub string_list_count: u32,
    pub custom_block_index_search_names_count: u32,
    pub data_definition_name_count: u32,
    pub array_layout_count: u32,
    pub field_type_count: u32,
    pub field_count: u32,
    /// v1 only. Each aggregate record is a flat (guid, name, max_count,
    /// first_field_index) tuple that the reader splits into a paired
    /// [`TagStructLayout`] and [`TagBlockLayout`].
    pub aggregate_layout_count: u32,
    /// v2/v3 only.
    pub struct_layout_count: u32,
    /// v2/v3 only.
    pub block_layout_count: u32,
    /// v2/v3 only.
    pub resource_layout_count: u32,
    /// v3 only.
    pub interop_layout_count: u32,
}

/// The full `blay` chunk: a tag's schema plus its 24-byte payload
/// header (`root_data_size`, `guid`, `version`). The outer `blay`
/// chunk header is always version 2, even when the inner layout
/// payload `version` (1/2/3/4) differs.
///
/// All name-bearing records reference [`Self::string_data`] by byte
/// offset; use [`Self::get_string`] to resolve.
#[derive(Debug)]
pub struct TagLayout {
    /// Raw-data size of the root struct (one element of the root
    /// block). Schema-level sanity check; preserved for roundtrip.
    pub root_data_size: u32,
    /// Stable identifier for the tag group / root block type.
    pub guid: [u8; 16],
    /// Layout payload version (1, 2, 3, or 4). Distinct from the
    /// outer `blay` chunk-header version (always 2).
    pub version: u32,

    pub header: TagLayoutHeader,
    /// `str*` chunk — concatenated null-terminated UTF-8 strings. All
    /// `name_offset` fields elsewhere index into here.
    pub string_data: Vec<u8>,
    /// `sz+x` chunk — u32 offsets into `string_data`. Referenced by
    /// [`TagStringList::first`] to form enum/flags value lists.
    pub string_offsets: Vec<u32>,
    /// `sz[]` chunk — named string-list records.
    pub string_lists: Vec<TagStringList>,
    /// `csbn` chunk — offsets into `string_data` for custom
    /// block-index search names. Referenced by custom-block-index
    /// field `definition` values.
    pub custom_block_index_search_names_offsets: Vec<u32>,
    /// `dtnm` chunk — offsets into `string_data` for data-field
    /// type names (used to distinguish different `data` field flavors).
    pub data_definition_name_offsets: Vec<u32>,
    /// `arr!` chunk.
    pub array_layouts: Vec<TagArrayLayout>,
    /// `tgft` chunk.
    pub field_types: Vec<TagFieldTypeLayout>,
    /// `gras` chunk. Field definitions are stored flat; each struct's
    /// fields are the range `[first_field_index .. first_field_index +
    /// n]` where `n` is the count of fields up to and including the
    /// `Terminator`.
    pub fields: Vec<TagFieldLayout>,
    /// `blv2` chunk (v2/v3) or reconstructed from v1 aggregate records.
    pub block_layouts: Vec<TagBlockLayout>,
    /// `rcv2` chunk. Empty in v1.
    pub resource_layouts: Vec<TagResourceLayout>,
    /// `]==[` chunk. Empty in v1/v2.
    pub interop_layouts: Vec<TagInteropLayout>,
    /// `stv2` chunk (v2/v3) or reconstructed from v1 aggregate records.
    pub struct_layouts: Vec<TagStructLayout>,
    /// Classic Halo 2 only: per-struct 4-char group tag (0 = none),
    /// parallel to `struct_layouts`. Non-zero for inline structs that
    /// carry a 16-byte block-style header on disk (e.g. `MAPP`). Empty
    /// for MCC layouts (read from `blay`), which have no such headers.
    pub struct_tags: Vec<u32>,
    /// Classic Halo 2 only: per-base-struct version variant table,
    /// parallel to `struct_layouts`. `Some(v)` for a multi-version layout,
    /// where `v[n]` is the `struct_layouts` index of the variant for
    /// on-disk version `n` (gaps padded with the base index). `None` for
    /// single-version structs and for the variant entries themselves.
    /// Empty for MCC layouts. The classic decoder reads a block/struct
    /// header's version field and resolves the matching FieldSet variant.
    pub struct_version_table: Vec<Option<Vec<u32>>>,
    /// Which template group each `tmpl` custom field stands in for.
    ///
    /// A `custom` field tagged `tmpl` is a fixed-size *hole* in a struct: the
    /// bytes of another group's render method, inlined without a field list.
    /// Halo 3 spells that method out by name; Reach onward hide the same ~100
    /// bytes behind the hole. The size already reaches
    /// [`TagFieldLayout::definition`] because the layout arithmetic needs it —
    /// what it does not say is *which* template those bytes belong to, and
    /// without that a comparison can only skip them and a converter can only
    /// guess whether two holes hold the same thing.
    ///
    /// Sparse and sorted by `field_index`, in the manner of [`Self::struct_tags`].
    /// **Empty for a layout parsed from a tag's own `blay`**: a shipped layout
    /// records the hole but not its provenance, so this is populated only by
    /// [`TagLayout::from_json`], which can read the template name out of the
    /// schema. Deliberately additive — it changes no size, field list or offset,
    /// so a tag still builds byte-for-byte the way the kits write it.
    pub tmpl_holes: Vec<TagTemplateHole>,
}

/// One `tmpl` custom hole: where it sits, what template fills it, how wide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TagTemplateHole {
    /// Index into [`TagLayout::fields`].
    pub field_index: u32,
    /// Group tag of the template the hole resolves to (e.g. `?rmp`).
    pub group_tag: u32,
    /// Bytes the hole occupies — the template's inherited root-struct chain.
    /// Zero for a template that resolves to nothing, which is still worth
    /// recording: the hole has an identity even when it has no width.
    pub size: u32,
}

impl TagLayout {
    /// The template hole at `field_index`, if that field is one.
    pub fn template_hole(&self, field_index: usize) -> Option<&TagTemplateHole> {
        self.tmpl_holes
            .binary_search_by_key(&(field_index as u32), |hole| hole.field_index)
            .ok()
            .map(|position| &self.tmpl_holes[position])
    }

    /// Resolve a `name_offset` into the UTF-8 string at that position
    /// in [`Self::string_data`] (the stored data is null-terminated).
    /// Returns `None` for an out-of-range offset.
    pub fn get_string(&self, offset: u32) -> Option<&str> {
        let start_offset = offset as usize;
        let mut end_offset = start_offset;

        if start_offset >= self.string_data.len() {
            return None;
        }

        while end_offset < self.string_data.len() {
            if self.string_data[end_offset] == 0 {
                break;
            }

            end_offset += 1;
        }

        str::from_utf8(&self.string_data[start_offset..end_offset]).ok()
    }

    /// Number of direct child chunks a struct produces when serialized — i.e. how many
    /// of its fields have `needs_sub_chunk` (struct/block/tag_reference/string_id/data/
    /// pageable/api_interop). Matches `_calculate_structure_expected_children_by_entry`
    /// in Blam-Creation-Suite (tag_file_reader.cpp).
    pub fn get_struct_expected_children(&self, struct_index: usize) -> u32 {
        let mut count = 0;
        let mut field_index = self.struct_layouts[struct_index].first_field_index as usize;

        loop {
            let field = &self.fields[field_index];

            if field.field_type == TagFieldType::Terminator {
                return count;
            }

            let field_type = &self.field_types[field.type_index as usize];

            if field_type.needs_sub_chunk != 0 {
                count += 1;
            }

            field_index += 1;
        }
    }

    /// Compute [`TagStructLayout::size`] and each
    /// [`TagFieldLayout::offset`] for `struct_index` by walking its
    /// fields, recursing into nested struct/array fields first so their
    /// sizes are known before we accumulate. Idempotent — re-running on
    /// an already-laid-out struct is a no-op.
    pub fn compute_struct_layout(&mut self, struct_index: usize) {
        if self.struct_layouts[struct_index].size != 0 {
            return;
        }

        let mut size = 0;
        let mut field_index = self.struct_layouts[struct_index].first_field_index as usize;

        let mut done = false;

        while !done {
            // Record this field's START offset before accumulating its size.
            self.fields[field_index].offset = size as u32;

            let field = &self.fields[field_index];

            if field.field_type == TagFieldType::Terminator {
                done = true;
            } else if field.field_type == TagFieldType::Struct {
                let child_index = field.definition as usize;
                self.compute_struct_layout(child_index);
                size += self.struct_layouts[child_index].size;
            } else if field.field_type == TagFieldType::Array {
                let (array_struct_index, count) = {
                    let array_layout = &self.array_layouts[field.definition as usize];
                    (array_layout.struct_index as usize, array_layout.count)
                };
                self.compute_struct_layout(array_struct_index);
                size += self.struct_layouts[array_struct_index].size * count as usize;
            } else if field.field_type == TagFieldType::Pad || field.field_type == TagFieldType::Skip {
                size += field.definition as usize;
            } else if field.field_type == TagFieldType::Custom {
                // For `tmpl` customs the JSON importer stashes the
                // template's parent-chain expansion size into the
                // `definition` slot (non-zero means bytes to inline
                // here). Zero for all other custom subtypes (UI-only
                // hints like `hide`/`edih`/`vert`/etc.).
                size += field.definition as usize;
            } else {
                size += self.field_types[field.type_index as usize].size as usize;
            }

            field_index += 1;
        }

        self.struct_layouts[struct_index].size = size;
    }

    /// Parse a `blay` chunk: outer chunk header, 24-byte payload
    /// header (`root_data_size` + `guid[16]` + payload `version`),
    /// then the layout body. The payload `version` (1/2/3/4) controls
    /// which layout-header fields are present and whether the body is
    /// wrapped in a `tgly` chunk with per-section sub-chunk headers.
    ///
    /// After parsing all records, resolves each field's
    /// [`TagFieldType`] and computes the size/offset of every struct
    /// so the data-layer parsing can dispatch cheaply.
    pub fn read<R: Seek + Read>(
        reader: &mut std::io::BufReader<R>,
        endian: Endian,
    ) -> Result<Self, TagReadError> {
        //================================================================================
        // Outer blay chunk header + 24-byte payload header.
        //
        // The blay chunk-header version is always 2 (a wire-format
        // version distinct from the payload version we read below).
        // Use the unvalidated header reader because
        // `read_validated_chunk_header` checks for version == 0, which
        // doesn't apply here.
        //================================================================================

        let blay_header_offset = reader.stream_position()?;
        let blay_header = read_chunk_header(reader, endian)?;
        if blay_header.signature != u32::from_be_bytes(*b"blay") {
            return Err(TagReadError::BadChunkSignature {
                offset: blay_header_offset,
                expected: *b"blay",
                got: blay_header.signature.to_be_bytes(),
            });
        }
        if blay_header.version != 2 {
            return Err(TagReadError::BadChunkVersion {
                chunk: "blay",
                version: blay_header.version,
            });
        }

        let blay_offset = reader.stream_position()?;

        let root_data_size = read_u32(reader, endian)?;
        let guid = read_u8_array(reader)?;
        let version = read_u32(reader, endian)?;
        let block_layout_version = version;

        if !matches!(block_layout_version, 1..=4) {
            return Err(TagReadError::UnsupportedLayoutVersion(block_layout_version));
        }

        let mut string_data;
        let mut string_offsets;
        let mut string_lists;
        let mut custom_block_index_search_names_offsets;
        let mut data_definition_name_offsets;
        let mut array_layouts;
        let mut field_types;
        let mut field_layouts;
        let mut block_layouts;
        let mut resource_layouts;
        let mut interop_layouts;
        let mut struct_layouts;

        //================================================================================
        // Read the tag layout header
        //================================================================================

        let header = TagLayoutHeader {
            tag_group_block_index: if matches!(block_layout_version, 2..=4) { read_u32(reader, endian)? } else { 0 },
            string_data_size: read_u32(reader, endian)?,
            string_offset_count: read_u32(reader, endian)?,
            string_list_count: read_u32(reader, endian)?,
            custom_block_index_search_names_count: read_u32(reader, endian)?,
            data_definition_name_count: read_u32(reader, endian)?,
            array_layout_count: read_u32(reader, endian)?,
            field_type_count: read_u32(reader, endian)?,
            field_count: read_u32(reader, endian)?,
            aggregate_layout_count: if block_layout_version == 1 { read_u32(reader, endian)? } else { 0 },
            struct_layout_count: if matches!(block_layout_version, 2..=4) { read_u32(reader, endian)? } else { 0 },
            block_layout_count: if matches!(block_layout_version, 2..=4) { read_u32(reader, endian)? } else { 0 },
            resource_layout_count: if matches!(block_layout_version, 2..=4) { read_u32(reader, endian)? } else { 0 },
            interop_layout_count: if matches!(block_layout_version, 3 | 4) { read_u32(reader, endian)? } else { 0 },
        };

        //================================================================================
        // Read the tag layout chunk header (if present).
        //
        // tgly's chunk-header version field carries the layout version
        // (2/3/4), not 0, so we can't use `read_validated_chunk_header`.
        //================================================================================

        let tag_layout_header_and_offset = if block_layout_version > 1 {
            let tgly_offset = reader.stream_position()?;
            let tag_layout_header = read_chunk_header(reader, endian)?;
            if tag_layout_header.signature != u32::from_be_bytes(*b"tgly") {
                return Err(TagReadError::BadChunkSignature {
                    offset: tgly_offset,
                    expected: *b"tgly",
                    got: tag_layout_header.signature.to_be_bytes(),
                });
            }
            if tag_layout_header.version != block_layout_version {
                return Err(TagReadError::BadChunkVersion {
                    chunk: "tgly",
                    version: tag_layout_header.version,
                });
            }
            Some((tag_layout_header, reader.stream_position()?))
        } else {
            None
        };

        //================================================================================
        // Read the string data
        //================================================================================

        if block_layout_version > 1 {
            let string_data_header = read_validated_chunk_header(reader, *b"str*", "str*", endian)?;
            if header.string_data_size != string_data_header.size {
                return Err(TagReadError::CountMismatch {
                    chunk: "str*",
                    header_count: header.string_data_size,
                    derived_count: string_data_header.size,
                });
            }
        }

        string_data = vec![0u8; header.string_data_size as _];
        reader.read_exact(string_data.as_mut_slice())?;

        //================================================================================
        // Read the string offsets
        //================================================================================

        if block_layout_version > 1 {
            let string_offsets_header = read_validated_chunk_header(reader, *b"sz+x", "sz+x", endian)?;
            check_count_matches_size(
                "sz+x",
                header.string_offset_count,
                string_offsets_header.size,
                std::mem::size_of::<u32>() as u32,
            )?;
        }

        string_offsets = vec![0; header.string_offset_count as usize];

        for slot in &mut string_offsets {
            *slot = read_u32(reader, endian)?;
        }

        //================================================================================
        // Read the string lists
        //================================================================================

        if block_layout_version > 1 {
            let string_lists_header = read_validated_chunk_header(reader, *b"sz[]", "sz[]", endian)?;
            check_count_matches_size("sz[]", header.string_list_count, string_lists_header.size, 12)?;
        }

        string_lists = Vec::with_capacity(header.string_list_count as usize);

        for _ in 0..header.string_list_count {
            string_lists.push(TagStringList {
                offset: read_u32(reader, endian)?,
                count: read_u32(reader, endian)?,
                first: read_u32(reader, endian)?,
            });
        }

        //================================================================================
        // Read the custom block index search names
        //================================================================================

        if block_layout_version > 1 {
            let csbn_header = read_validated_chunk_header(reader, *b"csbn", "csbn", endian)?;
            check_count_matches_size(
                "csbn",
                header.custom_block_index_search_names_count,
                csbn_header.size,
                std::mem::size_of::<u32>() as u32,
            )?;
        }

        custom_block_index_search_names_offsets = Vec::with_capacity(header.custom_block_index_search_names_count as usize);

        for _ in 0..header.custom_block_index_search_names_count {
            custom_block_index_search_names_offsets.push(read_u32(reader, endian)?);
        }

        //================================================================================
        // Read the data definition names
        //================================================================================

        if block_layout_version > 1 {
            let dtnm_header = read_validated_chunk_header(reader, *b"dtnm", "dtnm", endian)?;
            check_count_matches_size(
                "dtnm",
                header.data_definition_name_count,
                dtnm_header.size,
                std::mem::size_of::<u32>() as u32,
            )?;
        }

        data_definition_name_offsets = vec![0; header.data_definition_name_count as usize];

        for slot in &mut data_definition_name_offsets {
            *slot = read_u32(reader, endian)?;
        }

        //================================================================================
        // Read the array definitions
        //================================================================================

        if block_layout_version > 1 {
            let arr_header = read_validated_chunk_header(reader, *b"arr!", "arr!", endian)?;
            check_count_matches_size("arr!", header.array_layout_count, arr_header.size, 12)?;
        }

        array_layouts = Vec::with_capacity(header.array_layout_count as usize);

        for _ in 0..header.array_layout_count {
            array_layouts.push(TagArrayLayout {
                name_offset: read_u32(reader, endian)?,
                count: read_u32(reader, endian)?,
                struct_index: read_u32(reader, endian)?,
            });
        }

        //================================================================================
        // Read the field types
        //================================================================================

        if block_layout_version > 1 {
            let tgft_header = read_validated_chunk_header(reader, *b"tgft", "tgft", endian)?;
            check_count_matches_size("tgft", header.field_type_count, tgft_header.size, 12)?;
        }

        field_types = Vec::with_capacity(header.field_type_count as usize);

        for _ in 0..header.field_type_count {
            field_types.push(TagFieldTypeLayout {
                name_offset: read_u32(reader, endian)?,
                size: read_u32(reader, endian)?,
                needs_sub_chunk: read_u32(reader, endian)?,
            });
        }

        //================================================================================
        // Read the fields
        //================================================================================

        if block_layout_version > 1 {
            let gras_header = read_validated_chunk_header(reader, *b"gras", "gras", endian)?;
            check_count_matches_size("gras", header.field_count, gras_header.size, 12)?;
        }

        field_layouts = Vec::with_capacity(header.field_count as usize);

        for _ in 0..header.field_count {
            field_layouts.push(TagFieldLayout {
                name_offset: read_u32(reader, endian)?,
                type_index: read_u32(reader, endian)?,
                definition: read_u32(reader, endian)?,
                field_type: TagFieldType::Unknown,
                offset: 0,
            });
        }

        if block_layout_version == 1 {
            //================================================================================
            // Read the aggregate definitions
            //================================================================================

            block_layouts = Vec::with_capacity(header.aggregate_layout_count as usize);
            struct_layouts = Vec::with_capacity(header.aggregate_layout_count as usize);

            for i in 0..header.aggregate_layout_count {
                // Convert v1 agro records (28 bytes: guid[16] + name_offset + max_count + first_field_index)
                // into stv2 (24 bytes: guid[16] + name_offset + first_field_index)
                // and blv2 (12 bytes: name_offset + max_count + struct_index) format.
                let guid = read_u8_array(reader)?;
                let name_offset = read_u32(reader, endian)?;
                let max_count = read_u32(reader, endian)?;
                let first_field_index = read_u32(reader, endian)?;

                struct_layouts.push(TagStructLayout {
                    index: i,
                    guid,
                    name_offset,
                    first_field_index,
                    size: 0,
                    version: 0,
                });

                block_layouts.push(TagBlockLayout {
                    index: i,
                    name_offset,
                    max_count,
                    struct_index: i as _,
                });
            }

            // Not present in V1:
            resource_layouts = vec![];
            interop_layouts = vec![];
        } else {
            //================================================================================
            // Read the block definitions
            //================================================================================

            let block_layout_header = read_validated_chunk_header(reader, *b"blv2", "blv2", endian)?;
            check_count_matches_size("blv2", header.block_layout_count, block_layout_header.size, 12)?;

            block_layouts = Vec::with_capacity(header.block_layout_count as usize);

            for i in 0..header.block_layout_count {
                block_layouts.push(TagBlockLayout {
                    index: i,
                    name_offset: read_u32(reader, endian)?,
                    max_count: read_u32(reader, endian)?,
                    struct_index: read_u32(reader, endian)?,
                });
            }

            //================================================================================
            // Read the resource definitions
            //================================================================================

            let rcv2_header = read_validated_chunk_header(reader, *b"rcv2", "rcv2", endian)?;
            check_count_matches_size("rcv2", header.resource_layout_count, rcv2_header.size, 12)?;

            resource_layouts = Vec::with_capacity(header.resource_layout_count as usize);

            for _ in 0..header.resource_layout_count {
                resource_layouts.push(TagResourceLayout {
                    name_offset: read_u32(reader, endian)?,
                    unknown: read_u32(reader, endian)?,
                    struct_index: read_u32(reader, endian)?,
                });
            }

            //================================================================================
            // Read the interop definitions (not present in V2; present in V3 and V4)
            //================================================================================

            interop_layouts = vec![];

            if matches!(block_layout_version, 3 | 4) {
                let interop_header = read_validated_chunk_header(reader, *b"]==[", "]==[", endian)?;
                check_count_matches_size("]==[", header.interop_layout_count, interop_header.size, 24)?;

                interop_layouts.reserve(header.interop_layout_count as usize);

                for _ in 0..header.interop_layout_count {
                    interop_layouts.push(TagInteropLayout {
                        name_offset: read_u32(reader, endian)?,
                        struct_index: read_u32(reader, endian)?,
                        guid: read_u8_array(reader)?,
                    });
                }
            }

            //================================================================================
            // Read the struct definitions (stv2 for V2/V3, stv4 for V4).
            // stv4 adds a trailing `version: u32` per record → 28 bytes;
            // stv2 is 24 bytes.
            //================================================================================

            let (expected_struct_sig, struct_chunk_name, struct_record_size) = if block_layout_version == 4 {
                (*b"stv4", "stv4", 28u32)
            } else {
                (*b"stv2", "stv2", 24u32)
            };

            let struct_layouts_header =
                read_validated_chunk_header(reader, expected_struct_sig, struct_chunk_name, endian)?;
            check_count_matches_size(
                struct_chunk_name,
                header.struct_layout_count,
                struct_layouts_header.size,
                struct_record_size,
            )?;

            struct_layouts = Vec::with_capacity(header.struct_layout_count as usize);

            for i in 0..header.struct_layout_count {
                let guid = read_u8_array(reader)?;
                let name_offset = read_u32(reader, endian)?;
                let first_field_index = read_u32(reader, endian)?;
                let version = if block_layout_version == 4 { read_u32(reader, endian)? } else { 0 };
                struct_layouts.push(TagStructLayout {
                    index: i,
                    guid,
                    name_offset,
                    first_field_index,
                    size: 0,
                    version,
                });
            }
        }

        //================================================================================
        // Finished reading the tag layout chunk
        //================================================================================

        if let Some((tag_layout_header, tag_layout_offset)) = tag_layout_header_and_offset {
            check_chunk_end(reader, "tgly", tag_layout_offset, tag_layout_header.size)?;
        }

        check_chunk_end(reader, "blay", blay_offset, blay_header.size)?;

        let mut result = Self {
            root_data_size,
            guid,
            version,
            header,
            string_data,
            string_offsets,
            string_lists,
            custom_block_index_search_names_offsets,
            data_definition_name_offsets,
            array_layouts,
            field_types,
            fields: field_layouts,
            block_layouts,
            struct_layouts,
            resource_layouts,
            interop_layouts,
            // MCC tags carry no classic inline-struct headers.
            struct_tags: Vec::new(),
            // A shipped `blay` records the hole but never says which template
            // it stands in for, so there is nothing honest to put here.
            tmpl_holes: Vec::new(),
            // MCC tags are single-version (no on-disk FieldSet selection).
            struct_version_table: Vec::new(),
        };

        //================================================================================
        // Resolve each field's type-name string into a TagFieldType enum once,
        // so the hot parse loops can dispatch on the enum instead of re-walking
        // string_data and comparing strings for every field read.
        //================================================================================

        for i in 0..result.fields.len() {
            let type_name_offset = result.field_types[result.fields[i].type_index as usize].name_offset;
            let name = result.get_string(type_name_offset).ok_or(TagReadError::InvalidUtf8 {
                context: "layout field type name",
            })?;
            result.fields[i].field_type = TagFieldType::from_name(name);
        }

        for i in 0..result.struct_layouts.len() {
            result.compute_struct_layout(i);
        }

        Ok(result)
    }

    /// Write this layout as a `blay` chunk. Mirrors [`TagLayout::read`]:
    /// outer `blay` chunk header (always version 2), 24-byte payload
    /// header (root data size, guid, layout version), then the layout
    /// body. The body shape depends on the payload version (`self.version`):
    /// v1 emits flat records directly; v2/v3/v4 wrap them in a `tgly`
    /// chunk with per-section sub-chunks (`str*`, `sz+x`, `sz[]`,
    /// `csbn`, `dtnm`, `arr!`, `tgft`, `gras`, `blv2`, `rcv2`, optional
    /// `]==[`, and `stv2` for v2/v3 / `stv4` for v4).
    pub fn write<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        // Buffer the blay body into a Vec so we can emit the outer
        // chunk header with the correct size without pre-computing.
        let mut body = Vec::new();
        body.extend_from_slice(&self.root_data_size.to_le_bytes());
        body.extend_from_slice(&self.guid);
        body.extend_from_slice(&self.version.to_le_bytes());

        self.write_body(&mut body)?;

        write_tag_chunk_content(writer, u32::from_be_bytes(*b"blay"), 2, &body)
    }

    /// Write the layout body (everything after the 24-byte payload
    /// header). Private — [`TagLayout::write`] owns the outer chunk
    /// wrapping. Uses `self.version` to drive the conditional shape.
    fn write_body<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        let block_layout_version = self.version;

        //============================================================
        // Layout header
        //============================================================

        if matches!(block_layout_version, 2..=4) {
            writer.write_all(&self.header.tag_group_block_index.to_le_bytes())?;
        }
        writer.write_all(&self.header.string_data_size.to_le_bytes())?;
        writer.write_all(&self.header.string_offset_count.to_le_bytes())?;
        writer.write_all(&self.header.string_list_count.to_le_bytes())?;
        writer.write_all(&self.header.custom_block_index_search_names_count.to_le_bytes())?;
        writer.write_all(&self.header.data_definition_name_count.to_le_bytes())?;
        writer.write_all(&self.header.array_layout_count.to_le_bytes())?;
        writer.write_all(&self.header.field_type_count.to_le_bytes())?;
        writer.write_all(&self.header.field_count.to_le_bytes())?;
        if block_layout_version == 1 {
            writer.write_all(&self.header.aggregate_layout_count.to_le_bytes())?;
        }
        if matches!(block_layout_version, 2..=4) {
            writer.write_all(&self.header.struct_layout_count.to_le_bytes())?;
            writer.write_all(&self.header.block_layout_count.to_le_bytes())?;
            writer.write_all(&self.header.resource_layout_count.to_le_bytes())?;
        }
        if matches!(block_layout_version, 3 | 4) {
            writer.write_all(&self.header.interop_layout_count.to_le_bytes())?;
        }

        //============================================================
        // v1: flat records, no tgly wrapper, no section headers
        //============================================================

        if block_layout_version == 1 {
            self.write_layout_chunks(block_layout_version, writer)?;
            return Ok(());
        }

        //============================================================
        // v2/v3/v4: tgly chunk wrapping per-section chunks. Size is
        // precomputed so the body streams directly to `writer` without
        // buffering into a Vec. tgly.version == block_layout_version
        // (hypothesis-verified on read).
        //============================================================

        assert!(matches!(block_layout_version, 2..=4));

        write_tag_chunk_header(
            writer,
            u32::from_be_bytes(*b"tgly"),
            block_layout_version,
            self.compute_layout_size(block_layout_version),
        )?;
        self.write_layout_chunks(block_layout_version, writer)?;

        Ok(())
    }

    /// Precompute the exact number of bytes `write_layout_chunks` will emit
    /// for a v2/v3/v4 layout, so the enclosing `tgly` chunk header can be
    /// written with the correct size without buffering the body. Each
    /// section is a 12-byte chunk header plus `count * chunk_size`. The
    /// struct-definitions section is 24 bytes/record in `stv2` (v2/v3)
    /// and 28 bytes/record in `stv4` (v4).
    fn compute_layout_size(&self, block_layout_version: u32) -> u32 {
        assert!(matches!(block_layout_version, 2..=4));

        let section_size = |chunk_count: usize, chunk_size: usize| -> u32 {
            (12 + chunk_count * chunk_size) as u32
        };

        let struct_record_size = if block_layout_version == 4 { 28 } else { 24 };

        let mut size = 0u32;
        size += section_size(self.string_data.len(), 1);
        size += section_size(self.string_offsets.len(), size_of::<u32>());
        size += section_size(self.string_lists.len(), 12);
        size += section_size(self.custom_block_index_search_names_offsets.len(), size_of::<u32>());
        size += section_size(self.data_definition_name_offsets.len(), size_of::<u32>());
        size += section_size(self.array_layouts.len(), 12);
        size += section_size(self.field_types.len(), 12);
        size += section_size(self.fields.len(), 12);
        size += section_size(self.block_layouts.len(), 12);
        size += section_size(self.resource_layouts.len(), 12);
        if matches!(block_layout_version, 3 | 4) {
            size += section_size(self.interop_layouts.len(), 24);
        }
        size += section_size(self.struct_layouts.len(), struct_record_size);
        size
    }

    /// Write the body of the `tgly` chunk (v2/v3/v4) or the flat chunk
    /// stream (v1). Both forms emit the same logical chunk sequence;
    /// v2/v3/v4 wraps each chunk group in a chunk header, v1 emits raw.
    fn write_layout_chunks<W: Write>(
        &self,
        block_layout_version: u32,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let wrap_sections = matches!(block_layout_version, 2..=4);

        //------------------------------------------------------------
        // str*  — string data
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"str*"),
                0,
                self.string_data.len() as u32,
            )?;
        }
        writer.write_all(&self.string_data)?;

        //------------------------------------------------------------
        // sz+x  — string offsets (u32 each)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"sz+x"),
                0,
                (self.string_offsets.len() * size_of::<u32>()) as u32,
            )?;
        }
        for string_offset in &self.string_offsets {
            writer.write_all(&string_offset.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // sz[]  — string lists (12 bytes each: offset, count, first)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"sz[]"),
                0,
                (self.string_lists.len() * 12) as u32,
            )?;
        }
        for string_list in &self.string_lists {
            writer.write_all(&string_list.offset.to_le_bytes())?;
            writer.write_all(&string_list.count.to_le_bytes())?;
            writer.write_all(&string_list.first.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // csbn  — custom block index search name offsets (u32 each)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"csbn"),
                0,
                (self.custom_block_index_search_names_offsets.len() * size_of::<u32>()) as u32,
            )?;
        }
        for name_offset in &self.custom_block_index_search_names_offsets {
            writer.write_all(&name_offset.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // dtnm  — data definition name offsets (u32 each)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"dtnm"),
                0,
                (self.data_definition_name_offsets.len() * size_of::<u32>()) as u32,
            )?;
        }
        for name_offset in &self.data_definition_name_offsets {
            writer.write_all(&name_offset.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // arr!  — array definitions (12 bytes each)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"arr!"),
                0,
                (self.array_layouts.len() * 12) as u32,
            )?;
        }
        for array_layout in &self.array_layouts {
            writer.write_all(&array_layout.name_offset.to_le_bytes())?;
            writer.write_all(&array_layout.count.to_le_bytes())?;
            writer.write_all(&array_layout.struct_index.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // tgft  — field types (12 bytes each)
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"tgft"),
                0,
                (self.field_types.len() * 12) as u32,
            )?;
        }
        for field_type_layout in &self.field_types {
            writer.write_all(&field_type_layout.name_offset.to_le_bytes())?;
            writer.write_all(&field_type_layout.size.to_le_bytes())?;
            writer.write_all(&field_type_layout.needs_sub_chunk.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // gras  — field definitions (12 bytes each: name_offset,
        // type_index, definition). The in-memory TagFieldLayout also
        // carries derived `field_type` / `offset`, which are not
        // serialized — they're recomputed from the layout at read time.
        //------------------------------------------------------------
        if wrap_sections {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"gras"),
                0,
                (self.fields.len() * 12) as u32,
            )?;
        }
        for field_layout in &self.fields {
            writer.write_all(&field_layout.name_offset.to_le_bytes())?;
            writer.write_all(&field_layout.type_index.to_le_bytes())?;
            writer.write_all(&field_layout.definition.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // v1 aggregate definitions: flat 28-byte records reconstructed
        // from paired struct/block definitions (1:1, by index).
        //------------------------------------------------------------
        if block_layout_version == 1 {
            assert_eq!(self.struct_layouts.len(), self.block_layouts.len());
            for i in 0..self.struct_layouts.len() {
                let struct_layout = &self.struct_layouts[i];
                let block_layout = &self.block_layouts[i];
                writer.write_all(&struct_layout.guid)?;
                writer.write_all(&struct_layout.name_offset.to_le_bytes())?;
                writer.write_all(&block_layout.max_count.to_le_bytes())?;
                writer.write_all(&struct_layout.first_field_index.to_le_bytes())?;
            }
            return Ok(());
        }

        //------------------------------------------------------------
        // blv2  — block definitions (12 bytes each)
        //------------------------------------------------------------
        write_tag_chunk_header(
            writer,
            u32::from_be_bytes(*b"blv2"),
            0,
            (self.block_layouts.len() * 12) as u32,
        )?;
        for block_layout in &self.block_layouts {
            writer.write_all(&block_layout.name_offset.to_le_bytes())?;
            writer.write_all(&block_layout.max_count.to_le_bytes())?;
            writer.write_all(&block_layout.struct_index.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // rcv2  — resource definitions (12 bytes each)
        //------------------------------------------------------------
        write_tag_chunk_header(
            writer,
            u32::from_be_bytes(*b"rcv2"),
            0,
            (self.resource_layouts.len() * 12) as u32,
        )?;
        for resource_layout in &self.resource_layouts {
            writer.write_all(&resource_layout.name_offset.to_le_bytes())?;
            writer.write_all(&resource_layout.unknown.to_le_bytes())?;
            writer.write_all(&resource_layout.struct_index.to_le_bytes())?;
        }

        //------------------------------------------------------------
        // ]==[  — interop definitions (v3 and v4; 24 bytes each)
        //------------------------------------------------------------
        if matches!(block_layout_version, 3 | 4) {
            write_tag_chunk_header(
                writer,
                u32::from_be_bytes(*b"]==["),
                0,
                (self.interop_layouts.len() * 24) as u32,
            )?;
            for interop_layout in &self.interop_layouts {
                writer.write_all(&interop_layout.name_offset.to_le_bytes())?;
                writer.write_all(&interop_layout.struct_index.to_le_bytes())?;
                writer.write_all(&interop_layout.guid)?;
            }
        }

        //------------------------------------------------------------
        // stv2 / stv4  — struct definitions.
        // `size` is derived at read time via compute_struct_layout and
        // is not serialized. v4 adds a trailing `version: u32` per
        // record → 28 bytes/record; v2/v3 is 24.
        //------------------------------------------------------------
        let (struct_sig, struct_record_size) = if block_layout_version == 4 {
            (u32::from_be_bytes(*b"stv4"), 28usize)
        } else {
            (u32::from_be_bytes(*b"stv2"), 24usize)
        };
        write_tag_chunk_header(
            writer,
            struct_sig,
            0,
            (self.struct_layouts.len() * struct_record_size) as u32,
        )?;
        for struct_layout in &self.struct_layouts {
            writer.write_all(&struct_layout.guid)?;
            writer.write_all(&struct_layout.name_offset.to_le_bytes())?;
            writer.write_all(&struct_layout.first_field_index.to_le_bytes())?;
            if block_layout_version == 4 {
                writer.write_all(&struct_layout.version.to_le_bytes())?;
            }
        }

        Ok(())
    }
}
