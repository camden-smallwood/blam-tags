//! Round-trip validation sweep for 100% field-path compatibility.
//!
//! For every field in every tag, build the resolvable `clean_name#ordinal` path
//! from the root (exactly as Baboon's `append_field_path_for` does) and confirm
//! it resolves back to that *exact* field. On failure, find the **break point**
//! (the shallowest container descent that fails) and aggregate into failure
//! *classes* keyed by (group, canonical break path), so the cascade of child
//! failures under one broken block collapses to a single actionable class.
//!
//! Usage (from the blam-tags workspace root, so `definitions/` resolves):
//!   cargo run --release --example field_name_sweep -- <game> <tags_root>

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blam_tags::classic::{read_classic_tag_file, ClassicHeader};
use blam_tags::{TagFieldPath, TagFile, TagLayout, TagStruct};

#[derive(Default)]
struct Class {
    count: u64,
    example_tag: String,
    example_path: String,
    reason: &'static str,
}

#[derive(Default)]
struct Stats {
    tags_ok: usize,
    tags_unloadable: usize,
    fields_checked: u64,
    markup_fields: u64,
    // Failure classes keyed by "group | canonical-break-path".
    classes: BTreeMap<String, Class>,
    // Unloadable reasons keyed by a short reason string.
    unloadable: BTreeMap<String, (u64, String)>,
    markup_samples: BTreeMap<String, String>,
    path_mismatches: u64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let game = args.next().expect("usage: field_name_sweep <game> <tags_root>");
    let tags_root = PathBuf::from(args.next().expect("usage: field_name_sweep <game> <tags_root>"));
    let meta = load_meta_index(Path::new("definitions").join(&game).join("_meta.json"));

    let mut stats = Stats::default();
    let mut files = Vec::new();
    collect_files(&tags_root, &mut files);
    eprintln!("[{game}] {} candidate tag files", files.len());

    for (i, path) in files.iter().enumerate() {
        if i % 5000 == 0 && i > 0 {
            eprintln!(
                "[{game}] {i}/{} ok={} unloadable={} fields={} classes={}",
                files.len(), stats.tags_ok, stats.tags_unloadable,
                stats.fields_checked, stats.classes.len()
            );
        }
        match load_tag(path, &game, &meta) {
            Ok(tag) => {
                stats.tags_ok += 1;
                let root = tag.root();
                check_struct(&root, &root, "", path, &mut stats);
            }
            Err(reason) => {
                stats.tags_unloadable += 1;
                let entry = stats.unloadable.entry(reason).or_default();
                entry.0 += 1;
                if entry.1.is_empty() {
                    entry.1 = path.display().to_string();
                }
            }
        }
    }

    println!("\n================ {game} ================");
    println!("tags loaded ok       : {}", stats.tags_ok);
    println!("tags unloadable      : {}", stats.tags_unloadable);
    println!("fields checked       : {}", stats.fields_checked);
    println!("fields with markup   : {}", stats.markup_fields);
    println!("path round-trip drift: {}", stats.path_mismatches);
    let total_fail: u64 = stats.classes.values().map(|c| c.count).sum();
    println!("resolve failures     : {total_fail}  ({} distinct classes)", stats.classes.len());

    if !stats.classes.is_empty() {
        println!("\n-- FAILURE CLASSES (group | break-point) --");
        let mut rows: Vec<_> = stats.classes.iter().collect();
        rows.sort_by(|a, b| b.1.count.cmp(&a.1.count));
        for (key, c) in rows {
            println!(
                "  [{:>7}x] {key}  ({})\n            e.g. {} :: {}",
                c.count, c.reason, c.example_tag, c.example_path
            );
        }
    }
    if !stats.unloadable.is_empty() {
        println!("\n-- UNLOADABLE REASONS --");
        let mut rows: Vec<_> = stats.unloadable.iter().collect();
        rows.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
        for (reason, (n, ex)) in rows {
            println!("  [{n:>7}x] {reason}\n            e.g. {ex}");
        }
    }
    println!("\n{}", if total_fail == 0 { "PASS \u{2713}" } else { "FAIL \u{2717}" });
}

fn check_struct(root: &TagStruct<'_>, cur: &TagStruct<'_>, prefix: &str, tag_path: &Path, stats: &mut Stats) {
    for field in cur.fields() {
        let raw = field.name().to_owned();
        let clean = field.clean_name().into_owned();
        let ordinal = field.ordinal();
        let seg = format!("{clean}#{ordinal}");
        let full = if prefix.is_empty() { seg } else { format!("{prefix}/{seg}") };

        stats.fields_checked += 1;
        if clean != raw {
            stats.markup_fields += 1;
            stats.markup_samples.entry(raw.clone()).or_insert_with(|| clean.clone());
        }
        if TagFieldPath::parse(&full).to_string() != full {
            stats.path_mismatches += 1;
        }

        match root.field_path(&full) {
            Some(r) if r.name() == raw && r.ordinal() == ordinal => {}
            other => {
                let reason = if other.is_some() { "resolved-to-wrong-field" } else { "did-not-resolve" };
                record_failure(root, tag_path, &full, reason, stats);
            }
        }

        if let Some(inner) = field.as_struct() {
            check_struct(root, &inner, &full, tag_path, stats);
        } else if let Some(block) = field.as_block() {
            if let Some(el) = block.element(0) {
                check_struct(root, &el, &format!("{full}[0]"), tag_path, stats);
            }
        } else if let Some(array) = field.as_array() {
            if let Some(el) = array.element(0) {
                check_struct(root, &el, &format!("{full}[0]"), tag_path, stats);
            }
        }
    }
}

/// Find the shallowest failing point and record it as a class. Walks the path's
/// container prefixes; the first whose `descend` fails is the break (dedups the
/// cascade of child-field failures under one broken block).
fn record_failure(root: &TagStruct<'_>, tag_path: &Path, full: &str, leaf_reason: &'static str, stats: &mut Stats) {
    let segs: Vec<&str> = full.split('/').collect();
    let group = tag_path.extension().and_then(|e| e.to_str()).unwrap_or("?");

    // Find the shallowest container prefix (all but last segment) that won't descend.
    let mut break_path = full.to_string();
    let mut reason = leaf_reason;
    for i in 1..segs.len() {
        let prefix = segs[..i].join("/");
        if root.descend(&prefix).is_none() {
            break_path = prefix;
            reason = "descend-failed";
            break;
        }
    }

    let canon = TagFieldPath::parse(&break_path).strip_node_indices().to_string();
    let key = format!("{group} | {canon}");
    let entry = stats.classes.entry(key).or_insert_with(|| Class {
        example_tag: tag_path.display().to_string(),
        example_path: full.to_string(),
        reason,
        count: 0,
    });
    entry.count += 1;
}

fn load_tag(path: &Path, game: &str, meta: &BTreeMap<u32, String>) -> Result<TagFile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("io: {e}"))?;
    if let Some((header, _)) = ClassicHeader::parse(&bytes) {
        let group_tag = u32::from_be_bytes(header.group_tag);
        let name = meta.get(&group_tag).ok_or_else(|| {
            format!("classic group {:?} has no definition", String::from_utf8_lossy(&header.group_tag).trim_end())
        })?;
        let def_path = Path::new("definitions").join(game).join(format!("{name}.json"));
        let layout = TagLayout::from_json(&def_path).map_err(|_| format!("from_json failed: {name}"))?;
        return read_classic_tag_file(&bytes, layout).map_err(|e| format!("classic decode: {e:?}").chars().take(60).collect());
    }
    TagFile::read(path).map_err(|e| format!("modern read: {e:?}").chars().take(60).collect())
}

fn load_meta_index(meta_path: PathBuf) -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    let Ok(bytes) = std::fs::read(&meta_path) else { return map };
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    if let Some(obj) = value.get("tag_index").and_then(|v| v.as_object()) {
        for (tag_str, name) in obj {
            if let (Some(gt), Some(name)) = (blam_tags::parse_group_tag(tag_str), name.as_str()) {
                map.insert(gt, name.to_owned());
            }
        }
    }
    map
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() { collect_files(&path, out); } else if path.is_file() { out.push(path); }
    }
}
