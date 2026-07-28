//! Verify the JMS rest-skeleton reorientation: build a CE model's skeleton-only
//! JMS and check that each bone's world local +X now points down-the-bone
//! (toward its first child) — Halo's X-down convention, matching the JMA —
//! rather than down +Y (the raw MetaHuman convention).
use std::sync::Arc;
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::jms::JmsFile;
use blam_tags::math::RealVector3d;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let key=norm(&std::env::args().nth(1).unwrap_or_else(||"objects/characters/brute/brute".into()));
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|s:&str|{let s=s.to_ascii_lowercase();ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&s)).and_then(|e|a.read(&e.path).ok()))};
    let model=TagFile::read_from_bytes(&read(&format!("{key}-model.ubulk")).unwrap()).unwrap();
    let (_,sr)=model.root().read_tag_ref_with_group("skeleton model").unwrap();
    let skel=TagFile::read_from_bytes(&read(&format!("{}-skeleton_model.ubulk",norm(&sr))).unwrap()).unwrap();
    // Skeleton-only JMS (no parts): nodes are the reoriented rest skeleton.
    let jms=JmsFile::from_ue_skeletal_meshes(&[],&skel).expect("jms");
    // Derive children from parent links; measure down-bone axis alignment.
    let n=jms.nodes.len();
    let mut first_child=vec![-1i32;n];
    for i in 0..n{let p=jms.nodes[i].parent;if p>=0 && first_child[p as usize]<0{first_child[p as usize]=i as i32;}}
    let (mut xd,mut yd,mut zd,mut total)=(0,0,0,0);
    for i in 0..n{
        let fc=first_child[i];if fc<0{continue}
        let a=&jms.nodes[i].translation;let b=&jms.nodes[fc as usize].translation;
        let d=RealVector3d{i:b.x-a.x,j:b.y-a.y,k:b.z-a.z};
        let len=(d.i*d.i+d.j*d.j+d.k*d.k).sqrt();if len<1e-6{continue}
        let d=RealVector3d{i:d.i/len,j:d.j/len,k:d.k/len};
        // world local axes = rotation * unit
        let q=jms.nodes[i].rotation.normalized();
        let lx=q.rotate(RealVector3d{i:1.0,j:0.0,k:0.0});
        let ly=q.rotate(RealVector3d{i:0.0,j:1.0,k:0.0});
        let lz=q.rotate(RealVector3d{i:0.0,j:0.0,k:1.0});
        let dot=|u:RealVector3d|(u.i*d.i+u.j*d.j+u.k*d.k).abs();
        let (dx,dy,dz)=(dot(lx),dot(ly),dot(lz));
        total+=1;
        if dx>=dy&&dx>=dz{xd+=1}else if dy>=dx&&dy>=dz{yd+=1}else{zd+=1}
    }
    if std::env::var("PERBONE").is_ok(){
        println!("== per-bone local down-axis (which local axis the bone points toward its child) ==");
        for i in 0..n{
            let fc=first_child[i];if fc<0{continue}
            let a=&jms.nodes[i].translation;let b=&jms.nodes[fc as usize].translation;
            let d=RealVector3d{i:b.x-a.x,j:b.y-a.y,k:b.z-a.z};
            let len=(d.i*d.i+d.j*d.j+d.k*d.k).sqrt();if len<1e-6{continue}
            let d=RealVector3d{i:d.i/len,j:d.j/len,k:d.k/len};
            let q=jms.nodes[i].rotation.normalized();
            let lx=q.rotate(RealVector3d{i:1.0,j:0.0,k:0.0});
            let ly=q.rotate(RealVector3d{i:0.0,j:1.0,k:0.0});
            let lz=q.rotate(RealVector3d{i:0.0,j:0.0,k:1.0});
            let dx=lx.i*d.i+lx.j*d.j+lx.k*d.k;
            let dy=ly.i*d.i+ly.j*d.j+ly.k*d.k;
            let dz=lz.i*d.i+lz.j*d.j+lz.k*d.k;
            let (ax,mv)=[("+X",dx),("-X",-dx),("+Y",dy),("-Y",-dy),("+Z",dz),("-Z",-dz)].iter().fold(("?",f32::MIN),|acc,&(nm,v)|if v>acc.1{(nm,v)}else{acc});
            println!("  {:24} -> local down {ax} ({mv:.2})",jms.nodes[i].name);
        }
    }
    println!("{key}: {n} nodes, {total} with a child");
    println!("dominant down-bone axis: X={xd} Y={yd} Z={zd}  (expect X dominant after reorientation)");
    // sanity: node position spread (unchanged by reorientation)
    let (mut mn,mut mx)=([f32::MAX;3],[f32::MIN;3]);
    for nd in &jms.nodes{let p=[nd.translation.x,nd.translation.y,nd.translation.z];for k in 0..3{mn[k]=mn[k].min(p[k]);mx[k]=mx[k].max(p[k]);}}
    println!("node bbox: [{:.2},{:.2},{:.2}]..[{:.2},{:.2},{:.2}]",mn[0],mn[1],mn[2],mx[0],mx[1],mx[2]);
}
