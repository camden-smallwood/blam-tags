//! Sweep every `.decal_system` in a tags root and report any tag with
//! `max overlapping > 0`. Drives the Phase 7.1 "is c_decal_system::check_overlap
//! ever non-inert?" survey.
//!
//! Usage: `cargo run --release --example decal_overlap_sweep -- <tags_root>`

use std::path::{Path, PathBuf};

use blam_tags::TagFile;
use blam_tags::decal_system::DecalSystem;

fn main() {
    let Some(root_arg) = std::env::args().nth(1) else {
        eprintln!("usage: decal_overlap_sweep <tags_root>");
        std::process::exit(2);
    };
    let root = PathBuf::from(root_arg);
    if !root.is_dir() {
        eprintln!("not a directory: {}", root.display());
        std::process::exit(2);
    }

    let mut total = 0usize;
    let mut with_overlap = 0usize;
    let mut failed = 0usize;
    walk(&root, &mut |p| {
        if p.extension().and_then(|s| s.to_str()) != Some("decal_system") {
            return;
        }
        total += 1;
        let Ok(tag) = TagFile::read(p) else { failed += 1; return; };
        let Ok(ds) = DecalSystem::from_tag(&tag) else { failed += 1; return; };
        if ds.max_overlapping > 0 {
            with_overlap += 1;
            println!(
                "{}  max_overlapping={}  overlapping_threshold={:.3}  decals={}",
                p.strip_prefix(&root).unwrap_or(p).display(),
                ds.max_overlapping,
                ds.overlapping_threshold,
                ds.definitions.len(),
            );
        }
    });
    eprintln!(
        "\nsweep done: {total} decal_system tags  /  {with_overlap} definitions with max_overlapping > 0  /  {failed} failed reads",
    );
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() { walk(&p, f); } else { f(&p); }
    }
}
