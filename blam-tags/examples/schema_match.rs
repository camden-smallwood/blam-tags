//! Schema-vs-tag layout validator.
//!
//! For each per-group JSON schema under `<DEFS_DIR>`, finds one real
//! tag of that group under `<TAGS_ROOT>` and compares the dumped
//! schema's root struct against the tag's embedded layout. Reports
//! pass / mismatch / no-sample / read-error per group, then totals.
//!
//! Dev-era drift means individual tags don't always match the latest
//! schema exactly. The goal here isn't 100% — it's to confirm the
//! newly-dumped JSON is sane (internally consistent + at least one
//! representative tag agrees on the top-level shape).
//!
//! Usage:
//!
//! ```text
//! cargo run --release -p blam-tags --example schema_match -- \
//!     definitions/haloreach_mcc /Users/camden/Halo/haloreach_mcc/tags
//! ```

use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use blam_tags::{compare_root_layout, TagFile};

fn find_one_tag_with_ext(root: &Path, ext: &str) -> Option<PathBuf> {
    let read = std::fs::read_dir(root).ok()?;
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(hit) = find_one_tag_with_ext(&path, ext) {
                return Some(hit);
            }
        } else if path.extension() == Some(OsStr::new(ext)) {
            return Some(path);
        }
    }
    None
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

enum Outcome {
    Ok,
    SizeMismatch { schema: usize, tag: usize },
    FieldCountMismatch { schema: usize, tag: usize },
    NoSample,
    SchemaLoadErr(String),
    TagReadErr(String),
}

fn check_group(schema_path: &Path, tags_root: &Path) -> (String, Option<PathBuf>, Outcome) {
    let group_name = schema_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();

    let schema_tag = match TagFile::new(schema_path) {
        Ok(t) => t,
        Err(e) => return (group_name, None, Outcome::SchemaLoadErr(format!("{e}"))),
    };

    let tag_path = match find_one_tag_with_ext(tags_root, &group_name) {
        Some(p) => p,
        None => return (group_name, None, Outcome::NoSample),
    };

    let real_tag = match TagFile::read(&tag_path) {
        Ok(t) => t,
        Err(e) => return (group_name, Some(tag_path), Outcome::TagReadErr(format!("{e}"))),
    };

    let cmp = compare_root_layout(&schema_tag, &real_tag);
    if !cmp.root_size_match {
        return (
            group_name,
            Some(tag_path),
            Outcome::SizeMismatch { schema: cmp.expected_root_size, tag: cmp.actual_root_size },
        );
    }
    if !cmp.field_count_match {
        return (
            group_name,
            Some(tag_path),
            Outcome::FieldCountMismatch {
                schema: cmp.expected_field_count,
                tag: cmp.actual_field_count,
            },
        );
    }

    (group_name, Some(tag_path), Outcome::Ok)
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let defs_dir = PathBuf::from(
        args.next().ok_or("usage: schema_match <DEFS_DIR> <TAGS_ROOT>")?,
    );
    let tags_root = PathBuf::from(
        args.next().ok_or("usage: schema_match <DEFS_DIR> <TAGS_ROOT>")?,
    );

    let schemas = list_group_schemas(&defs_dir)?;
    println!(
        "Checking {} schemas under {} against tags in {}\n",
        schemas.len(),
        defs_dir.display(),
        tags_root.display()
    );

    let mut ok = 0usize;
    let mut size_miss = Vec::<(String, usize, usize)>::new();
    let mut field_miss = Vec::<(String, usize, usize)>::new();
    let mut no_sample = Vec::<String>::new();
    let mut schema_errs = Vec::<(String, String)>::new();
    let mut tag_errs = Vec::<(String, String)>::new();

    for schema_path in &schemas {
        let (group, _, outcome) = check_group(schema_path, &tags_root);
        match outcome {
            Outcome::Ok => {
                ok += 1;
            }
            Outcome::SizeMismatch { schema, tag } => {
                println!("  SIZE    {group:40}  schema={schema}  tag={tag}  (Δ={})",
                    schema as isize - tag as isize);
                size_miss.push((group, schema, tag));
            }
            Outcome::FieldCountMismatch { schema, tag } => {
                println!("  FIELDS  {group:40}  schema={schema}  tag={tag}");
                field_miss.push((group, schema, tag));
            }
            Outcome::NoSample => {
                no_sample.push(group);
            }
            Outcome::SchemaLoadErr(e) => {
                println!("  SCHEMA  {group:40}  {e}");
                schema_errs.push((group, e));
            }
            Outcome::TagReadErr(e) => {
                println!("  READ    {group:40}  {e}");
                tag_errs.push((group, e));
            }
        }
    }

    println!();
    println!("Summary ({} schemas):", schemas.len());
    println!("  OK                   : {ok}");
    println!("  size mismatch        : {}", size_miss.len());
    println!("  field count mismatch : {}", field_miss.len());
    println!("  no sample tag        : {}", no_sample.len());
    println!("  schema load error    : {}", schema_errs.len());
    println!("  tag read error       : {}", tag_errs.len());

    if !no_sample.is_empty() {
        println!("\nGroups with no sample tag found under {}:", tags_root.display());
        for g in &no_sample {
            println!("  {g}");
        }
    }

    Ok(())
}
