//! Feasibility probe for CE material-name extraction: decode each mesh's
//! material-slot array (`SkeletalMaterials`/`StaticMaterials`) + resolve default
//! material instances via the import table, and read the per-instance
//! `MaterialOverrides` off the RuntimeRegions. Reports parse success rate.
use std::sync::Arc; use std::io::Cursor; use std::collections::BTreeSet;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex, FPackageObjectIndexType};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::{MeshSyncRegions, read_material_slots};
use blam_tags::iostore::usmap::Usmap;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
const DA_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationDataAsset";
const COMP_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponent";
const COMP_BASE:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponentBase";
const SK_CLASS:&str="/Script/Engine.SkeletalMesh";
const SM_CLASS:&str="/Script/Engine.StaticMesh";
const PREFIX:usize=192*1024;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn hdr_prefix(a:&IoStoreArchive,path:&str)->Option<FZenPackageHeader>{
    let b=a.read_prefix(path,PREFIX).ok()?;
    FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).ok()
}
/// Resolve an FPackageIndex object ref (import if negative) to a package basename.
fn resolve_obj(hdr:&FZenPackageHeader,obj:i32)->Option<String>{
    if obj>=0{return None;}
    let imp=(-obj-1) as usize;
    let poi=hdr.import_map.get(imp)?;
    if poi.kind()==FPackageObjectIndexType::PackageImport{
        let r=poi.package_import()?;
        let name=hdr.imported_package_names.get(r.imported_package_index as usize)?;
        return Some(name.rsplit('/').next().unwrap_or(name).to_string());
    }
    None
}
fn main(){
    let key=norm(&std::env::args().nth(1).unwrap_or_else(||"objects/characters/brute/brute".into()));
    let da_c=FPackageObjectIndex::create_script_import(DA_CLASS);
    let comp_c=FPackageObjectIndex::create_script_import(COMP_CLASS);
    let comp_base_c=FPackageObjectIndex::create_script_import(COMP_BASE);
    let sk_c=FPackageObjectIndex::create_script_import(SK_CLASS);
    let sm_c=FPackageObjectIndex::create_script_import(SM_CLASS);
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read_pkg=|pkg:&str|{let t=norm(pkg);let t=t.strip_prefix("/game/").unwrap_or(&t);let suf=format!("/{t}.uasset");ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok()))};
    let usmap=Usmap::meteorite().unwrap();
    // resolve model -> one world actor's regions (class-based)
    let modkey=format!("{key}-model");
    let mut da=None;
    'o: for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with(".uasset"){continue};let Some(h)=hdr_prefix(a,&e.path) else{continue};if !h.exports_class(da_c){continue};if h.imported_package_names.iter().any(|p|norm(p).ends_with(&modkey)){da=Some(n.rsplit('/').next().unwrap().strip_suffix(".uasset").unwrap().to_string());break 'o;}}}
    let Some(da)=da else{println!("no DA for {modkey}");return;};
    let dal=norm(&da);
    let mut regions:Option<MeshSyncRegions>=None;
    'a: for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with(".uasset"){continue};let Some(h)=hdr_prefix(a,&e.path) else{continue};if !h.imported_package_names.iter().any(|p|norm(p).ends_with(&dal)){continue};if !(h.exports_class(comp_c)||h.exports_class(comp_base_c)){continue};let Ok(b)=a.read(&e.path) else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};let Some(c)=h.find_export_of_class(comp_c).or_else(||h.find_export_of_class(comp_base_c)) else{continue};let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;let end=s+c.cooked_serial_size as usize;let Some(exp)=b.get(s..end) else{continue};let names=h.name_map.copy_raw_names();if let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap){if rr.is_world(){regions=Some(rr);break 'a;}}}}
    let Some(regions)=regions else{println!("no world regions");return;};

    // Unique (package, is_skeletal) + collect overrides seen.
    let mut meshes:BTreeSet<(String,bool)>=BTreeSet::new();
    let mut overrides_seen=0usize;
    for r in &regions.regions{for p in &r.permutations{
        for m in &p.skeletal_meshes{meshes.insert((m.package.clone(),true));overrides_seen+=m.material_overrides.len();}
        for m in &p.static_meshes{meshes.insert((m.package.clone(),false));overrides_seen+=m.material_overrides.len();}
    }}
    println!("{modkey}: {} unique meshes, {overrides_seen} material-overrides across instances",meshes.len());
    // sample a few overrides
    for r in &regions.regions{for p in &r.permutations{for m in p.skeletal_meshes.iter().chain(&p.static_meshes){if !m.material_overrides.is_empty(){println!("  override on {}/{} {} -> {:?}",r.name,p.name,m.asset,m.material_overrides);break;}}}}

    if std::env::var("CE_DUMP").is_ok(){
        for cls in ["StaticMesh","SkeletalMesh"]{
            if let Some(fp)=usmap.flattened_properties(cls){
                println!("== {cls} schema (first 16) ==");
                for (i,p) in fp.iter().take(16).enumerate(){println!("  [{i}] {} : {:?}",p.name,p.ty);}
            }
        }
        for (pkg,is_sk) in &meshes{
            let Some(b)=read_pkg(pkg) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
            let cls=if *is_sk{sk_c}else{sm_c};
            let Some(exp)=h.find_export_of_class(cls) else{continue};
            let s=h.summary.header_size as usize+exp.cooked_serial_offset as usize;let end=(s+96).min(b.len());
            println!("== {} [{}] export first bytes ==",pkg.rsplit('/').next().unwrap_or(pkg),if *is_sk{"SK"}else{"SM"});
            if let Some(sl)=b.get(s..end){println!("  {}",sl.iter().map(|x|format!("{x:02x}")).collect::<Vec<_>>().join(" "));}
            break;
        }
        return;
    }
    // Robustness check: per mesh, how many distinct section material indices
    // (from the geometry reader) vs how many MI_/M_ imports it has. Single-slot
    // meshes name unambiguously from the sole import; multi-slot need the slot
    // array (which doesn't decode reliably for engine classes).
    use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
    use blam_tags::iostore::static_mesh::StaticMesh;
    let (mut single,mut multi,mut zero)=(0usize,0usize,0usize);
    for (pkg,is_sk) in &meshes{
        let Some(b)=read_pkg(pkg) else{continue};
        let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        let mis:Vec<String>=h.imported_package_names.iter().map(|p|p.rsplit('/').next().unwrap_or(p).to_string()).filter(|b|b.starts_with("MI_")||b.starts_with("M_")).collect();
        let sections:usize = if *is_sk {
            SkeletalMesh::from_package(&b,&h.name_map.copy_raw_names(),h.summary.header_size as usize).map(|m|{let mut s:BTreeSet<u16>=BTreeSet::new();for sec in &m.sections{s.insert(sec.material_index);}s.len()}).unwrap_or(0)
        } else {
            // Static reader merges to a single implicit material.
            StaticMesh::from_package(&b,h.summary.header_size as usize).map(|_|1usize).unwrap_or(0)
        };
        match sections {0=>zero+=1,1=>single+=1,_=>multi+=1}
        println!("  {} [{}]: {} section-slots, {} MI imports {:?}",pkg.rsplit('/').next().unwrap_or(pkg),if *is_sk{"SK"}else{"SM"},sections,mis.len(),mis);
    }
    println!("\nmesh material shape: {single} single-slot, {multi} multi-slot, {zero} unknown (of {})",meshes.len());
}
