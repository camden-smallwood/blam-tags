//! Validate the tag→UE-mesh resolution heuristic (char root via
//! DA_MeshSynchronization, variant selection, mesh/region filtering) across
//! several characters before porting into Baboon's preview loader.
//!
//! Run: cargo run -p blam-tags --features iostore --example ce_resolve -- [model-key ...]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn norm(p: &str) -> String {
    p.to_ascii_lowercase().replace('\\', "/")
}

/// Exclude non-body render meshes.
fn is_excluded(n: &str, base: &str) -> bool {
    n.contains("/skeleton/")
        || ["shield", "shadow", "animdynamics", "destroyed", "_dmg", "damage", "collision", "physics", "imposter"]
            .iter()
            .any(|k| base.contains(k))
}

fn region_of(base: &str) -> &'static str {
    if base.contains("head") {
        "head"
    } else if base.contains("leg") {
        "legs"
    } else if base.contains("arm") {
        "arms"
    } else {
        "body"
    }
}

fn main() {
    let keys: Vec<String> = std::env::args().skip(1).collect();
    let keys = if keys.is_empty() {
        vec![
            "objects/characters/elite_ai/elite_ai-model".to_string(),
            "objects/characters/spartans/spartans-model".to_string(),
            "objects/characters/floodcombat_elite/floodcombat_elite-model".to_string(),
        ]
    } else {
        keys
    };

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<Arc<IoStoreArchive>> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();

    for key in &keys {
        let key = norm(key);
        println!("\n===== model: {key} =====");

        // 1. char root via DA_MeshSynchronization importing this model.
        let mut char_root = None;
        'scan: for a in &archives {
            for e in a.entries() {
                let n = norm(&e.path);
                if !n.ends_with("meshsynchronization.uasset") {
                    continue;
                }
                let Ok(b) = a.read(&e.path) else { continue };
                let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]), None, CV, HV, None)
                else {
                    continue;
                };
                if h.imported_package_names.iter().any(|p| norm(p).ends_with(&key)) {
                    char_root = n.rsplit_once("/common/").map(|(r, _)| r.to_string())
                        .or_else(|| n.rsplit_once('/').map(|(r, _)| r.to_string()));
                    println!("  DA: {n}");
                    break 'scan;
                }
            }
        }
        let Some(char_root) = char_root else {
            println!("  !! no MeshSynchronization DA found");
            continue;
        };
        println!("  char_root: {char_root}");

        // 2. candidate SK_ meshes under char_root, grouped by variant.
        let root_slash = format!("{char_root}/");
        let mut by_variant: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in &archives {
            for e in a.entries() {
                let n = norm(&e.path);
                if !n.ends_with(".uasset") || !n.contains(&root_slash) {
                    continue;
                }
                let base = n.rsplit('/').next().unwrap_or("");
                if !base.starts_with("sk_") || is_excluded(&n, base) {
                    continue;
                }
                let rest = &n[root_slash.len()..];
                let variant = rest.split('/').next().unwrap_or("").to_string();
                by_variant.entry(variant).or_default().push(base.to_string());
            }
        }
        if by_variant.is_empty() {
            println!("  !! no SK_ meshes under char_root");
            continue;
        }

        // 3. pick variant: prefer default/common/base, else most meshes.
        let target = ["default", "common", "base"]
            .iter()
            .find(|v| by_variant.contains_key(**v))
            .map(|v| v.to_string())
            .unwrap_or_else(|| {
                by_variant.iter().max_by_key(|(_, m)| m.len()).map(|(v, _)| v.clone()).unwrap()
            });
        println!("  variants: {:?}", by_variant.keys().collect::<Vec<_>>());
        println!("  -> variant '{target}':");
        for m in &by_variant[&target] {
            println!("       [{}] {m}", region_of(m));
        }
    }
}
