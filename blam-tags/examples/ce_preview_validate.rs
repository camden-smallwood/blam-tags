//! Mirror the Baboon CE preview: mesh-sync → collect SK+SM parts → from_ue_meshes,
//! then report region/perm bindings + the assembled bbox (should match the OBJ).
use std::sync::Arc; use std::io::Cursor; use std::collections::BTreeSet;
use blam_tags::file::TagFile; use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::static_mesh::StaticMesh;
use blam_tags::jms::{UeMeshPart, UeStaticPart};
use blam_tags::render_model::RenderModel;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let key=norm(&std::env::args().nth(1).unwrap_or_else(||"objects/vehicles/human/pelican/pelican".into()));
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|s:&str|{let s=s.to_ascii_lowercase();ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&s)).and_then(|e|a.read(&e.path).ok()))};
    let read_pkg=|pkg:&str|{let t=norm(pkg);let t=t.strip_prefix("/game/").unwrap_or(&t);let suf=format!("/{t}.uasset");ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok()))};
    let model=TagFile::read_from_bytes(&read(&format!("{key}-model.ubulk")).unwrap()).unwrap();
    let (_,sr)=model.root().read_tag_ref_with_group("skeleton model").unwrap();
    let skel=TagFile::read_from_bytes(&read(&format!("{}-skeleton_model.ubulk",norm(&sr))).unwrap()).unwrap();
    // variants -> needed
    let mut needed:BTreeSet<(String,String)>=BTreeSet::new();
    if let Some(vb)=model.root().field_path("variants").and_then(|f|f.as_block()){for i in 0..vb.len(){let Some(v)=vb.element(i) else{continue};if let Some(rb)=v.field("regions").and_then(|f|f.as_block()){for j in 0..rb.len(){let Some(r)=rb.element(j) else{continue};let rn=r.read_string_id("region name").unwrap_or_default();let pn=r.field("permutations").and_then(|f|f.as_block()).and_then(|pb|pb.element(0)).and_then(|p|p.read_string_id("permutation name")).unwrap_or_default();if !rn.is_empty()&&!pn.is_empty(){needed.insert((rn.to_ascii_lowercase(),pn.to_ascii_lowercase()));}}}}}
    // meshsync regions
    let modkey=format!("{key}-model"); let mut da=None;
    'o: for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with("meshsynchronization.uasset"){continue};let Ok(b)=a.read(&e.path) else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};if h.imported_package_names.iter().any(|p|norm(p).ends_with(&modkey)){da=Some(n.rsplit('/').next().unwrap().strip_suffix(".uasset").unwrap().to_string());break 'o;}}}
    let da=da.unwrap(); let usmap=Usmap::meteorite().unwrap(); let mut best:Option<((bool,usize),MeshSyncRegions)>=None;
    for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with("actor.uasset"){continue};let Ok(b)=a.read(&e.path) else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};if !h.imported_package_names.iter().any(|p|norm(p).ends_with(&da)){continue};let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization")) else{continue};let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;let end=s+c.cooked_serial_size as usize;let Some(exp)=b.get(s..end) else{continue};let names=h.name_map.copy_raw_names();if let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap){let m:usize=rr.regions.iter().flat_map(|r|&r.permutations).map(|p|p.skeletal_meshes.len()+p.static_meshes.len()).sum();let sc=(rr.is_world(),m);if best.as_ref().map_or(true,|(b,_):&((bool,usize),MeshSyncRegions)|sc>*b){best=Some((sc,rr));}}}}
    let regions=best.map(|(_,r)|r).unwrap();
    // collect
    let mut sk_store=Vec::new(); let mut sm_store=Vec::new();
    for (region,perm) in &needed{
        for m in regions.skeletal_meshes(region,perm){if let Some(b)=read_pkg(&m.package){if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){let names=h.name_map.copy_raw_names();if let Ok(mesh)=SkeletalMesh::from_package(&b,&names,h.summary.header_size as usize){sk_store.push((region.clone(),perm.clone(),m.asset.clone(),mesh));}}}}
        for m in regions.static_meshes(region,perm){if let Some(b)=read_pkg(&m.package){if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){if let Ok(mesh)=StaticMesh::from_package(&b,h.summary.header_size as usize){sm_store.push((region.clone(),perm.clone(),m.asset.clone(),mesh,m.parent_bone.clone()));}}}}
    }
    let parts:Vec<UeMeshPart>=sk_store.iter().map(|(r,p,n,m)|UeMeshPart{mesh:m,region:r.clone(),permutation:p.clone(),name:n.clone(),material_names:vec![]}).collect();
    let sparts:Vec<UeStaticPart>=sm_store.iter().map(|(r,p,n,m,b)|UeStaticPart{mesh:m,bone_name:b.clone(),region:r.clone(),permutation:p.clone(),name:n.clone(),material_names:vec![]}).collect();
    println!("collected {} skeletal + {} static parts",parts.len(),sparts.len());
    let (rm,meshes)=RenderModel::from_ue_meshes(&parts,&sparts,&skel).expect("synth");
    println!("render_model: {} meshes, {} nodes, {} regions",meshes.len(),rm.nodes.len(),rm.regions.len());
    for reg in &rm.regions{ for p in &reg.permutations{ if p.mesh_count>0 && needed.contains(&(reg.name.to_ascii_lowercase(),p.name.to_ascii_lowercase())){ println!("  {}/{}: mesh[{}..{}] ({} meshes)",reg.name,p.name,p.mesh_index,p.mesh_index+p.mesh_count,p.mesh_count);}}}
    // bbox of all meshes referenced by needed perms
    let mut mn=[f32::MAX;3];let mut mx=[f32::MIN;3];let mut nv=0;
    for reg in &rm.regions{for p in &reg.permutations{ if !needed.contains(&(reg.name.to_ascii_lowercase(),p.name.to_ascii_lowercase())){continue;} for mi in p.mesh_index.max(0)..(p.mesh_index.max(0)+p.mesh_count.max(0)){if let Some(m)=meshes.get(mi as usize){for v in &m.vertices{nv+=1;mn[0]=mn[0].min(v.position.x);mn[1]=mn[1].min(v.position.y);mn[2]=mn[2].min(v.position.z);mx[0]=mx[0].max(v.position.x);mx[1]=mx[1].max(v.position.y);mx[2]=mx[2].max(v.position.z);}}}}}
    let wu=304.8;
    println!("assembled variant: {nv} verts, bbox {:.1}x{:.1}x{:.1}m",(mx[0]-mn[0])*wu/100.0,(mx[1]-mn[1])*wu/100.0,(mx[2]-mn[2])*wu/100.0);
}
