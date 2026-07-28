use std::sync::Arc; use std::io::Cursor;
use blam_tags::iostore::IoStoreArchive; use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions; use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion; use blam_tags::iostore::ue_types::EIoStoreTocVersion;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash; const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
 let pat=norm(&std::env::args().nth(1).unwrap_or_else(||"bp_fp_assaultrifle_weaponactor".into()));
 let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
 let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
 let usmap=Usmap::meteorite().unwrap();
 for a in &ar{for e in a.entries().iter().filter(|e|norm(&e.path).contains(&pat)&&norm(&e.path).ends_with(".uasset")){
   let Ok(b)=a.read(&e.path) else{continue};
   let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
   println!("== {} ==",norm(&e.path).rsplit('/').next().unwrap());
   // imports referencing render models / fp
   for p in h.imported_package_names.iter().filter(|p|{let n=norm(p);n.contains("_fp")||n.contains("firstperson")||n.contains("-model")||n.ends_with("hands")||n.contains("arm")}){println!("  import: {}",norm(p));}
   let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization")) else{println!("  (no mesh-sync component)");continue};
   let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;
   let Some(exp)=b.get(s..s+c.cooked_serial_size as usize) else{continue};
   let names=h.name_map.copy_raw_names();
   if let Ok(r)=MeshSyncRegions::from_component_export(exp,&names,&usmap){
     println!("  SynchronizedActorType={:?} is_world={}",r.synchronized_actor_type,r.is_world());
     for reg in &r.regions{for pm in &reg.permutations{
       let sk:Vec<_>=pm.skeletal_meshes.iter().map(|m|m.asset.as_str()).collect();
       let sm:Vec<_>=pm.static_meshes.iter().map(|m|m.asset.as_str()).collect();
       if !sk.is_empty()||!sm.is_empty(){println!("  region '{}' perm '{}': SK{:?} SM{:?}",reg.name,pm.name,sk,sm);}
     }}
   }
   return;
 }}
 println!("not found: {pat}");
}
