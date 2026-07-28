//! Run every .model through the preview resolution: find its mesh-sync BP,
//! decode RuntimeRegions, and try to LOAD every SK/SM mesh it binds. Reports
//! how many models fully resolve vs partial vs no-mesh-sync, + failure reasons.
use std::sync::Arc; use std::io::Cursor; use std::collections::{BTreeMap,BTreeSet,HashMap};
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::static_mesh::StaticMesh;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn base(p:&str)->String{let s=norm(p);let g=s.rsplit('/').next().unwrap_or("");g.strip_suffix(".uasset").unwrap_or(g).to_string()}
fn main(){
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let usmap=Usmap::meteorite().unwrap();
    // index: package path (lowercased, no ext) -> (container idx, entry path)
    let mut pkg_index:HashMap<String,(usize,String)>=HashMap::new();
    let mut models:Vec<String>=Vec::new();
    let mut da_to_model:BTreeMap<String,String>=BTreeMap::new();
    struct Bp{das:Vec<String>, sk:Vec<String>, sm:Vec<String>}
    let mut bps:Vec<Bp>=Vec::new();
    for (ci,a) in ar.iter().enumerate(){ for e in a.entries(){ let n=norm(&e.path);
        if n.ends_with(".uasset"){ let stem=n.rsplit("content/").next().unwrap_or(&n).strip_suffix(".uasset").unwrap_or("").to_string(); pkg_index.entry(stem).or_insert((ci,e.path.clone())); }
        if n.ends_with("-model.ubulk"){ models.push(n.rsplit("tags/").next().unwrap_or(&n).strip_suffix(".ubulk").unwrap().to_string()); }
    }}
    for a in &ar{ for e in a.entries(){ let n=norm(&e.path);
        if n.ends_with("meshsynchronization.uasset"){ if let Ok(b)=a.read(&e.path){ if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){ if let Some(mp)=h.imported_package_names.iter().find(|p|norm(p).ends_with("-model")){ da_to_model.insert(base(&e.path), norm(mp).rsplit("tags/").next().unwrap_or("").to_string()); }}}}
        if n.ends_with("actor.uasset"){ let Ok(b)=a.read(&e.path) else{continue}; let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue}; let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization")) else{continue}; let s=h.summary.header_size as usize+c.cooked_serial_offset as usize; let end=s+c.cooked_serial_size as usize; let Some(exp)=b.get(s..end) else{continue}; let names=h.name_map.copy_raw_names(); let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap) else{continue}; if rr.regions.is_empty(){continue};
            let das:Vec<String>=h.imported_package_names.iter().map(|p|base(p)).filter(|b|b.contains("meshsynchronization")).collect();
            let sk:Vec<String>=rr.regions.iter().flat_map(|r|&r.permutations).flat_map(|p|&p.skeletal_meshes).map(|m|norm(&m.package)).collect();
            let sm:Vec<String>=rr.regions.iter().flat_map(|r|&r.permutations).flat_map(|p|&p.static_meshes).map(|m|norm(&m.package)).collect();
            bps.push(Bp{das,sk,sm});
        }
    }}
    models.sort(); models.dedup();
    let read_pkg=|pkg:&str|->Option<Vec<u8>>{ let t=pkg.strip_prefix("/game/").unwrap_or(pkg); pkg_index.get(t).and_then(|(ci,p)|ar[*ci].read(p).ok()) };
    let mut load_cache:HashMap<String,Result<(),String>>=HashMap::new();
    let mut load=|pkg:&str,is_sk:bool|->Result<(),String>{
        if let Some(r)=load_cache.get(pkg){return r.clone();}
        let r=(||{ let b=read_pkg(pkg).ok_or("no package")?; let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).map_err(|e|format!("hdr:{e}"))?; if is_sk{let names=h.name_map.copy_raw_names(); SkeletalMesh::from_package(&b,&names,h.summary.header_size as usize).map(|_|()).map_err(|e|format!("sk:{e}"))} else {StaticMesh::from_package(&b,h.summary.header_size as usize).map(|_|()).map_err(|e|format!("sm:{e}"))} })();
        load_cache.insert(pkg.to_string(),r.clone()); r
    };
    let mut cat:BTreeMap<&str,usize>=BTreeMap::new();
    let mut fail_reasons:BTreeMap<String,usize>=BTreeMap::new();
    let mut partial_examples=Vec::new();
    for m in &models{
        let da:Vec<&String>=da_to_model.iter().filter(|(_,mm)|*mm==m).map(|(d,_)|d).collect();
        if da.is_empty(){ *cat.entry("no_meshsync").or_default()+=1; continue; }
        let bp=bps.iter().filter(|bp|bp.das.iter().any(|d|da.iter().any(|dd|*dd==d))).max_by_key(|bp|bp.sk.len()+bp.sm.len());
        let Some(bp)=bp else{ *cat.entry("da_no_bp").or_default()+=1; continue; };
        let sks:BTreeSet<&String>=bp.sk.iter().collect(); let sms:BTreeSet<&String>=bp.sm.iter().collect();
        let total=sks.len()+sms.len();
        if total==0{ *cat.entry("meshsync_no_mesh").or_default()+=1; continue; }
        let mut ok=0; let mut fails=Vec::new();
        for p in &sks{ match load(p,true){Ok(_)=>ok+=1,Err(e)=>{let k=e.split_whitespace().next().unwrap_or(&e).chars().take(24).collect::<String>();*fail_reasons.entry(k).or_default()+=1;fails.push((p.rsplit('/').next().unwrap_or(p).to_string(),e));}}}
        for p in &sms{ match load(p,false){Ok(_)=>ok+=1,Err(e)=>{let k=e.split_whitespace().next().unwrap_or(&e).chars().take(24).collect::<String>();*fail_reasons.entry(k).or_default()+=1;fails.push((p.rsplit('/').next().unwrap_or(p).to_string(),e));}}}
        if fails.is_empty(){ *cat.entry("FULLY_RESOLVED").or_default()+=1; }
        else if ok>0{ *cat.entry("PARTIAL").or_default()+=1; if partial_examples.len()<10{partial_examples.push((m.clone(),ok,fails.len(),fails.get(0).cloned()));}}
        else{ *cat.entry("ALL_FAILED").or_default()+=1; if partial_examples.len()<10{partial_examples.push((m.clone(),0,fails.len(),fails.get(0).cloned()));}}
    }
    println!("=== {} models ===",models.len());
    for (k,v) in &cat{println!("  {k:18} {v}");}
    println!("--- failure reasons (unique meshes) ---");
    for (k,v) in &fail_reasons{println!("  {v:4} {k}");}
    println!("--- partial/failed examples ---");
    for (m,ok,nf,ex) in &partial_examples{println!("  {m}  ({ok} ok, {nf} fail)  e.g. {:?}",ex.as_ref().map(|(n,e)|format!("{n}: {e}")));}
}
