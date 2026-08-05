//! Scratch probe (not for commit): for every Campaign Evolved group the
//! definitions define, does the game ship a tag of it, and is its wrapper class
//! bare?
//!
//! The question it answers: when creation falls past "a same-group tag ships",
//! is deriving the wrapper (`is_bare_group`) enough on its own, or is there a
//! group that is neither shipped nor derivable and so still needs a cross-group
//! donor?
//!
//! Run: cargo run --release --features iostore --example ce_group_census

use std::collections::{BTreeMap, BTreeSet};

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::asset::tag_package::is_bare_group;
use blam_tags::iostore::object::usmap::Usmap;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const DEFS: &str = "/Users/camden/Source/Baboon-local/definitions/haloce_evolved";

fn main() {
    let usmap = Usmap::meteorite().expect("bundled usmap");

    // Every group the New Tag dialog can offer: one schema file each.
    let mut defined: BTreeSet<String> = BTreeSet::new();
    for entry in std::fs::read_dir(DEFS).expect("definitions") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|x| x == "json")
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            defined.insert(stem.to_owned());
        }
    }

    // Every group the game actually ships at least one tag of.
    let mut shipped: BTreeMap<String, usize> = BTreeMap::new();
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
    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else {
            eprintln!("UNREADABLE: {}", utoc.display());
            continue;
        };
        for entry in archive.entries() {
            let lower = entry.path.to_ascii_lowercase();
            if !lower.contains("/tags/") || !lower.ends_with(".uasset") {
                continue;
            }
            let Some((_, group)) = lower
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".uasset"))
                .and_then(|stem| stem.rsplit_once('-'))
            else {
                continue;
            };
            *shipped.entry(group.to_owned()).or_default() += 1;
        }
    }

    let mut unshipped_bare = Vec::new();
    let mut unshipped_not_bare = Vec::new();
    let mut shipped_not_bare = 0usize;
    for group in &defined {
        let bare = is_bare_group(group, &usmap);
        if shipped.contains_key(group) {
            if !bare {
                shipped_not_bare += 1;
            }
        } else if bare {
            unshipped_bare.push(group.clone());
        } else {
            unshipped_not_bare.push(group.clone());
        }
    }

    // A group the paks carry that the definitions do not name would mean the
    // dialog's list and the game disagree; report it rather than silently
    // dropping it out of the totals.
    let undefined: Vec<&String> = shipped.keys().filter(|g| !defined.contains(*g)).collect();

    println!("defined groups (schemas on disk): {}", defined.len());
    println!("  shipped by the game:            {}", defined.iter().filter(|g| shipped.contains_key(*g)).count());
    println!("    of which NOT bare:            {shipped_not_bare}");
    println!("  not shipped:                    {}", unshipped_bare.len() + unshipped_not_bare.len());
    println!("    derivable (bare):             {}", unshipped_bare.len());
    println!("    NOT derivable (needs donor):  {}", unshipped_not_bare.len());
    println!();
    println!("shipped groups not in definitions: {:?}", undefined);
    println!();
    println!("--- not shipped, NOT derivable (the only case a cross-group donor could serve) ---");
    if unshipped_not_bare.is_empty() {
        println!("(none)");
    }
    for group in &unshipped_not_bare {
        println!("  {group}");
    }
    println!();
    println!("--- not shipped, derivable ({}) ---", unshipped_bare.len());
    for group in &unshipped_bare {
        println!("  {group}");
    }
}
