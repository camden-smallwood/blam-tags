//! Read-path errors.
//!
//! [`TagReadError`] is the typed error returned by every read-path
//! entry point — [`crate::TagFile::read`], [`crate::TagLayout::read`],
//! and the internal stream/chunk parsers. Each variant carries enough
//! context (offsets, expected/actual values, chunk names) to
//! diagnose a malformed tag without re-running with prints.
//!
//! The enum is `#[non_exhaustive]` — variants may be added in future
//! versions as we encounter new corruption modes. Match with a
//! catch-all arm.
//!
//! Schema-import errors live separately in
//! [`crate::TagSchemaError`] — that's the JSON-schema path, not the
//! binary tag-file path.

use std::error::Error;
use std::fmt;

/// All failures that can happen while reading a tag file or its
/// embedded layout. Variants are categorised by the kind of
/// corruption they describe; see each one's doc for the precise
/// trigger.
#[non_exhaustive]
#[derive(Debug)]
pub enum TagReadError {
    /// Underlying I/O failure — short read, bad descriptor, etc.
    Io(std::io::Error),

    /// A chunk header had the wrong 4-byte signature for its
    /// position. Tag files are heavily structured; an unexpected
    /// signature means the file diverged from the format spec we
    /// understand.
    BadChunkSignature {
        /// Byte offset where the chunk header started.
        offset: u64,
        /// Signature this chunk was expected to carry (ASCII bytes).
        expected: [u8; 4],
        /// Signature actually found on disk (ASCII bytes).
        got: [u8; 4],
    },

    /// A chunk's version number was outside what this lib supports.
    /// All known chunks except `tgly` use version 0; `tgly` carries
    /// the layout's payload version (1..=4).
    BadChunkVersion {
        /// Chunk name (e.g. `"blay"`, `"tgft"`, `"blv2"`).
        chunk: &'static str,
        /// Version found on disk.
        version: u32,
    },

    /// A chunk read crossed an offset that didn't match the chunk
    /// header's declared size.
    ChunkSizeMismatch {
        chunk: &'static str,
        /// Offset where the chunk's payload started.
        started_at: u64,
        /// Offset where the parser actually finished reading.
        ended_at: u64,
        /// Offset where the parser was supposed to finish, derived
        /// from the header's `size` field.
        expected_end: u64,
    },

    /// A header field claiming a count of N entries disagreed with
    /// the size of the data following — derived count = size /
    /// entry_size didn't match the header's count.
    CountMismatch {
        chunk: &'static str,
        /// Entry count from the chunk header.
        header_count: u32,
        /// Entry count derived from `payload_size / entry_size`.
        derived_count: u32,
    },

    /// Layout payload version (the `version` field on the layout
    /// chunk's payload header) was outside 1..=4.
    UnsupportedLayoutVersion(u32),

    /// Block-layout sub-version was outside 2..=4.
    UnsupportedBlockLayoutVersion(u32),

    /// A field carried a type-name string the lib doesn't know how
    /// to dispatch on.
    UnsupportedFieldType {
        type_name: String,
    },

    /// A field type that requires a sub-chunk payload (string_id,
    /// tag_reference, data, api_interop, etc.) was missing one in
    /// the on-disk data.
    MissingSubChunk {
        /// Field-type name as it appears in the layout's
        /// `string_data` table.
        field_type: &'static str,
    },

    /// A sub-chunk had an unrecognised 4-byte signature for its
    /// position in the parse tree.
    UnknownSubChunkSignature {
        /// Where in the parse tree this sub-chunk was found
        /// (e.g. `"resource chunk"`, `"block element"`).
        context: &'static str,
        signature: [u8; 4],
    },

    /// A string read from `string_data` (or a tag-reference path)
    /// wasn't valid UTF-8.
    InvalidUtf8 {
        /// Where in the parse tree this string came from
        /// (e.g. `"layout string_data"`, `"tag reference path"`).
        context: &'static str,
    },

    /// A `string_data` offset pointed past end-of-table.
    StringOffsetOutOfBounds {
        /// Offset that was requested.
        offset: u32,
        /// Size of the `string_data` table.
        table_size: usize,
    },

    /// A field's `definition` slot indexed past the end of the layout table it
    /// names. The tag's own layout is internally inconsistent, so the field
    /// cannot be resolved — reported rather than panicking, because a shipped tag
    /// really does hit this and a reader must not take the process down.
    LayoutIndexOutOfBounds {
        /// Which layout table was indexed (e.g. `"resource_layouts"`).
        table: &'static str,
        /// Index the field asked for.
        index: u32,
        /// Number of entries the table actually has.
        len: usize,
    },

    /// Two streams of the same kind were found in one tag file.
    /// Tags carry at most one each of `want` / `info` / `assd`.
    DuplicateOptionalStream {
        signature: [u8; 4],
    },

    /// File ended before all expected bytes had been read.
    UnexpectedEof {
        chunk: &'static str,
    },
}

impl fmt::Display for TagReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error reading tag: {e}"),
            Self::BadChunkSignature { offset, expected, got } => write!(
                f,
                "bad chunk signature at offset 0x{offset:X}: expected {}, got {}",
                show_sig(expected),
                show_sig(got),
            ),
            Self::BadChunkVersion { chunk, version } => {
                write!(f, "{chunk:?} chunk has unsupported version {version}")
            }
            Self::ChunkSizeMismatch { chunk, started_at, ended_at, expected_end } => write!(
                f,
                "{chunk:?} chunk size mismatch: started at 0x{started_at:X}, \
                 ended at 0x{ended_at:X}, expected end 0x{expected_end:X}",
            ),
            Self::CountMismatch { chunk, header_count, derived_count } => write!(
                f,
                "{chunk:?} count mismatch: header says {header_count}, \
                 derived from payload size = {derived_count}",
            ),
            Self::UnsupportedLayoutVersion(v) => {
                write!(f, "unsupported layout payload version {v} (expected 1..=4)")
            }
            Self::UnsupportedBlockLayoutVersion(v) => {
                write!(f, "unsupported block-layout sub-version {v} (expected 2..=4)")
            }
            Self::UnsupportedFieldType { type_name } => {
                write!(f, "unsupported field type {type_name:?}")
            }
            Self::MissingSubChunk { field_type } => write!(
                f,
                "field type {field_type:?} requires a sub-chunk payload but none was found",
            ),
            Self::UnknownSubChunkSignature { context, signature } => write!(
                f,
                "unknown sub-chunk signature {} in {context}",
                show_sig(signature),
            ),
            Self::InvalidUtf8 { context } => {
                write!(f, "invalid UTF-8 in {context}")
            }
            Self::StringOffsetOutOfBounds { offset, table_size } => write!(
                f,
                "string offset {offset} is past end of string table (size {table_size})",
            ),
            Self::LayoutIndexOutOfBounds { table, index, len } => write!(
                f,
                "layout {table} has {len} entries but a field asked for index {index}",
            ),
            Self::DuplicateOptionalStream { signature } => write!(
                f,
                "duplicate optional stream {} — tags carry at most one each of want / info / assd",
                show_sig(signature),
            ),
            Self::UnexpectedEof { chunk } => {
                write!(f, "unexpected EOF while reading {chunk:?} chunk")
            }
        }
    }
}

impl Error for TagReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for TagReadError {
    fn from(e: std::io::Error) -> Self { Self::Io(e) }
}

/// Render a 4-byte chunk signature as ASCII (e.g. `b"blay"` → `"blay"`).
/// Non-printable bytes are shown as `?`.
fn show_sig(sig: &[u8; 4]) -> String {
    let mut s = String::with_capacity(6);
    s.push('"');
    for &b in sig {
        if b.is_ascii_graphic() || b == b' ' {
            s.push(b as char);
        } else {
            s.push('?');
        }
    }
    s.push('"');
    s
}
