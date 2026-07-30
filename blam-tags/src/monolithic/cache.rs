//! Top-level monolithic-cache reader.
//!
//! [`MonolithicCache`] opens a `tag_cache/` directory (an
//! `blob_index.dat` next to a `blobs/` directory of partition files)
//! and exposes:
//!
//! - [`MonolithicCache::iter_tags`] — every [`TagFileEntry`] in the
//!   cache.
//! - [`MonolithicCache::find_tag`] — lookup by `(group_tag, name)`.
//! - [`MonolithicCache::read_tag_bytes`] — raw on-disk bytes for one
//!   tag, sliced out of a `tags_N` partition.
//! - [`MonolithicCache::read_tag`] — fully parsed [`TagFile`].
//!
//! Pageable-resource payloads (the `cache_N` partitions) aren't wired
//! up yet — that's Phase 4.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::data::{TagBlockData, TagResourceChunk, TagStructData, TagSubChunkContent};
use crate::error::TagReadError;
use crate::file::TagFile;

use super::chunk::MonolithicChunk;
use super::heap::{PartitionBlock, PartitionHeap, TagFileBlocks, WideDatumHandle};
use super::index::{TagFileEntry, TagFileIndex};
use super::xsync::XSyncState;

/// 16-byte session GUID at the start of `blob_index.dat`.
pub type SessionGuid = [u8; 16];

/// An opened monolithic tag cache. Holds the parsed `blob_index.dat`
/// and lazily opens partition blob files (`tags_N` / `cache_N`) on
/// demand.
///
/// **Read-only.** Writing back to monolithic caches isn't planned —
/// the format is dev-only and the on-disk graph (datum salts,
/// partition heaps, LRU bookkeeping) is too coupled to the runtime
/// editor to safely round-trip.
pub struct MonolithicCache {
    /// Directory containing `blob_index.dat` (and `blobs/`).
    root: PathBuf,
    /// 16-byte GUID identifying this build's session.
    pub session_guid: SessionGuid,
    /// All tag entries, in original index order.
    pub tag_index: TagFileIndex,
    /// Resolution table from a tag's `wide_block_index` to a
    /// [`crate::monolithic::TagFileBlock`].
    pub tag_blocks: TagFileBlocks,
    /// Partition heap for the `tags_N` blobs.
    pub tag_heap: PartitionHeap,
    /// Partition heap for the `cache_N` blobs (pageable-resource
    /// payloads).
    pub cache_heap: PartitionHeap,
    /// Lookup table from `(group_tag, name)` → entry index. Built
    /// once at open.
    by_group_and_name: HashMap<(u32, String), usize>,
    /// LRU-shaped file-handle cache for partition blobs. Most reads
    /// hit the same handful of `tags_N`s repeatedly; we keep them
    /// open behind a mutex so the call-site stays `&self`.
    partition_files: Mutex<HashMap<PartitionKey, BufReader<File>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PartitionKey {
    Tags(u32),
    Cache(u32),
}

impl MonolithicCache {
    /// Open the cache rooted at `tag_cache_dir`. Parses
    /// `tag_cache_dir/blob_index.dat` eagerly; partition blobs are
    /// opened lazily on the first read that needs them.
    pub fn open(tag_cache_dir: impl AsRef<Path>) -> Result<Self, TagReadError> {
        let root = tag_cache_dir.as_ref().to_path_buf();
        let index_path = root.join("blob_index.dat");
        let file = File::open(&index_path)?;
        let mut reader = BufReader::with_capacity(256 * 1024, file);

        let mut session_guid = [0u8; 16];
        reader.read_exact(&mut session_guid)?;

        let tgin = MonolithicChunk::read(&mut reader)?;
        if &tgin.signature.to_be_bytes() != b"tgin" {
            return Err(TagReadError::BadChunkSignature {
                offset: 16,
                expected: *b"tgin",
                got: tgin.signature.to_be_bytes(),
            });
        }

        let mut tag_index: Option<TagFileIndex> = None;
        let mut tag_heap: Option<PartitionHeap> = None;
        let mut cache_heap: Option<PartitionHeap> = None;
        let mut tag_blocks: Option<TagFileBlocks> = None;

        walk_tgin(
            &mut reader,
            tgin.payload_end(),
            &mut tag_index,
            &mut tag_heap,
            &mut cache_heap,
            &mut tag_blocks,
        )?;

        let tag_index = tag_index.ok_or(TagReadError::UnknownSubChunkSignature {
            context: "blob_index.dat",
            signature: *b"indx",
        })?;
        let tag_heap = tag_heap.ok_or(TagReadError::UnknownSubChunkSignature {
            context: "blob_index.dat",
            signature: *b"tags",
        })?;
        let cache_heap = cache_heap.ok_or(TagReadError::UnknownSubChunkSignature {
            context: "blob_index.dat",
            signature: *b"cash",
        })?;
        let tag_blocks = tag_blocks.ok_or(TagReadError::UnknownSubChunkSignature {
            context: "blob_index.dat",
            signature: *b"blok",
        })?;

        // Build (group, name) → index lookup.
        let mut by_group_and_name = HashMap::with_capacity(tag_index.entries.len());
        for (i, entry) in tag_index.entries.iter().enumerate() {
            by_group_and_name.insert((entry.group_tag, entry.name.clone()), i);
        }

        Ok(Self {
            root,
            session_guid,
            tag_index,
            tag_blocks,
            tag_heap,
            cache_heap,
            by_group_and_name,
            partition_files: Mutex::new(HashMap::new()),
        })
    }

    /// Iterate every tag in declaration order.
    pub fn iter_tags(&self) -> impl Iterator<Item = &TagFileEntry> {
        self.tag_index.entries.iter()
    }

    /// Number of tags in this cache.
    pub fn len(&self) -> usize {
        self.tag_index.entries.len()
    }

    /// `true` when the cache has zero tag entries.
    pub fn is_empty(&self) -> bool {
        self.tag_index.entries.is_empty()
    }

    /// Look up a tag entry by `(group_tag, name)`. Group tag is the
    /// BE-packed `u32` form (e.g. `u32::from_be_bytes(*b"bipd")`).
    pub fn find_tag(&self, group_tag: u32, name: &str) -> Option<&TagFileEntry> {
        let i = self.by_group_and_name.get(&(group_tag, name.to_string()))?;
        Some(&self.tag_index.entries[*i])
    }

    /// Resolve a tag entry's tag-heap partition block. `None` when
    /// the entry has no tag-heap data (`tag_heap_entry_index == -1`).
    pub fn resolve_tag_block(&self, entry: &TagFileEntry) -> Option<PartitionBlock> {
        let wide = WideDatumHandle::from_u64(entry.wide_block_index);
        let block = self.tag_blocks.resolve(wide)?;
        self.tag_heap.resolve_entry(block.tag_heap_entry_index)
    }

    /// Resolve a tag entry's cache-heap partition block (pageable
    /// resource payload). `None` when this tag has no cache data.
    pub fn resolve_cache_block(&self, entry: &TagFileEntry) -> Option<PartitionBlock> {
        let wide = WideDatumHandle::from_u64(entry.wide_block_index);
        let block = self.tag_blocks.resolve(wide)?;
        self.cache_heap.resolve_entry(block.cache_heap_entry_index)
    }

    /// Read this tag's raw on-disk bytes from its `tags_N` partition,
    /// trimmed to the tag's actual length (the partition block is
    /// always page-aligned and tail-padded with zeros). Errors if
    /// the entry has no tag-heap block, or the partition blob can't
    /// be opened.
    pub fn read_tag_bytes(&self, entry: &TagFileEntry) -> Result<Vec<u8>, TagReadError> {
        let block = self.resolve_tag_block(entry).ok_or_else(|| {
            TagReadError::UnknownSubChunkSignature {
                context: "monolithic tag has no tag-heap block",
                signature: entry.group_tag.to_be_bytes(),
            }
        })?;

        let mut bytes = self.read_partition_bytes(
            PartitionKey::Tags(block.file_index),
            block.offset,
            block.size,
        )?;
        let actual = actual_tag_size(&bytes, entry.group_tag)?;
        bytes.truncate(actual);
        Ok(bytes)
    }

    /// Parse the tag fully — same shape as [`TagFile::read`] from a
    /// standalone file, just sliced out of a monolithic partition.
    ///
    /// After parsing, performs a **resource-hydration** pass: every
    /// pageable-resource field whose payload is an inline `tgxc`
    /// xsync state is rewritten to a regular
    /// `TagResourceChunk::Exploded` with the primary data slurped
    /// from this tag's `cache_N` partition block. Consumers (bitmap,
    /// render-geometry, etc.) then read the resource bytes via the
    /// same API surface they use for MCC's inline-`Exploded` form.
    pub fn read_tag(&self, entry: &TagFileEntry) -> Result<TagFile, TagReadError> {
        let bytes = self.read_tag_bytes(entry)?;
        let mut tag = TagFile::read_from_bytes(&bytes)?;
        self.hydrate_resources(entry, &mut tag)?;
        Ok(tag)
    }

    /// Walk a freshly-parsed tag and convert every xsync pageable
    /// resource into a hydrated [`TagResourceChunk::Exploded`] with
    /// the bytes from this tag's `cache_N` partition block. No-op
    /// for tags with no cache block (most non-bitmap/non-geometry
    /// tags). Errors only on bad xsync metadata or partition reads.
    fn hydrate_resources(
        &self,
        entry: &TagFileEntry,
        tag: &mut TagFile,
    ) -> Result<(), TagReadError> {
        let Some(cache_block) = self.resolve_cache_block(entry) else {
            return Ok(()); // tag has no cache data — nothing to hydrate
        };

        // Slurp the whole cache block into memory once. Per-resource
        // slices reference `cache_location_offset..size` inside it.
        let cache_bytes = self.read_cache_bytes(cache_block)?;

        let chunk_version = tag.endian; // unused; kept as cache-mode marker
        let _ = chunk_version;

        // Hydrate the `tag!` stream (the only stream that carries
        // pageable resources — `want`/`info`/`assd` don't).
        hydrate_block(&mut tag.tag_stream.data, &cache_bytes)?;

        // For render-geometry-carrying tags, walk meshes and rebuild
        // author-format `per mesh temporary[i]` blocks from the
        // hydrated GPU resource so downstream JMS/ASS exporters see
        // the same shape as MCC-native tags. No-op when the tag has
        // no geometry struct or its api resource is MCC-native.
        let _ = crate::render_geometry::hydrate(tag, &cache_bytes);
        Ok(())
    }

    /// Convenience: look up by `(group, name)` and parse in one step.
    pub fn read_tag_by_name(
        &self,
        group_tag: u32,
        name: &str,
    ) -> Result<TagFile, TagReadError> {
        let entry =
            self.find_tag(group_tag, name)
                .ok_or_else(|| TagReadError::UnknownSubChunkSignature {
                    context: "tag not found in monolithic cache",
                    signature: group_tag.to_be_bytes(),
                })?;
        self.read_tag(entry)
    }

    /// Read a byte range from a `cache_N` partition. Used by Phase 4
    /// pageable-resource handling; exposed early so callers can probe
    /// resource layout.
    pub fn read_cache_bytes(
        &self,
        block: PartitionBlock,
    ) -> Result<Vec<u8>, TagReadError> {
        self.read_partition_bytes(PartitionKey::Cache(block.file_index), block.offset, block.size)
    }

    fn partition_path(&self, key: PartitionKey) -> PathBuf {
        let blobs = self.root.join("blobs");
        match key {
            PartitionKey::Tags(i) => blobs.join(format!("tags_{i}")),
            PartitionKey::Cache(i) => blobs.join(format!("cache_{i}")),
        }
    }

    fn read_partition_bytes(
        &self,
        key: PartitionKey,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>, TagReadError> {
        let path = self.partition_path(key);
        let mut handles = self.partition_files.lock().expect("partition mutex poisoned");
        let reader = match handles.get_mut(&key) {
            Some(r) => r,
            None => {
                let file = File::open(&path)?;
                handles.insert(key, BufReader::with_capacity(64 * 1024, file));
                handles.get_mut(&key).unwrap()
            }
        };
        reader.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0u8; size as usize];
        reader.read_exact(&mut bytes)?;
        Ok(bytes)
    }
}

/// Compute the byte length of an actual tag inside its partition
/// block — the block is page-aligned and tail-padded with zeros, so
/// we have to walk the chunk headers to find the real end.
///
/// Layout: 64-byte file header + `tag!` chunk + zero-or-more
/// optional `want`/`info`/`assd` chunks. Each chunk header is 12
/// bytes (4 BE signature + 4 BE version + 4 BE size).
fn actual_tag_size(bytes: &[u8], group_tag: u32) -> Result<usize, TagReadError> {
    const HEADER_SIZE: usize = 64;
    if bytes.len() < HEADER_SIZE {
        return Err(TagReadError::BadChunkSignature {
            offset: 0,
            expected: *b"BLAM",
            got: [0; 4],
        });
    }

    // `tag!` chunk header at offset 64.
    let mut offset = HEADER_SIZE;
    let want_be = u32::from_be_bytes(*b"tag!");
    let sig = read_be_u32_at(bytes, offset)?;
    if sig != want_be {
        return Err(TagReadError::BadChunkSignature {
            offset: offset as u64,
            expected: *b"tag!",
            got: sig.to_be_bytes(),
        });
    }
    let size = read_be_u32_at(bytes, offset + 8)? as usize;
    offset += 12 + size;

    // Optional `want` / `info` / `assd` streams, in order, possibly
    // not present.
    while offset + 12 <= bytes.len() {
        let sig = read_be_u32_at(bytes, offset)?;
        let sig_bytes = sig.to_be_bytes();
        if !matches!(&sig_bytes, b"want" | b"info" | b"assd") {
            break;
        }
        let size = read_be_u32_at(bytes, offset + 8)? as usize;
        offset += 12 + size;
    }

    if offset > bytes.len() {
        return Err(TagReadError::ChunkSizeMismatch {
            chunk: "monolithic tag content",
            started_at: 0,
            ended_at: bytes.len() as u64,
            expected_end: offset as u64,
        });
    }

    let _ = group_tag; // reserved for future cross-checks
    Ok(offset)
}

fn read_be_u32_at(bytes: &[u8], at: usize) -> Result<u32, TagReadError> {
    if at + 4 > bytes.len() {
        return Err(TagReadError::ChunkSizeMismatch {
            chunk: "monolithic tag chunk-header probe",
            started_at: at as u64,
            ended_at: bytes.len() as u64,
            expected_end: (at + 4) as u64,
        });
    }
    Ok(u32::from_be_bytes([
        bytes[at],
        bytes[at + 1],
        bytes[at + 2],
        bytes[at + 3],
    ]))
}

//================================================================================
// Resource hydration walkers
//================================================================================

/// Walk every element of a `tgbl` (`TagBlockData`) and hydrate any
/// xsync resources inside.
fn hydrate_block(
    block: &mut TagBlockData,
    cache_bytes: &[u8],
) -> Result<(), TagReadError> {
    for element in &mut block.elements {
        hydrate_struct(element, cache_bytes)?;
    }
    Ok(())
}

/// Walk every sub-chunk of a `tgst` (`TagStructData`) and hydrate
/// any xsync resources inside.
fn hydrate_struct(
    struct_data: &mut TagStructData,
    cache_bytes: &[u8],
) -> Result<(), TagReadError> {
    for entry in &mut struct_data.sub_chunks {
        match &mut entry.content {
            TagSubChunkContent::Struct(nested) => hydrate_struct(nested, cache_bytes)?,
            TagSubChunkContent::Block(nested_block) => {
                hydrate_block(nested_block, cache_bytes)?;
            }
            TagSubChunkContent::Array(elements) => {
                for elem in elements.iter_mut() {
                    hydrate_struct(elem, cache_bytes)?;
                }
            }
            TagSubChunkContent::Resource(chunk) => {
                hydrate_resource_chunk(chunk, cache_bytes)?;
                // After hydration the resource may have its own
                // nested struct_data to recurse into.
                if let TagResourceChunk::Exploded { struct_data, .. } = chunk {
                    hydrate_struct(struct_data, cache_bytes)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Hydrate a single resource chunk. No-op for variants other than
/// `Xsync`. Replaces `Xsync { payload }` with `Exploded { ... }`
/// where `exploded` is the concatenation of the optional buffer
/// followed by the primary buffer, both sliced from `cache_bytes`
/// at the offsets declared in the xsync state header.
///
/// The optional-first ordering matches how Halo 4 bitmaps store
/// their high-res mip 0 in the optional slot and the remaining mip
/// chain in the primary slot — consumers like
/// [`crate::Bitmap`] can then walk the concatenated buffer in
/// mip-0-first order.
fn hydrate_resource_chunk(
    chunk: &mut TagResourceChunk,
    cache_bytes: &[u8],
) -> Result<(), TagReadError> {
    let (version, payload) = match chunk {
        TagResourceChunk::Xsync { version, payload } => (*version, std::mem::take(payload)),
        _ => return Ok(()),
    };

    let state = XSyncState::parse(&payload, version)?;

    // The xsync state's four (offset, size) words are interleaved in
    // a non-obvious way: each named pair (`cache_location_*`,
    // `optional_location_*`) is split across the TWO regions stored
    // in the cache_block. The cache_block layout is:
    //
    //   [primary buffer : cache_location_size]
    //   [alignment padding to 0x100]
    //   [secondary buffer : optional_location_size]
    //
    // where the secondary buffer starts at byte
    // `cache_location_offset` and the primary buffer at byte
    // `optional_location_offset` (which is always `0` in observed
    // corpora). Empirically verified against Halo 4 monolithic
    // bitmaps: for DXT1 / DXT5 the secondary buffer contains the
    // high-res mip 0 (`optional_location_size` bytes) and the
    // primary buffer contains the smaller mip chain
    // (`cache_location_size` bytes). See
    // [`XSyncStateHeader`]'s field docs for the actual semantics.
    let secondary = slice_or_revert(
        cache_bytes,
        state.header.cache_location_offset,
        state.header.optional_location_size,
    );
    let primary = slice_or_revert(
        cache_bytes,
        state.header.optional_location_offset,
        state.header.cache_location_size,
    );

    let (secondary, primary) = match (secondary, primary) {
        (Ok(s), Ok(p)) => (s, p),
        // Cache offsets out of range — leave the resource un-hydrated
        // rather than failing. Caller's consumer (bitmap / geometry)
        // will report missing data with its own error path.
        _ => {
            *chunk = TagResourceChunk::Xsync { version, payload };
            return Ok(());
        }
    };

    // Concatenate `secondary + primary` so consumers walking the
    // payload in mip-0-first order (e.g. [`crate::Bitmap`] for X360
    // bitmaps) read the high-res buffer first, then the mip chain.
    let mut exploded = Vec::with_capacity(secondary.len() + primary.len());
    exploded.extend_from_slice(secondary);
    exploded.extend_from_slice(primary);

    // Default-empty struct_data for the resource — we don't yet
    // materialize the control-data tree (Phase 4c). Consumers that
    // only need the byte slice read via
    // `TagResource::exploded_payload()` which returns this buffer
    // directly.
    let struct_data = TagStructData {
        struct_index: 0,
        sub_chunks: Vec::new(),
        classic_struct_header: None,
    };

    *chunk = TagResourceChunk::Exploded {
        exploded,
        struct_data,
        xsync_state: Some(Box::new(state)),
    };
    Ok(())
}

/// Slice `cache_bytes[offset..offset+size]` if in range; otherwise
/// return `Err(())`. Zero-size requests return an empty slice
/// (used for resources with no optional buffer).
fn slice_or_revert(cache_bytes: &[u8], offset: u32, size: u32) -> Result<&[u8], ()> {
    if size == 0 {
        return Ok(&[]);
    }
    let off = offset as usize;
    let end = off.saturating_add(size as usize);
    if end > cache_bytes.len() {
        return Err(());
    }
    Ok(&cache_bytes[off..end])
}

/// Internal: recursive walk of `tgin → mtfi → mtag → {indx, tags,
/// cash, blok}`. Unknown / unhandled chunks (`id#6`, `mtdp`, `mreg`)
/// are skipped at this layer; `mreg` will be wired up in Phase 4
/// once we plumb embedded layouts through.
fn walk_tgin<R: Read + Seek>(
    reader: &mut std::io::BufReader<R>,
    end: u64,
    tag_index: &mut Option<TagFileIndex>,
    tag_heap: &mut Option<PartitionHeap>,
    cache_heap: &mut Option<PartitionHeap>,
    tag_blocks: &mut Option<TagFileBlocks>,
) -> Result<(), TagReadError> {
    while reader.stream_position()? < end {
        let chunk = MonolithicChunk::read(reader)?;
        match &chunk.signature.to_be_bytes() {
            b"mtfi" | b"mtag" => {
                walk_tgin(reader, chunk.payload_end(), tag_index, tag_heap, cache_heap, tag_blocks)?;
            }
            b"indx" => {
                *tag_index = Some(TagFileIndex::read(reader, chunk)?);
                reader.seek(SeekFrom::Start(chunk.payload_end()))?;
            }
            b"tags" => {
                *tag_heap = Some(PartitionHeap::read(reader, chunk)?);
                reader.seek(SeekFrom::Start(chunk.payload_end()))?;
            }
            b"cash" => {
                *cache_heap = Some(PartitionHeap::read(reader, chunk)?);
                reader.seek(SeekFrom::Start(chunk.payload_end()))?;
            }
            b"blok" => {
                *tag_blocks = Some(TagFileBlocks::read(reader, chunk)?);
                reader.seek(SeekFrom::Start(chunk.payload_end()))?;
            }
            _ => chunk.skip(reader)?,
        }
    }
    Ok(())
}
