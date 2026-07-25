//! Campaign Evolved schema-drift sweep.
//!
//! Mounts a CE `Paks` directory (all `.utoc` containers under it),
//! reads every `.ubulk` tag payload whose group we ship a JSON
//! definition for, and compares the tag's embedded layout against that
//! JSON via [`blam_tags::compare_root_layout`]. Tallies, per group,
//! how many tags land in Match / Drift / Incompatible, and lists the
//! drivers of any drift.
//!
//! This calibrates the import-validation policy for the "Import tag"
//! feature: if the shipped tags overwhelmingly Match, import can
//! hard-block on mismatch; if a group broadly Drifts, our JSON is the
//! thing to fix (or validation must warn, not block).
//!
//! Payloads are counted per-`.ubulk` across every pack (no base/patch
//! dedup) — the ratios, not the absolute counts, are what matter.
//!
//! Usage (requires the `iostore` feature):
//!
//! ```text
//! cargo run --release -p blam-tags --features iostore --example ce_schema_sweep -- \
//!     "/path/to/Meteorite/Content/Paks" [../definitions/haloce_evolved]
//! ```

use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};
use blam_tags::{compare_root_layout, LayoutSeverity, TagFile};

#[derive(Default)]
struct GroupTally {
    total: usize,
    matched: usize,
    drift: usize,
    incompatible: usize,
    read_err: usize,
    // Drift drivers (counted on each drifting tag).
    version_mism: usize,
    size_mism: usize,
    fieldcount_mism: usize,
    fieldlist_mism: usize,
    // One representative drift line for the report.
    example: Option<String>,
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

/// Group names we ship a JSON definition for (schema file stems).
fn known_groups(defs_dir: &Path) -> Result<HashSet<String>, Box<dyn Error>> {
    let mut out = HashSet::new();
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

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let paks_dir = PathBuf::from(args.next().ok_or(
        "usage: ce_schema_sweep <PaksDir> [DefsDir=../definitions/haloce_evolved]",
    )?);
    let defs_dir = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../definitions/haloce_evolved"));

    let known = known_groups(&defs_dir)?;
    let mut utocs = Vec::new();
    find_utocs(&paks_dir, &mut utocs);
    utocs.sort();
    println!(
        "Sweeping {} container(s) under {} against {} group definitions in {}\n",
        utocs.len(),
        paks_dir.display(),
        known.len(),
        defs_dir.display(),
    );

    // Expected (JSON-derived) tag per group, built once and reused.
    let mut expected: BTreeMap<String, Option<TagFile>> = BTreeMap::new();
    let mut tallies: BTreeMap<String, GroupTally> = BTreeMap::new();
    // Group long-names seen that we have NO JSON for (not parsed — could be
    // real groups we lack, or non-tag bulk whose fake "group" isn't real).
    let mut unknown_groups: BTreeMap<String, usize> = BTreeMap::new();
    let mut schema_load_errs: BTreeMap<String, String> = BTreeMap::new();

    for utoc in &utocs {
        let archive = match IoStoreArchive::open(utoc) {
            Ok(a) => a,
            Err(_) => continue, // index-less globals etc.
        };
        for e in archive.ublock_entries() {
            let Some((_tag_name, group)) = parse_ublock_stem(&e.path) else {
                continue;
            };
            if !known.contains(group) {
                *unknown_groups.entry(group.to_owned()).or_default() += 1;
                continue;
            }

            // Build (and cache) the expected layout for this group.
            let exp = expected.entry(group.to_owned()).or_insert_with(|| {
                match TagFile::new(defs_dir.join(format!("{group}.json"))) {
                    Ok(t) => Some(t),
                    Err(e) => {
                        schema_load_errs.insert(group.to_owned(), format!("{e}"));
                        None
                    }
                }
            });
            let Some(exp) = exp.as_ref() else { continue };

            let tally = tallies.entry(group.to_owned()).or_default();
            tally.total += 1;

            let bytes = match archive.read(&e.path) {
                Ok(b) => b,
                Err(_) => {
                    tally.read_err += 1;
                    continue;
                }
            };
            let actual = match TagFile::read_from_bytes(&bytes) {
                Ok(t) => t,
                Err(_) => {
                    tally.read_err += 1;
                    continue;
                }
            };

            let cmp = compare_root_layout(exp, &actual);
            match cmp.severity {
                LayoutSeverity::Match => tally.matched += 1,
                LayoutSeverity::Incompatible => tally.incompatible += 1,
                LayoutSeverity::Drift => {
                    tally.drift += 1;
                    if !cmp.version_match {
                        tally.version_mism += 1;
                    }
                    if !cmp.root_size_match {
                        tally.size_mism += 1;
                    }
                    if !cmp.field_count_match {
                        tally.fieldcount_mism += 1;
                    }
                    if !cmp.field_diffs.is_empty() {
                        tally.fieldlist_mism += 1;
                    }
                    if tally.example.is_none() {
                        tally.example = Some(format!(
                            "size {}→{} (Δ{}), fields {}→{}, ver {}→{}, {} field diffs",
                            cmp.expected_root_size,
                            cmp.actual_root_size,
                            cmp.actual_root_size as isize - cmp.expected_root_size as isize,
                            cmp.expected_field_count,
                            cmp.actual_field_count,
                            cmp.expected_version,
                            cmp.actual_version,
                            cmp.field_diffs.len(),
                        ));
                    }
                }
            }
        }
    }

    // Global roll-up.
    let mut g_total = 0usize;
    let mut g_match = 0usize;
    let mut g_drift = 0usize;
    let mut g_incompat = 0usize;
    let mut g_readerr = 0usize;
    for t in tallies.values() {
        g_total += t.total;
        g_match += t.matched;
        g_drift += t.drift;
        g_incompat += t.incompatible;
        g_readerr += t.read_err;
    }

    println!("=== Per-group results (groups with any drift/incompat first) ===\n");
    let mut rows: Vec<(&String, &GroupTally)> = tallies.iter().collect();
    rows.sort_by(|a, b| {
        let bad = |t: &GroupTally| t.drift + t.incompatible + t.read_err;
        bad(b.1)
            .cmp(&bad(a.1))
            .then_with(|| b.1.total.cmp(&a.1.total))
            .then_with(|| a.0.cmp(b.0))
    });
    for (group, t) in &rows {
        let clean = t.drift == 0 && t.incompatible == 0 && t.read_err == 0;
        let flag = if clean { "  " } else { "!!" };
        println!(
            "{flag} {group:32} total={:5}  match={:5}  drift={:4}  incompat={:4}  readerr={:3}",
            t.total, t.matched, t.drift, t.incompatible, t.read_err
        );
        if !clean {
            if t.version_mism + t.size_mism + t.fieldcount_mism + t.fieldlist_mism > 0 {
                println!(
                    "       drift drivers: version={} size={} field_count={} field_list={}",
                    t.version_mism, t.size_mism, t.fieldcount_mism, t.fieldlist_mism
                );
            }
            if let Some(ex) = &t.example {
                println!("       e.g. {ex}");
            }
        }
    }

    println!("\n=== Summary ===");
    println!("  groups compared      : {}", tallies.len());
    println!("  tags compared        : {g_total}");
    println!(
        "  MATCH                : {g_match}  ({:.1}%)",
        pct(g_match, g_total)
    );
    println!(
        "  DRIFT                : {g_drift}  ({:.1}%)",
        pct(g_drift, g_total)
    );
    println!(
        "  INCOMPATIBLE         : {g_incompat}  ({:.1}%)",
        pct(g_incompat, g_total)
    );
    println!("  read/parse errors    : {g_readerr}");

    if !schema_load_errs.is_empty() {
        println!("\n=== Groups whose JSON failed to load ({}): ===", schema_load_errs.len());
        for (g, e) in &schema_load_errs {
            println!("  {g:32} {e}");
        }
    }

    if !unknown_groups.is_empty() {
        // These weren't parsed; most are non-tag bulk data. Names that look
        // like real tag groups here would be groups we lack a JSON for.
        let mut u: Vec<(&String, &usize)> = unknown_groups.iter().collect();
        u.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        println!(
            "\n=== Ublock group-names with no shipped JSON ({} distinct; not parsed) ===",
            u.len()
        );
        for (g, n) in u.iter().take(60) {
            println!("  {g:40} {n}");
        }
        if u.len() > 60 {
            println!("  … {} more", u.len() - 60);
        }
    }

    Ok(())
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}
