//! Classify every package whose path matches a substring, by export class.
//!
//! `ce_object_manifest` walks the *import* closure, which by construction misses
//! anything reached by a soft object path -- and a weapon's meshes are soft
//! refs, so they are absent from it entirely. Matching on the path instead
//! catches the whole authored footprint of one object, which is what "what would
//! it take to make a new one" actually costs.
//!
//! Run: cargo run --release --features iostore --example ce_classify -- <substr>

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let needle = std::env::args().nth(1).unwrap_or_else(|| "shotgun".into()).to_ascii_lowercase();

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    for m in std::fs::read_dir(UHT).expect("UHT dump").filter_map(|e| e.ok()) {
        if !m.path().is_dir() {
            continue;
        }
        let module = m.file_name().to_string_lossy().to_string();
        for sub in ["Public", "Private", "Classes"] {
            let Ok(rd) = std::fs::read_dir(format!("{UHT}/{module}/{sub}")) else { continue };
            for f in rd.filter_map(|e| e.ok()) {
                let n = f.file_name().to_string_lossy().to_string();
                let Some(stem) = n.strip_suffix(".h") else { continue };
                by_hash
                    .entry(
                        FPackageObjectIndex::create_script_import(&format!("/Script/{module}.{stem}"))
                            .raw_index(),
                    )
                    .or_insert_with(|| stem.to_string());
            }
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut classes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut total = 0usize;
    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.ends_with(".uasset") && !lp.ends_with(".umap") {
                continue;
            }
            if !lp.contains(&needle) {
                continue;
            }
            total += 1;
            let Ok(bytes) = a.read_prefix(&e.path, 64 * 1024) else { continue };
            let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            // The package's class is the class of its *main* export, which is the
            // one named after the package -- not export[0]. A mesh package leads
            // with its `BodySetup`, so taking the first export reports a game
            // full of BodySetups and no meshes at all.
            let leaf = hdr
                .package_name()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let main = hdr
                .export_map
                .iter()
                .find(|ex| hdr.name_map.get(ex.object_name).to_ascii_lowercase() == leaf)
                .or_else(|| hdr.export_map.first());
            let class = main
                .and_then(|ex| by_hash.get(&ex.class_index.raw_index()).cloned())
                .unwrap_or_else(|| "<blueprint-generated or unlisted>".into());
            classes.entry(class).or_default().push(e.path.clone());
        }
    }

    println!("{total} packages matching {needle:?}\n");
    let mut rows: Vec<_> = classes.iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
    for (class, paths) in rows {
        println!("{:>5}  {class}", paths.len());
    }
}
