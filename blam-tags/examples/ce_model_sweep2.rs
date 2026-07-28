//! Sweep ALL CE .model tags: for each, resolve mesh-sync DA -> actor BP ->
//! RuntimeRegions, and categorize by what meshes it binds (SkeletalMesh /
//! StaticMesh / both / none) and the actor BP type.
use std::sync::Arc; use std::io::Cursor; use std::collections::BTreeMap;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn base(p:&str)->String{let s=norm(p);let seg=s.rsplit(char::from(47)).next().unwrap_or("");seg.strip_suffix(".uasset").unwrap_or(seg).to_string()}
fn main(){
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let usmap=Usmap::meteorite().unwrap();
    // Pass 1: index DAs (basename -> model logical key), models, actor BPs.
    let mut da_to_model:BTreeMap<String,String>=BTreeMap::new();
    let mut models:Vec<String>=Vec::new();
    // BP: (imported DA basenames, sk_count, sm_count, regions, actor_type)
    struct Bp{das:Vec<String>,sk:usize,sm:usize,regions:usize}
    let mut bps:Vec<(String,Bp)>=Vec::new();
    for a in &ar{ for e in a.entries(){ let n=norm(&e.path);
        if n.ends_with("-model.ubulk"){ let k=n.rsplit("tags/").next().unwrap_or(&n).strip_suffix(".ubulk").unwrap().to_string(); models.push(k); continue; }
        if n.ends_with("meshsynchronization.uasset"){ let Ok(b)=a.read(&e.path) else{continue};
            if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){
                if let Some(mp)=h.imported_package_names.iter().find(|p|norm(p).ends_with("-model")){
                    da_to_model.insert(base(&e.path), norm(mp).rsplit("tags/").next().unwrap_or("").to_string());
                }}
            continue; }
        if n.ends_with("actor.uasset"){ let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
            let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization")) else{continue};
            let s=h.summary.header_size as usize+c.cooked_serial_offset as usize; let end=s+c.cooked_serial_size as usize;
            let Some(exp)=b.get(s..end) else{continue}; let names=h.name_map.copy_raw_names();
            let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap) else{continue};
            if rr.regions.is_empty(){continue;}
            let sk:usize=rr.regions.iter().flat_map(|r|&r.permutations).map(|p|p.skeletal_meshes.len()).sum();
            let sm:usize=rr.regions.iter().flat_map(|r|&r.permutations).map(|p|p.static_meshes.len()).sum();
            let das:Vec<String>=h.imported_package_names.iter().map(|p|base(p)).filter(|b|b.contains("meshsynchronization")).collect();
            bps.push((base(&e.path), Bp{das,sk,sm,regions:rr.regions.len()}));
        }
    }}
    models.sort(); models.dedup();
    // model logical key without "-model" for display
    // join: model -> DA(s) -> BP(s)
    let mut cat:BTreeMap<&str,usize>=BTreeMap::new();
    let mut examples:BTreeMap<&str,Vec<String>>=BTreeMap::new();
    for m in &models{
        // find DA importing this model
        let da:Vec<&String>=da_to_model.iter().filter(|(_,mm)| *mm==m).map(|(d,_)|d).collect();
        if da.is_empty(){ *cat.entry("NO_DA").or_default()+=1; examples.entry("NO_DA").or_default().push(m.clone()); continue; }
        // find BP importing any of these DAs; pick max (sk+sm)
        let mut best:Option<&Bp>=None;
        for (_,bp) in &bps{ if bp.das.iter().any(|d|da.iter().any(|dd|*dd==d)){ if best.map_or(true,|b|bp.sk+bp.sm>b.sk+b.sm){best=Some(bp);}}}
        let Some(bp)=best else{ *cat.entry("DA_NO_BP").or_default()+=1; examples.entry("DA_NO_BP").or_default().push(m.clone()); continue; };
        let c = if bp.sk>0 && bp.sm>0 {"SK+SM"} else if bp.sk>0 {"SK_only"} else if bp.sm>0 {"SM_only"} else {"empty"};
        *cat.entry(c).or_default()+=1; examples.entry(c).or_default().push(format!("{m} (sk={} sm={})",bp.sk,bp.sm));
    }
    println!("=== {} models, {} mesh-sync DAs, {} actor BPs w/ regions ===",models.len(),da_to_model.len(),bps.len());
    for (k,v) in &cat{ println!("  {k:10} {v}"); }
    for k in ["SK_only","SK+SM","SM_only","DA_NO_BP","empty"]{
        if let Some(ex)=examples.get(k){ println!("--- {k} (showing 8 of {}):",ex.len()); for e in ex.iter().take(8){println!("    {e}");}}
    }
}
