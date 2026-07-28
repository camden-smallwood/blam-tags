//! How does a tag get loaded at runtime? Compute the transitive package-import
//! closure starting from a scenario tag (and from globals), and report how much
//! of the 12k tag set it reaches. If a new tag is reachable from an edited
//! tag's `CookedAssetsReferencedByTag`, it loads for free.
//!
//! Run: cargo run --release --features iostore --example ce_tag_load_closure [seed-substring]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let seed_hint = std::env::args().nth(1).unwrap_or_else(|| "a30-scenario".into()).to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // package name (lower) -> imported package names (lower)
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    let mut all_tags: BTreeSet<String> = BTreeSet::new();
    let mut seeds: Vec<String> = Vec::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") && !lower.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let name = h.package_name().to_ascii_lowercase();
            if name.starts_with("/game/tags/") {
                all_tags.insert(name.clone());
            }
            if lower.contains(&seed_hint) {
                seeds.push(name.clone());
            }
            graph.insert(
                name,
                h.imported_package_names
                    .iter()
                    .map(|s| s.to_ascii_lowercase())
                    .collect(),
            );
        }
    }
    eprintln!("graph: {} packages, {} tags, {} seeds", graph.len(), all_tags.len(), seeds.len());
    for s in seeds.iter().take(8) {
        eprintln!("    seed: {s}");
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut q: VecDeque<String> = seeds.iter().cloned().collect();
    for s in &seeds {
        seen.insert(s.clone());
    }
    while let Some(n) = q.pop_front() {
        let Some(deps) = graph.get(&n) else { continue };
        for d in deps {
            if seen.insert(d.clone()) {
                q.push_back(d.clone());
            }
        }
    }

    let reached_tags: BTreeSet<&String> = all_tags.iter().filter(|t| seen.contains(*t)).collect();
    println!("\nclosure from {} seed(s): {} packages total", seeds.len(), seen.len());
    println!("tags reached: {} of {} ({:.1}%)",
        reached_tags.len(), all_tags.len(),
        reached_tags.len() as f64 * 100.0 / all_tags.len().max(1) as f64);

    // Which tag groups are NOT reached?
    let mut unreached: BTreeMap<String, usize> = BTreeMap::new();
    for t in &all_tags {
        if !seen.contains(t) {
            let g = t.rsplit_once('-').map(|(_, g)| g.to_string()).unwrap_or_default();
            *unreached.entry(g).or_default() += 1;
        }
    }
    println!("\n-- unreached tags by group --");
    let mut us: Vec<_> = unreached.iter().collect();
    us.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (g, n) in us.iter().take(30) {
        println!("{n:>6}  {g}");
    }
}
