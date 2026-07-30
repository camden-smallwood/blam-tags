//! Which `EManagedArrayType`s does a geometry collection actually contain?
//!
//! `FManagedArrayCollection` is a tagged union keyed by a runtime type id, so
//! the typed model is an enum — but writing all 49 variants when the corpus
//! holds a handful is work with no evidence behind it. This counts them.
//!
//! Run: `ce_managed_array_census [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, GeometryCollectionTail, TailContext};
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

    let mut seen: BTreeMap<i32, u64> = BTreeMap::new();
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
                if short != "GeometryCollection" {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties() else { continue };
                let bulk: Vec<(i64, i64)> =
                    h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
                let ctx = TailContext {
                    bulk_data: &bulk,
                    origin: payloads[i].len() - parts.tail.len(),
                    usmap: &usmap,
                    resolver: None,
                    object_flags: ex.object_flags,
                };
                let _ = block;
                let _ = ctx;
                let mut r = blam_tags::iostore::object::archive_reader(&parts.tail, &names);
                if let Ok(t) = GeometryCollectionTail::read(&mut r) {
                    for a in &t.collection.attributes {
                        *seen.entry(a.type_id).or_default() += 1;
                    }
                }
            }
        }
    }
    println!("{:<6} {:<40} {}", "id", "EManagedArrayType", "attributes");
    for (id, n) in &seen {
        let name = blam_tags::iostore::object::managed_array_type_name(*id);
        println!("{id:<6} {name:<40} {n}");
    }
}
