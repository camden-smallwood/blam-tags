//! Derive the authoritative tag-group -> UClass map for Campaign Evolved by
//! reversing each tag package's `class_index` / `template_index` ScriptImport
//! hash against every `/Script/<Module>.<Class>` name harvested from the UHT
//! header dump.
//!
//! Run: cargo run --release --features iostore --example ce_tag_class_map

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    // 1. Harvest every module/class pair from the UHT dump, and build the
    //    ScriptImport hash for both the class and its CDO.
    let mut by_hash: HashMap<u64, String> = HashMap::new();
    let mut modules: Vec<String> = Vec::new();
    for m in std::fs::read_dir(UHT).unwrap().filter_map(|e| e.ok()) {
        if !m.path().is_dir() {
            continue;
        }
        modules.push(m.file_name().to_string_lossy().to_string());
    }
    modules.sort();
    let mut class_count = 0usize;
    for module in &modules {
        for sub in ["Public", "Private", "Classes", ""] {
            let dir = if sub.is_empty() {
                format!("{UHT}/{module}")
            } else {
                format!("{UHT}/{module}/{sub}")
            };
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for f in rd.filter_map(|e| e.ok()) {
                let name = f.file_name().to_string_lossy().to_string();
                let Some(stem) = name.strip_suffix(".h") else { continue };
                for prefix in ["U", "A", "F", "E", ""] {
                    let cls = format!("{prefix}{stem}");
                    let path = format!("/Script/{module}.{stem}");
                    let _ = cls;
                    let h = FPackageObjectIndex::create_script_import(&path);
                    by_hash.entry(h.raw_index()).or_insert_with(|| path.clone());
                    // the class default object lives at <Class>:Default__<Class>
                    let cdo = format!("/Script/{module}.Default__{stem}");
                    let hc = FPackageObjectIndex::create_script_import(&cdo);
                    by_hash.entry(hc.raw_index()).or_insert(cdo);
                    break;
                }
                class_count += 1;
            }
        }
    }
    eprintln!("harvested {class_count} headers across {} modules -> {} hashes",
        modules.len(), by_hash.len());

    // 2. Sweep every tag package, collect (group -> class hash / template hash).
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut groups: BTreeMap<String, (BTreeSet<u64>, BTreeSet<u64>, usize)> = BTreeMap::new();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
            else {
                continue;
            };
            let ent = groups.entry(group.to_string()).or_default();
            ent.2 += 1;
            for ex in &h.export_map {
                ent.0.insert(ex.class_index.raw_index());
                ent.1.insert(ex.template_index.raw_index());
            }
        }
    }

    println!("\n{:<48} {:>6}  {}", "tag group", "count", "UClass  /  CDO template");
    let mut unresolved = 0;
    for (g, (classes, templates, n)) in &groups {
        for c in classes {
            let cname = by_hash.get(c).cloned().unwrap_or_else(|| {
                unresolved += 1;
                format!("<unresolved {c:016X}>")
            });
            println!("{g:<48} {n:>6}  {cname}");
        }
        for t in templates {
            let tname = by_hash
                .get(t)
                .cloned()
                .unwrap_or_else(|| format!("<unresolved {t:016X}>"));
            println!("{:<48} {:>6}  ^ template: {tname}", "", "");
        }
    }
    eprintln!("\n{unresolved} unresolved class hashes");

    // 3. Which declared TagDataAsset classes ship no tags? (authorable groups
    //    the game knows about but the shipped content never uses)
    let declared: BTreeSet<String> = std::fs::read_dir(format!("{UHT}/BlamSynchronization/Public"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with("TagDataAsset.h"))
        .map(|n| n.trim_end_matches(".h").to_string())
        .collect();
    let used: BTreeSet<String> = groups
        .keys()
        .map(|g| {
            let mut s = String::from("Blam");
            for p in g.split('_') {
                let mut c = p.chars();
                if let Some(f) = c.next() {
                    s.push(f.to_ascii_uppercase());
                    s.push_str(c.as_str());
                }
            }
            s + "TagDataAsset"
        })
        .collect();
    println!("\n-- declared TagDataAsset classes with NO shipped tags ({}) --",
        declared.difference(&used).count());
    for d in declared.difference(&used) {
        println!("    {d}");
    }
    println!("\n-- groups whose guessed class name is NOT a declared class ({}) --",
        used.difference(&declared).count());
    for d in used.difference(&declared) {
        println!("    {d}");
    }
}
