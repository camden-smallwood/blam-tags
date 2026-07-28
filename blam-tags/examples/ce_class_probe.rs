//! Empirically validate CLASS-based identification of mesh-sync data assets and
//! components (replacing the fragile filename heuristics), using `read_prefix`
//! (header-only decode) so a full-container scan is cheap.
//!
//! A native UClass shows up as an export's `class_index` = ScriptImport(
//! CityHash64 of the lowercased `/Script/Module.Class` path). So we can identify
//! any asset by class in O(1) against a precomputed hash.
use std::sync::Arc; use std::io::Cursor;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
const DA_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationDataAsset";
const COMP_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponent";
const COMP_BASE:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponentBase";
const PREFIX:usize=192*1024;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn hdr_prefix(a:&IoStoreArchive,path:&str)->Option<FZenPackageHeader>{
    let b=a.read_prefix(path,PREFIX).ok()?;
    FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).ok()
}
fn main(){
    let key=norm(&std::env::args().nth(1).unwrap_or_else(||"objects/vehicles/covenant/tuning_fork/tuning_fork".into()));
    let da_c=FPackageObjectIndex::create_script_import(DA_CLASS);
    let comp_c=FPackageObjectIndex::create_script_import(COMP_CLASS);
    let comp_base_c=FPackageObjectIndex::create_script_import(COMP_BASE);
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    println!("opened {} containers",ar.len());

    // Single header-only pass: classify each .uasset as DA / component-actor.
    let t=std::time::Instant::now();
    let mut da_names:Vec<String>=Vec::new(); // da package basenames (stem, lowercase)
    let mut prefix_fail=0usize;
    let mut n_scanned=0usize;
    let mut seen=std::collections::HashSet::new();
    for a in &ar{ for e in a.entries(){
        let n=norm(&e.path); if !n.ends_with(".uasset"){continue}
        if !seen.insert(n.clone()){continue}
        n_scanned+=1;
        let Some(h)=hdr_prefix(a,&e.path) else{prefix_fail+=1;continue};
        if h.exports_class(da_c){
            da_names.push(n.rsplit('/').next().unwrap_or(&n).strip_suffix(".uasset").unwrap_or(&n).to_string());
        }
    }}
    println!("\n== CLASS CENSUS (header-prefix scan of {n_scanned} .uasset in {:.1}s, {prefix_fail} prefix-parse fails) ==",t.elapsed().as_secs_f32());
    println!("BlamMeshSynchronizationDataAsset packages (by class): {}",da_names.len());
    let missed:Vec<&String>=da_names.iter().filter(|n|!n.starts_with("da_")).collect();
    println!("  ...NOT starting with `da_` (missed by prefix filter): {:?}",missed);

    // Resolve THIS model by class.
    let modkey=format!("{key}-model");
    println!("\n== RESOLVE {modkey} ==");
    let mut matched:Vec<String>=Vec::new();
    for a in &ar{ for e in a.entries(){
        let n=norm(&e.path); if !n.ends_with(".uasset"){continue}
        let Some(h)=hdr_prefix(a,&e.path) else{continue};
        if !h.exports_class(da_c){continue}
        if h.imported_package_names.iter().any(|p|norm(p).ends_with(&modkey)){
            matched.push(n.rsplit('/').next().unwrap_or(&n).strip_suffix(".uasset").unwrap_or(&n).to_string());
        }
    }}
    println!("mesh-sync DAs importing {modkey} (by class): {:?}",matched);
    let usmap=Usmap::meteorite().unwrap();
    for da in &matched{
        let dal=norm(da);
        let mut best:Option<((bool,usize),String,MeshSyncRegions)>=None;
        for a in &ar{ for e in a.entries(){
            let n=norm(&e.path); if !n.ends_with(".uasset"){continue}
            // header-only gate first
            let Some(h)=hdr_prefix(a,&e.path) else{continue};
            if !h.imported_package_names.iter().any(|p|norm(p).ends_with(&dal)){continue}
            if !(h.exports_class(comp_c)||h.exports_class(comp_base_c)){continue}
            // candidate: full read to slice the component export body
            let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
            let Some(c)=h.find_export_of_class(comp_c).or_else(||h.find_export_of_class(comp_base_c)) else{continue};
            let s=h.summary.header_size as usize+c.cooked_serial_offset as usize; let end=s+c.cooked_serial_size as usize;
            let Some(exp)=b.get(s..end) else{continue};
            let names=h.name_map.copy_raw_names();
            if let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap){
                let m:usize=rr.regions.iter().flat_map(|r|&r.permutations).map(|p|p.skeletal_meshes.len()+p.static_meshes.len()).sum();
                let sc=(rr.is_world(),m);
                if best.as_ref().map_or(true,|(b,_,_)|sc>*b){best=Some((sc,n.rsplit('/').next().unwrap_or(&n).to_string(),rr));}
            }
        }}
        match best{
            Some(((world,m),actor,rr))=>println!("  DA {da} -> actor {actor} (world={world}) : {} regions, {m} meshes",rr.regions.len()),
            None=>println!("  DA {da} -> NO actor found"),
        }
    }
}
