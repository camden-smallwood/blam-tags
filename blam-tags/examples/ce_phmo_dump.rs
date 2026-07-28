//! Probe: how a Campaign Evolved `physics_model` binds its shapes to nodes.
//!
//! `build_phmo_parent_lookup` maps (shape type, shape index) -> node straight off
//! the `rigid bodies` block. The pelican's 38 polyhedra come out with parent -1,
//! so print what the rigid bodies actually reference and what indirection blocks
//! the tag carries.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_phmo_dump -- <paks> <substr>

use std::sync::Arc;

use blam_tags::file::TagFile;
use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};

const DEFAULT_PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

/// How many physics models bind shapes through a MOPP/list indirection the
/// parent lookup does not follow?
fn sweep(paks: &str) {
    let mut utocs: Vec<_> = std::fs::read_dir(paks)
        .expect("read paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut total = 0;
    let mut with_list = 0;
    let mut orphaned_polyhedra = 0;
    let mut total_polyhedra = 0;
    let mut worst: Vec<(usize, String)> = Vec::new();

    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        for e in archive.ublock_entries() {
            let Some((_n, group)) = parse_ublock_stem(&e.path) else { continue };
            if group != "physics_model" {
                continue;
            }
            let Ok(bytes) = archive.read(&e.path) else { continue };
            let Ok(tag) = TagFile::read_from_bytes(&bytes) else { continue };
            let root = tag.root();
            total += 1;
            let polyhedra = root
                .field_path("polyhedra")
                .and_then(|f| f.as_block())
                .map(|b| b.len())
                .unwrap_or(0);
            total_polyhedra += polyhedra;
            // Every (shape type, shape index) a rigid body names directly.
            let mut direct: std::collections::HashSet<(i64, i64)> = Default::default();
            if let Some(rbs) = root.field_path("rigid bodies").and_then(|f| f.as_block()) {
                for i in 0..rbs.len() {
                    let rb = rbs.element(i).unwrap();
                    if let Some(sr) = rb.field("shape reference").and_then(|f| f.as_struct()) {
                        let ty = sr.read_int_any("shape type").unwrap_or(-1) as i64;
                        let idx = sr.read_int_any("shape").unwrap_or(-1) as i64;
                        direct.insert((ty, idx));
                    }
                }
            }
            let lists = root
                .field_path("lists")
                .and_then(|f| f.as_block())
                .map(|b| b.len())
                .unwrap_or(0);
            if lists > 0 {
                with_list += 1;
            }
            // Polyhedron = shape type 4.
            let orphans = (0..polyhedra)
                .filter(|i| !direct.contains(&(4, *i as i64)))
                .count();
            orphaned_polyhedra += orphans;
            if orphans > 0 {
                worst.push((orphans, e.path.to_ascii_lowercase()));
            }
        }
    }
    worst.sort_by(|a, b| b.0.cmp(&a.0));
    println!("physics_models: {total}");
    println!("  with a `lists` block (MOPP/list indirection): {with_list}");
    println!("  polyhedra: {total_polyhedra}, of which {orphaned_polyhedra} are not named by any rigid body");
    for (n, path) in worst.iter().take(12) {
        println!("    {n:>3} orphaned: {path}");
    }
    println!("  ... {} model(s) affected", worst.len());
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let filter = args
        .next()
        .map(|f| {
            if f == "--sweep" {
                sweep(&paks);
                std::process::exit(0);
            }
            f
        })
        .unwrap_or_else(|| "vehicles/human/pelican/pelican".to_string())
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut found: Option<(Arc<IoStoreArchive>, String, String)> = None;
    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        let archive = Arc::new(archive);
        for e in archive.ublock_entries() {
            let Some((_n, group)) = parse_ublock_stem(&e.path) else { continue };
            if group != "physics_model" {
                continue;
            }
            let norm = e.path.to_ascii_lowercase().replace('\\', "/");
            if norm.contains(&filter) {
                found = Some((archive.clone(), norm, e.path.clone()));
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let Some((archive, norm, rel)) = found else {
        eprintln!("no physics_model matched {filter:?}");
        std::process::exit(1);
    };
    println!("=== {norm} ===");
    let bytes = archive.read(&rel).expect("read");
    let tag = TagFile::read_from_bytes(&bytes).expect("parse");
    let root = tag.root();

    for name in [
        "nodes",
        "materials",
        "rigid bodies",
        "spheres",
        "pills",
        "boxes",
        "triangles",
        "polyhedra",
        "polyhedron four vectors",
        "polyhedron plane equations",
        "lists",
        "list shapes",
        "mopps",
        "mopp codes",
        "regions",
        "nodes^",
    ] {
        let len = root
            .field_path(name)
            .and_then(|f| f.as_block())
            .map(|b| b.len());
        if let Some(len) = len {
            println!("  {name}: {len}");
        }
    }

    if let Some(rbs) = root.field_path("rigid bodies").and_then(|f| f.as_block()) {
        println!("\n  rigid bodies:");
        for i in 0..rbs.len() {
            let rb = rbs.element(i).unwrap();
            let node = rb.read_int_any("node").map(|v| v as i64).unwrap_or(-1);
            let (ty, idx) = rb
                .field("shape reference")
                .and_then(|f| f.as_struct())
                .map(|sr| {
                    (
                        sr.read_int_any("shape type").unwrap_or(-1),
                        sr.read_int_any("shape").unwrap_or(-1),
                    )
                })
                .unwrap_or((-1, -1));
            println!("    [{i}] node={node} shape_type={ty} shape_index={idx}");
        }
    }

    if let Some(mopps) = root.field_path("mopps").and_then(|f| f.as_block()) {
        println!("\n  mopps:");
        for i in 0..mopps.len() {
            let m = mopps.element(i).unwrap();
            println!("    [{i}] list index={:?}", m.read_block_index("list"));
        }
    }
    if let Some(lists) = root.field_path("lists").and_then(|f| f.as_block()) {
        println!("\n  lists:");
        for i in 0..lists.len() {
            let l = lists.element(i).unwrap();
            let count = l.read_int_any("child shapes size").unwrap_or(-1);
            println!("    [{i}] child shapes size={count}");
        }
    }
    if let Some(ls) = root.field_path("list shapes").and_then(|f| f.as_block()) {
        println!("\n  list shapes ({}):", ls.len());
        for i in 0..ls.len().min(6) {
            let e = ls.element(i).unwrap();
            let (ty, idx) = e
                .field("shape reference")
                .and_then(|f| f.as_struct())
                .map(|sr| {
                    (
                        sr.read_int_any("shape type").unwrap_or(-1),
                        sr.read_int_any("shape").unwrap_or(-1),
                    )
                })
                .unwrap_or((-1, -1));
            println!("    [{i}] shape_type={ty} shape_index={idx}");
        }
    }

    if let Some(poly) = root.field_path("polyhedra").and_then(|f| f.as_block()) {
        println!("\n  polyhedra (first 6):");
        for i in 0..poly.len().min(6) {
            let p = poly.element(i).unwrap();
            let name = p
                .field("base")
                .and_then(|f| f.as_struct())
                .and_then(|b| b.read_string_id("name"))
                .or_else(|| p.read_string_id("name"))
                .unwrap_or_default();
            println!("    [{i}] name={name:?}");
        }
    }
}
