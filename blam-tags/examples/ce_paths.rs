//! Every entry path in every Campaign Evolved container, one per line.
//!
//! The other listing examples cap how many matches they print, which is fine
//! when you are looking something up and wrong when you are measuring the tree.
//! This one prints all of them so the shape can be counted outside.
//!
//! Run: cargo run --release --features iostore --example ce_paths [paks]

use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let paks = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .collect();
    utocs.sort();

    let mut n = 0usize;
    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            println!("{}", e.path);
            n += 1;
        }
    }
    eprintln!("{n} entries across {} containers", utocs.len());
}
