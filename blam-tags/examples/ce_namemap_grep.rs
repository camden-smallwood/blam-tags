//! Grep a cooked package's name map (catches AnimGraph node classes, which are
//! script imports rather than package imports).
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    let pkg = std::env::args().nth(1).unwrap_or_default().to_ascii_lowercase();
    let needle = std::env::args().nth(2).unwrap_or_else(|| "sync".into()).to_ascii_lowercase();
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    for path in &u {
        let Ok(a) = IoStoreArchive::open(path) else { continue };
        for e in a.entries() {
            let l = e.path.to_ascii_lowercase();
            if !l.ends_with(".uasset") || !l.contains(&pkg) { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            let names = h.name_map.copy_raw_names();
            let hits: Vec<&String> = names.iter().filter(|n| n.to_ascii_lowercase().contains(&needle)).collect();
            println!("\n=== {}\n  {} names, {} matching '{needle}'", e.path, names.len(), hits.len());
            for n in hits.iter().take(30) { println!("    {n}"); }
        }
    }
}
