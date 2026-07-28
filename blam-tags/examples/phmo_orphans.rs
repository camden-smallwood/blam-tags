//! Probe: across a loose MCC tag kit, how many `physics_model` polyhedra are
//! reachable only through a MOPP/list indirection — i.e. invisible to
//! `build_phmo_parent_lookup`, which reads the `rigid bodies` block alone?
//!
//! Run:
//!   cargo run -p blam-tags --example phmo_orphans -- /Users/camden/Halo/haloreach_mcc/tags

use blam_tags::file::TagFile;
use blam_tags::JmsFile;

fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "physics_model") {
            out.push(path);
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).expect("tags dir");
    let mut files = Vec::new();
    walk(std::path::Path::new(&root), &mut files);
    files.sort();

    let mut total = 0;
    let mut with_list = 0;
    let mut polyhedra_total = 0;
    let mut orphaned = 0;
    let mut multi_list = 0;
    let mut tiling_mismatch = 0;
    let mut worst: Vec<(usize, String)> = Vec::new();

    for file in &files {
        let Ok(tag) = TagFile::read(file) else { continue };
        let root_struct = tag.root();
        total += 1;
        let polyhedra = root_struct
            .field_path("polyhedra")
            .and_then(|f| f.as_block())
            .map(|b| b.len())
            .unwrap_or(0);
        polyhedra_total += polyhedra;
        let mut direct: std::collections::HashSet<(i64, i64)> = Default::default();
        if let Some(rbs) = root_struct.field_path("rigid bodies").and_then(|f| f.as_block()) {
            for i in 0..rbs.len() {
                let rb = rbs.element(i).unwrap();
                if let Some(sr) = rb.field("shape reference").and_then(|f| f.as_struct()) {
                    let ty = sr.read_int_any("shape type").unwrap_or(-1) as i64;
                    let idx = sr.read_int_any("shape").unwrap_or(-1) as i64;
                    direct.insert((ty, idx));
                }
            }
        }
        if root_struct
            .field_path("lists")
            .and_then(|f| f.as_block())
            .is_some_and(|b| b.len() > 0)
        {
            with_list += 1;
        }
        // What the exporter actually emits, now that containers are followed.
        let n = match JmsFile::from_physics_model(&tag) {
            Ok(jms) => jms
                .spheres
                .iter()
                .map(|s| s.parent)
                .chain(jms.capsules.iter().map(|c| c.parent))
                .chain(jms.boxes.iter().map(|b| b.parent))
                .chain(jms.convex_shapes.iter().map(|c| c.parent))
                .filter(|p| *p < 0)
                .count(),
            Err(_) => 0,
        };
        orphaned += n;
        if n > 0 {
            worst.push((n, file.display().to_string()));
        }
        // Do list children tile the `list shapes` block contiguously? If they do,
        // a list's slice is just the running sum of the sizes before it.
        let lists = root_struct.field_path("lists").and_then(|f| f.as_block());
        let shapes = root_struct
            .field_path("list shapes")
            .and_then(|f| f.as_block())
            .map(|b| b.len())
            .unwrap_or(0);
        if let Some(lists) = lists {
            if lists.len() > 1 {
                multi_list += 1;
                let sum: i64 = (0..lists.len())
                    .map(|i| {
                        lists
                            .element(i)
                            .and_then(|e| e.read_int_any("child shapes size"))
                            .unwrap_or(0) as i64
                    })
                    .sum();
                if sum != shapes as i64 {
                    tiling_mismatch += 1;
                    if tiling_mismatch <= 5 {
                        println!(
                            "    TILING: {} lists sum to {sum} but `list shapes` has {shapes} -- {}",
                            lists.len(),
                            file.display()
                        );
                    }
                }
            }
        }
    }

    let mut rb_types: std::collections::BTreeMap<i64, usize> = Default::default();
    let mut ls_types: std::collections::BTreeMap<i64, usize> = Default::default();
    for file in &files {
        let Ok(tag) = TagFile::read(file) else { continue };
        let root_struct = tag.root();
        if let Some(rbs) = root_struct.field_path("rigid bodies").and_then(|f| f.as_block()) {
            for i in 0..rbs.len() {
                if let Some(sr) = rbs
                    .element(i)
                    .and_then(|e| e.field("shape reference").and_then(|f| f.as_struct()))
                {
                    *rb_types.entry(sr.read_int_any("shape type").unwrap_or(-1) as i64).or_default() += 1;
                }
            }
        }
        if let Some(ls) = root_struct.field_path("list shapes").and_then(|f| f.as_block()) {
            for i in 0..ls.len() {
                if let Some(sr) = ls
                    .element(i)
                    .and_then(|e| e.field("shape reference").and_then(|f| f.as_struct()))
                {
                    *ls_types.entry(sr.read_int_any("shape type").unwrap_or(-1) as i64).or_default() += 1;
                }
            }
        }
    }
    println!("  rigid-body shape types: {rb_types:?}");
    println!("  list-shape shape types: {ls_types:?}");

    worst.sort_by(|a, b| b.0.cmp(&a.0));
    println!("{root}");
    println!("  physics_models read: {total}");
    println!("  with a `lists` block: {with_list}");
    println!("  polyhedra: {polyhedra_total}; JMS shapes emitted with parent = -1: {orphaned}");
    for (n, path) in worst.iter().take(8) {
        println!("    {n:>3}  {path}");
    }
    println!("  {} model(s) affected", worst.len());
    println!("  models with >1 list: {multi_list}, of which {tiling_mismatch} do not tile `list shapes` by running sum");
}
