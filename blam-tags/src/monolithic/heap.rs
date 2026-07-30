//! Partition heap, data arrays, and the `blok` tag-block table.
//!
//! All of these structures appear inside `blob_index.dat`'s `mtag`
//! container. The shapes here mirror the on-disk layout exactly;
//! the runtime walkers in [`MonolithicCache`](super::MonolithicCache)
//! consume them to resolve a tag's `wide_block_index` into a
//! `(partition_file, offset, size)` triple.
//!
//! **Cache-format ground truth.** Field orderings here match TagTool
//! (verified against the Aug 22 2011 Halo 4 build's `blok` chunk —
//! datum-1 has 65528 blocks, datum-2 has 49423, sum matches the
//! `!@#$` per-block footer count). BCS's same-named structs in
//! `tag_file_blocks_chunk.h` document a partially-swapped field
//! order; we do **not** use it.
//!
//! **Page size is hard-coded to 512 bytes** (`PAGE_BITS = 9`) because
//! every observed partition stores that value in its
//! [`LruvPersistHeader`]. If a future build varies it, read the value
//! from the header instead.

use std::io::{Read, Seek};

use crate::error::TagReadError;
use crate::io::{read_u32, read_u8_array, Endian};

use super::chunk::MonolithicChunk;

/// Page-size bit shift used by every observed Halo 4 monolithic
/// cache. Multiply `(first_page_index, page_count)` by
/// `1 << PAGE_BITS` to get byte offsets / sizes.
pub const PAGE_BITS: u32 = 9;

/// `'!@#$'` — per-element footer signature on every `DataArray<T>`
/// element. BE-packed.
const FOOTER_BANG: u32 = 0x21402324;
/// `'d@ft'` — array trailer signature. BE-packed.
const FOOTER_DFT: u32 = 0x64406674;

/// A 32-bit datum handle (`salt: u16 high`, `index: u16 low`).
/// Looking up a slot in a `DataArray` reads `index` and validates
/// the slot's stored salt matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatumHandle(pub u32);

impl DatumHandle {
    /// Sentinel value (`0xFFFFFFFF`) — "none / invalid".
    pub const NONE: Self = Self(0xFFFF_FFFF);

    #[inline]
    pub fn salt(self) -> u16 {
        (self.0 >> 16) as u16
    }
    #[inline]
    pub fn index(self) -> u16 {
        self.0 as u16
    }
    #[inline]
    pub fn is_none(self) -> bool {
        self == Self::NONE
    }
}

/// 64-bit handle: `(partition_handle, datum_handle)` packed
/// big-endian. The high 32 bits identify a partition slot in a
/// `WideDataArray`; the low 32 bits identify a datum within that
/// partition's nested `DataArray`.
#[derive(Debug, Clone, Copy)]
pub struct WideDatumHandle {
    pub partition: DatumHandle,
    pub datum: DatumHandle,
}

impl WideDatumHandle {
    /// Unpack the `u64` stored as `wide_block_index` on a
    /// [`super::TagFileEntry`].
    pub fn from_u64(value: u64) -> Self {
        Self {
            partition: DatumHandle((value >> 32) as u32),
            datum: DatumHandle(value as u32),
        }
    }
}

/// 52-byte header that prefixes every `DataArray<T>`.
///
/// `actual_count` is the iteration count — read that many elements
/// from the stream — while `maximum_count` is the slot capacity
/// (the `DatumHandle.index` of any reachable element is `< max`).
#[derive(Debug, Clone)]
pub struct DataArrayHeader {
    pub name: [u8; 32],
    /// Per-element data size (sans the 4-byte index prefix and
    /// 4-byte footer).
    pub size: u32,
    pub maximum_count: u32,
    pub actual_count: u32,
    pub next_identifier: u32,
    pub signature: u32,
}

impl DataArrayHeader {
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            name: read_u8_array(reader)?,
            size: read_u32(reader, Endian::Be)?,
            maximum_count: read_u32(reader, Endian::Be)?,
            actual_count: read_u32(reader, Endian::Be)?,
            next_identifier: read_u32(reader, Endian::Be)?,
            signature: read_u32(reader, Endian::Be)?,
        })
    }

    /// Display name with trailing NULs / spaces stripped.
    pub fn display_name(&self) -> String {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(self.name.len());
        String::from_utf8_lossy(&self.name[..end]).into_owned()
    }
}

/// 48-byte header that prefixes a `WideDataArray<T>` (only used for
/// `blok`). The inner `Partitions` array follows immediately.
#[derive(Debug, Clone)]
pub struct WideDataArrayHeader {
    pub name: [u8; 32],
    pub maximum_count: u32,
    pub datum_size: u32,
    pub unknown1: u32,
    pub signature: u32,
}

impl WideDataArrayHeader {
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            name: read_u8_array(reader)?,
            maximum_count: read_u32(reader, Endian::Be)?,
            datum_size: read_u32(reader, Endian::Be)?,
            unknown1: read_u32(reader, Endian::Be)?,
            signature: read_u32(reader, Endian::Be)?,
        })
    }
}

/// Leaf datum in the `blok` chunk's nested array: 16 bytes,
/// stored alongside per-element `datum_index` (4 bytes before) and
/// footer (4 bytes after) inside a `DataArray`.
#[derive(Debug, Clone, Copy)]
pub struct TagFileBlock {
    /// Slot identifier + an opaque u16. The high u16 (`identifier`)
    /// must match the `salt` of the lookup [`WideDatumHandle`]'s
    /// datum component; the low u16 (`unknown1`) is preserved.
    pub identifier_and_unknown1: u32,
    /// Index into the `tags` [`PartitionHeap`]'s `entries`. `-1`
    /// (`0xFFFFFFFF`) means "no tag heap entry for this tag".
    pub tag_heap_entry_index: i32,
    /// Index into the `cash` [`PartitionHeap`]'s `entries`. `-1`
    /// when this tag has no pageable-resource payload (most tags).
    pub cache_heap_entry_index: i32,
    /// Trailing u32; observed value `0`. Preserve for round-trip.
    pub unknown4: u32,
}

impl TagFileBlock {
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            identifier_and_unknown1: read_u32(reader, Endian::Be)?,
            tag_heap_entry_index: read_u32(reader, Endian::Be)? as i32,
            cache_heap_entry_index: read_u32(reader, Endian::Be)? as i32,
            unknown4: read_u32(reader, Endian::Be)?,
        })
    }

    /// High u16 of [`Self::identifier_and_unknown1`] — must match the
    /// salt of the lookup handle.
    pub fn identifier(self) -> u16 {
        (self.identifier_and_unknown1 >> 16) as u16
    }
}

/// Leaf datum in a partition's `LruvCache.blocks` array: 24 bytes,
/// stored alongside `datum_index + footer` (8 bytes overhead) inside
/// a `DataArray`. Total per-element stride is 32 bytes.
#[derive(Debug, Clone, Copy)]
pub struct LruvBlock {
    /// Identifier (high u16) + flags (u8) + padding (u8). The
    /// identifier u16 must match the lookup handle's salt.
    pub identifier_flags: u32,
    /// Number of pages this block occupies. Bytes = `page_count << PAGE_BITS`.
    pub page_count: i32,
    /// First page index inside the partition's blob file. Byte offset
    /// = `first_page_index << PAGE_BITS`.
    pub first_page_index: i32,
    pub next_block_index: i32,
    pub previous_block_index: i32,
    pub last_used_frame_index: i32,
}

impl LruvBlock {
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            identifier_flags: read_u32(reader, Endian::Be)?,
            page_count: read_u32(reader, Endian::Be)? as i32,
            first_page_index: read_u32(reader, Endian::Be)? as i32,
            next_block_index: read_u32(reader, Endian::Be)? as i32,
            previous_block_index: read_u32(reader, Endian::Be)? as i32,
            last_used_frame_index: read_u32(reader, Endian::Be)? as i32,
        })
    }

    /// High u16 of [`Self::identifier_flags`] — must match salt of
    /// the lookup handle.
    pub fn identifier(self) -> u16 {
        (self.identifier_flags >> 16) as u16
    }
}

/// 60-byte header inside each `part` chunk, just past the 4-byte
/// `file_index`. Drives the `LruvCache` page layout.
#[derive(Debug, Clone)]
pub struct LruvPersistHeader {
    pub name: [u8; 32],
    pub unknown1: u32,
    pub page_size_bits: u32,
    pub unknown_count1: u32,
    pub unknown_count2: u32,
    pub first_datum: u32,
    pub last_datum: u32,
    pub signature: u32,
}

impl LruvPersistHeader {
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            name: read_u8_array(reader)?,
            unknown1: read_u32(reader, Endian::Be)?,
            page_size_bits: read_u32(reader, Endian::Be)?,
            unknown_count1: read_u32(reader, Endian::Be)?,
            unknown_count2: read_u32(reader, Endian::Be)?,
            first_datum: read_u32(reader, Endian::Be)?,
            last_datum: read_u32(reader, Endian::Be)?,
            signature: read_u32(reader, Endian::Be)?,
        })
    }
}

/// One partition (`part` chunk inside `ptls`). Holds an `LruvCache` —
/// the actual mapping from datum-handle to `(offset_pages, size_pages)`.
#[derive(Debug, Clone)]
pub struct LruvCache {
    pub file_index: u32,
    pub header: LruvPersistHeader,
    pub blocks_header: DataArrayHeader,
    /// Sparse array of [`LruvBlock`], indexed by `DatumHandle.index()`.
    /// Slots not populated on disk are `None`. Use [`Self::resolve`]
    /// to look up by handle (with salt validation).
    pub blocks: Vec<Option<(u16, LruvBlock)>>,
}

impl LruvCache {
    /// Parse a `part` chunk's content (chunk header already consumed,
    /// reader at `payload_start`). `payload_end` is the byte offset
    /// of the byte just past the chunk's content.
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
        payload_end: u64,
    ) -> Result<Self, TagReadError> {
        let file_index = read_u32(reader, Endian::Be)?;
        let header = LruvPersistHeader::read(reader)?;
        let blocks_header = DataArrayHeader::read(reader)?;

        let mut blocks: Vec<Option<(u16, LruvBlock)>> =
            vec![None; blocks_header.maximum_count as usize];
        for _ in 0..blocks_header.actual_count {
            let datum_index = read_u32(reader, Endian::Be)?;
            let block = LruvBlock::read(reader)?;
            let footer = read_u32(reader, Endian::Be)?;
            if footer != FOOTER_BANG {
                return Err(unexpected_footer("LruvBlock", FOOTER_BANG, footer));
            }
            let handle = DatumHandle(datum_index);
            blocks[handle.index() as usize] = Some((handle.salt(), block));
        }

        let arr_footer = read_u32(reader, Endian::Be)?;
        if arr_footer != FOOTER_DFT {
            return Err(unexpected_footer("LruvCache blocks", FOOTER_DFT, arr_footer));
        }

        // Trailing `lvft` after the blocks array — verified by
        // TagTool. We accept its presence but don't enforce when the
        // chunk-content boundary already covers it via padding.
        let lvft_sig = u32::from_be_bytes(*b"lvft");
        if reader.stream_position()? + 4 <= payload_end {
            let tag = read_u32(reader, Endian::Be)?;
            if tag != lvft_sig {
                return Err(unexpected_footer("LruvCache trailer (lvft)", lvft_sig, tag));
            }
        }

        Ok(Self { file_index, header, blocks_header, blocks })
    }

    /// Look up a block by datum handle. Returns `None` for
    /// out-of-range / salt-mismatch / unallocated slots.
    pub fn resolve(&self, handle: DatumHandle) -> Option<LruvBlock> {
        if handle.is_none() {
            return None;
        }
        let slot = self.blocks.get(handle.index() as usize)?;
        let &(salt, block) = slot.as_ref()?;
        if salt != handle.salt() {
            return None;
        }
        Some(block)
    }
}

/// One `(partition_index, datum_handle)` entry — produced by the
/// `hpls` chunk inside a partition heap.
#[derive(Debug, Clone, Copy)]
pub struct PartitionedHeapEntry {
    /// Index into the parent [`PartitionHeap::partitions`]. The top
    /// 2 bits of the on-disk u32 are flags; we mask them off here.
    pub partition_index: u32,
    pub block_handle: DatumHandle,
}

impl PartitionedHeapEntry {
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        let packed = read_u32(reader, Endian::Be)?;
        let block_handle = DatumHandle(read_u32(reader, Endian::Be)?);
        Ok(Self {
            // Match TagTool's `(packed << 2) >> 2` — strip the top
            // two flag bits.
            partition_index: (packed << 2) >> 2,
            block_handle,
        })
    }
}

/// A `tags` or `cash` chunk's parsed form: a flat heap-entry table
/// plus a list of partitions (each backing one of the
/// `tags_N` / `cache_N` blob files).
#[derive(Debug, Clone)]
pub struct PartitionHeap {
    /// Entries produced by the `hpls` chunk. Indexed by
    /// [`TagFileBlock::tag_heap_entry_index`] /
    /// `cache_heap_entry_index`.
    pub entries: Vec<PartitionedHeapEntry>,
    /// Partitions produced by walking `ptls -> part` chunks. Indexed
    /// by [`PartitionedHeapEntry::partition_index`].
    pub partitions: Vec<LruvCache>,
}

impl PartitionHeap {
    /// Parse a `tags` or `cash` chunk's body (chunk header already
    /// consumed, reader at `payload_start`). Walks all sub-chunks
    /// recursively — container chunks (`mtag` / `disk` / `heap` /
    /// `ptls`) recurse; leaf chunks (`hpls`, `part`) populate the
    /// returned table.
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
        chunk: MonolithicChunk,
    ) -> Result<Self, TagReadError> {
        let mut state = PartitionHeapState::default();
        state.walk(reader, chunk.payload_end())?;
        Ok(Self { entries: state.entries, partitions: state.partitions })
    }

    /// Look up a `(partition_file, byte_offset, byte_size)` triple
    /// for the given heap-entry index. Returns `None` if the entry
    /// or its underlying datum doesn't resolve.
    pub fn resolve_entry(&self, entry_index: i32) -> Option<PartitionBlock> {
        if entry_index < 0 {
            return None;
        }
        let entry = self.entries.get(entry_index as usize)?;
        let partition = self.partitions.get(entry.partition_index as usize)?;
        let block = partition.resolve(entry.block_handle)?;
        Some(PartitionBlock {
            file_index: partition.file_index,
            offset: (block.first_page_index as u32 as u64) << PAGE_BITS,
            size: (block.page_count as u32 as u64) << PAGE_BITS,
        })
    }
}

#[derive(Default)]
struct PartitionHeapState {
    entries: Vec<PartitionedHeapEntry>,
    partitions: Vec<LruvCache>,
}

impl PartitionHeapState {
    /// Walk every chunk between the reader's current offset and
    /// `end`. Containers recurse; `hpls` and `part` chunks populate
    /// `entries` / `partitions`. Unknown chunks are skipped.
    fn walk<R: Read + Seek>(
        &mut self,
        reader: &mut std::io::BufReader<R>,
        end: u64,
    ) -> Result<(), TagReadError> {
        while reader.stream_position()? < end {
            let chunk = MonolithicChunk::read(reader)?;
            match &chunk.signature.to_be_bytes() {
                // Pure containers — recurse into their contents.
                b"mtag" | b"disk" | b"heap" | b"ptls" => {
                    self.walk(reader, chunk.payload_end())?;
                }
                b"hpls" => {
                    self.read_hpls(reader)?;
                    reader.seek(std::io::SeekFrom::Start(chunk.payload_end()))?;
                }
                b"part" => {
                    let part = LruvCache::read(reader, chunk.payload_end())?;
                    self.partitions.push(part);
                    reader.seek(std::io::SeekFrom::Start(chunk.payload_end()))?;
                }
                _ => chunk.skip(reader)?,
            }
        }
        Ok(())
    }

    fn read_hpls<R: Read + Seek>(
        &mut self,
        reader: &mut std::io::BufReader<R>,
    ) -> Result<(), TagReadError> {
        let count = read_u32(reader, Endian::Be)?;
        let _maximum_count = read_u32(reader, Endian::Be)?;
        self.entries.reserve(count as usize);
        for _ in 0..count {
            self.entries.push(PartitionedHeapEntry::read(reader)?);
        }
        Ok(())
    }
}

/// Resolved `(file_index, byte_offset, byte_size)` triple — the
/// output of [`PartitionHeap::resolve_entry`] and the input to the
/// blob-file readers in `crate::monolithic::cache`.
#[derive(Debug, Clone, Copy)]
pub struct PartitionBlock {
    /// Index into `tags_N` or `cache_N` (which family depends on
    /// which heap produced it).
    pub file_index: u32,
    /// Absolute byte offset inside the partition blob.
    pub offset: u64,
    /// Byte length of this block.
    pub size: u64,
}

//================================================================================
// `blok` chunk: WideDataArray<TagFileBlock>
//================================================================================

/// Parsed `blok` chunk — a sparse two-level table of [`TagFileBlock`]
/// records, looked up by [`WideDatumHandle`].
#[derive(Debug)]
pub struct TagFileBlocks {
    pub wide_header: WideDataArrayHeader,
    pub partitions_header: DataArrayHeader,
    /// Outer-level partitions, indexed by `WideDatumHandle.partition`.
    /// Each entry stores `(salt, partition)`; `None` for unallocated
    /// slots.
    pub partitions: Vec<Option<(u16, TagFileBlocksPartition)>>,
}

#[derive(Debug)]
pub struct TagFileBlocksPartition {
    pub header: DataArrayHeader,
    /// Inner-level blocks, indexed by `WideDatumHandle.datum`.
    pub blocks: Vec<Option<(u16, TagFileBlock)>>,
}

impl TagFileBlocks {
    /// Parse a `blok` chunk's body (chunk header consumed, reader at
    /// `payload_start`).
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
        _chunk: MonolithicChunk,
    ) -> Result<Self, TagReadError> {
        let wide_header = WideDataArrayHeader::read(reader)?;
        let partitions_header = DataArrayHeader::read(reader)?;

        let mut partitions: Vec<Option<(u16, TagFileBlocksPartition)>> =
            (0..partitions_header.maximum_count).map(|_| None).collect();

        for _ in 0..partitions_header.actual_count {
            let outer_datum_index = read_u32(reader, Endian::Be)?;
            let outer_handle = DatumHandle(outer_datum_index);

            let inner_header = DataArrayHeader::read(reader)?;
            let mut blocks: Vec<Option<(u16, TagFileBlock)>> =
                vec![None; inner_header.maximum_count as usize];

            for _ in 0..inner_header.actual_count {
                let inner_datum_index = read_u32(reader, Endian::Be)?;
                let block = TagFileBlock::read(reader)?;
                let footer = read_u32(reader, Endian::Be)?;
                if footer != FOOTER_BANG {
                    return Err(unexpected_footer("TagFileBlock", FOOTER_BANG, footer));
                }
                let h = DatumHandle(inner_datum_index);
                blocks[h.index() as usize] = Some((h.salt(), block));
            }

            // Inner array footer: `d@ft` + `!@#$`.
            let arr_footer = read_u32(reader, Endian::Be)?;
            if arr_footer != FOOTER_DFT {
                return Err(unexpected_footer(
                    "TagFileBlocks inner array",
                    FOOTER_DFT,
                    arr_footer,
                ));
            }
            let outer_elem_footer = read_u32(reader, Endian::Be)?;
            if outer_elem_footer != FOOTER_BANG {
                return Err(unexpected_footer(
                    "TagFileBlocks outer element",
                    FOOTER_BANG,
                    outer_elem_footer,
                ));
            }

            partitions[outer_handle.index() as usize] = Some((
                outer_handle.salt(),
                TagFileBlocksPartition { header: inner_header, blocks },
            ));
        }

        // Outer array trailer: `d@ft` + `load`.
        let outer_arr_footer = read_u32(reader, Endian::Be)?;
        if outer_arr_footer != FOOTER_DFT {
            return Err(unexpected_footer(
                "TagFileBlocks outer array",
                FOOTER_DFT,
                outer_arr_footer,
            ));
        }
        let load_sig = u32::from_be_bytes(*b"load");
        let load_footer = read_u32(reader, Endian::Be)?;
        if load_footer != load_sig {
            return Err(unexpected_footer("TagFileBlocks `load` trailer", load_sig, load_footer));
        }

        Ok(Self { wide_header, partitions_header, partitions })
    }

    /// Resolve a [`WideDatumHandle`] to a [`TagFileBlock`]. Returns
    /// `None` for invalid handles (out-of-range, salt mismatch,
    /// unallocated slot).
    pub fn resolve(&self, handle: WideDatumHandle) -> Option<TagFileBlock> {
        if handle.partition.is_none() {
            return None;
        }
        let (psalt, partition) = self
            .partitions
            .get(handle.partition.index() as usize)?
            .as_ref()?;
        if *psalt != handle.partition.salt() {
            return None;
        }
        let (bsalt, block) = partition
            .blocks
            .get(handle.datum.index() as usize)?
            .as_ref()?;
        if *bsalt != handle.datum.salt() {
            return None;
        }
        Some(*block)
    }
}

//================================================================================
// Helpers
//================================================================================

fn unexpected_footer(_chunk: &'static str, expected: u32, got: u32) -> TagReadError {
    TagReadError::BadChunkSignature {
        offset: 0,
        expected: expected.to_be_bytes(),
        got: got.to_be_bytes(),
    }
}
