//! Parser for the `indx` chunk inside `blob_index.dat`.
//!
//! Layout:
//! - 0x3C-byte [`TagFileIndexHeader`] (counts + addresses)
//! - `tag_file_count` × 0x1C-byte compressed entries
//! - `name_buffer_size` bytes — null-delimited names indexed by
//!   `entry.name_offset`
//!
//! Address / size fields in the header (`entries_address`,
//! `name_buffer_address`) are runtime memory pointers from the
//! authoring tool — preserved for round-trip but unused for our
//! read-only path.

use std::io::{Read, Seek};

use crate::error::TagReadError;
use crate::io::{read_u32, read_u8_array, Endian};

use super::chunk::MonolithicChunk;

/// The 0x3C-byte fixed header at the start of every `indx` chunk.
/// All integers BE.
#[derive(Debug, Clone)]
pub struct TagFileIndexHeader {
    /// Declared total size of the indx content (matches the
    /// containing chunk header's size).
    pub size: u32,
    /// Schema version. Observed value: 6.
    pub version: i32,
    /// Two opaque 32-bit fields; preserve verbatim.
    pub unknown1: i32,
    pub unknown2: i32,
    /// Stable identifier for this tag-cache build's tag-index schema.
    pub guid: [u8; 16],
    /// Number of [`TagFileEntry`] records that follow.
    pub tag_file_count: i32,
    /// Offset (into the name buffer) of the creator/build label —
    /// the name of whoever ran the tool that wrote this cache.
    pub creator_name_offset: u32,
    /// `tag_file_count * 0x1C`. Stored explicitly so the writer can
    /// allocate without re-multiplying.
    pub entries_size: u32,
    /// Runtime memory address of the entries — opaque on disk.
    pub entries_address: u32,
    /// Third opaque 32-bit field; preserve verbatim.
    pub unknown3: i32,
    /// Length of the trailing name buffer in bytes.
    pub name_buffer_size: i32,
    /// Runtime memory address of the name buffer — opaque on disk.
    pub name_buffer_address: i32,
}

impl TagFileIndexHeader {
    /// Read the 0x3C-byte header at the reader's current position.
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            size: read_u32(reader, Endian::Be)?,
            version: read_u32(reader, Endian::Be)? as i32,
            unknown1: read_u32(reader, Endian::Be)? as i32,
            unknown2: read_u32(reader, Endian::Be)? as i32,
            guid: read_u8_array(reader)?,
            tag_file_count: read_u32(reader, Endian::Be)? as i32,
            creator_name_offset: read_u32(reader, Endian::Be)?,
            entries_size: read_u32(reader, Endian::Be)?,
            entries_address: read_u32(reader, Endian::Be)?,
            unknown3: read_u32(reader, Endian::Be)? as i32,
            name_buffer_size: read_u32(reader, Endian::Be)? as i32,
            name_buffer_address: read_u32(reader, Endian::Be)? as i32,
        })
    }
}

/// One resolved tag-index entry — schema's 0x1C-byte compressed form
/// plus the looked-up name string.
#[derive(Debug, Clone)]
pub struct TagFileEntry {
    /// 4-byte group tag (BE-packed `u32`, e.g. `b"bipd"`).
    pub group_tag: u32,
    /// Two timestamp-shaped opaque fields (likely the FILETIME of
    /// when this entry was written, split into low/high halves).
    /// Preserved for round-trip; not interpreted.
    pub creator_filetime_lo: u32,
    pub creator_filetime_hi: u32,
    /// Packed `(partition_handle: u32, datum_handle: u32)` lookup
    /// key for the `blok` data array. Use `WideDatumHandle::from_u64`
    /// in `heap.rs` to unpack.
    pub wide_block_index: u64,
    /// Stable per-tag id within this cache. Distinct from the entry's
    /// position in the array.
    pub id: u32,
    /// Offset into the name buffer of this entry's null-terminated
    /// name string. Resolved into [`Self::name`] at parse time.
    pub name_offset: u32,
    /// UTF-8 tag-relative path (e.g. `"objects/elite/elite"`). Empty
    /// for entries whose name string is the zero byte at offset 0
    /// (which the format uses as "no name" / placeholder).
    pub name: String,
}

/// Fully parsed `indx` chunk: every tag entry with its resolved name.
#[derive(Debug)]
pub struct TagFileIndex {
    pub header: TagFileIndexHeader,
    pub entries: Vec<TagFileEntry>,
    /// Verbatim name buffer — kept so callers that want raw access
    /// (e.g. resolving `creator_name_offset`) can slice into it.
    pub name_buffer: Vec<u8>,
}

impl TagFileIndex {
    /// Parse an `indx` chunk. `chunk` must describe an `indx` chunk
    /// already read from the stream (header consumed, reader at the
    /// chunk's `payload_start`).
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
        chunk: MonolithicChunk,
    ) -> Result<Self, TagReadError> {
        debug_assert_eq!(&chunk.signature.to_be_bytes(), b"indx");

        let header = TagFileIndexHeader::read(reader)?;

        let count = header.tag_file_count as usize;
        let mut compressed: Vec<CompressedEntry> = Vec::with_capacity(count);
        for _ in 0..count {
            compressed.push(CompressedEntry::read(reader)?);
        }

        let name_buffer_size = header.name_buffer_size as usize;
        let mut name_buffer = vec![0u8; name_buffer_size];
        reader.read_exact(&mut name_buffer)?;

        let mut entries = Vec::with_capacity(count);
        for c in compressed {
            let name = name_at(&name_buffer, c.name_offset as usize);
            entries.push(TagFileEntry {
                group_tag: c.group_tag,
                creator_filetime_lo: c.unknown1,
                creator_filetime_hi: c.unknown2,
                wide_block_index: c.wide_block_index,
                id: c.id,
                name_offset: c.name_offset,
                name,
            });
        }

        Ok(Self { header, entries, name_buffer })
    }
}

/// On-disk 0x1C-byte compressed entry — internal intermediate that we
/// expand into [`TagFileEntry`] with the looked-up name.
struct CompressedEntry {
    group_tag: u32,
    unknown1: u32,
    unknown2: u32,
    wide_block_index: u64,
    id: u32,
    name_offset: u32,
}

impl CompressedEntry {
    fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        Ok(Self {
            group_tag: read_u32(reader, Endian::Be)?,
            unknown1: read_u32(reader, Endian::Be)?,
            unknown2: read_u32(reader, Endian::Be)?,
            wide_block_index: crate::io::read_u64(reader, Endian::Be)?,
            id: read_u32(reader, Endian::Be)?,
            name_offset: read_u32(reader, Endian::Be)?,
        })
    }
}

/// Read a null-terminated UTF-8 (lossy) string starting at `offset`
/// in the name buffer. Empty string for out-of-range or empty entries.
fn name_at(buffer: &[u8], offset: usize) -> String {
    if offset >= buffer.len() {
        return String::new();
    }
    let end = buffer[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|i| offset + i)
        .unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[offset..end]).into_owned()
}
