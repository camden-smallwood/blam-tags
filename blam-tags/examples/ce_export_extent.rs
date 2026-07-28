//! Is a failing export actually truncated? Prints each export's declared serial
//! extent against the package chunk's real length, so a "read past end" can be
//! told apart from a mis-parse.
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    let want = std::env::args().nth(1).expect("package substring");
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            if !h.package_name().to_ascii_lowercase().contains(&want.to_ascii_lowercase()) { continue }
            println!("{} : chunk {} bytes, header_size {}", h.package_name(), b.len(), h.summary.header_size);
            for (i, ex) in h.export_map.iter().enumerate() {
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = off + ex.cooked_serial_size as usize;
                println!("  export {i}: off {off} size {} end {end}{}",
                    ex.cooked_serial_size,
                    if end > b.len() { format!("  *** TRUNCATED by {} bytes", end - b.len()) } else { String::new() });
            }
            return;
        }
    }
    println!("not found");
}
