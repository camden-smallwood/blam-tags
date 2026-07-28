//! Map the REVERSE direction: which non-tag Unreal assets point BACK at tags?
//! Focus on the mesh-sync chain (BP -> BlamMeshSynchronizationDataAsset ->
//! ModelTag) that binds an object tag to actual geometry.
use std::collections::BTreeMap;
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let sync_cls = FPackageObjectIndex::create_script_import("/Script/BlamSynchronization.BlamMeshSynchronizationDataAsset");
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut das: BTreeMap<String, String> = BTreeMap::new();   // DA package -> ModelTag package
    let mut no_tag = Vec::new();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase();
            if !lower.ends_with(".uasset") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            let Some(ex) = h.export_map.iter().find(|x| x.class_index == sync_cls) else { continue };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            if off >= b.len() { continue }
            let Ok(p) = read_export_struct(&b[off..end], &names, &usmap, "BlamMeshSynchronizationDataAsset") else { continue };
            let tgt = match p.get("ModelTag") {
                Some(PropValue::Object(i)) if *i < 0 => h.import_map.get((-*i-1) as usize)
                    .and_then(|im| im.package_import())
                    .and_then(|r| h.imported_package_names.get(r.imported_package_index as usize)).cloned(),
                _ => None,
            };
            match tgt { Some(t) => { das.insert(h.package_name(), t); }, None => no_tag.push(h.package_name()) }
        }
    }
    println!("{} BlamMeshSynchronizationDataAssets with a ModelTag, {} without", das.len(), no_tag.len());
    let mut rev: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (d, t) in &das { rev.entry(t.to_ascii_lowercase()).or_default().push(d.clone()) }
    println!("distinct model tags targeted: {}", rev.len());
    let multi: Vec<_> = rev.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("model tags targeted by >1 DA: {}", multi.len());
    for (t, v) in das.iter().take(10) { println!("   {t}\n      -> {v}") }
    println!("\n-- DAs with no ModelTag --");
    for d in no_tag.iter().take(6) { println!("   {d}") }
}
