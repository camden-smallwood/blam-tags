//! Gate: does an *edit* survive a package rebuild?
//!
//! Every other gate reads something and writes the same thing back, so all of
//! them are blind to the bugs that only appear once a value actually changes.
//! Two of those are structural and this is the only thing that exercises them:
//!
//!  * **The export resizes.** Editing an `FString` to a different length changes
//!    that export's size, so every export after it has to move. Nothing in the
//!    corpus changes size, so `write_package`'s offset recomputation is
//!    otherwise never tested against real data.
//!  * **The name map grows.** Setting a name property to a string the package
//!    does not contain means appending to the name map, which grows the header.
//!    An `FName` is just an index, so pointing one at a name that was never
//!    added fails silently rather than loudly.
//!
//! For each package it edits what it can, rebuilds, re-reads the result from
//! scratch, and checks both that the new value is there **and** that every
//! untouched export is still byte-identical. That second half is the point: an
//! edit that lands correctly while corrupting its neighbours would pass a
//! weaker check.
//!
//! Run: `ce_edit_roundtrip [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    intern_name, read_export, write_export, PropValue,
};
use blam_tags::iostore::package::builder::{read_payloads, write_package};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Deliberately long, so an edited `FString` changes length and forces every
/// later export to move.
const NEW_STRING: &str = "BlamEditProbe_ThisStringIsDeliberatelyLongSoTheExportResizes";
const NEW_NAME: &str = "BlamEditProbeName";
/// Small enough to fit every integer width, including a `uint8` enum. Editing a
/// *masked* zero to this also forces the entry to stop being zero-masked, which
/// grows the export — the other way an edit changes size.
const NEW_INT: i64 = 1;

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    let so = ScriptObjects::load(format!("{PAKS}/global.utoc")).expect("script objects");
    for e in so.entries() {
        if let Some(p) = so.resolve(e.global_index.raw_index()) {
            by_hash.insert(e.global_index.raw_index(), p.to_string());
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read Paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    let mut edited_pkgs = 0usize;
    let mut verified = 0usize;
    let mut str_edits = 0usize;
    let mut name_edits = 0usize;
    let mut int_edits = 0usize;
    let mut unmasked = 0usize;
    let mut resized = 0usize;
    let mut no_candidate = 0usize;
    let mut failures: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    let mut fail = |what: &'static str,
                    msg: String,
                    failures: &mut BTreeMap<&'static str, usize>,
                    samples: &mut Vec<String>| {
        *failures.entry(what).or_default() += 1;
        if samples.len() < 10 {
            samples.push(msg);
        }
    };

    for a in &archives {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();

            // Decode what we can, and find a property worth editing.
            let class_of = |i: usize| -> Option<String> {
                let ex = &h.export_map[i];
                let class = by_hash.get(&ex.class_index.raw_index())?;
                let short = class.rsplit('.').next().unwrap_or(class).to_string();
                usmap.flattened_properties(&short).map(|_| short)
            };

            #[derive(Clone, Copy, PartialEq)]
            enum Kind {
                Str,
                Name,
                Int,
            }
            let mut target: Option<(usize, String, String, Kind)> = None;
            for i in 0..h.export_map.len() {
                let Some(short) = class_of(i) else { continue };
                let Ok(parts) =
                    read_export(&payloads[i], &names, &usmap, &short, h.export_map[i].object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.block.as_ref() else { continue };
                // Prefer a string (tests resizing); fall back to a name (tests
                // name-map growth).
                if let Some(entry) = block.entries.iter().find(|e| {
                    matches!(&e.value, PropValue::Str(s) if s != NEW_STRING)
                }) {
                    target = Some((i, short, entry.name.to_string(), Kind::Str));
                    break;
                }
                if let Some(entry) = block.entries.iter().find(|e| {
                    matches!(&e.value, PropValue::Name(n) if n.as_str() != NEW_NAME)
                }) {
                    target = Some((i, short.clone(), entry.name.to_string(), Kind::Name));
                    continue;
                }
                // Last resort, but the broadest: almost every export has an
                // integer, so this is what carries the neighbour-integrity
                // check across the whole corpus rather than a corner of it.
                if target.is_none() {
                    if let Some(entry) = block.entries.iter().find(|e| {
                        matches!(&e.value, PropValue::Int(v) if *v != NEW_INT)
                    }) {
                        target = Some((i, short.clone(), entry.name.to_string(), Kind::Int));
                    }
                }
            }
            let Some((idx, class, prop, kind)) = target else {
                no_candidate += 1;
                continue;
            };

            // Apply the edit.
            let mut edited_header = h.clone();
            let mut edited_payloads = payloads.clone();
            let Ok(mut parts) = read_export(
                &payloads[idx],
                &names,
                &usmap,
                &class,
                h.export_map[idx].object_flags,
            ) else {
                continue;
            };
            let block = parts.block.as_mut().expect("checked above");
            let entry = block
                .entries
                .iter_mut()
                .find(|e| &*e.name == prop.as_str())
                .expect("found above");
            match kind {
                Kind::Str => {
                    entry.value = PropValue::Str(NEW_STRING.to_string());
                    str_edits += 1;
                }
                Kind::Name => {
                    entry.value =
                        PropValue::Name(intern_name(&mut edited_header.name_map, NEW_NAME));
                    name_edits += 1;
                }
                Kind::Int => {
                    if entry.slot.is_some_and(|s| s.zero_masked) {
                        unmasked += 1;
                    }
                    entry.value = PropValue::Int(NEW_INT);
                    int_edits += 1;
                }
            }
            let Ok(bytes) = write_export(&class, &parts, &usmap) else {
                fail("write_export", format!("{}: write_export failed", h.package_name()), &mut failures, &mut samples);
                continue;
            };
            if bytes.len() != edited_payloads[idx].len() {
                resized += 1;
            }
            edited_payloads[idx] = bytes;

            let (rebuilt, _) = match write_package(&edited_header, &edited_payloads, HV) {
                Ok(v) => v,
                Err(err) => {
                    fail("write_package", format!("{}: {err:#}", h.package_name()), &mut failures, &mut samples);
                    continue;
                }
            };
            edited_pkgs += 1;

            // Re-read the rebuilt package from scratch — no shortcuts.
            let Ok(h2) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&rebuilt), None, CV, HV, None)
            else {
                fail("reread-header", format!("{}: rebuilt header did not parse", h.package_name()), &mut failures, &mut samples);
                continue;
            };
            let Ok(payloads2) = read_payloads(&h2, &rebuilt) else {
                fail("reread-payloads", format!("{}: rebuilt payloads did not resolve", h.package_name()), &mut failures, &mut samples);
                continue;
            };
            let names2 = h2.name_map.copy_raw_names();

            // 1. The edit is there.
            let Ok(parts2) = read_export(
                &payloads2[idx],
                &names2,
                &usmap,
                &class,
                h2.export_map[idx].object_flags,
            ) else {
                fail("reread-export", format!("{}: edited export did not decode", h.package_name()), &mut failures, &mut samples);
                continue;
            };
            let read_back = parts2.block.as_ref().and_then(|b| b.get(&prop));
            let (got, want) = match kind {
                Kind::Int => (
                    read_back.map(|v| format!("{v:?}")),
                    format!("Int({NEW_INT})"),
                ),
                Kind::Str => (
                    read_back.and_then(|v| v.as_str().map(str::to_string)),
                    NEW_STRING.to_string(),
                ),
                Kind::Name => (
                    read_back.and_then(|v| v.as_str().map(str::to_string)),
                    NEW_NAME.to_string(),
                ),
            };
            if got.as_deref() != Some(want.as_str()) {
                fail(
                    "value-not-preserved",
                    format!(
                        "{} :: {class}.{prop}: expected {want}, got {got:?}",
                        h.package_name()
                    ),
                    &mut failures,
                    &mut samples,
                );
                continue;
            }

            // 2. Every untouched export is still byte-identical.
            let mut neighbours_ok = true;
            for i in 0..payloads.len() {
                if i != idx && payloads2.get(i) != Some(&payloads[i]) {
                    neighbours_ok = false;
                    fail(
                        "neighbour-corrupted",
                        format!(
                            "{} :: export {i} changed while editing export {idx}",
                            h.package_name()
                        ),
                        &mut failures,
                        &mut samples,
                    );
                    break;
                }
            }
            if neighbours_ok {
                verified += 1;
            }
        }
    }

    println!("packages edited      {edited_pkgs}");
    println!("fully verified       {verified} ({:.4}%)", 100.0 * verified as f64 / edited_pkgs.max(1) as f64);
    println!("  string edits       {str_edits}  (export changes size)");
    println!("  name edits         {name_edits}  (name map grows)");
    println!("  int edits          {int_edits}");
    println!("    were zero-masked {unmasked}  (edit forces the mask off)");
    println!("  exports resized    {resized}");
    println!("no editable property {no_candidate}");

    if !failures.is_empty() {
        println!("\nfailures:");
        let mut v: Vec<_> = failures.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (k, n) in v {
            println!("  {n:>7}  {k}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if verified != edited_pkgs || !failures.is_empty() {
        std::process::exit(1);
    }
}
