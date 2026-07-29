//! Where are the 4.77 GiB of unmodeled tail bytes actually concentrated?
//!
//! Phase 4 converts class tails from retained spans into models, one class at a
//! time. Which class to do first is a measurement, not a guess — the ordering in
//! the plan was taken from an earlier survey and the only thing that matters is
//! where the bytes are *now*.
//!
//! Reports per class: how many exports have a non-empty tail, how many bytes
//! those tails total, and the median size. A class with a huge total but a tiny
//! median is many small tails (cheap to model, wide reach); one with a large
//! median is a few big blobs (mesh or texture payloads).
//!
//! Run: `ce_tail_census [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::read_export;
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct Stat {
    exports: u64,
    with_tail: u64,
    bytes: u64,
    sizes: Vec<u32>,
}

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

    let mut stats: BTreeMap<String, Stat> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut total_exports = 0u64;

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
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let s = stats.entry(short.to_string()).or_default();
                s.exports += 1;
                total_exports += 1;
                if !parts.tail.is_empty() {
                    s.with_tail += 1;
                    s.bytes += parts.tail.len() as u64;
                    total_bytes += parts.tail.len() as u64;
                    s.sizes.push(parts.tail.len() as u32);
                }
            }
        }
    }

    let mut rows: Vec<(&String, &mut Stat)> = stats.iter_mut().collect();
    rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.bytes));

    println!(
        "{:<44} {:>10} {:>12} {:>9} {:>10} {:>7}",
        "class", "with tail", "MiB", "% of all", "median", "cum %"
    );
    let mut cum = 0u64;
    for (class, s) in rows.iter_mut() {
        if s.bytes == 0 {
            break;
        }
        s.sizes.sort_unstable();
        let median = s.sizes[s.sizes.len() / 2];
        cum += s.bytes;
        println!(
            "{:<44} {:>10} {:>12.1} {:>8.2}% {:>10} {:>6.1}%",
            class,
            s.with_tail,
            s.bytes as f64 / (1 << 20) as f64,
            100.0 * s.bytes as f64 / total_bytes.max(1) as f64,
            median,
            100.0 * cum as f64 / total_bytes.max(1) as f64,
        );
    }
    println!(
        "\n{total_exports} exports, {} classes, {:.2} GiB of tail",
        stats.len(),
        total_bytes as f64 / (1u64 << 30) as f64
    );
    let no_tail: u64 = stats.values().map(|s| s.exports - s.with_tail).sum();
    println!("{no_tail} exports have no tail at all ({:.1}%)", 100.0 * no_tail as f64 / total_exports.max(1) as f64);
}
