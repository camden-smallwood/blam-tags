//! Decode the authoritative CE region→permutation→mesh mapping from a
//! character's BP_*BipedActor mesh-sync component, via the RuntimeRegions
//! unversioned-property blob.
//! Run: cargo run -p blam-tags --features iostore --example ce_meshsync -- [bp.uasset substr]

use std::io::Cursor;
use std::sync::Arc;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p: &str) -> String { p.to_ascii_lowercase().replace('\\', "/") }

fn main() {
    let suf = std::env::args().nth(1).unwrap_or_else(|| "bp_basemarinebipedactor.uasset".into()).to_ascii_lowercase();
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let ar: Vec<Arc<IoStoreArchive>> = u.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let b = ar.iter().find_map(|a| a.entries().iter().find(|e| norm(&e.path).ends_with(&suf)).and_then(|e| a.read(&e.path).ok())).expect("bp");
    let h = FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]), None, CV, HV, None).unwrap();

    // Find the mesh-sync component export (name contains "MeshSynchronization").
    let names = h.name_map.copy_raw_names();
    let header_size = h.summary.header_size as usize;
    let comp = h.export_map.iter().find(|e| h.name_map.get(e.object_name).contains("MeshSynchronization"))
        .expect("no BlamMeshSynchronization export");
    let start = header_size + comp.cooked_serial_offset as usize;
    let end = start + comp.cooked_serial_size as usize;
    let export = &b[start..end];
    println!("component export '{}' — {} bytes", h.name_map.get(comp.object_name), export.len());

    let usmap = Usmap::meteorite().expect("usmap");
    let rr = match MeshSyncRegions::from_component_export(export, &names, &usmap) {
        Ok(rr) => rr,
        Err(e) => { eprintln!("decode failed: {e:#}"); std::process::exit(1); }
    };
    println!("SynchronizedActorType = {:?} (is_world={})", rr.synchronized_actor_type, rr.is_world());
    println!("{} regions:", rr.regions.len());
    for region in &rr.regions {
        println!("== {} ({} perms)", region.name, region.permutations.len());
        for p in &region.permutations {
            let sk: Vec<String> = p.skeletal_meshes.iter().map(|m| format!("{}[{}]", m.asset, m.class)).collect();
            let sm: Vec<String> = p.static_meshes.iter().map(|m| m.asset.clone()).collect();
            let mut line = format!("   {:24} SK={:?}", p.name, sk);
            if !sm.is_empty() { line += &format!(" SM={sm:?}"); }
            println!("{line}");
        }
    }

    // Resolve + load every referenced skeletal mesh package (mirrors the Baboon
    // path: package `/Game/..` → container `Content/..uasset` → SkeletalMesh).
    use std::collections::BTreeSet;
    let pkgs: BTreeSet<String> = rr.regions.iter().flat_map(|r| &r.permutations)
        .flat_map(|p| &p.skeletal_meshes).map(|m| m.package.clone()).collect();
    let mut ok = 0; let mut miss_entry = 0; let mut miss_load = 0;
    for pkg in &pkgs {
        let tail = pkg.to_ascii_lowercase().replace('\\',"/");
        let tail = tail.strip_prefix("/game/").unwrap_or(&tail);
        let suffix = format!("/{tail}.uasset");
        let found = ar.iter().find_map(|a| a.entries().iter().find(|e| norm(&e.path).ends_with(&suffix)).and_then(|e| a.read(&e.path).ok()));
        match found {
            None => { miss_entry += 1; eprintln!("  no entry for {pkg}"); }
            Some(bytes) => {
                let h2 = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None).unwrap();
                let nm2 = h2.name_map.copy_raw_names();
                match blam_tags::iostore::skeletal_mesh::SkeletalMesh::from_package(&bytes, &nm2, h2.summary.header_size as usize) {
                    Ok(m) => { ok += 1; let _ = m; }
                    Err(e) => { miss_load += 1; eprintln!("  load FAIL {pkg}: {e}"); }
                }
            }
        }
    }
    println!("\nmesh packages: {} total | {ok} loaded OK | {miss_entry} no-entry | {miss_load} load-fail", pkgs.len());
}
