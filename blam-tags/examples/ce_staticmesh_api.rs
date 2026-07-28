//! Confirm StaticMesh::from_package_preferring_nanite returns high-res geometry.
use std::sync::Arc;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::static_mesh::StaticMesh;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let want=norm(&std::env::args().nth(1).unwrap_or_else(||"sm_pelican_hull_m_default".into()));
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    for a in &ar {
        let Some(e)=a.entries().iter().find(|e|norm(&e.path).contains(&want)&&norm(&e.path).ends_with(".uasset")) else{continue};
        let uasset=a.read(&e.path).unwrap();
        let hs=FZenPackageHeader::deserialize(&mut std::io::Cursor::new(&uasset[..]),None,CV,HV,None).unwrap().summary.header_size as usize;
        let ubulk=a.read_bulk_for(e.chunk_index,0).ok();
        let fb=StaticMesh::from_package(&uasset,hs).unwrap();
        let hi=StaticMesh::from_package_preferring_nanite(&uasset,hs,ubulk.as_deref()).unwrap();
        println!("fallback : {} verts, {} tris",fb.vertices.len(),fb.indices.len()/3);
        println!("nanite   : {} verts, {} tris",hi.vertices.len(),hi.indices.len()/3);
        return;
    }
}
