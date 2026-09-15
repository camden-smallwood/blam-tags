//! Where a tag built from the definitions differs in *shape* from one the kit
//! wrote, for the same game and group.
//!
//! Run with `--ignored`. Kept as a diagnostic rather than an assertion: it
//! measures how close the two shapes are, and the remaining gap is real.
//!
//! The editing kits walk their own field list against a tag's, so a field list
//! that disagrees is a tag they can refuse or fall over on - which is what was
//! happening. Measured against HREK, and what it found:
//!
//! - **`explanation` fields were being dropped from the built layout**, on a
//!   stated belief that shipped tags carry none. They do carry them, as
//!   zero-width unnamed `custom` fields. `cheap_particle_emitter` - the group
//!   reported crashing the tools - declared 36 root fields against the kit's 43,
//!   the difference being exactly its seven explanations. Emitting them the way
//!   the kits do took it from **43 differences to 3**, with the root field list
//!   now matching. Fixed in `schema.rs`.
//! - **The kits do not store a name for a `custom` field**, whatever the JSON
//!   calls it - the decorators and the `tmpl` render-method hole alike. Dropping
//!   ours took `cheap_particle_emitter` the rest of the way, from 3 differences
//!   to **0**: structurally identical to the kit tag. Also fixed in `schema.rs`.
//! - **`effect` and `decal_system` roots are byte-identical** to the kit's, so
//!   whatever makes an effect fail is not its root shape.
//! - **`particle` cannot be judged this way.** Ours declares 496 bytes and the
//!   tag compared against carries 492: Reach ships that group at several layout
//!   revisions, so this is comparing two different ones. A fair comparison has
//!   to pick a kit tag whose root size matches the declared size first, and
//!   until it does the particle numbers here mean nothing.
//!
//! Its other limit: a freshly built tag has empty blocks, so only structs are
//! descended into. Block element shapes are unmeasured, and for `effect` that is
//! where the substance lives.

use blam_tags::convert::clean_field_key;
use blam_tags::{TagFile, TagStruct};
use std::path::PathBuf;

fn kit(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from("D:/SteamLibrary/steamapps/common")
        .join(name)
        .join(if name == "H4EK" { "tog" } else { "tags" });
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
