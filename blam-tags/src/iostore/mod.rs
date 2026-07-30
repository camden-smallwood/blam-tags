//! Read-only reader for UE5 **IoStore** containers (`.utoc` index + `.ucas`
//! data), used to mount Halo: Campaign Evolved's packaged tags as a virtual
//! filesystem. Gated behind the `iostore` feature.
//!
//! Campaign Evolved is a UE5 remake of Halo 1 on a modified Reach engine. Its
//! Reach tags are cooked into UE5 packages: each tag is a
//! `<name>-<group_longname>.uasset` (a thin Zen wrapper we ignore) paired with
//! a `<name>-<group_longname>.ubulk` whose bytes **are** a byte-complete,
//! self-describing Reach MCC tag file (64-byte header with `BLAM` at offset 60,
//! then a `tag!` stream). So mounting the container and handing each `.ubulk`'s
//! bytes to [`crate::file::TagFile::read_from_bytes`] is all that's needed.
//!
//! Scope of this module: parse the container, resolve a path to its chunk, and
//! return the chunk's decompressed bytes. It knows nothing about tag semantics.
//!
//! Supported layout (validated against `pakchunk0-WinGDK`): IoStore version 8,
//! unencrypted (`Compressed | Indexed`), Oodle-compressed, single partition.
//! Encrypted or multi-partition containers are rejected with a clear error.

//! # Layout
//!
//! The module is four layers, each depending only on the ones above it:
//!
//! | Layer | Module | Unit of work |
//! |---|---|---|
//! | 1 | [`container`] | chunks in a `.utoc`/`.ucas`/`.pak` |
//! | 2 | [`package`] | one cooked Zen package |
//! | 3 | [`object`] | one export's payload |
//! | 4 | [`asset`] | typed decoders (meshes, Wwise events, …) |
//!
//! [`IoStoreArchive`] itself lives here, at the root, because it is the entry
//! point every layer above is reached through.

pub mod asset;
pub mod compat;
pub mod container;
pub mod object;
pub mod package;
pub mod world;

// Back-compatible aliases for the pre-layering paths. Every one of these is the
// same module under its old name, so existing `iostore::zen::…` imports keep
// resolving while callers migrate. Deprecated; remove after one release.
pub use asset::{nanite, skeletal_mesh, static_mesh, wwise_event};
pub use container::{header as container_header, oodle, pak, writer};
pub use object::{unversioned, usmap};
pub use package::{name_map, script_objects, ser, ue_types, zen};

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use container::oodle::{OodleCodec, OodleError};

/// IoStore TOC magic: `-==--==--==--==-`.
const TOC_MAGIC: &[u8; 16] = b"-==--==--==--==-";
/// Header is a fixed 144 bytes across all versions we target.
const HEADER_SIZE: usize = 144;
/// Lowest IoStore version we parse (`ReplaceIoChunkHashWithIoHash`).
const MIN_VERSION: u8 = 8;

/// `EIoContainerFlags` bits we care about.
const FLAG_ENCRYPTED: u32 = 0x02;
const FLAG_SIGNED: u32 = 0x04;

/// Errors from opening or reading an IoStore container.
#[derive(Debug)]
pub enum IoStoreError {
    Io(std::io::Error),
    /// The `.utoc` magic didn't match.
    BadMagic,
    /// Version older than we support.
    UnsupportedVersion(u8),
    /// AES-encrypted container (no key handling here).
    Encrypted,
    /// More than one partition (`.ucas` split into `_s1`, `_s2`, …).
    MultiPartition(u32),
    /// Header/body length didn't add up — truncated or malformed.
    Truncated(&'static str),
    /// A requested path isn't in the directory index.
    NotFound(String),
    /// Oodle decode failed for a block.
    Oodle(OodleError),
    /// A `.uasset` Zen package didn't match the expected layout (refuse to
    /// patch rather than risk corruption).
    Package(&'static str),
    /// The `.ucas` mapping was released so the file could be replaced, and has
    /// not been mapped again. See [`IoStoreArchive::release_partition`].
    PartitionReleased,
}

impl fmt::Display for IoStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IoStoreError::Io(e) => write!(f, "io error: {e}"),
            IoStoreError::BadMagic => write!(f, "not an IoStore .utoc (bad magic)"),
            IoStoreError::UnsupportedVersion(v) => {
                write!(f, "unsupported IoStore version {v} (need >= {MIN_VERSION})")
            }
            IoStoreError::Encrypted => write!(f, "container is AES-encrypted (unsupported)"),
            IoStoreError::MultiPartition(n) => {
                write!(f, "container has {n} partitions (only single-partition supported)")
            }
            IoStoreError::Truncated(w) => write!(f, "malformed/truncated container: {w}"),
            IoStoreError::NotFound(p) => write!(f, "path not found in container: {p}"),
            IoStoreError::Oodle(e) => write!(f, "{e}"),
            IoStoreError::Package(m) => write!(f, "unexpected .uasset package layout: {m}"),
            IoStoreError::PartitionReleased => {
                write!(f, "container partition is released while its files are replaced")
            }
        }
    }
}

impl Error for IoStoreError {}
impl From<std::io::Error> for IoStoreError {
    fn from(e: std::io::Error) -> Self {
        IoStoreError::Io(e)
    }
}
impl From<OodleError> for IoStoreError {
    fn from(e: OodleError) -> Self {
        IoStoreError::Oodle(e)
    }
}

type Result<T> = std::result::Result<T, IoStoreError>;

/// A 12-byte IoStore chunk identifier (`FIoChunkId`), stored verbatim as it
/// appears in the TOC. Layout: `id: u64`, `index: u16` (stored byte-swapped),
/// `pad: u8`, `chunk_type: u8`. We keep the raw bytes so an override container
/// can reuse the exact id of the chunk it replaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FIoChunkId(pub [u8; 12]);

/// `EIoChunkType::ExportBundleData` — a package's `.uasset` payload.
pub const CHUNK_TYPE_EXPORT_BUNDLE: u8 = 1;
/// `EIoChunkType::BulkData` — a package's `.ubulk` payload.
pub const CHUNK_TYPE_BULK_DATA: u8 = 2;
/// `EIoChunkType::OptionalBulkData` — `.uptnl`.
pub const CHUNK_TYPE_OPTIONAL_BULK_DATA: u8 = 3;
/// `EIoChunkType::MemoryMappedBulkData` — `.m.ubulk`.
pub const CHUNK_TYPE_MEMORY_MAPPED_BULK_DATA: u8 = 4;

/// The file extension a payload chunk type lands on, if it has one.
fn chunk_type_extension(chunk_type: u8) -> Option<&'static str> {
    match chunk_type {
        CHUNK_TYPE_EXPORT_BUNDLE => Some(".uasset"),
        CHUNK_TYPE_BULK_DATA => Some(".ubulk"),
        CHUNK_TYPE_OPTIONAL_BULK_DATA => Some(".uptnl"),
        CHUNK_TYPE_MEMORY_MAPPED_BULK_DATA => Some(".m.ubulk"),
        _ => None,
    }
}

impl FIoChunkId {
    /// The `EIoChunkType` byte (e.g. ExportBundleData, BulkData).
    pub fn chunk_type(&self) -> u8 {
        self.0[11]
    }

    /// The 8-byte `FPackageId` prefix. A package's `.uasset`
    /// (`ExportBundleData`) and its `.ubulk` (`BulkData`) share this and differ
    /// only in the trailing type byte, which is what lets a payload chunk be
    /// paired back to the package that names it.
    pub fn package_id(&self) -> [u8; 8] {
        let mut id = [0u8; 8];
        id.copy_from_slice(&self.0[..8]);
        id
    }

    pub fn bytes(&self) -> &[u8; 12] {
        &self.0
    }
}

/// One directory-index file: a path (relative to the container mount) and the
/// TOC chunk index it resolves to.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Path with the mount prefix stripped, e.g.
    /// `Meteorite/Content/.../default-sound.ubulk`.
    pub path: String,
    /// Index into the TOC's chunk / offset-length arrays.
    pub chunk_index: u32,
}

/// The prefix container paths carry before the `/Game/` package root — e.g.
/// `Meteorite/Content/` for `Meteorite/Content/Tags/…`. Taken from the first
/// base entry that has a `Content/` component so the recovered paths line up
/// with the ones the base containers already use.
fn infer_content_prefix(bases: &[&IoStoreArchive]) -> Option<String> {
    for base in bases {
        for entry in base.entries() {
            if let Some(at) = entry.path.find("/Content/") {
                return Some(entry.path[..at + "/Content/".len()].to_string());
            }
        }
    }
    None
}

/// A parsed compression-block entry (12 bytes, bit-packed).
#[derive(Clone, Copy)]
struct Block {
    /// Physical byte offset into the partition (`.ucas`).
    offset: u64,
    /// Compressed (on-disk) size.
    comp_size: u32,
    /// Uncompressed size.
    raw_size: u32,
    /// 0 = stored uncompressed; else 1-based index into method names.
    method: u8,
}

/// An opened IoStore container, backed by an mmap of its `.ucas`.
pub struct IoStoreArchive {
    /// The `.utoc` bytes, kept resident (tens of MB) for offset/block lookups.
    toc: Vec<u8>,
    /// Memory-mapped `.ucas` partition (may be tens of GB — never read whole).
    ///
    /// `None` only while the mapping has been deliberately released by
    /// [`IoStoreArchive::release_partition`], which is the one way the file can
    /// be replaced underneath a mounted container: Windows refuses to truncate a
    /// file that has a mapped section open, so rewriting a container this
    /// process has mounted is impossible while this is held.
    ucas: Option<Mmap>,
    /// Where the partition was mapped from, so it can be mapped again.
    ucas_path: PathBuf,
    codec: Box<dyn OodleCodec>,

    // Header-derived cursors into `toc`.
    entry_count: u32,
    chunkid_off: usize,
    offlen_off: usize,
    cblock_off: usize,
    cblock_size: u64,

    entries: Vec<Entry>,
    by_path: HashMap<String, usize>,
}

impl IoStoreArchive {
    /// The mapped partition, or an error naming what has to happen first.
    ///
    /// Reads go through here rather than touching the field, so a released
    /// mapping is a refused read rather than a panic: the release window belongs
    /// to a caller replacing this container's files, and anything that reads it
    /// in the meantime is asking for bytes that are being overwritten.
    fn partition(&self) -> Result<&Mmap> {
        self.ucas
            .as_ref()
            .ok_or(IoStoreError::PartitionReleased)
    }

    /// Whether the `.ucas` partition is currently mapped.
    pub fn is_partition_mapped(&self) -> bool {
        self.ucas.is_some()
    }

    /// Drop the mapping of this container's `.ucas`, so the file can be replaced.
    ///
    /// Windows refuses to truncate or delete a file that has a mapped section
    /// open (`ERROR_USER_MAPPED_FILE`, os error 1224), which makes rewriting a
    /// container this process has mounted impossible while the mapping is held —
    /// including the common case of re-exporting a mod over an installed copy of
    /// itself. Reads fail with [`IoStoreError::PartitionReleased`] until
    /// [`Self::remap_partition`] runs; the `.utoc` index stays resident, so paths
    /// and chunk ids keep resolving.
    pub fn release_partition(&mut self) {
        self.ucas = None;
    }

    /// Map the `.ucas` partition again, after whatever required its release.
    ///
    /// The file may have been replaced in the meantime — that is the point — so
    /// this maps whatever is at the path now. The `.utoc` index in memory is the
    /// old one; a caller that replaced the container needs to reopen it rather
    /// than rely on this.
    pub fn remap_partition(&mut self) -> Result<()> {
        if self.ucas.is_some() {
            return Ok(());
        }
        let file = File::open(&self.ucas_path).map_err(|e| {
            IoStoreError::Io(std::io::Error::new(
                e.kind(),
                format!("{}: {e}", self.ucas_path.display()),
            ))
        })?;
        // Safety: as at open — this mapping is only ever read from.
        self.ucas = Some(unsafe { Mmap::map(&file)? });
        Ok(())
    }

    /// Open the container whose `.utoc` is at `utoc_path`; the sibling `.ucas`
    /// is opened automatically. Uses the default pure-Rust decode codec.
    pub fn open(utoc_path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_codec(utoc_path, oodle::default_codec())
    }

    /// Open with an explicit codec backend (e.g. an encode-capable one).
    pub fn open_with_codec(
        utoc_path: impl AsRef<Path>,
        codec: Box<dyn OodleCodec>,
    ) -> Result<Self> {
        let utoc_path = utoc_path.as_ref();
        let toc = std::fs::read(utoc_path)?;
        if toc.len() < HEADER_SIZE || &toc[..16] != TOC_MAGIC {
            return Err(IoStoreError::BadMagic);
        }
        let version = toc[16];
        if version < MIN_VERSION {
            return Err(IoStoreError::UnsupportedVersion(version));
        }

        // Header fields (all little-endian). See module docs for the layout.
        let rd_u32 = |off: usize| u32::from_le_bytes(toc[off..off + 4].try_into().unwrap());

        let entry_count = rd_u32(24);
        let cblock_count = rd_u32(28);
        let cmeth_count = rd_u32(36);
        let cmeth_len = rd_u32(40);
        let cblock_size = rd_u32(44) as u64;
        let diridx_size = rd_u32(48) as usize;
        let partition_count = rd_u32(52);
        let flags = rd_u32(80);
        let perfhash_seed_count = rd_u32(84);
        let without_hash_count = rd_u32(96);

        if flags & FLAG_ENCRYPTED != 0 {
            return Err(IoStoreError::Encrypted);
        }
        if partition_count != 1 {
            return Err(IoStoreError::MultiPartition(partition_count));
        }

        // Walk the body cursors in serialized order.
        let mut p = HEADER_SIZE;
        let chunkid_off = p;
        p += entry_count as usize * 12; // FIoChunkId[]
        let offlen_off = p;
        p += entry_count as usize * 10; // FIoOffsetAndLength[]
        p += perfhash_seed_count as usize * 4; // perfect-hash seeds (i32)
        p += without_hash_count as usize * 4; // chunks-without-perfect-hash (i32)
        let cblock_off = p;
        p += cblock_count as usize * 12; // FIoStoreTocCompressedBlockEntry[]
        p += cmeth_count as usize * cmeth_len as usize; // method-name strings
        if flags & FLAG_SIGNED != 0 {
            // int32 hashSize, tocSig[hashSize], blockSig[hashSize], FSHAHash[cblock_count]
            if p + 4 > toc.len() {
                return Err(IoStoreError::Truncated("signature header"));
            }
            let hash_size = i32::from_le_bytes(toc[p..p + 4].try_into().unwrap()) as usize;
            p += 4 + hash_size * 2 + cblock_count as usize * 20;
        }
        let diridx_off = p;
        if diridx_off + diridx_size > toc.len() {
            return Err(IoStoreError::Truncated("directory index"));
        }

        // Override containers carry no directory index (chunks are addressed by
        // id, not path); tolerate an absent one.
        let entries = if diridx_size == 0 {
            Vec::new()
        } else {
            parse_directory_index(&toc[diridx_off..diridx_off + diridx_size])?
        };
        let by_path = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();

        // mmap the sibling .ucas.
        let ucas_path = ucas_sibling(utoc_path);
        let file = File::open(&ucas_path)
            .map_err(|e| IoStoreError::Io(std::io::Error::new(e.kind(), format!("{}: {e}", ucas_path.display()))))?;
        // Safety: we only read; the file is not mutated while mapped.
        let ucas = unsafe { Mmap::map(&file)? };

        Ok(Self {
            toc,
            ucas: Some(ucas),
            ucas_path,
            codec,
            entry_count,
            chunkid_off,
            offlen_off,
            cblock_off,
            cblock_size,
            entries,
            by_path,
        })
    }

    /// All files in the container's directory index.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Rebuild a file list for a container that carries no directory index.
    ///
    /// Override/mod containers address chunks by id, so they ship with
    /// `directory index size = 0` and [`entries`](Self::entries) comes back
    /// empty — the container is perfectly loadable by the game but shows up as
    /// nothing in a browser. The chunk ids and package headers still carry
    /// enough to reconstruct the paths:
    ///
    /// - a chunk id that also appears in one of `bases` takes that container's
    ///   path, which preserves the original casing;
    /// - otherwise an `ExportBundleData` chunk names *itself* — its Zen header's
    ///   `summary.name` is the package path (this is what covers newly created
    ///   and renamed tags, whose ids appear in no base container);
    /// - a bulk chunk then inherits the path of the package sharing its
    ///   [`package_id`](FIoChunkId::package_id), with the extension swapped.
    ///
    /// The `/Game/…` package root is mapped back onto the containers' content
    /// prefix (e.g. `Meteorite/Content/`), taken from `content_prefix` when
    /// given and otherwise inferred from `bases`.
    ///
    /// No-ops (returning 0) when a directory index was already parsed. Returns
    /// the number of entries recovered.
    pub fn recover_entries(
        &mut self,
        bases: &[&IoStoreArchive],
        content_prefix: Option<&str>,
    ) -> usize {
        if !self.entries.is_empty() {
            return 0;
        }
        let prefix = content_prefix
            .map(str::to_string)
            .or_else(|| infer_content_prefix(bases));

        // Every id we can name from a base container, with its original path.
        let mut known: HashMap<[u8; 12], &str> = HashMap::new();
        for base in bases {
            for entry in base.entries() {
                if let Ok(id) = base.chunk_id(entry.chunk_index) {
                    known.insert(id.0, entry.path.as_str());
                }
            }
        }

        let mut paths: Vec<Option<String>> = vec![None; self.entry_count as usize];
        // Package id -> the `.uasset` path naming it, so bulk chunks can follow.
        let mut packages: HashMap<[u8; 8], String> = HashMap::new();

        for i in 0..self.entry_count {
            let Ok(id) = self.chunk_id(i) else { continue };
            let path = match known.get(&id.0) {
                Some(path) => Some((*path).to_string()),
                None if id.chunk_type() == CHUNK_TYPE_EXPORT_BUNDLE => {
                    self.package_path_from_chunk(i, prefix.as_deref())
                }
                None => None,
            };
            let Some(path) = path else { continue };
            if id.chunk_type() == CHUNK_TYPE_EXPORT_BUNDLE {
                packages.insert(id.package_id(), path.clone());
            }
            paths[i as usize] = Some(path);
        }

        // Bulk chunks that no base knew about follow their package.
        for i in 0..self.entry_count {
            if paths[i as usize].is_some() {
                continue;
            }
            let Ok(id) = self.chunk_id(i) else { continue };
            let Some(ext) = chunk_type_extension(id.chunk_type()) else { continue };
            let Some(package) = packages.get(&id.package_id()) else { continue };
            let stem = package.strip_suffix(".uasset").unwrap_or(package);
            paths[i as usize] = Some(format!("{stem}{ext}"));
        }

        let entries: Vec<Entry> = paths
            .into_iter()
            .enumerate()
            .filter_map(|(i, path)| path.map(|path| Entry { path, chunk_index: i as u32 }))
            .collect();
        self.by_path = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.path.clone(), i))
            .collect();
        self.entries = entries;
        self.entries.len()
    }

    /// Read an `ExportBundleData` chunk and turn the package path it declares
    /// into a container-relative path. `None` if the chunk will not parse as a
    /// Zen package.
    fn package_path_from_chunk(&self, chunk_index: u32, prefix: Option<&str>) -> Option<String> {
        use container_header::EIoContainerHeaderVersion;
        use ue_types::EIoStoreTocVersion;
        use zen::FZenPackageHeader;

        let bytes = self.read_chunk(chunk_index).ok()?;
        let header = FZenPackageHeader::deserialize(
            &mut std::io::Cursor::new(&bytes[..]),
            None,
            EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash,
            EIoContainerHeaderVersion::SoftPackageReferences,
            None,
        )
        .ok()?;
        let package = header.name_map.get(header.summary.name).to_string();
        let tail = package.strip_prefix("/Game/")?;
        Some(format!("{}{tail}.uasset", prefix.unwrap_or_default()))
    }

    /// Number of chunks in the TOC (works even without a directory index).
    pub fn chunk_count(&self) -> u32 {
        self.entry_count
    }

    /// Iterate only the `.ubulk` entries (the Reach tag payloads).
    pub fn ublock_entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| e.path.ends_with(".ubulk"))
    }

    /// Read and decompress the bytes for a path (as it appears in
    /// [`Entry::path`]).
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let &idx = self
            .by_path
            .get(path)
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))?;
        self.read_chunk(self.entries[idx].chunk_index)
    }

    /// The raw 12-byte `FIoChunkId` for a TOC chunk index. Used to reuse an
    /// original chunk's id when authoring an override container.
    pub fn chunk_id(&self, chunk_index: u32) -> Result<FIoChunkId> {
        if chunk_index >= self.entry_count {
            return Err(IoStoreError::Truncated("chunk index out of range"));
        }
        let o = self.chunkid_off + chunk_index as usize * 12;
        let mut id = [0u8; 12];
        id.copy_from_slice(&self.toc[o..o + 12]);
        Ok(FIoChunkId(id))
    }

    /// The `FIoChunkId` for a directory-index path.
    pub fn chunk_id_for(&self, path: &str) -> Result<FIoChunkId> {
        let &idx = self
            .by_path
            .get(path)
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))?;
        self.chunk_id(self.entries[idx].chunk_index)
    }

    /// The chunk's index into the TOC arrays for a directory-index path.
    pub fn chunk_index_for(&self, path: &str) -> Result<u32> {
        let &idx = self
            .by_path
            .get(path)
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))?;
        Ok(self.entries[idx].chunk_index)
    }

    /// The uncompressed byte length of a path's chunk, without decompressing it
    /// (from the TOC offset/length table). For a `.ubulk` this is the tag's
    /// byte length.
    pub fn uncompressed_len(&self, path: &str) -> Result<u64> {
        let &idx = self
            .by_path
            .get(path)
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))?;
        Ok(self.offset_length(self.entries[idx].chunk_index)?.1)
    }

    /// Whether a directory-index path exists in this container.
    pub fn contains(&self, path: &str) -> bool {
        self.by_path.contains_key(path)
    }

    /// Find a chunk's TOC index by its raw `FIoChunkId` (linear scan).
    pub fn find_chunk(&self, id: &FIoChunkId) -> Option<u32> {
        (0..self.entry_count).find(|&i| {
            let o = self.chunkid_off + i as usize * 12;
            self.toc[o..o + 12] == id.0
        })
    }

    /// The sibling **bulk-data** (`.ubulk`) chunk for a package chunk index, if
    /// present. In IoStore, bulk data shares the package id but uses chunk-type
    /// `2` (`BulkData`) and a bulk chunk index (usually 0). Returns the
    /// decompressed bulk bytes.
    pub fn read_bulk_for(&self, package_chunk_index: u32, bulk_chunk_index: u16) -> Result<Vec<u8>> {
        let pkg = self.chunk_id(package_chunk_index)?;
        let mut id = pkg.0;
        // [8..10] = big-endian bulk chunk index; [10] = 0; [11] = BulkData (2).
        id[8] = (bulk_chunk_index >> 8) as u8;
        id[9] = (bulk_chunk_index & 0xff) as u8;
        id[10] = 0;
        id[11] = 2;
        let bulk_id = FIoChunkId(id);
        let ci = self
            .find_chunk(&bulk_id)
            .ok_or(IoStoreError::Truncated("no BulkData chunk for package"))?;
        self.read_chunk(ci)
    }

    /// Read and decompress only the first `max_bytes` of a path's chunk,
    /// decompressing just the compression blocks that cover that prefix. Used to
    /// cheaply inspect a package's Zen header (name map + import/export tables +
    /// imported-package names all live at the front, before the bulky export
    /// data) without decompressing the whole mesh/Blueprint chunk. Returns fewer
    /// than `max_bytes` when the chunk itself is shorter.
    pub fn read_prefix(&self, path: &str, max_bytes: usize) -> Result<Vec<u8>> {
        let &idx = self
            .by_path
            .get(path)
            .ok_or_else(|| IoStoreError::NotFound(path.to_string()))?;
        let (offset, length) = self.offset_length(self.entries[idx].chunk_index)?;
        let want = (length as usize).min(max_bytes) as u64;
        if want == 0 {
            return Ok(Vec::new());
        }
        let cbs = self.cblock_size;
        let first = offset / cbs;
        let last = (offset + want - 1) / cbs;

        let mut out: Vec<u8> = Vec::with_capacity(want as usize + cbs as usize);
        for bi in first..=last {
            let blk = self.block(bi as usize)?;
            let start = blk.offset as usize;
            let end = start + blk.comp_size as usize;
            let comp = self
                .partition()?
                .get(start..end)
                .ok_or(IoStoreError::Truncated("block past end of .ucas"))?;
            if blk.method == 0 {
                out.extend_from_slice(comp);
            } else {
                let mut dst = vec![0u8; blk.raw_size as usize];
                self.codec.decompress(comp, &mut dst)?;
                out.extend_from_slice(&dst);
            }
        }
        let in_block = (offset % cbs) as usize;
        let end = (in_block + want as usize).min(out.len());
        Ok(out[in_block.min(out.len())..end].to_vec())
    }

    /// Read and decompress a chunk by its TOC index.
    pub fn read_chunk(&self, chunk_index: u32) -> Result<Vec<u8>> {
        let (offset, length) = self.offset_length(chunk_index)?;
        if length == 0 {
            return Ok(Vec::new());
        }
        let cbs = self.cblock_size;
        let first = offset / cbs;
        let last = (offset + length - 1) / cbs;

        let mut out: Vec<u8> = Vec::with_capacity(length as usize + cbs as usize);
        for bi in first..=last {
            let blk = self.block(bi as usize)?;
            let start = blk.offset as usize;
            let end = start + blk.comp_size as usize;
            let comp = self
                .partition()?
                .get(start..end)
                .ok_or(IoStoreError::Truncated("block past end of .ucas"))?;
            if blk.method == 0 {
                out.extend_from_slice(comp);
            } else {
                let mut dst = vec![0u8; blk.raw_size as usize];
                self.codec.decompress(comp, &mut dst)?;
                out.extend_from_slice(&dst);
            }
        }
        // Slice the requested window out of the concatenated blocks.
        let in_block = (offset % cbs) as usize;
        if in_block + length as usize > out.len() {
            return Err(IoStoreError::Truncated("decoded chunk shorter than expected"));
        }
        Ok(out[in_block..in_block + length as usize].to_vec())
    }

    /// `(offset, length)` for a chunk from the FIoOffsetAndLength array
    /// (5-byte big-endian fields).
    fn offset_length(&self, chunk_index: u32) -> Result<(u64, u64)> {
        if chunk_index >= self.entry_count {
            return Err(IoStoreError::Truncated("chunk index out of range"));
        }
        let o = self.offlen_off + chunk_index as usize * 10;
        let b = &self.toc[o..o + 10];
        Ok((be40(&b[0..5]), be40(&b[5..10])))
    }

    /// Unpack a compression-block entry.
    fn block(&self, i: usize) -> Result<Block> {
        let o = self.cblock_off + i * 12;
        if o + 12 > self.toc.len() {
            return Err(IoStoreError::Truncated("compression block index out of range"));
        }
        let v = u64::from_le_bytes(self.toc[o..o + 8].try_into().unwrap());
        let u = u32::from_le_bytes(self.toc[o + 8..o + 12].try_into().unwrap());
        Ok(Block {
            offset: v & ((1u64 << 40) - 1),
            comp_size: ((v >> 40) & ((1u64 << 24) - 1)) as u32,
            raw_size: u & ((1u32 << 24) - 1),
            method: (u >> 24) as u8,
        })
    }
}

/// Read a 40-bit big-endian integer from a 5-byte slice.
fn be40(b: &[u8]) -> u64 {
    (b[0] as u64) << 32
        | (b[1] as u64) << 24
        | (b[2] as u64) << 16
        | (b[3] as u64) << 8
        | (b[4] as u64)
}

/// `foo.utoc` -> `foo.ucas`.
fn ucas_sibling(utoc: &Path) -> PathBuf {
    utoc.with_extension("ucas")
}

/// Parse the serialized `FIoDirectoryIndexResource` into flat entries.
fn parse_directory_index(di: &[u8]) -> Result<Vec<Entry>> {
    let mut c = Cursor { d: di, p: 0 };
    let mount_raw = c.fstring()?;
    // The mount point carries the container's real content prefix (e.g.
    // `../../../Meteorite/Content/`). Different packs use different mounts
    // (pak0 mounts `../../../`, so its tree already includes `Meteorite/
    // Content`, while level chunks mount `../../../Meteorite/Content/`). Fold
    // the cleaned mount back into every entry path so all packs yield uniform
    // absolute-under-content paths.
    let mount = clean_mount(&mount_raw);

    let ndir = c.u32()? as usize;
    let dirs_off = c.p;
    c.skip(ndir * 16)?; // FIoDirectoryIndexEntry: 4 x u32
    let nfile = c.u32()? as usize;
    let files_off = c.p;
    c.skip(nfile * 12)?; // FIoFileIndexEntry: 3 x u32
    let nstr = c.u32()? as usize;
    let mut strings = Vec::with_capacity(nstr);
    for _ in 0..nstr {
        strings.push(c.fstring()?);
    }

    let dir_field = |i: usize, f: usize| -> u32 {
        let o = dirs_off + i * 16 + f * 4;
        u32::from_le_bytes(di[o..o + 4].try_into().unwrap())
    };
    let file_field = |i: usize, f: usize| -> u32 {
        let o = files_off + i * 12 + f * 4;
        u32::from_le_bytes(di[o..o + 4].try_into().unwrap())
    };

    const INVALID: u32 = 0xFFFF_FFFF;
    let mut out = Vec::new();
    // Iterative tree walk (avoid recursion for deep trees). Stack of
    // (dir_index, path_prefix).
    let mut stack: Vec<(u32, String)> = vec![(0, String::new())];
    while let Some((di_idx, prefix)) = stack.pop() {
        let mut d = di_idx;
        while d != INVALID {
            let name = dir_field(d as usize, 0);
            let first_child = dir_field(d as usize, 1);
            let next_sib = dir_field(d as usize, 2);
            let first_file = dir_field(d as usize, 3);

            let path = if name == INVALID {
                prefix.clone()
            } else {
                let seg = strings.get(name as usize).map(String::as_str).unwrap_or("");
                if prefix.is_empty() {
                    seg.to_string()
                } else {
                    format!("{prefix}/{seg}")
                }
            };

            // Files directly under this dir.
            let mut fe = first_file;
            while fe != INVALID {
                let fname = file_field(fe as usize, 0);
                let next_file = file_field(fe as usize, 1);
                let user_data = file_field(fe as usize, 2);
                let seg = strings.get(fname as usize).map(String::as_str).unwrap_or("");
                let full = if path.is_empty() {
                    seg.to_string()
                } else {
                    format!("{path}/{seg}")
                };
                out.push(Entry {
                    path: full,
                    chunk_index: user_data,
                });
                fe = next_file;
            }

            if first_child != INVALID {
                stack.push((first_child, path.clone()));
            }
            d = next_sib;
        }
    }
    // Prepend the cleaned mount prefix so paths are uniform across packs.
    if !mount.is_empty() {
        for e in &mut out {
            e.path = format!("{mount}/{}", e.path);
        }
    }
    Ok(out)
}

/// Strip a UE mount point down to its meaningful content prefix by removing
/// leading `../` and `/` segments: `../../../Meteorite/Content/` → `Meteorite/
/// Content`, `../../../` → ``, `/` → ``.
fn clean_mount(mount: &str) -> String {
    let mut s = mount;
    loop {
        if let Some(rest) = s.strip_prefix("../") {
            s = rest;
        } else if let Some(rest) = s.strip_prefix('/') {
            s = rest;
        } else {
            break;
        }
    }
    s.trim_end_matches('/').to_string()
}

/// Minimal little-endian cursor for the directory-index blob.
struct Cursor<'a> {
    d: &'a [u8],
    p: usize,
}

impl Cursor<'_> {
    fn u32(&mut self) -> Result<u32> {
        if self.p + 4 > self.d.len() {
            return Err(IoStoreError::Truncated("dir index u32"));
        }
        let v = u32::from_le_bytes(self.d[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        Ok(v)
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if self.p + n > self.d.len() {
            return Err(IoStoreError::Truncated("dir index array"));
        }
        self.p += n;
        Ok(())
    }

    /// UE `FString`: i32 length. 0 = empty; >0 = ASCII (len incl NUL);
    /// <0 = UTF-16LE (-len code units incl NUL).
    fn fstring(&mut self) -> Result<String> {
        if self.p + 4 > self.d.len() {
            return Err(IoStoreError::Truncated("fstring length"));
        }
        let len = i32::from_le_bytes(self.d[self.p..self.p + 4].try_into().unwrap());
        self.p += 4;
        if len == 0 {
            return Ok(String::new());
        }
        if len > 0 {
            let n = len as usize;
            if self.p + n > self.d.len() {
                return Err(IoStoreError::Truncated("fstring ascii"));
            }
            let bytes = &self.d[self.p..self.p + n];
            self.p += n;
            let s = bytes.split(|&b| b == 0).next().unwrap_or(&[]);
            Ok(String::from_utf8_lossy(s).into_owned())
        } else {
            let n = (-len) as usize;
            let bytes = n * 2;
            if self.p + bytes > self.d.len() {
                return Err(IoStoreError::Truncated("fstring utf16"));
            }
            let units: Vec<u16> = self.d[self.p..self.p + bytes]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            self.p += bytes;
            let s: String = String::from_utf16_lossy(&units);
            Ok(s.trim_end_matches('\0').to_string())
        }
    }
}

/// True if `payload` is a Reach MCC tag file: at least a 64-byte header with
/// the `BLAM` signature at offset 0x3C, in either byte order (`BLAM`
/// big-endian, `MALB` little-endian). Definitive way to tell a real tag
/// `.ubulk` from UE bulk data (texture mips, Wwise audio) that also ends in
/// `.ubulk`.
pub fn is_tag_payload(payload: &[u8]) -> bool {
    payload.len() >= 64 && matches!(&payload[60..64], b"BLAM" | b"MALB")
}

/// Split a `.ubulk`/`.uasset` cooked-tag filename into `(tag_name,
/// group_longname)`. The cook names each tag `<name>-<group_longname>.<ext>`,
/// where `group_longname` is the Reach tag group's long name (e.g. `sound`,
/// `survival_mode_globals`). Returns `None` when the stem has no `-group`.
pub fn parse_ublock_stem(filename: &str) -> Option<(&str, &str)> {
    let stem = filename
        .rsplit('/')
        .next()
        .unwrap_or(filename)
        .rsplit_once('.')
        .map(|(s, _ext)| s)
        .unwrap_or(filename);
    let (name, group) = stem.rsplit_once('-')?;
    if name.is_empty() || group.is_empty() {
        return None;
    }
    Some((name, group))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real container to validate against. Skipped when absent so CI (which
    /// doesn't ship the 37 GB game) stays green.
    const PAK0: &str =
        "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/pakchunk0-WinGDK.utoc";

    #[test]
    fn stem_split() {
        assert_eq!(parse_ublock_stem("a/b/default-sound.ubulk"), Some(("default", "sound")));
        assert_eq!(
            parse_ublock_stem("firefight_globals-survival_mode_globals.uasset"),
            Some(("firefight_globals", "survival_mode_globals"))
        );
        assert_eq!(parse_ublock_stem("T_GridChecker_A.ubulk"), None);
    }

    #[test]
    fn mount_and_decode_real_tags() {
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let archive = IoStoreArchive::open(PAK0).expect("open pakchunk0");
        assert!(archive.entries().len() > 100_000, "expected a full directory index");

        // Take a sample of name-candidate tags; every one whose payload really
        // is a tag (BLAM at 0x3C) must decode+parse without error.
        let candidates: Vec<&Entry> = archive
            .ublock_entries()
            .filter(|e| parse_ublock_stem(&e.path).is_some())
            .take(2000)
            .collect();
        assert!(candidates.len() > 500, "expected many candidate tags");

        let mut tags = 0usize;
        for e in candidates {
            let bytes = archive.read(&e.path).expect("decode chunk");
            if is_tag_payload(&bytes) {
                tags += 1;
                crate::file::TagFile::read_from_bytes(&bytes)
                    .unwrap_or_else(|err| panic!("tag {} failed to parse: {err}", e.path));
            }
        }
        assert!(tags > 400, "expected the sample to contain real tags, got {tags}");
    }
}
