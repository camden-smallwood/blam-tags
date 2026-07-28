//! For each bone with a child, which LOCAL axis of the node's object-space
//! rotation points toward the child? The dominant axis = the rig's "down-bone"
//! convention. Compares Reach (render_model) vs CE (skeleton_model).
use std::sync::Arc;
use blam_tags::file::TagFile; use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::RealQuaternion;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const REACH:&str="/Users/camden/Halo/haloreach_mcc/tags/objects/characters/spartans/spartans.render_model";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn qn(q:RealQuaternion)->RealQuaternion{let m=(q.i*q.i+q.j*q.j+q.k*q.k+q.w*q.w).sqrt().max(1e-9);RealQuaternion{i:q.i/m,j:q.j/m,k:q.k/m,w:q.w/m}}
fn qm(a:&RealQuaternion,b:&RealQuaternion)->RealQuaternion{RealQuaternion{i:a.w*b.i+a.i*b.w+a.j*b.k-a.k*b.j,j:a.w*b.j-a.i*b.k+a.j*b.w+a.k*b.i,k:a.w*b.k+a.i*b.j-a.j*b.i+a.k*b.w,w:a.w*b.w-a.i*b.i-a.j*b.j-a.k*b.k}}
fn qrot(q:&RealQuaternion,v:[f32;3])->[f32;3]{let(x,y,z,w)=(q.i,q.j,q.k,q.w);let tx=2.0*(y*v[2]-z*v[1]);let ty=2.0*(z*v[0]-x*v[2]);let tz=2.0*(x*v[1]-y*v[0]);[v[0]+w*tx+(y*tz-z*ty),v[1]+w*ty+(z*tx-x*tz),v[2]+w*tz+(x*ty-y*tx)]}
fn qinv(q:&RealQuaternion)->RealQuaternion{RealQuaternion{i:-q.i,j:-q.j,k:-q.k,w:q.w}}
fn analyze(label:&str,tag:&TagFile){
    let nb=tag.root().field_path("nodes").and_then(|f|f.as_block()).unwrap();
    let n=nb.len(); let mut par=vec![-1i32;n]; let mut lt=vec![[0f32;3];n]; let mut lr=vec![RealQuaternion{i:0.,j:0.,k:0.,w:1.};n];
    for i in 0..n{let e=nb.element(i).unwrap();par[i]=e.read_int_any("parent node").map(|x|x as i32).unwrap_or(-1);let t=e.read_point3d("default translation");lt[i]=[t.x,t.y,t.z];lr[i]=qn(e.read_quat("default rotation"));}
    // FK object transforms
    let mut op=vec![[0f32;3];n];let mut oq=vec![RealQuaternion{i:0.,j:0.,k:0.,w:1.};n];
    for i in 0..n{let p=par[i];if p<0{op[i]=lt[i];oq[i]=lr[i];}else{let pi=p as usize;let r=qrot(&oq[pi],lt[i]);op[i]=[op[pi][0]+r[0],op[pi][1]+r[1],op[pi][2]+r[2]];oq[i]=qn(qm(&oq[pi],&lr[i]));}}
    // for each node, use its FIRST child; direction to child in node-local frame
    let mut firstchild=vec![-1i32;n]; for i in 0..n{let p=par[i]; if p>=0 && firstchild[p as usize]<0{firstchild[p as usize]=i as i32;}}
    let labels=["+X","-X","+Y","-Y","+Z","-Z"];
    let mut tally=[0u32;6]; let mut count=0;
    for i in 0..n{let c=firstchild[i]; if c<0{continue;} let ci=c as usize;
        let d=[op[ci][0]-op[i][0],op[ci][1]-op[i][1],op[ci][2]-op[i][2]];
        let m=(d[0]*d[0]+d[1]*d[1]+d[2]*d[2]).sqrt(); if m<1e-5{continue;}
        let dn=[d[0]/m,d[1]/m,d[2]/m];
        // rotate into node-local: local = R^-1 * dir
        let ld=qrot(&qinv(&oq[i]),dn);
        // dominant axis
        let ax=[ld[0],ld[1],ld[2]]; let mut bi=0; for k in 1..3{if ax[k].abs()>ax[bi].abs(){bi=k;}}
        let idx=bi*2+ if ax[bi]<0.0{1}else{0}; tally[idx]+=1; count+=1;
    }
    print!("== {label}: {count} parent bones | down-bone local axis:"); for k in 0..6{if tally[k]>0{print!(" {}={}",labels[k],tally[k]);}} println!();
}
fn main(){
    if let Ok(t)=TagFile::read(std::path::Path::new(REACH)){analyze("REACH render_model",&t);}
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|suf:&str|ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(suf)).and_then(|e|a.read(&e.path).ok()));
    let m=TagFile::read_from_bytes(&read("objects/characters/spartans/spartans-model.ubulk").unwrap()).unwrap();
    let (_,sr)=m.root().read_tag_ref_with_group("skeleton model").unwrap();
    let s=TagFile::read_from_bytes(&read(&format!("{}-skeleton_model.ubulk",norm(&sr))).unwrap()).unwrap();
    analyze("CE skeleton_model",&s);
}
