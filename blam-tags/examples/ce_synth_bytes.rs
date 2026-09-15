//! Gate: synthesize a tag's `.uasset` from the derivation rules and require the
//! bytes to equal what shipped.
//!
//! Testing an authoring path by loading the result in the game proves one
//! package worked once, and needs the game. This proves the *rule* over every
//! tag the game already loads, and needs nothing but the containers.
//!
//! What is supplied to the builder is only what an author actually has: the
//! group, the package path, and the length of the tag body. Everything else --
//! the export payload, the bulk-data entry, the export filter flags -- is
//! constructed. That is the difference from the earlier probe, which borrowed
//! those three from the shipped file it was checking against and so could never
//! have failed on them.
//!
//! Scope is tags whose class is bare and whose shipped block has nothing
//! present. A tag that carries property data is a different job -- there is
//! nothing to derive its values from -- and one that has package imports cannot
//! be held to byte identity at all, because the import-map slot order is the
//! cooker's linker order. Both are counted and named rather than filtered away.
//!
//! Run: cargo run --release --features iostore --example ce_synth_bytes

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::asset::tag_package::{build_bare_tag_package, group_to_class, is_bare_group};
use blam_tags::iostore::container_header::{EIoContainerHeaderVersion, StoreEntry};
use blam_tags::iostore::object::usmap::Usmap;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct Counts {
    seen: usize,
    has_package_imports: usize,
    not_bare: usize,
    block_not_empty: usize,
    no_bulk_entry: usize,
    unreadable: usize,
    in_scope: usize,
    exact: usize,
    differed: usize,
    control_ok: usize,
    control_bad: usize,
}

fn main() {
    let usmap = Usmap::meteorite().expect("bundled usmap");
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    let mut c = Counts::default();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    let mut groups_covered: BTreeMap<String, usize> = BTreeMap::new();

    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for entry in archive.entries() {
            let lower = entry.path.to_ascii_lowercase();
            if !lower.contains("/tags/") || !lower.ends_with(".uasset") {
                continue;
            }
            let Some(stem) = lower
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".uasset"))
            else {
                continue;
            };
            let Some((_, group)) = stem.rsplit_once('-') else {
                continue;
            };
            c.seen += 1;

            let Ok(shipped_bytes) = archive.read(&entry.path) else {
                c.unreadable += 1;
                continue;
            };
            let Ok(shipped) = FZenPackageHeader::deserialize(
                &mut Cursor::new(&shipped_bytes[..]),
                None,
                CV,
                HV,
                None,
            ) else {
                c.unreadable += 1;
                continue;
            };

            // --- the control, always: re-serialize the SHIPPED header
            // untouched. If this cannot reproduce the file then the gap is in
            // the serializer, and no amount of fixing the rules would find it.
            {
                let mut buffer = Cursor::new(Vec::new());
                let mut store = StoreEntry::default();
                if shipped.clone().serialize(&mut buffer, &mut store, HV).is_ok() {
                    let mut round = buffer.into_inner();
                    round.extend_from_slice(&shipped_bytes[shipped.summary.header_size as usize..]);
                    if round == shipped_bytes {
                        c.control_ok += 1;
                    } else {
                        c.control_bad += 1;
                    }
                }
            }

            if !shipped.imported_packages.is_empty() {
                c.has_package_imports += 1;
                continue;
            }
            if !is_bare_group(group, &usmap) {
                c.not_bare += 1;
                continue;
            }
            let Some(bulk) = shipped.bulk_data.first() else {
                c.no_bulk_entry += 1;
                continue;
            };
            // A shipped block with anything present is carrying authored data,
            // which a brand-new tag by definition does not have.
            let payload_len = shipped
                .export_map
                .first()
                .map(|e| e.cooked_serial_size)
                .unwrap_or(0);
            let empty_len = empty_block_len(&group_to_class(group), &usmap);
            if Some(payload_len) != empty_len {
                c.block_not_empty += 1;
                continue;
            }

            // The tag body's length, taken from the paired `.ubulk` chunk rather
            // than from the header being reproduced.
            let ubulk_path = entry.path.replace(".uasset", ".ubulk");
            let tag_len = archive
                .uncompressed_len(&ubulk_path)
                .unwrap_or(bulk.serial_size as u64);

            c.in_scope += 1;
            *groups_covered.entry(group.to_string()).or_default() += 1;

            let package_path = shipped.package_name();
            match build_bare_tag_package(group, &package_path, tag_len, &usmap) {
                Ok((built, _store)) => {
                    if built == shipped_bytes {
                        c.exact += 1;
                    } else {
                        c.differed += 1;
                        let reason = if built.len() != shipped_bytes.len() {
                            format!("length {} vs {}", built.len(), shipped_bytes.len())
                        } else {
                            let at = built
                                .iter()
                                .zip(&shipped_bytes)
                                .position(|(a, b)| a != b)
                                .unwrap_or(0);
                            format!("first differing byte at {at}")
                        };
                        *reasons.entry(reason.clone()).or_default() += 1;
                        if samples.len() < 10 {
                            samples.push(format!("{}: {reason}", entry.path));
                        }
                    }
                }
                Err(error) => {
                    c.differed += 1;
                    *reasons.entry(format!("build failed: {error}")).or_default() += 1;
                }
            }
        }
    }

    println!("shipped tag .uassets seen         : {}", c.seen);
    println!("  unreadable / unparseable        : {}", c.unreadable);
    println!("  have package imports            : {}", c.has_package_imports);
    println!("  class is not bare               : {}", c.not_bare);
    println!("  shipped block carries data      : {}", c.block_not_empty);
    println!("  no bulk-data entry              : {}", c.no_bulk_entry);
    println!(
        "  --> IN SCOPE                    : {} (over {} groups)",
        c.in_scope,
        groups_covered.len()
    );
    println!();
    println!("control (re-serialize shipped)    : {} ok, {} BAD", c.control_ok, c.control_bad);
    println!("SYNTHESIZED BYTE-IDENTICAL        : {} / {}", c.exact, c.in_scope);
    println!("differed                          : {}", c.differed);

    let accounted = c.unreadable
        + c.has_package_imports
        + c.not_bare
        + c.block_not_empty
        + c.no_bulk_entry
        + c.in_scope;
    println!("\naccounted for: {accounted} / {} ({})", c.seen,
        if accounted == c.seen { "every package named" } else { "MISSING SOME" });

    if !reasons.is_empty() {
        println!("\n-- why they differed --");
        let mut v: Vec<_> = reasons.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (reason, n) in v.iter().take(10) {
            println!("  {n:>6}  {reason}");
        }
        println!("\n-- samples --");
        for s in &samples {
            println!("  {s}");
        }
    }

    println!("\n-- groups covered in scope --");
    let mut v: Vec<_> = groups_covered.iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (group, n) in v.iter().take(20) {
        println!("  {n:>6}  {group}");
    }
}

/// The export payload length an empty block of `class` produces, so "the
/// shipped block has nothing present" is decided by the same code that builds
/// one rather than by a hardcoded size.
fn empty_block_len(class: &str, usmap: &Usmap) -> Option<u64> {
    use blam_tags::iostore::object::block::flattened_schema;
    use blam_tags::iostore::object::export::{write_export, Export, ExportBlock, Trailer};
    use blam_tags::iostore::object::value::{BlockLayout, PropertyBlock};

    let schema_len = flattened_schema(class, usmap).ok()?.len() as u32;
    let export = Export {
        block: ExportBlock::Reflected(PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned {
                schema_len,
                leading_empty: 2,
            },
        }),
        trailer: Trailer::NoGuid,
        tail: Vec::new(),
    };
    Some(write_export(class, &export, usmap).ok()?.len() as u64)
}
