//! Pick the smallest real package exercising each interesting decoder feature,
//! and write them out as test fixtures.
//!
//! Every gate in this crate needs a 100+ GB game install, so none of them run in
//! CI and nothing protects the codec from regression between manual runs. A
//! few hundred KB of real packages fixes that — but only if they are *chosen*
//! rather than sampled, since the features that break a writer (a zero-masked
//! entry, a static array, a hand-written struct, a non-empty container removal
//! prefix) are rare and a random sample would miss all of them.
//!
//! Smallest-wins, so the fixtures stay tiny while still covering each case.
//!
//! Run: `ce_make_fixtures [out-dir] [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, BlockLayout, PropValue};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Keep any single fixture small enough that the set stays reviewable.
const MAX_FIXTURE_BYTES: usize = 96 * 1024;

/// What each fixture is there to cover. A fixture that stopped exercising its
/// feature would silently become a duplicate of the simple case.
const FEATURES: &[&str] = &[
    "removals",      // non-empty TSet/TMap delta prefix — 5 exports in the corpus
    "zero-masked",   // an entry that serialized no bytes
    "static-array",  // a UPROPERTY declared Thing[N]
    "native-struct", // a hand-written Serialize, kept as a retained span
    "text",          // FText, likewise hand-written
    "leading-empty", // a CE tag wrapper's two empty header fragments
    "multi-export",  // enough exports that an edit moves its neighbours
    "string",        // an editable FString, for the edit test
];

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "blam-tags/tests/fixtures/ce".to_string());
    let usmap_path = std::env::args().nth(2).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
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

    // feature -> (size, path, bytes)
    let mut best: BTreeMap<&'static str, (usize, String, Vec<u8>)> = BTreeMap::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            if b.len() > MAX_FIXTURE_BYTES {
                continue;
            }
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();

            let mut has: Vec<&'static str> = Vec::new();
            if h.export_map.len() >= 8 {
                has.push("multi-export");
            }
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties() else { continue };
                if let BlockLayout::Unversioned { leading_empty, .. } = block.layout {
                    if leading_empty > 0 {
                        has.push("leading-empty");
                    }
                }
                for entry in &block.entries {
                    if entry.slot.is_some_and(|s| s.zero_masked) {
                        has.push("zero-masked");
                    }
                    if entry.slot.is_some_and(|s| s.array_index > 0) {
                        has.push("static-array");
                    }
                    walk(&entry.value, &mut has);
                }
            }
            has.sort_unstable();
            has.dedup();

            for f in has {
                let entry = best.entry(f).or_insert((usize::MAX, String::new(), Vec::new()));
                if b.len() < entry.0 {
                    *entry = (b.len(), e.path.clone(), b.clone());
                }
            }
        }
    }

    std::fs::create_dir_all(&out_dir).expect("create fixture dir");
    let mut manifest = String::from(
        "# Real Campaign Evolved packages, one per decoder feature.\n\
         # Generated by `cargo run --example ce_make_fixtures`.\n\
         # feature\tbytes\tfile\tsource path\n",
    );
    let mut total = 0usize;
    let mut written: Vec<String> = Vec::new();
    for f in FEATURES {
        match best.get(f) {
            Some((size, path, bytes)) => {
                let file = format!("{f}.uasset");
                std::fs::write(format!("{out_dir}/{file}"), bytes).expect("write fixture");
                manifest.push_str(&format!("{f}\t{size}\t{file}\t{path}\n"));
                total += size;
                written.push(format!("{f:<14} {size:>7} bytes  {path}"));
            }
            None => {
                manifest.push_str(&format!("{f}\t-\t-\tNOT FOUND\n"));
                written.push(format!("{f:<14} NOT FOUND"));
            }
        }
    }
    std::fs::write(format!("{out_dir}/manifest.tsv"), &manifest).expect("write manifest");

    // The class of an export is a hash resolved through `global.utoc`, which a
    // CI machine has no copy of. Emit just the hashes the fixtures actually use
    // as text, so the tests need the packages and nothing else.
    let mut classes: BTreeMap<u64, String> = BTreeMap::new();
    for (_, _, bytes) in best.values() {
        if let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(bytes), None, CV, HV, None) {
            for ex in &h.export_map {
                let raw = ex.class_index.raw_index();
                if let Some(name) = by_hash.get(&raw) {
                    classes.insert(raw, name.clone());
                }
            }
        }
    }
    let mut classes_tsv =
        String::from("# class-index hash -> object path, for the fixture packages only.\n");
    for (hash, name) in &classes {
        classes_tsv.push_str(&format!("{hash}\t{name}\n"));
    }
    std::fs::write(format!("{out_dir}/classes.tsv"), &classes_tsv).expect("write classes");
    println!("{} class names recorded", classes.len());

    for w in &written {
        println!("{w}");
    }
    println!("\n{} fixtures, {total} bytes total -> {out_dir}", FEATURES.len());
}

/// Note every feature a value exhibits, however deeply nested.
fn walk(v: &PropValue, has: &mut Vec<&'static str>) {
    match v {
        PropValue::WithRemovals { .. } => {
            has.push("removals");
            if let PropValue::WithRemovals { inner, .. } = v {
                walk(inner, has);
            }
        }
        PropValue::Str(_) => has.push("string"),
        // The hand-written structs are typed now, so they are their own value
        // rather than a block with a retained span.
        PropValue::HandWritten(h) => has.push(match h {
            blam_tags::iostore::object::hand_written::HandWritten::Text(_) => "text",
            _ => "native-struct",
        }),
        PropValue::Struct(b) => {
            for (_, v) in b.iter() {
                walk(v, has);
            }
        }
        PropValue::Array(items) => items.iter().for_each(|v| walk(v, has)),
        PropValue::Map(entries) => entries.iter().for_each(|(k, v)| {
            walk(k, has);
            walk(v, has);
        }),
        _ => {}
    }
}
