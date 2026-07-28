//! Does the sim drive the UE pose? Look for AnimNode_BlamSyncPose / the
//! skeleton-sync component in the packages belonging to a given object.
use std::io::Cursor;
use std::collections::BTreeMap;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let needle = std::env::args().nth(1).unwrap_or_else(|| "pelican".into()).to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let mut hits: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut scanned = 0;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase().replace('\\', "/");
            if !p.ends_with(".uasset") || !p.contains(&needle) { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]), None, CV, HV, None) else { continue };
            scanned += 1;
            let mut found = Vec::new();
            for n in h.name_map.copy_raw_names() {
                let l = n.to_ascii_lowercase();
                if l.contains("syncpose") || l.contains("skeletonsynchronization")
                    || l.contains("blamskeletonbone") || l.contains("meshsynchronization") {
                    found.push(n);
                }
            }
            // Imports name the classes an asset actually uses.
            for n in &h.imported_package_names {
                let l = n.to_ascii_lowercase();
                if l.contains("syncpose") || l.contains("skeletonsync") { found.push(n.clone()); }
            }
            if !found.is_empty() {
                found.sort(); found.dedup();
                hits.insert(e.path.clone(), found);
            }
        }
    }
    println!("scanned {scanned} '{needle}' packages; {} reference the sync layer\n", hits.len());
    for (path, names) in hits.iter().take(12) {
        println!("  {}", path.rsplit('/').next().unwrap_or(path));
        println!("      {names:?}");
    }
}
