//! Minimal writer for **override/overlay** UE5 IoStore containers (v8,
//! unencrypted, single-partition, uncompressed).
//!
//! An override container replaces specific chunks of a shipping game by reusing
//! their exact [`FIoChunkId`]s; mounted at higher priority (a `_P` suffix) the
//! engine resolves those ids to our bytes (last-mounted-wins). It carries no
//! `ContainerHeader` — the base game already supplies the package-store entry;
//! serving raw chunks by id is sufficient (this mirrors retoc's `pack-raw`).
//!
//! All chunks are stored uncompressed (compression method 0), so no Oodle
//! *encoder* is needed. Layout matches [`super::IoStoreArchive`] exactly and is
//! validated by round-tripping through it.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use super::container_header::{EIoContainerHeaderVersion, FIoContainerHeader, StoreEntry};
use super::ue_types::{FIoContainerId, FPackageId};
use super::{FIoChunkId, IoStoreArchive, IoStoreError, Result};

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
    /// container via [`super::IoStoreArchive::chunk_id`]).
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
        Ok(())
    }
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
/// sibling `.ucas` is written alongside.
pub fn write_tag_override(
    base: &IoStoreArchive,
    ubulk_path: &str,
    new_tag_bytes: &[u8],
    out_utoc: &std::path::Path,
) -> Result<()> {
    let ub_id = base.chunk_id_for(ubulk_path)?;
    let old_len = base.uncompressed_len(ubulk_path)?;
    let new_len = new_tag_bytes.len() as u64;

    let mut writer = OverrideContainerWriter::new("../../../");

    if new_len != old_len {
        let ua_path = ubulk_path
            .strip_suffix(".ubulk")
            .map(|s| format!("{s}.uasset"))
            .ok_or(IoStoreError::Package("path is not a .ubulk"))?;
        if !base.contains(&ua_path) {
            return Err(IoStoreError::Package(
                "size-changing edit but no paired .uasset to patch",
            ));
        }
        let ua_id = base.chunk_id_for(&ua_path)?;
        let mut ua = base.read(&ua_path)?;
        patch_uasset_serial_size(&mut ua, old_len, new_len)?;
        writer.add_chunk(ua_id, ua);
    }

    writer.add_chunk(ub_id, new_tag_bytes.to_vec());
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
pub fn write_new_tag_container(
    template_uasset: &[u8],
    tag_bytes: &[u8],
    new_package_path: &str,
    redirect_from: Option<&str>,
    out_utoc: &std::path::Path,
) -> Result<()> {
    use super::ue_types::EIoStoreTocVersion;
    use super::zen::FZenPackageHeader;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
    let cv = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;

    // Parse the template package and mutate its identity.
    let mut cur = std::io::Cursor::new(template_uasset);
    let mut hdr = FZenPackageHeader::deserialize(&mut cur, None, cv, HV, None)
        .map_err(|_| IoStoreError::Package("failed to parse template .uasset"))?;
    if hdr.export_map.is_empty() {
        return Err(IoStoreError::Package("template .uasset has no export"));
    }
    let export_data = template_uasset[hdr.summary.header_size as usize..].to_vec();

    let new_obj = new_package_path
        .rsplit('/')
        .next()
        .unwrap_or(new_package_path);
    hdr.summary.name = hdr.name_map.store(new_package_path);
    hdr.export_map[0].object_name = hdr.name_map.store(new_obj);
    hdr.export_map[0].public_export_hash = container_id_from_name(new_obj);
    if let Some(entry) = hdr.bulk_data.first_mut() {
        entry.serial_size = tag_bytes.len() as i64;
    }

    let mut store = StoreEntry::default();
    let mut buf = std::io::Cursor::new(Vec::new());
    hdr.serialize(&mut buf, &mut store, HV)
        .map_err(|_| IoStoreError::Package("failed to serialize .uasset"))?;
    let mut new_uasset = buf.into_inner();
    new_uasset.extend_from_slice(&export_data);

    // Compute new chunk ids from the new package path.
    let new_pid = container_id_from_name(new_package_path);
    let uasset_id = make_chunk_id(new_pid, 0, CHUNK_TYPE_EXPORT_BUNDLE_DATA);
    let ubulk_id = make_chunk_id(new_pid, 0, CHUNK_TYPE_BULK_DATA);

    let mut w = OverrideContainerWriter::new("../../../");
    w.add_package(uasset_id, new_uasset, FPackageId(new_pid), store);
    w.add_chunk(ubulk_id, tag_bytes.to_vec());
    if let Some(old) = redirect_from {
        w.add_redirect(old, FPackageId(new_pid));
    }
    w.write(out_utoc)
}

/// Bundle several edited tags into ONE override (mod) container — a portable,
/// non-destructive overlay the game loads on top of the base. Each tag is a
/// same-name override: `(source_archive, ubulk_rel_path, new_tag_bytes)`. The
/// paired `.uasset` is included with its bulk size patched when an edit changed
/// the tag's length. Tags may come from different source paks (chunk ids are
/// globally unique).
pub fn write_mod_container(
    tags: &[(&IoStoreArchive, &str, &[u8])],
    out_utoc: &std::path::Path,
) -> Result<()> {
    let mut w = OverrideContainerWriter::new("../../../");
    for &(archive, ubulk_path, new_bytes) in tags {
        let ub_id = archive.chunk_id_for(ubulk_path)?;
        let old_len = archive.uncompressed_len(ubulk_path)?;
        if new_bytes.len() as u64 != old_len {
            let ua_path = ubulk_path
                .strip_suffix(".ubulk")
                .map(|s| format!("{s}.uasset"))
                .ok_or(IoStoreError::Package("path is not a .ubulk"))?;
            if !archive.contains(&ua_path) {
                return Err(IoStoreError::Package(
                    "size-changing edit but no paired .uasset to patch",
                ));
            }
            let ua_id = archive.chunk_id_for(&ua_path)?;
            let mut ua = archive.read(&ua_path)?;
            patch_uasset_serial_size(&mut ua, old_len, new_bytes.len() as u64)?;
            w.add_chunk(ua_id, ua);
        }
        w.add_chunk(ub_id, new_bytes.to_vec());
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
    let updates: Vec<(u32, Vec<u8>)> = {
        let archive = IoStoreArchive::open(utoc_path)?;
        let ub_idx = archive.chunk_index_for(ubulk_rel_path)?;
        let old_len = archive.uncompressed_len(ubulk_rel_path)?;
        let mut updates = vec![(ub_idx, new_tag_bytes.to_vec())];
        if new_tag_bytes.len() as u64 != old_len {
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
            let mut ua = archive.read(&ua_path)?;
            patch_uasset_serial_size(&mut ua, old_len, new_tag_bytes.len() as u64)?;
            updates.push((ua_idx, ua));
        }
        updates
        // archive (and its mmap) dropped here, before we touch the files.
    };
    overwrite_chunks_in_place(utoc_path, &updates)
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
        use super::super::{is_tag_payload, IoStoreArchive};
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

    /// Patch `.uasset` SerialSize to a new length, confirm it changed, patch
    /// back, and confirm byte-identity — on 4 real tags. Also confirm a wrong
    /// `old_len` is rejected (no accidental corruption).
    #[test]
    fn serial_size_patch_roundtrip() {
        use super::super::IoStoreArchive;
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
        use super::super::{is_tag_payload, IoStoreArchive};
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
        use super::super::IoStoreArchive;
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
        use super::super::IoStoreArchive;
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
        use super::super::IoStoreArchive;
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
