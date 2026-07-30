//! Gate: can a property the cooker *omitted* be added back and read again?
//!
//! `IsDefault` (UnversionedPropertySerialization.cpp:989) drops any property
//! whose value equals its class default, so most of a class's schema is absent
//! from any given export. Those are exactly the properties a user wants to
//! change — a setting left at its default is the one you go and turn on.
//!
//! Editing a property that is already present cannot test this: the header
//! keeps the same set of schema indices and only a value changes. Inserting one
//! rewrites the fragment stream, moves every later value, and resizes the
//! export.
//!
//! For each package it inserts an absent property, rebuilds, re-reads from
//! scratch, and checks the new property is there, the properties that were
//! already there are unchanged, and every other export is byte-identical.
//!
//! Run: `ce_insert_property [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, set_property, write_export, PropValue};
use blam_tags::iostore::package::builder::{read_payloads, write_package};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::{PropertyType, Usmap};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// A distinctive value that fits every integer width, including a `uint8` enum.
const PROBE: i64 = 3;

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

    let (mut inserted, mut verified, mut no_candidate) = (0usize, 0usize, 0usize);
    let mut grew = 0usize;
    let mut failures: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

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
            let original = payloads.clone();
            let mut payloads = payloads;
            let names = h.name_map.copy_raw_names();

            // An integer property the class declares but this export omits.
            let mut done: Option<(usize, String, String)> = None;
            for i in 0..h.export_map.len() {
                let ex = &h.export_map[i];
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class).to_string();
                let Some(flat) = usmap.flattened_slots(&short) else { continue };
                let Ok(mut parts) = read_export(&payloads[i], &names, &usmap, &short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties_mut() else { continue };
                let Some(prop) = flat
                    .iter()
                    .find(|(p, slot)| {
                        *slot == 0
                            && matches!(p.ty, PropertyType::Int | PropertyType::Int64)
                            && block.get(&p.name).is_none()
                    })
                    .map(|(p, _)| p.name.clone())
                else {
                    continue;
                };
                if set_property(block, &short, &prop, PropValue::Int(PROBE), &usmap).is_err() {
                    continue;
                }
                let Ok(bytes) = write_export(&short, &parts, &usmap) else { continue };
                if bytes.len() > payloads[i].len() {
                    grew += 1;
                }
                payloads[i] = bytes;
                done = Some((i, short, prop));
                break;
            }
            let Some((idx, class, prop)) = done else {
                no_candidate += 1;
                continue;
            };
            inserted += 1;

            let (rebuilt, _) = match write_package(&h, &payloads, HV) {
                Ok(v) => v,
                Err(err) => {
                    *failures.entry("write_package").or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!("{}: {err:#}", h.package_name()));
                    }
                    continue;
                }
            };

            let Ok(h2) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&rebuilt), None, CV, HV, None)
            else {
                *failures.entry("reread-header").or_default() += 1;
                continue;
            };
            let Ok(payloads2) = read_payloads(&h2, &rebuilt) else {
                *failures.entry("reread-payloads").or_default() += 1;
                continue;
            };
            let names2 = h2.name_map.copy_raw_names();
            let Ok(parts2) = read_export(
                &payloads2[idx],
                &names2,
                &usmap,
                &class,
                h2.export_map[idx].object_flags,
            ) else {
                *failures.entry("reread-export").or_default() += 1;
                if samples.len() < 8 {
                    samples.push(format!("{} :: {class}.{prop}: export no longer decodes", h.package_name()));
                }
                continue;
            };
            let block2 = parts2.properties().expect("block");
            if !matches!(block2.get(&prop), Some(PropValue::Int(v)) if *v == PROBE) {
                *failures.entry("value-missing").or_default() += 1;
                if samples.len() < 8 {
                    samples.push(format!(
                        "{} :: {class}.{prop}: expected Int({PROBE}), got {:?}",
                        h.package_name(),
                        block2.get(&prop)
                    ));
                }
                continue;
            }

            // The properties that were already there must be untouched, and so
            // must every other export.
            let mut ok = true;
            let parts0 = read_export(
                &original[idx],
                &names,
                &usmap,
                &class,
                h.export_map[idx].object_flags,
            )
            .expect("original decodes");
            // Compare by *schema slot*, not by name. A static array's slots
            // share one name, so matching them by name compares slot 1 against
            // slot 0 and reports a difference that is not there — which is
            // exactly what this gate did on `MovieScene2DTransformSection`
            // before the slot index was used.
            let after_by_slot: HashMap<u32, String> = block2
                .entries
                .iter()
                .filter_map(|e| e.slot.map(|s| (s.index, format!("{:?}", e.value))))
                .collect();
            for entry in &parts0.properties().expect("block").entries {
                let Some(slot) = entry.slot else { continue };
                let before = format!("{:?}", entry.value);
                if after_by_slot.get(&slot.index) != Some(&before) {
                    ok = false;
                    *failures.entry("existing-property-changed").or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {class}.{} [slot {}] changed while inserting {prop}",
                            h.package_name(),
                            entry.name,
                            slot.index
                        ));
                    }
                    break;
                }
            }
            for i in 0..original.len() {
                if i != idx && payloads2.get(i) != Some(&original[i]) {
                    ok = false;
                    *failures.entry("neighbour-corrupted").or_default() += 1;
                    break;
                }
            }
            if ok {
                verified += 1;
            }
        }
    }

    println!("properties inserted  {inserted}");
    println!("fully verified       {verified} ({:.4}%)", 100.0 * verified as f64 / inserted.max(1) as f64);
    println!("  export grew        {grew}");
    println!("no absent int prop   {no_candidate}");
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
    if verified != inserted || inserted == 0 {
        std::process::exit(1);
    }
}
