//! Full-corpus schema validator.
//!
//! Like `schema_match`, but scans **every** tag of each group rather
//! than the first one it finds. A group passes if *at least one* tag in
//! the corpus has a root-struct layout (size + field count) matching
//! the schema. Groups with zero corpus tags are reported as SKIP.
//!
//! Rationale: Bungie/343 reshaped struct definitions throughout
//! development, and any individual tag carries the layout that was
//! current at *its* save time. A schema can be "correct" against
//! current code but disagree with an older tag. Requiring at least
//! one match against the corpus catches schemas that disagree with
//! *every* tag (a real bug) without rejecting schemas that simply
//! drifted past some older tags.
//!
//! The size/field-count/field-diff comparison is the shared
//! [`blam_tags::compare_root_layout`] API.
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p blam-tags --example schema_match_full -- \
//!     definitions/haloreach_mcc /Users/camden/Halo/haloreach_mcc/tags
//! ```

use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use blam_tags::{compare_root_layout, FieldDiff, TagFile};

fn collect_tags_with_ext(root: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(root) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_tags_with_ext(&path, ext, out);
        } else if path.extension() == Some(OsStr::new(ext)) {
            out.push(path);
        }
    }
}

fn list_group_schemas(defs_dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(defs_dir)? {
        let path = entry?.path();
        if path.extension() == Some(OsStr::new("json"))
            && path.file_name() != Some(OsStr::new("_meta.json"))
        {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

struct GroupResult {
    group: String,
    schema_size: usize,
    schema_fields: usize,
    total_tags: usize,
    matching_tags: usize,
    closest_miss: Option<ClosestMiss>,
    schema_load_err: Option<String>,
    tag_errors: usize,
}

struct ClosestMiss {
    path: PathBuf,
    tag_size: usize,
    tag_fields_count: usize,
    delta: isize,
    /// Differing rows of the root-struct field alignment (from
    /// `compare_root_layout`); a matched field yields no row.
    diffs: Vec<FieldDiff>,
}

fn check_group(schema_path: &Path, tags_root: &Path) -> GroupResult {
    let group = schema_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();

    let schema_tag = match TagFile::new(schema_path) {
        Ok(t) => t,
        Err(e) => {
            return GroupResult {
                group,
                schema_size: 0,
                schema_fields: 0,
                total_tags: 0,
                matching_tags: 0,
                closest_miss: None,
                schema_load_err: Some(format!("{e}")),
                tag_errors: 0,
            };
        }
    };
    let schema_root = schema_tag.definitions().root_struct();
    let schema_size = schema_root.size();
    let schema_fields_count = schema_root.fields().count();

    let mut tags: Vec<PathBuf> = Vec::new();
    collect_tags_with_ext(tags_root, &group, &mut tags);

    let mut matching_tags = 0;
    let mut closest_miss: Option<ClosestMiss> = None;
    let mut tag_errors = 0;

    for tag_path in &tags {
        let real = match TagFile::read(tag_path) {
            Ok(t) => t,
            Err(_) => {
                tag_errors += 1;
                continue;
            }
        };
        let cmp = compare_root_layout(&schema_tag, &real);

        if cmp.root_size_match && cmp.field_count_match {
            matching_tags += 1;
        } else {
            let delta = cmp.expected_root_size as isize - cmp.actual_root_size as isize;
            let abs = delta.unsigned_abs();
            let take = match &closest_miss {
                None => true,
                Some(prev) => abs < prev.delta.unsigned_abs(),
            };
            if take {
                closest_miss = Some(ClosestMiss {
                    path: tag_path.clone(),
                    tag_size: cmp.actual_root_size,
                    tag_fields_count: cmp.actual_field_count,
                    delta,
                    diffs: cmp.field_diffs,
                });
            }
        }
    }

    GroupResult {
        group,
        schema_size,
        schema_fields: schema_fields_count,
        total_tags: tags.len(),
        matching_tags,
        closest_miss,
        schema_load_err: None,
        tag_errors,
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let defs_dir = PathBuf::from(
        args.next().ok_or("usage: schema_match_full <DEFS_DIR> <TAGS_ROOT>")?,
    );
    let tags_root = PathBuf::from(
        args.next().ok_or("usage: schema_match_full <DEFS_DIR> <TAGS_ROOT>")?,
    );

    let schemas = list_group_schemas(&defs_dir)?;
    println!(
        "Checking {} schemas under {} against tags in {}\n",
        schemas.len(),
        defs_dir.display(),
        tags_root.display()
    );

    let mut pass = 0usize;
    let mut fail = Vec::<GroupResult>::new();
    let mut skip = Vec::<String>::new();
    let mut schema_err = Vec::<GroupResult>::new();

    for schema_path in &schemas {
        let r = check_group(schema_path, &tags_root);
        if r.schema_load_err.is_some() {
            println!("  ERR   {:40}  {}", r.group, r.schema_load_err.as_deref().unwrap_or(""));
            schema_err.push(r);
            continue;
        }
        if r.total_tags == 0 {
            skip.push(r.group);
            continue;
        }
        if r.matching_tags > 0 {
            pass += 1;
            println!(
                "  PASS  {:40}  {}/{} matches",
                r.group, r.matching_tags, r.total_tags
            );
        } else {
            println!(
                "  FAIL  {:40}  0/{} matches  closest Δ={}",
                r.group,
                r.total_tags,
                r.closest_miss.as_ref().map(|m| m.delta).unwrap_or(0),
            );
            fail.push(r);
        }
    }

    println!();
    println!("Summary ({} schemas):", schemas.len());
    println!("  PASS              : {pass}");
    println!("  FAIL              : {}", fail.len());
    println!("  SKIP (no tags)    : {}", skip.len());
    println!("  schema load error : {}", schema_err.len());

    if !fail.is_empty() {
        println!("\nFailing groups (no matching tag in corpus):\n");
        let col_w = 56usize; // each side's column width
        for r in &fail {
            let Some(miss) = &r.closest_miss else {
                println!("  {:40}  (no readable tag)", r.group);
                continue;
            };
            println!(
                "── {}  schema size={} fields={}  vs tag size={} fields={} (Δ={})",
                r.group,
                r.schema_size,
                r.schema_fields,
                miss.tag_size,
                miss.tag_fields_count,
                miss.delta,
            );
            println!("   e.g. {}", miss.path.display());
            // Header
            println!(
                "   {:<col_w$}  │  {:<col_w$}",
                "SCHEMA", "TAG (closest)",
                col_w = col_w,
            );
            println!("   {0:─<col_w$}──┼──{0:─<col_w$}", "", col_w = col_w);
            if miss.diffs.is_empty() {
                println!("   (no field-list drift on wire-significant fields; size still differs by {} — primitive type drift)", miss.delta);
            }
            for diff in &miss.diffs {
                let l_str = match &diff.expected {
                    Some((ty, name)) => format!("{ty} '{name}'"),
                    None => String::new(),
                };
                let r_str = match &diff.actual {
                    Some((ty, name)) => format!("{ty} '{name}'"),
                    None => String::new(),
                };
                let mark = match (&diff.expected, &diff.actual) {
                    (Some(_), None) => '<',
                    (None, Some(_)) => '>',
                    _ => ' ',
                };
                let l_disp: String = l_str.chars().take(col_w).collect();
                let r_disp: String = r_str.chars().take(col_w).collect();
                println!("   {l_disp:<col_w$} {mark}│ {mark}{r_disp:<col_w$}", col_w = col_w);
            }
            if r.tag_errors > 0 {
                println!("   ({} tag(s) failed to read)", r.tag_errors);
            }
            println!();
        }
    }

    if !schema_err.is_empty() {
        println!("\nSchema load errors:");
        for r in &schema_err {
            println!("  {:40}  {}", r.group, r.schema_load_err.as_deref().unwrap_or(""));
        }
    }

    Ok(())
}
