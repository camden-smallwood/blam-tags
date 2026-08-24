//! Monolithic tag cache reader (Halo 4 development build format).
//!
//! A monolithic build stores every tag inside a small set of 1 GB
//! "partition" blob files (`tags_0..tags_N` for tag data,
//! `cache_0..cache_M` for pageable-resource payloads). A single index
//! file (`blob_index.dat`) maps each tag's group + name + GUID to a
//! `(partition_index, offset, size)` triple inside the blobs. The
//! whole format is big-endian.
//!
//! The 22 MB `blob_index.dat` is a recursive chunked file:
//!
//! ```text
//! [16-byte session GUID]
//! tgin (root container, v=1)
//! ├── mtfi (monolithic tag file info, v=0)
//! │   └── mtag (container, v=1)
//! │       ├── indx — TagFileIndexHeader + 28-byte entries + name buffer
//! │       ├── tags — partition heap pointing at tags_0..tags_N
//! │       ├── cash — partition heap pointing at cache_0..cache_M
//! │       ├── blok — WideDataArray<TagFileBlock> (entry → heap-slot)
//! │       └── id#6 — build identifier (8 bytes, opaque)
//! ├── mtdp — tag dependency pool (not parsed yet)
//! └── mreg — registry of per-group `blay` layouts (not parsed yet)
//! ```
//!
//! Resolution chain for one tag:
//! 1. [`TagFileEntry::wide_block_index`] → look up in `blok` (the
//!    [`TagFileBlock`] data array) → get a `tag_heap_entry_index`.
//! 2. Index into the `tags` [`PartitionHeap`]'s `entries` →
//!    `(partition_index, block_index)`.
//! 3. Index into that partition's [`LruvCache::blocks`] →
//!    `(first_page_index, page_count)`.
//! 4. Byte offset = `first_page_index << 9`, size = `page_count << 9`
//!    (page size is 512 bytes). Seek `tags_<partition_index>` file
//!    and read.
//!
//! Cache partitions (`cache_N`) follow the same chain through the
//! `cash` heap + the `cache_heap_entry_index` field on
//! [`TagFileBlock`]; that path covers pageable-resource payloads
//! (Phase 4).

mod cache;
mod control;
mod chunk;
mod heap;
mod index;
mod xsync;

pub use cache::{MonolithicCache, SessionGuid};
pub use control::{ControlReadError, ControlReadTally, read_struct_into};
pub use chunk::{read_be_chunk_header, MonolithicChunk};
pub use heap::{
    DataArrayHeader, DatumHandle, LruvBlock, LruvCache, LruvPersistHeader, PartitionBlock,
    PartitionHeap, PartitionedHeapEntry, TagFileBlock, TagFileBlocks, TagFileBlocksPartition,
    WideDataArrayHeader, WideDatumHandle, PAGE_BITS,
};
pub use index::{TagFileEntry, TagFileIndex, TagFileIndexHeader};
pub use xsync::{ControlFixup, FixupAddress, FixupTier, XSyncState, XSyncStateHeader};
