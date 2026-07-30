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

use blam_tags::iostore::object::unversioned::{
    flattened_schema, read_export_in, set_property, write_export_in, ExportContext, PropValue,
};
use blam_tags::iostore::package::builder::{read_payloads, write_package};
use blam_tags::iostore::usmap::{PropertyType, Usmap};
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

/// A distinctive value that fits every integer width, including a `uint8` enum.
const PROBE: i64 = 3;
/// Non-zero, exactly representable, and not a plausible class default.
const PROBE_F: f64 = 0.5;

/// A value to insert for a scalar property, or `None` for a type this gate
/// cannot synthesise one for.
///
/// Restricting this to `Int`/`Int64` left 45,292 packages — 43.6% — with
/// nothing to edit and recorded as "no candidate", which reads as a property of
/// the corpus and was a property of the gate. Widening it to the other scalar
/// widths, floats and bools is what actually exercises those packages, and
/// `Bool` in particular is the one that reaches the zero-mask path.
///
/// `Name`, `Str` and `Text` are deliberately out: inserting one means interning
/// a name or a string into the package, which is a different edit with a
/// different failure mode, and mixing it in here would blur what a failure
/// means.
fn probe_value(ty: &PropertyType) -> Option<PropValue> {
    match ty {
        PropertyType::Int
        | PropertyType::Int64
        | PropertyType::Int16
        | PropertyType::Int8
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { enum_name: None } => Some(PropValue::Int(PROBE)),
        PropertyType::Float | PropertyType::Double => Some(PropValue::Float(PROBE_F)),
        PropertyType::Bool => Some(PropValue::Bool(true)),
        _ => None,
    }
}

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let world = World::open(PAKS, usmap).expect("mount Paks");
    let usmap = world.usmap();

    let (mut inserted, mut verified, mut no_candidate) = (0usize, 0usize, 0usize);
    let mut grew = 0usize;
    // Why a package yielded no edit. Previously all of this was one bucket
    // called "no absent int prop", which is how two defects hid inside it: the
    // gate read without a resolver, so every export needing one was recorded as
    // having no free slot, and it used `Usmap::flattened_slots` directly, which
    // misses the fallback `flattened_schema` applies.
    let mut why: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut failures: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for a in world.archives() {
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
            // The edit path needs the same context the read path does: a data
            // table's row layout and a property bag's member types live in
            // other packages, and without them those exports simply fail to
            // read and get counted as uneditable.
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let resolver = world.resolver(&h, &b, &names);
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };

            // An integer property the class declares but this export omits.
            let mut done: Option<(usize, String, String, PropValue)> = None;
            for i in 0..h.export_map.len() {
                let ex = &h.export_map[i];
                let Some(class) = world.class_path(ex.class_index.raw_index()) else {
                    *why.entry("class not in script objects").or_default() += 1;
                    continue;
                };
                let short = class.rsplit('.').next().unwrap_or(class).to_string();
                let Ok(flat) = flattened_schema(&short, usmap) else {
                    *why.entry("no schema for the class").or_default() += 1;
                    continue;
                };
                let Ok(mut parts) =
                    read_export_in(&payloads[i], &names, usmap, &short, ex.object_flags, &ctx)
                else {
                    *why.entry("export did not read").or_default() += 1;
                    continue;
                };
                let Some(block) = parts.properties_mut() else {
                    *why.entry("no property block").or_default() += 1;
                    continue;
                };
                let Some((prop, want)) = flat
                    .iter()
                    .filter(|(_, slot, _)| *slot == 0)
                    .find_map(|(p, _, _)| {
                        (block.get(&p.name).is_none())
                            .then(|| probe_value(&p.ty).map(|v| (p.name.clone(), v)))
                            .flatten()
                    })
                else {
                    *why.entry("no absent scalar property").or_default() += 1;
                    continue;
                };
                if set_property(block, &short, &prop, want.clone(), usmap).is_err() {
                    *why.entry("set_property refused").or_default() += 1;
                    continue;
                }
                let Ok(bytes) = write_export_in(&short, &parts, usmap, Some(&resolver)) else {
                    *why.entry("export did not write").or_default() += 1;
                    continue;
                };
                if bytes.len() > payloads[i].len() {
                    grew += 1;
                }
                payloads[i] = bytes;
                done = Some((i, short, prop, want));
                break;
            }
            let Some((idx, class, prop, want)) = done else {
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
            let bulk2: Vec<(i64, i64)> =
                h2.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let resolver2 = world.resolver(&h2, &rebuilt, &names2);
            let ctx2 = ExportContext { bulk_data: &bulk2, resolver: Some(&resolver2) };
            let Ok(parts2) = read_export_in(
                &payloads2[idx],
                &names2,
                usmap,
                &class,
                h2.export_map[idx].object_flags,
                &ctx2,
            ) else {
                *failures.entry("reread-export").or_default() += 1;
                if samples.len() < 8 {
                    samples.push(format!("{} :: {class}.{prop}: export no longer decodes", h.package_name()));
                }
                continue;
            };
            let block2 = parts2.properties().expect("block");
            if !block2.get(&prop).is_some_and(|v| v.semantic_eq(&want)) {
                *failures.entry("value-missing").or_default() += 1;
                if samples.len() < 8 {
                    samples.push(format!(
                        "{} :: {class}.{prop}: expected {want:?}, got {:?}",
                        h.package_name(),
                        block2.get(&prop)
                    ));
                }
                continue;
            }

            // The properties that were already there must be untouched, and so
            // must every other export.
            let mut ok = true;
            let parts0 = read_export_in(
                &original[idx],
                &names,
                usmap,
                &class,
                h.export_map[idx].object_flags,
                &ctx,
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
    println!("no edit made         {no_candidate}");
    for (reason, n) in &why {
        println!("    {n:>8}  {reason}");
    }
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
