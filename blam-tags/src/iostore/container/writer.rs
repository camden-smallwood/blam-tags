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
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::header::{EIoContainerHeaderVersion, FIoContainerHeader, StoreEntry};
use crate::iostore::package::ue_types::{FIoContainerId, FPackageId};
use crate::iostore::{FIoChunkId, IoStoreArchive, IoStoreError, Result};

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
                    return Err(IoStoreError::Package("unsupported container header version"));
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
                Some((make_chunk_id(container_id, 0, CHUNK_TYPE_CONTAINER_HEADER), bytes))
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
                blocks.push(encode_block(ucas_offset, block.len() as u32, block.len() as u32, 0));
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
        return Err(IoStoreError::Package("bad imported_public_export_hashes_offset"));
    }
    let ipeh = ipeh as usize;
    let rd = |off: usize| u64::from_le_bytes(uasset[off..off + 8].try_into().unwrap());

    let map_size = rd(ipeh - 40); // int64 BulkDataMapSize
    let dup = rd(ipeh - 24); // DuplicateSerialOffset (single entry => -1)
    let serial_size_off = ipeh - 16; // SerialSize
    let cur = rd(serial_size_off);
    if map_size != 32 {
        return Err(IoStoreError::Package("expected exactly one bulk-data entry"));
    }
    if dup != u64::MAX {
        return Err(IoStoreError::Package("bulk-data entry signature mismatch"));
    }
    if cur != old_len {
        return Err(IoStoreError::Package("current SerialSize != old .ubulk length"));
    }
    uasset[serial_size_off..serial_size_off + 8].copy_from_slice(&new_len.to_le_bytes());
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
/// Mutates `template_uasset`'s identity to `new_package_path` (a template is
/// typically an existing same-group tag's `.uasset`), sets the tag content in
/// the `.ubulk`, and writes the container with a ContainerHeader package-store
/// entry so the new package is locatable by the engine. `redirect_from` (the
/// old `/Game/Tags/...` package path) adds a rename redirect so existing
/// references resolve to the renamed tag. All hashing/ids derive from the
/// names; nothing depends on retoc at runtime.
/// A brand-new tag package to add to an override container: the source `.uasset`
/// template (typically an existing same-group tag's), the new tag `.ubulk` bytes,
/// the target UE package path (`/Game/Tags/<rel>-<group>`), and an optional
/// old→new package redirect for renames.
pub struct NewPackage<'a> {
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

/// Add one brand-new tag package (mutating the template `.uasset`'s identity to
/// `new_package_path`, setting the `.ubulk` content, plus an optional redirect)
/// to an in-progress override container writer.
fn add_new_package_to_writer(w: &mut OverrideContainerWriter, pkg: &NewPackage) -> Result<()> {
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
    let export_data = match sanitize_donated_export(&mut sanitized_hdr, &export_data, pkg) {
        Ok(bytes) => {
            hdr = sanitized_hdr;
            bytes
        }
        Err(SanitizeSkip::NothingToStrip) => export_data,
        Err(SanitizeSkip::Failed(e)) => return Err(e),
    };

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
        read_import_slots, split_tag_package, tag_wrapper_class_path, write_import_slots,
        ImportSlot,
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
    let class = class_path.rsplit('.').next().unwrap_or(&class_path).to_owned();

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
    let had_donor_data = block.get("AssetReference").is_some()
        || block.get("CookedAssetsReferencedByTag").is_some();
    if !had_donor_data && pkg.asset_reference.is_none() {
        return Err(SanitizeSkip::NothingToStrip);
    }

    block.entries.retain(|e| {
        &*e.name != "AssetReference" && &*e.name != "CookedAssetsReferencedByTag"
    });

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
    use crate::iostore::package::ue_types::EIoStoreTocVersion;
    use crate::iostore::package::zen::FZenPackageHeader;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

    let mut w = OverrideContainerWriter::new("../../../");
    for over in overrides {
        let id = over.archive.chunk_id_for(over.uasset_path)?;
        // The package's own name is the identity the store is keyed by, and it
        // is inside the bytes we are about to write — so take it from there
        // rather than from the path, which is a filename convention.
        let header =
            FZenPackageHeader::deserialize(&mut std::io::Cursor::new(&over.bytes), None, CV, HV, None)
                .map_err(|_| IoStoreError::Package("rebuilt package did not parse"))?;
        let package_id = FPackageId::from_name(&header.package_name());
        w.add_package(id, over.bytes.clone(), package_id, over.store.clone());
    }
    w.write(out_utoc)
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
    let mut w = OverrideContainerWriter::new("../../../");
    for over in overrides {
        add_override_to_writer(
            &mut w,
            over.archive,
            over.ubulk_path,
            over.tag_bytes,
            over.uasset_bytes,
        )?;
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
    let cblock_off = offlen_off
        + entry_count as usize * 10
        + seeds as usize * 4
        + without_hash as usize * 4;
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
            appended.push(encode_block(phys, block.len() as u32, block.len() as u32, 0));
            phys += block.len() as u64;
            off = end;
        }
        // Repoint offset/length (logical, block-aligned) + refresh the meta hash.
        let ol = offlen_off + *chunk_index as usize * 10;
        toc[ol..ol + 10]
            .copy_from_slice(&encode_offset_length(start_block as u64 * cbs, bytes.len() as u64));
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
    if shift == 0 { val } else { val.rotate_right(shift) }
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
    let u = rotate(a.wrapping_add(g), 43)
        .wrapping_add(rotate(b, 30).wrapping_add(c).wrapping_mul(9));
    let v = ((a.wrapping_add(g)) ^ d).wrapping_add(f).wrapping_add(1);
    let w = (u.wrapping_add(v).wrapping_mul(mul)).swap_bytes().wrapping_add(h);
    let x = rotate(e.wrapping_add(f), 42).wrapping_add(c);
    let y = (w
        .wrapping_add(v)
        .wrapping_mul(mul))
    .swap_bytes()
    .wrapping_add(g)
    .wrapping_mul(mul);
    let z = e.wrapping_add(f).wrapping_add(c);
    a = ((x.wrapping_add(z)).wrapping_mul(mul).wrapping_add(y)).swap_bytes().wrapping_add(b);
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
            x.wrapping_add(y).wrapping_add(v.0).wrapping_add(fetch64(&s[off + 8..])),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cityhash_matches_real_container_id() {
        // Ground truth: pakchunk0-WinGDK's TOC header container_id.
        assert_eq!(container_id_from_name("pakchunk0-WinGDK"), 0xfbb7216c3fc8ce45);
    }

    #[test]
    fn public_export_hash_is_cityhash_of_name() {
        // A tag export's public_export_hash = CityHash64(lowercased UTF-16 of
        // the object name). Validated against real tag export hashes read from
        // pak0 — exercises the 33..64-byte CityHash path (16/17-32/>64 covered
        // elsewhere). If these hold, tag reference resolution ids are fully
        // computable from names.
        assert_eq!(container_id_from_name("jackal-model"), 0x9595babddd1ed22f); // 24 B
        assert_eq!(container_id_from_name("plasma_pistol-weapon"), 0x368e3d0b13dcbb23); // 40 B
        assert_eq!(container_id_from_name("default-sound_combiner"), 0xb7a53f4d676890ac); // 44 B
    }

    const PAK0: &str =
        "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/pakchunk0-WinGDK.utoc";

    /// Take a real tag's `.ubulk` chunk from pak0, write a same-size override
    /// container reusing its id, then read it back through the reader and
    /// confirm the id and bytes survive. Skipped when the game is absent.
    #[test]
    fn override_container_roundtrip() {
        use crate::iostore::{is_tag_payload, IoStoreArchive};
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
        use crate::iostore::{FIoChunkId, IoStoreArchive, IoStoreError, CHUNK_TYPE_BULK_DATA};

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
            matches!(
                archive.read_chunk(0),
                Err(IoStoreError::PartitionReleased)
            ),
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
        let mi = b.windows(4).rposition(|w| w == magic).expect("footer magic");
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
        assert_eq!(&b[0x26..0x3a], &phi_hash, "PathHashIndex hash matches game stub");
        assert_eq!(&b[0x4e..0x62], &fdi_hash, "FullDirectoryIndex hash matches game stub");
    }

    /// Writing any override drops the discovery `.pak` stub beside the
    /// `.utoc`/`.ucas`, and the stub is the valid empty pak. No game files.
    #[test]
    fn write_emits_pak_stub_sibling() {
        let utoc = std::env::temp_dir().join("blamtags_pakstub-WinGDK_P.utoc");
        let mut w = OverrideContainerWriter::new("../../../");
        w.add_chunk(make_chunk_id(0x1234_5678, 0, CHUNK_TYPE_BULK_DATA), vec![1, 2, 3, 4]);
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
        use crate::iostore::{is_tag_payload, IoStoreArchive};
        use crate::file::TagFile;
        if !std::path::Path::new(PAK0).exists() {
            eprintln!("skipping: {PAK0} not present");
            return;
        }
        let base = IoStoreArchive::open(PAK0).expect("open base");

        for tag_name in ["default-biped", "default-weapon", "default-vehicle", "default-effect"] {
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
            assert_ne!(new_ubulk.len(), old_ubulk.len(), "{tag_name}: edit should change size");
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
            assert_eq!(ss, new_ubulk.len() as u64, "{tag_name}: SerialSize == new length");
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
        assert_eq!(a.read_chunk(0).unwrap(), vec![0xAA; 100], "chunk 0 untouched");
        assert_eq!(a.read_chunk(1).unwrap(), new_bytes, "chunk 1 overwritten");

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    /// Native create/rename: mutate a template `.uasset`'s identity, write a
    /// container with a ContainerHeader + package store entry + redirect, and
    /// confirm it opens with the expected new chunk ids and a valid tag.
    #[test]
    fn native_create_tag_container() {
        use crate::iostore::IoStoreArchive;
        use crate::file::TagFile;
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
        assert_eq!(c.chunk_id(0).unwrap(), make_chunk_id(new_pid, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA));
        assert_eq!(c.chunk_id(1).unwrap(), make_chunk_id(new_pid, 0, CHUNK_TYPE_BULK_DATA));
        assert_eq!(c.chunk_id(2).unwrap().chunk_type(), CHUNK_TYPE_CONTAINER_HEADER);
        // The generated .ubulk still parses as a Reach tag.
        TagFile::read_from_bytes(&c.read_chunk(1).unwrap()).expect("ubulk is a tag");

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }

    /// The one-call `write_tag_override` helper: a size-changing edit produces a
    /// 2-chunk override (patched uasset + new ubulk); read the ubulk back.
    #[test]
    fn write_tag_override_helper_works() {
        use crate::iostore::IoStoreArchive;
        use crate::file::TagFile;
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
        assert_eq!(over.chunk_id(1).unwrap(), base.chunk_id_for(&ub_path).unwrap());
        assert_eq!(over.read_chunk(1).unwrap(), new);

        let _ = std::fs::remove_file(&utoc);
        let _ = std::fs::remove_file(utoc.with_extension("ucas"));
    }
}
