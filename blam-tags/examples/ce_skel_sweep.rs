//! Sweep every CE .model: compare the classic skeleton_model tag skeleton vs
//! the UE skeletal-mesh reference skeleton (bone counts, shared names, and
//! whether shared bones differ only by handedness or genuinely).
use std::sync::Arc;
use std::io::Cursor;
use std::collections::{HashMap, BTreeSet};
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::file::TagFile;
use blam_tags::math::{RealQuaternion, RealVector3d};
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
const C:f32=1.0/304.8;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn chain(n:usize,par:&[i32],lr:&[RealQuaternion],lt:&[RealVector3d])->Vec<RealVector3d>{
    let mut wr=vec![RealQuaternion{i:0.0,j:0.0,k:0.0,w:1.0};n];let mut wt=vec![RealVector3d{i:0.0,j:0.0,k:0.0};n];
    for i in 0..n{let p=par[i];if p>=0&&(p as usize)<i{let p=p as usize;wr[i]=(wr[p]*lr[i].normalized()).normalized();wt[i]=wt[p]+wr[p].rotate(lt[i]);}else{wr[i]=lr[i].normalized();wt[i]=lt[i];}}
    wt
}
fn read_pkg(ar:&[Arc<IoStoreArchive>],package:&str)->Option<(Vec<u8>,u32)>{
    let tail=norm(package);let tail=tail.strip_prefix("/game/").unwrap_or(&tail);let suf=format!("/{tail}.uasset");
    for a in ar{for e in a.entries(){if norm(&e.path).ends_with(&suf){return a.read(&e.path).ok().map(|b|(b,e.chunk_index));}}}None
}
fn ue_skel(ar:&[Arc<IoStoreArchive>],usmap:&Usmap,model_key:&str)->Option<(usize,HashMap<String,RealVector3d>)>{
    // DA importing model
    let mut da=vec![];
    for a in ar{for e in a.entries().iter().filter(|e|norm(&e.path).ends_with("meshsynchronization.uasset")){let Ok(b)=a.read(&e.path)else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None)else{continue};if h.imported_package_names.iter().any(|p|norm(p).ends_with(model_key)){if let Some(x)=norm(&e.path).rsplit('/').next(){da.push(x.strip_suffix(".uasset").unwrap_or(x).to_string());}}}}
    if da.is_empty(){return None;}
    // BP with most meshes
    let mut best:Option<(usize,MeshSyncRegions)>=None;
    for a in ar{for e in a.entries().iter().filter(|e|norm(&e.path).ends_with("actor.uasset")){let Ok(b)=a.read(&e.path)else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None)else{continue};if !h.imported_package_names.iter().any(|p|{let bp=norm(p);da.iter().any(|d|bp.ends_with(d.as_str()))}){continue;}let Some(c)=h.export_map.iter().find(|ex|h.name_map.get(ex.object_name).contains("MeshSynchronization"))else{continue};let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;let Some(exp)=b.get(s..s+c.cooked_serial_size as usize)else{continue};let names=h.name_map.copy_raw_names();if let Ok(r)=MeshSyncRegions::from_component_export(exp,&names,usmap){let m:usize=r.regions.iter().flat_map(|x|&x.permutations).map(|p|p.skeletal_meshes.len()).sum();if best.as_ref().map_or(true,|(bm,_)|m>*bm){best=Some((m,r));}}}}
    let regions=best?.1;
    // pick skeletal mesh with most bones
    let mut pkgs:BTreeSet<String>=BTreeSet::new();
    for r in &regions.regions{for p in &r.permutations{for m in &p.skeletal_meshes{pkgs.insert(m.package.clone());}}}
    let mut bestmesh:Option<SkeletalMesh>=None;
    for pk in pkgs{if let Some((b,_))=read_pkg(ar,&pk){if let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None){let names=h.name_map.copy_raw_names();if let Ok(m)=SkeletalMesh::from_package(&b,&names,h.summary.header_size as usize){if bestmesh.as_ref().map_or(true,|bm|m.bones.len()>bm.bones.len()){bestmesh=Some(m);}}}}}
    let mesh=bestmesh?;
    let n=mesh.bones.len();
    let par:Vec<i32>=mesh.bones.iter().map(|b|b.parent).collect();
    let lr:Vec<RealQuaternion>=mesh.bones.iter().map(|b|RealQuaternion{i:b.rest_rotation[0],j:b.rest_rotation[1],k:b.rest_rotation[2],w:b.rest_rotation[3]}).collect();
    let lt:Vec<RealVector3d>=mesh.bones.iter().map(|b|RealVector3d{i:b.rest_translation[0],j:b.rest_translation[1],k:b.rest_translation[2]}).collect();
    let wt=chain(n,&par,&lr,&lt);
    // convert to classic WU (x,-y,z)*C
    let map:HashMap<String,RealVector3d>=mesh.bones.iter().enumerate().map(|(i,b)|(b.name.to_lowercase(),RealVector3d{i:wt[i].i*C,j:-wt[i].j*C,k:wt[i].k*C})).collect();
    Some((n,map))
}
fn tag_skel(tag:&TagFile)->Option<(usize,HashMap<String,RealVector3d>)>{
    let root=tag.root();let nb=root.field_path("nodes").and_then(|f|f.as_block())?;let n=nb.len();
    let par:Vec<i32>=(0..n).map(|i|nb.element(i).unwrap().read_block_index("parent node") as i32).collect();
    let lr:Vec<RealQuaternion>=(0..n).map(|i|nb.element(i).unwrap().read_quat("default rotation")).collect();
    let lt:Vec<RealVector3d>=(0..n).map(|i|{let p=nb.element(i).unwrap().read_point3d("default translation");RealVector3d{i:p.x,j:p.y,k:p.z}}).collect();
    let wt=chain(n,&par,&lr,&lt);
    let names:Vec<String>=(0..n).map(|i|nb.element(i).unwrap().read_string_id("name").unwrap_or_default().to_lowercase()).collect();
    Some((n,names.into_iter().zip(wt).collect()))
}
fn main(){
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let usmap=Usmap::meteorite().unwrap();
    // enumerate all -model.ubulk
    let mut models:Vec<(String,String)>=vec![]; // (display, entrypath)
    for a in &ar{for e in a.entries().iter().filter(|e|norm(&e.path).ends_with("-model.ubulk")){models.push((norm(&e.path),e.path.clone()));}}
    models.sort();models.dedup();
    println!("{} .model tags",models.len());
    let (mut tot,mut with_ue,mut hand_only,mut genuine)=(0,0,0,0);
    for (disp,path) in &models{
        let Some((b,_))=ar.iter().find_map(|a|a.entries().iter().find(|e|&e.path==path).and_then(|e|a.read(&e.path).ok().map(|b|(b,0u32)))) else{continue};
        let Ok(tag)=TagFile::read_from_bytes(&b) else{continue};
        let Some((_,skref))=tag.root().read_tag_ref_with_group("skeleton model") else{continue};
        if skref.trim().is_empty(){continue;}
        tot+=1;
        // model_key from path: strip tags/ + -model.ubulk
        let mk={let s=disp.strip_suffix(".ubulk").unwrap_or(disp);s.rsplit("tags/").next().unwrap_or(s).to_string()};
        // tag skel: skref -> find -skeleton_model.ubulk
        let skfile=format!("{}-skeleton_model.ubulk",norm(&skref));
        let Some(sb)=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&skfile)).and_then(|e|a.read(&e.path).ok())) else{continue};
        let Ok(sktag)=TagFile::read_from_bytes(&sb) else{continue};
        let Some((tn,tmap))=tag_skel(&sktag) else{continue};
        let short=mk.rsplit('/').next().unwrap_or(&mk);
        match ue_skel(&ar,&usmap,&mk){
            Some((un,umap))=>{
                with_ue+=1;
                let shared:Vec<&String>=tmap.keys().filter(|k|umap.contains_key(*k)).collect();
                let mut match_h=0;let mut diff=0;let mut worst=0f32;let mut worstname=String::new();
                for k in &shared{let t=tmap[*k];let u=umap[*k];let d=((t.i-u.i).powi(2)+(t.j-u.j).powi(2)+(t.k-u.k).powi(2)).sqrt();if d<0.02{match_h+=1;}else{diff+=1;if d>worst{worst=d;worstname=(*k).clone();}}}
                if diff==0{hand_only+=1;}else{genuine+=1;}
                println!("{short:28} UE={un:3} tag={tn:3} shared={:3} handMatch={match_h:3} differ={diff:3} worst={worst:.2}WU({worstname})",shared.len());
            }
            None=>println!("{short:28} UE=?   tag={tn:3} (no UE skeletal mesh resolved)"),
        }
    }
    println!("\n== {tot} models w/ skeleton_model; {with_ue} resolved a UE skeleton; {hand_only} handedness-only match, {genuine} have genuinely-different bones ==");
}
