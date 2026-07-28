//! Can an `AssetReference` be computed from just the target package path, or
//! must the exact (package, public-export-hash) pair be copied from a donor tag?
//!
//! Tests: hash == cityhash64(lowercase UTF-16 "<PackageLeaf>_C")  (BP generated
//! class), and whether every tag that references the same package uses the same
//! hash.
//!
//! Run: cargo run --release --features iostore --example ce_assetref_hash_rule

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::writer::cityhash64;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn ch(s: &str) -> u64 {
    cityhash64(&s.to_ascii_lowercase().encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>())
}

fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // target package -> set of hashes used as AssetReference
    let mut by_target: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut rule_c = 0usize;
    let mut rule_plain = 0usize;
    let mut rule_none = 0usize;
    let mut total = 0usize;
    let mut misses: Vec<String> = Vec::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.first() else { continue };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(ua.len());
            if off >= ua.len() {
                continue;
            }
            let class = class_for_group(group);
            let Ok(props) = read_export_struct(&ua[off..end], &names, &usmap, &class) else { continue };
            let Some(PropValue::Object(i)) = props.get("AssetReference") else { continue };
            if *i >= 0 {
                continue;
            }
            let Some(r) = h.import_map.get((-*i - 1) as usize).and_then(|im| im.package_import())
            else {
                continue;
            };
            let Some(pkg) = h.imported_package_names.get(r.imported_package_index as usize) else {
                continue;
            };
            let hash = h.imported_public_export_hashes[r.imported_public_export_hash_index as usize];
            total += 1;
            by_target.entry(pkg.clone()).or_default().insert(hash);

            let leaf = pkg.rsplit('/').next().unwrap_or("");
            if ch(&format!("{leaf}_C")) == hash {
                rule_c += 1;
            } else if ch(leaf) == hash {
                rule_plain += 1;
            } else {
                rule_none += 1;
                if misses.len() < 10 {
                    misses.push(format!("    {pkg}  hash {hash:016x}  (leaf_C={:016x}, leaf={:016x})",
                        ch(&format!("{leaf}_C")), ch(leaf)));
                }
            }
        }
    }

    println!("{total} AssetReference values");
    println!("  hash == cityhash64(\"<leaf>_C\") : {rule_c}");
    println!("  hash == cityhash64(\"<leaf>\")   : {rule_plain}");
    println!("  neither                          : {rule_none}");
    let multi: Vec<_> = by_target.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("\ntarget packages referenced with >1 distinct hash: {} of {}",
        multi.len(), by_target.len());
    for (t, hs) in multi.iter().take(8) {
        println!("    {t}: {:?}", hs.iter().map(|h| format!("{h:016x}")).collect::<Vec<_>>());
    }
    println!("\n-- rule misses --");
    for m in &misses {
        println!("{m}");
    }
}

fn class_for_group(group: &str) -> String {
    let mut out = String::from("Blam");
    for part in group.split('_') {
        let mut c = part.chars();
        if let Some(f) = c.next() {
            out.push(f.to_ascii_uppercase());
            out.push_str(c.as_str());
        }
    }
    out.push_str("TagDataAsset");
    out
}
