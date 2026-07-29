//! Reassembling a cooked package from its parts.
//!
//! [`FZenPackageHeader::serialize`] recomputes the header's *own* internal
//! offsets by seeking back once it knows where everything landed, but it writes
//! the export map verbatim — so each export's `cooked_serial_offset` and
//! `cooked_serial_size` are the caller's responsibility, and nothing was filling
//! that role. That is the gap between "we can write an export" and "we can write
//! a package", and it is what this module closes.

use anyhow::{bail, Result};
use std::io::{Cursor, Write};

use super::zen::FZenPackageHeader;
use crate::iostore::container::header::{EIoContainerHeaderVersion, StoreEntry};

/// `PACKAGE_FILE_TAG` (ObjectVersion.h:14) — the four bytes every cooked
/// package ends with, after the last export.
///
/// It sits outside every export's `cooked_serial_offset`/`cooked_serial_size`,
/// so a reader that only walks the export map never sees it and a writer that
/// only concatenates payloads silently drops it. Measured on the shipped
/// corpus: all 103,867 packages have exactly these four trailing bytes, none
/// has any other slack.
pub const PACKAGE_FILE_TAG: u32 = 0x9E2A_83C1;

/// Rebuild a package from its header and one payload per export.
///
/// `payloads` is indexed by export-map index, and each entry is that export's
/// whole serial range — what [`write_export`](crate::iostore::object::export::write_export)
/// produces. Every `cooked_serial_offset`/`cooked_serial_size` is recomputed
/// from the payloads actually supplied, so an export that changed size is
/// handled without the caller tracking offsets.
///
/// Returns the package bytes and the [`StoreEntry`] the container index needs
/// for it, since the entry's `export_bundles_size` is only knowable here.
pub fn write_package(
    header: &FZenPackageHeader,
    payloads: &[Vec<u8>],
    container_header_version: EIoContainerHeaderVersion,
) -> Result<(Vec<u8>, StoreEntry)> {
    if payloads.len() != header.export_map.len() {
        bail!(
            "package has {} exports but {} payloads were supplied",
            header.export_map.len(),
            payloads.len()
        );
    }

    // Exports are laid out in the order the export map lists them, each
    // starting where the previous ended — measured across all 103,867 cooked
    // packages, which is what `ce_package_rebuild` re-checks. The offsets are
    // relative to the end of the header, so they depend only on the payload
    // sizes and not on how large the header turns out to be.
    let mut patched = header.clone();
    let mut offset = 0u64;
    for (entry, payload) in patched.export_map.iter_mut().zip(payloads) {
        entry.cooked_serial_offset = offset;
        entry.cooked_serial_size = payload.len() as u64;
        offset += payload.len() as u64;
    }

    let mut out = Cursor::new(Vec::new());
    let mut store_entry = StoreEntry::default();
    patched.serialize(&mut out, &mut store_entry, container_header_version)?;
    let mut out = out.into_inner();
    for payload in payloads {
        out.write_all(payload)?;
    }

    out.write_all(&PACKAGE_FILE_TAG.to_le_bytes())?;

    // The container index records how much export data follows the header.
    // The trailing tag is not part of any export, so it is not counted here.
    store_entry.export_bundles_size = offset;
    Ok((out, store_entry))
}

/// The export payloads of a package, in export-map order.
///
/// The inverse of what [`write_package`] consumes, so a caller can read a
/// package apart, change one export, and put it back without having to know how
/// exports are placed.
pub fn read_payloads(header: &FZenPackageHeader, package: &[u8]) -> Result<Vec<Vec<u8>>> {
    let base = header.summary.header_size as usize;
    let mut out = Vec::with_capacity(header.export_map.len());
    for (i, entry) in header.export_map.iter().enumerate() {
        let start = base + entry.cooked_serial_offset as usize;
        let end = start + entry.cooked_serial_size as usize;
        if end > package.len() || start > end {
            bail!(
                "export {i} spans {start}..{end}, past the {}-byte package",
                package.len()
            );
        }
        out.push(package[start..end].to_vec());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::package::zen::{
        EExportCommandType, EExportFilterFlags, FDependencyBundleHeader, FExportBundleEntry,
        FExportMapEntry,
    };
    use crate::iostore::package::ue_types::FPackageObjectIndex;
    use crate::iostore::package::name_map::FMappedName;

    fn entry(offset: u64, size: u64) -> FExportMapEntry {
        FExportMapEntry {
            cooked_serial_offset: offset,
            cooked_serial_size: size,
            object_name: FMappedName::default(),
            outer_index: FPackageObjectIndex::create_null(),
            class_index: FPackageObjectIndex::create_null(),
            super_index: FPackageObjectIndex::create_null(),
            template_index: FPackageObjectIndex::create_null(),
            public_export_hash: 0,
            object_flags: 0,
            filter_flags: EExportFilterFlags::None,
            padding: [0; 3],
        }
    }

    fn header(sizes: &[u64]) -> FZenPackageHeader {
        let mut h = FZenPackageHeader::default();
        let mut off = 0;
        for (i, &s) in sizes.iter().enumerate() {
            h.export_map.push(entry(off, s));
            off += s;
            // Every export needs a Create and a Serialize command, which the
            // reader enforces — a header without them is not a package.
            for command_type in [EExportCommandType::Create, EExportCommandType::Serialize] {
                h.export_bundle_entries
                    .push(FExportBundleEntry { local_export_index: i as u32, command_type });
            }
            // ...and one dependency bundle header, likewise enforced.
            h.dependency_bundle_headers.push(FDependencyBundleHeader::default());
        }
        h
    }

    /// The reason offsets are recomputed rather than trusted: an export that
    /// changes size has to move every export after it. Nothing in the corpus
    /// changes size, so the round-trip gate can never exercise this — it is
    /// exactly the shape of bug that only appears once someone edits a tag.
    #[test]
    fn a_resized_export_shifts_the_ones_after_it() {
        let h = header(&[10, 20, 30]);
        // Middle export grows from 20 bytes to 25.
        let payloads = vec![vec![0xAA; 10], vec![0xBB; 25], vec![0xCC; 30]];
        let (bytes, store) =
            write_package(&h, &payloads, EIoContainerHeaderVersion::SoftPackageReferences)
                .expect("build");

        // Re-read the header we just wrote and check where it says things are.
        let rebuilt = FZenPackageHeader::deserialize(
            &mut Cursor::new(&bytes),
            None,
            crate::iostore::package::ue_types::EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash,
            EIoContainerHeaderVersion::SoftPackageReferences,
            None,
        )
        .expect("re-read the header");

        let placed: Vec<(u64, u64)> = rebuilt
            .export_map
            .iter()
            .map(|e| (e.cooked_serial_offset, e.cooked_serial_size))
            .collect();
        assert_eq!(
            placed,
            vec![(0, 10), (10, 25), (35, 30)],
            "the export after the resized one did not move"
        );
        assert_eq!(store.export_bundles_size, 65);
        assert_eq!(
            read_payloads(&rebuilt, &bytes).expect("read back"),
            payloads,
            "payloads did not survive placement"
        );
    }

    /// Every cooked package ends with `PACKAGE_FILE_TAG`, outside any export's
    /// serial range — so concatenating payloads alone silently drops it.
    #[test]
    fn a_package_ends_with_the_file_tag() {
        let h = header(&[4]);
        let (bytes, _) = write_package(
            &h,
            &[vec![1, 2, 3, 4]],
            EIoContainerHeaderVersion::SoftPackageReferences,
        )
        .expect("build");
        assert_eq!(bytes[bytes.len() - 4..], PACKAGE_FILE_TAG.to_le_bytes());
    }

    /// A payload per export, or the placement is meaningless.
    #[test]
    fn a_payload_count_mismatch_is_refused() {
        let h = header(&[4, 4]);
        let e = write_package(&h, &[vec![0; 4]], EIoContainerHeaderVersion::SoftPackageReferences)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 exports"), "unhelpful refusal: {e}");
    }
}
