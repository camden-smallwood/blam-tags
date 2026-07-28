//! Compare the CE pelican skeleton_model rig to the actual Halo Reach pelican
//! render_model rig, node-by-node: names, parent structure, and per-bone local
//! down-axis. If they match, CE kept the authored Reach vehicle rig (so no
//! MetaHuman reorientation applies); if CE's is uniformly Y-down, it was re-rigged.
use std::sync::Arc; use std::collections::BTreeMap;
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::jms::JmsFile;
use blam_tags::math::RealVector3d;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const REACH:&str="/Users/camden/Halo/haloreach_mcc/tags/objects/vehicles/human/pelican/pelican.render_model";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
/// name -> (parent_name, local-down-axis) from a JMS skeleton.
fn rig(jms:&JmsFile)->BTreeMap<String,(String,String)>{
    let n=jms.nodes.len();
    let mut fc=vec![-1i32;n];
    for i in 0..n{let p=jms.nodes[i].parent;if p>=0 && fc[p as usize]<0{fc[p as usize]=i as i32;}}
    let mut out=BTreeMap::new();
    for i in 0..n{
        let name=jms.nodes[i].name.clone();
        let parent=if jms.nodes[i].parent>=0{jms.nodes[jms.nodes[i].parent as usize].name.clone()}else{"-".into()};
        let axis=if fc[i]<0{"leaf".to_string()}else{
            let a=&jms.nodes[i].translation;let b=&jms.nodes[fc[i] as usize].translation;
            let d=RealVector3d{i:b.x-a.x,j:b.y-a.y,k:b.z-a.z};
            let len=(d.i*d.i+d.j*d.j+d.k*d.k).sqrt();
            if len<1e-6{"~0".into()}else{
                let d=RealVector3d{i:d.i/len,j:d.j/len,k:d.k/len};
                let q=jms.nodes[i].rotation.normalized();
                let lx=q.rotate(RealVector3d{i:1.0,j:0.0,k:0.0});
                let ly=q.rotate(RealVector3d{i:0.0,j:1.0,k:0.0});
                let lz=q.rotate(RealVector3d{i:0.0,j:0.0,k:1.0});
                let dx=lx.i*d.i+lx.j*d.j+lx.k*d.k;let dy=ly.i*d.i+ly.j*d.j+ly.k*d.k;let dz=lz.i*d.i+lz.j*d.j+lz.k*d.k;
                let cands=[("+X",dx),("-X",-dx),("+Y",dy),("-Y",-dy),("+Z",dz),("-Z",-dz)];
                cands.iter().max_by(|a,b|a.1.total_cmp(&b.1)).unwrap().0.to_string()
            }
        };
        out.insert(name,(parent,axis));
    }
    out
}
fn main(){
    // Reach render_model (self-describing MCC tag). Arg overrides the pelican.
    let reach_path=std::env::args().nth(1).unwrap_or_else(||REACH.to_string());
    let reach=TagFile::read(&reach_path).expect("reach render_model");
    let reach_jms=JmsFile::from_ue_skeletal_meshes(&[],&reach).expect("reach jms");
    // CE pelican skeleton_model (from container).
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|s:&str|{let s=s.to_ascii_lowercase();ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&s)).and_then(|e|a.read(&e.path).ok()))};
    let model=TagFile::read_from_bytes(&read("objects/vehicles/human/pelican/pelican-model.ubulk").unwrap()).unwrap();
    let (_,sr)=model.root().read_tag_ref_with_group("skeleton model").unwrap();
    let skel=TagFile::read_from_bytes(&read(&format!("{}-skeleton_model.ubulk",norm(&sr))).unwrap()).unwrap();
    let ce_jms=JmsFile::from_ue_skeletal_meshes(&[],&skel).expect("ce jms");

    let dump=|label:&str,jms:&JmsFile|{
        let n=jms.nodes.len();
        let mut fc=vec![-1i32;n];
        for i in 0..n{let p=jms.nodes[i].parent;if p>=0 && fc[p as usize]<0{fc[p as usize]=i as i32;}}
        println!("\n== {label}: {n} nodes ==");
        for i in 0..n{
            let nd=&jms.nodes[i];
            let par=if nd.parent>=0{jms.nodes[nd.parent as usize].name.clone()}else{"-".into()};
            let q=nd.rotation.normalized();
            // world local axes
            let lx=q.rotate(RealVector3d{i:1.0,j:0.0,k:0.0});
            let downaxis=if fc[i]<0{"leaf".to_string()}else{
                let a=&nd.translation;let b=&jms.nodes[fc[i] as usize].translation;
                let d=RealVector3d{i:b.x-a.x,j:b.y-a.y,k:b.z-a.z};let len=(d.i*d.i+d.j*d.j+d.k*d.k).sqrt();
                if len<1e-6{"~0".into()}else{let d=RealVector3d{i:d.i/len,j:d.j/len,k:d.k/len};
                    let ly=q.rotate(RealVector3d{i:0.0,j:1.0,k:0.0});let lz=q.rotate(RealVector3d{i:0.0,j:0.0,k:1.0});
                    let c=[("+X",lx.i*d.i+lx.j*d.j+lx.k*d.k),("-X",-(lx.i*d.i+lx.j*d.j+lx.k*d.k)),("+Y",ly.i*d.i+ly.j*d.j+ly.k*d.k),("-Y",-(ly.i*d.i+ly.j*d.j+ly.k*d.k)),("+Z",lz.i*d.i+lz.j*d.j+lz.k*d.k),("-Z",-(lz.i*d.i+lz.j*d.j+lz.k*d.k))];
                    c.iter().max_by(|a,b|a.1.total_cmp(&b.1)).unwrap().0.to_string()}
            };
            println!("  {:2} {:24} par={:20} pos[{:6.2},{:6.2},{:6.2}] wX[{:5.2},{:5.2},{:5.2}] down={}",
                i,nd.name,par,nd.translation.x,nd.translation.y,nd.translation.z,lx.i,lx.j,lx.k,downaxis);
        }
    };
    // RAW local node data (name, parent-index, local rotation quat, translation)
    // straight from the tag — to see the authored convention, not derived world.
    let raw=|label:&str,tag:&TagFile,is_vec:bool|{
        let root=tag.root();
        let Some(nb)=root.field_path("nodes").and_then(|f|f.as_block()) else{println!("{label}: no nodes block");return;};
        println!("\n== {label} RAW local nodes: {} ==",nb.len());
        for i in 0..nb.len(){
            let e=nb.element(i).unwrap();
            let name=e.read_string_id("name").or_else(||e.read_string("name")).unwrap_or_default();
            let par=e.read_block_index("parent node");
            let q=e.read_quat("default rotation");
            let t=if is_vec{let v=e.read_vec3("default translation");[v.i,v.j,v.k]}else{let p=e.read_point3d("default translation");[p.x,p.y,p.z]};
            println!("  {:2} {:22} par={:3} rot[{:6.3},{:6.3},{:6.3},{:6.3}] t[{:7.2},{:7.2},{:7.2}]",i,name,par,q.i,q.j,q.k,q.w,t[0],t[1],t[2]);
        }
    };
    raw("REACH render_model",&reach,false); raw("CE skeleton_model",&skel,false);
    dump("REACH",&reach_jms); dump("CE",&ce_jms);
    let r=rig(&reach_jms); let c=rig(&ce_jms);
    println!("\nReach nodes: {}, CE nodes: {}",r.len(),c.len());
    let allnames:std::collections::BTreeSet<&String>=r.keys().chain(c.keys()).collect();
    let (mut both,mut axis_match,mut parent_match,mut only_r,mut only_c)=(0,0,0,0,0);
    println!("{:26} {:14} {:14} {:8}","NODE","REACH(par,axis)","CE(par,axis)","MATCH");
    for name in &allnames{
        match (r.get(*name),c.get(*name)){
            (Some((rp,ra)),Some((cp,ca)))=>{both+=1;let am=ra==ca;let pm=rp.eq_ignore_ascii_case(cp);if am{axis_match+=1}if pm{parent_match+=1}
                if !am||!pm{println!("{:26} {:14} {:14} {}{}",name,format!("{rp},{ra}"),format!("{cp},{ca}"),if am{""}else{"AXIS≠ "},if pm{""}else{"PARENT≠"});}}
            (Some(_),None)=>{only_r+=1;}
            (None,Some(_))=>{only_c+=1;}
            _=>{}
        }
    }
    println!("\nshared {both}: axis-match {axis_match}/{both}, parent-match {parent_match}/{both}; only-in-Reach {only_r}, only-in-CE {only_c}");
}
