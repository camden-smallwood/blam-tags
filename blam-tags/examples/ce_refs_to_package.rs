//! Exhaustive reverse-reference search: every cooked package whose imports (or
//! name map) mention a target, with no filename pre-filter.
//!
//! Uses `read_prefix` so each package costs a header-sized read rather than a
//! full decompress — the earlier probe skipped anything not named like a data
//! asset, which is exactly how a reference gets missed.
//!
//! Run: cargo run --features iostore --example ce_refs_to_package -- <substring> [prefix-kb]

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn norm(p: &str) -> String {
    p.to_ascii_lowercase().replace('\\', "/")
}

fn main() {
    let target = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "sk_pelican_common".into())
        .to_ascii_lowercase();
    let prefix_kb: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(192);
    let prefix = prefix_kb * 1024;

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut scanned = 0usize;
    let mut parsed = 0usize;
    let mut by_import: Vec<String> = Vec::new();
    let mut by_namemap: Vec<String> = Vec::new();

    for path in &utocs {
        let Ok(a) = IoStoreArchive::open(path) else { continue };
        for e in a.entries() {
            let n = norm(&e.path);
            if !n.ends_with(".uasset") {
                continue;
            }
            scanned += 1;
            // The package itself is not a reference to itself.
            let is_self = n.contains(&target);
            let Ok(bytes) = a.read_prefix(&e.path, prefix) else { continue };
            let Ok(h) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            parsed += 1;
            let imports: Vec<&String> = h
                .imported_package_names
                .iter()
                .filter(|p| norm(p).contains(&target))
                .collect();
            if !imports.is_empty() {
                by_import.push(format!("{}  ->  {:?}", e.path, imports));
                continue;
            }
            if is_self {
                continue;
            }
            let names = h.name_map.copy_raw_names();
            if names.iter().any(|s| s.to_ascii_lowercase().contains(&target)) {
                by_namemap.push(e.path.clone());
            }
        }
    }

    println!("scanned {scanned} .uasset entries, parsed {parsed} headers\n");
    println!("=== hard package imports of '{target}' ({}) ===", by_import.len());
    for h in &by_import {
        println!("  {h}");
    }
    println!(
        "\n=== name-map mentions only (soft refs / same-name objects) ({}) ===",
        by_namemap.len()
    );
    for h in by_namemap.iter().take(40) {
        println!("  {h}");
    }
    if by_namemap.len() > 40 {
        println!("  … and {} more", by_namemap.len() - 40);
    }
}
