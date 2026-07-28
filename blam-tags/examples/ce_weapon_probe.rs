use std::sync::Arc; use std::io::Cursor; use std::collections::BTreeSet;
use blam_tags::iostore::IoStoreArchive; use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh; use blam_tags::iostore::unversioned::MeshSyncRegions; use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion; use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::file::TagFile;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash; const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn read_pkg(ar:&[Arc<IoStoreArchive>],pk:&str)->Option<Vec<u8>>{let tail=norm(pk);let tail=tail.strip_prefix("/game/").unwrap_or(&tail);let suf=format!("/{tail}.uasset");for a in ar{for e in a.entries(){if norm(&e.path).ends_with(&suf){return a.read(&e.path).ok();}}}None}
fn main(){
 let model=std::env::args().nth(1).unwrap_or_else(||"objects/weapons/rifle/assault_rifle/assault_rifle-model".into());
 let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
 let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
 let usmap=Usmap::meteorite().unwrap();
 let hb=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&format!("{model}.ubulk"))).and_then(|e|a.read(&e.path).ok())).expect("hlmt");
 let hlmt=TagFile::read_from_bytes(&hb).unwrap();
 let (_,skref)=hlmt.root().read_tag_ref_with_group("skeleton model").unwrap();
 let skf=format!("{}-skeleton_model.ubulk",norm(&skref).rsplit('/').next().unwrap());
 let sb=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&skf)).and_then(|e|a.read(&e.path).ok())).unwrap();
 let sktag=TagFile::read_from_bytes(&sb).unwrap();
 let nb=sktag.root().field_path("nodes").and_then(|f|f.as_block()).unwrap();
 let tnames:BTreeSet<String>=(0..nb.len()).map(|i|nb.element(i).unwrap().read_string_id("name").unwrap_or_default().to_lowercase()).collect();
 println!("TAG skeleton {} nodes: {:?}",tnames.len(),tnames.iter().take(30).collect::<Vec<_>>());
 // mesh-sync
 let mk=norm(&model).strip_suffix("-model").unwrap_or(&norm(&model)).to_string();
 let mut da=vec![];for a in &ar{for e in a.entries().iter().filter(|e|norm(&e.path).ends_with("meshsynchronization.uasset")){let Ok(b)=a.read(&e.path)else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None)else{continue};if h.imported_package_names.iter().any(|p|norm(p).ends_with(&format!("{mk}-model"))){if let Some(x)=norm(&e.path).rsplit('/').next(){da.push(x.strip_suffix(".uasset").unwrap_or(x).to_string());}}}}
 let mut best:Option<(usize,MeshSyncRegions)>=None;
 for a in &ar{for e in a.entries().iter().filter(|e|norm(&e.path).ends_with("actor.uasset")){let Ok(b)=a.read(&e.path)else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None)else{continue};if !h.imported_package_names.iter().any(|p|{let bp=norm(p);da.iter().any(|d|bp.ends_with(d.as_str()))}){continue;}let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization"))else{continue};let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;let Some(exp)=b.get(s..s+c.cooked_serial_size as usize)else{continue};let names=h.name_map.copy_raw_names();if let Ok(r)=MeshSyncRegions::from_component_export(exp,&names,&usmap){let m:usize=r.regions.iter().flat_map(|x|&x.permutations).map(|p|p.skeletal_meshes.len()+p.static_meshes.len()).sum();if best.as_ref().map_or(true,|(bm,_)|m>*bm){best=Some((m,r));}}}}
 let regions=best.unwrap().1;
 // static bones: are they in tag?
 let mut statbones:BTreeSet<String>=BTreeSet::new(); let mut skmeshes:BTreeSet<String>=BTreeSet::new();
 for r in &regions.regions{for p in &r.permutations{for m in &p.static_meshes{statbones.insert(m.parent_bone.to_lowercase());}for m in &p.skeletal_meshes{skmeshes.insert(m.asset.clone());}}}
 let in_tag:Vec<_>=statbones.iter().filter(|b|tnames.contains(*b)).collect();
 let not_tag:Vec<_>=statbones.iter().filter(|b|!tnames.contains(*b)&&!b.is_empty()).collect();
 println!("static parent_bones: {} total, {} in tag, {} NOT in tag: {:?}",statbones.len(),in_tag.len(),not_tag.len(),not_tag);
 println!("skeletal meshes: {:?}",skmeshes);
 // dump bones of each skeletal mesh + whether in tag
 for sk in &skmeshes{ if let Some(b)=read_pkg(&ar,&format!("/Game/../{sk}")).or_else(||ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&format!("/{}.uasset",sk.to_lowercase()))).and_then(|e|a.read(&e.path).ok()))){
   if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){let names=h.name_map.copy_raw_names();if let Ok(m)=SkeletalMesh::from_package(&b,&names,h.summary.header_size as usize){let bn:Vec<_>=m.bones.iter().map(|x|x.name.to_lowercase()).collect();let int=bn.iter().filter(|b|tnames.contains(*b)).count();println!("  SK {sk}: {} bones, {} in tag, names={:?}",bn.len(),int,bn.iter().take(12).collect::<Vec<_>>());}}}}
}
