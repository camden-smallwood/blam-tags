//! What engine/package versions does Campaign Evolved actually ship?
//!
//! The struct-size and tail tables encode UE-5.5.4-specific facts. Gating on a
//! version means knowing which one was measured — so measure it rather than
//! assume it from the engine tag.
use std::collections::BTreeMap;
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
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
    let mut combos: BTreeMap<String, u64> = BTreeMap::new();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            let v = &h.versioning_info;
            *combos.entry(format!(
                "unversioned={} zen={:?} ue4={} ue5={} licensee={} custom_versions={}",
                h.is_unversioned, v.zen_version,
                v.package_file_version.file_version_ue4, v.package_file_version.file_version_ue5,
                v.licensee_version, v.custom_versions.len()
            )).or_default() += 1;
        }
    }
    println!("\npackage versioning info:");
    for (k, n) in &combos { println!("  {n:>8}  {k}"); }
}
