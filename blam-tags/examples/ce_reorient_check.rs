//! Verify halo_bone_reorientation: after applying it to CE spartans' rest pose,
//! bones should be X-down (like Reach) and object-space joint POSITIONS must be
//! unchanged (deformation preserved).
use std::sync::Arc;
use blam_tags::file::TagFile; use blam_tags::iostore::IoStoreArchive;
use blam_tags::animation::{Animation, Skeleton};
use blam_tags::extract::animation::{build_defaults, additional_node_data_is_object_space, halo_bone_reorientation, apply_reorientation};
use blam_tags::math::{RealQuaternion, RealVector3d};
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn fk(sk:&Skeleton,loc:&[blam_tags::NodeTransform])->(Vec<RealQuaternion>,Vec<RealVector3d>){
    let n=sk.nodes.len();let mut oq=vec![RealQuaternion::IDENTITY;n];let mut op=vec![RealVector3d::ZERO;n];
    for i in 0..n{let t=loc[i].translation;let lt=RealVector3d{i:t.x,j:t.y,k:t.z};let lr=loc[i].rotation.normalized();
        match sk.nodes[i].parent{p if p<0=>{oq[i]=lr;op[i]=lt;}p=>{let pi=p as usize;oq[i]=(oq[pi]*lr).normalized();op[i]=op[pi]+oq[pi].rotate(lt);}}}
    (oq,op)}
fn axis_tally(sk:&Skeleton,oq:&[RealQuaternion],op:&[RealVector3d])->[u32;3]{
    let mut t=[0u32;3];
    for i in 0..sk.nodes.len(){let fc=sk.nodes[i].first_child;if fc<0{continue;}
        let d=(op[fc as usize]-op[i]).normalized();if d==RealVector3d::ZERO{continue;}
        let l=oq[i].conjugate().rotate(d);let(x,y,z)=(l.i.abs(),l.j.abs(),l.k.abs());
        let b=if x>=y&&x>=z{0}else if y>=z{1}else{2};t[b]+=1;}
    t}
fn main(){
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|s:&str|ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(s)).and_then(|e|a.read(&e.path).ok()));
    let jmad=TagFile::read_from_bytes(&read("objects/characters/spartans/spartans-model_animation_graph.ubulk").unwrap()).unwrap();
    let a=Animation::new(&jmad).unwrap(); let sk=Skeleton::from_tag(&jmad);
    let mut def=build_defaults(&sk,&jmad,None,additional_node_data_is_object_space(&a));
    let (oq0,op0)=fk(&sk,&def);
    let t0=axis_tally(&sk,&oq0,&op0);
    println!("BEFORE: down-bone axis X={} Y={} Z={}",t0[0],t0[1],t0[2]);
    let corr=halo_bone_reorientation(&sk,&def).expect("expected reorientation for CE");
    apply_reorientation(&mut def,&sk,&corr);
    let (oq1,op1)=fk(&sk,&def);
    let t1=axis_tally(&sk,&oq1,&op1);
    println!("AFTER : down-bone axis X={} Y={} Z={}",t1[0],t1[1],t1[2]);
    let maxdp=op0.iter().zip(&op1).map(|(a,b)|(*a-*b).length()).fold(0f32,f32::max);
    println!("max object-position drift after reorientation: {maxdp:.6} (should be ~0)");
}
