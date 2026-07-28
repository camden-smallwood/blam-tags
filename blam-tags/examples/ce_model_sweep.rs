//! Sweep every `.model` (hlmt) tag in Campaign Evolved and report whether it
//! resolves to UE meshes: OK / NO_DA (no MeshSynchronization link) / NO_MESH
//! (DA found but no SK_ meshes under its character root).
//!
//! Run: cargo run -p blam-tags --features iostore --example ce_model_sweep

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
fn is_excluded(n: &str, base: &str) -> bool {
    n.contains("/skeleton/")
        || ["shield", "shadow", "animdynamics", "destroyed", "_dmg", "damage", "collision", "physics", "imposter"]
            .iter().any(|k| base.contains(k))
}

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<Arc<IoStoreArchive>> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();

    // One pass: model keys, MeshSync DA paths, SK_ candidate paths.
    let mut models: Vec<String> = Vec::new(); // model_key
    let mut da_paths: Vec<String> = Vec::new(); // normalized MeshSync DA paths
    let mut sk_paths: Vec<String> = Vec::new(); // normalized SK_ candidate paths
    for a in &archives {
        for e in a.entries() {
            let n = norm(&e.path);
            if n.ends_with("-model.ubulk") {
                let stem = n.strip_suffix(".ubulk").unwrap();
                models.push(stem.rsplit("tags/").next().unwrap_or(stem).to_string());
            } else if n.ends_with("meshsynchronization.uasset") {
                da_paths.push(n);
            } else if n.ends_with(".uasset") {
                let base = n.rsplit('/').next().unwrap_or("");
                if base.starts_with("sk_") && !is_excluded(&n, base) {
                    sk_paths.push(n);
                }
            }
        }
    }
    models.sort();
    models.dedup();

    // Parse each MeshSync DA → map model_key -> char_root.
    let mut key_to_root: BTreeMap<String, String> = BTreeMap::new();
    for da in &da_paths {
        let bytes = archives.iter().find_map(|a| {
            a.entries().iter().find(|e| norm(&e.path) == *da).and_then(|e| a.read(&e.path).ok())
        });
        let Some(bytes) = bytes else { continue };
        let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None) else { continue };
        let root = da.rsplit_once("/common/").map(|(r, _)| r.to_string())
            .or_else(|| da.rsplit_once('/').map(|(r, _)| r.to_string()));
        let Some(root) = root else { continue };
        for p in &h.imported_package_names {
            let np = norm(p);
            if np.ends_with("-model") {
                if let Some(k) = np.rsplit("tags/").next() {
                    key_to_root.entry(k.to_string()).or_insert_with(|| root.clone());
                }
            }
        }
    }

    // For each model: resolve char_root, then count SK_ under it (variant-selected).
    let mut ok = 0;
    let mut no_da = Vec::new();
    let mut no_mesh = Vec::new();
    for m in &models {
        let Some(root) = key_to_root.get(m) else { no_da.push(m.clone()); continue };
        let root_slash = format!("{root}/");
        let mut by_variant: BTreeMap<String, usize> = BTreeMap::new();
        for s in &sk_paths {
            if let Some(i) = s.find(&root_slash) {
                let variant = s[i + root_slash.len()..].split('/').next().unwrap_or("").to_string();
                *by_variant.entry(variant).or_default() += 1;
            }
        }
        if by_variant.is_empty() {
            no_mesh.push(format!("{m}  (root {root})"));
        } else {
            ok += 1;
        }
    }

    println!("=== .model sweep: {} total ===", models.len());
    println!("  OK (resolved to meshes): {ok}");
    println!("  NO_DA (no MeshSynchronization link): {}", no_da.len());
    println!("  NO_MESH (DA found, no SK_ meshes): {}", no_mesh.len());

    // NO_DA broken down by whether it's a character (should resolve) or not.
    let (chars, others): (Vec<_>, Vec<_>) = no_da.iter().partition(|m| m.contains("characters/"));
    println!("\n  NO_DA characters ({}) — these SHOULD resolve:", chars.len());
    for m in &chars { println!("    {m}"); }
    println!("\n  NO_DA non-characters ({}, expected — weapons/vehicles/scenery):", others.len());
    for m in others.iter().take(12) { println!("    {m}"); }
    if others.len() > 12 { println!("    ... +{} more", others.len() - 12); }
    println!("\n  NO_MESH ({}) — DA links but mesh filter finds nothing:", no_mesh.len());
    for m in &no_mesh { println!("    {m}"); }
}
