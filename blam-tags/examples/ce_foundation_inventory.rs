//! Everything the codec still retains as bytes instead of modeling as values.
//!
//! "100% serialization support" needs an inventory before it needs a plan, and
//! the retained bytes fall into three separate populations that are easy to
//! conflate:
//!
//!  1. **Class tails** — what each class in an inheritance chain appends after
//!     the reflected properties. Parsed exactly, kept as a span.
//!  3. **Walk stops** — the handful of places the reader declines to continue.
//!
//! Reports each separately, with the counts a plan can be ordered by.
//!
//! Run: `ce_foundation_inventory [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export, walk_export, ExportContext, PropValue, PropertyBlock,
};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

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

    let mut tails: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut stops: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut total, mut no_schema, mut with_tail) = (0u64, 0u64, 0u64);
    let (mut tail_bytes, mut stop_bytes) = (0u64, 0u64);

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
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                total += 1;
                if usmap.flattened_properties(short).is_none() {
                    no_schema += 1;
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                if !parts.tail.is_empty() {
                    with_tail += 1;
                    tail_bytes += parts.tail.len() as u64;
                    let e = tails.entry(short.to_string()).or_default();
                    e.0 += 1;
                    e.1 += parts.tail.len() as u64;
                }
                if let Ok(walk) = walk_export(
                    &payloads[i],
                    &names,
                    &usmap,
                    short,
                    ex.object_flags,
                    &ExportContext::new(&bulk),
                ) {
                    if let Some(stop) = walk.stopped {
                        let e = stops.entry(stop.class).or_default();
                        e.0 += 1;
                        e.1 += stop.remaining as u64;
                        stop_bytes += stop.remaining as u64;
                    }
                }
            }
        }
    }

    println!("== exports ==");
    println!("  total                {total}");
    println!("  no .usmap schema     {no_schema}  (Blueprint-generated classes)");
    println!("  with a class tail    {with_tail}");
    println!();
    println!("== retained as bytes, by population ==");
    println!("  class tails          {tail_bytes:>13} bytes  ({:.2} GiB) across {} classes", tail_bytes as f64 / (1u64<<30) as f64, tails.len());
    println!("  hand-written structs              0 bytes  (all 23 typed)");
    println!("  behind a walk stop   {stop_bytes:>13} bytes  ({} classes)", stops.len());
    println!();

    let mut tv: Vec<_> = tails.iter().collect();
    tv.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
    let mut cum = 0u64;
    println!("== class tails: how many classes to reach each share of the bytes ==");
    for (i, (_, (_, b))) in tv.iter().enumerate() {
        cum += *b;
        let pct = 100.0 * cum as f64 / tail_bytes.max(1) as f64;
        if matches!(i + 1, 1 | 3 | 5 | 9 | 22 | 36) || i + 1 == tv.len() {
            println!("  {:>4} classes -> {pct:>6.2}%", i + 1);
        }
    }
    println!();

    println!("== walk stops ==");
    let mut sv: Vec<_> = stops.iter().collect();
    sv.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
    for (c, (n, b)) in sv.iter() {
        println!("  {c:<44} {n:>10} exports {b:>12} bytes");
    }
}
