//! Can a mod container be listed WITHOUT a directory index, including new and
//! renamed packages? Test: (a) does a .uasset chunk id share its first 8 bytes
//! (the FPackageId) with its .ubulk, and (b) does the .uasset self-describe its
//! own package path via the Zen header name map?
use std::io::Cursor;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let Some(ub) = a.entries().iter()
            .find(|e| e.path.to_ascii_lowercase().replace('\\',"/").ends_with("pelican-skeleton_model.ubulk"))
            .map(|e| e.path.clone()) else { continue };
        let ua = ub.strip_suffix(".ubulk").map(|s| format!("{s}.uasset")).unwrap();
        println!("container: {}", u.file_name().unwrap().to_string_lossy());
        let id_ub = a.chunk_id_for(&ub).unwrap();
        let id_ua = a.chunk_id_for(&ua).unwrap();
        println!("  .ubulk  id = {id_ub:?}");
        println!("  .uasset id = {id_ua:?}");
        let (bu, ba) = (format!("{id_ub:?}"), format!("{id_ua:?}"));
        println!("  -> first-8-bytes (FPackageId) shared: {}",
            bu.split(',').take(8).collect::<Vec<_>>() == ba.split(',').take(8).collect::<Vec<_>>());

        // (b) does the .uasset self-describe its package path?
        let bytes = a.read(&ua).unwrap();
        match FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None) {
            Ok(h) => {
                let name = h.name_map.get(h.summary.name).to_string();
                println!("  -> Zen header summary.name = {name:?}");
                println!("  -> exports: {}", h.export_map.len());
            }
            Err(e) => println!("  -> zen parse failed: {e:?}"),
        }
        break;
    }
}
