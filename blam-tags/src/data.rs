//! Tag data tree: the per-tag instance values shaped by a layout.
//!
//! Byte ownership is **per block**. Each [`TagBlockData`] owns a single
//! `raw_data` buffer holding all of its elements' bytes laid out
//! contiguously. Nested structs, inline arrays, and exploded
//! pageable-resource payloads are *offset regions* inside their
//! enclosing block's `raw_data` — they don't own bytes of their own.
//! Navigating into a sub-block starts a fresh byte region (the
//! sub-block's own `raw_data`).
//!
//! This matches the on-disk `tgbl` chunk layout 1:1: `count + flags +
//! concatenated element bytes + per-element tgst sub-chunks`.

use std::io::{Read, Seek, SeekFrom, Write};

use crate::error::TagReadError;
use crate::fields::{deserialize_field, serialize_field, TagFieldData, TagFieldType};
use crate::io::*;
use crate::layout::{TagBlockLayout, TagLayout, TagStructLayout};
use crate::monolithic::XSyncState;

/// A struct within a tag's data tree. Owns its `sub_chunks` (nested
/// structures + leaf sub-chunks); its *bytes* live in the enclosing
/// [`TagBlockData::raw_data`] at an offset determined by path descent.
#[derive(Debug, Clone)]
pub(crate) struct TagStructData {
    /// Index into [`TagLayout::struct_layouts`].
    pub(crate) struct_index: u32,
    /// Classic **Halo 2** only: the 16-byte block-style header that
    /// precedes a tag'd inline struct's fields on disk (e.g. `MAPP`).
    /// `None` for MCC, Halo CE, and untagged H2 structs. Preserved
    /// verbatim for byte-exact write.
    pub(crate) classic_struct_header: Option<Vec<u8>>,
    /// Sub-chunks emitted inside this struct's `tgst` chunk, in
    /// emission order. Only populated for fields whose type needs a
    /// sub-chunk. The tgst chunk itself has no raw bytes of its
    /// own — the parent block's `raw_data` carries them.
    pub(crate) sub_chunks: Vec<TagSubChunkEntry>,
}

/// One entry in a `tgst` chunk's sub-chunk list. Pairs an owning
/// layout field index with the entry's typed payload so the writer
/// can re-emit each child chunk in its original position.
#[derive(Debug, Clone)]
pub(crate) struct TagSubChunkEntry {
    /// Index into [`TagLayout::fields`] for the owning field, or
    /// `None` for empty placeholder `tgst` chunks that don't
    /// correspond to any layout field. See
    /// [`TagSubChunkContent::EmptyPlaceholder`].
    pub(crate) field_index: Option<u32>,
    /// Typed payload for this entry (struct / block / array / leaf
    /// chunk / resource / placeholder).
    pub(crate) content: TagSubChunkContent,
}

/// Per-shape payload for a `tgst` sub-chunk entry. The variant
/// reflects the on-disk chunk signature; bytes for primitive leaves
/// are preserved verbatim so writes are byte-exact.
#[derive(Debug, Clone)]
pub(crate) enum TagSubChunkContent {
    /// Nested struct field. Its raw bytes live in the enclosing
    /// block's `raw_data` at the field's offset within the containing
    /// struct.
    Struct(TagStructData),
    /// Nested block field. Starts a new byte region — the block
    /// carries its own `raw_data`.
    Block(TagBlockData),
    /// Inline fixed-count array. Each element's raw bytes live in the
    /// enclosing block's `raw_data` at `field.offset + i *
    /// element_size`. The vector length equals the schema-declared
    /// array count.
    Array(Vec<TagStructData>),
    /// `tgrf` chunk payload (4-byte group_tag + null-terminated path).
    /// Header is implicit — signature and size are reconstructible on
    /// write.
    TagReference(Vec<u8>),
    /// `tgsi` chunk payload (utf-8 bytes, empty = string_id::NONE).
    StringId(Vec<u8>),
    /// `tgsi` chunk payload for old-style string ids.
    OldStringId(Vec<u8>),
    /// `tgda` chunk payload.
    Data(Vec<u8>),
    /// `[]it` chunk payload for an `api_interop` field. In the
    /// observed corpus the payload is a fixed 12 bytes matching BCS's
    /// `s_tag_interop { descriptor: u32, address: u32,
    /// definition_address: u32 }`, but we preserve the raw bytes
    /// verbatim so future variants with different sizes still
    /// roundtrip byte-exactly.
    ApiInterop(Vec<u8>),
    /// Pageable resource. Signature distinguishes between concrete
    /// resource chunk shapes. Only the two observed in Halo 3 / Reach
    /// tags are modeled.
    Resource(TagResourceChunk),
    /// An empty `tgst` chunk (size=0) that doesn't correspond to any
    /// layout field. MCC's writer emits these as a placeholder before
    /// the real tgst for a struct sub-chunk field, and as trailing
    /// filler at the end of some struct contents. Preserved verbatim
    /// (as the entry's position within the parent's `sub_chunks`) so
    /// write-side can re-emit them at the correct byte offset.
    EmptyPlaceholder,
}

/// Pageable-resource on-disk shape. The signature on the chunk
/// distinguishes the variants — only the two observed in Halo 3 /
/// Reach tags are modeled, with `Xsync` covering opaque future
/// payloads.
#[derive(Debug, Clone)]
pub(crate) enum TagResourceChunk {
    /// `tg\0c` — empty null resource.
    Null,
    /// `tgrc` — exploded/control resource. Wraps a nested `tgdt`
    /// payload blob and the resource's own struct tree. The resource
    /// struct's raw bytes (typically 8 inline bytes) live in the
    /// enclosing block's `raw_data` at the resource field's offset.
    ///
    /// Also the post-hydration shape for monolithic-cache xsync
    /// resources — see [`xsync_state`](Self::Exploded::xsync_state).
    Exploded {
        /// `tgdt` payload (content bytes only; header reconstructible
        /// on write).
        exploded: Vec<u8>,
        /// Nested resource struct tree (sub_chunks only).
        struct_data: TagStructData,
        /// For resources synthesized by the monolithic-cache
        /// hydration pass (`tgxc` → `Exploded`), the parsed xsync
        /// state — control_data (with fixups already applied via
        /// [`XSyncState::apply_control_fixups`] at hydration time is
        /// up to the consumer), root_address, pageable/optional
        /// fixups, and interop GUIDs. `None` for native MCC `tgrc`
        /// resources whose definition lives directly in
        /// `exploded[..struct_size]`. Not written back to disk —
        /// hydrated form serializes back as plain `tgrc`.
        xsync_state: Option<Box<XSyncState>>,
    },
    /// `tgxc` — XSync resource. Opaque payload from the tag stream
    /// (the metadata that points at cache-resident data on
    /// monolithic builds). MCC writes `version = 0`; the Halo 4
    /// X360 dev build writes `version = 3` with a different
    /// internal layout. The version is preserved here so the
    /// monolithic-hydration pass in [`crate::monolithic`] can pick
    /// the right xsync-state shape.
    Xsync { version: u32, payload: Vec<u8> },
}

/// Match a struct field's stored name against a lookup query, tolerating
/// a trailing `|<code>` suffix on the stored name.
///
/// Some definitions (notably the Halo 2 `model_animation_graph`) carry a
/// dumper artifact on block field names — e.g. `animations|ABCDCC` — where
/// the portion after `|` is a name-code, not part of the addressable field
/// name. Callers look the field up as plain `animations`, so we compare the
/// stored name's pre-`|` segment. An exact match (no `|` present) still
/// works because `split('|').next()` returns the whole string. Verified
/// collision-free across the H2/H2A/H3 jmad definitions.
pub(crate) fn field_name_matches(stored: Option<&str>, query: &str) -> bool {
    match stored {
        Some(s) => s.split('|').next() == Some(query),
        None => false,
    }
}

/// Heuristic: does this layout describe a Halo 2 classic tag? H2 classic
/// definitions carry per-struct tag signatures and/or a struct-version
/// table; MCC and Halo CE layouts have neither. Used to gate H2-only
/// block-header synthesis.
fn looks_like_h2_classic_layout(layout: &TagLayout) -> bool {
    layout.struct_tags.iter().any(|tag| *tag != 0)
        || layout.struct_version_table.iter().any(Option::is_some)
}

#[cfg(test)]
mod dirty_tests {
    use super::{TagBlockData, TagStructData};
    use crate::io::Endian;
    use crate::layout::TagLayout;

    #[test]
    fn add_element_preserves_versioned_classic_block_width() {
        let layout = TagLayout::from_json("../definitions/halo2_mcc/model.json").unwrap();
        let (block_index, base_struct_index, active_struct_index) = layout
            .block_layouts
            .iter()
            .enumerate()
            .find_map(|(block_index, block)| {
                let base_struct_index = block.struct_index as usize;
                let active_struct_index =
                    *layout.struct_version_table.get(base_struct_index)?.as_ref()?.first()? as usize;
                (layout.struct_layouts[active_struct_index].size
                    != layout.struct_layouts[base_struct_index].size)
                    .then_some((block_index, base_struct_index, active_struct_index))
            })
            .unwrap();
        let active_size = layout.struct_layouts[active_struct_index].size;
        assert_ne!(active_size, layout.struct_layouts[base_struct_index].size);

        let mut block = TagBlockData {
            block_index: block_index as u32,
            flags: 0,
            raw_data: vec![0; active_size],
            endian: Endian::Le,
            elements: vec![TagStructData::new_default(&layout, active_struct_index, Endian::Le)],
            classic_block_header: Some(vec![0; 16]),
            classic_structural_dirty: false,
            classic_trailing: None,
        };

        block.add_element(&layout);

        // The new element keeps the existing on-disk variant width, so
        // raw_data stays count * active_size (not base_size).
        assert_eq!(block.raw_data.len(), active_size * 2);
        assert_eq!(block.elements[1].struct_index as usize, active_struct_index);
        assert!(block.classic_structural_dirty);
    }

    #[test]
    fn empty_to_nonempty_and_back_marks_dirty_and_synthesizes_header() {
        let layout = TagLayout::from_json("../definitions/halo2_mcc/model.json").unwrap();
        let block_index = layout
            .block_layouts
            .iter()
            .position(|block| {
                layout
                    .get_string(block.name_offset)
                    .is_some_and(|name| name == "model_variant_block_2")
            })
            .unwrap();

        let mut block = TagBlockData {
            block_index: block_index as u32,
            flags: 0,
            raw_data: Vec::new(),
            endian: Endian::Le,
            elements: Vec::new(),
            classic_block_header: None,
            classic_structural_dirty: false,
            classic_trailing: None,
        };

        // Empty -> non-empty: a dfbt header is synthesized and the block is
        // marked dirty so the encoder rewrites the inline count word.
        block.add_element(&layout);
        assert!(block.classic_structural_dirty);
        let header = block.classic_block_header.as_deref().expect("header synthesized");
        assert_eq!(&header[0..4], b"dfbt");

        // Non-empty -> empty: clearing also marks dirty so the now-empty
        // block emits no trailing header/data on the next encode.
        block.classic_structural_dirty = false;
        block.clear();
        assert!(block.classic_structural_dirty);
        assert!(block.elements.is_empty());
    }
}

impl TagStructData {
    /// Parse a `tgst` chunk.
    ///
    /// This method parses only the `tgst` header and its sub-chunks
    /// from `reader`; the raw bytes themselves stay in the enclosing
    /// block's `raw_data`.
    pub(crate) fn read<R: Seek + Read>(
        layout: &TagLayout,
        definition: &TagStructLayout,
        reader: &mut std::io::BufReader<R>,
        endian: Endian,
    ) -> Result<Self, TagReadError> {
        let tag_struct_header_offset = reader.stream_position()?;
        let tag_struct_header = read_chunk_header(reader, endian)?;
        let tag_struct_offset = reader.stream_position()?;
        if tag_struct_header.signature != u32::from_be_bytes(*b"tgst") {
            return Err(TagReadError::BadChunkSignature {
                offset: tag_struct_header_offset,
                expected: *b"tgst",
                got: tag_struct_header.signature.to_be_bytes(),
            });
        }
        // tgst.version always equals tgst.size. Treat divergence as
        // size-mismatch since the version field is being used as a
        // duplicate size carrier here.
        if tag_struct_header.version != tag_struct_header.size {
            return Err(TagReadError::ChunkSizeMismatch {
                chunk: "tgst",
                started_at: tag_struct_offset,
                ended_at: tag_struct_offset + tag_struct_header.version as u64,
                expected_end: tag_struct_offset + tag_struct_header.size as u64,
            });
        }

        // tgst with size=0 is a null struct: no sub-chunks follow.
        let sub_chunks = if tag_struct_header.size != 0 {
            let mut sub_chunks = read_sub_chunks(layout, definition, reader, endian)?;

            // Trailing empty-tgst absorb: MCC's writer occasionally
            // emits size=0 tgst chunks at the end of a struct's
            // content that don't correspond to any layout field.
            // Preserve them as EmptyPlaceholder entries so write-side
            // re-emits them at the same position.
            let mut end_offset = reader.stream_position()?;
            let expected_offset = tag_struct_offset + tag_struct_header.size as u64;

            if end_offset != expected_offset {
                let mut non_empty_trailing_chunks = false;

                loop {
                    end_offset = reader.stream_position()?;

                    if end_offset == expected_offset {
                        break;
                    }

                    let trailer = read_chunk_header(reader, endian)?;

                    if trailer.signature != u32::from_be_bytes(*b"tgst") || trailer.size != 0 {
                        non_empty_trailing_chunks = true;
                        break;
                    }

                    if trailer.version != 0 {
                        return Err(TagReadError::BadChunkVersion {
                            chunk: "trailing empty tgst",
                            version: trailer.version,
                        });
                    }
                    sub_chunks.push(TagSubChunkEntry {
                        field_index: None,
                        content: TagSubChunkContent::EmptyPlaceholder,
                    });
                }

                if non_empty_trailing_chunks {
                    return Err(TagReadError::ChunkSizeMismatch {
                        chunk: "tgst",
                        started_at: tag_struct_offset,
                        ended_at: end_offset,
                        expected_end: expected_offset,
                    });
                }
            }

            sub_chunks
        } else {
            // Empty tgst (size=0): no sub-chunk bytes on disk, but the
            // struct may still hold fixed-size containers (Array,
            // Struct) that need scaffolding to be navigable via the
            // API. Same situation as a simple-block element.
            TagStructData::new_default(layout, definition.index as usize, endian).sub_chunks
        };

        Ok(Self {
            struct_index: definition.index,
            sub_chunks,
            classic_struct_header: None,
        })
    }

    /// Write this struct as a `tgst` chunk. Emits only the sub_chunks
    /// content; the struct's raw bytes flow out through the enclosing
    /// block's `raw_data` concatenation.
    pub(crate) fn write<W: Write>(
        &self,
        layout: &TagLayout,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let mut content = Vec::new();
        write_sub_chunks(&self.sub_chunks, layout, &mut content)?;
        let size = content.len() as u32;
        write_tag_chunk_header(writer, u32::from_be_bytes(*b"tgst"), size, size)?;
        writer.write_all(&content)?;
        Ok(())
    }

    /// Parse a single field's value.
    ///
    /// `struct_raw` is the slice of the enclosing block's `raw_data`
    /// that covers exactly this struct's bytes — typically obtained
    /// via [`crate::path::lookup`] or a caller-computed offset. For
    /// sub-chunk leaf fields (string_id / tag_reference / data),
    /// walks `self.sub_chunks` to find the matching payload.
    pub(crate) fn parse_field(
        &self,
        layout: &TagLayout,
        struct_raw: &[u8],
        field_index: usize,
        endian: Endian,
    ) -> Option<TagFieldData> {
        let field = &layout.fields[field_index];
        let sub_chunk = self
            .sub_chunks
            .iter()
            .find(|entry| entry.field_index == Some(field_index as u32))
            .map(|entry| &entry.content);
        deserialize_field(layout, field, struct_raw, sub_chunk, endian)
    }

    /// Write `value` back to this struct.
    ///
    /// Primitive, enum/flag, and math values mutate `struct_raw` at
    /// the field's offset. Sub-chunk leaf values swap the matching
    /// `TagSubChunkEntry.content`; that entry is expected to exist
    /// already (set on read or via `new_default`).
    pub(crate) fn set_field(
        &mut self,
        layout: &TagLayout,
        struct_raw: &mut [u8],
        field_index: usize,
        value: TagFieldData,
        endian: Endian,
    ) {
        let field = &layout.fields[field_index];
        if let Some(new_content) = serialize_field(field, &value, struct_raw, endian) {
            let entry = self
                .sub_chunks
                .iter_mut()
                .find(|entry| entry.field_index == Some(field_index as u32))
                .expect("set_field: sub-chunk entry missing for sub-chunk-bearing field");
            entry.content = new_content;
        }
    }

    /// Build a struct tree with default sub_chunks for every
    /// sub-chunk-bearing field. Used by [`TagBlockData::add_element`]
    /// and friends to initialize a new element's struct tree. Does
    /// not allocate any raw bytes — the caller (the block) provides
    /// them by growing its own `raw_data`.
    pub(crate) fn new_default(layout: &TagLayout, struct_index: usize, endian: Endian) -> Self {
        let struct_layout = &layout.struct_layouts[struct_index];
        let mut sub_chunks = Vec::new();
        let mut field_index = struct_layout.first_field_index as usize;

        loop {
            let field = &layout.fields[field_index];
            if field.field_type == TagFieldType::Terminator {
                break;
            }

            let content: Option<TagSubChunkContent> = match field.field_type {
                TagFieldType::Struct => Some(TagSubChunkContent::Struct(
                    // Instantiate the CANONICAL (latest) variant a versioned
                    // struct field points at — new tags use the newest schema.
                    // A corpus sweep confirms this matches real H2 usage (e.g.
                    // 97.5% of `mapping_function` occurrences are the canonical
                    // v1, not the legacy `__v0`).
                    TagStructData::new_default(layout, field.definition as usize, endian),
                )),
                TagFieldType::Block => {
                    let block_layout = &layout.block_layouts[field.definition as usize];
                    Some(TagSubChunkContent::Block(TagBlockData {
                        block_index: block_layout.index,
                        flags: 0,
                        raw_data: Vec::new(),
                        endian,
                        elements: Vec::new(),
                        classic_block_header: None,
                        classic_structural_dirty: false,
                        classic_trailing: None,
                    }))
                }
                TagFieldType::Array => {
                    let array_layout = &layout.array_layouts[field.definition as usize];
                    let mut elements = Vec::with_capacity(array_layout.count as usize);
                    for _ in 0..array_layout.count {
                        elements.push(TagStructData::new_default(
                            layout,
                            array_layout.struct_index as usize,
                            endian,
                        ));
                    }
                    Some(TagSubChunkContent::Array(elements))
                }
                TagFieldType::TagReference => Some(TagSubChunkContent::TagReference(Vec::new())),
                TagFieldType::StringId => Some(TagSubChunkContent::StringId(Vec::new())),
                TagFieldType::OldStringId => Some(TagSubChunkContent::OldStringId(Vec::new())),
                TagFieldType::Data => Some(TagSubChunkContent::Data(Vec::new())),
                TagFieldType::ApiInterop => {
                    // 12 zero bytes matches BCS's reset pattern except
                    // for `address` (which BCS sets to `UINT_MAX`). A
                    // freshly-defaulted interop won't reach a runtime
                    // that cares, so plain zeroes are safe.
                    Some(TagSubChunkContent::ApiInterop(vec![0u8; 12]))
                }
                TagFieldType::PageableResource => {
                    Some(TagSubChunkContent::Resource(TagResourceChunk::Null))
                }
                _ => None,
            };

            if let Some(content) = content {
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content,
                });
            }

            field_index += 1;
        }

        Self {
            struct_index: struct_layout.index,
            sub_chunks,
            classic_struct_header: None,
        }
    }

    /// Find the index (into `layout.fields`) of a field in this
    /// struct by name. Case-sensitive. Walks fields starting at
    /// `first_field_index` up to the terminator and returns the
    /// first match. Returns `None` if no such field exists.
    pub(crate) fn find_field_by_name(&self, layout: &TagLayout, name: &str) -> Option<usize> {
        let struct_layout = &layout.struct_layouts[self.struct_index as usize];
        let mut field_index = struct_layout.first_field_index as usize;
        loop {
            let field = &layout.fields[field_index];
            if field.field_type == TagFieldType::Terminator {
                return None;
            }
            if field_name_matches(layout.get_string(field.name_offset), name) {
                return Some(field_index);
            }
            field_index += 1;
        }
    }

    /// Iterate the user-addressable field names of this struct:
    /// everything except terminator / pad / useless_pad / skip /
    /// explanation / unknown. Empty names are skipped too.
    pub(crate) fn field_names<'a>(
        &'a self,
        layout: &'a TagLayout,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let struct_layout = &layout.struct_layouts[self.struct_index as usize];
        let start = struct_layout.first_field_index as usize;
        layout.fields[start..]
            .iter()
            .take_while(|f| f.field_type != TagFieldType::Terminator)
            .filter(|f| {
                !matches!(
                    f.field_type,
                    TagFieldType::Pad
                        | TagFieldType::UselessPad
                        | TagFieldType::Skip
                        | TagFieldType::Explanation
                        | TagFieldType::Unknown,
                )
            })
            .filter_map(|f| layout.get_string(f.name_offset))
            .filter(|name| !name.is_empty())
    }

    /// Step into a nested struct field. Returns `(nested_struct,
    /// nested_raw)` where `nested_raw` is the slice of `element_raw`
    /// covering the nested struct's bytes. Returns `None` if
    /// `field_index` isn't a Struct field or the sub-chunk is
    /// missing.
    pub(crate) fn nested_struct<'a>(
        &'a self,
        layout: &TagLayout,
        element_raw: &'a [u8],
        field_index: usize,
    ) -> Option<(&'a TagStructData, &'a [u8])> {
        let field = &layout.fields[field_index];
        if field.field_type != TagFieldType::Struct {
            return None;
        }
        let entry = self
            .sub_chunks
            .iter()
            .find(|e| e.field_index == Some(field_index as u32))?;
        let nested = match &entry.content {
            TagSubChunkContent::Struct(s) => s,
            _ => return None,
        };
        let nested_size = layout.struct_layouts[nested.struct_index as usize].size;
        let offset = field.offset as usize;
        // A truncated (clamped) parent element may not carry this inline
        // struct's full bytes (or any of them). Clamp the sub-slice to
        // what's present — fully-absent → None, partially-present → the
        // available prefix, where each missing inner field then decodes
        // as absent via the guard in `deserialize_field`. Mirrors the H2
        // engine's zero-fill of bytes past `stored_size`.
        if offset >= element_raw.len() {
            return None;
        }
        let end = (offset + nested_size).min(element_raw.len());
        Some((nested, &element_raw[offset..end]))
    }

    /// Mutable counterpart to [`Self::nested_struct`].
    pub(crate) fn nested_struct_mut<'a>(
        &'a mut self,
        layout: &TagLayout,
        element_raw: &'a mut [u8],
        field_index: usize,
    ) -> Option<(&'a mut TagStructData, &'a mut [u8])> {
        let field = &layout.fields[field_index];
        if field.field_type != TagFieldType::Struct {
            return None;
        }
        // Pre-compute sizing before borrowing sub_chunks mutably.
        let offset = field.offset as usize;

        let entry = self
            .sub_chunks
            .iter_mut()
            .find(|e| e.field_index == Some(field_index as u32))?;
        let nested = match &mut entry.content {
            TagSubChunkContent::Struct(s) => s,
            _ => return None,
        };
        let nested_size = layout.struct_layouts[nested.struct_index as usize].size;
        // Same truncated-element clamp as the read-path `nested_struct`.
        if offset >= element_raw.len() {
            return None;
        }
        let end = (offset + nested_size).min(element_raw.len());
        Some((nested, &mut element_raw[offset..end]))
    }
}

/// Walk a struct definition's fields, reading each sub-chunk-producing
/// field's chunk from the stream. Primitive / pad / skip / custom /
/// explanation / terminator fields contribute nothing here — their
/// values live in `raw_data` at the precomputed `field.offset`.
fn read_sub_chunks<R: Seek + Read>(
    layout: &TagLayout,
    definition: &TagStructLayout,
    reader: &mut std::io::BufReader<R>,
    endian: Endian,
) -> Result<Vec<TagSubChunkEntry>, TagReadError> {
    let mut sub_chunks = Vec::new();
    let mut field_index = definition.first_field_index as usize;

    loop {
        let field = &layout.fields[field_index];

        match field.field_type {
            TagFieldType::Terminator => break,

            TagFieldType::Struct => {
                let nested_definition = &layout.struct_layouts[field.definition as usize];

                // Placeholder-skip: MCC may emit size=0 tgst placeholder(s) before
                // the real tgst when the nested struct expects sub-chunks.
                let expected_children = layout.get_struct_expected_children(field.definition as usize);

                if expected_children > 0 {
                    loop {
                        let header_offset = reader.stream_position()?;
                        let header = read_chunk_header(reader, endian)?;

                        if header.signature != u32::from_be_bytes(*b"tgst") {
                            return Err(TagReadError::BadChunkSignature {
                                offset: header_offset,
                                expected: *b"tgst",
                                got: header.signature.to_be_bytes(),
                            });
                        }

                        if header.size == 0 {
                            if header.version != 0 {
                                return Err(TagReadError::BadChunkVersion {
                                    chunk: "empty placeholder tgst",
                                    version: header.version,
                                });
                            }
                            sub_chunks.push(TagSubChunkEntry {
                                field_index: None,
                                content: TagSubChunkContent::EmptyPlaceholder,
                            });
                            continue;
                        }

                        reader.seek(SeekFrom::Start(header_offset))?;
                        break;
                    }
                }

                let nested = TagStructData::read(layout, nested_definition, reader, endian)?;

                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::Struct(nested),
                });
            }

            TagFieldType::Array => {
                let array_layout = &layout.array_layouts[field.definition as usize];
                let element_definition = &layout.struct_layouts[array_layout.struct_index as usize];

                let mut elements = Vec::with_capacity(array_layout.count as usize);

                for _ in 0..array_layout.count as usize {
                    let element_sub_chunks = read_sub_chunks(layout, element_definition, reader, endian)?;

                    elements.push(TagStructData {
                        struct_index: element_definition.index,
                        sub_chunks: element_sub_chunks,
                        classic_struct_header: None,
                    });
                }

                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::Array(elements),
                });
            }

            TagFieldType::Block => {
                let block_layout = &layout.block_layouts[field.definition as usize];
                let block_data = TagBlockData::read(layout, block_layout, reader, endian)?;

                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::Block(block_data),
                });
            }

            TagFieldType::TagReference => {
                let (version, content) = read_tag_chunk_content(reader, u32::from_be_bytes(*b"tgrf"), endian)?;
                if version != 0 {
                    return Err(TagReadError::BadChunkVersion { chunk: "tgrf", version });
                }
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::TagReference(content),
                });
            }

            TagFieldType::StringId => {
                let (version, content) = read_tag_chunk_content(reader, u32::from_be_bytes(*b"tgsi"), endian)?;
                if version != 0 {
                    return Err(TagReadError::BadChunkVersion {
                        chunk: "tgsi (string_id)",
                        version,
                    });
                }
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::StringId(content),
                });
            }

            TagFieldType::OldStringId => {
                let (version, content) = read_tag_chunk_content(reader, u32::from_be_bytes(*b"tgsi"), endian)?;
                if version != 0 {
                    return Err(TagReadError::BadChunkVersion {
                        chunk: "tgsi (old_string_id)",
                        version,
                    });
                }
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::OldStringId(content),
                });
            }

            TagFieldType::Data => {
                let (version, content) = read_tag_chunk_content(reader, u32::from_be_bytes(*b"tgda"), endian)?;
                if version != 0 {
                    return Err(TagReadError::BadChunkVersion { chunk: "tgda", version });
                }
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::Data(content),
                });
            }

            TagFieldType::PageableResource => {
                let resource_layout = &layout.resource_layouts[field.definition as usize];
                let resource_struct_definition = &layout.struct_layouts[resource_layout.struct_index as usize];

                let outer_header = read_chunk_header(reader, endian)?;
                let outer_content_offset = reader.stream_position()?;

                let resource = match &outer_header.signature.to_be_bytes() {
                    b"tg\0c" => {
                        if outer_header.version != 0 {
                            return Err(TagReadError::BadChunkVersion {
                                chunk: "tg\\0c",
                                version: outer_header.version,
                            });
                        }
                        TagResourceChunk::Null
                    }

                    b"tgrc" => {
                        if outer_header.version != 0 {
                            return Err(TagReadError::BadChunkVersion {
                                chunk: "tgrc",
                                version: outer_header.version,
                            });
                        }

                        let tgdt_header = read_validated_chunk_header(reader, *b"tgdt", "tgdt", endian)?;

                        let mut exploded = vec![0u8; tgdt_header.size as usize];
                        reader.read_exact(&mut exploded)?;

                        let struct_data = TagStructData::read(
                            layout,
                            resource_struct_definition,
                            reader,
                            endian,
                        )?;

                        TagResourceChunk::Exploded {
                            exploded,
                            struct_data,
                            xsync_state: None,
                        }
                    }

                    b"tgxc" => {
                        // MCC writes tgxc v0; the Halo 4 X360 dev
                        // build writes v3 with a different internal
                        // payload format. We preserve both the
                        // bytes and the version so the monolithic
                        // hydration pass can pick the right xsync
                        // state shape downstream.
                        let mut payload = vec![0u8; outer_header.size as usize];
                        reader.read_exact(&mut payload)?;
                        TagResourceChunk::Xsync { version: outer_header.version, payload }
                    }

                    signature => {
                        return Err(TagReadError::UnknownSubChunkSignature {
                            context: "pageable resource",
                            signature: *signature,
                        });
                    }
                };

                let end_offset = reader.stream_position()?;
                let expected_offset = outer_content_offset + outer_header.size as u64;

                if end_offset != expected_offset {
                    return Err(TagReadError::ChunkSizeMismatch {
                        chunk: "pageable resource",
                        started_at: outer_content_offset,
                        ended_at: end_offset,
                        expected_end: expected_offset,
                    });
                }

                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::Resource(resource),
                });
            }

            TagFieldType::ApiInterop => {
                let (version, content) = read_tag_chunk_content(reader, u32::from_be_bytes(*b"ti]["), endian)?;
                if version != 0 {
                    return Err(TagReadError::BadChunkVersion {
                        chunk: "ti][ (api_interop)",
                        version,
                    });
                }
                sub_chunks.push(TagSubChunkEntry {
                    field_index: Some(field_index as u32),
                    content: TagSubChunkContent::ApiInterop(content),
                });
            }

            // Primitives / pad / skip / custom / explanation / useless_pad.
            _ => {
                let field_type = &layout.field_types[field.type_index as usize];

                if field_type.needs_sub_chunk != 0 {
                    let name = layout
                        .get_string(field_type.name_offset)
                        .unwrap_or("<bad name>")
                        .to_owned();
                    return Err(TagReadError::UnsupportedFieldType { type_name: name });
                }
            }
        }

        field_index += 1;
    }

    Ok(sub_chunks)
}

/// Serialize a vec of sub-chunk entries in stored order. Mirrors
/// `read_sub_chunks`.
fn write_sub_chunks<W: Write>(
    entries: &[TagSubChunkEntry],
    layout: &TagLayout,
    writer: &mut W,
) -> std::io::Result<()> {
    for entry in entries {
        match &entry.content {
            TagSubChunkContent::EmptyPlaceholder => {
                write_tag_chunk_header(writer, u32::from_be_bytes(*b"tgst"), 0, 0)?;
            }

            TagSubChunkContent::Struct(nested_struct_data) => {
                nested_struct_data.write(layout, writer)?;
            }

            TagSubChunkContent::Block(nested_block_data) => {
                nested_block_data.write(layout, writer)?;
            }

            TagSubChunkContent::Array(elements) => {
                // Array elements have no wrapping tgst; their sub-chunks
                // flow inline into the parent's tgst content.
                for element in elements {
                    write_sub_chunks(&element.sub_chunks, layout, writer)?;
                }
            }

            TagSubChunkContent::TagReference(content) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgrf"), 0, content)?;
            }

            TagSubChunkContent::StringId(content) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgsi"), 0, content)?;
            }

            TagSubChunkContent::OldStringId(content) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgsi"), 0, content)?;
            }

            TagSubChunkContent::Data(content) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgda"), 0, content)?;
            }

            TagSubChunkContent::ApiInterop(content) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"ti]["), 0, content)?;
            }

            TagSubChunkContent::Resource(TagResourceChunk::Null) => {
                write_tag_chunk_header(writer, u32::from_be_bytes(*b"tg\0c"), 0, 0)?;
            }

            TagSubChunkContent::Resource(TagResourceChunk::Exploded { exploded, struct_data, .. }) => {
                let mut inner = Vec::new();
                write_tag_chunk_content(&mut inner, u32::from_be_bytes(*b"tgdt"), 0, exploded)?;
                struct_data.write(layout, &mut inner)?;
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgrc"), 0, &inner)?;
            }

            TagSubChunkContent::Resource(TagResourceChunk::Xsync { version, payload }) => {
                write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgxc"), *version, payload)?;
            }
        }
    }
    Ok(())
}

/// A `tgbl` chunk: a variable-count array of struct elements.
///
/// `raw_data` is a single concatenated byte buffer of length
/// `elements.len() * element_size`; element `i`'s bytes live at
/// `raw_data[i * element_size..(i + 1) * element_size]`. Nested
/// struct/array fields within an element are offset regions inside
/// this same buffer; nested block fields start fresh buffers in their
/// own `TagBlockData`.
///
/// Two shapes, selected by `flags` bit 0:
/// - **Complex** (bit 0 clear): each element has a `tgst` sub-chunk
///   for its sub-chunk-bearing fields.
/// - **Simple** (bit 0 set, `is_simple_data_type=1` in BCS): element
///   bytes only, no per-element `tgst` and no sub-chunks.
#[derive(Debug, Clone)]
pub(crate) struct TagBlockData {
    /// Index into [`TagLayout::block_layouts`].
    pub(crate) block_index: u32,
    /// Block flags. Bit 0 toggles simple vs complex shape; other bits
    /// are preserved verbatim for roundtrip.
    pub(crate) flags: u32,
    /// Concatenated element bytes. Resized atomically by the block
    /// operations (`add_element`, `insert_at`, `duplicate_at`,
    /// `delete_at`, `clear`). Held in **source-wire order** — for an
    /// Xbox 360 / BE-loaded tag the integers/floats within these
    /// bytes are big-endian. Field readers in [`crate::fields`]
    /// dispatch on [`Self::endian`] when slicing.
    pub(crate) raw_data: Vec<u8>,
    /// Source byte order of [`Self::raw_data`]. Propagated from the
    /// file's wire endian during [`Self::read`]; new blocks default
    /// to [`Endian::Le`] since we only emit LE on write.
    pub(crate) endian: Endian,
    /// Per-element struct trees. Each element's raw bytes live in
    /// `raw_data` at index `i * element_size`. Simple-block elements
    /// have empty `sub_chunks`.
    pub(crate) elements: Vec<TagStructData>,
    /// Classic **Halo 2** only: the 16-byte block header (`4cc +
    /// version + count + size`) that precedes this block's elements on
    /// disk. `None` for MCC and Halo CE (whose blocks are headerless).
    /// Preserved verbatim for byte-exact write; the count is re-synced
    /// from `elements.len()` on encode. The root block carries one too.
    pub(crate) classic_block_header: Option<Vec<u8>>,
    /// Classic **Halo 2** only: this block's structure was changed after
    /// load (add/insert/delete/clear/paste/reorder). H2 normally treats
    /// inline block-count words as runtime garbage, so unmodified blocks
    /// preserve them byte-exactly. Once edited, the inline presence/count
    /// must match the emitted trailing block data or the next read will
    /// skip/over-read bytes and misalign the tag stream.
    pub(crate) classic_structural_dirty: bool,
    /// Classic **Halo 2** only, **root block only**: raw bytes that follow
    /// the entire structured body on disk (appended sample/cache data,
    /// e.g. multi-MB ambience-sound audio) which no layout field
    /// references. Both we and HABT read only the fixed structure; this
    /// blob is preserved verbatim so read→write stays byte-exact. `None`
    /// everywhere else.
    pub(crate) classic_trailing: Option<Vec<u8>>,
}

impl TagBlockData {
    /// Parse a `tgbl` chunk. Complex vs simple shape is decided by
    /// `flags` bit 0.
    pub(crate) fn read<R: Seek + Read>(
        layout: &TagLayout,
        definition: &TagBlockLayout,
        reader: &mut std::io::BufReader<R>,
        endian: Endian,
    ) -> Result<Self, TagReadError> {
        let tag_block_header = read_validated_chunk_header(reader, *b"tgbl", "tgbl", endian)?;
        let tag_block_offset = reader.stream_position()?;

        let block_element_count = read_u32(reader, endian)?;
        let block_flags = read_u32(reader, endian)?;

        let struct_layout = &layout.struct_layouts[definition.struct_index as usize];
        let element_size = struct_layout.size;

        let mut raw_data = vec![0u8; element_size * block_element_count as usize];
        reader.read_exact(&mut raw_data)?;

        let mut elements = Vec::with_capacity(block_element_count as usize);

        if (block_flags & 1) == 0 {
            // Complex block: per-element tgst sub-chunks.
            for _ in 0..block_element_count {
                elements.push(TagStructData::read(layout, struct_layout, reader, endian)?);
            }
        } else {
            // Simple block: raw bytes only, no per-element tgst on disk.
            // Even so, container fields (Array, Struct, ...) still need
            // in-memory sub_chunk entries for the API to navigate them
            // — those entries don't consume any disk bytes since their
            // payload is fully fixed-size and lives inline in raw_data.
            // Going through new_default builds that scaffolding.
            for _ in 0..block_element_count {
                elements.push(TagStructData::new_default(
                    layout,
                    struct_layout.index as usize,
                    endian,
                ));
            }
        }

        check_chunk_end(reader, "tgbl", tag_block_offset, tag_block_header.size)?;

        Ok(Self {
            block_index: definition.index,
            flags: block_flags,
            raw_data,
            endian,
            elements,
            classic_block_header: None,
            classic_structural_dirty: false,
            classic_trailing: None,
        })
    }

    /// Write this block as a `tgbl` chunk.
    pub(crate) fn write<W: Write>(
        &self,
        layout: &TagLayout,
        writer: &mut W,
    ) -> std::io::Result<()> {
        let mut body = Vec::new();
        let element_count = self.elements.len() as u32;
        body.extend_from_slice(&element_count.to_le_bytes());
        body.extend_from_slice(&self.flags.to_le_bytes());
        body.extend_from_slice(&self.raw_data);

        if (self.flags & 1) == 0 {
            for element in &self.elements {
                element.write(layout, &mut body)?;
            }
        }

        write_tag_chunk_content(writer, u32::from_be_bytes(*b"tgbl"), 0, &body)?;
        Ok(())
    }

    /// Size of one element's byte region.
    fn element_size(&self, layout: &TagLayout) -> usize {
        // For a populated block the on-disk element size is authoritative
        // as `raw_data / count` — this is what the (classic) encoder uses
        // and is essential for VERSIONED classic blocks, whose elements
        // are a FieldSet variant that may be smaller/larger than the
        // block's base/latest struct (e.g. H2 bitmap_data v1 = 116 bytes
        // vs the latest 140). Empty blocks (and fresh MCC allocations)
        // fall back to the base struct size; for non-versioned blocks both
        // agree, so this is a no-op there.
        if !self.elements.is_empty() && !self.raw_data.is_empty() {
            return self.raw_data.len() / self.elements.len();
        }
        let struct_index = layout.block_layouts[self.block_index as usize].struct_index as usize;
        layout.struct_layouts[struct_index].size
    }

    /// The struct index a newly added element should use: the variant the
    /// existing elements already use (so versioned classic blocks stay on
    /// their on-disk FieldSet), falling back to the block's base struct for
    /// an empty block.
    fn element_struct_index(&self, layout: &TagLayout) -> usize {
        self.elements
            .first()
            .map(|element| element.struct_index as usize)
            .unwrap_or_else(|| layout.block_layouts[self.block_index as usize].struct_index as usize)
    }

    /// Synthesize the 16-byte H2 classic block header (`dfbt` + version 0
    /// + count 0 + element_size) for a block that is about to gain its
    /// first element. An empty H2 block carries no header on disk; once it
    /// becomes non-empty the encoder needs a header to emit the
    /// authoritative count/size, so create one if the layout looks like H2
    /// classic and the block has none yet. No-op for MCC/CE.
    pub(crate) fn ensure_h2_classic_header_for_nonempty(
        &mut self,
        layout: &TagLayout,
        element_size: usize,
    ) {
        if self.classic_block_header.is_some()
            || !self.elements.is_empty()
            || !looks_like_h2_classic_layout(layout)
        {
            return;
        }

        let mut header = Vec::with_capacity(16);
        header.extend_from_slice(b"dfbt");
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&(element_size as u32).to_le_bytes());
        self.classic_block_header = Some(header);
    }

    /// Mark this block as structurally edited (element count/order changed)
    /// so the H2 classic encoder rewrites the inline count word and block
    /// header to match the emitted body instead of preserving the original
    /// (possibly runtime-garbage) bytes verbatim.
    pub(crate) fn mark_classic_structural_dirty(&mut self) {
        self.classic_structural_dirty = true;
    }

    /// Append a fresh zero-initialized element. Grows `raw_data` by
    /// one element_size and pushes a default `TagStructData`. Returns a
    /// mutable reference to the new element.
    pub(crate) fn add_element(&mut self, layout: &TagLayout) -> &mut TagStructData {
        let struct_index = self.element_struct_index(layout);
        let element_size = self.element_size(layout);
        self.ensure_h2_classic_header_for_nonempty(layout, element_size);
        self.mark_classic_structural_dirty();
        let old_len = self.raw_data.len();
        self.raw_data.resize(old_len + element_size, 0);
        self.elements.push(TagStructData::new_default(layout, struct_index, self.endian));
        self.elements.last_mut().unwrap()
    }

    /// Insert a fresh zero-initialized element at `index` (shifting
    /// later elements right).
    pub(crate) fn insert_element(&mut self, layout: &TagLayout, index: usize) -> &mut TagStructData {
        let struct_index = self.element_struct_index(layout);
        let element_size = self.element_size(layout);
        self.ensure_h2_classic_header_for_nonempty(layout, element_size);
        self.mark_classic_structural_dirty();
        let insert_offset = index * element_size;
        self.raw_data.splice(
            insert_offset..insert_offset,
            std::iter::repeat_n(0, element_size),
        );
        self.elements.insert(index, TagStructData::new_default(layout, struct_index, self.endian));
        &mut self.elements[index]
    }

    /// Deep-copy the element at `index` and insert the copy directly
    /// after it. Returns a mutable reference to the new element.
    pub(crate) fn duplicate_element(&mut self, layout: &TagLayout, index: usize) -> &mut TagStructData {
        let element_size = self.element_size(layout);
        self.mark_classic_structural_dirty();
        let src_offset = index * element_size;
        let copy_bytes: Vec<u8> = self.raw_data[src_offset..src_offset + element_size].to_vec();
        let insert_offset = (index + 1) * element_size;
        self.raw_data.splice(insert_offset..insert_offset, copy_bytes);
        let cloned = self.elements[index].clone();
        self.elements.insert(index + 1, cloned);
        &mut self.elements[index + 1]
    }

    /// Remove the element at `index`. Panics if out of range.
    pub(crate) fn remove_element(&mut self, layout: &TagLayout, index: usize) {
        let element_size = self.element_size(layout);
        self.mark_classic_structural_dirty();
        let start = index * element_size;
        self.raw_data.drain(start..start + element_size);
        self.elements.remove(index);
    }

    /// Swap elements at `i` and `j`. Panics if either is out of range.
    pub(crate) fn swap_elements(&mut self, layout: &TagLayout, i: usize, j: usize) {
        if i == j {
            return;
        }
        self.mark_classic_structural_dirty();
        let size = self.element_size(layout);
        self.elements.swap(i, j);

        // Swap the two raw-data regions via a temporary buffer.
        let (lo, hi) = if i < j { (i, j) } else { (j, i) };
        let lo_start = lo * size;
        let hi_start = hi * size;
        let mut buf = vec![0u8; size];
        buf.copy_from_slice(&self.raw_data[lo_start..lo_start + size]);
        self.raw_data.copy_within(hi_start..hi_start + size, lo_start);
        self.raw_data[hi_start..hi_start + size].copy_from_slice(&buf);
    }

    /// Move the element at `from` to `to` (Vec::remove + Vec::insert
    /// semantics — `to` is the target index in the final ordering).
    /// Panics if either is out of range.
    pub(crate) fn move_element(&mut self, layout: &TagLayout, from: usize, to: usize) {
        if from == to {
            return;
        }
        self.mark_classic_structural_dirty();
        let size = self.element_size(layout);
        let src = from * size;
        let bytes: Vec<u8> = self.raw_data.drain(src..src + size).collect();
        let dst = to * size;
        self.raw_data.splice(dst..dst, bytes);

        let elem = self.elements.remove(from);
        self.elements.insert(to, elem);
    }

    /// Remove all elements.
    pub(crate) fn clear(&mut self) {
        self.mark_classic_structural_dirty();
        self.raw_data.clear();
        self.elements.clear();
    }

    /// Slice of `raw_data` covering element `i`'s bytes.
    pub(crate) fn element_raw(&self, layout: &TagLayout, i: usize) -> &[u8] {
        let size = self.element_size(layout);
        let start = i * size;
        &self.raw_data[start..start + size]
    }

    /// Iterate `(raw_slice, struct_ref)` pairs for every element in
    /// order. Each raw slice is the element's region within
    /// `self.raw_data`. Cheap — no allocation, just offset walking.
    pub(crate) fn iter_elements<'a>(
        &'a self,
        layout: &'a TagLayout,
    ) -> impl Iterator<Item = (&'a [u8], &'a TagStructData)> + 'a {
        let element_size = self.element_size(layout);
        self.elements.iter().enumerate().map(move |(i, element)| {
            let start = i * element_size;
            (&self.raw_data[start..start + element_size], element)
        })
    }

    /// Block with exactly one zero-filled default element. Used as
    /// the root block of a freshly created tag file so it has a
    /// single, loadable element out of the box. Nested sub-chunks
    /// (including any child blocks, which stay empty) are populated
    /// by [`TagStructData::new_default`].
    pub(crate) fn new_root_default(layout: &TagLayout, block_index: u32, endian: Endian) -> Self {
        let block_layout = &layout.block_layouts[block_index as usize];
        let struct_layout = &layout.struct_layouts[block_layout.struct_index as usize];
        let element_size = struct_layout.size;

        Self {
            block_index,
            flags: 0,
            raw_data: vec![0u8; element_size],
            endian,
            elements: vec![TagStructData::new_default(
                layout,
                block_layout.struct_index as usize,
                endian,
            )],
            classic_block_header: None,
            classic_structural_dirty: false,
            classic_trailing: None,
        }
    }
}
