//! Does our writer produce the same ContainerHeader StoreEntry the game does?
//! For a sample of shipped tag packages, read the game's own StoreEntry out of
//! its pak's container header, then re-derive one by round-tripping the package
//! through FZenPackageHeader::serialize (exactly what the mod writer does) and
//! compare field by field.
use std::io::Cursor;
use blam_tags::iostore::container_header::{EIoContainerHeaderVersion, FIoContainerHeader, StoreEntry};
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main() {
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let (mut n, mut same, mut diff) = (0usize, 0usize, 0usize);
    let mut samples = Vec::new();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        // container header chunk = type 6, index 0, id = container id
        let Some(hdr_idx) = (0..a.chunk_count()).find(|&i| a.chunk_id(i).map(|c| c.chunk_type()==6).unwrap_or(false)) else {
            println!("{}: no container header chunk", utoc.file_stem().unwrap().to_string_lossy());
            continue;
        };
        let Ok(bytes) = a.read_chunk(hdr_idx) else { continue };
        let Ok(hdr) = FIoContainerHeader::deserialize(&mut Cursor::new(&bytes), Some(HV)) else {
            println!("{}: header parse failed", utoc.file_stem().unwrap().to_string_lossy()); continue };
        let mut local = 0;
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") { continue }
            if local >= 40 { break }
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None) else { continue };
            let pid = FPackageId::from_name(&h.package_name());
            let Some(theirs) = hdr.get_store_entry(pid) else { continue };
            local += 1; n += 1;
            let mut ours = StoreEntry::default();
            let mut buf = Cursor::new(Vec::new());
            if h.serialize(&mut buf, &mut ours, HV).is_err() { continue }
            let match_all = ours.export_bundles_size == theirs.export_bundles_size
                && ours.imported_packages == theirs.imported_packages
                && ours.shader_map_hashes.len() == theirs.shader_map_hashes.len();
            if match_all { same += 1 } else {
                diff += 1;
                if samples.len() < 8 {
                    samples.push(format!("{}\n     theirs: bundles_size={} load_order={} imports={} shaders={}\n     ours  : bundles_size={} load_order={} imports={} shaders={}",
                        h.package_name(),
                        theirs.export_bundles_size, theirs.load_order, theirs.imported_packages.len(), theirs.shader_map_hashes.len(),
                        ours.export_bundles_size, ours.load_order, ours.imported_packages.len(), ours.shader_map_hashes.len()));
                }
            }
        }
    }
    println!("\ncompared {n} tag packages: identical {same}, different {diff}");
    for s in &samples { println!("\n{s}") }
}
