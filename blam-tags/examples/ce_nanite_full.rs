//! Phase 6: decode a full high-res Nanite mesh + validate vs NumInputTriangles.
use std::sync::Arc;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::nanite::{NaniteResources, decode_nanite};
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let want=norm(&std::env::args().nth(1).unwrap_or_else(||"sm_pelican_hull_m_cabin_default".into()));
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    for a in &ar {
        let Some(e)=a.entries().iter().find(|e|norm(&e.path).contains(&want)&&norm(&e.path).ends_with(".uasset")) else{continue};
        let uasset=a.read(&e.path).unwrap();
        let h=FZenPackageHeader::deserialize(&mut std::io::Cursor::new(&uasset[..]),None,CV,HV,None).unwrap();
        let Some(res)=NaniteResources::parse(&uasset,h.summary.header_size as usize) else{eprintln!("no nanite resources");return};
        println!("{}",e.path);
        println!("  NumInputTriangles={} NumClusters={} NumRootPages={} pages={} deps={}",res.num_input_triangles,res.num_clusters,res.num_root_pages,res.streaming_states.len(),res.page_dependencies.len());
        let ubulk=a.read_bulk_for(e.chunk_index,0).unwrap_or_default();
        println!("  ubulk={} B",ubulk.len());
        let t0=std::time::Instant::now();
        let mesh=decode_nanite(&uasset,&ubulk,&res);
        let dt=t0.elapsed();
        let mut bbox=[[f32::MAX;3],[f32::MIN;3]];
        for p in &mesh.positions { for k in 0..3 {bbox[0][k]=bbox[0][k].min(p[k]); bbox[1][k]=bbox[1][k].max(p[k]);} }
        println!("  DECODED: {} verts, {} tris ({} unresolved) in {:?}",mesh.positions.len(),mesh.triangles.len(),mesh.unresolved_vertices,dt);
        println!("  vs NumInputTriangles {} -> ratio {:.3}",res.num_input_triangles,mesh.triangles.len() as f64/res.num_input_triangles as f64);
        println!("  bbox cm extent [{:.1} {:.1} {:.1}]",bbox[1][0]-bbox[0][0],bbox[1][1]-bbox[0][1],bbox[1][2]-bbox[0][2]);
        return;
    }
}
