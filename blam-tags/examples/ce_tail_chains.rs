//! Which unmodeled tail classes are just a base class's tail wearing a new name?
//!
//! 203 classes still hold a retained tail, but the byte totals hide the real
//! shape: `ADecalActor` is 25,503 exports and `UHaloAudioPlacementComponent`
//! 41,849, and neither is likely to serialize anything of its own — an actor
//! subclass that adds no `Serialize` override has exactly `AActor`'s tail.
//!
//! Groups the remaining classes by the *deepest ancestor that has its own tail
//! arm*, so one model can cover a whole family. Prints the `.usmap` super chain
//! for anything that does not fit a known family, which is the list that still
//! needs individual work.
//!
//! Run: `ce_tail_chains [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export, roundtrip_tail, TailContext, CLASSES_WITH_OWN_TAIL,
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

/// The `.usmap` super chain, most-derived first.
fn chain(usmap: &Usmap, class: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = class.to_string();
    for _ in 0..64 {
        out.push(cur.clone());
        match usmap.get(&cur).and_then(|s| s.super_name.clone()) {
            Some(s) => cur = s,
            None => break,
        }
    }
    out
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

    // class -> (exports with a tail, tail bytes)
    let mut unmodeled: BTreeMap<String, (u64, u64)> = BTreeMap::new();

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
                if parts.tail.is_empty() {
                    continue;
                }
                // `roundtrip_tail` is the authority on whether a model exists —
                // families are dispatched by chain, so no name list can be.
                let Some(block) = parts.block.as_ref() else { continue };
                let bulk: Vec<(i64, i64)> =
                    h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
                let ctx = TailContext {
                    bulk_data: &bulk,
                    origin: payloads[i].len() - parts.tail.len(),
                    usmap: &usmap,
                    resolver: None,
                };
                if roundtrip_tail(short, &parts.tail, &names, block, ctx).is_some() {
                    continue;
                }
                let e = unmodeled.entry(short.to_string()).or_default();
                e.0 += 1;
                e.1 += parts.tail.len() as u64;
            }
        }
    }

    // Group by the set of ancestors that carry their own tail arm. A class whose
    // only tail-bearing ancestor is `Actor` needs no model of its own.
    let mut families: BTreeMap<String, (u64, u64, Vec<String>)> = BTreeMap::new();
    for (class, (n, bytes)) in &unmodeled {
        let ch = chain(&usmap, class);
        let owners: Vec<&str> = ch
            .iter()
            .filter(|c| CLASSES_WITH_OWN_TAIL.contains(&c.as_str()))
            .map(String::as_str)
            .collect();
        let key = if owners.is_empty() {
            format!("(no tail-bearing ancestor)  chain: {}", ch.join(" <- "))
        } else {
            owners.join(" + ")
        };
        let e = families.entry(key).or_default();
        e.0 += n;
        e.1 += bytes;
        e.2.push(class.clone());
    }

    let mut v: Vec<_> = families.iter().collect();
    v.sort_by_key(|(_, (_, b, _))| std::cmp::Reverse(*b));
    println!("{} unmodeled classes in {} families\n", unmodeled.len(), families.len());
    for (family, (n, bytes, classes)) in v {
        println!(
            "{family}\n    {n} exports, {:.2} MiB, {} classes",
            *bytes as f64 / (1u64 << 20) as f64,
            classes.len()
        );
        let mut c = classes.clone();
        c.sort_by_key(|x| std::cmp::Reverse(unmodeled[x].1));
        println!("    {}", c.iter().take(8).cloned().collect::<Vec<_>>().join(", "));
    }
}
