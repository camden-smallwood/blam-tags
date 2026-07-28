//! Brute-force a ScriptImport hash back to an object path.
//! Run: cargo run --release --features iostore --example ce_hash_probe -- <hex hash>

use blam_tags::iostore::ue_types::FPackageObjectIndex;

const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";

fn main() {
    let targets: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|a| u64::from_str_radix(a.trim_start_matches("0x"), 16).ok())
        .collect();
    let targets = if targets.is_empty() {
        vec![0x11E679BF31B0CC8A, 0x3D9B31710023FFD1]
    } else {
        targets
    };

    let mut modules: Vec<String> = std::fs::read_dir(UHT)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    modules.sort();

    // candidate class stems: every header stem in every module, plus generated
    // names for the shipped tag groups.
    let mut stems: Vec<String> = Vec::new();
    for m in &modules {
        for sub in ["Public", "Private", "Classes"] {
            if let Ok(rd) = std::fs::read_dir(format!("{UHT}/{m}/{sub}")) {
                for f in rd.filter_map(|e| e.ok()) {
                    let n = f.file_name().to_string_lossy().to_string();
                    if let Some(s) = n.strip_suffix(".h") {
                        stems.push(s.to_string());
                    }
                }
            }
        }
    }
    // common engine/package object paths that aren't header stems
    for m in ["/Script/CoreUObject", "/Script/Engine", "/Script/BlamSynchronization"] {
        for extra in ["Package", "Class", "Object", "DataAsset", "PrimaryDataAsset", "Default__Object"] {
            let _ = (m, extra);
        }
    }
    for extra in [
        "/Script/CoreUObject",
        "/Script/Engine",
        "/Script/BlamSynchronization",
        "/Script/CoreUObject.Package",
        "/Script/CoreUObject.Class",
        "/Script/CoreUObject.Object",
        "/Script/Engine.DataAsset",
        "/Script/Engine.Default__DataAsset",
    ] {
        for t in &targets {
            if FPackageObjectIndex::create_script_import(extra).raw_index() == *t {
                println!("{t:016X} = {extra}  (direct)");
            }
        }
    }
    for extra in [
        "BlamFrameEventListTagDataAsset",
        "BlamModelAnimationFrameEventListTagDataAsset",
        "BlamAnimationFrameEventListTagDataAsset",
        "BlamFrameEventsTagDataAsset",
        "BlamFrameEventListDataAsset",
        "FrameEventListTagDataAsset",
        "BlamFrameeventlistTagDataAsset",
    ] {
        stems.push(extra.to_string());
    }
    stems.sort();
    stems.dedup();
    eprintln!("{} modules x {} stems = {} candidates", modules.len(), stems.len(),
        modules.len() * stems.len() * 2);

    for &t in &targets {
        let mut hit = false;
        for m in &modules {
            for s in &stems {
                for form in [format!("/Script/{m}.{s}"), format!("/Script/{m}.Default__{s}")] {
                    if FPackageObjectIndex::create_script_import(&form).raw_index() == t {
                        println!("{t:016X} = {form}");
                        hit = true;
                    }
                }
            }
        }
        if !hit {
            println!("{t:016X} = <no match>");
        }
    }
}
