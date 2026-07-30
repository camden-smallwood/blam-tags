//! Recover a missing class schema by finding which known schema the data fits.
//!
//! Three classes from `/Script/XGTGPerformanceOverlayTool` have cooked assets in
//! the shipping game but no reflection anywhere: absent from the `.usmap`, from
//! the UHT header dump, from UE 5.5.4, and — checked byte-wise — from both
//! shipping executables. The module was stripped and its widget assets were not.
//!
//! A thin subclass that adds no properties of its own has *exactly* its super's
//! flattened schema, which is why `Blam*TagDataAsset` decodes against
//! `BlamTagDataAssetBase`. That makes "which schema is it?" a falsifiable
//! question rather than a guess: try every schema in the `.usmap` and keep only
//! those under which *every* export of the class decodes, consumes its whole
//! property region, and re-encodes to the original bytes.
//!
//! Run: `ce_schema_fit [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{has_schema, read_export, write_export};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// One export to fit: its payload, the package's name map, and its flags.
struct Sample {
    payload: Vec<u8>,
    names: Vec<String>,
    flags: u32,
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

    let mut samples: BTreeMap<String, Vec<Sample>> = BTreeMap::new();
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
                if has_schema(short, &usmap) {
                    continue;
                }
                samples.entry(short.to_string()).or_default().push(Sample {
                    payload: payloads[i].clone(),
                    names: names.clone(),
                    flags: ex.object_flags,
                });
            }
        }
    }

    for (class, group) in &samples {
        println!("\n=== {class}: {} exports ===", group.len());
        // Rank fits by how little they leave unexplained. A candidate that
        // round-trips but leaves 80 bytes of "tail" has explained nothing.
        let mut fits: Vec<(usize, &str)> = Vec::new();
        for cand in &usmap.structs {
            let mut worst_tail = 0usize;
            let ok = group.iter().all(|s| {
                let Ok(ex) = read_export(&s.payload, &s.names, &usmap, &cand.name, s.flags) else {
                    return false;
                };
                worst_tail = worst_tail.max(ex.tail.len());
                write_export(&cand.name, &ex, &usmap).is_ok_and(|w| w == s.payload)
            });
            if ok {
                fits.push((worst_tail, &cand.name));
            }
        }
        fits.sort();
        println!("  {} schemas fit; tightest:", fits.len());
        for (tail, name) in fits.iter().take(12) {
            println!("    tail {tail:>4}B  {name}  (flattened {})",
                usmap.get(name).map(|s| s.prop_count).unwrap_or(0));
        }
    }
}
