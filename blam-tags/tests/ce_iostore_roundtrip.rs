//! The IoStore codec's round-trip claims, on committed fixtures.
//!
//! Every corpus gate in `examples/` needs a 100+ GB Campaign Evolved install, so
//! none of them run in CI. These are the same claims against eight real packages
//! totalling ~7 KB, chosen by `ce_make_fixtures` so that each one exercises a
//! feature that would otherwise only appear far out in the tail: a non-empty
//! container removal prefix (5 exports in the whole corpus), a zero-masked
//! entry, a static array, a hand-written struct, an `FText`, a Campaign Evolved
//! tag wrapper's leading empty header fragments.
//!
//! The corpus gates remain the real measurement — 1,153,838 exports and 103,867
//! packages. These stop a regression reaching `master` between manual runs.

#![cfg(feature = "iostore")]

use std::collections::HashMap;
use std::io::Cursor;
use std::path::PathBuf;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::block::emit_block;
use blam_tags::iostore::object::unversioned::{read_export, write_export, PropValue};
use blam_tags::iostore::package::builder::{read_payloads, write_package, PACKAGE_FILE_TAG};
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;

const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

const FIXTURES: &[&str] = &[
    "removals",
    "zero-masked",
    "static-array",
    "native-struct",
    "text",
    "leading-empty",
    "multi-export",
    "string",
];

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ce")
}

fn load(name: &str) -> Vec<u8> {
    let p = fixture_dir().join(format!("{name}.uasset"));
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing fixture {}: {e}", p.display()))
}

/// Class-index hash → object path, recorded alongside the fixtures because the
/// real mapping lives in `global.utoc`, which CI has no copy of.
fn classes() -> HashMap<u64, String> {
    let text = std::fs::read_to_string(fixture_dir().join("classes.tsv")).expect("classes.tsv");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let (h, n) = l.split_once('\t')?;
            Some((h.parse().ok()?, n.to_string()))
        })
        .collect()
}

fn usmap() -> Usmap {
    let mut u = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut u);
    u
}

/// Decode every export a fixture package holds, calling `f` for each one whose
/// class the `.usmap` describes.
fn for_each_export(
    package: &[u8],
    usmap: &Usmap,
    classes: &HashMap<u64, String>,
    mut f: impl FnMut(usize, &str, &[u8], u32, &[String]),
) -> usize {
    let h = FZenPackageHeader::deserialize(&mut Cursor::new(package), None, CV, HV, None)
        .expect("parse package header");
    let payloads = read_payloads(&h, package).expect("payloads");
    let names = h.name_map.copy_raw_names();
    let mut seen = 0;
    for (i, ex) in h.export_map.iter().enumerate() {
        let Some(class) = classes.get(&ex.class_index.raw_index()) else { continue };
        let short = class.rsplit('.').next().unwrap_or(class);
        if usmap.flattened_properties(short).is_none() {
            continue;
        }
        f(i, short, &payloads[i], ex.object_flags, &names);
        seen += 1;
    }
    seen
}

/// Every property block must re-emit the exact bytes it was read from. This is
/// the claim the whole writer stands on — the corpus runs it over 1,153,836
/// blocks, and these eight packages keep the rare shapes in CI.
#[test]
fn every_fixture_block_round_trips() {
    let (usmap, classes) = (usmap(), classes());
    let mut blocks = 0;
    for name in FIXTURES {
        let pkg = load(name);
        for_each_export(&pkg, &usmap, &classes, |i, class, payload, flags, names| {
            let parts = read_export(payload, names, &usmap, class, flags)
                .unwrap_or_else(|e| panic!("{name}[{i}] {class}: read_export: {e:#}"));
            let Some(block) = parts.properties() else { return };
            let (_, used) = blam_tags::iostore::object::unversioned::read_export_struct_len(
                payload, names, &usmap, class,
            )
            .expect("block length");
            let out = emit_block(class, block, &usmap)
                .unwrap_or_else(|e| panic!("{name}[{i}] {class}: emit_block: {e:#}"));
            assert_eq!(
                out,
                &payload[..used],
                "{name}[{i}] {class}: property block did not re-emit its own bytes"
            );
            blocks += 1;
        });
    }
    assert!(blocks >= FIXTURES.len(), "expected at least one block per fixture, got {blocks}");
}

/// An export is a block, a `UObject` trailer and a native tail; taking it apart
/// and putting it back must be exact.
#[test]
fn every_fixture_export_round_trips() {
    let (usmap, classes) = (usmap(), classes());
    for name in FIXTURES {
        let pkg = load(name);
        let seen = for_each_export(&pkg, &usmap, &classes, |i, class, payload, flags, names| {
            let parts = read_export(payload, names, &usmap, class, flags)
                .unwrap_or_else(|e| panic!("{name}[{i}] {class}: read_export: {e:#}"));
            let out = write_export(class, &parts, &usmap)
                .unwrap_or_else(|e| panic!("{name}[{i}] {class}: write_export: {e:#}"));
            assert_eq!(out, payload, "{name}[{i}] {class}: export did not round trip");
        });
        assert!(seen > 0, "{name}: no decodable exports, fixture is not testing anything");
    }
}

/// A package rebuilt from its own re-encoded exports must be byte-identical,
/// including the four-byte `PACKAGE_FILE_TAG` that sits outside every export's
/// serial range and is therefore the easiest thing in the format to drop.
#[test]
fn every_fixture_package_rebuilds() {
    let (usmap, classes) = (usmap(), classes());
    for name in FIXTURES {
        let pkg = load(name);
        let h = FZenPackageHeader::deserialize(&mut Cursor::new(&pkg), None, CV, HV, None)
            .expect("header");
        let mut payloads = read_payloads(&h, &pkg).expect("payloads");
        let names = h.name_map.copy_raw_names();
        for (i, ex) in h.export_map.iter().enumerate() {
            let Some(class) = classes.get(&ex.class_index.raw_index()) else { continue };
            let short = class.rsplit('.').next().unwrap_or(class);
            if usmap.flattened_properties(short).is_none() {
                continue;
            }
            if let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags) {
                payloads[i] = write_export(short, &parts, &usmap).expect("write_export");
            }
        }
        let (out, _) = write_package(&h, &payloads, HV).expect("write_package");
        assert_eq!(out, pkg, "{name}: package did not rebuild byte-identically");
        assert_eq!(
            out[out.len() - 4..],
            PACKAGE_FILE_TAG.to_le_bytes(),
            "{name}: package footer missing"
        );
    }
}

/// The fixture that carries a non-empty container removal prefix — 5 exports in
/// the entire shipped corpus have one, and dropping their contents made exactly
/// those unwritable.
#[test]
fn the_removals_fixture_really_has_removals() {
    let (usmap, classes) = (usmap(), classes());
    let pkg = load("removals");
    let mut found = false;
    for_each_export(&pkg, &usmap, &classes, |_, class, payload, flags, names| {
        let Ok(parts) = read_export(payload, names, &usmap, class, flags) else { return };
        let Some(block) = parts.properties() else { return };
        for (_, v) in block.iter() {
            if matches!(v, PropValue::WithRemovals { removals: Some(r), .. } if !r.is_empty()) {
                found = true;
            }
        }
    });
    assert!(found, "the removals fixture no longer exercises a non-empty removal prefix");
}

/// The Campaign Evolved tag wrappers open with two empty header fragments that
/// `FUnversionedHeaderBuilder` cannot produce, so the count has to be carried.
#[test]
fn the_tag_wrapper_fixture_has_leading_empty_fragments() {
    use blam_tags::iostore::object::unversioned::BlockLayout;
    let (usmap, classes) = (usmap(), classes());
    let pkg = load("leading-empty");
    let mut max_leading = 0;
    for_each_export(&pkg, &usmap, &classes, |_, class, payload, flags, names| {
        let Ok(parts) = read_export(payload, names, &usmap, class, flags) else { return };
        if let Some(block) = parts.properties() {
            if let BlockLayout::Unversioned { leading_empty, .. } = block.layout {
                max_leading = max_leading.max(leading_empty);
            }
        }
    });
    assert!(
        max_leading > 0,
        "the tag-wrapper fixture no longer has leading empty fragments"
    );
}

/// Editing a property must survive a rebuild, and must move the exports after it
/// without disturbing their contents. Nothing in the corpus changes size, so
/// this is the only place the offset recomputation is exercised in CI.
#[test]
fn an_edit_survives_a_package_rebuild() {
    let (usmap, classes) = (usmap(), classes());
    const NEW: &str = "BlamFixtureEditProbe_LongEnoughToResizeTheExport";

    let pkg = load("string");
    let h = FZenPackageHeader::deserialize(&mut Cursor::new(&pkg), None, CV, HV, None)
        .expect("header");
    let mut payloads = read_payloads(&h, &pkg).expect("payloads");
    let original = payloads.clone();
    let names = h.name_map.copy_raw_names();

    // Find a string property to change.
    let mut edited: Option<(usize, String, String)> = None;
    for (i, ex) in h.export_map.iter().enumerate() {
        let Some(class) = classes.get(&ex.class_index.raw_index()) else { continue };
        let short = class.rsplit('.').next().unwrap_or(class).to_string();
        if usmap.flattened_properties(&short).is_none() {
            continue;
        }
        let Ok(mut parts) = read_export(&payloads[i], &names, &usmap, &short, ex.object_flags)
        else {
            continue;
        };
        let Some(block) = parts.properties_mut() else { continue };
        let Some(entry) = block
            .entries
            .iter_mut()
            .find(|e| matches!(&e.value, PropValue::Str(s) if s != NEW))
        else {
            continue;
        };
        let prop = entry.name.to_string();
        entry.value = PropValue::Str(NEW.into());
        payloads[i] = write_export(&short, &parts, &usmap).expect("write_export");
        edited = Some((i, short, prop));
        break;
    }
    let (idx, class, prop) = edited.expect("the string fixture has no editable string property");
    assert_ne!(
        payloads[idx].len(),
        original[idx].len(),
        "the probe string should have changed the export's size"
    );

    let (rebuilt, _) = write_package(&h, &payloads, HV).expect("rebuild");
    assert_ne!(rebuilt.len(), pkg.len(), "the package should have changed size");

    // Re-read the rebuilt package from scratch.
    let h2 = FZenPackageHeader::deserialize(&mut Cursor::new(&rebuilt), None, CV, HV, None)
        .expect("re-read header");
    let payloads2 = read_payloads(&h2, &rebuilt).expect("re-read payloads");
    let names2 = h2.name_map.copy_raw_names();
    let parts2 = read_export(
        &payloads2[idx],
        &names2,
        &usmap,
        &class,
        h2.export_map[idx].object_flags,
    )
    .expect("re-read export");
    assert_eq!(
        parts2.properties().and_then(|b| b.get(&prop)).and_then(|v| v.as_str()),
        Some(NEW),
        "the edit did not survive the rebuild"
    );

    // And every untouched export is exactly as it was.
    for i in 0..original.len() {
        if i != idx {
            assert_eq!(payloads2[i], original[i], "export {i} changed while editing {idx}");
        }
    }
}

/// Editing a genuinely zero-masked property — one the cooker chose to encode as
/// a mask bit with no bytes — must start emitting bytes for it.
///
/// Replaying the file's mask bit instead of deciding per save discards the edit
/// outright, since a masked entry writes nothing. The synthetic probe in
/// `object::block` covers the mechanism; this covers it against real cooked
/// bytes, where the property is one the cooker actually masked.
#[test]
fn editing_a_zero_masked_property_emits_it() {
    use blam_tags::iostore::object::unversioned::intern_name;
    let (usmap, classes) = (usmap(), classes());

    // Which fixture holds a masked entry is a property of the shipped data, not
    // something to hard-code: the masked population is overwhelmingly enums and
    // names, so scan for whichever kind turns up first.
    for name in FIXTURES {
        let pkg = load(name);
        let mut h = FZenPackageHeader::deserialize(&mut Cursor::new(&pkg), None, CV, HV, None)
            .expect("header");
        let mut payloads = read_payloads(&h, &pkg).expect("payloads");
        let names = h.name_map.copy_raw_names();

        let mut edited: Option<(usize, String, String)> = None;
        for i in 0..h.export_map.len() {
            let ex = &h.export_map[i];
            let Some(class) = classes.get(&ex.class_index.raw_index()) else { continue };
            let short = class.rsplit('.').next().unwrap_or(class).to_string();
            if usmap.flattened_properties(&short).is_none() {
                continue;
            }
            let flags = ex.object_flags;
            let Ok(mut parts) = read_export(&payloads[i], &names, &usmap, &short, flags) else {
                continue;
            };
            let Some(block) = parts.properties_mut() else { continue };
            let Some(pos) = block.entries.iter().position(|e| {
                e.slot.is_some_and(|s| s.zero_masked)
                    && matches!(
                        e.value,
                        PropValue::Int(0)
                            | PropValue::Name(_)
                            | PropValue::Bool(false)
                            | PropValue::Object(0)
                    )
            }) else {
                continue;
            };
            let prop = block.entries[pos].name.to_string();
            let new_value = match &block.entries[pos].value {
                PropValue::Int(_) => PropValue::Int(1),
                PropValue::Bool(_) => PropValue::Bool(true),
                PropValue::Object(_) => PropValue::Object(1),
                // A masked name is `NAME_None`; giving it a real name also grows
                // the package's name map.
                _ => PropValue::Name(intern_name(&mut h.name_map, "BlamFixtureMaskProbe")),
            };
            block.entries[pos].value = new_value;
            payloads[i] = write_export(&short, &parts, &usmap).expect("write_export");
            edited = Some((i, short, prop));
            break;
        }
        let Some((idx, class, prop)) = edited else { continue };

        let (rebuilt, _) = write_package(&h, &payloads, HV).expect("rebuild");
        let h2 = FZenPackageHeader::deserialize(&mut Cursor::new(&rebuilt), None, CV, HV, None)
            .expect("re-read header");
        let payloads2 = read_payloads(&h2, &rebuilt).expect("re-read payloads");
        let names2 = h2.name_map.copy_raw_names();
        let parts2 = read_export(
            &payloads2[idx],
            &names2,
            &usmap,
            &class,
            h2.export_map[idx].object_flags,
        )
        .expect("re-read export");
        let block = parts2.properties().expect("block");
        let slot = block
            .entries
            .iter()
            .find(|e| &*e.name == prop.as_str())
            .and_then(|e| e.slot)
            .expect("the edited property should still be present");
        assert!(
            !slot.zero_masked,
            "{name}: {prop} still zero-masked after being given a non-zero value \
             — the edit writes no bytes and is discarded"
        );
        let ok = matches!(
            block.get(&prop),
            Some(PropValue::Int(1)) | Some(PropValue::Bool(true)) | Some(PropValue::Object(1))
        ) || block.get(&prop).and_then(|v| v.as_str()) == Some("BlamFixtureMaskProbe");
        assert!(ok, "{name}: the edit to masked property {prop} did not survive: {:?}", block.get(&prop));
        return;
    }
    panic!("no fixture has a zero-masked property to edit — the suite would not catch a mask regression");
}
