//! Validate the UStaticMesh reader against real SM_ assets. Args: [substr]
use std::sync::Arc;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::static_mesh::StaticMesh;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let sub=norm(&std::env::args().nth(1).unwrap_or_else(||"sm_pelican".into()));
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let mut seen=std::collections::BTreeSet::new();
    let (mut ok,mut fail)=(0,0);
    for a in &ar{for e in a.entries(){let n=norm(&e.path);let base=n.rsplit('/').next().unwrap_or("");
        if !base.ends_with(".uasset")||!base.starts_with("sm_")||!n.contains(&sub){continue;}
        if base.contains("damaged")||base.contains("collision")||base.contains("_lod"){continue;}
        if !seen.insert(base.to_string()){continue;}
        let Ok(b)=a.read(&e.path) else{continue};
        let Ok(h)=FZenPackageHeader::deserialize(&mut std::io::Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        match StaticMesh::from_package(&b,h.summary.header_size as usize){
            Ok(m)=>{ let tris=m.indices.len()/3;
                let mut mn=[f32::MAX;3];let mut mx=[f32::MIN;3];for v in &m.vertices{for k in 0..3{mn[k]=mn[k].min(v.position[k]);mx[k]=mx[k].max(v.position[k]);}}
                let dim=if m.vertices.is_empty(){[0.0;3]}else{[mx[0]-mn[0],mx[1]-mn[1],mx[2]-mn[2]]};
                println!("  OK   {base:38} {:5}v {:5}t  min({:.0},{:.0},{:.0}) max({:.0},{:.0},{:.0})",m.vertices.len(),tris,mn[0],mn[1],mn[2],mx[0],mx[1],mx[2]); ok+=1;}
            Err(e)=>{println!("  FAIL {base:42} {e}"); fail+=1;}
        }
    }}
    println!("=> {ok} ok, {fail} fail");
}
