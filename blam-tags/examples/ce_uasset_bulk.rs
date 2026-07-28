//! What does a CE tag's `.uasset` actually hold, and what does it say about
//! the paired `.ubulk`?
use std::io::Cursor;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let suffix = std::env::args().nth(1)
        .unwrap_or_else(|| "pelican-skeleton_model.uasset".into()).to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let Some(rel) = a.entries().iter()
            .find(|e| e.path.to_ascii_lowercase().replace('\\',"/").ends_with(&suffix))
            .map(|e| e.path.clone()) else { continue };
        let ua = a.read(&rel).unwrap();
        let ub_rel = rel.strip_suffix(".uasset").map(|s| format!("{s}.ubulk")).unwrap();
        let ub_len = a.uncompressed_len(&ub_rel).unwrap_or(0);
        let h = FZenPackageHeader::deserialize(&mut Cursor::new(&ua[..]), None, CV, HV, None).unwrap();
        println!("{rel}");
        println!("  .uasset {} bytes   .ubulk {} bytes (the tag itself)", ua.len(), ub_len);
        println!("  exports {}  imports {}  imported packages {}  names {}",
            h.export_map.len(), h.import_map.len(), h.imported_package_names.len(),
            h.name_map.copy_raw_names().len());
        println!("  bulk data map entries: {}", h.bulk_data.len());
        for (i, b) in h.bulk_data.iter().enumerate() {
            println!("    [{i}] serial_offset={} duplicate_serial_offset={} serial_size={} flags=0x{:x} cooked_index={}",
                b.serial_offset, b.duplicate_serial_offset, b.serial_size, b.flags, b.cooked_index);
            println!("        serial_size == .ubulk length: {}", b.serial_size as u64 == ub_len);
        }
        for (i, e) in h.export_map.iter().enumerate() {
            println!("    export[{i}] name={:?} serial_size={} serial_offset={}",
                h.name_map.get(e.object_name).to_string(), e.cooked_serial_size, e.cooked_serial_offset);
        }
        return;
    }
    eprintln!("not found: {suffix}");
}
