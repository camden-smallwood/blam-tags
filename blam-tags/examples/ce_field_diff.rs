//! Diagnostic: show the aligned root-struct field diff between our JSON
//! schema and a shipped CE tag's embedded layout, for one group.
//! Includes ALL field types (custom / explanation / pad), so we can see
//! exactly which fields the blay carries that the JSON lacks (or vice
//! versa) when extending the JSON schemas.
//!
//! Usage (requires the `iostore` feature):
//! ```text
//! cargo run --release -p blam-tags --features iostore --example ce_field_diff -- \
//!     "/path/to/Paks" ../definitions/haloce_evolved damage_effect
//! ```

use std::error::Error;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};
use blam_tags::{TagFieldDefinition, TagFile, TagStructDefinition};

/// Rich per-field key: field-type discriminant + type name + raw name.
type Key = (String, String, String);

fn key(f: TagFieldDefinition<'_>) -> Key {
    (
        format!("{:?}", f.field_type()),
        f.type_name().to_owned(),
        f.name().to_owned(),
    )
}

fn fields(s: TagStructDefinition<'_>) -> Vec<Key> {
    s.fields().map(key).collect()
}

fn align_lcs(a: &[Key], b: &[Key]) -> Vec<(Option<Key>, Option<Key>)> {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push((Some(a[i - 1].clone()), Some(b[j - 1].clone())));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            out.push((Some(a[i - 1].clone()), None));
            i -= 1;
        } else {
            out.push((None, Some(b[j - 1].clone())));
            j -= 1;
        }
    }
    while i > 0 {
        out.push((Some(a[i - 1].clone()), None));
        i -= 1;
    }
    while j > 0 {
        out.push((None, Some(b[j - 1].clone())));
        j -= 1;
    }
    out.reverse();
    out
}

fn find_tag_bytes(paks: &Path, group: &str) -> Option<Vec<u8>> {
    let mut utocs = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension() == Some(OsStr::new("utoc")) {
                out.push(p);
            }
        }
    }
    walk(paks, &mut utocs);
    utocs.sort();
    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        for e in archive.ublock_entries() {
            if let Some((_n, g)) = parse_ublock_stem(&e.path)
                && g == group
                && let Ok(bytes) = archive.read(&e.path)
            {
                return Some(bytes);
            }
        }
    }
    None
}

fn show(label: &str, k: &Option<Key>) -> String {
    match k {
        Some((ft, ty, name)) => format!("{ft}/{ty} '{name}'"),
        None => format!("{label}"),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let paks = PathBuf::from(args.next().ok_or("usage: ce_field_diff <Paks> <Defs> <group>")?);
    let defs = PathBuf::from(args.next().ok_or("usage: ce_field_diff <Paks> <Defs> <group>")?);
    let group = args.next().ok_or("usage: ce_field_diff <Paks> <Defs> <group>")?;

    let expected = TagFile::new(defs.join(format!("{group}.json")))?;
    let bytes = find_tag_bytes(&paks, &group).ok_or("no tag of that group found")?;
    let actual = TagFile::read_from_bytes(&bytes)?;

    let e_root = expected.definitions().root_struct();
    let a_root = actual.definitions().root_struct();
    println!(
        "group {group}: JSON root '{}' size={} fields={}  |  blay root '{}' size={} fields={}\n",
        e_root.name(),
        e_root.size(),
        e_root.fields().count(),
        a_root.name(),
        a_root.size(),
        a_root.fields().count(),
    );

    let ef = fields(e_root);
    let af = fields(a_root);
    let col = 52usize;
    println!("{:<col$}  {:<col$}", "JSON (schema)", "BLAY (shipped tag)", col = col);
    println!("{0:-<col$}  {0:-<col$}", "", col = col);
    for (l, r) in align_lcs(&ef, &af) {
        let mark = match (&l, &r) {
            (Some(_), None) => "  <  MISSING IN BLAY",
            (None, Some(_)) => "  >  MISSING IN JSON",
            _ => "",
        };
        let ls: String = show("", &l).chars().take(col).collect();
        let rs: String = show("", &r).chars().take(col).collect();
        println!("{ls:<col$}  {rs:<col$}{mark}", col = col);
    }
    Ok(())
}
