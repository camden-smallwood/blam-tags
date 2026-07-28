//! Determine exactly how a tag package's `public_export_hash` is derived, by
//! testing candidate formulas against every shipped tag package.
//!
//! Run: cargo run --release --features iostore --example ce_export_hash_probe

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex, FPackageId};
use blam_tags::iostore::writer::cityhash64;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn ch(s: &str) -> u64 {
    cityhash64(&s.to_ascii_lowercase().encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>())
}

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut hits: BTreeMap<&str, usize> = BTreeMap::new();
    let mut n = 0usize;
    let mut pkg_id_ok = 0usize;
    let mut collisions: BTreeMap<u64, Vec<String>> = BTreeMap::new();
    let mut examples: Vec<String> = Vec::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.first() else { continue };
            n += 1;
            let pkg = h.package_name();
            let obj = h.name_map.get(ex.object_name).to_string();
            let full = format!("{pkg}.{obj}");
            let target = ex.public_export_hash;

            // package id check
            if FPackageId::from_name(&pkg).0 == u64::from_le_bytes(
                a.chunk_id_for(&e.path).unwrap().package_id()) {
                pkg_id_ok += 1;
            }

            let cands: [(&str, u64); 6] = [
                ("cityhash64(objectname)", ch(&obj)),
                ("cityhash64(pkg.obj)", ch(&full)),
                ("cityhash64(pkg/obj)", ch(&format!("{pkg}/{obj}"))),
                ("import_hash(pkg.obj)", FPackageObjectIndex::create_script_import(&full).raw_index()),
                ("cityhash64(pkg)", ch(&pkg)),
                ("import_hash(objectname)", FPackageObjectIndex::create_script_import(&obj).raw_index()),
            ];
            for (name, v) in cands {
                if v == target {
                    *hits.entry(name).or_default() += 1;
                }
            }
            if examples.len() < 4 {
                examples.push(format!(
                    "{pkg}\n    object={obj}\n    public_export_hash={target:016x}\n    {}",
                    cands.iter().map(|(k, v)| format!("{k}={v:016x}")).collect::<Vec<_>>().join("\n    ")
                ));
            }
            collisions.entry(target).or_default().push(pkg);
        }
    }

    println!("{n} tag packages; package-id formula matched {pkg_id_ok}\n");
    for (k, v) in &hits {
        println!("{v:>6}/{n}  {k}");
    }
    println!("\n-- samples --");
    for e in &examples {
        println!("{e}\n");
    }
    let dup: Vec<_> = collisions.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("-- public_export_hash collisions across tags: {} --", dup.len());
    for (h, v) in dup.iter().take(10) {
        println!("    {h:016x}: {v:?}");
    }
}
