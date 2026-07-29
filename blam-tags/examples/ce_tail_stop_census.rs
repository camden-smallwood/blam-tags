//! Where does native-tail modeling actually stop, and how much is behind it?
//!
//! The tail dispatcher has 49 sites that rewind and decline to continue. Each is
//! a deliberate boundary, but as a list of 49 they say nothing about which ones
//! matter: one that fires on every `StaticMesh` is the frontier, one that has
//! never fired is a hypothetical.
//!
//! Reports per stopping class: how many exports it stops, and how many bytes are
//! left unmodeled behind it. That ordering is what says where Phase 4 work is
//! worth doing — and what says which of the 49 could simply be deleted.
//!
//! Run: `ce_tail_stop_census [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{walk_export, ExportContext};
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

    // stopping class -> (exports stopped, bytes left unmodeled)
    let mut stops: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut walked, mut complete) = (0u64, 0u64);
    let mut unmodeled_total = 0u64;

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
            // The bulk-data map is not optional context: `BodySetup` and friends
            // resolve their cooked payloads through it, so omitting it makes
            // them fail every time and attributes their whole tail to a "stop"
            // that is really a missing argument.
            let bulk: Vec<(i64, i64)> = h
                .bulk_data
                .iter()
                .map(|b| (b.serial_offset, b.serial_size))
                .collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(walk) = walk_export(
                    &payloads[i],
                    &names,
                    &usmap,
                    short,
                    ex.object_flags,
                    &ExportContext::new(&bulk),
                ) else {
                    continue;
                };
                walked += 1;
                match walk.stopped {
                    None => complete += 1,
                    Some(stop) => {
                        let e = stops.entry(stop.class).or_default();
                        e.0 += 1;
                        e.1 += stop.remaining as u64;
                        unmodeled_total += stop.remaining as u64;
                    }
                }
            }
        }
    }

    println!("exports walked        {walked}");
    println!("chain fully modeled   {complete} ({:.2}%)", 100.0 * complete as f64 / walked.max(1) as f64);
    println!("stopped early         {}", walked - complete);
    println!("bytes behind a stop   {unmodeled_total} ({:.2} GiB)", unmodeled_total as f64 / (1u64 << 30) as f64);

    let mut rows: Vec<_> = stops.iter().collect();
    rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    println!("\n{:<40} {:>12} {:>16}", "stopping class", "exports", "unmodeled bytes");
    for (class, (n, bytes)) in rows.iter().take(25) {
        println!("{class:<40} {n:>12} {bytes:>16}");
    }
    println!("\n{} distinct stopping classes", stops.len());
}
