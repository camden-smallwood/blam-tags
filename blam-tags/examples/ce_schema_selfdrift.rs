//! Campaign Evolved intra-group schema self-drift.
//!
//! Ignores our JSON entirely. For every `.ubulk` tag (of a group we
//! recognize), computes a recursive structural fingerprint of its
//! **embedded** layout (`blay`) — the set of every distinct struct shape
//! reachable from the root, each keyed by `guid + size + wire-field
//! list` — and buckets tags by group. A group with more than one
//! distinct fingerprint means two shipped tags of that group disagree on
//! their own schema (genuine save-time drift), as opposed to the whole
//! group merely being offset from our JSON.
//!
//! Usage (requires the `iostore` feature):
//!
//! ```text
//! cargo run --release -p blam-tags --features iostore --example ce_schema_selfdrift -- \
//!     "/path/to/Meteorite/Content/Paks" [../definitions/haloce_evolved]
//! ```

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};
use blam_tags::{field_key, TagFieldType, TagFile, TagStructDefinition};

const DEPTH_CAP: usize = 24;

/// Canonical one-line shape of a single struct: guid + size + its
/// wire-significant field keys (type:name). guid alone usually suffices,
/// but including size + fields makes zero-guid structs still distinguish.
fn struct_shape(s: TagStructDefinition<'_>) -> String {
    let guid: String = s.guid().iter().map(|b| format!("{b:02x}")).collect();
    let fields: Vec<String> = s
        .fields()
        .filter(|f| {
            !matches!(
                f.field_type(),
                TagFieldType::Custom | TagFieldType::Explanation | TagFieldType::Terminator,
            )
        })
        .map(|f| {
            let (t, n) = field_key(f);
            format!("{t}:{n}")
        })
        .collect();
    format!("{guid}|{}|{}", s.size(), fields.join(","))
}

/// Recursively collect the set of distinct struct shapes reachable from
/// `s`. Dedup by shape string doubles as cycle protection (a struct that
/// recurses into itself has the same shape and is only walked once).
fn collect_shapes(s: TagStructDefinition<'_>, visited: &mut BTreeSet<String>, depth: usize) {
    if depth > DEPTH_CAP {
        return;
    }
    let shape = struct_shape(s);
    if !visited.insert(shape) {
        return;
    }
    for f in s.fields() {
        if let Some(inner) = f.as_struct() {
            collect_shapes(inner, visited, depth + 1);
        } else if let Some(b) = f.as_block() {
            collect_shapes(b.struct_definition(), visited, depth + 1);
        } else if let Some(a) = f.as_array() {
            collect_shapes(a.struct_definition(), visited, depth + 1);
        } else if let Some(r) = f.as_resource() {
            collect_shapes(r.struct_definition(), visited, depth + 1);
        }
    }
}

fn find_utocs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_utocs(&path, out);
        } else if path.extension() == Some(OsStr::new("utoc")) {
            out.push(path);
        }
    }
}

fn known_groups(defs_dir: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut out = BTreeSet::new();
    for entry in std::fs::read_dir(defs_dir)? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("json"))
            && path.file_name() != Some(OsStr::new("_meta.json"))
            && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
        {
            out.insert(stem.to_owned());
        }
    }
    Ok(out)
}

/// One distinct embedded schema seen within a group.
struct Variant {
    count: usize,
    example: String,
    root_size: usize,
    root_fields: usize,
    struct_count: usize,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let paks_dir = PathBuf::from(args.next().ok_or(
        "usage: ce_schema_selfdrift <PaksDir> [DefsDir=../definitions/haloce_evolved]",
    )?);
    let defs_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../definitions/haloce_evolved"));

    let known = known_groups(&defs_dir)?;
    let mut utocs = Vec::new();
    find_utocs(&paks_dir, &mut utocs);
    utocs.sort();

    // group name -> (fingerprint -> Variant)
    let mut groups: BTreeMap<String, BTreeMap<String, Variant>> = BTreeMap::new();
    let mut total_tags = 0usize;

    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        for e in archive.ublock_entries() {
            let Some((_name, group)) = parse_ublock_stem(&e.path) else { continue };
            if !known.contains(group) {
                continue;
            }
            let Ok(bytes) = archive.read(&e.path) else { continue };
            let Ok(tag) = TagFile::read_from_bytes(&bytes) else { continue };
            total_tags += 1;

            let root = tag.definitions().root_struct();
            let root_size = root.size();
            let root_fields = root.fields().count();
            let mut visited = BTreeSet::new();
            collect_shapes(root, &mut visited, 0);
            let struct_count = visited.len();
            let fp = visited.into_iter().collect::<Vec<_>>().join("\n");

            let variants = groups.entry(group.to_owned()).or_default();
            let v = variants.entry(fp).or_insert_with(|| Variant {
                count: 0,
                example: format!("{}::{}", utoc.file_stem().and_then(|s| s.to_str()).unwrap_or("?"), e.path),
                root_size,
                root_fields,
                struct_count,
            });
            v.count += 1;
        }
    }

    let mut multi: Vec<(&String, &BTreeMap<String, Variant>)> =
        groups.iter().filter(|(_, v)| v.len() > 1).collect();
    multi.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));

    println!(
        "Self-drift over {} tags in {} groups ({} containers).\n",
        total_tags,
        groups.len(),
        utocs.len()
    );

    if multi.is_empty() {
        println!("Every group has EXACTLY ONE distinct embedded schema — no intra-group drift.");
    } else {
        println!(
            "{} group(s) have >1 distinct embedded schema (intra-group drift):\n",
            multi.len()
        );
        for (group, variants) in &multi {
            let total: usize = variants.values().map(|v| v.count).sum();
            println!("── {group}  ({} variants over {total} tags)", variants.len());
            let mut vs: Vec<&Variant> = variants.values().collect();
            vs.sort_by(|a, b| b.count.cmp(&a.count));
            for v in vs {
                println!(
                    "     ×{:<5} root_size={:<5} root_fields={:<3} structs={:<4} e.g. {}",
                    v.count, v.root_size, v.root_fields, v.struct_count, v.example
                );
            }
        }
    }

    let single = groups.len() - multi.len();
    println!("\n=== Summary ===");
    println!("  groups with 1 schema  : {single}");
    println!("  groups with >1 schema : {}", multi.len());
    println!("  tags scanned          : {total_tags}");

    Ok(())
}
