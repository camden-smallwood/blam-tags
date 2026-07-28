//! Reader for UE4/UE5 *legacy* `.pak` containers.
//!
//! IoStore (`.utoc`/`.ucas`) carries cooked packages, but a shipped build still
//! stages plenty of content as loose files inside classic `.pak` archives. In
//! Campaign Evolved that is where all of the Wwise audio lives: `pakchunk0`
//! holds the non-localized `.wem`/`.bnk` media (mounted at the staging root),
//! and `pakchunk1..13` hold one language each (mounted directly at
//! `Meteorite/Content/WwiseAudio/`). None of it is reachable through
//! [`IoStoreArchive`](crate::iostore::IoStoreArchive).
//!
//! Scope: the shipping shape — pak version 8..=11, unencrypted index, with a
//! full directory index. Compression is whatever the footer names, decoded
//! through the same [`OodleCodec`] abstraction the IoStore reader uses (plus
//! zlib/zstd when a pak names them).
//!
//! # Layout notes
//!
//! The footer (`FPakInfo`) is fixed-size and sits at the very end of the file:
//! encryption-key GUID (16), encrypted-index flag (1), magic (4), version (4),
//! index offset (8), index size (8), index hash (20), then a fixed array of
//! 32-byte compression-method names.
//!
//! The index holds a mount point, an entry count, and — for version 10+ — an
//! *encoded* entry blob plus a full directory index that maps each path to a
//! byte offset into that blob. Entries there are bit-packed
//! (`FPakFile::DecodePakEntry`); each file's payload is additionally preceded
//! on disk by a re-serialized `FPakEntry` header, which is what
//! [`entry_header_len`] accounts for.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::oodle::{self, OodleCodec};
use crate::iostore::{IoStoreError, Result};

/// `FPakInfo::Magic`.
const PAK_MAGIC: u32 = 0x5A6F12E1;
/// Oldest version whose index layout this reader understands.
const MIN_VERSION: u32 = 8;
/// Newest version verified against real containers.
const MAX_VERSION: u32 = 11;
/// `FPakInfo::CompressionMethodNameLen`.
const METHOD_NAME_LEN: usize = 32;
/// `FPakInfo::MaxNumCompressionMethods` for version 8+.
const MAX_METHODS: usize = 5;
/// Footer size for version 8+: the fixed fields plus the method-name array.
const FOOTER_LEN: u64 = (16 + 1 + 4 + 4 + 8 + 8 + 20 + METHOD_NAME_LEN * MAX_METHODS) as u64;

/// Little-endian cursor over an in-memory index blob.
struct Cur<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, o: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        self.b
            .get(self.o..self.o + n)
            .inspect(|_| self.o += n)
            .ok_or(IoStoreError::Truncated("pak index ran short"))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// UE `FString`: positive length = ASCII, negative = UTF-16; both are
    /// NUL-terminated and the length counts the terminator.
    fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n < 0 {
            let n = n.unsigned_abs() as usize;
            let raw = self.take(n * 2)?;
            let u: Vec<u16> =
                raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(String::from_utf16_lossy(&u).trim_end_matches('\0').to_string())
        } else {
            let raw = self.take(n as usize)?;
            Ok(String::from_utf8_lossy(raw).trim_end_matches('\0').to_string())
        }
    }
}

/// One file's location and compression state, as decoded from the packed index.
#[derive(Debug, Clone)]
struct PakEntry {
    /// Byte offset of the entry's on-disk `FPakEntry` header.
    offset: u64,
    /// Compressed (on-disk) payload size.
    size: u64,
    /// Size after decompression.
    uncompressed_size: u64,
    /// 0 = stored; else 1-based index into the footer's method names.
    method: u32,
    /// Decompressed bytes produced per block.
    block_size: u32,
    /// `(start, size)` of each compressed block, relative to the payload.
    blocks: Vec<(u64, u64)>,
}

/// `FPakFile::DecodePakEntry` — the bit-packed form used by the encoded index.
///
/// Bit 31 marks a 32-bit offset, bit 30 a 32-bit uncompressed size, bit 29 a
/// 32-bit compressed size; bits 23..28 hold the compression-method index, bit
/// 22 the encrypted flag, bits 6..21 the block count, and bits 0..5 the block
/// size divided by 2048 (`0x3F` escapes to an explicit `u32`).
fn decode_entry(blob: &[u8], at: usize) -> Result<PakEntry> {
    let mut c = Cur::new(blob.get(at..).ok_or(IoStoreError::Truncated("pak entry offset"))?);
    let v = c.u32()?;
    let method = (v >> 23) & 0x3F;
    let encrypted = v & (1 << 22) != 0;
    let nblocks = ((v >> 6) & 0xFFFF) as usize;
    let bs_field = v & 0x3F;

    let offset = if v & (1 << 31) != 0 { c.u32()? as u64 } else { c.u64()? };
    let uncompressed_size = if v & (1 << 30) != 0 { c.u32()? as u64 } else { c.u64()? };
    let size = if method != 0 {
        if v & (1 << 29) != 0 { c.u32()? as u64 } else { c.u64()? }
    } else {
        uncompressed_size
    };
    // A single-block entry leaves this field zero; the block then simply spans
    // the whole payload, so the uncompressed length is the real output size.
    let block_size = if bs_field == 0x3F { c.u32()? } else { bs_field << 11 };

    let mut blocks = Vec::new();
    if method != 0 {
        if nblocks == 1 && !encrypted {
            blocks.push((0, size));
        } else {
            let mut base = 0u64;
            for _ in 0..nblocks {
                let bsz = c.u32()? as u64;
                blocks.push((base, bsz));
                base += bsz;
            }
        }
    }
    if encrypted {
        return Err(IoStoreError::Encrypted);
    }
    Ok(PakEntry { offset, size, uncompressed_size, method, block_size, blocks })
}

/// Size of the `FPakEntry` header re-serialized ahead of each payload:
/// offset, size, uncompressed size, method index, 20-byte hash, the block
/// array when compressed, then the encryption flag and block size.
fn entry_header_len(method: u32, nblocks: usize) -> u64 {
    let mut n: u64 = 8 + 8 + 8 + 4 + 20;
    if method != 0 {
        n += 4 + nblocks as u64 * 16;
    }
    n + 1 + 4
}

/// One file in a pak's directory index.
#[derive(Debug, Clone)]
pub struct PakFile {
    /// Path as stored, relative to the container's mount point.
    pub path: String,
    /// Mount point joined with [`path`](Self::path) and normalized, so files
    /// from containers mounted at different depths share one namespace.
    pub mounted_path: String,
    /// Offset into the encoded-entry blob.
    encoded_at: usize,
}

/// An opened legacy `.pak` container.
pub struct PakArchive {
    file: File,
    mount: String,
    methods: Vec<String>,
    codec: Box<dyn OodleCodec>,
    files: Vec<PakFile>,
    by_path: HashMap<String, usize>,
    encoded: Vec<u8>,
}

/// Resolve a UE mount point against an entry path.
///
/// Mount points are recorded relative to the staged executable directory, so
/// they are littered with `../`. Collapsing them yields the path a file would
/// have on disk under the staging root — which is what makes an entry in a
/// container mounted at `.../Content/WwiseAudio/` addressable by the same name
/// as one in a container mounted at the root.
fn normalize_mount(mount: &str, path: &str) -> String {
    let joined = format!("{}/{}", mount.trim_end_matches('/'), path.trim_start_matches('/'));
    let mut out: Vec<&str> = Vec::new();
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

impl PakArchive {
    /// Open the pak at `path` using the default pure-Rust decode codec.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_codec(path, oodle::default_codec())
    }

    /// Open with an explicit codec backend.
    pub fn open_with_codec(path: impl AsRef<Path>, codec: Box<dyn OodleCodec>) -> Result<Self> {
        let mut file = File::open(path.as_ref())?;
        let len = file.metadata()?.len();
        if len < FOOTER_LEN {
            return Err(IoStoreError::Truncated("file shorter than pak footer"));
        }
        file.seek(SeekFrom::Start(len - FOOTER_LEN))?;
        let mut footer = vec![0u8; FOOTER_LEN as usize];
        file.read_exact(&mut footer)?;

        let rd_u32 = |o: usize| u32::from_le_bytes(footer[o..o + 4].try_into().unwrap());
        let rd_u64 = |o: usize| u64::from_le_bytes(footer[o..o + 8].try_into().unwrap());

        if rd_u32(17) != PAK_MAGIC {
            return Err(IoStoreError::BadMagic);
        }
        let version = rd_u32(21);
        if !(MIN_VERSION..=MAX_VERSION).contains(&version) {
            return Err(IoStoreError::UnsupportedVersion(version.min(u8::MAX as u32) as u8));
        }
        if footer[16] != 0 {
            return Err(IoStoreError::Encrypted);
        }
        let idx_off = rd_u64(25);
        let idx_size = rd_u64(33);
        let methods: Vec<String> = (0..MAX_METHODS)
            .map(|i| {
                let s = &footer[61 + i * METHOD_NAME_LEN..61 + (i + 1) * METHOD_NAME_LEN];
                let end = s.iter().position(|&b| b == 0).unwrap_or(s.len());
                String::from_utf8_lossy(&s[..end]).to_string()
            })
            .filter(|s| !s.is_empty())
            .collect();

        if idx_off.saturating_add(idx_size) > len {
            return Err(IoStoreError::Truncated("pak index past end of file"));
        }
        file.seek(SeekFrom::Start(idx_off))?;
        let mut idx = vec![0u8; idx_size as usize];
        file.read_exact(&mut idx)?;

        let mut c = Cur::new(&idx);
        let mount = c.fstring()?;
        let _num_entries = c.i32()?;
        let _path_hash_seed = c.u64()?;
        if c.i32()? != 0 {
            // Path-hash index: offset, size, hash. Redundant with the full
            // directory index, which we prefer because it keeps real names.
            c.u64()?;
            c.u64()?;
            c.take(20)?;
        }
        let (mut fd_off, mut fd_size) = (0u64, 0u64);
        if c.i32()? != 0 {
            fd_off = c.u64()?;
            fd_size = c.u64()?;
            c.take(20)?;
        }
        let enc_len = c.i32()? as usize;
        let encoded = c.take(enc_len)?.to_vec();

        if fd_size == 0 {
            return Err(IoStoreError::Truncated("pak has no full directory index"));
        }
        if fd_off.saturating_add(fd_size) > len {
            return Err(IoStoreError::Truncated("pak directory index past end of file"));
        }
        file.seek(SeekFrom::Start(fd_off))?;
        let mut fd = vec![0u8; fd_size as usize];
        file.read_exact(&mut fd)?;

        let mut d = Cur::new(&fd);
        let ndirs = d.i32()?;
        let mut files = Vec::new();
        let mut by_path = HashMap::new();
        for _ in 0..ndirs {
            let dir = d.fstring()?;
            let nfiles = d.i32()?;
            for _ in 0..nfiles {
                let name = d.fstring()?;
                let at = d.i32()? as usize;
                let path = format!("{dir}{name}");
                let path = path.trim_start_matches('/').to_string();
                let mounted_path = normalize_mount(&mount, &path);
                by_path.insert(mounted_path.clone(), files.len());
                by_path.entry(path.clone()).or_insert(files.len());
                files.push(PakFile { path, mounted_path, encoded_at: at });
            }
        }

        Ok(Self { file, mount, methods, codec, files, by_path, encoded })
    }

    /// The container's mount point, as recorded in the index.
    pub fn mount_point(&self) -> &str {
        &self.mount
    }

    /// Compression method names this container uses, in footer order.
    pub fn methods(&self) -> &[String] {
        &self.methods
    }

    /// Every file in the directory index.
    pub fn files(&self) -> &[PakFile] {
        &self.files
    }

    /// Whether `path` resolves, by either mounted or container-relative name.
    pub fn contains(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    /// Decompressed size of `path` without reading its payload.
    pub fn uncompressed_len(&self, path: &str) -> Result<u64> {
        let f = self.lookup(path)?;
        Ok(decode_entry(&self.encoded, f.encoded_at)?.uncompressed_size)
    }

    fn lookup(&self, path: &str) -> Result<&PakFile> {
        self.by_path
            .get(path)
            .map(|&i| &self.files[i])
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))
    }

    /// Read and decompress one file. `path` may be either the mounted path or
    /// the container-relative one.
    pub fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let entry = {
            let f = self.lookup(path)?;
            decode_entry(&self.encoded, f.encoded_at)?
        };
        let data_at = entry.offset + entry_header_len(entry.method, entry.blocks.len());
        self.file.seek(SeekFrom::Start(data_at))?;
        let mut raw = vec![0u8; entry.size as usize];
        self.file.read_exact(&mut raw)?;

        if entry.method == 0 {
            return Ok(raw);
        }
        let method = self
            .methods
            .get(entry.method as usize - 1)
            .ok_or(IoStoreError::Truncated("compression method index out of range"))?
            .clone();

        // Single-block entries encode no block size; the block covers the whole
        // payload, so the uncompressed length is the output size.
        let block_out =
            if entry.block_size != 0 { entry.block_size as u64 } else { entry.uncompressed_size };

        let mut out = Vec::with_capacity(entry.uncompressed_size as usize);
        let mut left = entry.uncompressed_size;
        for (start, sz) in &entry.blocks {
            let end = (*start + *sz).min(entry.size);
            let src = raw
                .get(*start as usize..end as usize)
                .ok_or(IoStoreError::Truncated("pak block past payload"))?;
            let want = left.min(block_out) as usize;
            let mut dst = vec![0u8; want];
            decompress_block(&*self.codec, &method, src, &mut dst)?;
            out.extend_from_slice(&dst);
            left -= want as u64;
        }
        Ok(out)
    }
}

/// Decompress one block with whichever method the footer named.
fn decompress_block(
    codec: &dyn OodleCodec,
    method: &str,
    src: &[u8],
    dst: &mut [u8],
) -> Result<()> {
    if method.eq_ignore_ascii_case("oodle") {
        codec.decompress(src, dst)?;
        return Ok(());
    }
    if method.eq_ignore_ascii_case("zlib") {
        use std::io::Read as _;
        let mut z = flate2::read::ZlibDecoder::new(src);
        z.read_exact(dst).map_err(|_| IoStoreError::Truncated("zlib block decode failed"))?;
        return Ok(());
    }
    if method.eq_ignore_ascii_case("zstd") {
        let mut d = ruzstd::StreamingDecoder::new(src)
            .map_err(|_| IoStoreError::Truncated("zstd block init failed"))?;
        use std::io::Read as _;
        d.read_exact(dst).map_err(|_| IoStoreError::Truncated("zstd block decode failed"))?;
        return Ok(());
    }
    Err(IoStoreError::Truncated("unsupported pak compression method"))
}

/// Open every `.pak` in `dir` and index them by mounted path, so a lookup can
/// span the whole set (a build splits its staged files across many chunks).
pub struct PakSet {
    paks: Vec<PakArchive>,
    /// mounted path -> index into `paks`
    owner: BTreeMap<String, usize>,
}

impl PakSet {
    /// Open all `.pak` files directly inside `dir`. Containers that fail to
    /// open (stub paks with no directory index, for instance) are skipped.
    pub fn open_dir(dir: impl AsRef<Path>) -> Result<Self> {
        let mut paths: Vec<_> = std::fs::read_dir(dir.as_ref())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pak")))
            .collect();
        paths.sort();

        let mut paks = Vec::new();
        let mut owner = BTreeMap::new();
        for p in paths {
            let Ok(pak) = PakArchive::open(&p) else { continue };
            let i = paks.len();
            for f in pak.files() {
                owner.entry(f.mounted_path.clone()).or_insert(i);
            }
            paks.push(pak);
        }
        Ok(Self { paks, owner })
    }

    /// Number of containers successfully opened.
    pub fn len(&self) -> usize {
        self.paks.len()
    }

    /// Whether no containers were opened.
    pub fn is_empty(&self) -> bool {
        self.paks.is_empty()
    }

    /// Every distinct mounted path across the set.
    pub fn paths(&self) -> impl Iterator<Item = &String> {
        self.owner.keys()
    }

    /// Whether any container provides `mounted_path`.
    pub fn contains(&self, mounted_path: &str) -> bool {
        self.owner.contains_key(mounted_path)
    }

    /// Read a file by mounted path from whichever container holds it.
    pub fn read(&mut self, mounted_path: &str) -> Result<Vec<u8>> {
        let i = *self
            .owner
            .get(mounted_path)
            .ok_or_else(|| IoStoreError::NotFound(mounted_path.to_string()))?;
        self.paks[i].read(mounted_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_normalization_collapses_dot_dot() {
        // chunk0-style: mounted at the staging root.
        assert_eq!(
            normalize_mount("../../../", "Meteorite/Content/WwiseAudio/Media/43/1.wem"),
            "Meteorite/Content/WwiseAudio/Media/43/1.wem"
        );
        // Per-language chunks mount deeper, but must land on the same name.
        assert_eq!(
            normalize_mount("../../../Meteorite/Content/WwiseAudio/", "Media/German/17/2.wem"),
            "Meteorite/Content/WwiseAudio/Media/German/17/2.wem"
        );
    }

    #[test]
    fn header_len_matches_observed_single_block_entry() {
        // Verified against a real chunk0 entry whose on-disk block start was
        // 73 — i.e. exactly the header length for one compressed block.
        assert_eq!(entry_header_len(1, 1), 73);
        // Stored entries carry no block array.
        assert_eq!(entry_header_len(0, 0), 53);
    }
}
