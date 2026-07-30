//! Which packages reference a given one — the reverse of `ce_dep_search`.
//!
//! Asked of a weapon tag, this answers the question that decides whether a new
//! weapon can ever be spawned: who names it. A tag nothing points at is a tag
//! the game never loads, however correct its contents.
//!
//! Works off each package's imported-package list, so it sees hard package
//! dependencies. Soft object paths (`TSoftObjectPtr`) are not package imports
//! and are not counted here.
//!
//! Run: cargo run --release --features iostore --example ce_referenced_by -- <substr>

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let needle = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "shotgun-weapon".into())
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    // Pass 1: package-name -> id, so the needle can be resolved to the ids that
    // an importer's list actually holds.
    let mut want: BTreeMap<u64, String> = BTreeMap::new();
    let mut archives = Vec::new();
    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.ends_with(".uasset") && !lp.ends_with(".umap") {
                continue;
            }
            if !lp.contains(&needle) {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            want.insert(FPackageId::from_name(&hdr.package_name()).0, e.path.clone());
        }
        archives.push(utoc.clone());
    }
    if want.is_empty() {
        eprintln!("nothing matches {needle:?}");
        return;
    }
    println!("targets ({}):", want.len());
    for p in want.values() {
        println!("   {p}");
    }
    println!();

    // Pass 2: every package, checking its imported-package ids against the set.
    let mut hits: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let (mut scanned, mut unreadable) = (0usize, 0usize);
    for utoc in &archives {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.ends_with(".uasset") && !lp.ends_with(".umap") {
                continue;
            }
            scanned += 1;
            let Ok(bytes) = a.read(&e.path) else {
                unreadable += 1;
                continue;
            };
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                unreadable += 1;
                continue;
            };
            for imp in &hdr.imported_packages {
                if let Some(target) = want.get(&imp.0) {
                    hits.entry(target.clone()).or_default().push(e.path.clone());
                }
            }
        }
    }

    println!("scanned {scanned} packages ({unreadable} unreadable)\n");
    for (target, refs) in &hits {
        println!("== {} referenced by {} ==", target, refs.len());
        for r in refs {
            println!("   {r}");
        }
        println!();
    }
    for target in want.values() {
        if !hits.contains_key(target) {
            println!("== {target} referenced by NOTHING ==\n");
        }
    }
}
