//! Minimal writer for **override/overlay** UE5 IoStore containers (v8,
//! unencrypted, single-partition, uncompressed).
//!
//! An override container replaces specific chunks of a shipping game by reusing
//! their exact [`FIoChunkId`]s; mounted at higher priority (a `_P` suffix) the
//! engine resolves those ids to our bytes (last-mounted-wins). It carries no
//! `ContainerHeader` — the base game already supplies the package-store entry;
//! serving raw chunks by id is sufficient (this mirrors retoc's `pack-raw`).
//!
//! Every override is written as the standard IoStore **triplet**: `.utoc` +
//! `.ucas` + a `.pak` stub. The stock UE loader (`FPakPlatformFile`) discovers
//! containers by scanning `Paks/*.pak` and derives the `.utoc`/`.ucas` from that
//! path, so the (empty) `.pak` is required for the container to be mounted at
//! all — see `empty_pak_stub`.
//!
//! All chunks are stored uncompressed (compression method 0), so no Oodle
//! *encoder* is needed. Layout matches [`crate::iostore::IoStoreArchive`] exactly and is
//! validated by round-tripping through it.

use crate::iostore::imports::ImportTarget;
use crate::iostore::ue_types::FPackageObjectIndex;
use crate::iostore::zen::FZenPackageHeader;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Cursor, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::header::{EIoContainerHeaderVersion, FIoContainerHeader, StoreEntry};
use crate::iostore::package::ue_types::{FIoContainerId, FPackageId};
use crate::iostore::{Entry, FIoChunkId, IoStoreArchive, IoStoreError, Result};

/// `EIoChunkType` raw on-disk bytes (UE5 numbering, version 8).
pub const CHUNK_TYPE_EXPORT_BUNDLE_DATA: u8 = 1;
pub const CHUNK_TYPE_BULK_DATA: u8 = 2;
const CHUNK_TYPE_CONTAINER_HEADER: u8 = 6;

/// Build a 12-byte `FIoChunkId`: `[package_id u64 LE][index u16 LE][0][type]`.
pub fn make_chunk_id(package_id: u64, index: u16, chunk_type: u8) -> FIoChunkId {
    let mut b = [0u8; 12];
    b[0..8].copy_from_slice(&package_id.to_le_bytes());
    b[8..10].copy_from_slice(&index.to_le_bytes());
    b[11] = chunk_type;
    FIoChunkId(b)
}

const COMPRESSION_BLOCK_SIZE: u64 = 0x10000;
const HEADER_SIZE: usize = 144;

/// Accumulates chunks (id + uncompressed bytes) and writes a `.utoc`/`.ucas`
/// override container.
pub struct OverrideContainerWriter {
    mount_point: String,
    chunks: Vec<(FIoChunkId, Vec<u8>)>,
    /// Packages this container declares (for the ContainerHeader's package
    /// store). Needed so a *new* package is locatable by the engine.
    packages: Vec<(FPackageId, StoreEntry)>,
    /// Package-name → new-id redirects (renames): existing references to the
    /// old name resolve to the new package.
    redirects: Vec<(String, FPackageId)>,
}

impl OverrideContainerWriter {
    /// `mount_point` is stored in the header only for completeness; an id-based
    /// override doesn't need a directory index. `"../../../"` matches the game.
    pub fn new(mount_point: impl Into<String>) -> Self {
        Self {
            mount_point: mount_point.into(),
            chunks: Vec::new(),
            packages: Vec::new(),
            redirects: Vec::new(),
        }
    }

    /// Add a chunk, reusing the original `id` verbatim (as read from the base
    /// container via [`crate::iostore::IoStoreArchive::chunk_id`]).
    pub fn add_chunk(&mut self, id: FIoChunkId, data: Vec<u8>) {
        self.chunks.push((id, data));
    }

    /// Add a package chunk (a `.uasset` ExportBundleData) and register it in the
    /// container's package store, so a newly-created package can be located.
    pub fn add_package(
        &mut self,
        id: FIoChunkId,
        data: Vec<u8>,
        package_id: FPackageId,
        store: StoreEntry,
    ) {
        self.chunks.push((id, data));
        self.packages.push((package_id, store));
    }

    /// Redirect an old package name to a new package id (for renames).
    pub fn add_redirect(&mut self, old_package_name: &str, new_id: FPackageId) {
        self.redirects.push((old_package_name.to_string(), new_id));
    }

    /// Write the container. `utoc_path` must end in `.utoc`; the sibling
    /// `.ucas` is written alongside. The container id is derived from the file
    /// stem (CityHash64 of its lowercased UTF-16 name), matching the engine.
    pub fn write(&self, utoc_path: &Path) -> Result<()> {
        let stem = utoc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or(IoStoreError::Truncated("utoc path has no stem"))?;
        let container_id = container_id_from_name(stem);

        // Build the ContainerHeader chunk when this container declares packages
        // or redirects (create/rename). Serialized + 16-byte aligned, written as
        // a chunk with id create(container_id, 0, ContainerHeader).
        let header_chunk: Option<(FIoChunkId, Vec<u8>)> =
            if self.packages.is_empty() && self.redirects.is_empty() {
                None
            } else {
                // A version the target build rejects would escape silently from
                // here; nothing downstream checks it.
                if crate::iostore::compat::check_writable_container_header_version(
                    EIoContainerHeaderVersion::SoftPackageReferences,
                )
                .is_err()
                {
                    return Err(IoStoreError::Package(
                        "unsupported container header version",
                    ));
                }
                let mut header = FIoContainerHeader::new(
                    EIoContainerHeaderVersion::SoftPackageReferences,
                    FIoContainerId(container_id),
                );
                for (pid, store) in &self.packages {
                    header.add_package(*pid, store.clone());
                }
                for (name, nid) in &self.redirects {
                    header
                        .add_package_redirect(name, *nid)
                        .map_err(|_| IoStoreError::Package("failed to add package redirect"))?;
                }
                let mut buf = std::io::Cursor::new(Vec::new());
                header
                    .serialize(&mut buf)
                    .map_err(|_| IoStoreError::Package("failed to serialize container header"))?;
                let mut bytes = buf.into_inner();
                let aligned = (bytes.len() + 15) & !15; // 16-byte align
                bytes.resize(aligned, 0);
                Some((
                    make_chunk_id(container_id, 0, CHUNK_TYPE_CONTAINER_HEADER),
                    bytes,
                ))
            };
        let entry_count = self.chunks.len() + header_chunk.is_some() as usize;

        // --- .ucas + per-chunk TOC arrays ---
        let ucas_path = utoc_path.with_extension("ucas");
        let mut ucas = BufWriter::new(File::create(&ucas_path)?);
        let mut ucas_offset: u64 = 0;

        let mut chunk_ids = Vec::with_capacity(entry_count * 12);
        let mut offset_lengths = Vec::with_capacity(entry_count * 10);
        let mut blocks: Vec<[u8; 12]> = Vec::new();
        let mut metas = Vec::with_capacity(entry_count * 24);

        for (id, data) in self.chunks.iter().chain(header_chunk.as_ref()) {
            let start_block = blocks.len() as u64;
            // Split into 64 KiB uncompressed blocks written straight to .ucas.
            let mut written = 0usize;
            while written < data.len() {
                let end = (written + COMPRESSION_BLOCK_SIZE as usize).min(data.len());
                let block = &data[written..end];
                ucas.write_all(block)?;
                blocks.push(encode_block(
                    ucas_offset,
                    block.len() as u32,
                    block.len() as u32,
                    0,
                ));
                ucas_offset += block.len() as u64;
                written = end;
            }
            // An empty chunk still needs a (zero-length) block? Real chunks are
            // non-empty; guard anyway so the offset table stays consistent.

            chunk_ids.extend_from_slice(id.bytes());
            // Logical offset is block-aligned; physical offset lives in blocks.
            offset_lengths.extend_from_slice(&encode_offset_length(
                start_block * COMPRESSION_BLOCK_SIZE,
                data.len() as u64,
            ));
            // v8 meta: 20-byte Blake3 of the uncompressed chunk + flags + pad.
            let hash = blake3::hash(data);
            metas.extend_from_slice(&hash.as_bytes()[..20]);
            metas.push(0); // flags
            metas.extend_from_slice(&[0u8; 3]); // pad
        }
        ucas.flush()?;

        // --- .utoc ---
        let mut toc = Vec::with_capacity(HEADER_SIZE + chunk_ids.len() + blocks.len() * 12);
        write_header(
            &mut toc,
            entry_count as u32,
            blocks.len() as u32,
            container_id,
        );
        toc.extend_from_slice(&chunk_ids);
        toc.extend_from_slice(&offset_lengths);
        for b in &blocks {
            toc.extend_from_slice(b);
        }
        // No compression method names (all method 0), no directory index (id
        // override), no signatures. Meta array closes it out.
        toc.extend_from_slice(&metas);

        // mount_point kept for API symmetry; unused without a directory index.
        let _ = &self.mount_point;

        std::fs::write(utoc_path, &toc)?;

        // Emit the sibling `.pak` stub. The stock-UE loader discovers containers
        // by scanning `Paks/*.pak` and derives the `.utoc`/`.ucas` from that
        // path, so without a `.pak` next to us the override is never mounted.
        std::fs::write(utoc_path.with_extension("pak"), empty_pak_stub())?;
        Ok(())
    }
}

/// Build a minimal, empty UE5 `.pak` stub to sit beside an override container.
///
/// Campaign Evolved (stock UE5 `FPakPlatformFile`) discovers containers by
/// scanning `Paks/*.pak` and derives the sibling `.utoc`/`.ucas` from the `.pak`
/// path — so an IoStore override is only ever mounted when a `.pak` exists next
/// to it. The game's own IoStore-only chunks ship exactly this: a 339-byte pak
/// carrying zero file entries (all real data lives in the `.ucas`). We generate
/// the same thing clean-room.
///
/// `PakFileVersion 11`, unencrypted, mount point `/`, zero entries, with the
/// present-but-empty PathHash + FullDirectory sub-indexes UnrealPak emits. The
/// pak encodes nothing about the mod (its name or chunks), so a single stub
/// serves every override; only the on-disk filename (`<stem>_P.pak`) matters for
/// discovery. Layout matches the shipped stubs byte-for-byte apart from the
/// (arbitrary) `PathHashSeed` and the index hash that depends on it.
fn empty_pak_stub() -> Vec<u8> {
    use sha1::{Digest, Sha1};
    fn sha1(bytes: &[u8]) -> [u8; 20] {
        let mut h = Sha1::new();
        h.update(bytes);
        h.finalize().into()
    }

    const MAGIC: u32 = 0x5A6F_12E1;
    const VERSION: i32 = 11; // PakFileVersion matching the game's own stubs

    // Empty secondary indexes: the path-hash index is a single u64 count (0),
    // the full-directory index a single i32 count (0).
    let path_hash_index = 0u64.to_le_bytes().to_vec(); // 8 bytes
    let full_dir_index = 0i32.to_le_bytes().to_vec(); // 4 bytes

    // Primary index. Its tail after `PathHashSeed` is a fixed 88 bytes:
    //   hasPHI(4) + PHIOffset(8) + PHISize(8) + PHIHash(20)
    // + hasFDI(4) + FDIOffset(8) + FDISize(8) + FDIHash(20)
    // + EncodedEntriesSize(4) + NumFiles(4)
    let mount: &[u8] = b"/\0"; // FString: 2 chars incl. null terminator
    let mut primary = Vec::new();
    primary.extend_from_slice(&(mount.len() as i32).to_le_bytes()); // MountPoint len
    primary.extend_from_slice(mount);
    primary.extend_from_slice(&0i32.to_le_bytes()); // NumEntries
    primary.extend_from_slice(&0u64.to_le_bytes()); // PathHashSeed (arbitrary)
    let primary_size = primary.len() + 88;
    // PHI/FDI offsets are absolute; the primary index sits at file offset 0.
    let phi_offset = primary_size as i64;
    let fdi_offset = (primary_size + path_hash_index.len()) as i64;
    primary.extend_from_slice(&1i32.to_le_bytes()); // bReaderHasPathHashIndex
    primary.extend_from_slice(&phi_offset.to_le_bytes());
    primary.extend_from_slice(&(path_hash_index.len() as i64).to_le_bytes());
    primary.extend_from_slice(&sha1(&path_hash_index));
    primary.extend_from_slice(&1i32.to_le_bytes()); // bReaderHasFullDirectoryIndex
    primary.extend_from_slice(&fdi_offset.to_le_bytes());
    primary.extend_from_slice(&(full_dir_index.len() as i64).to_le_bytes());
    primary.extend_from_slice(&sha1(&full_dir_index));
    primary.extend_from_slice(&0i32.to_le_bytes()); // EncodedPakEntries size
    primary.extend_from_slice(&0i32.to_le_bytes()); // NumFiles
    debug_assert_eq!(primary.len(), primary_size);

    let index_hash = sha1(&primary);

    let mut pak =
        Vec::with_capacity(primary.len() + path_hash_index.len() + full_dir_index.len() + 221);
    pak.extend_from_slice(&primary);
    pak.extend_from_slice(&path_hash_index);
    pak.extend_from_slice(&full_dir_index);
    // Footer (`FPakInfo`), 221 bytes.
    pak.extend_from_slice(&[0u8; 16]); // EncryptionKeyGuid (unencrypted)
    pak.push(0); // bEncryptedIndex
    pak.extend_from_slice(&MAGIC.to_le_bytes());
    pak.extend_from_slice(&VERSION.to_le_bytes());
    pak.extend_from_slice(&0i64.to_le_bytes()); // IndexOffset
    pak.extend_from_slice(&(primary.len() as i64).to_le_bytes()); // IndexSize
    pak.extend_from_slice(&index_hash);
    pak.extend_from_slice(&[0u8; 5 * 32]); // compression method names (none)
    pak
}

/// Write the fixed 144-byte `FIoStoreTocHeader`.
fn write_header(out: &mut Vec<u8>, entry_count: u32, block_count: u32, container_id: u64) {
    let start = out.len();
    out.extend_from_slice(b"-==--==--==--==-"); // magic (16)
    out.push(8); // version = ReplaceIoChunkHashWithIoHash
    out.push(0); // reserved0
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved1
    out.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes()); // toc_header_size
    out.extend_from_slice(&entry_count.to_le_bytes());
    out.extend_from_slice(&block_count.to_le_bytes());
    out.extend_from_slice(&12u32.to_le_bytes()); // compressed block entry size
    out.extend_from_slice(&0u32.to_le_bytes()); // compression method name count
    out.extend_from_slice(&32u32.to_le_bytes()); // compression method name length
    out.extend_from_slice(&(COMPRESSION_BLOCK_SIZE as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // directory index size
    out.extend_from_slice(&1u32.to_le_bytes()); // partition count
    out.extend_from_slice(&container_id.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]); // encryption key guid
    out.extend_from_slice(&0x08u32.to_le_bytes()); // container flags = Indexed
    out.extend_from_slice(&0u32.to_le_bytes()); // perfect-hash seeds count
    out.extend_from_slice(&u64::MAX.to_le_bytes()); // partition size
    out.extend_from_slice(&0u32.to_le_bytes()); // chunks-without-perfect-hash count
    out.extend_from_slice(&0u32.to_le_bytes()); // reserved7
    out.extend_from_slice(&[0u8; 40]); // reserved8 (5 x u64)
    debug_assert_eq!(out.len() - start, HEADER_SIZE);
}

/// `FIoOffsetAndLength`: 5-byte big-endian offset + 5-byte big-endian length.
fn encode_offset_length(offset: u64, length: u64) -> [u8; 10] {
    let mut b = [0u8; 10];
    b[0..5].copy_from_slice(&be40(offset));
    b[5..10].copy_from_slice(&be40(length));
    b
}

fn be40(v: u64) -> [u8; 5] {
    [
        (v >> 32) as u8,
        (v >> 24) as u8,
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
    ]
}

/// `FIoStoreTocCompressedBlockEntry`: u64 { offset:40, compressed_size:24 } +
/// u32 { uncompressed_size:24, method:8 }, little-endian.
fn encode_block(offset: u64, comp_size: u32, raw_size: u32, method: u8) -> [u8; 12] {
    let v = (offset & ((1u64 << 40) - 1)) | (((comp_size as u64) & ((1 << 24) - 1)) << 40);
    let u = (raw_size & ((1u32 << 24) - 1)) | ((method as u32) << 24);
    let mut b = [0u8; 12];
    b[0..8].copy_from_slice(&v.to_le_bytes());
    b[8..12].copy_from_slice(&u.to_le_bytes());
    b
}

/// Patch a tag's paired `.uasset` (UE5 Zen package) so its bulk-data map's
/// `SerialSize` matches a new `.ubulk` length. Required for **size-changing**
/// tag edits: UE reads the tag's byte length from here, not from the IoStore
/// chunk length, so a size change corrupts unless this is updated too.
///
/// Locates the single bulk-data entry without parsing the name map: the map
/// ends at `imported_public_export_hashes_offset` (summary field at 0x18), so
/// for one 32-byte entry `SerialSize` is 16 bytes before that. Verifies the
/// entry's signature (`DuplicateSerialOffset == -1`, map size 32, current
/// `SerialSize == old_len`) and refuses to patch anything unexpected. Does not
/// change the package's total length.
pub fn patch_uasset_serial_size(uasset: &mut [u8], old_len: u64, new_len: u64) -> Result<()> {
    if uasset.len() < 0x1c {
        return Err(IoStoreError::Package("too small for a Zen summary"));
    }
    let ipeh = i32::from_le_bytes(uasset[0x18..0x1c].try_into().unwrap());
    if ipeh < 40 || ipeh as usize > uasset.len() {
        return Err(IoStoreError::Package(
            "bad imported_public_export_hashes_offset",
        ));
    }
    let ipeh = ipeh as usize;
    let rd = |off: usize| u64::from_le_bytes(uasset[off..off + 8].try_into().unwrap());

    let map_size = rd(ipeh - 40); // int64 BulkDataMapSize
    let dup = rd(ipeh - 24); // DuplicateSerialOffset (single entry => -1)
    let serial_size_off = ipeh - 16; // SerialSize
    let cur = rd(serial_size_off);
    if map_size != 32 {
        return Err(IoStoreError::Package(
            "expected exactly one bulk-data entry",
        ));
    }
    if dup != u64::MAX {
        return Err(IoStoreError::Package("bulk-data entry signature mismatch"));
    }
    if cur != old_len {
        return Err(IoStoreError::Package(
            "current SerialSize != old .ubulk length",
        ));
    }
    uasset[serial_size_off..serial_size_off + 8].copy_from_slice(&new_len.to_le_bytes());
    Ok(())
}

/// The complete input to [`duplicate_tag_in_place_with`].
///
/// `source_uasset` is intentionally independent of `archive`: a mounted
/// lower-priority provider may supply the wrapper while `archive` is the exact
/// container that receives the new chunks. The two destination filenames are
/// explicit because an indexless mod has no directory resource from which to
/// infer them.
pub struct InPlaceTagDuplicate<'a> {
    pub source_uasset: &'a [u8],
    pub tag_bytes: &'a [u8],
    pub destination_package_path: &'a str,
    pub destination_uasset_path: &'a str,
    pub destination_ubulk_path: &'a str,
}

/// Clone a Campaign Evolved tag package into the exact existing IoStore
/// container represented by `archive`.
///
/// The operation appends all replacement/new bytes to the sibling `.ucas`,
/// then atomically replaces only the `.utoc`. Existing chunks, compression
/// blocks, and directory entries are retained. If reopen or validation fails,
/// the original `.utoc` is restored; an appended UCAS tail can therefore remain
/// only as unreachable dead space.
///
/// A perfect-hash table in the target is **dropped** rather than carried over —
/// see [`plan_toc_append`] for why an appended entry invalidates one.
pub fn duplicate_tag_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDuplicate<'_>,
) -> Result<()> {
    duplicate_tag_in_place_impl(archive, utoc_path, request, None)
}

/// Identity of a tag package to retire from the container that holds it.
///
/// Deliberately narrower than [`InPlaceTagDuplicate`]: the chunk ids are derived
/// from `package_path` exactly as duplication derived them, so there is no
/// second source of truth to disagree with the first.
pub struct InPlaceTagDeletion<'a> {
    /// The UE package path whose `.uasset` and `.ubulk` chunks are retired,
    /// e.g. `/Game/Tags/objects/copy-biped`.
    pub package_path: &'a str,
    /// How many chunks the container held before this caller ever appended to
    /// it. Both target chunks must sit at or past that index.
    ///
    /// This is the only provenance evidence that can exist: a `.utoc` records
    /// nothing about who wrote a chunk, and membership of the
    /// chunks-without-perfect-hash list proves nothing either — the cooking
    /// writer puts shipped chunks there whenever a seed bucket's search fails.
    /// `None` skips the check and trusts the caller entirely.
    pub minimum_appended_index: Option<u32>,
    /// When set, the delete only proceeds if these are exactly the directory
    /// paths currently pointing at the two chunks.
    pub expected_uasset_path: Option<&'a str>,
    pub expected_ubulk_path: Option<&'a str>,
}

/// Retire a previously duplicated tag package from the exact existing IoStore
/// container represented by `archive`.
///
/// Index-stable by construction: the chunk count, every surviving chunk's
/// index, and every existing compression block are left exactly as they were,
/// because a TOC's perfect hash maps an id to a *slot* in the chunk-id array and
/// both the slot count and the slot contents are part of that mapping. Only the
/// two retired slots, the container header chunk, the directory index and the
/// overflow list change.
///
/// The `.ucas` is appended to (the rewritten container header) and never
/// truncated; the retired payload stays behind as dead space. The `.utoc` is
/// replaced atomically and restored if reopen or validation fails.
pub fn delete_tag_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDeletion<'_>,
) -> Result<()> {
    delete_tag_in_place_impl(archive, utoc_path, request, None)
}

/// A package to move to a new path inside the container that already holds it.
///
/// Deliberately general over packages rather than shaped like a tag. A tag is
/// exactly a `.uasset` and a `.ubulk`; an arbitrary cooked package can own
/// several bulk chunks, and its identity lives in an export that may not be the
/// first one. Both are handled here, and the tag case is just the two-chunk one.
pub struct InPlacePackageRename<'a> {
    /// The package as the container currently holds it,
    /// e.g. `/Game/Vehicles/Warthog/SM_Warthog`.
    pub old_package_path: &'a str,
    /// Where it should be. May differ in folder, in leaf, or in both.
    pub new_package_path: &'a str,
    /// Replacement export-bundle bytes, when the caller has a rebuilt package —
    /// so an edited document can be renamed in one transaction rather than
    /// saved and then moved. `None` carries the container's own bytes across
    /// with only the header rewritten.
    pub replacement_export_bundle: Option<&'a [u8]>,
    /// Replacement payload for the package's bulk-data chunk, for the same
    /// reason: a tag whose body was edited can be renamed in one transaction.
    ///
    /// Refused unless the package owns exactly one `BulkData` chunk and its
    /// header describes exactly one bulk-data entry starting at offset zero —
    /// otherwise "the new length" does not say which entry grew, and a wrong
    /// `SerialSize` reads the payload off the end of the chunk.
    pub replacement_bulk_data: Option<&'a [u8]>,
    /// How many chunks the container held before this caller ever appended to
    /// it. Every chunk of the package must sit at or past that index. Same
    /// provenance role, and the same limits, as [`InPlaceTagDeletion`].
    pub minimum_appended_index: Option<u32>,
    /// Install an old→new package redirect, so a name that pointed at the old
    /// path still resolves. Off for a move that should leave nothing behind.
    pub redirect: bool,
}

/// Move a package to a new path inside the exact container that holds it.
///
/// One transaction. Composing the existing duplicate and delete would not
/// merely perform worse — it does not work: `validate_delete_result` asserts the
/// chunk count is unchanged, which is false once the duplicate half has run;
/// delete-then-duplicate is impossible because the delete retires the very
/// `.uasset` the duplicate clones; and `resolve_tag_deletion` refuses to delete
/// anything a redirect points at, which the rename's own redirect creates.
///
/// A rename is not a metadata edit. `FPackageId` is a hash of the package path,
/// and every chunk id derives from it, so moving the path moves every chunk id
/// with it. What actually happens is: the package's chunks are re-emitted under
/// the new id, the old ones are retired as tombstones, the directory index is
/// rebuilt around the new paths, and the container header's store entry moves.
///
/// The `.ucas` is appended to and never truncated; the old payload stays behind
/// as dead space. The `.utoc` is replaced atomically and restored if anything
/// fails. A perfect-hash table in the target is **dropped** — see
/// [`plan_toc_append`].
pub fn rename_package_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlacePackageRename<'_>,
) -> Result<()> {
    rename_package_in_place_impl(archive, utoc_path, request, None)
}

/// Identity of a Campaign Evolved tag to move inside the container holding it.
///
/// A tag is a package plus a naming contract: `/Game/Tags/<path>-<group>`, where
/// the group half names the wrapper's `Blam<Group>TagDataAsset` class. Nothing
/// else distinguishes it, which is why this is a wrapper and not a second
/// implementation.
pub struct InPlaceTagRename<'a> {
    /// The tag's package path as the container holds it, e.g.
    /// `/Game/Tags/objects/characters/masterchief-biped`.
    pub old_package_path: &'a str,
    /// Where it should be. Same group; the name, the folder, or both may move.
    pub new_package_path: &'a str,
    /// Replacement tag body for the `.ubulk`, when the caller holds an edited
    /// document — so renaming a dirty tag does not have to be a save followed
    /// by a move. `None` carries the stored body across untouched.
    pub tag_bytes: Option<&'a [u8]>,
    /// Same provenance role, and the same limits, as [`InPlaceTagDeletion`].
    pub minimum_appended_index: Option<u32>,
    /// Install an old→new redirect so the old path still resolves.
    pub redirect: bool,
}

/// Move a tag to a new path inside the exact container that holds it.
///
/// Everything mechanical is [`rename_package_in_place_with`]; what this adds is
/// the one contract a tag has and a package does not. The group suffix is the
/// wrapper export's class — `-biped` is a `BlamBipedTagDataAsset` — and the
/// class is a script import hashed into the game's own binary, not something
/// the package path can redefine. So renaming across groups would produce a tag
/// the browser files under one group and the engine loads as another, and the
/// mismatch would not surface until load. Refused here rather than repaired.
///
/// Renaming *within* a group is the whole operation: `-biped` to `-biped`, any
/// name, any folder.
pub fn rename_tag_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagRename<'_>,
) -> Result<()> {
    let old = crate::iostore::package::imports::split_tag_package(request.old_package_path)
        .ok_or(IoStoreError::Package(
            "the tag to rename is not at a /Game/Tags/<path>-<group> path",
        ))?;
    let new = crate::iostore::package::imports::split_tag_package(request.new_package_path)
        .ok_or(IoStoreError::Package(
            "the destination is not a /Game/Tags/<path>-<group> path",
        ))?;
    if !old.1.eq_ignore_ascii_case(new.1) {
        return Err(IoStoreError::Package(
            "a tag cannot be renamed into a different group",
        ));
    }

    rename_package_in_place_with(
        archive,
        utoc_path,
        &InPlacePackageRename {
            old_package_path: request.old_package_path,
            new_package_path: request.new_package_path,
            replacement_export_bundle: None,
            replacement_bulk_data: request.tag_bytes,
            minimum_appended_index: request.minimum_appended_index,
            redirect: request.redirect,
        },
    )
}

#[cfg(test)]
fn rename_package_in_place_with_failure_for_test(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlacePackageRename<'_>,
    failure: DuplicateFailurePoint,
) -> Result<()> {
    rename_package_in_place_impl(archive, utoc_path, request, Some(failure))
}

/// Where to abort an in-place operation, so the rollback ladder can be tested at
/// every step. Shared by duplication and deletion — they run the same ladder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DuplicateFailurePoint {
    AfterAppend,
    AfterTocWrite,
    BeforeValidation,
}

#[cfg(test)]
fn duplicate_tag_in_place_with_failure_for_test(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDuplicate<'_>,
    failure: DuplicateFailurePoint,
) -> Result<()> {
    duplicate_tag_in_place_impl(archive, utoc_path, request, Some(failure))
}

#[cfg(test)]
fn delete_tag_in_place_with_failure_for_test(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDeletion<'_>,
    failure: DuplicateFailurePoint,
) -> Result<()> {
    delete_tag_in_place_impl(archive, utoc_path, request, Some(failure))
}

struct ClonedTagPackage {
    source_header: FZenPackageHeader,
    package: Vec<u8>,
    export_payload: Vec<u8>,
    store: StoreEntry,
    package_id: FPackageId,
    uasset_id: FIoChunkId,
    ubulk_id: FIoChunkId,
    object_name: String,
}

fn clone_tag_package(
    request: &InPlaceTagDuplicate<'_>,
    object_name: &str,
) -> Result<ClonedTagPackage> {
    use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};

    if request.source_uasset.is_empty() {
        return Err(IoStoreError::Package("source .uasset is empty"));
    }
    let mut source_cursor = Cursor::new(request.source_uasset);
    let source_header = FZenPackageHeader::deserialize(
        &mut source_cursor,
        None,
        CE_TOC_VERSION,
        CE_CONTAINER_HEADER_VERSION,
        None,
    )
    .map_err(|_| IoStoreError::Package("failed to parse source .uasset"))?;
    if source_header.export_map.is_empty() {
        return Err(IoStoreError::Package("source .uasset has no export"));
    }
    if source_header.bulk_data.is_empty() {
        return Err(IoStoreError::Package("source .uasset has no bulk entry"));
    }

    let source_header_size = source_header.summary.header_size as usize;
    if source_header_size > request.source_uasset.len() {
        return Err(IoStoreError::Package(
            "source .uasset header exceeds its bytes",
        ));
    }
    for export in &source_header.export_map {
        let start = source_header_size
            .checked_add(export.cooked_serial_offset as usize)
            .ok_or(IoStoreError::Package("source export offset overflow"))?;
        let end = start
            .checked_add(export.cooked_serial_size as usize)
            .ok_or(IoStoreError::Package("source export size overflow"))?;
        if end > request.source_uasset.len() {
            return Err(IoStoreError::Package("source export exceeds its .uasset"));
        }
    }

    let export_payload = request.source_uasset[source_header_size..].to_vec();
    if request.tag_bytes.len() > i64::MAX as usize {
        return Err(IoStoreError::Package(
            "tag body is too large for SerialSize",
        ));
    }

    let mut cloned_header = source_header.clone();
    cloned_header.summary.name = cloned_header
        .name_map
        .store(request.destination_package_path);
    cloned_header.export_map[0].object_name = cloned_header.name_map.store(object_name);
    cloned_header.export_map[0].public_export_hash = container_id_from_name(object_name);
    cloned_header.bulk_data[0].serial_size = request.tag_bytes.len() as i64;

    let mut store = StoreEntry::default();
    let mut serialized_header = Cursor::new(Vec::new());
    cloned_header
        .serialize(
            &mut serialized_header,
            &mut store,
            crate::iostore::compat::CE_CONTAINER_HEADER_VERSION,
        )
        .map_err(|_| IoStoreError::Package("failed to serialize cloned .uasset"))?;
    let mut package = serialized_header.into_inner();
    package.extend_from_slice(&export_payload);

    let package_id = FPackageId::from_name(request.destination_package_path);
    let uasset_id = make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
    let ubulk_id = make_chunk_id(package_id.0, 0, CHUNK_TYPE_BULK_DATA);

    Ok(ClonedTagPackage {
        source_header,
        package,
        export_payload,
        store,
        package_id,
        uasset_id,
        ubulk_id,
        object_name: object_name.to_string(),
    })
}

fn validate_destination_path_forms(request: &InPlaceTagDuplicate<'_>) -> Result<String> {
    let package_path = request.destination_package_path;
    if !valid_package_path(package_path) {
        return Err(IoStoreError::Package("invalid destination package path"));
    }
    let uasset_path = request.destination_uasset_path;
    let ubulk_path = request.destination_ubulk_path;
    if !valid_container_path(uasset_path, ".uasset") || !valid_container_path(ubulk_path, ".ubulk")
    {
        return Err(IoStoreError::Package("invalid destination asset path"));
    }
    let uasset_stem = uasset_path
        .strip_suffix(".uasset")
        .ok_or(IoStoreError::Package("destination asset is not a .uasset"))?;
    let ubulk_stem = ubulk_path
        .strip_suffix(".ubulk")
        .ok_or(IoStoreError::Package("destination bulk is not a .ubulk"))?;
    if uasset_stem != ubulk_stem {
        return Err(IoStoreError::Package(
            "destination .uasset and .ubulk stems do not match",
        ));
    }
    let package_leaf = package_path
        .rsplit('/')
        .next()
        .ok_or(IoStoreError::Package("destination package has no leaf"))?;
    let asset_leaf = uasset_stem
        .rsplit('/')
        .next()
        .ok_or(IoStoreError::Package("destination asset has no leaf"))?;
    if package_leaf != asset_leaf {
        return Err(IoStoreError::Package(
            "destination package and asset leaves do not match",
        ));
    }
    let package_relative = package_path
        .strip_prefix("/Game/")
        .ok_or(IoStoreError::Package(
            "destination package is not a /Game path",
        ))?;
    let relative_matches = uasset_stem == package_relative
        || uasset_stem
            .strip_suffix(&format!("/{package_relative}"))
            .is_some();
    if !relative_matches {
        return Err(IoStoreError::Package(
            "destination asset path does not match package path",
        ));
    }
    Ok(package_leaf.to_string())
}

fn valid_package_path(path: &str) -> bool {
    path.starts_with("/Game/")
        && path.len() > "/Game/".len()
        && !path.contains('\\')
        && !path.contains("//")
        && !path.ends_with('/')
        && path
            .strip_prefix('/')
            .unwrap_or(path)
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && !path.ends_with(".uasset")
        && !path.ends_with(".ubulk")
}

fn valid_container_path(path: &str, extension: &str) -> bool {
    path.ends_with(extension)
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.contains("//")
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

const TOC_MAGIC_BYTES: &[u8; 16] = b"-==--==--==--==-";
const FLAG_TOC_ENCRYPTED: u32 = 0x02;
const FLAG_TOC_SIGNED: u32 = 0x04;
const TOC_HEADER_SIZE: usize = 144;
const TOC_META_SIZE: usize = 24;
const MAX_PACKED_BLOCK_VALUE: u64 = (1 << 24) - 1;
const MAX_PACKED_OFFSET: u64 = (1 << 40) - 1;

struct ParsedToc {
    original: Vec<u8>,
    entry_count: u32,
    block_count: u32,
    block_size: u64,
    directory_index_size: usize,
    container_id: u64,
    partition_size: u64,
    chunk_ids: Vec<FIoChunkId>,
    offset_lengths: Vec<[u8; 10]>,
    perfect_hash_seeds: Vec<u8>,
    chunks_without_perfect_hash: Vec<u8>,
    blocks: Vec<[u8; 12]>,
    compression_methods: Vec<u8>,
    directory_index: Vec<u8>,
    metas: Vec<[u8; TOC_META_SIZE]>,
    trailing: Vec<u8>,
}

struct TocAppendItem {
    chunk_index: u32,
    id: FIoChunkId,
    bytes: Vec<u8>,
    path: Option<String>,
}

struct NewTocChunk {
    id: FIoChunkId,
    bytes: Vec<u8>,
    path: Option<String>,
}

struct ExistingTocReplacement {
    chunk_index: u32,
    bytes: Vec<u8>,
}

/// A chunk slot retired in place: zero-length payload, retired id, and — this is
/// the point — the index itself stays occupied.
///
/// Removing the slot outright is not an option. A TOC's perfect hash resolves
/// `slot = Hash(seed, id) % entry_count` and then verifies `ChunkIds[slot] == id`,
/// so both the entry count and every chunk's position are part of the mapping.
/// Compacting the arrays would move unrelated chunks — the game's own — off the
/// slots their seeds point at.
struct TocTombstone {
    chunk_index: u32,
}

struct TocAppendPlan {
    items: Vec<TocAppendItem>,
    new_chunk_indices: Vec<u32>,
    retired_chunk_indices: Vec<u32>,
    /// Whether this plan discarded a perfect-hash table the source TOC had.
    dropped_perfect_hash: bool,
    new_toc: Vec<u8>,
}

/// `EIoChunkType::Invalid`. Nothing constructs a chunk id with this type —
/// [`make_chunk_id`] is only ever called with export-bundle, bulk-data, or
/// container-header — so a retired id can never be resolved, and can never
/// collide with a live chunk or with a later re-creation of the same package.
const RETIRED_CHUNK_TYPE: u8 = 0;

/// Retire a chunk id in place, stashing the original type in the pad byte so a
/// retired slot still says what it used to be.
fn retire_chunk_id(id: FIoChunkId) -> FIoChunkId {
    let mut bytes = id.0;
    bytes[10] = bytes[11];
    bytes[11] = RETIRED_CHUNK_TYPE;
    FIoChunkId(bytes)
}

/// Read the chunks-without-perfect-hash section as the index list it is.
fn toc_overflow_indices(bytes: &[u8]) -> Result<Vec<i32>> {
    if bytes.len() % 4 != 0 {
        return Err(IoStoreError::Truncated(
            "chunks-without-perfect-hash section is not aligned",
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|raw| i32::from_le_bytes(raw.try_into().unwrap()))
        .collect())
}

fn parse_toc(bytes: &[u8]) -> Result<ParsedToc> {
    if bytes.len() < TOC_HEADER_SIZE || &bytes[..16] != TOC_MAGIC_BYTES {
        return Err(IoStoreError::BadMagic);
    }
    if bytes[16] != 8 {
        return Err(IoStoreError::UnsupportedVersion(bytes[16]));
    }
    if toc_u32(bytes, 20)? as usize != TOC_HEADER_SIZE {
        return Err(IoStoreError::Truncated("unsupported TOC header size"));
    }

    let entry_count = toc_u32(bytes, 24)?;
    let block_count = toc_u32(bytes, 28)?;
    if toc_u32(bytes, 32)? != 12 {
        return Err(IoStoreError::Truncated(
            "unsupported compressed-block entry size",
        ));
    }
    let compression_method_count = toc_u32(bytes, 36)? as usize;
    let compression_method_len = toc_u32(bytes, 40)? as usize;
    let block_size = toc_u32(bytes, 44)? as u64;
    if block_size == 0 || block_size > MAX_PACKED_BLOCK_VALUE {
        return Err(IoStoreError::Truncated(
            "unsupported compression block size",
        ));
    }
    let directory_index_size = toc_u32(bytes, 48)? as usize;
    let partition_count = toc_u32(bytes, 52)?;
    if partition_count != 1 {
        return Err(IoStoreError::MultiPartition(partition_count));
    }
    let container_id = toc_u64(bytes, 56)?;
    let flags = toc_u32(bytes, 80)?;
    if flags & FLAG_TOC_ENCRYPTED != 0 {
        return Err(IoStoreError::Encrypted);
    }
    if flags & FLAG_TOC_SIGNED != 0 {
        return Err(IoStoreError::Package(
            "signed TOCs are unsupported for in-place duplication",
        ));
    }
    let seed_count = toc_u32(bytes, 84)? as usize;
    let partition_size = toc_u64(bytes, 88)?;
    let without_hash_count = toc_u32(bytes, 96)? as usize;

    let chunkid_off = TOC_HEADER_SIZE;
    let offlen_off = checked_toc_end(chunkid_off, entry_count as usize, 12, bytes.len())?;
    let seeds_off = checked_toc_end(offlen_off, entry_count as usize, 10, bytes.len())?;
    let without_hash_off = checked_toc_end(seeds_off, seed_count, 4, bytes.len())?;
    let block_off = checked_toc_end(without_hash_off, without_hash_count, 4, bytes.len())?;
    let methods_off = checked_toc_end(block_off, block_count as usize, 12, bytes.len())?;
    let directory_off = checked_toc_end(
        methods_off,
        compression_method_count,
        compression_method_len,
        bytes.len(),
    )?;
    let directory_end = directory_off
        .checked_add(directory_index_size)
        .ok_or(IoStoreError::Truncated("directory index offset overflow"))?;
    if directory_end > bytes.len() {
        return Err(IoStoreError::Truncated("directory index past end of TOC"));
    }
    let meta_end = directory_end
        .checked_add(
            (entry_count as usize)
                .checked_mul(TOC_META_SIZE)
                .ok_or(IoStoreError::Truncated("TOC metadata size overflow"))?,
        )
        .ok_or(IoStoreError::Truncated("TOC metadata offset overflow"))?;
    if meta_end > bytes.len() {
        return Err(IoStoreError::Truncated("TOC metadata past end of TOC"));
    }

    let chunk_ids = bytes[chunkid_off..offlen_off]
        .chunks_exact(12)
        .map(|raw| {
            let mut id = [0u8; 12];
            id.copy_from_slice(raw);
            FIoChunkId(id)
        })
        .collect();
    let offset_lengths = bytes[offlen_off..seeds_off]
        .chunks_exact(10)
        .map(|raw| {
            let mut value = [0u8; 10];
            value.copy_from_slice(raw);
            value
        })
        .collect();
    let blocks = bytes[block_off..methods_off]
        .chunks_exact(12)
        .map(|raw| {
            let mut value = [0u8; 12];
            value.copy_from_slice(raw);
            value
        })
        .collect();
    let metas = bytes[directory_end..meta_end]
        .chunks_exact(TOC_META_SIZE)
        .map(|raw| {
            let mut value = [0u8; TOC_META_SIZE];
            value.copy_from_slice(raw);
            value
        })
        .collect();

    Ok(ParsedToc {
        original: bytes.to_vec(),
        entry_count,
        block_count,
        block_size,
        directory_index_size,
        container_id,
        partition_size,
        chunk_ids,
        offset_lengths,
        perfect_hash_seeds: bytes[seeds_off..without_hash_off].to_vec(),
        chunks_without_perfect_hash: bytes[without_hash_off..block_off].to_vec(),
        blocks,
        compression_methods: bytes[methods_off..directory_off].to_vec(),
        directory_index: bytes[directory_off..directory_end].to_vec(),
        metas,
        trailing: bytes[meta_end..].to_vec(),
    })
}

fn toc_u32(bytes: &[u8], offset: usize) -> Result<u32> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(IoStoreError::Truncated("TOC header field"))?;
    Ok(u32::from_le_bytes(raw.try_into().unwrap()))
}

fn toc_u64(bytes: &[u8], offset: usize) -> Result<u64> {
    let raw = bytes
        .get(offset..offset + 8)
        .ok_or(IoStoreError::Truncated("TOC header field"))?;
    Ok(u64::from_le_bytes(raw.try_into().unwrap()))
}

fn checked_toc_end(start: usize, count: usize, item_size: usize, total: usize) -> Result<usize> {
    let size = count
        .checked_mul(item_size)
        .ok_or(IoStoreError::Truncated("TOC section size overflow"))?;
    let end = start
        .checked_add(size)
        .ok_or(IoStoreError::Truncated("TOC section offset overflow"))?;
    if end > total {
        return Err(IoStoreError::Truncated("TOC section past end of TOC"));
    }
    Ok(end)
}

/// Rebuild a TOC with chunks appended, replaced, and/or retired.
///
/// `entries` is the directory-index entry set the result should carry — the
/// caller passes the archive's own entries for an append, and the surviving
/// subset when retiring slots.
fn plan_toc_append(
    toc: &ParsedToc,
    old_ucas_len: u64,
    additions: Vec<NewTocChunk>,
    replacements: Vec<ExistingTocReplacement>,
    tombstones: &[TocTombstone],
    entries: &[Entry],
) -> Result<TocAppendPlan> {
    let new_entry_count = toc
        .entry_count
        .checked_add(additions.len() as u32)
        .ok_or(IoStoreError::Truncated("TOC chunk count overflow"))?;
    let mut chunk_ids = toc.chunk_ids.clone();
    let mut offset_lengths = toc.offset_lengths.clone();
    let mut metas = toc.metas.clone();
    let mut blocks = toc.blocks.clone();
    let mut overflow = toc_overflow_indices(&toc.chunks_without_perfect_hash)?;
    let mut items = Vec::with_capacity(additions.len() + replacements.len());
    let mut new_chunk_indices = Vec::with_capacity(additions.len());

    for (offset, addition) in additions.into_iter().enumerate() {
        let chunk_index = toc.entry_count + offset as u32;
        if addition.bytes.len() as u64 > MAX_PACKED_OFFSET {
            return Err(IoStoreError::Package("new chunk is too large for a TOC"));
        }
        chunk_ids.push(addition.id);
        offset_lengths.push([0; 10]);
        metas.push([0; TOC_META_SIZE]);
        if chunk_index > i32::MAX as u32 {
            return Err(IoStoreError::Package(
                "new chunk index exceeds TOC overflow format",
            ));
        }
        overflow.push(chunk_index as i32);
        new_chunk_indices.push(chunk_index);
        items.push(TocAppendItem {
            chunk_index,
            id: addition.id,
            bytes: addition.bytes,
            path: addition.path,
        });
    }

    for replacement in replacements {
        if replacement.chunk_index >= toc.entry_count {
            return Err(IoStoreError::Truncated(
                "replacement chunk index out of range",
            ));
        }
        if replacement.bytes.len() as u64 > MAX_PACKED_OFFSET {
            return Err(IoStoreError::Package(
                "replacement chunk is too large for a TOC",
            ));
        }
        if items
            .iter()
            .any(|item| item.chunk_index == replacement.chunk_index)
        {
            return Err(IoStoreError::Package("the same chunk was appended twice"));
        }
        items.push(TocAppendItem {
            chunk_index: replacement.chunk_index,
            id: toc.chunk_ids[replacement.chunk_index as usize],
            bytes: replacement.bytes,
            path: None,
        });
    }

    let mut retired_chunk_indices = Vec::with_capacity(tombstones.len());
    for tombstone in tombstones {
        let index = tombstone.chunk_index as usize;
        if tombstone.chunk_index >= toc.entry_count {
            return Err(IoStoreError::Truncated("retired chunk index out of range"));
        }
        if items
            .iter()
            .any(|item| item.chunk_index == tombstone.chunk_index)
        {
            return Err(IoStoreError::Package(
                "the same chunk was both rewritten and retired",
            ));
        }
        if retired_chunk_indices.contains(&tombstone.chunk_index) {
            return Err(IoStoreError::Package("the same chunk was retired twice"));
        }
        // The slot keeps its index and loses everything else: no payload, no
        // content hash, and an id no lookup can construct.
        chunk_ids[index] = retire_chunk_id(chunk_ids[index]);
        offset_lengths[index] = [0; 10];
        metas[index] = [0; TOC_META_SIZE];
        retired_chunk_indices.push(tombstone.chunk_index);
    }
    overflow.retain(|index| !retired_chunk_indices.contains(&(*index as u32)));

    let mut physical_offset = old_ucas_len;
    let mut appended_blocks = Vec::new();
    for item in &items {
        let start_block = toc
            .block_count
            .checked_add(appended_blocks.len() as u32)
            .ok_or(IoStoreError::Truncated("TOC block count overflow"))?;
        let logical_offset = (start_block as u64)
            .checked_mul(toc.block_size)
            .ok_or(IoStoreError::Truncated("TOC logical offset overflow"))?;
        if logical_offset > MAX_PACKED_OFFSET {
            return Err(IoStoreError::Package("new chunk offset exceeds TOC format"));
        }
        let mut block_offset = 0usize;
        while block_offset < item.bytes.len() {
            let block_end = (block_offset + toc.block_size as usize).min(item.bytes.len());
            let block_len = block_end - block_offset;
            if block_len as u64 > MAX_PACKED_BLOCK_VALUE {
                return Err(IoStoreError::Package("new block is too large for a TOC"));
            }
            let physical_end = physical_offset
                .checked_add(block_len as u64)
                .ok_or(IoStoreError::Truncated("UCAS length overflow"))?;
            if physical_offset > MAX_PACKED_OFFSET || physical_end > MAX_PACKED_OFFSET {
                return Err(IoStoreError::Package("UCAS offset exceeds TOC format"));
            }
            appended_blocks.push(encode_block(
                physical_offset,
                block_len as u32,
                block_len as u32,
                0,
            ));
            physical_offset = physical_end;
            block_offset = block_end;
        }
        offset_lengths[item.chunk_index as usize] =
            encode_offset_length(logical_offset, item.bytes.len() as u64);
        let hash = blake3::hash(&item.bytes);
        let mut meta = [0u8; TOC_META_SIZE];
        meta[..20].copy_from_slice(&hash.as_bytes()[..20]);
        metas[item.chunk_index as usize] = meta;
    }
    blocks.extend(appended_blocks);
    let block_count = u32::try_from(blocks.len())
        .map_err(|_| IoStoreError::Truncated("TOC block count overflow"))?;

    // A perfect-hash table cannot survive an in-place edit. It maps a chunk id
    // to a *slot in the chunk-id array* — `slot = Hash(seed, id) % entry_count`,
    // verified against `ChunkIds[slot]` — so appending entries changes the
    // modulo base for every chunk in the container, and retiring an id makes its
    // own slot stop verifying. There is no seed generator here to rebuild the
    // table with, so drop it: with no seeds the runtime indexes every chunk id
    // directly, which is exactly how the override containers this module has
    // always written are laid out. The overflow list only catches perfect-hash
    // misses, so it goes with the table.
    let drops_perfect_hash = !toc.perfect_hash_seeds.is_empty();
    let perfect_hash_seeds: &[u8] = if drops_perfect_hash {
        &[]
    } else {
        toc.perfect_hash_seeds.as_slice()
    };
    let overflow_bytes: Vec<u8> = if drops_perfect_hash {
        Vec::new()
    } else {
        overflow.iter().flat_map(|index| index.to_le_bytes()).collect()
    };
    let seed_count = u32::try_from(perfect_hash_seeds.len() / 4)
        .map_err(|_| IoStoreError::Truncated("TOC seed count overflow"))?;
    let overflow_count = u32::try_from(overflow_bytes.len() / 4)
        .map_err(|_| IoStoreError::Truncated("TOC overflow count overflow"))?;

    let directory_index = if toc.directory_index_size != 0 {
        let mut entries = entries.to_vec();
        for item in &items {
            if let Some(path) = &item.path {
                entries.push(Entry {
                    path: path.clone(),
                    chunk_index: item.chunk_index,
                });
            }
        }
        serialize_directory_index(toc.directory_index.as_slice(), &entries)?
    } else {
        Vec::new()
    };
    let directory_size = u32::try_from(directory_index.len())
        .map_err(|_| IoStoreError::Package("directory index is too large"))?;

    if toc.partition_size != u64::MAX && physical_offset > toc.partition_size {
        return Err(IoStoreError::Package(
            "appended UCAS data exceeds the partition size",
        ));
    }

    let mut header = toc.original[..TOC_HEADER_SIZE].to_vec();
    header[24..28].copy_from_slice(&new_entry_count.to_le_bytes());
    header[28..32].copy_from_slice(&block_count.to_le_bytes());
    header[48..52].copy_from_slice(&directory_size.to_le_bytes());
    header[84..88].copy_from_slice(&seed_count.to_le_bytes());
    header[96..100].copy_from_slice(&overflow_count.to_le_bytes());

    let mut new_toc = Vec::with_capacity(
        TOC_HEADER_SIZE
            + chunk_ids.len() * 12
            + offset_lengths.len() * 10
            + perfect_hash_seeds.len()
            + overflow_bytes.len()
            + blocks.len() * 12
            + toc.compression_methods.len()
            + directory_index.len()
            + metas.len() * TOC_META_SIZE
            + toc.trailing.len(),
    );
    new_toc.extend_from_slice(&header);
    for id in &chunk_ids {
        new_toc.extend_from_slice(id.bytes());
    }
    for offset_length in &offset_lengths {
        new_toc.extend_from_slice(offset_length);
    }
    new_toc.extend_from_slice(perfect_hash_seeds);
    new_toc.extend_from_slice(&overflow_bytes);
    for block in &blocks {
        new_toc.extend_from_slice(block);
    }
    new_toc.extend_from_slice(&toc.compression_methods);
    new_toc.extend_from_slice(&directory_index);
    for meta in &metas {
        new_toc.extend_from_slice(meta);
    }
    new_toc.extend_from_slice(&toc.trailing);

    Ok(TocAppendPlan {
        items,
        new_chunk_indices,
        retired_chunk_indices,
        dropped_perfect_hash: drops_perfect_hash,
        new_toc,
    })
}

struct DirectoryNode {
    name: Option<String>,
    children: BTreeMap<String, usize>,
    files: BTreeMap<String, u32>,
}

fn serialize_directory_index(original: &[u8], entries: &[Entry]) -> Result<Vec<u8>> {
    if original.is_empty() {
        return Err(IoStoreError::Truncated("missing source directory index"));
    }
    let mut cursor = 0usize;
    let mount = read_directory_fstring(original, &mut cursor)?;
    let mount_encoded = original[..cursor].to_vec();
    let mount_prefix = clean_mount_for_writer(&mount);
    let mut nodes = vec![DirectoryNode {
        name: None,
        children: BTreeMap::new(),
        files: BTreeMap::new(),
    }];

    for entry in entries {
        let relative = if mount_prefix.is_empty() {
            entry.path.as_str()
        } else {
            entry
                .path
                .strip_prefix(&mount_prefix)
                .and_then(|rest| rest.strip_prefix('/'))
                .ok_or(IoStoreError::Package(
                    "destination path does not match directory mount point",
                ))?
        };
        let parts: Vec<&str> = relative.split('/').collect();
        if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
            return Err(IoStoreError::Package(
                "directory index contains an empty path",
            ));
        }
        let mut node_index = 0usize;
        for (part_index, part) in parts.iter().enumerate() {
            let last = part_index + 1 == parts.len();
            if last {
                if nodes[node_index].children.contains_key(*part) {
                    return Err(IoStoreError::Package(
                        "directory index has a file/directory name collision",
                    ));
                }
                if nodes[node_index]
                    .files
                    .insert((*part).to_string(), entry.chunk_index)
                    .is_some()
                {
                    return Err(IoStoreError::Package(
                        "directory index contains a duplicate path",
                    ));
                }
            } else {
                if nodes[node_index].files.contains_key(*part) {
                    return Err(IoStoreError::Package(
                        "directory index has a file/directory name collision",
                    ));
                }
                let next = if let Some(next) = nodes[node_index].children.get(*part) {
                    *next
                } else {
                    let next = nodes.len();
                    nodes.push(DirectoryNode {
                        name: Some((*part).to_string()),
                        children: BTreeMap::new(),
                        files: BTreeMap::new(),
                    });
                    nodes[node_index].children.insert((*part).to_string(), next);
                    next
                };
                node_index = next;
            }
        }
    }

    let mut string_set = BTreeSet::new();
    for node in &nodes {
        if let Some(name) = &node.name {
            string_set.insert(name.clone());
        }
        string_set.extend(node.files.keys().cloned());
    }
    let strings: Vec<String> = string_set.into_iter().collect();
    let string_indices: BTreeMap<String, u32> = strings
        .iter()
        .enumerate()
        .map(|(index, value)| (value.clone(), index as u32))
        .collect();

    const INVALID: u32 = u32::MAX;
    let mut directories = vec![[INVALID; 4]; nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Some(name) = &node.name {
            directories[index][0] = *string_indices
                .get(name)
                .ok_or(IoStoreError::Truncated("directory name string missing"))?;
        }
        let children: Vec<usize> = node.children.values().copied().collect();
        if let Some(first) = children.first() {
            directories[index][1] = *first as u32;
            for pair in children.windows(2) {
                directories[pair[0]][2] = pair[1] as u32;
            }
        }
    }

    let mut files: Vec<[u32; 3]> = Vec::with_capacity(entries.len());
    for (directory_index, node) in nodes.iter().enumerate() {
        let mut first_file = INVALID;
        let mut previous_file = None;
        for (name, chunk_index) in &node.files {
            let current = u32::try_from(files.len())
                .map_err(|_| IoStoreError::Package("directory file count exceeds TOC format"))?;
            let name_index = *string_indices
                .get(name)
                .ok_or(IoStoreError::Truncated("file name string missing"))?;
            files.push([name_index, INVALID, *chunk_index]);
            if let Some(previous) = previous_file {
                files[previous as usize][1] = current;
            } else {
                first_file = current;
            }
            previous_file = Some(current);
        }
        directories[directory_index][3] = first_file;
    }

    let directory_count = u32::try_from(directories.len())
        .map_err(|_| IoStoreError::Package("directory count exceeds TOC format"))?;
    let file_count = u32::try_from(files.len())
        .map_err(|_| IoStoreError::Package("directory file count exceeds TOC format"))?;
    let string_count = u32::try_from(strings.len())
        .map_err(|_| IoStoreError::Package("directory string count exceeds TOC format"))?;
    let mut out = Vec::new();
    out.extend_from_slice(&mount_encoded);
    out.extend_from_slice(&directory_count.to_le_bytes());
    for directory in directories {
        for field in directory {
            out.extend_from_slice(&field.to_le_bytes());
        }
    }
    out.extend_from_slice(&file_count.to_le_bytes());
    for file in files {
        for field in file {
            out.extend_from_slice(&field.to_le_bytes());
        }
    }
    out.extend_from_slice(&string_count.to_le_bytes());
    for string in strings {
        out.extend_from_slice(&encode_directory_fstring(&string)?);
    }
    Ok(out)
}

fn read_directory_fstring(bytes: &[u8], cursor: &mut usize) -> Result<String> {
    let length_bytes = bytes
        .get(*cursor..*cursor + 4)
        .ok_or(IoStoreError::Truncated("directory FString length"))?;
    let length = i32::from_le_bytes(length_bytes.try_into().unwrap());
    *cursor += 4;
    if length == 0 {
        return Ok(String::new());
    }
    if length > 0 {
        let count = length as usize;
        let raw = bytes
            .get(*cursor..*cursor + count)
            .ok_or(IoStoreError::Truncated("directory FString bytes"))?;
        *cursor += count;
        return Ok(String::from_utf8_lossy(
            raw.split(|byte| *byte == 0).next().unwrap_or_default(),
        )
        .into_owned());
    }
    let count = length
        .checked_neg()
        .ok_or(IoStoreError::Truncated("directory FString length overflow"))?
        as usize;
    let byte_count = count
        .checked_mul(2)
        .ok_or(IoStoreError::Truncated("directory FString size overflow"))?;
    let raw = bytes
        .get(*cursor..*cursor + byte_count)
        .ok_or(IoStoreError::Truncated("directory UTF-16 FString bytes"))?;
    *cursor += byte_count;
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    Ok(String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string())
}

fn encode_directory_fstring(value: &str) -> Result<Vec<u8>> {
    if value.is_ascii() {
        let bytes = value.as_bytes();
        let length = i32::try_from(bytes.len() + 1)
            .map_err(|_| IoStoreError::Package("directory string is too long"))?;
        let mut out = Vec::with_capacity(4 + bytes.len() + 1);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(bytes);
        out.push(0);
        return Ok(out);
    }
    let units: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let length = i32::try_from(units.len())
        .map_err(|_| IoStoreError::Package("directory string is too long"))?;
    let mut out = Vec::with_capacity(4 + units.len() * 2);
    out.extend_from_slice(&(-length).to_le_bytes());
    for unit in units {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(out)
}

fn clean_mount_for_writer(mount: &str) -> String {
    let mut value = mount;
    loop {
        if let Some(rest) = value.strip_prefix("../") {
            value = rest;
        } else if let Some(rest) = value.strip_prefix('/') {
            value = rest;
        } else {
            break;
        }
    }
    value.trim_end_matches('/').to_string()
}

fn duplicate_tag_in_place_impl(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDuplicate<'_>,
    failure: Option<DuplicateFailurePoint>,
) -> Result<()> {
    let object_name = validate_destination_path_forms(request)?;
    let original_toc_bytes = std::fs::read(utoc_path)?;
    if !archive.matches_utoc_path(utoc_path) || archive.toc_bytes() != original_toc_bytes {
        return Err(IoStoreError::Package(
            "archive handle is stale or targets another container",
        ));
    }
    let toc = parse_toc(&original_toc_bytes)?;
    if archive.chunk_count() != toc.entry_count {
        return Err(IoStoreError::Package("archive handle chunk count is stale"));
    }
    for (index, expected_id) in toc.chunk_ids.iter().enumerate() {
        if archive.chunk_id(index as u32)? != *expected_id {
            return Err(IoStoreError::Package("archive handle chunk ids are stale"));
        }
    }
    let mut raw_ids = BTreeSet::new();
    for id in &toc.chunk_ids {
        if !raw_ids.insert(id.0) {
            return Err(IoStoreError::Package(
                "target TOC contains a duplicate chunk id",
            ));
        }
    }

    let cloned = clone_tag_package(request, &object_name)?;
    if toc
        .chunk_ids
        .iter()
        .any(|id| *id == cloned.uasset_id || *id == cloned.ubulk_id)
    {
        return Err(IoStoreError::Package("destination chunk id already exists"));
    }
    if archive.entries().iter().any(|entry| {
        entry.path == request.destination_uasset_path
            || entry.path == request.destination_ubulk_path
    }) {
        return Err(IoStoreError::Package("destination path already exists"));
    }

    let header_indices: Vec<u32> = toc
        .chunk_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            (id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER).then_some(index as u32)
        })
        .collect();
    if header_indices.len() > 1 {
        return Err(IoStoreError::Package(
            "target has multiple ContainerHeader chunks",
        ));
    }
    let new_header_id = make_chunk_id(toc.container_id, 0, CHUNK_TYPE_CONTAINER_HEADER);
    let (header_id, header_bytes, header_replacement) = if let Some(&header_index) =
        header_indices.first()
    {
        let header_id = toc.chunk_ids[header_index as usize];
        if header_id.package_id() != toc.container_id.to_le_bytes() {
            return Err(IoStoreError::Package(
                "ContainerHeader id does not match container id",
            ));
        }
        let old_header_bytes = archive.read_chunk(header_index)?;
        let mut header = FIoContainerHeader::deserialize(&mut Cursor::new(old_header_bytes), None)
            .map_err(|_| IoStoreError::Package("container package-store header did not parse"))?;
        if header.container_id.0 != toc.container_id {
            return Err(IoStoreError::Package(
                "ContainerHeader payload id does not match TOC",
            ));
        }
        crate::iostore::compat::check_writable_container_header_version(header.version)
            .map_err(|_| IoStoreError::Package("unsupported container package-store header"))?;
        if header.get_store_entry(cloned.package_id).is_some() {
            return Err(IoStoreError::Package(
                "destination package is already in the package store",
            ));
        }
        header.add_package(cloned.package_id, cloned.store.clone());
        let bytes = serialize_aligned_container_header(&header)?;
        (
            header_id,
            bytes,
            Some(ExistingTocReplacement {
                chunk_index: header_index,
                bytes: Vec::new(),
            }),
        )
    } else {
        if toc.chunk_ids.iter().any(|id| *id == new_header_id) {
            return Err(IoStoreError::Package(
                "ContainerHeader chunk id already exists",
            ));
        }
        crate::iostore::compat::check_writable_container_header_version(
            crate::iostore::compat::CE_CONTAINER_HEADER_VERSION,
        )
        .map_err(|_| IoStoreError::Package("unsupported container package-store header"))?;
        let mut header = FIoContainerHeader::new(
            crate::iostore::compat::CE_CONTAINER_HEADER_VERSION,
            FIoContainerId(toc.container_id),
        );
        header.add_package(cloned.package_id, cloned.store.clone());
        let bytes = serialize_aligned_container_header(&header)?;
        (new_header_id, bytes, None)
    };

    let mut additions = vec![
        NewTocChunk {
            id: cloned.uasset_id,
            bytes: cloned.package.clone(),
            path: Some(request.destination_uasset_path.to_string()),
        },
        NewTocChunk {
            id: cloned.ubulk_id,
            bytes: request.tag_bytes.to_vec(),
            path: Some(request.destination_ubulk_path.to_string()),
        },
    ];
    let mut replacements = Vec::new();
    if let Some(mut replacement) = header_replacement {
        replacement.bytes = header_bytes.clone();
        replacements.push(replacement);
    } else {
        additions.push(NewTocChunk {
            id: header_id,
            bytes: header_bytes.clone(),
            path: None,
        });
    }

    if additions
        .iter()
        .any(|addition| raw_ids.contains(&addition.id.0))
    {
        return Err(IoStoreError::Package("destination chunk id already exists"));
    }
    let ucas_path = utoc_path.with_extension("ucas");
    let old_ucas_len = std::fs::metadata(&ucas_path)?.len();
    let plan = plan_toc_append(
        &toc,
        old_ucas_len,
        additions,
        replacements,
        &[],
        archive.entries(),
    )?;

    append_ucas_items(&ucas_path, &plan.items)?;
    if failure == Some(DuplicateFailurePoint::AfterAppend) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-append failure"),
        );
    }
    if let Err(error) = atomic_replace_file(utoc_path, &plan.new_toc) {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    if failure == Some(DuplicateFailurePoint::AfterTocWrite) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-TOC-write failure"),
        );
    }

    if failure == Some(DuplicateFailurePoint::BeforeValidation) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected validation failure"),
        );
    }
    let validation = validate_duplicate_result(
        utoc_path,
        &toc,
        archive.entries(),
        request,
        &cloned,
        header_id,
        &plan,
    );
    if let Err(error) = validation {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    Ok(())
}

struct ResolvedTagDeletion {
    package_id: FPackageId,
    uasset_id: FIoChunkId,
    ubulk_id: FIoChunkId,
    uasset_index: u32,
    ubulk_index: u32,
    removed_paths: Vec<String>,
    surviving_entries: Vec<Entry>,
    header_index: u32,
    header_id: FIoChunkId,
    header_bytes: Vec<u8>,
    surviving_package_ids: BTreeSet<FPackageId>,
}

/// Establish, from the TOC and the container header alone, that exactly two
/// slots belong to `package_path` and that nothing else still refers to them.
fn resolve_tag_deletion(
    archive: &IoStoreArchive,
    toc: &ParsedToc,
    request: &InPlaceTagDeletion<'_>,
) -> Result<ResolvedTagDeletion> {
    use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};

    if request.package_path.is_empty() {
        return Err(IoStoreError::Package("deletion package path is empty"));
    }
    let package_id = FPackageId::from_name(request.package_path);
    let uasset_id = make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
    let ubulk_id = make_chunk_id(package_id.0, 0, CHUNK_TYPE_BULK_DATA);
    let index_of = |wanted: &FIoChunkId| {
        toc.chunk_ids
            .iter()
            .position(|id| id == wanted)
            .map(|index| index as u32)
    };
    let (Some(uasset_index), Some(ubulk_index)) = (index_of(&uasset_id), index_of(&ubulk_id)) else {
        return Err(IoStoreError::Package(
            "the package to delete is not in this container",
        ));
    };

    if let Some(minimum) = request.minimum_appended_index
        && (uasset_index < minimum || ubulk_index < minimum)
    {
        return Err(IoStoreError::Package(
            "the package to delete predates this container's appended chunks",
        ));
    }

    // Prove the slot really holds the package the caller named, rather than
    // trusting a hash collision or a stale record.
    let package_bytes = archive.read_chunk(uasset_index)?;
    let header = FZenPackageHeader::deserialize(
        &mut Cursor::new(package_bytes),
        None,
        CE_TOC_VERSION,
        CE_CONTAINER_HEADER_VERSION,
        None,
    )
    .map_err(|_| IoStoreError::Package("the package to delete did not parse"))?;
    if !header
        .package_name()
        .eq_ignore_ascii_case(request.package_path)
    {
        return Err(IoStoreError::Package(
            "the package to delete names a different package",
        ));
    }

    // A third chunk sharing the identity (an optional-segment payload, a second
    // bulk index) would be orphaned by retiring only these two.
    let package_id_bytes = package_id.0.to_le_bytes();
    if toc.chunk_ids.iter().enumerate().any(|(index, id)| {
        id.package_id() == package_id_bytes
            && index as u32 != uasset_index
            && index as u32 != ubulk_index
    }) {
        return Err(IoStoreError::Package(
            "the package to delete has more than two chunks",
        ));
    }

    let mut removed_paths = Vec::new();
    let mut surviving_entries = Vec::new();
    for entry in archive.entries() {
        if entry.chunk_index == uasset_index || entry.chunk_index == ubulk_index {
            removed_paths.push(entry.path.clone());
        } else {
            surviving_entries.push(entry.clone());
        }
    }
    let path_matches = |expected: Option<&str>, id: &FIoChunkId, index: u32| match expected {
        None => Ok(()),
        Some(expected) => {
            let listed = archive
                .entries()
                .iter()
                .any(|entry| entry.chunk_index == index && entry.path == expected);
            // An indexless overlay lists nothing, so the id is the only handle
            // there; an indexed container must agree with the caller exactly.
            if listed || (toc.directory_index_size == 0 && index_of(id) == Some(index)) {
                Ok(())
            } else {
                Err(IoStoreError::Package(
                    "the package to delete is at a different path",
                ))
            }
        }
    };
    path_matches(request.expected_uasset_path, &uasset_id, uasset_index)?;
    path_matches(request.expected_ubulk_path, &ubulk_id, ubulk_index)?;

    let header_indices: Vec<u32> = toc
        .chunk_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            (id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER).then_some(index as u32)
        })
        .collect();
    if header_indices.len() != 1 {
        // Duplication always leaves exactly one behind, synthesizing it when the
        // overlay had none. Any other shape means something else wrote this
        // container and the store-entry invariant cannot be established.
        return Err(IoStoreError::Package(
            "target does not have exactly one ContainerHeader chunk",
        ));
    }
    let header_index = header_indices[0];
    let header_id = toc.chunk_ids[header_index as usize];
    if header_id.package_id() != toc.container_id.to_le_bytes() {
        return Err(IoStoreError::Package(
            "ContainerHeader id does not match container id",
        ));
    }
    let mut container_header =
        FIoContainerHeader::deserialize(&mut Cursor::new(archive.read_chunk(header_index)?), None)
            .map_err(|_| IoStoreError::Package("container package-store header did not parse"))?;
    if container_header.container_id.0 != toc.container_id {
        return Err(IoStoreError::Package(
            "ContainerHeader payload id does not match TOC",
        ));
    }
    crate::iostore::compat::check_writable_container_header_version(container_header.version)
        .map_err(|_| IoStoreError::Package("unsupported container package-store header"))?;
    // Each of these is keyed by store-entry ordinal or by package id, and none
    // of them is rewritten here, so removing a package would silently desync it.
    if container_header.has_soft_package_references() {
        return Err(IoStoreError::Package(
            "container header carries soft package references",
        ));
    }
    if container_header.has_optional_segment() {
        return Err(IoStoreError::Package(
            "container header carries an optional segment",
        ));
    }
    if container_header.redirects_to(package_id) {
        return Err(IoStoreError::Package(
            "another package redirects to the package to delete",
        ));
    }
    if container_header.is_localized_source(package_id) {
        return Err(IoStoreError::Package(
            "the package to delete is a localized source package",
        ));
    }
    if !container_header.remove_package(package_id) {
        return Err(IoStoreError::Package(
            "the package to delete is not in the package store",
        ));
    }
    let surviving_package_ids: BTreeSet<FPackageId> = container_header.package_ids().collect();
    let header_bytes = serialize_aligned_container_header(&container_header)?;

    Ok(ResolvedTagDeletion {
        package_id,
        uasset_id,
        ubulk_id,
        uasset_index,
        ubulk_index,
        removed_paths,
        surviving_entries,
        header_index,
        header_id,
        header_bytes,
        surviving_package_ids,
    })
}

fn delete_tag_in_place_impl(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlaceTagDeletion<'_>,
    failure: Option<DuplicateFailurePoint>,
) -> Result<()> {
    let original_toc_bytes = std::fs::read(utoc_path)?;
    if !archive.matches_utoc_path(utoc_path) || archive.toc_bytes() != original_toc_bytes {
        return Err(IoStoreError::Package(
            "archive handle is stale or targets another container",
        ));
    }
    let toc = parse_toc(&original_toc_bytes)?;
    if archive.chunk_count() != toc.entry_count {
        return Err(IoStoreError::Package("archive handle chunk count is stale"));
    }
    for (index, expected_id) in toc.chunk_ids.iter().enumerate() {
        if archive.chunk_id(index as u32)? != *expected_id {
            return Err(IoStoreError::Package("archive handle chunk ids are stale"));
        }
    }
    let mut raw_ids = BTreeSet::new();
    for id in &toc.chunk_ids {
        if !raw_ids.insert(id.0) {
            return Err(IoStoreError::Package(
                "target TOC contains a duplicate chunk id",
            ));
        }
    }

    let resolved = resolve_tag_deletion(archive, &toc, request)?;
    if resolved.header_index == resolved.uasset_index || resolved.header_index == resolved.ubulk_index
    {
        return Err(IoStoreError::Package(
            "the package to delete is the ContainerHeader chunk",
        ));
    }

    let ucas_path = utoc_path.with_extension("ucas");
    let old_ucas_len = std::fs::metadata(&ucas_path)?.len();
    let plan = plan_toc_append(
        &toc,
        old_ucas_len,
        Vec::new(),
        vec![ExistingTocReplacement {
            chunk_index: resolved.header_index,
            bytes: resolved.header_bytes.clone(),
        }],
        &[
            TocTombstone {
                chunk_index: resolved.uasset_index,
            },
            TocTombstone {
                chunk_index: resolved.ubulk_index,
            },
        ],
        &resolved.surviving_entries,
    )?;

    append_ucas_items(&ucas_path, &plan.items)?;
    if failure == Some(DuplicateFailurePoint::AfterAppend) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-append failure"),
        );
    }
    if let Err(error) = atomic_replace_file(utoc_path, &plan.new_toc) {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    if failure == Some(DuplicateFailurePoint::AfterTocWrite) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-TOC-write failure"),
        );
    }
    if failure == Some(DuplicateFailurePoint::BeforeValidation) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected validation failure"),
        );
    }
    if let Err(error) = validate_delete_result(utoc_path, &toc, &resolved, &plan) {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    Ok(())
}

/// Move a chunk id to a different package, preserving everything else in it.
///
/// Byte substitution rather than [`make_chunk_id`] on purpose. The id's 16-bit
/// index field is written little-endian there, while `FIoChunkId`'s own doc says
/// it is stored byte-swapped — and every call site passes index 0, where the two
/// encodings coincide, so nothing has ever read it back and the question is
/// unresolved. Copying the bytes sidesteps it entirely, and preserves the pad
/// byte that `retire_chunk_id` repurposes.
fn retarget_chunk_id(old: FIoChunkId, new_package_id: FPackageId) -> FIoChunkId {
    let mut bytes = old.0;
    bytes[..8].copy_from_slice(&new_package_id.0.to_le_bytes());
    FIoChunkId(bytes)
}

/// The leaf of a `/Game/...` package path, and everything before it.
fn split_package_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((parent, leaf)) => (parent, leaf),
        None => ("", path),
    }
}

/// Rewrite one member's container path for the move.
///
/// The path is taken from the directory index and edited, never synthesized:
/// `chunk_type_extension` has no `.umap` arm while `world` treats `.umap` as a
/// first-class package extension, so building a path from the package name and a
/// chunk type would quietly rename a level's `.umap` to `.uasset`. The extension
/// — including a compound one like `.m.ubulk` — is carried through untouched.
fn rename_entry_path(
    entry_path: &str,
    old_package: &str,
    new_package: &str,
) -> Result<String> {
    let (old_parent, old_leaf) = split_package_path(old_package);
    let (new_parent, new_leaf) = split_package_path(new_package);
    let normalized = entry_path.replace('\\', "/");
    let (dir, file) = match normalized.rsplit_once('/') {
        Some((dir, file)) => (dir.to_owned(), file.to_owned()),
        None => (String::new(), normalized.clone()),
    };
    if !file.len().checked_sub(old_leaf.len()).is_some_and(|_| {
        file.get(..old_leaf.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(old_leaf))
            && file[old_leaf.len()..].starts_with('.')
    }) {
        return Err(IoStoreError::Package(
            "a chunk of the package to rename is at an unexpected path",
        ));
    }
    let file = format!("{new_leaf}{}", &file[old_leaf.len()..]);

    if old_parent.eq_ignore_ascii_case(new_parent) {
        return Ok(if dir.is_empty() {
            file
        } else {
            format!("{dir}/{file}")
        });
    }
    // A folder move. The container's mount prefix (`Meteorite/Content/`) is not
    // derivable from the package path, so match on the tail the two share and
    // replace only that, keeping the container's own spelling of the prefix.
    let old_tail = old_parent.trim_start_matches('/');
    let old_tail = old_tail.strip_prefix("Game/").unwrap_or(old_tail);
    let new_tail = new_parent.trim_start_matches('/');
    let new_tail = new_tail.strip_prefix("Game/").unwrap_or(new_tail);
    let Some(cut) = dir.len().checked_sub(old_tail.len()) else {
        return Err(IoStoreError::Package(
            "the package to rename is not under the folder its path names",
        ));
    };
    if !dir[cut..].eq_ignore_ascii_case(old_tail) {
        return Err(IoStoreError::Package(
            "the package to rename is not under the folder its path names",
        ));
    }
    let dir = format!("{}{new_tail}", &dir[..cut]);
    Ok(if dir.is_empty() {
        file
    } else {
        format!("{dir}/{file}")
    })
}

/// Rewrite a package's own header so it says what it is now called.
///
/// Splices rather than going through `write_package`: the export payloads are
/// carried across untouched, and rebuilding them would require decoding every
/// export of a package this crate may not have a schema for. Export offsets are
/// relative to the end of the header, so a header that changes size is fine.
fn retarget_package_identity(
    bytes: &[u8],
    old_package: &str,
    new_package: &str,
    bulk_serial_size: Option<i64>,
) -> Result<Vec<u8>> {
    use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};

    let mut header = FZenPackageHeader::deserialize(
        &mut Cursor::new(bytes),
        None,
        CE_TOC_VERSION,
        CE_CONTAINER_HEADER_VERSION,
        None,
    )
    .map_err(|_| IoStoreError::Package("the package to rename did not parse"))?;
    let header_size = header.summary.header_size as usize;
    if header_size > bytes.len() {
        return Err(IoStoreError::Package(
            "the package to rename has a header longer than its bytes",
        ));
    }
    let tail = bytes[header_size..].to_vec();

    header.summary.name = header.name_map.store(new_package);

    // The bulk chunk's length lives in the *package's* header, not in the TOC
    // entry, so replacing the payload without this reads the old number of
    // bytes out of the new chunk. Rewritten on the parsed header rather than
    // byte-patched, because the header is being reserialized here anyway.
    if let Some(size) = bulk_serial_size {
        if header.bulk_data.len() != 1 {
            return Err(IoStoreError::Package(
                "a replacement body needs exactly one bulk-data map entry",
            ));
        }
        if header.bulk_data[0].serial_offset != 0 {
            return Err(IoStoreError::Package(
                "the bulk-data entry does not start at the beginning of its chunk",
            ));
        }
        header.bulk_data[0].serial_size = size;
    }

    // Only the export the package is named after, and only when that name
    // actually changes. `public_export_hash` is how *other* packages address
    // this export, so a folder move — which leaves the object name alone — must
    // not touch it.
    let (_, old_leaf) = split_package_path(old_package);
    let (_, new_leaf) = split_package_path(new_package);
    if !old_leaf.eq_ignore_ascii_case(new_leaf) {
        let matches: Vec<usize> = header
            .export_map
            .iter()
            .enumerate()
            .filter(|(_, export)| export.outer_index.is_null())
            .filter(|(_, export)| {
                header
                    .name_map
                    .try_get(export.object_name)
                    .is_some_and(|name| name.eq_ignore_ascii_case(old_leaf))
            })
            .map(|(index, _)| index)
            .collect();
        // Exactly one, or none. Several exports sharing the package's name is a
        // shape this cannot reason about, and guessing which one is the asset
        // would rename the wrong object; leaving them all is recoverable.
        if matches.len() == 1 {
            let index = matches[0];
            header.export_map[index].object_name = header.name_map.store(new_leaf);
            header.export_map[index].public_export_hash =
                crate::iostore::package::imports::public_export_hash(new_leaf);
        }
    }

    let mut store = StoreEntry::default();
    let mut serialized = Cursor::new(Vec::new());
    header
        .serialize(&mut serialized, &mut store, CE_CONTAINER_HEADER_VERSION)
        .map_err(|_| IoStoreError::Package("failed to serialize the renamed package"))?;
    let mut package = serialized.into_inner();
    package.extend_from_slice(&tail);
    Ok(package)
}

struct RenamedMember {
    old_index: u32,
    new_id: FIoChunkId,
    bytes: Vec<u8>,
    new_path: Option<String>,
}

struct ResolvedPackageRename {
    new_package_id: FPackageId,
    members: Vec<RenamedMember>,
    surviving_entries: Vec<Entry>,
    header_index: u32,
    header_bytes: Vec<u8>,
    expected_package_ids: BTreeSet<FPackageId>,
}

/// Establish what a rename would move, and prove it is safe, before anything is
/// written.
fn resolve_package_rename(
    archive: &IoStoreArchive,
    toc: &ParsedToc,
    request: &InPlacePackageRename<'_>,
) -> Result<ResolvedPackageRename> {
    if request.old_package_path.is_empty() || request.new_package_path.is_empty() {
        return Err(IoStoreError::Package("rename package path is empty"));
    }
    let old_id = FPackageId::from_name(request.old_package_path);
    let new_id = FPackageId::from_name(request.new_package_path);
    // `FPackageId::from_name` lowercases, so a case-only rename produces the
    // same id — and the addition would collide with its own tombstone.
    if old_id == new_id {
        return Err(IoStoreError::Package(
            "the new package path hashes to the same id as the old one",
        ));
    }

    let old_key = old_id.0.to_le_bytes();
    // Tombstones are skipped, for the same reason the collision check below
    // skips them: `retire_chunk_id` rewrites only the type and pad bytes, so a
    // retired slot keeps the package id it was retired from. A package that has
    // been renamed away from this path before has left some here, and they are
    // neither payload to move nor a shape to refuse -- counting them as members
    // made every second rename fail on the member-type gate.
    let member_indices: Vec<u32> = toc
        .chunk_ids
        .iter()
        .enumerate()
        .filter(|(_, id)| id.package_id() == old_key && id.chunk_type() != RETIRED_CHUNK_TYPE)
        .map(|(index, _)| index as u32)
        .collect();
    if member_indices.is_empty() {
        return Err(IoStoreError::Package(
            "the package to rename is not in this container",
        ));
    }
    if let Some(minimum) = request.minimum_appended_index
        && member_indices.iter().any(|index| *index < minimum)
    {
        return Err(IoStoreError::Package(
            "the package to rename predates this container's appended chunks",
        ));
    }

    let bundles: Vec<u32> = member_indices
        .iter()
        .copied()
        .filter(|index| toc.chunk_ids[*index as usize].chunk_type() == CHUNK_TYPE_EXPORT_BUNDLE_DATA)
        .collect();
    if bundles.len() != 1 {
        return Err(IoStoreError::Package(
            "the package to rename does not have exactly one export bundle chunk",
        ));
    }
    let bundle_index = bundles[0];
    for index in &member_indices {
        let kind = toc.chunk_ids[*index as usize].chunk_type();
        // Anything else sharing the identity is a shape this cannot move, and
        // moving only part of a package is worse than refusing.
        if *index != bundle_index
            && !matches!(
                kind,
                CHUNK_TYPE_BULK_DATA
                    | crate::iostore::CHUNK_TYPE_OPTIONAL_BULK_DATA
                    | crate::iostore::CHUNK_TYPE_MEMORY_MAPPED_BULK_DATA
            )
        {
            return Err(IoStoreError::Package(
                "the package to rename owns a chunk of an unexpected type",
            ));
        }
    }

    // Prove the bundle really holds the package the caller named.
    let bundle_bytes = match request.replacement_export_bundle {
        Some(bytes) => bytes.to_vec(),
        None => archive.read_chunk(bundle_index)?,
    };
    {
        use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};
        let header = FZenPackageHeader::deserialize(
            &mut Cursor::new(&bundle_bytes),
            None,
            CE_TOC_VERSION,
            CE_CONTAINER_HEADER_VERSION,
            None,
        )
        .map_err(|_| IoStoreError::Package("the package to rename did not parse"))?;
        if !header
            .package_name()
            .eq_ignore_ascii_case(request.old_package_path)
        {
            return Err(IoStoreError::Package(
                "the package to rename names a different package",
            ));
        }
    }

    // A replacement body must name exactly one chunk, or "the new length" does
    // not say which one changed.
    let replacement_bulk = match request.replacement_bulk_data {
        Some(body) => {
            let bulk: Vec<u32> = member_indices
                .iter()
                .copied()
                .filter(|index| toc.chunk_ids[*index as usize].chunk_type() == CHUNK_TYPE_BULK_DATA)
                .collect();
            if bulk.len() != 1 {
                return Err(IoStoreError::Package(
                    "a replacement body needs the package to own exactly one bulk-data chunk",
                ));
            }
            if body.len() > i64::MAX as usize {
                return Err(IoStoreError::Package(
                    "replacement body is too large for SerialSize",
                ));
            }
            Some((bulk[0], body))
        }
        None => None,
    };

    let indexed = toc.directory_index_size != 0;
    let mut members = Vec::new();
    let mut moved_indices = BTreeSet::new();
    for index in &member_indices {
        moved_indices.insert(*index);
        let bytes = if *index == bundle_index {
            retarget_package_identity(
                &bundle_bytes,
                request.old_package_path,
                request.new_package_path,
                replacement_bulk.map(|(_, body)| body.len() as i64),
            )?
        } else if let Some((bulk_index, body)) = replacement_bulk
            && *index == bulk_index
        {
            body.to_vec()
        } else {
            archive.read_chunk(*index)?
        };
        let new_path = if indexed {
            let entry = archive
                .entries()
                .iter()
                .find(|entry| entry.chunk_index == *index)
                .ok_or(IoStoreError::Package(
                    "a chunk of the package to rename has no directory entry",
                ))?;
            Some(rename_entry_path(
                &entry.path,
                request.old_package_path,
                request.new_package_path,
            )?)
        } else {
            None
        };
        members.push(RenamedMember {
            old_index: *index,
            new_id: retarget_chunk_id(toc.chunk_ids[*index as usize], new_id),
            bytes,
            new_path,
        });
    }

    // Retired slots are skipped, and that is the whole reason renaming a package
    // back to a name it once had works. `retire_chunk_id` rewrites only the type
    // and pad bytes, so a tombstone keeps the package id it was retired from --
    // it just holds no payload and can never be resolved. Treating one as an
    // occupant would make every name single-use for the life of the container.
    if toc.chunk_ids.iter().any(|id| {
        id.chunk_type() != RETIRED_CHUNK_TYPE && id.package_id() == new_id.0.to_le_bytes()
    }) {
        return Err(IoStoreError::Package(
            "the destination package already has chunks in this container",
        ));
    }
    let surviving_entries: Vec<Entry> = archive
        .entries()
        .iter()
        .filter(|entry| !moved_indices.contains(&entry.chunk_index))
        .cloned()
        .collect();
    if indexed
        && members.iter().any(|member| {
            member.new_path.as_ref().is_some_and(|path| {
                surviving_entries
                    .iter()
                    .any(|entry| entry.path.eq_ignore_ascii_case(path))
            })
        })
    {
        return Err(IoStoreError::Package("destination path already exists"));
    }

    let header_indices: Vec<u32> = toc
        .chunk_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            (id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER).then_some(index as u32)
        })
        .collect();
    if header_indices.len() != 1 {
        return Err(IoStoreError::Package(
            "target does not have exactly one ContainerHeader chunk",
        ));
    }
    let header_index = header_indices[0];
    if moved_indices.contains(&header_index) {
        return Err(IoStoreError::Package(
            "the package to rename is the ContainerHeader chunk",
        ));
    }
    let mut container_header =
        FIoContainerHeader::deserialize(&mut Cursor::new(archive.read_chunk(header_index)?), None)
            .map_err(|_| IoStoreError::Package("container package-store header did not parse"))?;
    if container_header.container_id.0 != toc.container_id {
        return Err(IoStoreError::Package(
            "ContainerHeader payload id does not match TOC",
        ));
    }
    crate::iostore::compat::check_writable_container_header_version(container_header.version)
        .map_err(|_| IoStoreError::Package("unsupported container package-store header"))?;
    // Both are keyed by store-entry ordinal, and the store is a BTreeMap over
    // FPackageId — so adding renumbers them exactly as removing does.
    if container_header.has_soft_package_references() {
        return Err(IoStoreError::Package(
            "container header carries soft package references",
        ));
    }
    if container_header.has_optional_segment() {
        return Err(IoStoreError::Package(
            "container header carries an optional segment",
        ));
    }
    if container_header.is_localized_source(old_id) {
        return Err(IoStoreError::Package(
            "the package to rename is a localized source package",
        ));
    }
    if container_header.get_store_entry(new_id).is_some() {
        return Err(IoStoreError::Package(
            "the destination package is already in the package store",
        ));
    }
    // Carried over rather than re-derived. Nothing in a store entry depends on
    // the package's own name, and re-deriving one from a header parsed without
    // its store entry would silently drop every shader map hash it had.
    let store = container_header
        .get_store_entry(old_id)
        .ok_or(IoStoreError::Package(
            "the package to rename is not in the package store",
        ))?;
    if !container_header.remove_package(old_id) {
        return Err(IoStoreError::Package(
            "the package to rename is not in the package store",
        ));
    }
    container_header.add_package(new_id, store);
    // Anything that pointed at the old id follows it, so a package can be
    // renamed more than once.
    container_header.retarget_package_redirect(old_id, new_id);
    if request.redirect {
        container_header
            .add_package_redirect(request.old_package_path, new_id)
            .map_err(|_| IoStoreError::Package("failed to record the rename redirect"))?;
    }
    let expected_package_ids: BTreeSet<FPackageId> = container_header.package_ids().collect();
    let header_bytes = serialize_aligned_container_header(&container_header)?;

    Ok(ResolvedPackageRename {
        new_package_id: new_id,
        members,
        surviving_entries,
        header_index,
        header_bytes,
        expected_package_ids,
    })
}

fn rename_package_in_place_impl(
    archive: &IoStoreArchive,
    utoc_path: &Path,
    request: &InPlacePackageRename<'_>,
    failure: Option<DuplicateFailurePoint>,
) -> Result<()> {
    let original_toc_bytes = std::fs::read(utoc_path)?;
    if !archive.matches_utoc_path(utoc_path) || archive.toc_bytes() != original_toc_bytes {
        return Err(IoStoreError::Package(
            "archive handle is stale or targets another container",
        ));
    }
    let toc = parse_toc(&original_toc_bytes)?;
    if archive.chunk_count() != toc.entry_count {
        return Err(IoStoreError::Package("archive handle chunk count is stale"));
    }
    for (index, expected_id) in toc.chunk_ids.iter().enumerate() {
        if archive.chunk_id(index as u32)? != *expected_id {
            return Err(IoStoreError::Package("archive handle chunk ids are stale"));
        }
    }
    let mut raw_ids = BTreeSet::new();
    for id in &toc.chunk_ids {
        if !raw_ids.insert(id.0) {
            return Err(IoStoreError::Package(
                "target TOC contains a duplicate chunk id",
            ));
        }
    }

    let resolved = resolve_package_rename(archive, &toc, request)?;

    let additions: Vec<NewTocChunk> = resolved
        .members
        .iter()
        .map(|member| NewTocChunk {
            id: member.new_id,
            bytes: member.bytes.clone(),
            path: member.new_path.clone(),
        })
        .collect();
    let tombstones: Vec<TocTombstone> = resolved
        .members
        .iter()
        .map(|member| TocTombstone {
            chunk_index: member.old_index,
        })
        .collect();

    let ucas_path = utoc_path.with_extension("ucas");
    let old_ucas_len = std::fs::metadata(&ucas_path)?.len();
    let plan = plan_toc_append(
        &toc,
        old_ucas_len,
        additions,
        vec![ExistingTocReplacement {
            chunk_index: resolved.header_index,
            bytes: resolved.header_bytes.clone(),
        }],
        &tombstones,
        &resolved.surviving_entries,
    )?;

    append_ucas_items(&ucas_path, &plan.items)?;
    if failure == Some(DuplicateFailurePoint::AfterAppend) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-append failure"),
        );
    }
    if let Err(error) = atomic_replace_file(utoc_path, &plan.new_toc) {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    if failure == Some(DuplicateFailurePoint::AfterTocWrite) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected post-TOC-write failure"),
        );
    }
    if failure == Some(DuplicateFailurePoint::BeforeValidation) {
        return restore_original_toc(
            utoc_path,
            &original_toc_bytes,
            IoStoreError::Package("injected validation failure"),
        );
    }
    if let Err(error) = validate_rename_result(utoc_path, &toc, request, &resolved, &plan) {
        return restore_original_toc(utoc_path, &original_toc_bytes, error);
    }
    Ok(())
}

/// Reopen the container and prove the rename landed.
///
/// Not `validate_delete_result`: that asserts the chunk count is unchanged, and
/// a rename grows it by one slot per member. The rest of the obligations are the
/// same, plus the ones only a rename has — that the old ids are gone, the new
/// ones resolve, and the store entry moved.
fn validate_rename_result(
    utoc_path: &Path,
    original_toc: &ParsedToc,
    request: &InPlacePackageRename<'_>,
    resolved: &ResolvedPackageRename,
    plan: &TocAppendPlan,
) -> Result<()> {
    let saved_toc_bytes = std::fs::read(utoc_path)?;
    let saved_toc = parse_toc(&saved_toc_bytes)?;

    if saved_toc.entry_count != original_toc.entry_count + resolved.members.len() as u32 {
        return Err(IoStoreError::Package(
            "rename did not add exactly one chunk per member",
        ));
    }
    validate_perfect_hash_result(original_toc, &saved_toc, plan)?;
    if saved_toc.partition_size != original_toc.partition_size
        || saved_toc.container_id != original_toc.container_id
        || saved_toc.block_size != original_toc.block_size
    {
        return Err(IoStoreError::Package("rename changed TOC header fields"));
    }
    if saved_toc.blocks.get(..original_toc.blocks.len()) != Some(original_toc.blocks.as_slice()) {
        return Err(IoStoreError::Package(
            "rename rewrote existing compression blocks",
        ));
    }

    // Each retired slot keeps its position and its retired marker, so the
    // surviving chunks' indices — and the perfect hash's modulo base — are
    // untouched.
    for member in &resolved.members {
        let slot = member.old_index as usize;
        let retired = retire_chunk_id(original_toc.chunk_ids[slot]);
        if saved_toc.chunk_ids.get(slot) != Some(&retired) {
            return Err(IoStoreError::Package(
                "a renamed chunk's old slot was not retired",
            ));
        }
        if saved_toc.offset_lengths.get(slot) != Some(&[0u8; 10]) {
            return Err(IoStoreError::Package(
                "a retired slot still points at payload",
            ));
        }
    }

    let reopened = IoStoreArchive::open(utoc_path)?;
    for member in &resolved.members {
        if reopened.find_chunk(&member.new_id).is_none() {
            return Err(IoStoreError::Package(
                "a renamed chunk is missing from the reopened container",
            ));
        }
        if reopened
            .find_chunk(&original_toc.chunk_ids[member.old_index as usize])
            .is_some()
        {
            return Err(IoStoreError::Package(
                "a renamed chunk's old id still resolves",
            ));
        }
    }

    // Paths are only meaningful where a directory index exists. An indexless
    // overlay reconstructs them, and comparing against that would roll back
    // correct writes over recovered casing.
    if saved_toc.directory_index_size != 0 {
        for member in &resolved.members {
            let Some(path) = &member.new_path else { continue };
            if !reopened
                .entries()
                .iter()
                .any(|entry| entry.path.eq_ignore_ascii_case(path))
            {
                return Err(IoStoreError::Package(
                    "a renamed chunk is not at its new path",
                ));
            }
        }
        for entry in &resolved.surviving_entries {
            if !reopened
                .entries()
                .iter()
                .any(|saved| saved.path == entry.path)
            {
                return Err(IoStoreError::Package("rename dropped a surviving entry"));
            }
        }
    }

    let header_bytes = reopened.read_chunk(resolved.header_index)?;
    let header = FIoContainerHeader::deserialize(&mut Cursor::new(header_bytes), None)
        .map_err(|_| IoStoreError::Package("rewritten container header did not parse"))?;
    let saved_ids: BTreeSet<FPackageId> = header.package_ids().collect();
    if saved_ids != resolved.expected_package_ids {
        return Err(IoStoreError::Package(
            "rename did not move the package store entry",
        ));
    }
    if header.get_store_entry(resolved.new_package_id).is_none() {
        return Err(IoStoreError::Package(
            "the renamed package is not in the package store",
        ));
    }
    if request.redirect
        && header
            .lookup_package_redirect(FPackageId::from_name(request.old_package_path))
            != Some(resolved.new_package_id)
    {
        return Err(IoStoreError::Package(
            "the rename redirect was not recorded",
        ));
    }
    Ok(())
}

fn validate_delete_result(
    utoc_path: &Path,
    original_toc: &ParsedToc,
    resolved: &ResolvedTagDeletion,
    plan: &TocAppendPlan,
) -> Result<()> {
    let saved_toc_bytes = std::fs::read(utoc_path)?;
    let saved_toc = parse_toc(&saved_toc_bytes)?;

    // The assertion the whole design rests on. A shrinking chunk count would
    // change the perfect hash's modulo base for chunks nobody asked to touch.
    if saved_toc.entry_count != original_toc.entry_count {
        return Err(IoStoreError::Package("deletion changed the TOC chunk count"));
    }
    validate_perfect_hash_result(original_toc, &saved_toc, plan)?;
    if saved_toc.partition_size != original_toc.partition_size
        || saved_toc.container_id != original_toc.container_id
        || saved_toc.block_size != original_toc.block_size
        || saved_toc.compression_methods != original_toc.compression_methods
    {
        return Err(IoStoreError::Package("deletion changed TOC header fields"));
    }
    if saved_toc.blocks.get(..original_toc.blocks.len()) != Some(original_toc.blocks.as_slice()) {
        return Err(IoStoreError::Package(
            "deletion rewrote existing compression blocks",
        ));
    }

    let retired = [resolved.uasset_index, resolved.ubulk_index];
    for index in retired {
        let original_id = original_toc
            .chunk_ids
            .get(index as usize)
            .ok_or(IoStoreError::Package("retired chunk index out of range"))?;
        if saved_toc.chunk_ids.get(index as usize) != Some(&retire_chunk_id(*original_id)) {
            return Err(IoStoreError::Package("a retired chunk kept its id"));
        }
        if saved_toc.offset_lengths[index as usize] != [0; 10]
            || saved_toc.metas[index as usize] != [0; TOC_META_SIZE]
        {
            return Err(IoStoreError::Package("a retired chunk kept its payload"));
        }
    }
    let mut touched = retired.to_vec();
    touched.push(resolved.header_index);
    validate_untouched_chunks(original_toc, &saved_toc, &touched)?;

    let reopened = IoStoreArchive::open(utoc_path)?;
    if reopened.chunk_count() != original_toc.entry_count {
        return Err(IoStoreError::Package("reopened chunk count is incorrect"));
    }
    if reopened.find_chunk(&resolved.uasset_id).is_some()
        || reopened.find_chunk(&resolved.ubulk_id).is_some()
    {
        return Err(IoStoreError::Package(
            "the deleted package is still resolvable",
        ));
    }
    let indexed = original_toc.directory_index_size != 0;
    if indexed != reopened.has_directory_index() {
        return Err(IoStoreError::Package("directory-index mode changed"));
    }
    // Only an indexed container has paths to check. An indexless overlay
    // addresses everything by id, and the paths it appears to have are
    // reconstructed by the caller's recovery pass, not stored in the file.
    if indexed {
        for entry in &resolved.surviving_entries {
            let expected = original_toc
                .chunk_ids
                .get(entry.chunk_index as usize)
                .ok_or(IoStoreError::Package(
                    "a surviving directory entry has a bad chunk index",
                ))?;
            if reopened.chunk_id_for(&entry.path)? != *expected {
                return Err(IoStoreError::Package("a surviving directory entry changed"));
            }
        }
        for path in &resolved.removed_paths {
            if reopened.contains(path) {
                return Err(IoStoreError::Package(
                    "the deleted package is still listed in the directory index",
                ));
            }
        }
    }
    let saved_header_index = reopened
        .find_chunk(&resolved.header_id)
        .ok_or(IoStoreError::Package("saved ContainerHeader is absent"))?;
    let saved_header = FIoContainerHeader::deserialize(
        &mut Cursor::new(reopened.read_chunk(saved_header_index)?),
        None,
    )
    .map_err(|_| IoStoreError::Package("saved ContainerHeader did not parse"))?;
    if saved_header.container_id.0 != original_toc.container_id
        || saved_header.get_store_entry(resolved.package_id).is_some()
    {
        return Err(IoStoreError::Package(
            "the deleted package is still in the package store",
        ));
    }
    if saved_header.package_ids().collect::<BTreeSet<_>>() != resolved.surviving_package_ids {
        return Err(IoStoreError::Package(
            "deletion changed another package's store entry",
        ));
    }
    Ok(())
}

fn serialize_aligned_container_header(header: &FIoContainerHeader) -> Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    header
        .serialize(&mut cursor)
        .map_err(|_| IoStoreError::Package("container package-store header did not serialize"))?;
    let mut bytes = cursor.into_inner();
    let aligned = (bytes.len() + 15) & !15;
    bytes.resize(aligned, 0);
    Ok(bytes)
}

fn append_ucas_items(ucas_path: &Path, items: &[TocAppendItem]) -> Result<()> {
    let mut ucas = std::fs::OpenOptions::new().append(true).open(ucas_path)?;
    for item in items {
        ucas.write_all(&item.bytes)?;
    }
    ucas.flush()?;
    ucas.sync_all()?;
    Ok(())
}

fn restore_original_toc(utoc_path: &Path, original: &[u8], error: IoStoreError) -> Result<()> {
    match atomic_replace_file(utoc_path, original) {
        Ok(()) => Err(error),
        Err(rollback_error) => Err(rollback_error),
    }
}

fn atomic_replace_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or(IoStoreError::Truncated("path has no filename"))?
        .to_string_lossy();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".{file_name}.codex-{nonce}-{}", std::process::id()));
    let write_result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    write_result
}

fn validate_duplicate_result(
    utoc_path: &Path,
    original_toc: &ParsedToc,
    old_entries: &[Entry],
    request: &InPlaceTagDuplicate<'_>,
    cloned: &ClonedTagPackage,
    header_id: FIoChunkId,
    plan: &TocAppendPlan,
) -> Result<()> {
    let saved_toc_bytes = std::fs::read(utoc_path)?;
    let saved_toc = parse_toc(&saved_toc_bytes)?;
    validate_perfect_hash_result(original_toc, &saved_toc, plan)?;
    if !plan.dropped_perfect_hash {
        for index in &plan.new_chunk_indices {
            if !toc_overflow_contains(&saved_toc.chunks_without_perfect_hash, *index) {
                return Err(IoStoreError::Package(
                    "new chunk is absent from TOC overflow lookup",
                ));
            }
        }
    }

    let reopened = IoStoreArchive::open(utoc_path)?;
    if reopened.chunk_count() != original_toc.entry_count + plan.new_chunk_indices.len() as u32 {
        return Err(IoStoreError::Package("reopened chunk count is incorrect"));
    }
    let indexed = original_toc.directory_index_size != 0;

    let touched: Vec<u32> = plan.items.iter().map(|item| item.chunk_index).collect();
    validate_untouched_chunks(original_toc, &saved_toc, &touched)?;
    for item in &plan.items {
        let index = reopened
            .find_chunk(&item.id)
            .ok_or(IoStoreError::Package("saved chunk id is absent"))?;
        if reopened.read_chunk(index)? != item.bytes {
            return Err(IoStoreError::Package("saved chunk bytes failed validation"));
        }
    }

    // Only an indexed container has paths to check — the same rule the deletion
    // path follows. An indexless overlay stores no paths at all: the ones it
    // appears to have were reconstructed by the caller, from whichever base
    // containers it had, and those carry the *base's* casing. Re-deriving them
    // here from package names alone disagrees with that for 7,534 of the
    // shipped corpus's 12,329 tag packages — `Tags/objects/Characters/Marine/`
    // against `/Game/Tags/objects/characters/marine/` — so every lookup of a
    // caller-supplied path missed and rolled a correct write back.
    //
    // Nothing is lost by skipping them. Both new chunks are already verified by
    // id and compared byte for byte above, every untouched chunk is checked by
    // `validate_untouched_chunks`, and the package-store entry is checked below.
    if indexed {
        for old_entry in old_entries {
            let id = original_toc
                .chunk_ids
                .get(old_entry.chunk_index as usize)
                .ok_or(IoStoreError::Package(
                    "old directory entry has a bad chunk index",
                ))?;
            if reopened.chunk_id_for(&old_entry.path)? != *id {
                return Err(IoStoreError::Package("an existing directory entry changed"));
            }
        }
        if reopened.chunk_id_for(request.destination_uasset_path)? != cloned.uasset_id
            || reopened.chunk_id_for(request.destination_ubulk_path)? != cloned.ubulk_id
        {
            return Err(IoStoreError::Package(
                "destination directory entries are incorrect",
            ));
        }
    }
    if indexed != reopened.has_directory_index() {
        return Err(IoStoreError::Package("directory-index mode changed"));
    }

    let package_index = reopened
        .find_chunk(&cloned.uasset_id)
        .ok_or(IoStoreError::Package("saved cloned package is absent"))?;
    let package_bytes = reopened.read_chunk(package_index)?;
    validate_cloned_package(cloned, request, &package_bytes)?;

    let saved_header_index = reopened
        .find_chunk(&header_id)
        .ok_or(IoStoreError::Package("saved ContainerHeader is absent"))?;
    let saved_header = FIoContainerHeader::deserialize(
        &mut Cursor::new(reopened.read_chunk(saved_header_index)?),
        None,
    )
    .map_err(|_| IoStoreError::Package("saved ContainerHeader did not parse"))?;
    if saved_header.container_id.0 != original_toc.container_id
        || saved_header.get_store_entry(cloned.package_id) != Some(cloned.store.clone())
    {
        return Err(IoStoreError::Package(
            "saved package-store entry is incorrect",
        ));
    }
    Ok(())
}

/// Assert that every chunk the operation did not touch still addresses exactly
/// the bytes it did before: same id, same offset and length, same content hash,
/// and an unchanged prefix of the compression-block array.
///
/// This is deliberately structural rather than a read-back of the whole
/// container. Decompressing every chunk of a shipping pak costs minutes per
/// edit, and — worse — it fails on chunks the bundled Oodle implementation
/// cannot decode at all, which a real game pak contains and which no in-place
/// edit has any bearing on. Those reads never proved anything the offsets and
/// blocks do not: a chunk whose entry and blocks are byte-identical resolves to
/// byte-identical input, whether or not this crate happens to be able to
/// decompress it.
fn validate_untouched_chunks(
    original_toc: &ParsedToc,
    saved_toc: &ParsedToc,
    touched: &[u32],
) -> Result<()> {
    for (index, original_id) in original_toc.chunk_ids.iter().enumerate() {
        if touched.contains(&(index as u32)) {
            continue;
        }
        if saved_toc.chunk_ids.get(index) != Some(original_id) {
            return Err(IoStoreError::Package("an existing chunk id changed"));
        }
        if saved_toc.offset_lengths.get(index) != Some(&original_toc.offset_lengths[index]) {
            return Err(IoStoreError::Package("an existing chunk moved"));
        }
        if saved_toc.metas.get(index) != Some(&original_toc.metas[index]) {
            return Err(IoStoreError::Package("an existing chunk's metadata changed"));
        }
    }
    if saved_toc.blocks.get(..original_toc.blocks.len()) != Some(original_toc.blocks.as_slice()) {
        return Err(IoStoreError::Package(
            "existing compression blocks were rewritten",
        ));
    }
    if saved_toc.block_size != original_toc.block_size
        || saved_toc.compression_methods != original_toc.compression_methods
    {
        return Err(IoStoreError::Package("compression parameters changed"));
    }
    Ok(())
}

/// Assert the saved TOC's perfect-hash state is the one the plan intended:
/// either the table was dropped wholesale (along with the overflow list it
/// serves), or it survived byte-for-byte with the existing overflow entries
/// intact and only the retired indices removed.
///
/// This is checked on the TOC bytes rather than through the reopened reader on
/// purpose. [`IoStoreArchive::find_chunk`] resolves ids with a linear scan, so
/// it would happily confirm every chunk in a container whose hash table no
/// longer addresses them — the failure is invisible until the game mounts it.
fn validate_perfect_hash_result(
    original_toc: &ParsedToc,
    saved_toc: &ParsedToc,
    plan: &TocAppendPlan,
) -> Result<()> {
    if plan.dropped_perfect_hash {
        if !saved_toc.perfect_hash_seeds.is_empty()
            || !saved_toc.chunks_without_perfect_hash.is_empty()
        {
            return Err(IoStoreError::Package(
                "perfect-hash table was not dropped as planned",
            ));
        }
        return Ok(());
    }
    if saved_toc.perfect_hash_seeds != original_toc.perfect_hash_seeds {
        return Err(IoStoreError::Package("perfect-hash seeds changed"));
    }
    let mut expected = toc_overflow_indices(&original_toc.chunks_without_perfect_hash)?;
    expected.retain(|index| !plan.retired_chunk_indices.contains(&(*index as u32)));
    let saved = toc_overflow_indices(&saved_toc.chunks_without_perfect_hash)?;
    if saved.get(..expected.len()) != Some(expected.as_slice()) {
        return Err(IoStoreError::Package(
            "existing perfect-hash overflow changed",
        ));
    }
    Ok(())
}

fn toc_overflow_contains(bytes: &[u8], chunk_index: u32) -> bool {
    bytes
        .chunks_exact(4)
        .any(|raw| i32::from_le_bytes(raw.try_into().unwrap()) == chunk_index as i32)
}

fn validate_cloned_package(
    cloned: &ClonedTagPackage,
    request: &InPlaceTagDuplicate<'_>,
    package_bytes: &[u8],
) -> Result<()> {
    use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};

    let mut cursor = Cursor::new(package_bytes);
    let parsed = FZenPackageHeader::deserialize(
        &mut cursor,
        None,
        CE_TOC_VERSION,
        CE_CONTAINER_HEADER_VERSION,
        None,
    )
    .map_err(|_| IoStoreError::Package("saved cloned .uasset did not parse"))?;
    if parsed.package_name() != request.destination_package_path {
        return Err(IoStoreError::Package(
            "cloned package identity is incorrect",
        ));
    }
    let export = parsed
        .export_map
        .first()
        .ok_or(IoStoreError::Package("saved cloned .uasset has no export"))?;
    if parsed.name_map.get(export.object_name) != cloned.object_name
        || export.public_export_hash != container_id_from_name(&cloned.object_name)
    {
        return Err(IoStoreError::Package("cloned export identity is incorrect"));
    }
    if parsed.bulk_data.first().map(|entry| entry.serial_size)
        != Some(request.tag_bytes.len() as i64)
    {
        return Err(IoStoreError::Package("cloned bulk SerialSize is incorrect"));
    }

    let source = &cloned.source_header;
    if parsed.import_map != source.import_map
        || parsed.imported_packages != source.imported_packages
        || parsed.imported_public_export_hashes != source.imported_public_export_hashes
        || parsed.imported_package_names != source.imported_package_names
        || parsed.shader_map_hashes != source.shader_map_hashes
        || parsed.versioning_info != source.versioning_info
        || parsed.export_bundle_headers != source.export_bundle_headers
        || parsed.export_bundle_entries != source.export_bundle_entries
        || parsed.dependency_bundle_headers != source.dependency_bundle_headers
        || parsed.dependency_bundle_entries != source.dependency_bundle_entries
        || parsed.internal_dependency_arcs != source.internal_dependency_arcs
        || parsed.external_package_dependencies != source.external_package_dependencies
        || parsed.cell_import_map != source.cell_import_map
        || parsed.cell_export_map != source.cell_export_map
        || parsed.is_unversioned != source.is_unversioned
        || parsed.container_header_version != source.container_header_version
        || parsed.summary.has_versioning_info != source.summary.has_versioning_info
        || parsed.summary.package_flags != source.summary.package_flags
        || parsed.summary.cooked_header_size != source.summary.cooked_header_size
        || parsed.summary.source_name != source.summary.source_name
    {
        return Err(IoStoreError::Package("cloned package metadata changed"));
    }

    let mut expected_exports = source.export_map.clone();
    expected_exports[0].object_name = export.object_name;
    expected_exports[0].public_export_hash = export.public_export_hash;
    if parsed.export_map != expected_exports {
        return Err(IoStoreError::Package("cloned export metadata changed"));
    }
    let mut expected_bulk = source.bulk_data.clone();
    expected_bulk[0].serial_size = request.tag_bytes.len() as i64;
    if parsed.bulk_data != expected_bulk {
        return Err(IoStoreError::Package("cloned bulk metadata changed"));
    }

    let source_names = source.name_map.copy_raw_names();
    let parsed_names = parsed.name_map.copy_raw_names();
    if parsed_names.get(..source_names.len()) != Some(source_names.as_slice()) {
        return Err(IoStoreError::Package("cloned name map changed"));
    }
    let header_size = parsed.summary.header_size as usize;
    if header_size > package_bytes.len() || &package_bytes[header_size..] != cloned.export_payload {
        return Err(IoStoreError::Package("cloned export payload changed"));
    }
    let source_payloads = crate::iostore::package::builder::read_payloads(
        source,
        request.source_uasset,
    )
    .map_err(|_| IoStoreError::Package("source export payloads did not parse"))?;
    let parsed_payloads = crate::iostore::package::builder::read_payloads(&parsed, package_bytes)
        .map_err(|_| IoStoreError::Package("cloned export payloads did not parse"))?;
    if parsed_payloads != source_payloads {
        return Err(IoStoreError::Package("cloned export payloads changed"));
    }
    Ok(())
}

/// Write an override container that replaces one tag with `new_tag_bytes`.
///
/// Reuses the original `.ubulk` chunk id from `base`; when the edit changed the
/// tag's size it also overrides the paired `.uasset` with its bulk-data map
/// `SerialSize` patched to the new length (UE reads the tag length from there).
/// The base container is only read, never modified. `out_utoc` should be named
/// with a `_P` suffix (e.g. `mymod-WinGDK_P.utoc`) for patch priority; the
/// sibling `.ucas` and a `.pak` stub are written alongside (the game only mounts
/// containers it discovers via a `Paks/*.pak` scan).
pub fn write_tag_override(
    base: &IoStoreArchive,
    ubulk_path: &str,
    new_tag_bytes: &[u8],
    out_utoc: &std::path::Path,
) -> Result<()> {
    let mut writer = OverrideContainerWriter::new("../../../");
    add_override_to_writer(&mut writer, base, ubulk_path, new_tag_bytes, None)?;
    writer.write(out_utoc)
}

/// Generate an override container that adds a NEW or RENAMED tag package.
///
/// Retargets `template_uasset` to `new_package_path` -- identity *and* group,
/// so the donor need not be of the destination group -- sets the tag content in
/// the `.ubulk`, and writes the container with a ContainerHeader package-store
/// entry so the new package is locatable by the engine. `redirect_from` (the
/// old `/Game/Tags/...` package path) adds a rename redirect so existing
/// references resolve to the renamed tag. All hashing/ids derive from the
/// names; nothing depends on retoc at runtime.
/// A brand-new tag package to add to an override container: a donor `.uasset`
/// to take the package structure from, the new tag `.ubulk` bytes, the target UE
/// package path (`/Game/Tags/<rel>-<group>`), and an optional old→new package
/// redirect for renames.
pub struct NewPackage<'a> {
    /// Any shipped tag's `.uasset`, for its package *structure* only.
    ///
    /// It does not have to be of the destination group. Everything group-shaped
    /// -- the wrapper class, its CDO, the script imports naming them, and the
    /// flag pair -- is derived from `new_package_path`, because the game ships
    /// no tag at all for 38 of the 139 defined groups and those could otherwise
    /// never be created.
    ///
    /// A donor of another group must carry no properties of its own: they are
    /// positional against the donor class's schema and would name different
    /// properties under the destination's. The 47 bare groups all qualify.
    pub template_uasset: &'a [u8],
    pub tag_bytes: &'a [u8],
    pub new_package_path: &'a str,
    pub redirect_from: Option<&'a str>,
    /// What the new tag's `AssetReference` should point at.
    ///
    /// `None` leaves it unset, which is the right default and a change from
    /// what cloning did. The template is a *donor* -- some other tag of the same
    /// group -- and copying its export payload verbatim made a new biped
    /// silently present as whichever biped supplied the template, with that
    /// tag's dependency list attached. Nothing said so.
    pub asset_reference: Option<ImportTarget>,
}

/// Add one same-name override (edited tag, plus the paired `.uasset` with its
/// bulk `SerialSize` patched to the new length) to an in-progress override
/// container writer.
///
/// The `.uasset` rides along even when the edit did not change the tag's length
/// and the patch is a no-op. It costs one small chunk, and it makes the mod
/// self-contained: in-place surgery on a container can only repoint chunks it
/// already has, so a mod that shipped the `.ubulk` alone could never be edited
/// again into a different length.
fn add_override_to_writer(
    w: &mut OverrideContainerWriter,
    archive: &IoStoreArchive,
    ubulk_path: &str,
    new_bytes: &[u8],
    new_uasset: Option<&[u8]>,
) -> Result<()> {
    let ub_id = archive.chunk_id_for(ubulk_path)?;
    let old_len = archive.uncompressed_len(ubulk_path)?;
    let size_changed = new_bytes.len() as u64 != old_len;
    let ua_path = ubulk_path
        .strip_suffix(".ubulk")
        .map(|s| format!("{s}.uasset"))
        .ok_or(IoStoreError::Package("path is not a .ubulk"))?;
    if !archive.contains(&ua_path) {
        // Only fatal when something actually has to be written into it.
        if size_changed || new_uasset.is_some() {
            return Err(IoStoreError::Package(
                "size-changing edit but no paired .uasset to patch",
            ));
        }
    } else {
        let ua_id = archive.chunk_id_for(&ua_path)?;
        // A rebuilt wrapper replaces the shipped one outright. Its bulk-data
        // entry still declares the *original* tag length -- rebuilding changed
        // properties, not the payload it points at -- so the same patch applies
        // to either source.
        let mut ua = match new_uasset {
            Some(bytes) => bytes.to_vec(),
            None => archive.read(&ua_path)?,
        };
        match patch_uasset_serial_size(&mut ua, old_len, new_bytes.len() as u64) {
            Ok(()) => w.add_chunk(ua_id, ua),
            // A same-length edit needs nothing patched into the `.uasset`, so an
            // unrecognised one is not worth failing the export over -- but a
            // rebuilt wrapper still has to ship, or the edit is silently lost.
            Err(e) if size_changed => return Err(e),
            Err(_) => {
                if new_uasset.is_some() {
                    w.add_chunk(ua_id, ua);
                }
            }
        }
    }
    w.add_chunk(ub_id, new_bytes.to_vec());
    Ok(())
}

/// Add one brand-new tag package (retargeting the donor `.uasset` to
/// `new_package_path`'s identity and group, setting the `.ubulk` content, plus
/// an optional redirect) to an in-progress override container writer.
fn add_new_package_to_writer(w: &mut OverrideContainerWriter, pkg: &NewPackage) -> Result<()> {
    use crate::iostore::package::imports::split_tag_package;
    use crate::iostore::package::ue_types::EIoStoreTocVersion;
    use crate::iostore::package::zen::FZenPackageHeader;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
    let cv = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

    // Parse the template package and mutate its identity.
    let mut cur = std::io::Cursor::new(pkg.template_uasset);
    let mut hdr = FZenPackageHeader::deserialize(&mut cur, None, cv, HV, None)
        .map_err(|_| IoStoreError::Package("failed to parse template .uasset"))?;
    if hdr.export_map.is_empty() {
        return Err(IoStoreError::Package("template .uasset has no export"));
    }
    let export_data = pkg.template_uasset[hdr.summary.header_size as usize..].to_vec();
    // Strip what belongs to the donor rather than to the new tag, and set the
    // caller's binding if they gave one. Falls back to the verbatim copy only
    // when the wrapper carries nothing to strip.
    // On a scratch copy: the sanitizer rewrites import slots before it touches
    // the payload, so a mid-way bail that fell back to the verbatim export would
    // ship a header describing imports the payload no longer matches.
    let mut sanitized_hdr = hdr.clone();
    let sanitized = sanitize_donated_export(&mut sanitized_hdr, &export_data, pkg);
    let donor_was_same_group = sanitized.is_ok();
    let export_data = match sanitized {
        Ok(bytes) => {
            hdr = sanitized_hdr;
            bytes
        }
        Err(SanitizeSkip::NothingToStrip) => export_data,
        Err(SanitizeSkip::Failed(e)) => return Err(e),
    };

    // A donor only supplies structure. Everything that makes the wrapper belong
    // to a *group* is derived from the destination path, so a tag can be created
    // in a group the game ships no instance of -- there are 38 such groups, and
    // before this they could not be created at all.
    let Some((_, group)) = split_tag_package(pkg.new_package_path) else {
        return Err(IoStoreError::Package(
            "a new tag package path must be /Game/Tags/<path>-<group>",
        ));
    };
    if !donor_was_same_group {
        // `sanitize_donated_export` decodes against the donor's own class, and
        // declines when that is not the destination group's. So a cross-group
        // donor arrives here unsanitized, and only an *empty* property block is
        // transferable: a present property is indexed against the donor class's
        // schema and would name a different property under the new class.
        //
        // Nothing shipped needs the general case -- every caller creating a tag
        // donates from one of the 47 bare groups -- so this is a guard, not a
        // limitation to work around.
        if !crate::iostore::object::export::export_block_is_empty(&export_data)
            .map_err(|_| IoStoreError::Package("could not read a donated wrapper's property block"))?
        {
            return Err(IoStoreError::Package(
                "a new tag needs a donor of its own group, or one whose wrapper carries no properties",
            ));
        }
        if pkg.asset_reference.is_some() {
            // Applying it would mean encoding against the destination class with
            // a block laid out for the donor's. Failing beats silently dropping
            // the binding the caller asked for.
            return Err(IoStoreError::Package(
                "a new tag's AssetReference needs a donor of its own group",
            ));
        }
    }
    retarget_wrapper_to_group(&mut hdr, group)?;

    let new_obj = pkg
        .new_package_path
        .rsplit('/')
        .next()
        .unwrap_or(pkg.new_package_path);
    hdr.summary.name = hdr.name_map.store(pkg.new_package_path);
    hdr.export_map[0].object_name = hdr.name_map.store(new_obj);
    hdr.export_map[0].public_export_hash = container_id_from_name(new_obj);
    if let Some(entry) = hdr.bulk_data.first_mut() {
        entry.serial_size = pkg.tag_bytes.len() as i64;
    }

    let mut store = StoreEntry::default();
    let mut buf = std::io::Cursor::new(Vec::new());
    hdr.serialize(&mut buf, &mut store, HV)
        .map_err(|_| IoStoreError::Package("failed to serialize .uasset"))?;
    let mut new_uasset = buf.into_inner();
    new_uasset.extend_from_slice(&export_data);

    // Compute new chunk ids from the new package path.
    let new_pid = container_id_from_name(pkg.new_package_path);
    let uasset_id = make_chunk_id(new_pid, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
    let ubulk_id = make_chunk_id(new_pid, 0, CHUNK_TYPE_BULK_DATA);

    w.add_package(uasset_id, new_uasset, FPackageId(new_pid), store);
    w.add_chunk(ubulk_id, pkg.tag_bytes.to_vec());
    if let Some(old) = pkg.redirect_from {
        w.add_redirect(old, FPackageId(new_pid));
    }
    Ok(())
}

/// Point a wrapper at `group`'s tag-data-asset class.
///
/// Four things in a tag `.uasset` say which group it is -- the export's class
/// and its CDO, the script imports naming those two, and the flag pair -- and
/// all four are mechanically derivable from the group long name. Deriving them
/// is what lets any tag donate its structure to any other group; without it a
/// donor had to already be of the destination group, and the game ships no tag
/// at all for 38 of the 139 defined groups.
///
/// Identity (package name, object name, `public_export_hash`, bulk size) is the
/// caller's; this only handles what is group-shaped.
fn retarget_wrapper_to_group(hdr: &mut FZenPackageHeader, group: &str) -> Result<()> {
    use crate::iostore::package::imports::{
        read_import_slots, tag_package_flags, tag_wrapper_cdo_path, tag_wrapper_class_path,
        write_import_slots, ImportSlot, TAG_WRAPPER_MODULE_PATH,
    };

    let class = FPackageObjectIndex::create_script_import(&tag_wrapper_class_path(group));
    let cdo = FPackageObjectIndex::create_script_import(&tag_wrapper_cdo_path(group));
    let module = FPackageObjectIndex::create_script_import(TAG_WRAPPER_MODULE_PATH);
    let (object_flags, package_flags) = tag_package_flags(group);

    let export = hdr
        .export_map
        .first_mut()
        .ok_or(IoStoreError::Package("a tag wrapper has no export"))?;
    export.class_index = class;
    export.template_index = cdo;
    export.object_flags = object_flags;
    hdr.summary.package_flags = package_flags;

    // Rewritten in place rather than rebuilt: an export property names an import
    // by slot index, so reordering the map would silently re-point it. Every
    // shipped tag has exactly these three script imports, in this order.
    let mut slots = read_import_slots(hdr)
        .map_err(|_| IoStoreError::Package("could not read a donated wrapper's import map"))?;
    let mut retargeted = [class, cdo, module].into_iter();
    for slot in slots.iter_mut() {
        if let ImportSlot::Script(index) = slot {
            *index = retargeted.next().ok_or(IoStoreError::Package(
                "a donated wrapper has more than the three script imports a tag carries",
            ))?;
        }
    }
    if retargeted.next().is_some() {
        return Err(IoStoreError::Package(
            "a donated wrapper has fewer than the three script imports a tag carries",
        ));
    }
    write_import_slots(hdr, &slots)
        .map_err(|_| IoStoreError::Package("could not rewrite a donated wrapper's import map"))?;
    Ok(())
}

/// Why a donated export was left as-is.
enum SanitizeSkip {
    /// The wrapper declares nothing that could belong to the donor -- true for
    /// the 47 bare groups, where cloning was always correct.
    NothingToStrip,
    Failed(IoStoreError),
}

/// Remove the donor tag's own bindings from a cloned wrapper, and apply the
/// caller's `AssetReference` if they supplied one.
///
/// `AssetReference` names the Blueprint the tag presents as, and
/// `CookedAssetsReferencedByTag` is the donor's dependency list. Both are
/// properties of the donor, not of the group, so a clone carries them into a
/// tag they say nothing true about.
///
/// `CookedAssetsReferencedByTag` is cleared rather than rebuilt. Deriving the
/// real set means walking the new tag's body for its references, which is a
/// larger job; an empty list is a shape 9,118 shipped tags already have, and
/// resolution is ultimately by name with a group-default fallback, so an absent
/// entry degrades rather than breaks. Rebuilding it properly is the follow-up.
fn sanitize_donated_export(
    hdr: &mut FZenPackageHeader,
    export_data: &[u8],
    pkg: &NewPackage<'_>,
) -> std::result::Result<Vec<u8>, SanitizeSkip> {
    use crate::iostore::object::export::{read_export, write_export};
    use crate::iostore::object::value::PropValue;
    use crate::iostore::package::imports::{
        ImportSlot, read_import_slots, split_tag_package, tag_wrapper_class_path,
        write_import_slots,
    };
    use crate::iostore::usmap::Usmap;

    let Some((_, group)) = split_tag_package(pkg.new_package_path) else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    let class_path = tag_wrapper_class_path(group);
    // Verify against the template rather than trusting the name: a donor of a
    // different group would otherwise be decoded against the wrong schema, which
    // does not error -- it produces plausible values.
    let Some(export_entry) = hdr.export_map.first() else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    if export_entry.class_index != FPackageObjectIndex::create_script_import(&class_path) {
        return Err(SanitizeSkip::NothingToStrip);
    }
    let class = class_path
        .rsplit('.')
        .next()
        .unwrap_or(&class_path)
        .to_owned();

    let Ok(usmap) = Usmap::meteorite() else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    if usmap.get(&class).is_none() {
        return Err(SanitizeSkip::NothingToStrip);
    }

    let payload_len = export_entry.cooked_serial_size as usize;
    let Some(payload) = export_data.get(..payload_len) else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    let trailing = export_data[payload_len.min(export_data.len())..].to_vec();

    let names = hdr.name_map.copy_raw_names();
    let Ok(mut export) = read_export(payload, &names, &usmap, &class, export_entry.object_flags)
    else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    let Some(block) = export.properties_mut() else {
        return Err(SanitizeSkip::NothingToStrip);
    };
    let had_donor_data =
        block.get("AssetReference").is_some() || block.get("CookedAssetsReferencedByTag").is_some();
    if !had_donor_data && pkg.asset_reference.is_none() {
        return Err(SanitizeSkip::NothingToStrip);
    }

    block
        .entries
        .retain(|e| &*e.name != "AssetReference" && &*e.name != "CookedAssetsReferencedByTag");

    // Every package import existed to serve one of those two properties, so with
    // both gone only the script imports (class, CDO, module) are still named.
    // Rebuilding from the surviving slots is what drops the donor's packages
    // from `imported_packages` too, rather than leaving them declared.
    let slots: Vec<ImportSlot> = read_import_slots(hdr)
        .map_err(|_| SanitizeSkip::NothingToStrip)?
        .into_iter()
        .filter(|slot| matches!(slot, ImportSlot::Script(_)))
        .collect();
    write_import_slots(hdr, &slots).map_err(|_| SanitizeSkip::NothingToStrip)?;

    if let Some(target) = pkg.asset_reference.clone() {
        let block = export.properties_mut().expect("checked above");
        // Re-inserted through the same path an edit uses, so the import slot and
        // the property index are produced by one piece of code.
        crate::iostore::object::edit::set_object_property(
            hdr,
            block,
            "AssetReference",
            target,
            &class,
            &usmap,
        )
        .map_err(|e| {
            // A requested binding that cannot be applied must fail loudly. The
            // fallback path ships the donor's wrapper, which is the exact
            // outcome the caller asked to avoid.
            let _ = e;
            SanitizeSkip::Failed(IoStoreError::Package(
                "could not set the requested AssetReference on a new tag",
            ))
        })?;
    }

    let mut out = write_export(&class, &export, &usmap)
        .map_err(|_| SanitizeSkip::Failed(IoStoreError::Package("re-encode donated wrapper")))?;
    hdr.export_map[0].cooked_serial_size = out.len() as u64;
    out.extend_from_slice(&trailing);
    // Any object property still pointing into the import map would now be
    // dangling, since every package slot was dropped.
    if let Some(block) = export.properties() {
        if block
            .entries
            .iter()
            .any(|e| matches!(e.value.unwrapped(), PropValue::Object(i) if *i < 0))
            && pkg.asset_reference.is_none()
        {
            return Err(SanitizeSkip::Failed(IoStoreError::Package(
                "a donated wrapper kept an import reference after sanitizing",
            )));
        }
    }
    Ok(out)
}

pub fn write_new_tag_container(
    template_uasset: &[u8],
    tag_bytes: &[u8],
    new_package_path: &str,
    redirect_from: Option<&str>,
    out_utoc: &std::path::Path,
) -> Result<()> {
    let mut w = OverrideContainerWriter::new("../../../");
    add_new_package_to_writer(
        &mut w,
        &NewPackage {
            template_uasset,
            tag_bytes,
            new_package_path,
            redirect_from,
            asset_reference: None,
        },
    )?;
    w.write(out_utoc)
}

/// One rebuilt package to bundle as a same-name override.
///
/// The difference from a tag override is what is being replaced: that path
/// swaps a tag's `.ubulk` blob and patches the paired `.uasset`'s recorded
/// length, leaving the package structure alone. This replaces the *package* —
/// the Zen header, export map and every export payload — which is what an edit
/// to a reflected property produces, since changing a property can move every
/// export after it.
pub struct PackageOverride<'a> {
    /// The container the package came from, used for its chunk id so the
    /// override lands on the same chunk the base container defines.
    pub archive: &'a IoStoreArchive,
    /// The package's `.uasset` path in that container.
    pub uasset_path: &'a str,
    /// The rebuilt package and its store entry, as returned together by
    /// [`write_package`](crate::iostore::package::builder::write_package).
    pub bytes: Vec<u8>,
    pub store: StoreEntry,
}

fn add_package_override_to_writer(
    writer: &mut OverrideContainerWriter,
    over: &PackageOverride<'_>,
) -> Result<()> {
    use crate::iostore::package::ue_types::EIoStoreTocVersion;
    use crate::iostore::package::zen::FZenPackageHeader;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

    let id = over.archive.chunk_id_for(over.uasset_path)?;
    // The package's own name is the identity the store is keyed by, and it is
    // inside the bytes we are about to write — so take it from there rather
    // than from the path, which is a filename convention.
    let header =
        FZenPackageHeader::deserialize(&mut std::io::Cursor::new(&over.bytes), None, CV, HV, None)
            .map_err(|_| IoStoreError::Package("rebuilt package did not parse"))?;
    let package_id = FPackageId::from_name(&header.package_name());
    // The chunk id came from the base container's path; the store key came from
    // the rebuilt name. If an edit moved the name, those two now disagree, and
    // the result is a container that builds and cannot be read: the engine
    // resolves a name to a package id and that id to a chunk id, so it would
    // compute one nothing in the container serves.
    //
    // `overwrite_packages_in_place_with` has always refused this. The override
    // path silently accepted it, which made renaming a package look like an
    // ordinary property edit right up until the mod did nothing.
    if id.package_id() != package_id.0.to_le_bytes() {
        return Err(IoStoreError::Package(
            "rebuilt package identity changed; renaming a package needs a new chunk id and a \
             container-header entry, not an override of the old one",
        ));
    }
    writer.add_package(id, over.bytes.clone(), package_id, over.store.clone());
    Ok(())
}

/// Bundle rebuilt packages into an override (mod) container.
///
/// The store entry is re-declared rather than inherited, because an edit can
/// change `export_bundles_size` and the base container's entry would then
/// describe the old layout. The override container has higher priority, so its
/// entry is the one the engine uses.
pub fn write_package_mod_container(
    overrides: &[PackageOverride<'_>],
    out_utoc: &std::path::Path,
) -> Result<()> {
    write_combined_mod_container(&[], overrides, &[], out_utoc)
}

/// One rebuilt package to install into its existing source container.
pub struct PackageReplacement<'a> {
    pub uasset_path: &'a str,
    pub rebuilt_bytes: &'a [u8],
    pub store: &'a StoreEntry,
}

/// Overwrite several rebuilt packages inside one source container in a single
/// append/update operation. Every export-bundle chunk and the package-store
/// header are validated after reopening. UCAS writes are append-only; the
/// original UTOC is restored if any package fails validation, which makes the
/// newly appended bytes unreachable.
pub fn overwrite_packages_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &std::path::Path,
    replacements: &[PackageReplacement<'_>],
) -> Result<()> {
    use crate::iostore::package::ue_types::EIoStoreTocVersion;
    use std::io::Cursor;

    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

    if replacements.is_empty() {
        return Ok(());
    }

    let mut packages = Vec::with_capacity(replacements.len());
    let mut seen_chunks = std::collections::BTreeSet::new();
    for replacement in replacements {
        let package_chunk = archive.chunk_index_for(replacement.uasset_path)?;
        if !seen_chunks.insert(package_chunk) {
            return Err(IoStoreError::Package("the same package was replaced twice"));
        }
        let source_id = archive.chunk_id(package_chunk)?;
        if source_id.chunk_type() != CHUNK_TYPE_EXPORT_BUNDLE_DATA {
            return Err(IoStoreError::Package(
                "source path is not an export-bundle package",
            ));
        }
        let rebuilt = FZenPackageHeader::deserialize(
            &mut Cursor::new(replacement.rebuilt_bytes),
            None,
            CV,
            HV,
            None,
        )
        .map_err(|_| IoStoreError::Package("rebuilt package did not parse"))?;
        let package_id = FPackageId::from_name(&rebuilt.package_name());
        if source_id.package_id() != package_id.0.to_le_bytes() {
            return Err(IoStoreError::Package("rebuilt package identity changed"));
        }
        packages.push((package_chunk, source_id, package_id, replacement));
    }

    let header_chunk = (0..archive.chunk_count())
        .find(|&index| {
            archive
                .chunk_id(index)
                .is_ok_and(|id| id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER)
        })
        .ok_or(IoStoreError::Package(
            "container has no writable package-store header",
        ))?;
    let header_id = archive.chunk_id(header_chunk)?;
    let mut header =
        FIoContainerHeader::deserialize(&mut Cursor::new(archive.read_chunk(header_chunk)?), None)
            .map_err(|_| IoStoreError::Package("container package-store header did not parse"))?;
    crate::iostore::compat::check_writable_container_header_version(header.version)
        .map_err(|_| IoStoreError::Package("unsupported container package-store header"))?;
    for (_, _, package_id, replacement) in &packages {
        if header.get_store_entry(*package_id).is_none() {
            return Err(IoStoreError::Package(
                "package is absent from the container store",
            ));
        }
        header.add_package(*package_id, replacement.store.clone());
    }
    let mut serialized = Cursor::new(Vec::new());
    header
        .serialize(&mut serialized)
        .map_err(|_| IoStoreError::Package("container package-store header did not serialize"))?;
    let mut header_bytes = serialized.into_inner();
    header_bytes.resize((header_bytes.len() + 15) & !15, 0);

    let original_toc = std::fs::read(utoc_path)?;
    let mut chunks: Vec<(u32, Vec<u8>)> = packages
        .iter()
        .map(|(chunk, _, _, replacement)| (*chunk, replacement.rebuilt_bytes.to_vec()))
        .collect();
    chunks.push((header_chunk, header_bytes));
    if let Err(error) = overwrite_chunks_in_place(utoc_path, &chunks) {
        std::fs::write(utoc_path, &original_toc)?;
        return Err(error);
    }

    let validation = (|| -> Result<()> {
        let reopened = IoStoreArchive::open(utoc_path)?;
        for (_, source_id, _, replacement) in &packages {
            let package_index = reopened
                .find_chunk(source_id)
                .ok_or(IoStoreError::Package("saved package chunk is absent"))?;
            if reopened.read_chunk(package_index)? != replacement.rebuilt_bytes {
                return Err(IoStoreError::Package(
                    "saved package bytes failed validation",
                ));
            }
        }
        let header_index = reopened
            .find_chunk(&header_id)
            .ok_or(IoStoreError::Package(
                "saved package-store header is absent",
            ))?;
        let saved_header = FIoContainerHeader::deserialize(
            &mut Cursor::new(reopened.read_chunk(header_index)?),
            None,
        )
        .map_err(|_| IoStoreError::Package("saved package-store header did not parse"))?;
        for (_, _, package_id, _) in &packages {
            if saved_header.get_store_entry(*package_id).is_none() {
                return Err(IoStoreError::Package("saved package-store entry is absent"));
            }
        }
        Ok(())
    })();
    if let Err(error) = validation {
        std::fs::write(utoc_path, original_toc)?;
        return Err(error);
    }
    Ok(())
}

/// Overwrite one rebuilt package inside the container that currently supplies
/// it. This compatibility wrapper uses the same transactional batch path.
pub fn overwrite_package_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &std::path::Path,
    uasset_path: &str,
    rebuilt_bytes: &[u8],
    store: &StoreEntry,
) -> Result<()> {
    overwrite_packages_in_place_with(
        archive,
        utoc_path,
        &[PackageReplacement {
            uasset_path,
            rebuilt_bytes,
            store,
        }],
    )
}

/// Bundle several edited tags into ONE override (mod) container — a portable,
/// non-destructive overlay the game loads on top of the base. Each tag is a
/// same-name override: `(source_archive, ubulk_rel_path, new_tag_bytes)`. The
/// paired `.uasset` is included with its bulk size patched when an edit changed
/// the tag's length. Tags may come from different source paks (chunk ids are
/// globally unique). Writes the `.utoc`/`.ucas`/`.pak` triplet at `out_utoc`.
pub fn write_mod_container(
    tags: &[(&IoStoreArchive, &str, &[u8])],
    out_utoc: &std::path::Path,
) -> Result<()> {
    write_mod_container_ex(tags, &[], out_utoc)
}

/// Like [`write_mod_container`] but also bundles brand-new tag packages
/// (`new_packages`) alongside same-name `overrides`, into one override
/// container. Lets a mod contain both edited base-game tags and net-new tags.
pub fn write_mod_container_ex(
    overrides: &[(&IoStoreArchive, &str, &[u8])],
    new_packages: &[NewPackage],
    out_utoc: &std::path::Path,
) -> Result<()> {
    let full: Vec<TagOverride<'_>> = overrides
        .iter()
        .map(|&(archive, ubulk_path, tag_bytes)| TagOverride {
            archive,
            ubulk_path,
            tag_bytes,
            uasset_bytes: None,
        })
        .collect();
    write_mod_container_full(&full, new_packages, out_utoc)
}

/// One tag in an override container: its body, and optionally a rebuilt Unreal
/// wrapper to ship beside it.
///
/// `uasset_bytes` is what makes a *bridge* edit expressible. Without it an
/// override can only replace the `.ubulk`, so repointing a tag's
/// `AssetReference` at a different Blueprint -- a change that lives entirely in
/// the `.uasset` -- had nowhere to go and would export as a no-op.
pub struct TagOverride<'a> {
    pub archive: &'a IoStoreArchive,
    /// The `.ubulk` path in `archive`, which also names the paired `.uasset`.
    pub ubulk_path: &'a str,
    pub tag_bytes: &'a [u8],
    /// A rebuilt package replacing the shipped `.uasset`, or `None` to patch
    /// the shipped one as before.
    pub uasset_bytes: Option<&'a [u8]>,
}

/// As [`write_mod_container_ex`], with per-tag control over the wrapper.
pub fn write_mod_container_full(
    overrides: &[TagOverride<'_>],
    new_packages: &[NewPackage],
    out_utoc: &std::path::Path,
) -> Result<()> {
    write_combined_mod_container(overrides, &[], new_packages, out_utoc)
}

/// Build one mod container from Halo tag overrides, rebuilt ordinary Unreal
/// packages, and new tag packages.
///
/// Baboon's Campaign Evolved project can carry both kinds of edit at once. Two
/// independently written `_P` containers would make their relative priority a
/// filename accident and could not be published as one reviewed mod, so the
/// backend owns the combined operation.
pub fn write_combined_mod_container(
    tag_overrides: &[TagOverride<'_>],
    package_overrides: &[PackageOverride<'_>],
    new_packages: &[NewPackage],
    out_utoc: &std::path::Path,
) -> Result<()> {
    let mut w = OverrideContainerWriter::new("../../../");
    for over in tag_overrides {
        add_override_to_writer(
            &mut w,
            over.archive,
            over.ubulk_path,
            over.tag_bytes,
            over.uasset_bytes,
        )?;
    }
    for over in package_overrides {
        add_package_override_to_writer(&mut w, over)?;
    }
    for pkg in new_packages {
        add_new_package_to_writer(&mut w, pkg)?;
    }
    w.write(out_utoc)
}

/// Overwrite a tag INSIDE an existing container, in place — **destructive**:
/// this modifies the game's own pak files. Appends the edited `.ubulk` (and the
/// paired `.uasset` with its bulk size patched, on a size change) to the `.ucas`
/// and rewrites the `.utoc` to point at the new bytes; the original chunk bytes
/// become dead space. Chunk ids/indices are unchanged, so the perfect-hash
/// tables stay valid and are preserved verbatim. Callers should confirm with the
/// user and recommend a backup first.
pub fn overwrite_tag_in_place(
    utoc_path: &std::path::Path,
    ubulk_rel_path: &str,
    new_tag_bytes: &[u8],
) -> Result<()> {
    // Resolve chunk indices + the paired .uasset via a fresh read-only handle.
    let updates = {
        let archive = IoStoreArchive::open(utoc_path)?;
        plan_tag_overwrite(&archive, ubulk_rel_path, new_tag_bytes)?
        // archive (and its mmap) dropped here, before we touch the files.
    };
    overwrite_chunks_in_place(utoc_path, &updates)
}

/// Like [`overwrite_tag_in_place`], but resolves the paths against an
/// already-open `archive` instead of a fresh handle.
///
/// Required for an override/mod container: it addresses chunks by id and ships
/// no directory index, so a freshly opened handle knows no paths at all and
/// every lookup fails with [`IoStoreError::NotFound`]. Only a handle whose file
/// list was rebuilt by [`IoStoreArchive::recover_entries`] can name the chunks —
/// pass that one here. `archive` must be a handle on `utoc_path`.
pub fn overwrite_tag_in_place_with(
    archive: &IoStoreArchive,
    utoc_path: &std::path::Path,
    ubulk_rel_path: &str,
    new_tag_bytes: &[u8],
) -> Result<()> {
    let updates = plan_tag_overwrite(archive, ubulk_rel_path, new_tag_bytes)?;
    overwrite_chunks_in_place(utoc_path, &updates)
}

/// Resolve what an in-place tag overwrite has to rewrite: the `.ubulk` chunk
/// itself, plus the paired `.uasset` with its bulk size patched when the edit
/// changed the tag's length. Returns `(chunk_index, new_bytes)` pairs ready for
/// [`overwrite_chunks_in_place`].
pub fn plan_tag_overwrite(
    archive: &IoStoreArchive,
    ubulk_rel_path: &str,
    new_tag_bytes: &[u8],
) -> Result<Vec<(u32, Vec<u8>)>> {
    plan_tag_overwrite_with(archive, ubulk_rel_path, new_tag_bytes, None)
}

/// As [`plan_tag_overwrite`], optionally replacing the paired `.uasset` with a
/// rebuilt one -- see [`TagOverride::uasset_bytes`].
pub fn plan_tag_overwrite_with(
    archive: &IoStoreArchive,
    ubulk_rel_path: &str,
    new_tag_bytes: &[u8],
    new_uasset: Option<&[u8]>,
) -> Result<Vec<(u32, Vec<u8>)>> {
    let ub_idx = archive.chunk_index_for(ubulk_rel_path)?;
    let old_len = archive.uncompressed_len(ubulk_rel_path)?;
    let mut updates = vec![(ub_idx, new_tag_bytes.to_vec())];
    if new_tag_bytes.len() as u64 != old_len || new_uasset.is_some() {
        let ua_path = ubulk_rel_path
            .strip_suffix(".ubulk")
            .map(|s| format!("{s}.uasset"))
            .ok_or(IoStoreError::Package("path is not a .ubulk"))?;
        if !archive.contains(&ua_path) {
            return Err(IoStoreError::Package(
                "size-changing edit but no paired .uasset to patch",
            ));
        }
        let ua_idx = archive.chunk_index_for(&ua_path)?;
        let mut ua = match new_uasset {
            Some(bytes) => bytes.to_vec(),
            None => archive.read(&ua_path)?,
        };
        // A rebuilt wrapper whose tag did not change length needs no patch, and
        // an unrecognised bulk entry must not stop it shipping.
        match patch_uasset_serial_size(&mut ua, old_len, new_tag_bytes.len() as u64) {
            Ok(()) => {}
            Err(e) if new_uasset.is_none() => return Err(e),
            Err(_) => {}
        }
        updates.push((ua_idx, ua));
    }
    Ok(updates)
}

/// Core in-place surgery: append each `(chunk_index, new_bytes)` to the `.ucas`
/// and rewrite the `.utoc` to repoint those chunks at the appended data. Every
/// other section (chunk ids, perfect-hash seeds, method names, directory index)
/// is preserved verbatim. The `.utoc` is written atomically (temp + rename).
pub fn overwrite_chunks_in_place(
    utoc_path: &std::path::Path,
    updates: &[(u32, Vec<u8>)],
) -> Result<()> {
    let mut toc = std::fs::read(utoc_path)?;
    if toc.len() < HEADER_SIZE || &toc[..16] != b"-==--==--==--==-" {
        return Err(IoStoreError::BadMagic);
    }
    let rd = |t: &[u8], o: usize| u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
    let entry_count = rd(&toc, 24);
    let cblock_count = rd(&toc, 28);
    let cmeth_count = rd(&toc, 36);
    let cmeth_len = rd(&toc, 40);
    let cbs = rd(&toc, 44) as u64;
    let diridx_size = rd(&toc, 48) as usize;
    let flags = rd(&toc, 80);
    let seeds = rd(&toc, 84);
    let without_hash = rd(&toc, 96);

    let offlen_off = HEADER_SIZE + entry_count as usize * 12;
    let cblock_off =
        offlen_off + entry_count as usize * 10 + seeds as usize * 4 + without_hash as usize * 4;
    let cblocks_end = cblock_off + cblock_count as usize * 12;
    let mut p = cblocks_end + cmeth_count as usize * cmeth_len as usize;
    if flags & 0x04 != 0 {
        let hash_size = i32::from_le_bytes(toc[p..p + 4].try_into().unwrap()) as usize;
        p += 4 + hash_size * 2 + cblock_count as usize * 20;
    }
    let meta_off = p + diridx_size;
    if meta_off + entry_count as usize * 24 > toc.len() {
        return Err(IoStoreError::Truncated("meta section past end of .utoc"));
    }

    // Append the new chunk data to the .ucas and plan the new block entries.
    let ucas_path = utoc_path.with_extension("ucas");
    let mut ucas = std::fs::OpenOptions::new().append(true).open(&ucas_path)?;
    let mut phys = ucas.metadata()?.len();
    let mut appended: Vec<[u8; 12]> = Vec::new();
    for (chunk_index, bytes) in updates {
        if *chunk_index >= entry_count {
            return Err(IoStoreError::Truncated("chunk index out of range"));
        }
        let start_block = cblock_count as usize + appended.len();
        let mut off = 0usize;
        while off < bytes.len() {
            let end = (off + cbs as usize).min(bytes.len());
            let block = &bytes[off..end];
            ucas.write_all(block)?;
            appended.push(encode_block(
                phys,
                block.len() as u32,
                block.len() as u32,
                0,
            ));
            phys += block.len() as u64;
            off = end;
        }
        // Repoint offset/length (logical, block-aligned) + refresh the meta hash.
        let ol = offlen_off + *chunk_index as usize * 10;
        toc[ol..ol + 10].copy_from_slice(&encode_offset_length(
            start_block as u64 * cbs,
            bytes.len() as u64,
        ));
        let m = meta_off + *chunk_index as usize * 24;
        let hash = blake3::hash(bytes);
        toc[m..m + 20].copy_from_slice(&hash.as_bytes()[..20]);
        toc[m + 20] = 0;
        toc[m + 21..m + 24].copy_from_slice(&[0u8; 3]);
    }
    ucas.flush()?;
    drop(ucas);

    // Bump the block count and splice the appended blocks in after the originals.
    let new_block_count = cblock_count + appended.len() as u32;
    toc[28..32].copy_from_slice(&new_block_count.to_le_bytes());
    let mut new_toc = Vec::with_capacity(toc.len() + appended.len() * 12);
    new_toc.extend_from_slice(&toc[..cblocks_end]);
    for b in &appended {
        new_toc.extend_from_slice(b);
    }
    new_toc.extend_from_slice(&toc[cblocks_end..]);

    // Atomic .utoc replace.
    let tmp = utoc_path.with_extension("utoc.tmp");
    std::fs::write(&tmp, &new_toc)?;
    std::fs::rename(&tmp, utoc_path)?;
    Ok(())
}

/// `FIoContainerId::from_name`: CityHash64 of the lowercased container name
/// encoded as UTF-16LE.
pub fn container_id_from_name(name: &str) -> u64 {
    let utf16: Vec<u8> = name
        .to_lowercase()
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    cityhash64(&utf16)
}

// --- CityHash64 (Google CityHash v1.1, the variant UE uses) ---

const K0: u64 = 0xc3a5c85c97cb3127;
const K1: u64 = 0xb492b66fbe98f273;
const K2: u64 = 0x9ae16a3b2f90404f;

fn fetch64(p: &[u8]) -> u64 {
    u64::from_le_bytes(p[..8].try_into().unwrap())
}
fn fetch32(p: &[u8]) -> u64 {
    u32::from_le_bytes(p[..4].try_into().unwrap()) as u64
}
fn rotate(val: u64, shift: u32) -> u64 {
    if shift == 0 {
        val
    } else {
        val.rotate_right(shift)
    }
}
fn shift_mix(val: u64) -> u64 {
    val ^ (val >> 47)
}
fn hash_len16_mul(u: u64, v: u64, mul: u64) -> u64 {
    let mut a = (u ^ v).wrapping_mul(mul);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(mul);
    b ^= b >> 47;
    b = b.wrapping_mul(mul);
    b
}
fn hash_len16(u: u64, v: u64) -> u64 {
    // Hash128to64 with kMul.
    const KMUL: u64 = 0x9ddfea08eb382d69;
    let mut a = (u ^ v).wrapping_mul(KMUL);
    a ^= a >> 47;
    let mut b = (v ^ a).wrapping_mul(KMUL);
    b ^= b >> 47;
    b = b.wrapping_mul(KMUL);
    b
}

fn hash_len0to16(s: &[u8]) -> u64 {
    let len = s.len();
    if len >= 8 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let a = fetch64(s).wrapping_add(K2);
        let b = fetch64(&s[len - 8..]);
        let c = rotate(b, 37).wrapping_mul(mul).wrapping_add(a);
        let d = rotate(a, 25).wrapping_add(b).wrapping_mul(mul);
        return hash_len16_mul(c, d, mul);
    }
    if len >= 4 {
        let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
        let a = fetch32(s);
        return hash_len16_mul(
            (len as u64).wrapping_add(a << 3),
            fetch32(&s[len - 4..]),
            mul,
        );
    }
    if len > 0 {
        let a = s[0] as u64;
        let b = s[len >> 1] as u64;
        let c = s[len - 1] as u64;
        let y = a.wrapping_add(b << 8);
        let z = (len as u64).wrapping_add(c << 2);
        return shift_mix(y.wrapping_mul(K2) ^ z.wrapping_mul(K0)).wrapping_mul(K2);
    }
    K2
}

fn hash_len17to32(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let a = fetch64(s).wrapping_mul(K1);
    let b = fetch64(&s[8..]);
    let c = fetch64(&s[len - 8..]).wrapping_mul(mul);
    let d = fetch64(&s[len - 16..]).wrapping_mul(K2);
    hash_len16_mul(
        rotate(a.wrapping_add(b), 43)
            .wrapping_add(rotate(c, 30))
            .wrapping_add(d),
        a.wrapping_add(rotate(b.wrapping_add(K2), 18))
            .wrapping_add(c),
        mul,
    )
}

fn hash_len33to64(s: &[u8]) -> u64 {
    let len = s.len();
    let mul = K2.wrapping_add((len as u64).wrapping_mul(2));
    let mut a = fetch64(s).wrapping_mul(K2);
    let mut b = fetch64(&s[8..]);
    let c = fetch64(&s[len - 24..]);
    let d = fetch64(&s[len - 32..]);
    let e = fetch64(&s[16..]).wrapping_mul(K2);
    let f = fetch64(&s[24..]).wrapping_mul(9);
    let g = fetch64(&s[len - 8..]);
    let h = fetch64(&s[len - 16..]).wrapping_mul(mul);
    let u =
        rotate(a.wrapping_add(g), 43).wrapping_add(rotate(b, 30).wrapping_add(c).wrapping_mul(9));
    let v = ((a.wrapping_add(g)) ^ d).wrapping_add(f).wrapping_add(1);
    let w = (u.wrapping_add(v).wrapping_mul(mul))
        .swap_bytes()
        .wrapping_add(h);
    let x = rotate(e.wrapping_add(f), 42).wrapping_add(c);
    let y = (w.wrapping_add(v).wrapping_mul(mul))
        .swap_bytes()
        .wrapping_add(g)
        .wrapping_mul(mul);
    let z = e.wrapping_add(f).wrapping_add(c);
    a = ((x.wrapping_add(z)).wrapping_mul(mul).wrapping_add(y))
        .swap_bytes()
        .wrapping_add(b);
    b = shift_mix(
        (z.wrapping_add(a).wrapping_mul(mul))
            .wrapping_add(d)
            .wrapping_add(h),
    )
    .wrapping_mul(mul);
    b.wrapping_add(x)
}

fn weak_hash_len32_with_seeds(s: &[u8], a: u64, b: u64) -> (u64, u64) {
    weak_hash(
        fetch64(s),
        fetch64(&s[8..]),
        fetch64(&s[16..]),
        fetch64(&s[24..]),
        a,
        b,
    )
}
fn weak_hash(w: u64, x: u64, y: u64, z: u64, mut a: u64, mut b: u64) -> (u64, u64) {
    a = a.wrapping_add(w);
    b = rotate(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    a = a.wrapping_add(x);
    a = a.wrapping_add(y);
    b = b.wrapping_add(rotate(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

pub fn cityhash64(s: &[u8]) -> u64 {
    let len = s.len();
    if len <= 32 {
        if len <= 16 {
            return hash_len0to16(s);
        }
        return hash_len17to32(s);
    }
    if len <= 64 {
        return hash_len33to64(s);
    }

    let mut x = fetch64(&s[len - 40..]);
    let mut y = fetch64(&s[len - 16..]).wrapping_add(fetch64(&s[len - 56..]));
    let mut z = hash_len16(
        fetch64(&s[len - 48..]).wrapping_add(len as u64),
        fetch64(&s[len - 24..]),
    );
    let mut v = weak_hash_len32_with_seeds(&s[len - 64..], len as u64, z);
    let mut w = weak_hash_len32_with_seeds(&s[len - 32..], y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(fetch64(s));

    let mut off = 0usize;
    let mut remaining = (len - 1) & !63;
    loop {
        x = rotate(
            x.wrapping_add(y)
                .wrapping_add(v.0)
                .wrapping_add(fetch64(&s[off + 8..])),
            37,
        )
        .wrapping_mul(K1);
        y = rotate(
            y.wrapping_add(v.1).wrapping_add(fetch64(&s[off + 48..])),
            42,
        )
        .wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(fetch64(&s[off + 40..]));
        z = rotate(z.wrapping_add(w.0), 33).wrapping_mul(K1);
        v = weak_hash_len32_with_seeds(&s[off..], v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = weak_hash_len32_with_seeds(
            &s[off + 32..],
            z.wrapping_add(w.1),
            y.wrapping_add(fetch64(&s[off + 16..])),
        );
        std::mem::swap(&mut z, &mut x);
        off += 64;
        remaining -= 64;
        if remaining == 0 {
            break;
        }
    }
    hash_len16(
        hash_len16(v.0, w.0)
            .wrapping_add(shift_mix(y).wrapping_mul(K1))
            .wrapping_add(z),
        hash_len16(v.1, w.1).wrapping_add(x),
    )
}

/// Prepare the one experiment that decides whether renaming in place is worth
/// shipping at all.
///
/// Everything else about a rename can be proved here: the TOC surgery, the
/// tombstones, the directory index, the store entry, the rollback. What cannot
/// be proved here is whether the *game* loads the result — nothing in a pak
/// records how the runtime resolves a package id, and this crate has never read
/// a container redirect back (`lookup_package_redirect` has no callers). So the
/// answer has to come from running the game, and this is the harness that sets
/// that up and says what to look for.
///
/// Runs only against a **copy** of an install, behind its own environment
/// variable, and refuses to run against the same directory `CE_PAKS` names.
/// Every other gated test in this crate reads paks; this one writes to them.
#[cfg(test)]
mod rename_experiment {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn utocs_under(root: &str) -> Vec<PathBuf> {
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(root)
            .expect("read the paks directory")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("utoc"))
            })
            .filter(|path| {
                !path
                    .file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("global.utoc"))
            })
            .collect();
        utocs.sort();
        utocs
    }

    fn header_of(bytes: &[u8]) -> Option<FZenPackageHeader> {
        use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};
        FZenPackageHeader::deserialize(
            &mut Cursor::new(bytes),
            None,
            CE_TOC_VERSION,
            CE_CONTAINER_HEADER_VERSION,
            None,
        )
        .ok()
    }

    /// Which tag packages nothing imports, and which pak each one is in.
    ///
    /// Read-only, and gated on `CE_PAKS` like every other corpus control,
    /// because it only reads. It exists so the experiment does not have to take
    /// whatever candidate happens to sort first: that is in the largest pak, and
    /// restoring a 37 GB pak to undo a one-package edit is a bad trade. Pick a
    /// candidate in a small pak and pass it as `CE_RENAME_PACKAGE`.
    ///
    /// The count is also the answer to a real design question — how much of the
    /// corpus a zero-referrer restriction would leave usable.
    ///
    ///   CE_PAKS=/path/to/Meteorite/Content/Paks \
    ///     cargo test --release --features iostore rename_experiment \
    ///       -- --ignored --nocapture report_packages
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn report_packages_nothing_imports() {
        let root = std::env::var("CE_PAKS").expect("set CE_PAKS to the game's Content/Paks");
        let utocs = utocs_under(&root);
        assert!(!utocs.is_empty(), "no pakchunk .utoc found under {root}");

        // Referrer count plus one example referrer's pak, per imported package.
        // Counts rather than lists, because the corpus has 122k packages and
        // keeping every edge would cost far more than the question is worth.
        let mut imported: std::collections::HashMap<String, (usize, String)> =
            std::collections::HashMap::new();
        let mut tags: Vec<(String, String)> = Vec::new();
        for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else {
                continue;
            };
            let label = utoc
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else {
                    continue;
                };
                let Some(header) = header_of(&bytes) else {
                    continue;
                };
                for import in &header.imported_package_names {
                    let slot = imported
                        .entry(import.to_ascii_lowercase())
                        .or_insert((0, label.clone()));
                    slot.0 += 1;
                }
                if lower.contains("/content/tags/") {
                    tags.push((label.clone(), header.package_name()));
                }
            }
        }

        let mut by_pak: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        // Exactly one referrer is what experiments B and C need: with more than
        // one, a partial failure cannot be told from a partial success.
        let mut single: Vec<(&str, &str, &str)> = Vec::new();
        for (label, name) in &tags {
            match imported.get(&name.to_ascii_lowercase()) {
                None => by_pak.entry(label).or_default().push(name),
                Some((1, referrer)) => single.push((label, name, referrer)),
                Some(_) => {}
            }
        }
        let free: usize = by_pak.values().map(Vec::len).sum();
        let gb_of = |label: &str| {
            std::fs::metadata(
                std::path::Path::new(&root)
                    .join(label)
                    .with_extension("ucas"),
            )
            .map(|meta| meta.len() as f64 / 1024.0 / 1024.0 / 1024.0)
            .unwrap_or(0.0)
        };

        println!("\ntag packages               {}", tags.len());
        println!("with no importer at all    {free}");
        println!("with exactly one importer  {}\n", single.len());
        for (label, names) in &by_pak {
            println!(
                "{label:<28} {:>5} unimported  {:>6.2} GB ucas  e.g. {}",
                names.len(),
                gb_of(label),
                names[0]
            );
        }

        // Sorted so the cheapest pak to restore comes first, and same-pak
        // referrers before cross-pak ones -- B is the experiment to run before
        // C, because B failing makes C moot.
        println!("\nsingle-importer candidates, cheapest pak first:");
        single.sort_by(|a, b| {
            // Same pak before cross-pak, then cheapest to restore first. The
            // tuple compares a label against a label; comparing the package
            // name against it instead silently sorts by nothing at all.
            (a.0 != a.2).cmp(&(b.0 != b.2)).then(
                gb_of(a.0)
                    .partial_cmp(&gb_of(b.0))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        // Both kinds, cheapest of each, rather than the cheapest overall: the
        // two are different experiments and the cost of running them differs by
        // two orders of magnitude, so the choice is the caller's to make.
        for (scope, cross) in [
            ("B, referrer in the same pak", false),
            ("C, referrer elsewhere", true),
        ] {
            println!("\n  experiment {scope}:");
            let mut shown = 0;
            for (label, name, referrer) in single.iter().filter(|(l, _, r)| (l != r) == cross) {
                println!("  {:>6.2} GB  {label:<26} {name}", gb_of(label));
                shown += 1;
                if shown == 5 {
                    break;
                }
            }
            if shown == 0 {
                println!("  (none)");
            }
        }
        assert!(!tags.is_empty(), "no tag package was found under CE_PAKS");
    }

    /// Move one package inside a copied install, and print what to watch for.
    ///
    /// Which experiment this is depends on what you point it at.
    ///
    /// **A — control.** No `CE_RENAME_PACKAGE`; a tag nothing imports is chosen
    /// for you. This isolates the container surgery from the redirect question
    /// entirely: if the level still loads, the TOC rewrite, the tombstones and
    /// the store re-registration are all sound. If A fails, the redirect was
    /// never the problem and the rest of the plan is moot.
    ///
    /// **B and C.** Name a package that something imports. One referrer in the
    /// same pak is B; one in a different pak is C. B passing and C failing is
    /// the signature of per-container redirect scoping, and it is the outcome
    /// most likely to slip past casual testing.
    ///
    /// The decisive observable is not "does it look right" — it is the game's
    /// own log. `LogStreaming` names the `FPackageId` in hex when a package
    /// import fails to resolve, so the old id printed below is what to search
    /// `Meteorite/Saved/Logs/*.log` for after loading a level.
    ///
    ///   CE_RENAME_EXPERIMENT=/path/to/a/COPY/of/Content/Paks \
    ///     cargo test --release --features iostore rename_experiment \
    ///       -- --ignored --nocapture
    #[test]
    #[ignore = "writes to a copied Campaign Evolved install; set CE_RENAME_EXPERIMENT"]
    fn move_one_package_in_a_copied_install() {
        let root = std::env::var("CE_RENAME_EXPERIMENT")
            .expect("set CE_RENAME_EXPERIMENT to a COPY of the game's Content/Paks");
        // The other gated tests read; this one writes. Pointing it at the
        // install every other test measures would corrupt the baseline every
        // later answer is compared against.
        if let Ok(live) = std::env::var("CE_PAKS") {
            let same = std::fs::canonicalize(&root)
                .ok()
                .zip(std::fs::canonicalize(&live).ok())
                .is_some_and(|(a, b)| a == b);
            assert!(
                !same,
                "CE_RENAME_EXPERIMENT points at the same directory as CE_PAKS.\n\
                 Copy the Paks folder first — this test rewrites a .utoc in place."
            );
        }

        let utocs = utocs_under(&root);
        assert!(!utocs.is_empty(), "no pakchunk .utoc found under {root}");

        // One pass over every package, collecting what each one imports. Cheaper
        // than asking "who imports X?" per candidate, and it answers that for
        // every package at once.
        let mut imported: HashSet<String> = HashSet::new();
        let mut referrers_of_target: Vec<(String, String)> = Vec::new();
        let mut tags: Vec<(usize, String)> = Vec::new();
        let mut found: Option<(usize, String)> = None;
        let wanted = std::env::var("CE_RENAME_PACKAGE").ok();
        let wanted_lower = wanted.as_ref().map(|name| name.to_ascii_lowercase());

        for (index, utoc) in utocs.iter().enumerate() {
            let Ok(archive) = IoStoreArchive::open(utoc) else {
                continue;
            };
            let label = utoc
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else {
                    continue;
                };
                let Some(header) = header_of(&bytes) else {
                    continue;
                };
                let name = header.package_name();
                // Located during the same pass that reads it, rather than by a
                // second search afterwards. A named package and a reported
                // candidate then agree by construction — a separate lookup can
                // disagree with the scan about where a package is, and the
                // failure looks like "not in any container" when it is right
                // there.
                if wanted_lower.as_deref() == Some(name.to_ascii_lowercase().as_str()) {
                    found = Some((index, name.clone()));
                }
                for import in &header.imported_package_names {
                    let import = import.to_ascii_lowercase();
                    if wanted_lower.as_deref() == Some(import.as_str()) {
                        referrers_of_target.push((name.clone(), label.clone()));
                    }
                    imported.insert(import);
                }
                if wanted.is_none() && lower.contains("/content/tags/") {
                    tags.push((index, name));
                }
            }
        }

        let (container, package) = match wanted {
            Some(package) => found.unwrap_or_else(|| {
                panic!("no package under {root} is named {package}");
            }),
            None => {
                let free: Vec<&(usize, String)> = tags
                    .iter()
                    .filter(|(_, name)| !imported.contains(&name.to_ascii_lowercase()))
                    .collect();
                println!(
                    "{} of {} tag packages have no importer at all",
                    free.len(),
                    tags.len()
                );
                let chosen = free
                    .first()
                    .expect("no tag package is free of importers; name one with CE_RENAME_PACKAGE");
                (chosen.0, chosen.1.clone())
            }
        };

        let (parent, leaf) = split_package_path(&package);
        let destination = format!("{parent}/baboonmoved-{leaf}");
        let old_id = FPackageId::from_name(&package);
        let new_id = FPackageId::from_name(&destination);
        let utoc = &utocs[container];

        println!("\n--- experiment ---");
        println!("container   {}", utoc.display());
        println!("from        {package}");
        println!("to          {destination}");
        println!("old id      0x{:016X}", old_id.0);
        println!("new id      0x{:016X}", new_id.0);
        println!("referrers   {}", referrers_of_target.len());
        for (referrer, label) in &referrers_of_target {
            let elsewhere = if label
                == &utoc
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            {
                "same pak"
            } else {
                "OTHER PAK — this is experiment C"
            };
            println!("            {referrer}  [{label}, {elsewhere}]");
        }

        let archive = IoStoreArchive::open(utoc).expect("open the container to write");
        rename_package_in_place_with(
            &archive,
            utoc,
            &InPlacePackageRename {
                old_package_path: &package,
                new_package_path: &destination,
                replacement_export_bundle: None,
                replacement_bulk_data: None,
                minimum_appended_index: None,
                redirect: true,
            },
        )
        .expect("the rename itself");
        drop(archive);

        // Reopening proves only that the write is well-formed. It says nothing
        // about the runtime, which is the entire point of the experiment.
        let reopened = IoStoreArchive::open(utoc).expect("reopen after the rename");
        assert!(
            reopened
                .find_chunk(&make_chunk_id(new_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .is_some(),
            "the package is at its new id"
        );
        assert!(
            reopened
                .find_chunk(&make_chunk_id(old_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .is_none(),
            "and no longer at the old one"
        );

        println!("\nwritten. now run the copied install and load a level, then:");
        println!("  grep -i {:016x} Meteorite/Saved/Logs/*.log", old_id.0);
        println!(
            "a hit is the runtime failing to resolve the old id — the redirect did not apply."
        );
        println!("no hit, and the asset renders: the redirect resolved imports.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cityhash_matches_real_container_id() {
        // Ground truth: pakchunk0-WinGDK's TOC header container_id.
        assert_eq!(
            container_id_from_name("pakchunk0-WinGDK"),
            0xfbb7216c3fc8ce45
        );
    }

    #[test]
    fn public_export_hash_is_cityhash_of_name() {
        // A tag export's public_export_hash = CityHash64(lowercased UTF-16 of
        // the object name). Validated against real tag export hashes read from
        // pak0 — exercises the 33..64-byte CityHash path (16/17-32/>64 covered
        // elsewhere). If these hold, tag reference resolution ids are fully
        // computable from names.
        assert_eq!(container_id_from_name("jackal-model"), 0x9595babddd1ed22f); // 24 B
        assert_eq!(
            container_id_from_name("plasma_pistol-weapon"),
            0x368e3d0b13dcbb23
        ); // 40 B
        assert_eq!(
            container_id_from_name("default-sound_combiner"),
            0xb7a53f4d676890ac
        ); // 44 B
    }

    const PAK0: &str =
        "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/pakchunk0-WinGDK.utoc";

    /// Take a real tag's `.ubulk` chunk from pak0, write a same-size override
    /// container reusing its id, then read it back through the reader and
    /// confirm the id and bytes survive. Skipped when the game is absent.
    #[test]
    fn override_container_roundtrip() {
        use crate::iostore::{IoStoreArchive, is_tag_payload};
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");
        let entry = base
            .ublock_entries()
            .find(|e| e.path.ends_with("default-sound_combiner.ubulk"))
            .expect("find tag");
        let id = base.chunk_id(entry.chunk_index).expect("id");
        let mut bytes = base.read(&entry.path).expect("read");
        assert!(is_tag_payload(&bytes));

        // Simulate a same-size edit: flip a byte in the tag body (not header).
        let flip = bytes.len() - 1;
        bytes[flip] ^= 0xff;

        let dir = std::env::temp_dir();
        let utoc = dir.join("blamtags_override_test-WinGDK_P.utoc");
        let mut w = OverrideContainerWriter::new("../../../");
        w.add_chunk(id, bytes.clone());
        w.write(&utoc).expect("write override");

        // Read the override back and confirm it round-trips.
        let over = IoStoreArchive::open(&utoc).expect("open override");
        assert_eq!(over.chunk_id(0).unwrap(), id, "chunk id preserved");
        let got = over.read_chunk(0).expect("read override chunk");
        assert_eq!(got, bytes, "override bytes round-trip");

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    /// A mounted container's `.ucas` is memory-mapped, and Windows will not let a
    /// mapped file be truncated (`ERROR_USER_MAPPED_FILE`, os error 1224) — so a
    /// container this process has open cannot be rewritten until the mapping is
    /// released. Prove the full cycle: read, release, replace the file, map it
    /// again, read what replaced it. Needs no game files.
    #[test]
    fn a_released_partition_can_be_replaced_and_mapped_again() {
        use crate::iostore::{CHUNK_TYPE_BULK_DATA, FIoChunkId, IoStoreArchive, IoStoreError};

        let mut id_bytes = [0u8; 12];
        id_bytes[..8].copy_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
        id_bytes[11] = CHUNK_TYPE_BULK_DATA;
        let id = FIoChunkId(id_bytes);

        let utoc = std::env::temp_dir().join(format!(
            "blamtags_release_test-{}_P.utoc",
            std::process::id()
        ));
        let write_with = |payload: &[u8]| {
            let mut writer = OverrideContainerWriter::new("../../../");
            writer.add_chunk(id, payload.to_vec());
            writer.write(&utoc).expect("write container");
        };

        let first = vec![0xAAu8; 4096];
        write_with(&first);
        let mut archive = IoStoreArchive::open(&utoc).expect("open container");
        assert_eq!(archive.read_chunk(0).expect("read mapped"), first);
        assert!(archive.is_partition_mapped());

        archive.release_partition();
        assert!(!archive.is_partition_mapped());
        assert!(
            matches!(archive.read_chunk(0), Err(IoStoreError::PartitionReleased)),
            "a read while released is refused, not served stale or crashed"
        );

        // What the release is for: the file is replaced underneath the archive.
        // Same layout, so the resident `.utoc` still describes it.
        let second = vec![0x55u8; 4096];
        write_with(&second);

        archive.remap_partition().expect("map again");
        assert!(archive.is_partition_mapped());
        assert_eq!(
            archive.read_chunk(0).expect("read remapped"),
            second,
            "the archive reads what replaced the file, not what it mapped before"
        );
        // Idempotent: a caller that cannot tell whether it released may say so
        // twice.
        archive.remap_partition().expect("remap is idempotent");

        for extension in ["utoc", "ucas", "pak"] {
            let _ = std::fs::remove_file(utoc.with_extension(extension));
        }
    }

    /// The clean-room empty pak stub matches the shipped 339-byte layout: right
    /// size, `FPakInfo` footer (magic, version 11, IndexOffset 0, IndexSize 106),
    /// and — since they hash only the empty sub-index payloads — PathHashIndex /
    /// FullDirectoryIndex hashes identical to the game's own stubs. Needs no game
    /// files.
    #[test]
    fn empty_pak_stub_structure() {
        let b = empty_pak_stub();
        assert_eq!(b.len(), 339, "shipped stub size");

        let magic = 0x5A6F_12E1u32.to_le_bytes();
        let mi = b
            .windows(4)
            .rposition(|w| w == magic)
            .expect("footer magic");
        assert_eq!(&b[mi + 4..mi + 8], &11i32.to_le_bytes(), "PakFileVersion");
        let idx_off = i64::from_le_bytes(b[mi + 8..mi + 16].try_into().unwrap());
        let idx_size = i64::from_le_bytes(b[mi + 16..mi + 24].try_into().unwrap());
        assert_eq!((idx_off, idx_size), (0, 106), "primary index offset/size");
        assert_eq!(b[mi - 17], 0, "bEncryptedIndex = 0 (unencrypted)");

        // Seed-independent sub-index hashes (primary index: PHIHash @0x26,
        // FDIHash @0x4e) — ground truth from the game's shipped level stubs.
        let phi_hash: [u8; 20] = [
            0x05, 0xfe, 0x40, 0x57, 0x53, 0x16, 0x6f, 0x12, 0x55, 0x59, 0xe7, 0xc9, 0xac, 0x55,
            0x86, 0x54, 0xf1, 0x07, 0xc7, 0xe9,
        ];
        let fdi_hash: [u8; 20] = [
            0x90, 0x69, 0xca, 0x78, 0xe7, 0x45, 0x0a, 0x28, 0x51, 0x73, 0x43, 0x1b, 0x3e, 0x52,
            0xc5, 0xc2, 0x52, 0x99, 0xe4, 0x73,
        ];
        assert_eq!(
            &b[0x26..0x3a],
            &phi_hash,
            "PathHashIndex hash matches game stub"
        );
        assert_eq!(
            &b[0x4e..0x62],
            &fdi_hash,
            "FullDirectoryIndex hash matches game stub"
        );
    }

    /// Writing any override drops the discovery `.pak` stub beside the
    /// `.utoc`/`.ucas`, and the stub is the valid empty pak. No game files.
    #[test]
    fn write_emits_pak_stub_sibling() {
        let utoc = std::env::temp_dir().join("blamtags_pakstub-WinGDK_P.utoc");
        let mut w = OverrideContainerWriter::new("../../../");
        w.add_chunk(
            make_chunk_id(0x1234_5678, 0, CHUNK_TYPE_BULK_DATA),
            vec![1, 2, 3, 4],
        );
        w.write(&utoc).expect("write override");

        for ext in ["utoc", "ucas", "pak"] {
            assert!(utoc.with_extension(ext).exists(), "{ext} written");
        }
        let pak = std::fs::read(utoc.with_extension("pak")).expect("read pak");
        assert_eq!(pak, empty_pak_stub(), "sibling is the empty stub");

        for ext in ["utoc", "ucas", "pak"] {
            let _ = std::fs::remove_file(utoc.with_extension(ext));
        }
    }

    #[test]
    fn combined_mod_carries_tag_and_ordinary_package_overrides() {
        use crate::iostore::IoStoreArchive;
        use crate::iostore::package::builder::{read_payloads, write_package};
        use crate::iostore::package::ue_types::EIoStoreTocVersion;
        use crate::iostore::package::zen::FZenPackageHeader;
        use std::io::Cursor;

        const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
        const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
        let tag_package =
            include_bytes!("../../../tests/fixtures/ce/leading-empty.uasset").to_vec();
        let ordinary_package = include_bytes!("../../../tests/fixtures/ce/text.uasset").to_vec();
        let parse = |bytes: &[u8]| {
            let header =
                FZenPackageHeader::deserialize(&mut Cursor::new(bytes), None, CV, HV, None)
                    .expect("parse fixture package");
            let payloads = read_payloads(&header, bytes).expect("fixture payloads");
            let (rebuilt, store) =
                write_package(&header, &payloads, HV).expect("fixture package rebuild");
            assert_eq!(rebuilt, bytes, "fixture must rebuild exactly");
            (header, store)
        };
        let (tag_header, tag_store) = parse(&tag_package);
        let (ordinary_header, ordinary_store) = parse(&ordinary_package);
        let tag_id = FPackageId::from_name(&tag_header.package_name());
        let ordinary_id = FPackageId::from_name(&ordinary_header.package_name());

        let ipeh = i32::from_le_bytes(tag_package[0x18..0x1c].try_into().unwrap()) as usize;
        let old_tag_len =
            u64::from_le_bytes(tag_package[ipeh - 16..ipeh - 8].try_into().unwrap()) as usize;
        let old_tag = vec![0x31; old_tag_len];
        let mut new_tag = old_tag.clone();
        new_tag[old_tag_len / 2] ^= 0xff;

        let base_utoc = std::env::temp_dir().join(format!(
            "blamtags_combined_base_{}.utoc",
            std::process::id()
        ));
        let out_utoc = std::env::temp_dir().join(format!(
            "blamtags_combined_output_{}_P.utoc",
            std::process::id()
        ));
        let mut base_writer = OverrideContainerWriter::new("../../../");
        base_writer.add_package(
            make_chunk_id(tag_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
            tag_package,
            tag_id,
            tag_store,
        );
        base_writer.add_chunk(make_chunk_id(tag_id.0, 0, CHUNK_TYPE_BULK_DATA), old_tag);
        base_writer.add_package(
            make_chunk_id(ordinary_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
            ordinary_package.clone(),
            ordinary_id,
            ordinary_store.clone(),
        );
        base_writer.write(&base_utoc).expect("write synthetic base");

        let mut base = IoStoreArchive::open(&base_utoc).expect("open synthetic base");
        assert!(base.recover_entries(&[], Some("Meteorite/Content/")) >= 3);
        let tag_ubulk = base
            .entries()
            .iter()
            .find(|entry| {
                entry.path.ends_with(".ubulk")
                    && base
                        .chunk_id(entry.chunk_index)
                        .is_ok_and(|id| id.package_id() == tag_id.0.to_le_bytes())
            })
            .expect("tag bulk path")
            .path
            .clone();
        let ordinary_uasset = base
            .entries()
            .iter()
            .find(|entry| {
                entry.path.ends_with(".uasset")
                    && base
                        .chunk_id(entry.chunk_index)
                        .is_ok_and(|id| id.package_id() == ordinary_id.0.to_le_bytes())
            })
            .expect("ordinary package path")
            .path
            .clone();

        write_combined_mod_container(
            &[TagOverride {
                archive: &base,
                ubulk_path: &tag_ubulk,
                tag_bytes: &new_tag,
                uasset_bytes: None,
            }],
            &[PackageOverride {
                archive: &base,
                uasset_path: &ordinary_uasset,
                bytes: ordinary_package.clone(),
                store: ordinary_store,
            }],
            &[],
            &out_utoc,
        )
        .expect("write combined mod");

        let output = IoStoreArchive::open(&out_utoc).expect("open combined mod");
        let tag_chunk = (0..output.chunk_count())
            .find(|&index| {
                output.chunk_id(index).is_ok_and(|id| {
                    id.package_id() == tag_id.0.to_le_bytes()
                        && id.chunk_type() == CHUNK_TYPE_BULK_DATA
                })
            })
            .expect("combined tag chunk");
        let package_chunk = (0..output.chunk_count())
            .find(|&index| {
                output.chunk_id(index).is_ok_and(|id| {
                    id.package_id() == ordinary_id.0.to_le_bytes()
                        && id.chunk_type() == CHUNK_TYPE_EXPORT_BUNDLE_DATA
                })
            })
            .expect("combined package chunk");
        assert_eq!(output.read_chunk(tag_chunk).unwrap(), new_tag);
        assert_eq!(output.read_chunk(package_chunk).unwrap(), ordinary_package);

        for path in [&base_utoc, &out_utoc] {
            for extension in ["utoc", "ucas", "pak"] {
                let _ = std::fs::remove_file(path.with_extension(extension));
            }
        }
    }

    #[test]
    fn package_overwrite_updates_chunk_and_store_entry() {
        use crate::iostore::IoStoreArchive;
        use crate::iostore::package::builder::{read_payloads, write_package};
        use crate::iostore::package::ue_types::EIoStoreTocVersion;
        use crate::iostore::package::zen::FZenPackageHeader;
        use std::io::Cursor;

        const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
        const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
        let original = include_bytes!("../../../tests/fixtures/ce/text.uasset").to_vec();
        let header =
            FZenPackageHeader::deserialize(&mut Cursor::new(&original), None, CV, HV, None)
                .expect("parse fixture package");
        let payloads = read_payloads(&header, &original).expect("read fixture payloads");
        let (_, original_store) =
            write_package(&header, &payloads, HV).expect("rebuild original package");
        let package_id = FPackageId::from_name(&header.package_name());
        let utoc = std::env::temp_dir().join(format!(
            "blamtags_package_inplace_{}.utoc",
            std::process::id()
        ));
        let mut writer = OverrideContainerWriter::new("../../../");
        writer.add_package(
            make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
            original,
            package_id,
            original_store,
        );
        writer
            .write(&utoc)
            .expect("write package fixture container");

        let mut archive = IoStoreArchive::open(&utoc).expect("open package fixture container");
        archive.recover_entries(&[], Some("Meteorite/Content/"));
        let package_path = archive
            .entries()
            .iter()
            .find(|entry| entry.path.ends_with(".uasset"))
            .expect("recovered package path")
            .path
            .clone();
        let mut edited_payloads = payloads;
        edited_payloads
            .last_mut()
            .expect("package export")
            .push(0x5a);
        let (edited, edited_store) =
            write_package(&header, &edited_payloads, HV).expect("write edited package");
        overwrite_package_in_place_with(&archive, &utoc, &package_path, &edited, &edited_store)
            .expect("overwrite package");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen overwritten container");
        let package_chunk = (0..reopened.chunk_count())
            .find(|&index| {
                reopened.chunk_id(index).is_ok_and(|id| {
                    id.package_id() == package_id.0.to_le_bytes()
                        && id.chunk_type() == CHUNK_TYPE_EXPORT_BUNDLE_DATA
                })
            })
            .expect("package chunk");
        assert_eq!(reopened.read_chunk(package_chunk).unwrap(), edited);
        let container_header_chunk = (0..reopened.chunk_count())
            .find(|&index| {
                reopened
                    .chunk_id(index)
                    .is_ok_and(|id| id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER)
            })
            .expect("container header chunk");
        let container_header = FIoContainerHeader::deserialize(
            &mut Cursor::new(reopened.read_chunk(container_header_chunk).unwrap()),
            None,
        )
        .expect("parse updated container header");
        assert!(container_header.get_store_entry(package_id).is_some());

        for extension in ["utoc", "ucas", "pak"] {
            let _ = std::fs::remove_file(utoc.with_extension(extension));
        }
    }

    /// Patch `.uasset` SerialSize to a new length, confirm it changed, patch
    /// back, and confirm byte-identity — on 4 real tags. Also confirm a wrong
    /// `old_len` is rejected (no accidental corruption).
    #[test]
    fn serial_size_patch_roundtrip() {
        use crate::iostore::IoStoreArchive;
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");
        for tag in [
            "default-sound_combiner",
            "default-biped",
            "default-weapon",
            "default-effect",
        ] {
            let ua_path = base
                .entries()
                .iter()
                .find(|e| e.path.ends_with(&format!("{tag}.uasset")))
                .expect("uasset")
                .path
                .clone();
            let ub_path = base
                .entries()
                .iter()
                .find(|e| e.path.ends_with(&format!("{tag}.ubulk")))
                .expect("ubulk")
                .path
                .clone();
            let orig = base.read(&ua_path).expect("read uasset");
            let ub_len = base.read(&ub_path).expect("read ubulk").len() as u64;

            let mut ua = orig.clone();
            let new_len = ub_len + 137;
            patch_uasset_serial_size(&mut ua, ub_len, new_len)
                .unwrap_or_else(|e| panic!("{tag}: patch failed: {e}"));
            assert_ne!(ua, orig, "{tag}: patch should change bytes");

            // Patch back → must restore exactly.
            patch_uasset_serial_size(&mut ua, new_len, ub_len).expect("patch back");
            assert_eq!(ua, orig, "{tag}: patch back should restore original");

            // A wrong old_len must be refused.
            assert!(
                patch_uasset_serial_size(&mut ua, 999_999, 1).is_err(),
                "{tag}: wrong old_len should be rejected"
            );
        }
    }

    /// Full size-CHANGING override: make a real edit that grows the tag
    /// (`add_element`), write the new `.ubulk` + the `SerialSize`-patched
    /// `.uasset` into one override container, read both chunks back, and confirm
    /// the new tag re-parses and the `.uasset` reports the new length.
    #[test]
    fn size_changing_override_roundtrip() {
        use crate::file::TagFile;
        use crate::iostore::{IoStoreArchive, is_tag_payload};
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");

        for tag_name in [
            "default-biped",
            "default-weapon",
            "default-vehicle",
            "default-effect",
        ] {
            let Some(ua_path) = base
                .entries()
                .iter()
                .find(|e| e.path.ends_with(&format!("{tag_name}.uasset")))
                .map(|e| e.path.clone())
            else {
                continue;
            };
            let Some(ub_path) = base
                .entries()
                .iter()
                .find(|e| e.path.ends_with(&format!("{tag_name}.ubulk")))
                .map(|e| e.path.clone())
            else {
                continue;
            };
            let ua_id = base.chunk_id_for(&ua_path).unwrap();
            let ub_id = base.chunk_id_for(&ub_path).unwrap();

            let old_ubulk = base.read(&ub_path).unwrap();
            let mut tag = TagFile::read_from_bytes(&old_ubulk).unwrap();

            // Find a root-level block and grow it by one element.
            let Some(block_name) = tag
                .root()
                .fields()
                .find_map(|f| f.as_block().map(|_| f.name().to_string()))
            else {
                continue;
            };
            {
                let mut root = tag.root_mut();
                let mut field = root.field_mut(&block_name).unwrap();
                let mut block = field.as_block_mut().unwrap();
                block.add_element();
            }
            let new_ubulk = tag.write_to_bytes().unwrap();
            assert_ne!(
                new_ubulk.len(),
                old_ubulk.len(),
                "{tag_name}: edit should change size"
            );
            assert!(is_tag_payload(&new_ubulk));

            // Patch the .uasset to the new length.
            let mut ua = base.read(&ua_path).unwrap();
            patch_uasset_serial_size(&mut ua, old_ubulk.len() as u64, new_ubulk.len() as u64)
                .unwrap_or_else(|e| panic!("{tag_name}: patch failed: {e}"));

            // Write override with BOTH chunks.
            let utoc = std::env::temp_dir().join("blamtags_sizechange-WinGDK_P.utoc");
            let mut w = OverrideContainerWriter::new("../../../");
            w.add_chunk(ua_id, ua.clone());
            w.add_chunk(ub_id, new_ubulk.clone());
            w.write(&utoc).unwrap();

            // Read both back.
            let over = IoStoreArchive::open(&utoc).unwrap();
            assert_eq!(over.chunk_id(0).unwrap(), ua_id);
            assert_eq!(over.chunk_id(1).unwrap(), ub_id);
            let got_ua = over.read_chunk(0).unwrap();
            let got_ub = over.read_chunk(1).unwrap();
            assert_eq!(got_ua, ua, "{tag_name}: patched uasset round-trips");
            assert_eq!(got_ub, new_ubulk, "{tag_name}: new ubulk round-trips");

            // The patched .uasset reports the new length, and the new tag parses.
            let ipeh = i32::from_le_bytes(got_ua[0x18..0x1c].try_into().unwrap()) as usize;
            let ss = u64::from_le_bytes(got_ua[ipeh - 16..ipeh - 8].try_into().unwrap());
            assert_eq!(
                ss,
                new_ubulk.len() as u64,
                "{tag_name}: SerialSize == new length"
            );
            TagFile::read_from_bytes(&got_ub).expect("new tag re-parses");

            eprintln!(
                "{tag_name}: {} -> {} bytes ({:+}), override OK",
                old_ubulk.len(),
                new_ubulk.len(),
                new_ubulk.len() as i64 - old_ubulk.len() as i64
            );
            let _ = std::fs::remove_file(&utoc);
            let _ = std::fs::remove_file(utoc.with_extension("ucas"));
            return; // one success is enough
        }
        panic!("no tag with a root block found to test");
    }

    /// In-place chunk overwrite: append + repoint, other chunks untouched.
    #[test]
    fn overwrite_chunk_in_place_roundtrips() {
        use crate::iostore::IoStoreArchive;
        let utoc = std::env::temp_dir().join("blamtags_inplace_test.utoc");
        let id0 = make_chunk_id(0x1111, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
        let id1 = make_chunk_id(0x1111, 0, CHUNK_TYPE_BULK_DATA);
        let mut w = OverrideContainerWriter::new("../../../");
        w.add_chunk(id0, vec![0xAA; 100]);
        w.add_chunk(id1, vec![0xBB; 200]);
        w.write(&utoc).unwrap();

        // Overwrite chunk 1 with different-size bytes.
        let new_bytes = vec![0xCC; 500];
        overwrite_chunks_in_place(&utoc, &[(1, new_bytes.clone())]).unwrap();

        let a = IoStoreArchive::open(&utoc).unwrap();
        assert_eq!(a.chunk_count(), 2);
        assert_eq!(a.chunk_id(0).unwrap(), id0);
        assert_eq!(a.chunk_id(1).unwrap(), id1);
        assert_eq!(
            a.read_chunk(0).unwrap(),
            vec![0xAA; 100],
            "chunk 0 untouched"
        );
        assert_eq!(a.read_chunk(1).unwrap(), new_bytes, "chunk 1 overwritten");

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    /// Native create/rename: mutate a template `.uasset`'s identity, write a
    /// container with a ContainerHeader + package store entry + redirect, and
    /// confirm it opens with the expected new chunk ids and a valid tag.
    #[test]
    fn native_create_tag_container() {
        use crate::file::TagFile;
        use crate::iostore::IoStoreArchive;
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");
        let ua = base
            .entries()
            .iter()
            .find(|e| e.path.ends_with("default-biped.uasset"))
            .expect("uasset")
            .path
            .clone();
        let ub = base
            .entries()
            .iter()
            .find(|e| e.path.ends_with("default-biped.ubulk"))
            .expect("ubulk")
            .path
            .clone();
        let template = base.read(&ua).unwrap();
        let tag_bytes = base.read(&ub).unwrap();

        let new_pkg = "/Game/Tags/Default/mynewbiped-biped";
        let utoc = std::env::temp_dir().join("blamtags_native_create-WinGDK_P.utoc");
        write_new_tag_container(
            &template,
            &tag_bytes,
            new_pkg,
            Some("/Game/Tags/Default/default-biped"),
            &utoc,
        )
        .expect("write new tag container");

        let c = IoStoreArchive::open(&utoc).expect("open generated");
        assert_eq!(c.chunk_count(), 3, "uasset + ubulk + ContainerHeader");
        let new_pid = container_id_from_name(new_pkg);
        assert_eq!(
            c.chunk_id(0).unwrap(),
            make_chunk_id(new_pid, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA)
        );
        assert_eq!(
            c.chunk_id(1).unwrap(),
            make_chunk_id(new_pid, 0, CHUNK_TYPE_BULK_DATA)
        );
        assert_eq!(
            c.chunk_id(2).unwrap().chunk_type(),
            CHUNK_TYPE_CONTAINER_HEADER
        );
        // The generated .ubulk still parses as a Reach tag.
        TagFile::read_from_bytes(&c.read_chunk(1).unwrap()).expect("ubulk is a tag");

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    /// The one-call `write_tag_override` helper: a size-changing edit produces a
    /// 2-chunk override (patched uasset + new ubulk); read the ubulk back.
    #[test]
    fn write_tag_override_helper_works() {
        use crate::file::TagFile;
        use crate::iostore::IoStoreArchive;
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");
        let ub_path = base
            .entries()
            .iter()
            .find(|e| e.path.ends_with("default-biped.ubulk"))
            .expect("biped")
            .path
            .clone();

        let mut tag = TagFile::read_from_bytes(&base.read(&ub_path).unwrap()).unwrap();
        let block_name = tag
            .root()
            .fields()
            .find_map(|f| f.as_block().map(|_| f.name().to_string()))
            .expect("a block");
        {
            let mut root = tag.root_mut();
            let mut field = root.field_mut(&block_name).unwrap();
            field.as_block_mut().unwrap().add_element();
        }
        let new = tag.write_to_bytes().unwrap();

        let utoc = std::env::temp_dir().join("blamtags_helper-WinGDK_P.utoc");
        write_tag_override(&base, &ub_path, &new, &utoc).expect("write override");

        let over = IoStoreArchive::open(&utoc).unwrap();
        assert_eq!(over.chunk_count(), 2, "uasset + ubulk overridden");
        // Helper adds uasset first, then ubulk.
        assert_eq!(
            over.chunk_id(1).unwrap(),
            base.chunk_id_for(&ub_path).unwrap()
        );
        assert_eq!(over.read_chunk(1).unwrap(), new);

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    fn empty_directory_index_for_test() -> Vec<u8> {
        let mut bytes = encode_directory_fstring("../../../").expect("mount FString");
        bytes.extend_from_slice(&1u32.to_le_bytes()); // root directory
        bytes.extend_from_slice(&[u8::MAX; 16]); // no name/children/files
        bytes.extend_from_slice(&0u32.to_le_bytes()); // files
        bytes.extend_from_slice(&0u32.to_le_bytes()); // strings
        bytes
    }

    fn add_test_perfect_hash_seed(utoc: &std::path::Path, seed: u32) {
        let toc = parse_toc(&std::fs::read(utoc).expect("read test TOC")).expect("parse test TOC");
        assert!(toc.perfect_hash_seeds.is_empty(), "fixture already has seeds");
        let mut header = toc.original[..TOC_HEADER_SIZE].to_vec();
        header[84..88].copy_from_slice(&1u32.to_le_bytes());
        let mut bytes = header;
        for id in &toc.chunk_ids {
            bytes.extend_from_slice(id.bytes());
        }
        for offset_length in &toc.offset_lengths {
            bytes.extend_from_slice(offset_length);
        }
        bytes.extend_from_slice(&seed.to_le_bytes());
        bytes.extend_from_slice(&toc.chunks_without_perfect_hash);
        for block in &toc.blocks {
            bytes.extend_from_slice(block);
        }
        bytes.extend_from_slice(&toc.compression_methods);
        bytes.extend_from_slice(&toc.directory_index);
        for meta in &toc.metas {
            bytes.extend_from_slice(meta);
        }
        bytes.extend_from_slice(&toc.trailing);
        std::fs::write(utoc, bytes).expect("add test perfect-hash seed");
    }

    fn set_test_partition_size(utoc: &std::path::Path, partition_size: u64) {
        let mut bytes = std::fs::read(utoc).expect("read test TOC");
        bytes[88..96].copy_from_slice(&partition_size.to_le_bytes());
        std::fs::write(utoc, bytes).expect("set test partition size");
    }

    fn rewrite_test_directory_index(utoc: &std::path::Path, entries: &[Entry]) {
        let toc = parse_toc(&std::fs::read(utoc).expect("read test TOC")).expect("parse test TOC");
        let directory = serialize_directory_index(&empty_directory_index_for_test(), entries)
            .expect("write directory");
        let mut header = toc.original[..TOC_HEADER_SIZE].to_vec();
        header[48..52].copy_from_slice(&(directory.len() as u32).to_le_bytes());
        let mut bytes = header;
        for id in &toc.chunk_ids {
            bytes.extend_from_slice(id.bytes());
        }
        for offset_length in &toc.offset_lengths {
            bytes.extend_from_slice(offset_length);
        }
        bytes.extend_from_slice(&toc.perfect_hash_seeds);
        bytes.extend_from_slice(&toc.chunks_without_perfect_hash);
        for block in &toc.blocks {
            bytes.extend_from_slice(block);
        }
        bytes.extend_from_slice(&toc.compression_methods);
        bytes.extend_from_slice(&directory);
        for meta in &toc.metas {
            bytes.extend_from_slice(meta);
        }
        bytes.extend_from_slice(&toc.trailing);
        std::fs::write(utoc, bytes).expect("rewrite test directory");
    }

    fn duplicate_fixture(
        name: &str,
        with_header: bool,
        indexed: bool,
    ) -> (
        std::path::PathBuf,
        Vec<u8>,
        FZenPackageHeader,
        String,
        String,
        Vec<u8>,
    ) {
        use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};
        use crate::iostore::package::builder::{read_payloads, write_package};

        let source_uasset =
            include_bytes!("../../../tests/fixtures/ce/leading-empty.uasset").to_vec();
        let source_header = FZenPackageHeader::deserialize(
            &mut Cursor::new(&source_uasset),
            None,
            CE_TOC_VERSION,
            CE_CONTAINER_HEADER_VERSION,
            None,
        )
        .expect("parse source fixture");
        let payloads = read_payloads(&source_header, &source_uasset).expect("read source payloads");
        let (_, source_store) =
            write_package(&source_header, &payloads, CE_CONTAINER_HEADER_VERSION)
                .expect("source store");
        let source_id = FPackageId::from_name(&source_header.package_name());
        let old_bulk = vec![0x31; 73];
        let utoc = std::env::temp_dir().join(format!(
            "blamtags_duplicate_{name}_{}_{}.utoc",
            std::process::id(),
            source_id.0
        ));
        let mut writer = OverrideContainerWriter::new("../../../");
        if with_header {
            writer.add_package(
                make_chunk_id(source_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
                source_uasset.clone(),
                source_id,
                source_store,
            );
        } else {
            writer.add_chunk(
                make_chunk_id(source_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
                source_uasset.clone(),
            );
        }
        writer.add_chunk(
            make_chunk_id(source_id.0, 0, CHUNK_TYPE_BULK_DATA),
            old_bulk.clone(),
        );
        writer.write(&utoc).expect("write duplicate fixture");

        let relative = source_header
            .package_name()
            .strip_prefix("/Game/")
            .expect("fixture package is under /Game/")
            .to_string();
        let old_uasset_path = format!("Meteorite/Content/{relative}.uasset");
        let old_ubulk_path = format!("Meteorite/Content/{relative}.ubulk");
        if indexed {
            add_test_perfect_hash_seed(&utoc, 0x1234_5678);
            rewrite_test_directory_index(
                &utoc,
                &[
                    Entry {
                        path: old_uasset_path.clone(),
                        chunk_index: 0,
                    },
                    Entry {
                        path: old_ubulk_path.clone(),
                        chunk_index: 1,
                    },
                ],
            );
        }
        (
            utoc,
            source_uasset,
            source_header,
            old_uasset_path,
            old_ubulk_path,
            old_bulk,
        )
    }

    fn remove_duplicate_fixture(utoc: &std::path::Path) {
        for extension in ["utoc", "ucas", "pak"] {
            let _ = std::fs::remove_file(utoc.with_extension(extension));
        }
    }

    #[test]
    fn duplicate_tag_indexed_existing_header_preserves_package_and_directory() {
        let (utoc, source_uasset, source_header, old_uasset_path, old_ubulk_path, old_bulk) =
            duplicate_fixture("indexed", true, true);
        let destination_package = "/Game/Tags/Fixture/clone-leading-empty";
        let destination_uasset = "Meteorite/Content/Tags/Fixture/clone-leading-empty.uasset";
        let destination_ubulk = "Meteorite/Content/Tags/Fixture/clone-leading-empty.ubulk";
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x42; 119],
            destination_package_path: destination_package,
            destination_uasset_path: destination_uasset,
            destination_ubulk_path: destination_ubulk,
        };
        let original_toc = std::fs::read(&utoc).expect("original TOC");
        let original = parse_toc(&original_toc).expect("parse original TOC");
        let original_pak = std::fs::read(utoc.with_extension("pak")).expect("original pak");
        let archive = IoStoreArchive::open(&utoc).expect("open indexed fixture");
        assert!(archive.has_directory_index());
        duplicate_tag_in_place_with(&archive, &utoc, &request).expect("duplicate indexed tag");
        assert_eq!(
            std::fs::read(utoc.with_extension("pak")).expect("saved pak"),
            original_pak,
            "same-stem .pak is untouched"
        );

        let reopened = IoStoreArchive::open(&utoc).expect("reopen indexed fixture");
        assert!(reopened.has_directory_index());
        assert_eq!(reopened.read(&old_uasset_path).unwrap(), source_uasset);
        assert_eq!(reopened.read(&old_ubulk_path).unwrap(), old_bulk);
        assert_eq!(
            reopened.read(destination_ubulk).unwrap(),
            vec![0x42; 119],
            "new body is addressable by directory path"
        );
        let new_package = reopened.read(destination_uasset).unwrap();
        let new_header = FZenPackageHeader::deserialize(
            &mut Cursor::new(&new_package),
            None,
            crate::iostore::compat::CE_TOC_VERSION,
            crate::iostore::compat::CE_CONTAINER_HEADER_VERSION,
            None,
        )
        .expect("parse cloned package");
        assert_eq!(new_header.package_name(), destination_package);
        assert_eq!(
            new_header
                .name_map
                .get(new_header.export_map[0].object_name),
            "clone-leading-empty"
        );
        assert_eq!(
            new_header.export_map[0].public_export_hash,
            container_id_from_name("clone-leading-empty")
        );
        assert_eq!(new_header.bulk_data[0].serial_size, 119);
        assert_eq!(new_header.import_map, source_header.import_map);
        assert_eq!(
            new_header.imported_public_export_hashes,
            source_header.imported_public_export_hashes
        );
        assert_eq!(new_header.imported_packages, source_header.imported_packages);
        assert_eq!(new_header.summary.package_flags, source_header.summary.package_flags);
        assert_eq!(new_header.summary.source_name, source_header.summary.source_name);
        let mut expected_exports = source_header.export_map.clone();
        expected_exports[0].object_name = new_header.export_map[0].object_name;
        expected_exports[0].public_export_hash = new_header.export_map[0].public_export_hash;
        assert_eq!(new_header.export_map, expected_exports);
        let mut expected_bulk = source_header.bulk_data.clone();
        expected_bulk[0].serial_size = 119;
        assert_eq!(new_header.bulk_data, expected_bulk);
        assert_eq!(
            &new_package[new_header.summary.header_size as usize..],
            &source_uasset[source_header.summary.header_size as usize..]
        );
        let source_payloads = crate::iostore::package::builder::read_payloads(
            &source_header,
            &source_uasset,
        )
        .expect("read source export payloads");
        let cloned_payloads = crate::iostore::package::builder::read_payloads(
            &new_header,
            &new_package,
        )
        .expect("read cloned export payloads");
        assert_eq!(cloned_payloads, source_payloads);

        let saved = parse_toc(&std::fs::read(&utoc).expect("saved TOC")).expect("parse saved TOC");
        // The fixture ships a live seed, and appending entries changes the
        // modulo base every seeded lookup in the container depends on. There is
        // no seed generator here to rebuild the table with, so it is dropped and
        // the runtime falls back to indexing chunk ids directly.
        assert!(!original.perfect_hash_seeds.is_empty());
        assert!(saved.perfect_hash_seeds.is_empty());
        assert!(saved.chunks_without_perfect_hash.is_empty());
        assert_eq!(
            &saved.blocks[..original.blocks.len()],
            &original.blocks,
            "existing compression blocks are preserved"
        );
        assert_eq!(
            &saved.metas[..original.metas.len() - 1],
            &original.metas[..original.metas.len() - 1],
            "unchanged chunk metadata is preserved"
        );
        assert_eq!(
            &saved.offset_lengths[..original.offset_lengths.len() - 1],
            &original.offset_lengths[..original.offset_lengths.len() - 1],
            "unchanged chunk offsets are preserved"
        );
        assert_eq!(saved.compression_methods, original.compression_methods);
        assert_eq!(
            &saved.chunk_ids[..original.entry_count as usize],
            &original.chunk_ids
        );
        // With the seed table gone the overflow list has nothing left to catch,
        // so the appended chunks resolve through the id index instead — which
        // the `find_chunk` assertions below exercise directly.
        assert!(!toc_overflow_contains(
            &saved.chunks_without_perfect_hash,
            3
        ));
        assert!(!toc_overflow_contains(
            &saved.chunks_without_perfect_hash,
            4
        ));
        let new_package_id = FPackageId::from_name(destination_package);
        assert_eq!(
            reopened
                .find_chunk(&make_chunk_id(
                    new_package_id.0,
                    0,
                    CHUNK_TYPE_EXPORT_BUNDLE_DATA,
                ))
                .map(|index| reopened.read_chunk(index).unwrap()),
            Some(new_package.clone())
        );
        assert_eq!(
            reopened.find_chunk(&make_chunk_id(new_package_id.0, 0, CHUNK_TYPE_BULK_DATA)),
            Some(4)
        );
        let header_index = reopened
            .find_chunk(&make_chunk_id(
                saved.container_id,
                0,
                CHUNK_TYPE_CONTAINER_HEADER,
            ))
            .expect("updated ContainerHeader");
        let container_header = FIoContainerHeader::deserialize(
            &mut Cursor::new(reopened.read_chunk(header_index).unwrap()),
            None,
        )
        .expect("parse updated ContainerHeader");
        assert!(container_header.get_store_entry(new_package_id).is_some());
        remove_duplicate_fixture(&utoc);
    }

    /// Upper-case the first letter of every path segment under `Content/`, the
    /// way the shipped directory index capitalises folders its package names do
    /// not (`Tags/objects/Characters/Marine/` for `/Game/Tags/objects/characters/marine/`).
    fn shift_directory_case(path: &str) -> String {
        let Some(split) = path.find("/Content/") else {
            return path.to_owned();
        };
        let cut = split + "/Content/".len();
        let shifted = path[cut..]
            .split('/')
            .map(|segment| match segment.chars().next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), &segment[first.len_utf8()..]),
                None => segment.to_owned(),
            })
            .collect::<Vec<_>>()
            .join("/");
        format!("{}{shifted}", &path[..cut])
    }

    /// Duplicating into a mod overlay whose entry paths were recovered from the
    /// containers it overrides.
    ///
    /// This is the shape Baboon mounts: an overlay carries no directory index,
    /// so its paths are reconstructed — and when a base container is available
    /// to reconstruct them from, they carry the *base's* casing. The shipped
    /// corpus makes that casing differ from the package name's for 7,534 of its
    /// 12,329 tag packages, so validating the overlay by re-deriving paths from
    /// package names alone cannot find them again.
    ///
    /// `duplicate_tag_indexless_headerless_overlay_recovers_new_paths` cannot
    /// catch this: it hands over an archive whose `entries()` is still empty, so
    /// the old-entry loop has nothing to walk, and its fixture paths are built
    /// from the package name and therefore agree with it by construction.
    #[test]
    fn duplicate_into_indexless_overlay_whose_paths_came_from_a_base() {
        let (overlay_utoc, source_uasset, _header, package_uasset, package_ubulk, old_bulk) =
            duplicate_fixture("case-overlay", false, false);
        let (base_utoc, ..) = duplicate_fixture("case-base", true, true);
        let based_uasset = shift_directory_case(&package_uasset);
        let based_ubulk = shift_directory_case(&package_ubulk);
        assert_ne!(based_uasset, package_uasset, "fixture must diverge in case");
        rewrite_test_directory_index(
            &base_utoc,
            &[
                Entry {
                    path: based_uasset.clone(),
                    chunk_index: 0,
                },
                Entry {
                    path: based_ubulk.clone(),
                    chunk_index: 1,
                },
            ],
        );
        let base = IoStoreArchive::open(&base_utoc).expect("open base fixture");

        let mut overlay = IoStoreArchive::open(&overlay_utoc).expect("open overlay fixture");
        assert!(!overlay.has_directory_index());
        overlay.recover_entries(&[&base], None);
        assert!(
            overlay.entries().iter().any(|e| e.path == based_uasset),
            "the overlay should have taken the base container's casing"
        );

        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x77; 83],
            destination_package_path: "/Game/Tags/Fixture/clone-cased",
            destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-cased.uasset",
            destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-cased.ubulk",
        };
        duplicate_tag_in_place_with(&overlay, &overlay_utoc, &request)
            .expect("duplicate into a recovered overlay");

        let mut reopened = IoStoreArchive::open(&overlay_utoc).expect("reopen overlay");
        reopened.recover_entries(&[&base], None);
        assert_eq!(reopened.read(&based_uasset).unwrap(), source_uasset);
        assert_eq!(reopened.read(&based_ubulk).unwrap(), old_bulk);
        assert_eq!(
            reopened
                .read("Meteorite/Content/Tags/Fixture/clone-cased.ubulk")
                .unwrap(),
            vec![0x77; 83]
        );
        remove_duplicate_fixture(&overlay_utoc);
        remove_duplicate_fixture(&base_utoc);
    }

    #[test]
    fn duplicate_tag_indexless_headerless_overlay_recovers_new_paths() {
        let (utoc, source_uasset, _source_header, old_uasset_path, old_ubulk_path, old_bulk) =
            duplicate_fixture("indexless", false, false);
        let destination_package = "/Game/Tags/Fixture/clone-indexless";
        let destination_uasset = "Meteorite/Content/Tags/Fixture/clone-indexless.uasset";
        let destination_ubulk = "Meteorite/Content/Tags/Fixture/clone-indexless.ubulk";
        let archive = IoStoreArchive::open(&utoc).expect("open indexless fixture");
        assert!(!archive.has_directory_index());
        assert!(archive.entries().is_empty());
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x53; 91],
            destination_package_path: destination_package,
            destination_uasset_path: destination_uasset,
            destination_ubulk_path: destination_ubulk,
        };
        duplicate_tag_in_place_with(&archive, &utoc, &request).expect("duplicate indexless tag");

        let mut reopened = IoStoreArchive::open(&utoc).expect("reopen indexless fixture");
        assert!(!reopened.has_directory_index());
        assert_eq!(reopened.recover_entries(&[], Some("Meteorite/Content/")), 4);
        assert_eq!(reopened.read(&old_uasset_path).unwrap(), source_uasset);
        assert_eq!(reopened.read(&old_ubulk_path).unwrap(), old_bulk);
        assert_eq!(reopened.read(destination_ubulk).unwrap(), vec![0x53; 91]);
        let saved = parse_toc(&std::fs::read(&utoc).expect("saved TOC")).expect("parse saved TOC");
        assert_eq!(saved.entry_count, 5);
        assert!(
            saved
                .chunk_ids
                .iter()
                .any(|id| id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER)
        );
        assert!(toc_overflow_contains(&saved.chunks_without_perfect_hash, 2));
        assert!(toc_overflow_contains(&saved.chunks_without_perfect_hash, 3));
        assert!(toc_overflow_contains(&saved.chunks_without_perfect_hash, 4));
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn duplicate_tag_rejects_bad_paths_stale_handles_and_repeats() {
        let (utoc, source_uasset, _source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("reject", true, true);
        let valid_package = "/Game/Tags/Fixture/clone-reject";
        let valid_uasset = "Meteorite/Content/Tags/Fixture/clone-reject.uasset";
        let valid_ubulk = "Meteorite/Content/Tags/Fixture/clone-reject.ubulk";
        let archive = IoStoreArchive::open(&utoc).expect("open reject fixture");
        let original_toc = std::fs::read(&utoc).expect("read unchanged TOC");
        let original_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("metadata unchanged UCAS")
            .len();
        for (package, uasset, ubulk) in [
            (
                "/Game/Tags/Fixture/../clone-reject",
                valid_uasset,
                valid_ubulk,
            ),
            ("/Game//Tags/Fixture/clone-reject", valid_uasset, valid_ubulk),
            ("/Game/Tags/Fixture/clone-reject", "/Meteorite/Content/Tags/Fixture/clone-reject.uasset", valid_ubulk),
            ("/Game/Tags/Fixture/clone-reject", "Meteorite\\Content\\Tags\\Fixture\\clone-reject.uasset", valid_ubulk),
            (valid_package, "Meteorite/Content/Tags/Fixture/other.uasset", "Meteorite/Content/Tags/Fixture/other.ubulk"),
            (valid_package, "Other/clone-reject.uasset", "Other/clone-reject.ubulk"),
        ] {
            let invalid = InPlaceTagDuplicate {
                source_uasset: &source_uasset,
                tag_bytes: &[1, 2, 3],
                destination_package_path: package,
                destination_uasset_path: uasset,
                destination_ubulk_path: ubulk,
            };
            assert!(duplicate_tag_in_place_with(&archive, &utoc, &invalid).is_err());
            assert_eq!(std::fs::read(&utoc).unwrap(), original_toc);
            assert_eq!(
                std::fs::metadata(utoc.with_extension("ucas"))
                    .unwrap()
                    .len(),
                original_ucas_len
            );
        }

        let mut stale = original_toc.clone();
        stale[140] ^= 1;
        std::fs::write(&utoc, &stale).expect("make stale target");
        let stale_request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[1, 2, 3],
            destination_package_path: valid_package,
            destination_uasset_path: valid_uasset,
            destination_ubulk_path: valid_ubulk,
        };
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &stale_request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), stale);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            original_ucas_len
        );
        std::fs::write(&utoc, &original_toc).expect("restore stale target");

        let archive = IoStoreArchive::open(&utoc).expect("reopen reject fixture");
        duplicate_tag_in_place_with(&archive, &utoc, &stale_request).expect("first duplicate");
        let after_first = std::fs::read(&utoc).expect("read first duplicate");
        let after_first_ucas = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("metadata first duplicate")
            .len();
        let repeated = IoStoreArchive::open(&utoc).expect("open repeated target");
        assert!(duplicate_tag_in_place_with(&repeated, &utoc, &stale_request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), after_first);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            after_first_ucas
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn duplicate_tag_rejects_raw_id_collision_without_directory_collision() {
        let (utoc, source_uasset, source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("raw-id-collision", false, false);
        let destination_package = source_header.package_name();
        let destination_uasset = "Other/Tags/Fixture/leading-empty.uasset";
        let destination_ubulk = "Other/Tags/Fixture/leading-empty.ubulk";
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x29; 17],
            destination_package_path: &destination_package,
            destination_uasset_path: destination_uasset,
            destination_ubulk_path: destination_ubulk,
        };
        let archive = IoStoreArchive::open(&utoc).expect("open raw-id fixture");
        assert!(archive.entries().is_empty());
        let original_toc = std::fs::read(&utoc).expect("original raw-id TOC");
        let original_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("original raw-id UCAS")
            .len();
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), original_toc);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            original_ucas_len
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn duplicate_tag_rejects_malformed_sources_and_unsupported_toc_flags() {
        let (utoc, source_uasset, _source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("malformed", true, true);
        let archive = IoStoreArchive::open(&utoc).expect("open malformed fixture");
        let original = std::fs::read(&utoc).expect("read valid TOC");
        let original_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("metadata valid UCAS")
            .len();
        let malformed_sources: [&[u8]; 3] = [
            &[],
            b"not a Zen package",
            &source_uasset[..source_uasset.len() / 2],
        ];
        for source in malformed_sources {
            let request = InPlaceTagDuplicate {
                source_uasset: source,
                tag_bytes: &[1, 2, 3],
                destination_package_path: "/Game/Tags/Fixture/clone-malformed",
                destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-malformed.uasset",
                destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-malformed.ubulk",
            };
            assert!(duplicate_tag_in_place_with(&archive, &utoc, &request).is_err());
            assert_eq!(std::fs::read(&utoc).unwrap(), original);
            assert_eq!(
                std::fs::metadata(utoc.with_extension("ucas"))
                    .unwrap()
                    .len(),
                original_ucas_len
            );
        }

        let valid_request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[1, 2, 3],
            destination_package_path: "/Game/Tags/Fixture/clone-malformed",
            destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-malformed.uasset",
            destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-malformed.ubulk",
        };
        let mut encrypted = original.clone();
        encrypted[80..84].copy_from_slice(&FLAG_TOC_ENCRYPTED.to_le_bytes());
        assert!(matches!(
            parse_toc(&encrypted),
            Err(IoStoreError::Encrypted)
        ));
        std::fs::write(&utoc, &encrypted).expect("write encrypted test TOC");
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &valid_request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), encrypted);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            original_ucas_len
        );
        std::fs::write(&utoc, &original).expect("restore encrypted test TOC");

        let mut signed = original.clone();
        signed[80..84].copy_from_slice(&FLAG_TOC_SIGNED.to_le_bytes());
        assert!(matches!(
            parse_toc(&signed),
            Err(IoStoreError::Package(
                "signed TOCs are unsupported for in-place duplication"
            ))
        ));
        std::fs::write(&utoc, &signed).expect("write signed test TOC");
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &valid_request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), signed);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            original_ucas_len
        );
        std::fs::write(&utoc, &original).expect("restore signed test TOC");

        let mut multipart = original;
        multipart[52..56].copy_from_slice(&2u32.to_le_bytes());
        assert!(matches!(
            parse_toc(&multipart),
            Err(IoStoreError::MultiPartition(2))
        ));
        std::fs::write(&utoc, &multipart).expect("write multipart test TOC");
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &valid_request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), multipart);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            original_ucas_len
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn duplicate_tag_preserves_finite_partition_size_and_rejects_boundary_crossing() {
        let (utoc, source_uasset, _source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("partition-success", true, true);
        let old_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("metadata finite-partition UCAS")
            .len();
        let finite_partition_size = old_ucas_len + 65_536;
        set_test_partition_size(&utoc, finite_partition_size);
        let original_toc = std::fs::read(&utoc).expect("finite-partition TOC");
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x61; 211],
            destination_package_path: "/Game/Tags/Fixture/clone-partition-success",
            destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-partition-success.uasset",
            destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-partition-success.ubulk",
        };
        let archive = IoStoreArchive::open(&utoc).expect("open finite-partition fixture");
        duplicate_tag_in_place_with(&archive, &utoc, &request).expect("finite partition append");
        let saved = parse_toc(&std::fs::read(&utoc).expect("saved finite-partition TOC"))
            .expect("parse saved finite-partition TOC");
        assert_eq!(saved.partition_size, finite_partition_size);
        assert_eq!(&saved.original[88..96], &original_toc[88..96]);
        remove_duplicate_fixture(&utoc);

        let (utoc, source_uasset, _source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("partition-boundary", true, true);
        let old_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("metadata boundary UCAS")
            .len();
        set_test_partition_size(&utoc, old_ucas_len + 1);
        let original_toc = std::fs::read(&utoc).expect("boundary TOC");
        let archive = IoStoreArchive::open(&utoc).expect("open boundary fixture");
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x62; 17],
            destination_package_path: "/Game/Tags/Fixture/clone-partition-boundary",
            destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-partition-boundary.uasset",
            destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-partition-boundary.ubulk",
        };
        assert!(duplicate_tag_in_place_with(&archive, &utoc, &request).is_err());
        assert_eq!(std::fs::read(&utoc).unwrap(), original_toc);
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len(),
            old_ucas_len
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn duplicate_tag_rolls_back_utoc_after_each_injected_failure_point() {
        let (utoc, source_uasset, _source_header, _old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("rollback", false, false);
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x71; 37],
            destination_package_path: "/Game/Tags/Fixture/clone-rollback",
            destination_uasset_path: "Meteorite/Content/Tags/Fixture/clone-rollback.uasset",
            destination_ubulk_path: "Meteorite/Content/Tags/Fixture/clone-rollback.ubulk",
        };
        let original_toc = std::fs::read(&utoc).expect("original TOC");
        let original_pak = std::fs::read(utoc.with_extension("pak")).expect("original pak");
        let original_ucas_len = std::fs::metadata(utoc.with_extension("ucas"))
            .expect("original UCAS")
            .len();
        let mut archive = IoStoreArchive::open(&utoc).expect("open rollback fixture");
        archive.recover_entries(&[], Some("Meteorite/Content/"));
        for failure in [
            DuplicateFailurePoint::AfterAppend,
            DuplicateFailurePoint::AfterTocWrite,
            DuplicateFailurePoint::BeforeValidation,
        ] {
            assert!(
                duplicate_tag_in_place_with_failure_for_test(&archive, &utoc, &request, failure)
                    .is_err()
            );
            assert_eq!(std::fs::read(&utoc).unwrap(), original_toc);
            assert_eq!(
                std::fs::read(utoc.with_extension("pak")).unwrap(),
                original_pak
            );
            let reopened = IoStoreArchive::open(&utoc).expect("reopen rolled-back target");
            assert_eq!(reopened.chunk_count(), 2);
        }
        assert!(
            std::fs::metadata(utoc.with_extension("ucas"))
                .unwrap()
                .len()
                > original_ucas_len
        );
        remove_duplicate_fixture(&utoc);
    }

    const DELETE_FIXTURE_PACKAGE: &str = "/Game/Tags/Fixture/clone-leading-empty";
    const DELETE_FIXTURE_UASSET: &str = "Meteorite/Content/Tags/Fixture/clone-leading-empty.uasset";
    const DELETE_FIXTURE_UBULK: &str = "Meteorite/Content/Tags/Fixture/clone-leading-empty.ubulk";

    /// Build a fixture and duplicate one tag into it, returning the container
    /// and the TOC as it stood *before* the duplicate — the state a delete is
    /// expected to walk the container back to.
    fn fixture_with_one_duplicate(
        name: &str,
        with_header: bool,
        indexed: bool,
    ) -> (std::path::PathBuf, Vec<u8>, Vec<u8>, String, String) {
        let (utoc, source_uasset, _source_header, old_uasset_path, old_ubulk_path, _old_bulk) =
            duplicate_fixture(name, with_header, indexed);
        let before_duplicate = std::fs::read(&utoc).expect("pre-duplicate TOC");
        let request = InPlaceTagDuplicate {
            source_uasset: &source_uasset,
            tag_bytes: &[0x42; 119],
            destination_package_path: DELETE_FIXTURE_PACKAGE,
            destination_uasset_path: DELETE_FIXTURE_UASSET,
            destination_ubulk_path: DELETE_FIXTURE_UBULK,
        };
        let archive = IoStoreArchive::open(&utoc).expect("open fixture");
        duplicate_tag_in_place_with(&archive, &utoc, &request).expect("duplicate fixture tag");
        (
            utoc,
            before_duplicate,
            source_uasset,
            old_uasset_path,
            old_ubulk_path,
        )
    }

    fn delete_request(minimum: Option<u32>) -> InPlaceTagDeletion<'static> {
        InPlaceTagDeletion {
            package_path: DELETE_FIXTURE_PACKAGE,
            minimum_appended_index: minimum,
            expected_uasset_path: Some(DELETE_FIXTURE_UASSET),
            expected_ubulk_path: Some(DELETE_FIXTURE_UBULK),
        }
    }

    /// Mark one compression block as compressed with a codec that cannot decode
    /// its bytes, so the chunk it belongs to fails to read. A real shipping pak
    /// has chunks this crate's Oodle implementation cannot decode either.
    fn corrupt_test_block_method(utoc: &std::path::Path, block_index: usize) {
        let bytes = std::fs::read(utoc).expect("read test TOC");
        let toc = parse_toc(&bytes).expect("parse test TOC");
        let entries = toc.entry_count as usize;
        let block_off = TOC_HEADER_SIZE
            + entries * 12
            + entries * 10
            + toc.perfect_hash_seeds.len()
            + toc.chunks_without_perfect_hash.len();
        let mut bytes = bytes;
        let field = block_off + block_index * 12 + 8;
        let mut packed = u32::from_le_bytes(bytes[field..field + 4].try_into().unwrap());
        packed = (packed & 0x00ff_ffff) | (1u32 << 24);
        bytes[field..field + 4].copy_from_slice(&packed.to_le_bytes());
        std::fs::write(utoc, bytes).expect("corrupt test block");
    }

    #[test]
    fn duplicating_does_not_require_decoding_chunks_it_never_touches() {
        // A 40 GB shipping pak contains chunks this crate cannot decompress.
        // They have nothing to do with an in-place edit, and re-reading the
        // whole container to "validate" it made every duplicate fail on them.
        let (utoc, source_uasset, _header, old_uasset_path, _old_ubulk_path, _old_bulk) =
            duplicate_fixture("undecodable", true, true);
        corrupt_test_block_method(&utoc, 1);
        {
            let archive = IoStoreArchive::open(&utoc).expect("open corrupted fixture");
            assert!(
                archive.read_chunk(1).is_err(),
                "the fixture must contain a chunk that cannot be decoded"
            );
        }

        let archive = IoStoreArchive::open(&utoc).expect("open corrupted fixture");
        duplicate_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagDuplicate {
                source_uasset: &source_uasset,
                tag_bytes: &[0x42; 119],
                destination_package_path: DELETE_FIXTURE_PACKAGE,
                destination_uasset_path: DELETE_FIXTURE_UASSET,
                destination_ubulk_path: DELETE_FIXTURE_UBULK,
            },
        )
        .expect("an undecodable neighbour must not block a duplicate");

        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        assert_eq!(reopened.read(DELETE_FIXTURE_UBULK).unwrap(), vec![0x42; 119]);
        assert_eq!(reopened.read(&old_uasset_path).unwrap(), source_uasset);
        assert!(
            reopened.read_chunk(1).is_err(),
            "the undecodable chunk is left exactly as it was"
        );

        // And it must still be deletable afterwards.
        let archive = IoStoreArchive::open(&utoc).expect("reopen for delete");
        delete_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagDeletion {
                package_path: DELETE_FIXTURE_PACKAGE,
                minimum_appended_index: None,
                expected_uasset_path: None,
                expected_ubulk_path: None,
            },
        )
        .expect("an undecodable neighbour must not block a delete");
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn delete_tag_retires_the_duplicate_and_leaves_the_originals_readable() {
        let (utoc, before_duplicate, source_uasset, old_uasset_path, old_ubulk_path) =
            fixture_with_one_duplicate("delete-indexed", true, true);
        let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
        let after_duplicate =
            parse_toc(&std::fs::read(&utoc).expect("TOC")).expect("parse duplicated TOC");
        let original_pak = std::fs::read(utoc.with_extension("pak")).expect("pak");
        let package_id = FPackageId::from_name(DELETE_FIXTURE_PACKAGE);

        let archive = IoStoreArchive::open(&utoc).expect("open duplicated fixture");
        delete_tag_in_place_with(&archive, &utoc, &delete_request(Some(before.entry_count)))
            .expect("delete the duplicate");

        let saved = parse_toc(&std::fs::read(&utoc).expect("saved TOC")).expect("parse saved TOC");
        assert_eq!(
            saved.entry_count, after_duplicate.entry_count,
            "retiring a slot must not change the chunk count"
        );
        assert_eq!(
            std::fs::read(utoc.with_extension("pak")).expect("saved pak"),
            original_pak,
            "same-stem .pak is untouched"
        );

        let reopened = IoStoreArchive::open(&utoc).expect("reopen after delete");
        assert!(
            reopened
                .find_chunk(&make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .is_none()
        );
        assert!(
            reopened
                .find_chunk(&make_chunk_id(package_id.0, 0, CHUNK_TYPE_BULK_DATA))
                .is_none()
        );
        assert!(!reopened.contains(DELETE_FIXTURE_UASSET));
        assert!(!reopened.contains(DELETE_FIXTURE_UBULK));
        assert_eq!(reopened.read(&old_uasset_path).unwrap(), source_uasset);
        assert_eq!(reopened.read(&old_ubulk_path).unwrap().len(), 73);

        let header_index = reopened
            .find_chunk(&make_chunk_id(
                saved.container_id,
                0,
                CHUNK_TYPE_CONTAINER_HEADER,
            ))
            .expect("container header survives");
        let header = FIoContainerHeader::deserialize(
            &mut Cursor::new(reopened.read_chunk(header_index).unwrap()),
            None,
        )
        .expect("parse saved header");
        assert!(header.get_store_entry(package_id).is_none());
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn delete_tag_keeps_every_surviving_chunk_on_its_own_index() {
        // The regression test for the whole design: a TOC's perfect hash maps an
        // id to a slot in the chunk-id array, so shrinking the array or moving a
        // chunk would break lookups for chunks nobody asked to touch. Asserted on
        // the raw TOC because `find_chunk` is a linear scan and cannot see it.
        let (utoc, before_duplicate, _source, _uasset_path, _ubulk_path) =
            fixture_with_one_duplicate("delete-stable", true, true);
        let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
        let duplicated_bytes = std::fs::read(&utoc).expect("TOC");
        let duplicated = parse_toc(&duplicated_bytes).expect("parse duplicated TOC");
        let package_id = FPackageId::from_name(DELETE_FIXTURE_PACKAGE);
        let retired: Vec<u32> = [
            make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA),
            make_chunk_id(package_id.0, 0, CHUNK_TYPE_BULK_DATA),
        ]
        .iter()
        .map(|id| {
            duplicated
                .chunk_ids
                .iter()
                .position(|existing| existing == id)
                .expect("duplicate chunk is present") as u32
        })
        .collect();

        let archive = IoStoreArchive::open(&utoc).expect("open duplicated fixture");
        delete_tag_in_place_with(&archive, &utoc, &delete_request(Some(before.entry_count)))
            .expect("delete the duplicate");

        let saved_bytes = std::fs::read(&utoc).expect("saved TOC");
        let saved = parse_toc(&saved_bytes).expect("parse saved TOC");
        assert_eq!(
            saved_bytes[24..28],
            duplicated_bytes[24..28],
            "the header's chunk count is unchanged"
        );
        for (index, id) in duplicated.chunk_ids.iter().enumerate() {
            if retired.contains(&(index as u32)) {
                assert_eq!(saved.chunk_ids[index], retire_chunk_id(*id));
                assert_eq!(saved.offset_lengths[index], [0; 10]);
                assert_eq!(saved.metas[index], [0; TOC_META_SIZE]);
            } else {
                assert_eq!(saved.chunk_ids[index], *id, "chunk {index} moved or changed");
            }
        }
        assert_eq!(
            &saved.blocks[..duplicated.blocks.len()],
            &duplicated.blocks,
            "existing compression blocks are preserved"
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn delete_tag_works_on_an_indexless_headerless_overlay() {
        let (utoc, before_duplicate, _source, _uasset_path, _ubulk_path) =
            fixture_with_one_duplicate("delete-indexless", false, false);
        let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
        let package_id = FPackageId::from_name(DELETE_FIXTURE_PACKAGE);

        let archive = IoStoreArchive::open(&utoc).expect("open duplicated overlay");
        assert!(!archive.has_directory_index());
        // Duplication synthesizes the container header for a headerless overlay,
        // so the chunks being retired are not the last entries — which is the
        // case tail-truncation could never have handled.
        delete_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagDeletion {
                package_path: DELETE_FIXTURE_PACKAGE,
                minimum_appended_index: Some(before.entry_count),
                expected_uasset_path: None,
                expected_ubulk_path: None,
            },
        )
        .expect("delete the duplicate");

        let reopened = IoStoreArchive::open(&utoc).expect("reopen after delete");
        assert!(!reopened.has_directory_index());
        assert!(
            reopened
                .find_chunk(&make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .is_none()
        );
        let saved = parse_toc(&std::fs::read(&utoc).expect("saved TOC")).expect("parse saved TOC");
        assert_eq!(saved.directory_index_size, 0);
        assert!(
            reopened
                .find_chunk(&make_chunk_id(
                    saved.container_id,
                    0,
                    CHUNK_TYPE_CONTAINER_HEADER
                ))
                .is_some(),
            "the synthesized container header is never removed"
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn a_deleted_package_path_can_be_duplicated_again() {
        let (utoc, before_duplicate, source_uasset, _uasset_path, _ubulk_path) =
            fixture_with_one_duplicate("delete-recreate", true, true);
        let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
        let archive = IoStoreArchive::open(&utoc).expect("open duplicated fixture");
        delete_tag_in_place_with(&archive, &utoc, &delete_request(Some(before.entry_count)))
            .expect("delete the duplicate");

        // A retired id must not keep the path reserved: the retired type is one
        // nothing can construct, so re-creating the same tag is not a collision.
        let archive = IoStoreArchive::open(&utoc).expect("reopen after delete");
        duplicate_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagDuplicate {
                source_uasset: &source_uasset,
                tag_bytes: &[0x43; 64],
                destination_package_path: DELETE_FIXTURE_PACKAGE,
                destination_uasset_path: DELETE_FIXTURE_UASSET,
                destination_ubulk_path: DELETE_FIXTURE_UBULK,
            },
        )
        .expect("re-create the deleted tag");

        let reopened = IoStoreArchive::open(&utoc).expect("reopen after re-create");
        assert_eq!(reopened.read(DELETE_FIXTURE_UBULK).unwrap(), vec![0x43; 64]);
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn delete_tag_refuses_shipped_chunks_and_unknown_packages() {
        let (utoc, before_duplicate, _source, _uasset_path, _ubulk_path) =
            fixture_with_one_duplicate("delete-refuses", true, true);
        let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
        let untouched = std::fs::read(&utoc).expect("TOC");
        let ucas_len = std::fs::metadata(utoc.with_extension("ucas")).unwrap().len();
        let archive = IoStoreArchive::open(&utoc).expect("open duplicated fixture");

        // The container's own tag predates anything this caller appended.
        let shipped = InPlaceTagDeletion {
            package_path: "/Game/Fixture/leading-empty",
            minimum_appended_index: Some(before.entry_count),
            expected_uasset_path: None,
            expected_ubulk_path: None,
        };
        assert!(delete_tag_in_place_with(&archive, &utoc, &shipped).is_err());

        // A package that was never in this container at all.
        let absent = InPlaceTagDeletion {
            package_path: "/Game/Tags/Fixture/never-existed",
            minimum_appended_index: None,
            expected_uasset_path: None,
            expected_ubulk_path: None,
        };
        assert!(delete_tag_in_place_with(&archive, &utoc, &absent).is_err());

        // The right package under the wrong path.
        let wrong_path = InPlaceTagDeletion {
            package_path: DELETE_FIXTURE_PACKAGE,
            minimum_appended_index: None,
            expected_uasset_path: Some("Meteorite/Content/Tags/Fixture/elsewhere.uasset"),
            expected_ubulk_path: None,
        };
        assert!(delete_tag_in_place_with(&archive, &utoc, &wrong_path).is_err());

        assert_eq!(
            std::fs::read(&utoc).expect("TOC after refusals"),
            untouched,
            "a refused delete leaves the TOC byte-identical"
        );
        assert_eq!(
            std::fs::metadata(utoc.with_extension("ucas")).unwrap().len(),
            ucas_len,
            "a refused delete does not touch the UCAS"
        );
        remove_duplicate_fixture(&utoc);
    }

    #[test]
    fn delete_tag_rolls_back_utoc_after_each_injected_failure_point() {
        for failure in [
            DuplicateFailurePoint::AfterAppend,
            DuplicateFailurePoint::AfterTocWrite,
            DuplicateFailurePoint::BeforeValidation,
        ] {
            let (utoc, before_duplicate, _source, _uasset_path, _ubulk_path) =
                fixture_with_one_duplicate("delete-rollback", true, true);
            let before = parse_toc(&before_duplicate).expect("parse pre-duplicate TOC");
            let duplicated = std::fs::read(&utoc).expect("TOC");
            let original_pak = std::fs::read(utoc.with_extension("pak")).expect("pak");
            let ucas_len = std::fs::metadata(utoc.with_extension("ucas")).unwrap().len();
            let archive = IoStoreArchive::open(&utoc).expect("open duplicated fixture");

            assert!(
                delete_tag_in_place_with_failure_for_test(
                    &archive,
                    &utoc,
                    &delete_request(Some(before.entry_count)),
                    failure,
                )
                .is_err()
            );
            assert_eq!(
                std::fs::read(&utoc).expect("restored TOC"),
                duplicated,
                "{failure:?} must restore the TOC"
            );
            assert_eq!(
                std::fs::read(utoc.with_extension("pak")).expect("pak"),
                original_pak
            );
            assert!(
                std::fs::metadata(utoc.with_extension("ucas")).unwrap().len() >= ucas_len,
                "the UCAS is only ever appended to"
            );
            let reopened = IoStoreArchive::open(&utoc).expect("reopen after rollback");
            assert!(reopened.contains(DELETE_FIXTURE_UBULK), "{failure:?}");
            remove_duplicate_fixture(&utoc);
        }
    }

    #[test]
    fn retired_chunk_ids_are_distinct_and_cannot_be_constructed() {
        let package_id = FPackageId::from_name(DELETE_FIXTURE_PACKAGE);
        let uasset = make_chunk_id(package_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
        let ubulk = make_chunk_id(package_id.0, 0, CHUNK_TYPE_BULK_DATA);
        let retired_uasset = retire_chunk_id(uasset);
        let retired_ubulk = retire_chunk_id(ubulk);

        assert_ne!(retired_uasset, retired_ubulk, "one package, two live slots");
        assert_eq!(retired_uasset.chunk_type(), RETIRED_CHUNK_TYPE);
        assert_eq!(retired_ubulk.chunk_type(), RETIRED_CHUNK_TYPE);
        for chunk_type in [
            CHUNK_TYPE_EXPORT_BUNDLE_DATA,
            CHUNK_TYPE_BULK_DATA,
            CHUNK_TYPE_CONTAINER_HEADER,
        ] {
            assert_ne!(make_chunk_id(package_id.0, 0, chunk_type), retired_uasset);
            assert_ne!(make_chunk_id(package_id.0, 0, chunk_type), retired_ubulk);
        }
    }

    /// The id's index and type bytes have to survive the move; only the package
    /// half changes. Byte substitution is used rather than `make_chunk_id`
    /// because the two disagree about the index field's byte order, and every
    /// call site passes zero, so nothing has ever settled it.
    #[test]
    fn retargeting_a_chunk_id_moves_only_the_package_half() {
        let old = FIoChunkId([1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xBB, 0xCC, CHUNK_TYPE_BULK_DATA]);
        let moved = retarget_chunk_id(old, FPackageId(0x1122_3344_5566_7788));
        assert_eq!(&moved.0[..8], &0x1122_3344_5566_7788u64.to_le_bytes());
        assert_eq!(&moved.0[8..], &old.0[8..], "index, pad and type are carried");
        assert_eq!(moved.chunk_type(), CHUNK_TYPE_BULK_DATA);

        // At index 0 it agrees with `make_chunk_id`, which is the only shape
        // anything in this crate has ever produced.
        let zeroed = FIoChunkId([9, 9, 9, 9, 9, 9, 9, 9, 0, 0, 0, CHUNK_TYPE_BULK_DATA]);
        assert_eq!(
            retarget_chunk_id(zeroed, FPackageId(7)),
            make_chunk_id(7, 0, CHUNK_TYPE_BULK_DATA)
        );
    }

    /// The extension is carried from the directory index, never rebuilt from the
    /// chunk type: `chunk_type_extension` has no `.umap` arm, so a level would
    /// come back as `.uasset` and stop resolving.
    #[test]
    fn a_renamed_entry_path_keeps_its_extension_and_folder() {
        let rename = |path: &str, old: &str, new: &str| rename_entry_path(path, old, new).unwrap();

        assert_eq!(
            rename(
                "Meteorite/Content/Vehicles/SM_Warthog.uasset",
                "/Game/Vehicles/SM_Warthog",
                "/Game/Vehicles/SM_Scorpion"
            ),
            "Meteorite/Content/Vehicles/SM_Scorpion.uasset"
        );
        // A level keeps `.umap`.
        assert_eq!(
            rename(
                "Meteorite/Content/Levels/C10.umap",
                "/Game/Levels/C10",
                "/Game/Levels/C20"
            ),
            "Meteorite/Content/Levels/C20.umap"
        );
        // A compound extension survives whole.
        assert_eq!(
            rename(
                "Meteorite/Content/A/Mesh.m.ubulk",
                "/Game/A/Mesh",
                "/Game/A/Other"
            ),
            "Meteorite/Content/A/Other.m.ubulk"
        );
        // A folder move rewrites only the tail the package path names, keeping
        // the container's own spelling of the mount prefix.
        assert_eq!(
            rename(
                "Meteorite/Content/Vehicles/SM_Warthog.uasset",
                "/Game/Vehicles/SM_Warthog",
                "/Game/Props/Broken/SM_Warthog"
            ),
            "Meteorite/Content/Props/Broken/SM_Warthog.uasset"
        );
        // A chunk that is not where its package says it is has to be refused
        // rather than guessed at.
        assert!(
            rename_entry_path(
                "Meteorite/Content/Elsewhere/Other.uasset",
                "/Game/Vehicles/SM_Warthog",
                "/Game/Vehicles/SM_Scorpion"
            )
            .is_err()
        );
    }

    fn rename_request<'a>(old: &'a str, new: &'a str) -> InPlacePackageRename<'a> {
        InPlacePackageRename {
            old_package_path: old,
            new_package_path: new,
            replacement_export_bundle: None,
            replacement_bulk_data: None,
            minimum_appended_index: None,
            redirect: true,
        }
    }

    fn container_header_of(archive: &IoStoreArchive) -> FIoContainerHeader {
        let index = (0..archive.chunk_count())
            .find(|index| {
                archive
                    .chunk_id(*index)
                    .is_ok_and(|id| id.chunk_type() == CHUNK_TYPE_CONTAINER_HEADER)
            })
            .expect("the fixture has a container header");
        FIoContainerHeader::deserialize(
            &mut Cursor::new(archive.read_chunk(index).expect("read header")),
            None,
        )
        .expect("container header parses")
    }

    #[test]
    fn rename_moves_every_chunk_and_retires_the_old_slots() {
        let (utoc, _source, source_header, old_uasset_path, old_ubulk_path, old_bulk) =
            duplicate_fixture("rename", true, true);
        let old_package = source_header.package_name();
        let new_package = "/Game/Tags/Fixture/renamed-leading-empty";
        let old_id = FPackageId::from_name(&old_package);
        let new_id = FPackageId::from_name(new_package);

        let original = parse_toc(&std::fs::read(&utoc).expect("read")).expect("parse");
        let archive = IoStoreArchive::open(&utoc).expect("open fixture");
        rename_package_in_place_with(&archive, &utoc, &rename_request(&old_package, new_package))
            .expect("rename");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        // One new slot per member; the old ones stay put, retired.
        assert_eq!(reopened.chunk_count(), original.entry_count + 2);
        for kind in [CHUNK_TYPE_EXPORT_BUNDLE_DATA, CHUNK_TYPE_BULK_DATA] {
            assert!(
                reopened
                    .find_chunk(&make_chunk_id(new_id.0, 0, kind))
                    .is_some(),
                "the new chunk of type {kind} resolves"
            );
            assert!(
                reopened
                    .find_chunk(&make_chunk_id(old_id.0, 0, kind))
                    .is_none(),
                "the old chunk of type {kind} is gone"
            );
        }
        // The payload came across untouched.
        let bulk = reopened
            .find_chunk(&make_chunk_id(new_id.0, 0, CHUNK_TYPE_BULK_DATA))
            .expect("bulk chunk");
        assert_eq!(reopened.read_chunk(bulk).expect("read bulk"), old_bulk);

        // The package now says what it is called, and the index agrees.
        let bundle = reopened
            .find_chunk(&make_chunk_id(new_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
            .expect("bundle chunk");
        let header = FZenPackageHeader::deserialize(
            &mut Cursor::new(reopened.read_chunk(bundle).expect("read bundle")),
            None,
            crate::iostore::compat::CE_TOC_VERSION,
            crate::iostore::compat::CE_CONTAINER_HEADER_VERSION,
            None,
        )
        .expect("the renamed package parses");
        assert_eq!(header.package_name(), new_package);
        let paths: Vec<&str> = reopened.entries().iter().map(|e| e.path.as_str()).collect();
        assert!(
            paths
                .iter()
                .any(|path| path.ends_with("renamed-leading-empty.uasset"))
        );
        assert!(!paths.contains(&old_uasset_path.as_str()));
        assert!(!paths.contains(&old_ubulk_path.as_str()));

        // The store entry moved, and the old name still resolves.
        let container = container_header_of(&reopened);
        assert!(container.get_store_entry(new_id).is_some());
        assert!(container.get_store_entry(old_id).is_none());
        assert_eq!(container.lookup_package_redirect(old_id), Some(new_id));

        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }

    /// Folders come free: `serialize_directory_index` creates intermediate
    /// nodes on demand, so moving into a folder that does not exist makes it.
    /// This is the same mechanism the browser's pending folders rely on.
    #[test]
    fn rename_into_a_new_folder_creates_the_intermediate_directories() {
        let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
            duplicate_fixture("renamefolder", true, true);
        let old_package = source_header.package_name();
        let new_package = "/Game/Tags/Fixture/New/Deeper/moved-leading-empty";

        let archive = IoStoreArchive::open(&utoc).expect("open");
        rename_package_in_place_with(&archive, &utoc, &rename_request(&old_package, new_package))
            .expect("rename into a new folder");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        assert!(
            reopened.entries().iter().any(|entry| entry
                .path
                .replace('\\', "/")
                .contains("Tags/Fixture/New/Deeper/moved-leading-empty.uasset")),
            "the intermediate folders were materialised: {:?}",
            reopened
                .entries()
                .iter()
                .map(|entry| &entry.path)
                .collect::<Vec<_>>()
        );
        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }

    /// Renaming A to B installs a redirect targeting B. Without retargeting,
    /// renaming B to C would leave the first redirect pointing at a package
    /// that is no longer there.
    #[test]
    fn renaming_twice_retargets_the_first_redirect() {
        let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
            duplicate_fixture("renametwice", true, true);
        let first = source_header.package_name();
        let second = "/Game/Tags/Fixture/second-name";
        let third = "/Game/Tags/Fixture/third-name";

        let archive = IoStoreArchive::open(&utoc).expect("open");
        rename_package_in_place_with(&archive, &utoc, &rename_request(&first, second))
            .expect("first rename");
        drop(archive);
        let archive = IoStoreArchive::open(&utoc).expect("reopen");
        rename_package_in_place_with(&archive, &utoc, &rename_request(second, third))
            .expect("second rename");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen twice");
        let container = container_header_of(&reopened);
        let third_id = FPackageId::from_name(third);
        assert_eq!(
            container.lookup_package_redirect(FPackageId::from_name(&first)),
            Some(third_id),
            "the original name follows the whole chain"
        );
        assert_eq!(
            container.lookup_package_redirect(FPackageId::from_name(second)),
            Some(third_id)
        );
        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }

    /// `FPackageId::from_name` lowercases, so a case-only rename produces the
    /// same id, and the addition would collide with its own tombstone.
    #[test]
    fn rename_refuses_a_case_only_change() {
        let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
            duplicate_fixture("renamecase", true, true);
        let old_package = source_header.package_name();
        let shouted = old_package.to_uppercase();
        let archive = IoStoreArchive::open(&utoc).expect("open");
        let error =
            rename_package_in_place_with(&archive, &utoc, &rename_request(&old_package, &shouted))
                .expect_err("a case-only rename is refused");
        assert!(format!("{error}").contains("same id"), "{error}");
        drop(archive);
        remove_duplicate_fixture(&utoc);
    }

    /// Every step of the ladder restores the original `.utoc`. The appended
    /// `.ucas` tail may remain as dead space, because it is unreachable.
    #[test]
    fn rename_rolls_back_after_each_injected_failure_point() {
        for failure in [
            DuplicateFailurePoint::AfterAppend,
            DuplicateFailurePoint::AfterTocWrite,
            DuplicateFailurePoint::BeforeValidation,
        ] {
            let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
                duplicate_fixture(&format!("renameroll{failure:?}"), true, true);
            let old_package = source_header.package_name();
            let before = std::fs::read(&utoc).expect("original TOC");
            let archive = IoStoreArchive::open(&utoc).expect("open");
            let result = rename_package_in_place_with_failure_for_test(
                &archive,
                &utoc,
                &rename_request(&old_package, "/Game/Tags/Fixture/rolled-back"),
                failure,
            );
            drop(archive);
            assert!(result.is_err(), "{failure:?} must fail");
            assert_eq!(
                std::fs::read(&utoc).expect("restored TOC"),
                before,
                "{failure:?} left the TOC as it was"
            );
            // And the container still opens with the package where it started.
            let reopened = IoStoreArchive::open(&utoc).expect("reopen after rollback");
            let old_id = FPackageId::from_name(&old_package);
            assert!(
                reopened
                    .find_chunk(&make_chunk_id(old_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                    .is_some()
            );
            drop(reopened);
            remove_duplicate_fixture(&utoc);
        }
    }

    /// An overlay lists nothing, so paths are inert there and only ids matter.
    #[test]
    fn rename_works_on_an_indexless_container() {
        let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
            duplicate_fixture("renamebare", true, false);
        let old_package = source_header.package_name();
        let new_package = "/Game/Tags/Fixture/bare-renamed";
        let archive = IoStoreArchive::open(&utoc).expect("open");
        assert!(!archive.has_directory_index());
        rename_package_in_place_with(&archive, &utoc, &rename_request(&old_package, new_package))
            .expect("rename an indexless container");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        let new_id = FPackageId::from_name(new_package);
        assert!(
            reopened
                .find_chunk(&make_chunk_id(new_id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .is_some()
        );
        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }

    /// The operation is reversible, which is a good part of what makes it safe
    /// to offer at all.
    #[test]
    fn a_renamed_package_can_be_renamed_back() {
        let (utoc, _source, source_header, old_uasset_path, _old_ubulk, old_bulk) =
            duplicate_fixture("renameback", true, true);
        let old_package = source_header.package_name();
        let moved = "/Game/Tags/Fixture/temporarily-moved";

        let archive = IoStoreArchive::open(&utoc).expect("open");
        rename_package_in_place_with(&archive, &utoc, &rename_request(&old_package, moved))
            .expect("rename out");
        drop(archive);
        let archive = IoStoreArchive::open(&utoc).expect("reopen");
        rename_package_in_place_with(&archive, &utoc, &rename_request(moved, &old_package))
            .expect("rename back");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen twice");
        let old_id = FPackageId::from_name(&old_package);
        let bulk = reopened
            .find_chunk(&make_chunk_id(old_id.0, 0, CHUNK_TYPE_BULK_DATA))
            .expect("the package is back");
        assert_eq!(
            reopened.read_chunk(bulk).expect("read"),
            old_bulk,
            "the payload survived the round trip"
        );
        assert!(
            reopened
                .entries()
                .iter()
                .any(|entry| entry.path == old_uasset_path),
            "and so did its path"
        );
        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }

    /// A package that has been renamed before must still be renamable.
    ///
    /// It was not: the container header populated its localized-source set from
    /// `package_redirects` instead of `localized_packages`, so re-parsing a
    /// container reported every redirect's source as a localized package — and
    /// both rename and delete refuse one of those. A rename installs a redirect,
    /// so the second move of a package was refused, with a message about
    /// localization that had nothing to do with what the caller had done.
    #[test]
    fn a_package_can_be_moved_again_after_a_round_trip_left_redirects_behind() {
        let (utoc, _source, source_header, _old_uasset, _old_ubulk, _bulk) =
            duplicate_fixture("renamerelocalized", true, true);
        let home = source_header.package_name();
        let away = "/Game/Tags/Fixture/away-empty";
        let onward = "/Game/Tags/Fixture/onward-empty";

        for (from, to) in [(home.as_str(), away), (away, home.as_str())] {
            let archive = IoStoreArchive::open(&utoc).expect("open");
            rename_package_in_place_with(&archive, &utoc, &rename_request(from, to))
                .expect("the round trip itself works");
            drop(archive);
        }
        // Redirects now name both paths as sources; neither is localized.
        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        let header = container_header_of(&reopened);
        assert!(!header.is_localized_source(FPackageId::from_name(&home)));
        assert!(!header.is_localized_source(FPackageId::from_name(away)));
        drop(reopened);

        let archive = IoStoreArchive::open(&utoc).expect("reopen");
        rename_package_in_place_with(&archive, &utoc, &rename_request(&home, onward))
            .expect("and the package can still be moved on");
        drop(archive);
        remove_duplicate_fixture(&utoc);
    }

    /// The group half of the leaf names the wrapper's native class, which no
    /// package path can redefine — so this is refused rather than written and
    /// discovered at load.
    #[test]
    fn a_tag_cannot_be_renamed_into_a_different_group() {
        let (utoc, _source, source_header, _uasset, _ubulk, _bulk) =
            duplicate_fixture("renametaggroup", true, true);
        let tag_package = source_header.package_name();
        let (stem, group) = tag_package.rsplit_once('-').expect("the fixture is a tag");
        assert_ne!(group, "biped");
        let other_group = format!("{stem}-biped");
        let before = std::fs::read(&utoc).expect("TOC before");

        let archive = IoStoreArchive::open(&utoc).expect("open");
        let error = rename_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagRename {
                old_package_path: &tag_package,
                new_package_path: &other_group,
                tag_bytes: None,
                minimum_appended_index: None,
                redirect: true,
            },
        )
        .expect_err("a group change is refused");
        assert!(format!("{error}").contains("different group"), "{error}");
        drop(archive);
        assert_eq!(
            std::fs::read(&utoc).expect("TOC after"),
            before,
            "and nothing was written"
        );

        // The same move within the group is allowed, so the refusal is about
        // the group and not about the rename.
        let within_group = format!("/Game/Tags/Fixture/Elsewhere/renamed-{group}");
        let archive = IoStoreArchive::open(&utoc).expect("reopen");
        rename_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagRename {
                old_package_path: &tag_package,
                new_package_path: &within_group,
                tag_bytes: None,
                minimum_appended_index: None,
                redirect: true,
            },
        )
        .expect("a rename within the group is a plain move");
        drop(archive);
        remove_duplicate_fixture(&utoc);
    }

    /// A package outside the tag layout has no group to preserve, so the
    /// wrapper cannot speak for it — in either direction. The general primitive
    /// still can, which is why this refuses rather than falling back to it.
    #[test]
    fn a_package_outside_the_tag_layout_is_not_a_tag_rename() {
        let (utoc, _source, source_header, _uasset, _ubulk, _bulk) =
            duplicate_fixture("renamenontag", true, true);
        let tag_package = source_header.package_name();
        let archive = IoStoreArchive::open(&utoc).expect("open");

        for (old, new) in [
            ("/Game/Meshes/SM_Warthog", "/Game/Meshes/SM_Scorpion"),
            (tag_package.as_str(), "/Game/Meshes/SM_Scorpion"),
            ("/Game/Tags/Fixture/no-extension-here/", tag_package.as_str()),
        ] {
            let error = rename_tag_in_place_with(
                &archive,
                &utoc,
                &InPlaceTagRename {
                    old_package_path: old,
                    new_package_path: new,
                    tag_bytes: None,
                    minimum_appended_index: None,
                    redirect: true,
                },
            )
            .expect_err("the wrapper refuses a non-tag path");
            // Refused on the path alone, before the container is consulted.
            assert!(format!("{error}").contains("/Game/Tags/"), "{error}");
        }
        drop(archive);
        remove_duplicate_fixture(&utoc);
    }

    /// Renaming a tag whose body was edited is one transaction, not a save
    /// followed by a move. The body's length lives in the package header, so
    /// the two have to move together or the chunk reads short.
    #[test]
    fn renaming_a_tag_installs_an_edited_body_in_the_same_transaction() {
        use crate::iostore::compat::{CE_CONTAINER_HEADER_VERSION, CE_TOC_VERSION};

        let (utoc, _source, source_header, _uasset, _ubulk, old_bulk) =
            duplicate_fixture("renametagbody", true, true);
        let tag_package = source_header.package_name();
        let group = tag_package.rsplit_once('-').expect("the fixture is a tag").1;
        let new_package = format!("/Game/Tags/Fixture/edited-{group}");
        let new_package = new_package.as_str();
        let new_body = vec![0x5au8; old_bulk.len() + 41];

        let serial_size_of = |archive: &IoStoreArchive, package: &str| -> i64 {
            let id = FPackageId::from_name(package);
            let index = archive
                .find_chunk(&make_chunk_id(id.0, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA))
                .expect("the package has an export bundle chunk");
            let bytes = archive.read_chunk(index).expect("read the bundle");
            let header = FZenPackageHeader::deserialize(
                &mut Cursor::new(&bytes),
                None,
                CE_TOC_VERSION,
                CE_CONTAINER_HEADER_VERSION,
                None,
            )
            .expect("the bundle parses");
            assert_eq!(header.bulk_data.len(), 1);
            header.bulk_data[0].serial_size
        };

        let archive = IoStoreArchive::open(&utoc).expect("open");
        let before = serial_size_of(&archive, &tag_package);
        rename_tag_in_place_with(
            &archive,
            &utoc,
            &InPlaceTagRename {
                old_package_path: &tag_package,
                new_package_path: new_package,
                tag_bytes: Some(&new_body),
                minimum_appended_index: None,
                redirect: true,
            },
        )
        .expect("rename and install the edited body");
        drop(archive);

        let reopened = IoStoreArchive::open(&utoc).expect("reopen");
        let new_id = FPackageId::from_name(new_package);
        let bulk = reopened
            .find_chunk(&make_chunk_id(new_id.0, 0, CHUNK_TYPE_BULK_DATA))
            .expect("the renamed tag has a bulk chunk");
        assert_eq!(
            reopened.read_chunk(bulk).expect("read the body"),
            new_body,
            "the edited body is what landed"
        );
        assert_ne!(before, new_body.len() as i64, "the length really changed");
        assert_eq!(
            serial_size_of(&reopened, new_package),
            new_body.len() as i64,
            "and the header says so"
        );
        // The path moved too, so this is a rename and not an overwrite.
        let moved_leaf = format!("{}.ubulk", split_package_path(new_package).1);
        assert!(
            reopened
                .entries()
                .iter()
                .any(|entry| entry.path.ends_with(&moved_leaf))
        );
        drop(reopened);
        remove_duplicate_fixture(&utoc);
    }
}
