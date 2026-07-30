//! Big-endian chunk-walker primitive for the monolithic index file.
//!
//! The `blob_index.dat` chunk layout is identical to the tag-file
//! chunks parsed by [`crate::io::read_chunk_header`] — 4-byte
//! signature + i32 version + i32 size, all big-endian — but the
//! consumer is structurally different: we walk an unknown sequence
//! of sibling chunks and dispatch on signature. The helpers here are
//! a thin layer over the LE/BE-dispatching readers in [`crate::io`].
//!
//! Use [`MonolithicChunk`] for the read-and-parse loop:
//!
//! ```ignore
//! while reader.stream_position()? < end {
//!     let chunk = MonolithicChunk::read(reader)?;
//!     match &chunk.signature.to_be_bytes() {
//!         b"indx" => parse_indx(reader, chunk.payload_size)?,
//!         // …
//!         _ => chunk.skip(reader)?,
//!     }
//! }
//! ```

use std::io::{Read, Seek, SeekFrom};

use crate::error::TagReadError;
use crate::io::{read_chunk_header, Endian};

/// A single chunk in `blob_index.dat` (or a sub-chunk thereof).
///
/// `payload_start` is the byte offset of the first content byte (i.e.
/// just past this chunk's 12-byte header); `payload_size` is the
/// declared content length from the chunk header. The reader is left
/// positioned at `payload_start` after [`Self::read`].
#[derive(Debug, Clone, Copy)]
pub struct MonolithicChunk {
    /// 4-byte signature, BE-packed (use `signature.to_be_bytes()` to
    /// recover ASCII).
    pub signature: u32,
    /// Per-chunk-type version. Most chunks are v0; `tgin` is v1;
    /// `blok` is v1; the rest are inspection-as-found.
    pub version: u32,
    /// First content byte (caller-relative, absolute file offset).
    pub payload_start: u64,
    /// Declared content length in bytes (does NOT include the 12-byte
    /// header).
    pub payload_size: u32,
}

impl MonolithicChunk {
    /// Read the 12-byte chunk header at the reader's current position
    /// (big-endian). Returns a [`MonolithicChunk`] describing the
    /// chunk; the reader is left positioned at `payload_start`.
    pub fn read<R: Read + Seek>(
        reader: &mut std::io::BufReader<R>,
    ) -> Result<Self, TagReadError> {
        let header = read_chunk_header(reader, Endian::Be)?;
        let payload_start = reader.stream_position()?;
        Ok(Self {
            signature: header.signature,
            version: header.version,
            payload_start,
            payload_size: header.size,
        })
    }

    /// Absolute file offset of the byte just past this chunk's
    /// content. Used as the upper bound of a sub-chunk walk loop.
    #[inline]
    pub fn payload_end(&self) -> u64 {
        self.payload_start + self.payload_size as u64
    }

    /// Skip this chunk's content — useful when the chunk's signature
    /// isn't one we know how to parse.
    pub fn skip<R: Seek>(&self, reader: &mut R) -> Result<(), TagReadError> {
        reader.seek(SeekFrom::Start(self.payload_end()))?;
        Ok(())
    }
}

/// Convenience for code that wants to read just the header without
/// constructing a [`MonolithicChunk`]. Mirrors `crate::io::read_chunk_header`
/// hard-coded to BE.
pub fn read_be_chunk_header<R: Read>(
    reader: &mut std::io::BufReader<R>,
) -> Result<crate::io::TagChunkHeader, TagReadError> {
    read_chunk_header(reader, Endian::Be)
}
