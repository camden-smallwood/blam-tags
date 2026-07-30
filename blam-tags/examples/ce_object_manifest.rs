//! The complete manifest of what one game object is made of, both halves.
//!
//! Starting from a tag package, follow every imported package transitively and
//! report each reached package by the class of its first export, split into the
//! part that is specific to this object and the part it shares with the rest of
//! the game. That is the real answer to "what would authoring a new one take":
//! the object-specific column is the work, and the class histogram says which
//! serializers a writer would have to have.
//!
//! Export classes are ScriptImports living in `global.utoc`, so they are
//! recognised by hashing candidate `/Script/Module.Class` paths out of the UHT
//! dump rather than read from the package.
//!
//! Run: cargo run --release --features iostore --example ce_object_manifest -- <tag-substr> [own-substr]

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "shotgun-weapon".into()).to_ascii_lowercase();
    // What counts as "belongs to this object" -- defaults to the root's own stem.
    let own = std::env::args()
        .nth(2)
        .unwrap_or_else(|| root.split('-').next().unwrap_or(&root).to_string())
        .to_ascii_lowercase();

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
    let archives: Vec<Arc<IoStoreArchive>> = utocs
        .iter()
        .filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new))
        .collect();

    // package id -> (path, archive index). One pass, so the walk is lookups.
    let mut index: HashMap<u64, (String, usize)> = HashMap::new();
    let mut start: Option<u64> = None;
    for (ai, a) in archives.iter().enumerate() {
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.ends_with(".uasset") && !lp.ends_with(".umap") {
                continue;
            }
            let Ok(bytes) = a.read_prefix(&e.path, 64 * 1024) else { continue };
            let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            let id = FPackageId::from_name(&hdr.package_name()).0;
            if start.is_none() && lp.contains(&root) {
                start = Some(id);
            }
            index.insert(id, (e.path.clone(), ai));
        }
    }
    let Some(start) = start else {
        eprintln!("no package matches {root:?}");
        return;
    };

    // Transitive closure over imported packages.
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    let mut queue: VecDeque<u64> = VecDeque::new();
    let mut classes: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // class -> (own, shared)
    let mut own_list: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unresolved = 0usize;
    queue.push_back(start);
    seen.insert(start);

    while let Some(id) = queue.pop_front() {
        let Some((path, ai)) = index.get(&id).cloned() else {
            unresolved += 1;
            continue;
        };
        let Ok(bytes) = archives[ai].read(&path) else { continue };
        let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        else {
            continue;
        };
        let class = hdr
            .export_map
            .first()
            .and_then(|ex| by_hash.get(&ex.class_index.raw_index()).cloned())
            .unwrap_or_else(|| "<blueprint-generated or unlisted>".into());
        let is_own = path.to_ascii_lowercase().contains(&own);
        let slot = classes.entry(class.clone()).or_default();
        if is_own {
            slot.0 += 1;
            own_list.entry(class).or_default().push(path.clone());
        } else {
            slot.1 += 1;
        }
        for imp in &hdr.imported_packages {
            if seen.insert(imp.0) {
                queue.push_back(imp.0);
            }
        }
    }

    println!("root: {root}   \"belongs to this object\" = path contains {own:?}");
    println!("closure: {} packages ({unresolved} import ids not in any container)\n", seen.len());
    println!("{:<46} {:>6} {:>8}", "export class", "own", "shared");
    println!("{}", "-".repeat(62));
    let mut rows: Vec<_> = classes.iter().collect();
    rows.sort_by(|a, b| (b.1 .0 + b.1 .1).cmp(&(a.1 .0 + a.1 .1)));
    for (class, (o, s)) in rows {
        println!("{class:<46} {o:>6} {s:>8}");
    }

    println!("\n== the object-specific packages, by class ==");
    for (class, paths) in &own_list {
        println!("\n-- {class} ({}) --", paths.len());
        for p in paths {
            println!("   {p}");
        }
    }
}
