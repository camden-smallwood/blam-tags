use std::io::Cursor;
use std::sync::Arc;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let dir = std::env::args().nth(1).unwrap_or_else(||"characters/marine/".into());
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    // For each non-mesh .uasset under dir, count references to sk_marine_* meshes (imports + name map).
    for a in &ar { for e in a.entries(){ let n=norm(&e.path);
        if !n.contains(&dir)||!n.ends_with(".uasset") {continue;}
        let base=n.rsplit('/').next().unwrap_or("");
        if base.starts_with("sk_")||base.starts_with("sm_")||n.contains("/textures/")||n.contains("/materials/"){continue;}
        let Ok(b)=a.read(&e.path) else{continue};
        let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        let names=h.name_map.copy_raw_names();
        let mesh_refs:Vec<&String>=names.iter().filter(|s|{let l=s.to_ascii_lowercase(); l.contains("torso")||l.contains("sk_marine_arms")||l.contains("sk_marine_legs")||l.contains("_helmet")||l.contains("anatomy")}).collect();
        let mesh_imports=h.imported_package_names.iter().filter(|p|{let l=norm(p); l.contains("/sk_")||l.contains("/sm_")}).count();
        if mesh_refs.len()>=3 || mesh_imports>=3 {
            println!("{base}: {} name-map mesh-ish refs, {mesh_imports} mesh imports", mesh_refs.len());
            for m in mesh_refs.iter().take(10){println!("    name: {m}");}
        }
    }}
}
