//! Who imports tag packages? Scans EVERY cooked package in the game (not just
//! tags) and reports, for each importer root, how many tag packages it pulls in.
//! Determines whether a new tag can become reachable by any route other than
//! another tag's CookedAssetsReferencedByTag.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn root(p: &str) -> String { p.split('/').take(3).collect::<Vec<_>>().join("/") }
fn main() {
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut importers: BTreeMap<String, usize> = BTreeMap::new();
    let mut imported_tags: BTreeSet<String> = BTreeSet::new();
    let mut nontag_importers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut all_tags: BTreeSet<String> = BTreeSet::new();
    let mut tag_imported_by_tag: BTreeSet<String> = BTreeSet::new();
    let mut total = 0usize;
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") && !lower.ends_with(".umap") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            total += 1;
            let me = h.package_name().to_ascii_lowercase();
            let me_is_tag = me.starts_with("/game/tags/");
            if me_is_tag { all_tags.insert(me.clone()); }
            for imp in &h.imported_package_names {
                let t = imp.to_ascii_lowercase();
                if !t.starts_with("/game/tags/") { continue }
                imported_tags.insert(t.clone());
                *importers.entry(root(&me)).or_default() += 1;
                if me_is_tag { tag_imported_by_tag.insert(t.clone()); }
                else { nontag_importers.entry(root(&me)).or_default().insert(t.clone()); }
            }
        }
    }
    println!("scanned {total} packages; {} tag packages", all_tags.len());
    println!("tag packages imported by SOMETHING: {}", imported_tags.len());
    println!("tag packages imported by NOTHING  : {}", all_tags.difference(&imported_tags).count());
    println!("\n-- importer roots (import edges into /Game/Tags) --");
    let mut v: Vec<_> = importers.iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (r, n) in v.iter().take(20) { println!("  {n:>8}  {r}"); }
    println!("\n-- NON-tag roots and how many DISTINCT tags each reaches --");
    let mut w: Vec<_> = nontag_importers.iter().collect();
    w.sort_by_key(|(_, s)| std::cmp::Reverse(s.len()));
    for (r, s) in w.iter().take(20) {
        let sample: Vec<&String> = s.iter().take(2).collect();
        println!("  {:>6}  {r}   e.g. {sample:?}", s.len());
    }
    let only_nontag: BTreeSet<&String> = imported_tags.iter().filter(|t| !tag_imported_by_tag.contains(*t)).collect();
    println!("\ntags reachable ONLY from a non-tag package: {}", only_nontag.len());
    let mut byg: BTreeMap<String, usize> = BTreeMap::new();
    for t in &only_nontag { *byg.entry(t.rsplit('-').next().unwrap_or("").to_string()).or_default() += 1; }
    let mut g: Vec<_> = byg.iter().collect(); g.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    println!("  by group: {:?}", g.iter().take(15).collect::<Vec<_>>());
    println!("\ntags imported by NOTHING, by group:");
    let mut byg2: BTreeMap<String, usize> = BTreeMap::new();
    for t in all_tags.difference(&imported_tags) { *byg2.entry(t.rsplit('-').next().unwrap_or("").to_string()).or_default() += 1; }
    let mut g2: Vec<_> = byg2.iter().collect(); g2.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (k, n) in g2.iter().take(20) { println!("  {n:>6}  {k}"); }
}
