//! Which fixed native struct sizes does the shipped corpus actually exercise?
//!
//! The coverage matrix proves every byte of every export is accounted for, so
//! any size the corpus *uses* is right. It says nothing about the rest — and
//! `native_struct_size` contains entries Campaign Evolved may never touch. Those
//! are unverified guesses sitting in a lookup table that looks uniformly
//! authoritative, and the recurring bug shape in this format is "same type, two
//! sizes".
//!
//! This separates the two. An entry the corpus exercises is measured; one it
//! does not is a claim, and the place to spend a source citation.
//!
//! Run: `ce_native_struct_census [usmap-path]`
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::structs::{native_struct_size, NATIVE_STRUCT_NAMES};
use blam_tags::iostore::object::unversioned::{read_export, PropValue, PropertyBlock};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::{PropertyType, Usmap};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Record native structs actually *decoded*, following the value.
///
/// Walking the outer class's schema alone is not enough and gets the answer
/// wrong: `FQuat` only ever appears inside `FTransform`, a nested reflected
/// struct, so a schema-only walk reports it unverified when the corpus reads
/// 357,000 of them.
fn collect_value(
    ty: &PropertyType,
    v: &PropValue,
    usmap: &Usmap,
    out: &mut BTreeMap<String, u64>,
    depth: usize,
) {
    if depth > 16 {
        return;
    }
    match (ty, v) {
        (PropertyType::Struct(name), PropValue::Native(_)) => {
            *out.entry(name.clone()).or_default() += 1;
        }
        (PropertyType::Struct(name), PropValue::Struct(block)) => {
            let Some(flat) = usmap.flattened_slots(name) else { return };
            walk_block(block, &flat, usmap, out, depth + 1);
        }
        (PropertyType::Array(inner) | PropertyType::Set(inner), PropValue::Array(items)) => {
            for e in items {
                collect_value(inner, e, usmap, out, depth + 1);
            }
        }
        (PropertyType::Map(k, val), PropValue::Map(entries)) => {
            for (a, b) in entries {
                collect_value(k, a, usmap, out, depth + 1);
                collect_value(val, b, usmap, out, depth + 1);
            }
        }
        (_, PropValue::WithRemovals { inner, .. }) => collect_value(ty, inner, usmap, out, depth + 1),
        (PropertyType::Optional(inner), set) => collect_value(inner, set, usmap, out, depth + 1),
        _ => {}
    }
}

fn walk_block(
    block: &PropertyBlock,
    flat: &[(&blam_tags::iostore::usmap::UsmapProperty, u8)],
    usmap: &Usmap,
    out: &mut BTreeMap<String, u64>,
    depth: usize,
) {
    for entry in &block.entries {
        let Some(slot) = entry.slot else { continue };
        if let Some((p, _)) = flat.get(slot.index as usize) {
            collect_value(&p.ty, &entry.value, usmap, out, depth);
        }
    }
}

/// Record every struct name reachable from a property type, however nested.
fn collect(ty: &PropertyType, out: &mut BTreeMap<String, u64>) {
    match ty {
        PropertyType::Struct(name) => {
            if native_struct_size(name).is_some() {
                *out.entry(name.clone()).or_default() += 1;
            }
        }
        PropertyType::Array(inner) | PropertyType::Set(inner) | PropertyType::Optional(inner) => {
            collect(inner, out)
        }
        PropertyType::Map(k, v) => {
            collect(k, out);
            collect(v, out);
        }
        _ => {}
    }
}

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
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

    // Two different questions: which sizes are *declared* by a schema CE uses,
    // and which are actually *present* in a decoded block. Only the second is
    // proof, because a property that is never present is never read.
    let mut declared: BTreeMap<String, u64> = BTreeMap::new();
    let mut present: BTreeMap<String, u64> = BTreeMap::new();
    let mut classes_seen: BTreeSet<String> = BTreeSet::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
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
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let Some(flat) = usmap.flattened_slots(short) else { continue };
                if classes_seen.insert(short.to_string()) {
                    for (p, _) in &flat {
                        collect(&p.ty, &mut declared);
                    }
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties() else { continue };
                walk_block(block, &flat, &usmap, &mut present, 0);
            }
        }
    }

    let mut rows: Vec<(&str, u64, u64)> = NATIVE_STRUCT_NAMES
        .iter()
        .map(|n| {
            (
                *n,
                declared.get(*n).copied().unwrap_or(0),
                present.get(*n).copied().unwrap_or(0),
            )
        })
        .collect();
    rows.sort_by_key(|(n, _, p)| (*p, n.to_string()));

    println!("{:<34} {:>10} {:>14} {:>6}", "struct", "declared", "present", "size");
    for (name, d, p) in &rows {
        println!(
            "{name:<34} {d:>10} {p:>14} {:>6}  {}",
            native_struct_size(name).unwrap_or(0),
            if *p > 0 { "" } else { "<- UNVERIFIED" }
        );
    }

    let verified = rows.iter().filter(|(_, _, p)| *p > 0).count();
    println!(
        "\n{verified} of {} sizes exercised by the corpus, {} unverified",
        NATIVE_STRUCT_NAMES.len(),
        NATIVE_STRUCT_NAMES.len() - verified
    );
    println!("{} classes' schemas walked", classes_seen.len());
    let unverified: Vec<&str> =
        rows.iter().filter(|(_, _, p)| *p == 0).map(|(n, _, _)| *n).collect();
    if !unverified.is_empty() {
        println!("\nunverified (never present in a decoded block):");
        println!("  {}", unverified.join(", "));
    }
}
