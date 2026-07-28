//! What does the UE-side attachment lookup actually resolve against?
//! `UBlamSynchronizationHelperLibrary::GetEffectSocketNamesFromMarkerGroup(USkeleton*,
//! MarkerGroupName, ...)` maps a Blam marker group onto USkeleton sockets — so
//! compare the pelican's tag marker groups against the names living in its UE
//! Skeleton/SkeletalMesh packages.
use std::io::Cursor;
use std::collections::BTreeSet;
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let archives: Vec<_> = utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    // Tag side: marker group names + node names.
    let mut markers: BTreeSet<String> = BTreeSet::new();
    let mut nodes: BTreeSet<String> = BTreeSet::new();
    'o: for a in &archives {
        for e in a.entries() {
            if !e.path.to_ascii_lowercase().replace('\\',"/").ends_with("/pelican/pelican-skeleton_model.ubulk") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(tag) = TagFile::read_from_bytes(&b) else { continue };
            let root = tag.root();
            if let Some(mg) = root.field_path("marker groups").and_then(|f| f.as_block()) {
                for i in 0..mg.len() {
                    if let Some(g) = mg.element(i) {
                        if let Some(n) = g.read_string_id("name") { markers.insert(n); }
                    }
                }
            }
            if let Some(nb) = root.field_path("nodes").and_then(|f| f.as_block()) {
                for i in 0..nb.len() {
                    if let Some(n) = nb.element(i) { if let Some(s) = n.read_string_id("name") { nodes.insert(s); } }
                }
            }
            break 'o;
        }
    }
    println!("tag marker groups: {}   tag nodes: {}", markers.len(), nodes.len());

    // UE side: every name in the pelican's Skeleton / SkeletalMesh packages.
    let mut ue_names: BTreeSet<String> = BTreeSet::new();
    let mut pkgs = 0;
    for a in &archives {
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase().replace('\\',"/");
            if !p.ends_with(".uasset") || !p.contains("pelican") { continue }
            if !(p.contains("skeleton") || p.contains("skeletalmesh") || p.contains("_sk") || p.contains("mesh")) { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]), None, CV, HV, None) else { continue };
            pkgs += 1;
            for n in h.name_map.copy_raw_names() {
                ue_names.insert(n);
            }
        }
    }
    println!("UE packages scanned: {pkgs}   distinct names: {}", ue_names.len());

    let lower: BTreeSet<String> = ue_names.iter().map(|s| s.to_ascii_lowercase()).collect();
    let hit_m: Vec<_> = markers.iter().filter(|m| lower.contains(&m.to_ascii_lowercase())).collect();
    let hit_n: Vec<_> = nodes.iter().filter(|n| lower.contains(&n.to_ascii_lowercase())).collect();
    println!("\nmarker groups present in the UE packages: {}/{}", hit_m.len(), markers.len());
    println!("nodes         present in the UE packages: {}/{}", hit_n.len(), nodes.len());
    println!("\nsample marker groups: {:?}", markers.iter().take(12).collect::<Vec<_>>());
    println!("matched markers     : {:?}", hit_m.iter().take(12).collect::<Vec<_>>());
}
