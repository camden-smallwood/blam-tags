//! Dump one cooked package's imports + export classes — forward direction.
//!
//! Run: cargo run --features iostore --example ce_pkg_imports -- <path-substring>

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

const CLASSES: &[&str] = &[
    "/Script/Engine.SkeletalMesh",
    "/Script/Engine.StaticMesh",
    "/Script/Engine.Skeleton",
    "/Script/Engine.Blueprint",
    "/Script/Engine.BlueprintGeneratedClass",
    "/Script/Engine.AnimBlueprintGeneratedClass",
    "/Script/BlamSynchronization.BlamMeshSynchronizationDataAsset",
    "/Script/BlamSynchronization.BlamModelRegionStringTable",
];

fn main() {
    let hint = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "DA_Pelican_MeshSynchronization".into())
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase();
            if !lower.ends_with(".uasset") || !lower.contains(&hint) {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
            else {
                continue;
            };
            println!("\n=== {}", e.path);
            println!("  {} bytes, {} exports", bytes.len(), h.export_map.len());
            let found: Vec<&str> = CLASSES
                .iter()
                .copied()
                .filter(|c| h.exports_class(FPackageObjectIndex::create_script_import(c)))
                .collect();
            println!("  export classes: {found:?}");
            println!("  imported packages ({}):", h.imported_package_names.len());
            for i in &h.imported_package_names {
                println!("    {i}");
            }
        }
    }
}
