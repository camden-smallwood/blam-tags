//! AKPK (Audiokinetic Package) reader — Halo 4 / H2A Wwise audio containers.
//!
//! Halo 4 and Halo 2 Anniversary store their runtime audio with the Wwise
//! engine rather than FMOD: `<game>/sound/pc/*.pck` are `AKPK` packages, each
//! holding a set of Wwise soundbanks (`.bnk`) and/or streamed media (`.wem`).
//! `sfxbank.pck` carries the embedded-media banks (event graph + inline `.wem`
//! in each bank's `DATA` chunk); `sfxstream.pck` carries the loose streamed
//! `.wem`. Layout (all little-endian on PC):
//!
//! ```text
//!   0  "AKPK"
//!   4  headerLength (u32)  — bytes after this field to the first payload
//!   8  version (u32)       — 1 for H4
//!  12  languagesSize (u32)
//!  16  banksSize (u32)
//!  20  streamsSize (u32)
//!  24  externalsSize (u32)
//!  28  [ languages section ] : count(u32), then count × (nameOffset(u32), id(u32));
//!                              names are NUL-terminated UTF-16LE at
//!                              languagesSectionStart + nameOffset
//!      [ banks section ]     : count(u32), then count × FileEntry
//!      [ streams section ]   : count(u32), then count × FileEntry
//!      [ externals section ] : unused here
//! ```
//!
//! `FileEntry` is 20 bytes: `id(u32), blockSize(u32), fileSize(u32),
//! startBlock(u32), languageId(u32)`; the payload lives at
//! `startBlock * blockSize` for `fileSize` bytes. Only the header region is
//! parsed up front; each entry's bytes are read lazily (`sfxstream.pck` is
//! ~1.7 GB).

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const AKPK_MAGIC: &[u8; 4] = b"AKPK";
const FILE_ENTRY_BYTES: usize = 20;

fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// One packaged file within a `.pck`: an embedded `.bnk` or a streamed `.wem`.
#[derive(Debug, Clone)]
pub struct PckEntry {
    /// Wwise file id (soundbank id for banks; source id for streamed media).
    pub id: u32,
    /// Absolute byte offset of the payload in the `.pck`.
    pub offset: u64,
    /// Payload length in bytes.
    pub size: u64,
    /// Language folder name this entry belongs to (`sfx` for shared audio).
    pub language: String,
}

/// A parsed AKPK package directory. Payload bytes stay on disk.
#[derive(Debug)]
pub struct Pck {
    path: PathBuf,
    /// Embedded soundbanks (`.bnk`) — carry the event graph and inline media.
    pub banks: Vec<PckEntry>,
    /// Streamed media (`.wem`) referenced by the banks' event graph.
    pub streams: Vec<PckEntry>,
}

impl Pck {
    /// Parse a `.pck`'s directory (header + language/bank/stream tables).
    /// Reads only the header region; payloads are read lazily by
    /// [`Pck::read_entry`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;

        let mut fixed = [0u8; 28];
        file.read_exact(&mut fixed)
            .map_err(|e| format!("read AKPK header: {e}"))?;
        if &fixed[0..4] != AKPK_MAGIC {
            return Err(format!("not an AKPK package (magic {:02x?})", &fixed[0..4]));
        }
        let header_len = rd_u32(&fixed, 4) as usize;
        let languages_size = rd_u32(&fixed, 12) as usize;
        let banks_size = rd_u32(&fixed, 16) as usize;
        let streams_size = rd_u32(&fixed, 20) as usize;

        // The header region spans from just after `headerLength` (offset 8) for
        // `header_len` bytes; the section sizes cover everything from offset 28.
        let sections_len = languages_size + banks_size + streams_size;
        if sections_len > header_len {
            return Err(format!(
                "AKPK section sizes ({sections_len}) exceed header ({header_len})"
            ));
        }
        let mut sections = vec![0u8; sections_len];
        file.read_exact(&mut sections)
            .map_err(|e| format!("read AKPK sections: {e}"))?;

        let languages = parse_languages(&sections[..languages_size]);
        let banks = parse_files(&sections[languages_size..languages_size + banks_size], &languages)?;
        let streams = parse_files(
            &sections[languages_size + banks_size..languages_size + banks_size + streams_size],
            &languages,
        )?;

        Ok(Self {
            path,
            banks,
            streams,
        })
    }

    /// Read an entry's raw bytes (a `.bnk` blob or a `.wem`) from disk.
    pub fn read_entry(&self, entry: &PckEntry) -> Result<Vec<u8>, String> {
        self.read_range(entry.offset, entry.size)
    }

    /// Read `size` bytes at an absolute `offset` in the `.pck` (for embedded
    /// `.wem` slices whose location was computed from a bank's DIDX/DATA).
    pub fn read_range(&self, offset: u64, size: u64) -> Result<Vec<u8>, String> {
        let mut file = File::open(&self.path).map_err(|e| format!("reopen pck: {e}"))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| format!("seek: {e}"))?;
        let mut buf = vec![0u8; size as usize];
        file.read_exact(&mut buf)
            .map_err(|e| format!("read: {e}"))?;
        Ok(buf)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Parse the language table into an id → name map. Names are NUL-terminated
/// UTF-16LE at `nameOffset` from the section start.
fn parse_languages(section: &[u8]) -> HashMap<u32, String> {
    let mut map = HashMap::new();
    if section.len() < 4 {
        return map;
    }
    let count = rd_u32(section, 0) as usize;
    let mut p = 4;
    for _ in 0..count {
        if p + 8 > section.len() {
            break;
        }
        let name_off = rd_u32(section, p) as usize;
        let id = rd_u32(section, p + 4);
        p += 8;
        map.insert(id, read_utf16z(section, name_off));
    }
    map
}

/// Parse a bank/stream file table: `count(u32)`, then `count` × 20-byte entry.
fn parse_files(section: &[u8], languages: &HashMap<u32, String>) -> Result<Vec<PckEntry>, String> {
    if section.len() < 4 {
        return Ok(Vec::new());
    }
    let count = rd_u32(section, 0) as usize;
    let mut entries = Vec::with_capacity(count);
    let mut p = 4;
    for _ in 0..count {
        if p + FILE_ENTRY_BYTES > section.len() {
            return Err("AKPK file table truncated".into());
        }
        let id = rd_u32(section, p);
        let block_size = rd_u32(section, p + 4) as u64;
        let file_size = rd_u32(section, p + 8) as u64;
        let start_block = rd_u32(section, p + 12) as u64;
        let lang_id = rd_u32(section, p + 16);
        p += FILE_ENTRY_BYTES;
        entries.push(PckEntry {
            id,
            offset: start_block * block_size,
            size: file_size,
            language: languages.get(&lang_id).cloned().unwrap_or_default(),
        });
    }
    Ok(entries)
}

/// Read a NUL-terminated UTF-16LE string starting at `off` within `buf`.
fn read_utf16z(buf: &[u8], off: usize) -> String {
    let mut units = Vec::new();
    let mut p = off;
    while p + 1 < buf.len() {
        let u = u16::from_le_bytes([buf[p], buf[p + 1]]);
        if u == 0 {
            break;
        }
        units.push(u);
        p += 2;
    }
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set `WWISE_PCK=/path/to/sfxbank.pck` to exercise against real data.
    fn pck_path() -> Option<PathBuf> {
        std::env::var_os("WWISE_PCK").map(PathBuf::from)
    }

    #[test]
    fn parses_akpk_directory() {
        let Some(path) = pck_path() else {
            eprintln!("WWISE_PCK unset; skipping");
            return;
        };
        let pck = Pck::open(&path).expect("open pck");
        eprintln!(
            "{}: {} banks, {} streams",
            path.display(),
            pck.banks.len(),
            pck.streams.len()
        );
        assert!(!pck.banks.is_empty() || !pck.streams.is_empty());
        // The first embedded bank must begin with a Wwise BKHD chunk.
        if let Some(bank0) = pck.banks.first() {
            let bytes = pck.read_entry(bank0).expect("read bank0");
            assert_eq!(&bytes[0..4], b"BKHD", "bank0 should start with BKHD");
        }
    }
}
