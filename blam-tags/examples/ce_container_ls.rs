//! List/inspect the non-tag (UE5 native) side of the Campaign Evolved
//! containers: extension histogram, directory shape, and every entry whose
//! path matches a substring (default `elite`) — to find where the UE5
//! SkeletalMesh/Skeleton assets live and how they're named relative to the
//! classic tag tree.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_container_ls -- \
//!     "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks" [substr]

use std::collections::BTreeMap;

use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let substr = args.next().unwrap_or_else(|| "elite".to_string()).to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut ext_hist: BTreeMap<String, usize> = BTreeMap::new();
    let mut top_dirs: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = 0usize;
    let mut matches: Vec<String> = Vec::new();

    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        for e in archive.entries() {
            total += 1;
            let p = &e.path;
            let ext = p.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
            *ext_hist.entry(ext).or_default() += 1;
            // top two path components after any "…/Content/"
            let lower = p.to_ascii_lowercase().replace('\\', "/");
            if let Some(idx) = lower.find("/content/") {
                let rest = &lower[idx + "/content/".len()..];
                let comps: Vec<&str> = rest.split('/').collect();
                let key = comps.iter().take(2).cloned().collect::<Vec<_>>().join("/");
                *top_dirs.entry(key).or_default() += 1;
            }
            if lower.contains(&substr) {
                matches.push(p.clone());
            }
        }
    }

    println!("total entries across {} paks: {total}", utocs.len());
    println!("\n=== extension histogram ===");
    let mut exts: Vec<_> = ext_hist.iter().collect();
    exts.sort_by(|a, b| b.1.cmp(a.1));
    for (ext, n) in exts.iter().take(25) {
        println!("  {n:>7}  .{ext}");
    }

    println!("\n=== top dirs under Content/ (by entry count) ===");
    let mut dirs: Vec<_> = top_dirs.iter().collect();
    dirs.sort_by(|a, b| b.1.cmp(a.1));
    for (d, n) in dirs.iter().take(40) {
        println!("  {n:>7}  {d}");
    }

    matches.sort();
    matches.dedup();
    println!("\n=== entries containing {substr:?}: {} ===", matches.len());
    for p in matches.iter().take(120) {
        println!("  {p}");
    }
    if matches.len() > 120 {
        println!("  ... +{} more", matches.len() - 120);
    }
}
