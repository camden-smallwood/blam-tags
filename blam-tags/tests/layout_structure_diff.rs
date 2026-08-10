//! Where a tag built from the definitions differs in *shape* from one the kit
//! wrote, for the same game and group.
//!
//! Run with `--ignored`. Kept as a diagnostic rather than an assertion because
//! it is measuring a known defect, not guarding a fixed one: the editing kits
//! reject a tag whose field list disagrees with their own, and this says exactly
//! where the disagreement is. What it found on HREK, which is the starting point
//! for fixing it:
//!
//! | group | ours | the kit's |
//! |---|---|---|
//! | `effect` | 96B / 34 fields | identical |
//! | `decal_system` | 60B / 12 fields | identical |
//! | `particle` | **496B** / 32 fields | **492B** / 34 fields |
//! | `cheap_particle_emitter` | 268B / **36** fields | 268B / **43** fields |
//!
//! Two distinct defects, both in the JSON-to-layout builder rather than in the
//! converter:
//!
//! 1. **Zero-width `custom` fields the dump omits.** `cheap_particle_emitter`
//!    agrees on the root's total size and still declares seven fewer fields, the
//!    missing ones including `custom` entries at indices 3 and 5. They occupy no
//!    bytes, so emitting them aligns the field list without moving any data -
//!    which is what a tool comparing field lists needs.
//! 2. **`particle`'s root is four bytes too large.** 496 against the kit's 492,
//!    so the `tmpl` expansion is being added where the kit does not add it.
//!
//! `effect` and `decal_system` roots are byte-identical, so whatever makes those
//! fail is not here. Note the honest limit of this comparison: a freshly built
//! tag has empty blocks, so only structs are descended into. Block element
//! shapes are unmeasured and are the next place to look.

use blam_tags::convert::clean_field_key;
use blam_tags::{TagFile, TagStruct};
use std::path::PathBuf;

fn kit(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from("D:/SteamLibrary/steamapps/common")
        .join(name)
        .join("tags");
    path.is_dir().then_some(path)
}

fn walk_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

/// `(name, type)` for every field, in order.
fn shape(value: TagStruct<'_>) -> Vec<(String, String)> {
    value
        .fields()
        .map(|field| {
            let key = clean_field_key(field.name());
            let name = if key.is_empty() {
                format!("<{}>", field.type_name())
            } else {
                key
            };
            (name, format!("{:?}", field.field_type()))
        })
        .collect()
}

/// Compare two structs by shape, reporting the first divergences under `path`.
fn compare(ours: TagStruct<'_>, theirs: TagStruct<'_>, path: &str, out: &mut Vec<String>) {
    let a = shape(ours);
    let b = shape(theirs);
    if ours.definition().size() != theirs.definition().size() || a.len() != b.len() {
        out.push(format!(
            "{path}: ours {}B/{} fields, kit {}B/{} fields",
            ours.definition().size(),
            a.len(),
            theirs.definition().size(),
            b.len()
        ));
    }
    // Field-by-field, so a rename or an inserted field is visible rather than
    // just a count.
    for index in 0..a.len().max(b.len()) {
        match (a.get(index), b.get(index)) {
            (Some(ours), Some(theirs)) if ours != theirs => {
                out.push(format!("{path}[{index}]: ours {ours:?}, kit {theirs:?}"));
            }
            (None, Some(theirs)) => out.push(format!("{path}[{index}]: missing, kit {theirs:?}")),
            (Some(ours), None) => out.push(format!("{path}[{index}]: ours {ours:?}, kit has none")),
            _ => {}
        }
    }
    // Descend into struct fields only: a freshly built tag has empty blocks, so
    // there is no element to compare against.
    for field in theirs.fields() {
        let key = clean_field_key(field.name());
        let Some(their_child) = field.as_struct() else {
            continue;
        };
        let Some(our_child) = ours
            .fields()
            .find(|candidate| clean_field_key(candidate.name()) == key)
            .and_then(|candidate| candidate.as_struct())
        else {
            out.push(format!("{path}/{key}: struct absent from ours"));
            continue;
        };
        compare(our_child, their_child, &format!("{path}/{key}"), out);
    }
}

#[test]
#[ignore = "diagnostic"]
fn built_tag_against_kit_tag() {
    let Some(reach) = kit("HREK") else {
        eprintln!("skipping: needs HREK");
        return;
    };
    let definitions = PathBuf::from("../../blam-tag-gui/definitions");
    let files = walk_files(&reach);
    for group in ["effect", "particle", "cheap_particle_emitter", "decal_system"] {
        eprintln!("=== haloreach_mcc {group}");
        let json = definitions.join("haloreach_mcc").join(format!("{group}.json"));
        if !json.is_file() {
            eprintln!("  no definition file for this group");
            continue;
        }
        let ours = match std::panic::catch_unwind(|| TagFile::new(&json)) {
            Ok(Ok(tag)) => tag,
            Ok(Err(error)) => {
                eprintln!("  ours: {error}");
                continue;
            }
            Err(_) => {
                eprintln!("  ours: panicked");
                continue;
            }
        };
        let Some(path) = files
            .iter()
            .find(|path| path.extension().and_then(|e| e.to_str()) == Some(group))
        else {
            eprintln!("  HREK ships none");
            continue;
        };
        let Ok(theirs) = TagFile::read(path) else {
            eprintln!("  kit tag unreadable");
            continue;
        };
        let mut out = Vec::new();
        compare(ours.root(), theirs.root(), "root", &mut out);
        eprintln!(
            "  {} difference(s) against {}",
            out.len(),
            path.file_name().unwrap().to_string_lossy()
        );
        for line in out.iter().take(12) {
            eprintln!("    {line}");
        }
    }
}
