//! Gate: `decode(encode(decode(x))) == decode(x)` for every export.
//!
//! This is the contract every model must meet, and the only one a lossy payload
//! can. Byte-identity is stronger and `ce_export_roundtrip` measures it, but BC
//! texture compression discards information by construction — decoding a mip and
//! re-compressing cannot reproduce the original blocks. What can always be
//! required is that the *data* survives: whatever could be read out originally
//! still reads out after writing.
//!
//! It is deliberately built before the models it will judge. Right now every
//! tail is a retained span, so this passes trivially — which is the point. It is
//! the harness that says whether each conversion preserved meaning, and a
//! conversion that breaks it is a model that lost something.
//!
//! Reports separately the exports that are byte-identical too, because a class
//! *without* a lossy payload should be, and one that quietly stops being is a
//! regression rather than an accepted cost.
//!
//! Run: `ce_semantic_roundtrip [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, write_export};
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

    let (mut total, mut semantic_ok, mut byte_ok, mut failed) = (0u64, 0u64, 0u64, 0u64);
    // Classes that survive semantically but are no longer byte-identical. Each
    // one has to be a class that genuinely contains a lossy codec.
    let mut lossy: BTreeMap<String, u64> = BTreeMap::new();
    let mut broke: BTreeMap<String, u64> = BTreeMap::new();
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
            let names = h.name_map.copy_raw_names();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(first) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                total += 1;

                let Ok(bytes) = write_export(short, &first, &usmap) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    continue;
                };
                let Ok(second) = read_export(&bytes, &names, &usmap, short, ex.object_flags) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: re-reading our own output failed",
                            h.package_name()
                        ));
                    }
                    continue;
                };

                if first.semantic_eq(&second) {
                    semantic_ok += 1;
                    if bytes == payloads[i] {
                        byte_ok += 1;
                    } else {
                        *lossy.entry(short.to_string()).or_default() += 1;
                    }
                } else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: value did not survive the round trip",
                            h.package_name()
                        ));
                    }
                }
            }
        }
    }

    println!("exports examined      {total}");
    println!(
        "semantically stable   {semantic_ok} ({:.4}%)   <- the contract",
        100.0 * semantic_ok as f64 / total.max(1) as f64
    );
    println!(
        "  also byte-identical {byte_ok} ({:.4}%)",
        100.0 * byte_ok as f64 / total.max(1) as f64
    );
    println!("  value-stable only   {}", semantic_ok - byte_ok);
    println!("broken                {failed}");

    if !lossy.is_empty() {
        println!("\nsemantically stable but no longer byte-identical:");
        println!("  (each must be a class that genuinely contains a lossy codec)");
        let mut v: Vec<_> = lossy.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    if !broke.is_empty() {
        println!("\nbroken by class:");
        let mut v: Vec<_> = broke.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
