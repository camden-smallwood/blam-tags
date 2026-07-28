//! Is `imported_packages` sorted by FPackageId? And is `imported_public_export_hashes`
//! ordered by first use in the import map? Both must be reproducible by a builder.
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let (mut n, mut id_sorted, mut ids_match_names, mut hash_first_use, mut name_sorted) = (0,0,0,0,0);
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\',"/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") { continue }
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None) else { continue };
            if h.imported_packages.is_empty() { continue }
            n += 1;
            if h.imported_packages.windows(2).all(|w| w[0].0 <= w[1].0) { id_sorted += 1 }
            let mut names = h.imported_package_names.clone(); names.sort();
            if names == h.imported_package_names { name_sorted += 1 }
            if h.imported_package_names.iter().zip(h.imported_packages.iter())
                .all(|(nm, id)| FPackageId::from_name(nm) == *id) { ids_match_names += 1 }
            // hashes ordered by first use in import map?
            let mut expect: Vec<u64> = Vec::new();
            let mut seen_pair: Vec<(u32, u64)> = Vec::new();
            for im in &h.import_map {
                if let Some(r) = im.package_import() {
                    let hv = h.imported_public_export_hashes[r.imported_public_export_hash_index as usize];
                    if !seen_pair.contains(&(r.imported_package_index, hv)) {
                        seen_pair.push((r.imported_package_index, hv));
                        expect.push(hv);
                    }
                }
            }
            if expect.len() == h.imported_public_export_hashes.len()
                && expect == h.imported_public_export_hashes { hash_first_use += 1 }
        }
    }
    println!("{n} tag packages with imports");
    println!("  imported_packages sorted by FPackageId : {id_sorted}");
    println!("  imported_package_names sorted by string: {name_sorted}");
    println!("  ids == cityhash of the parallel name   : {ids_match_names}");
    println!("  hashes = 1 per unique (pkg,hash) pair : {hash_first_use}");
}
