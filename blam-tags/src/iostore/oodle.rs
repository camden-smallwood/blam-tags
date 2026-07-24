//! Oodle codec abstraction for IoStore compression blocks.
//!
//! Reading a UE5 IoStore container only needs *decode*. Writing/repacking
//! (a later phase) needs *encode*, which no pure-Rust library provides — that
//! path will add a second backend (the proprietary `oo2core` via
//! `oodle_loader`) behind this same trait. Keeping the codec behind a trait
//! means the reader never hard-codes a backend and the encode path can drop in
//! without touching call sites.
//!
//! The default backend here is [`OozextractCodec`]: pure-Rust, decode-only,
//! covering the Kraken/Mermaid/Selkie/Leviathan family UE ships.

use std::error::Error;
use std::fmt;

/// Error from an Oodle codec operation.
#[derive(Debug)]
pub enum OodleError {
    /// Decompression failed (corrupt block, wrong size, unsupported codec).
    Decode(String),
    /// The active backend cannot encode (e.g. the pure-Rust decode-only one).
    /// The write path must select an encode-capable backend.
    EncodeUnsupported,
}

impl fmt::Display for OodleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OodleError::Decode(m) => write!(f, "oodle decode failed: {m}"),
            OodleError::EncodeUnsupported => {
                write!(f, "oodle encode is not supported by the active backend")
            }
        }
    }
}

impl Error for OodleError {}

/// A pluggable Oodle backend. `decompress` is mandatory (read path);
/// `compress` defaults to unsupported so decode-only backends need not
/// implement it.
pub trait OodleCodec: Send + Sync {
    /// Decompress `src` into `dst`, which is pre-sized to the block's known
    /// uncompressed length. Implementations must fill all of `dst`.
    fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<(), OodleError>;

    /// Compress `src` (for the future write/repack path). Decode-only
    /// backends return [`OodleError::EncodeUnsupported`].
    fn compress(&self, _src: &[u8]) -> Result<Vec<u8>, OodleError> {
        Err(OodleError::EncodeUnsupported)
    }
}

/// Pure-Rust, decode-only backend built on the `oozextract` crate.
///
/// Zero external/native dependencies, so it always works (CI, cross-platform,
/// no proprietary blob). It cannot encode.
pub struct OozextractCodec;

impl OodleCodec for OozextractCodec {
    fn decompress(&self, src: &[u8], dst: &mut [u8]) -> Result<(), OodleError> {
        oozextract::Extractor::new()
            .read_from_slice(src, dst)
            .map(|_written| ())
            .map_err(|e| OodleError::Decode(e.to_string()))
    }
}

/// The default codec used when a caller doesn't supply its own.
pub fn default_codec() -> Box<dyn OodleCodec> {
    Box::new(OozextractCodec)
}
