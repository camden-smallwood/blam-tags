//! Probe: what does re-serializing a workspace's stashed tags cost?
//!
//! Baboon's Campaign Evolved autosave calls `write_to_bytes` on every dirty
//! document on the UI thread, every 0.75 s. Time that for the tags in a real
//! reported-slow workspace.
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_serialize_cost -- <paks>

use std::sync::Arc;
use std::time::Instant;

use blam_tags::file::TagFile;
use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};

const DEFAULT_PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

/// The six tabs in the report.
const WANTED: &[(&str, &str)] = &[
    ("objects/vehicles/human/pelican/pelican", "vehicle"),
    ("objects/vehicles/human/pelican/pelican", "skeleton_model"),
    ("objects/vehicles/human/pelican/pelican", "physics_model"),
    ("objects/vehicles/human/pelican/pelican", "model_animation_graph"),
    ("objects/characters/spartans/spartans", "model_animation_graph"),
    ("b30", "scenario"),
];

fn main() {
    let paks = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut index: Vec<(String, Arc<IoStoreArchive>, String)> = Vec::new();
    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        let archive = Arc::new(archive);
        for e in archive.ublock_entries() {
            if parse_ublock_stem(&e.path).is_none() {
                continue;
            }
            index.push((
                e.path.to_ascii_lowercase().replace('\\', "/"),
                archive.clone(),
                e.path.clone(),
            ));
        }
    }

    let mut total_write = 0.0f64;
    let mut total_bytes = 0usize;
    for (path, group) in WANTED {
        let needle = format!("{path}-{group}.ubulk");
        let Some((_, archive, rel)) = index.iter().find(|(norm, _, _)| norm.ends_with(&needle))
        else {
            println!("{path}.{group}: NOT FOUND");
            continue;
        };
        let bytes = archive.read(rel).expect("read");
        let read_start = Instant::now();
        let tag = TagFile::read_from_bytes(&bytes).expect("parse");
        let read_ms = read_start.elapsed().as_secs_f64() * 1000.0;

        // Five writes, so a single allocation spike does not dominate.
        let write_start = Instant::now();
        let mut out = Vec::new();
        for _ in 0..5 {
            out = tag.write_to_bytes().expect("write");
        }
        let write_ms = write_start.elapsed().as_secs_f64() * 1000.0 / 5.0;
        total_write += write_ms;
        total_bytes += out.len();
        println!(
            "{path}.{group}: {} KiB   read {read_ms:.1} ms   write_to_bytes {write_ms:.1} ms",
            out.len() / 1024
        );
    }
    println!(
        "\ntotal write_to_bytes per autosave: {total_write:.1} ms over {} KiB",
        total_bytes / 1024
    );
}
