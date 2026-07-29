//! Reproduce the Baboon CE render-JMS export for a model (class-based mesh-sync
//! resolution + Nanite static parts + skeleton_model fuse) and write the JMS,
//! to diagnose a malformed-JMS import error.
use std::sync::Arc; use std::io::{Cursor,BufWriter}; use std::collections::BTreeSet;
use blam_tags::file::TagFile; use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::MeshSyncRegions;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::static_mesh::StaticMesh;
use blam_tags::jms::{UeMeshPart, UeStaticPart};
use blam_tags::JmsFile;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
const DA_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationDataAsset";
const COMP_CLASS:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponent";
const COMP_BASE:&str="/Script/BlamSynchronization.BlamMeshSynchronizationComponentBase";
const PREFIX:usize=192*1024;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn strip(n:&str)->String{for p in ["MIP_","MI_","M_"]{if let Some(r)=n.strip_prefix(p){return r.to_string();}}n.to_string()}
fn excluded(pkg:&str,asset:&str)->bool{
    let p=norm(pkg); let a=asset.to_ascii_lowercase();
    p.contains("/skeleton/") || ["shield","shadow","animdynamics","destroyed","_dmg","damage","collision","physics","imposter"].iter().any(|k|a.contains(k))
}
fn hdrp(a:&IoStoreArchive,path:&str)->Option<FZenPackageHeader>{let b=a.read_prefix(path,PREFIX).ok()?;FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).ok()}
fn eff_mats(hdr:&FZenPackageHeader,ov:&[(String,String)])->Vec<String>{
    let mut mis:Vec<String>=hdr.imported_package_names.iter().map(|p|p.rsplit('/').next().unwrap_or(p).to_string()).filter(|b|b.starts_with("MI_")||b.starts_with("M_")).collect();
    for (slot,over) in ov{if let Some(pos)=mis.iter().position(|m|m.eq_ignore_ascii_case(slot)){mis[pos]=over.clone();}}
    mis.iter().map(|m|strip(m)).collect()
}
fn main(){
    let key=norm(&std::env::args().nth(1).unwrap_or_else(||"objects/characters/hunter/hunter".into()));
    let da_c=FPackageObjectIndex::create_script_import(DA_CLASS);
    let comp_c=FPackageObjectIndex::create_script_import(COMP_CLASS);
    let comp_base_c=FPackageObjectIndex::create_script_import(COMP_BASE);
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let read=|s:&str|{let s=s.to_ascii_lowercase();ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&s)).and_then(|e|a.read(&e.path).ok()))};
    let read_pkg=|pkg:&str|{let t=norm(pkg);let t=t.strip_prefix("/game/").unwrap_or(&t);let suf=format!("/{t}.uasset");ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok()))};
    let read_bulk=|pkg:&str|{let t=norm(pkg);let t=t.strip_prefix("/game/").unwrap_or(&t);let suf=format!("/{t}.uasset");ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read_bulk_for(e.chunk_index,0).ok()))};
    let model=TagFile::read_from_bytes(&read(&format!("{key}-model.ubulk")).unwrap()).unwrap();
    let (_,sr)=model.root().read_tag_ref_with_group("skeleton model").unwrap();
    let skel=TagFile::read_from_bytes(&read(&format!("{}-skeleton_model.ubulk",norm(&sr))).unwrap()).unwrap();
    if std::env::var("REGIONS").is_ok(){
        println!("=== .model (hlmt) variants -> regions[permutations] ===");
        if let Some(vb)=model.root().field_path("variants").and_then(|f|f.as_block()){
            for i in 0..vb.len(){ if let Some(v)=vb.element(i){
                let vn=v.read_string_id("name").unwrap_or_default(); let mut regs=vec![];
                if let Some(rb)=v.field("regions").and_then(|f|f.as_block()){for j in 0..rb.len(){if let Some(r)=rb.element(j){
                    let rn=r.read_string_id("region name").unwrap_or_default(); let mut perms=vec![];
                    if let Some(pb)=r.field("permutations").and_then(|f|f.as_block()){for k in 0..pb.len(){if let Some(p)=pb.element(k){perms.push(p.read_string_id("permutation name").unwrap_or_default());}}}
                    regs.push(format!("{rn}[{}]",perms.join(",")));
                }}}
                println!("  variant '{vn}': {}",regs.join("  "));
            }}
        }
        println!("=== .skeleton_model regions[permutations] ===");
        if let Some(rb)=skel.root().field_path("regions").and_then(|f|f.as_block()){
            for j in 0..rb.len(){if let Some(r)=rb.element(j){
                let rn=r.read_string_id("name").unwrap_or_default(); let mut perms=vec![];
                if let Some(pb)=r.field("permutations").and_then(|f|f.as_block()){for k in 0..pb.len(){if let Some(p)=pb.element(k){perms.push(p.read_string_id("name").unwrap_or_default());}}}
                println!("  region '{rn}': [{}]",perms.join(","));
            }}
        }
        return;
    }
    // needed region/perm
    let mut needed:BTreeSet<(String,String)>=BTreeSet::new();
    if let Some(vb)=model.root().field_path("variants").and_then(|f|f.as_block()){for i in 0..vb.len(){let Some(v)=vb.element(i) else{continue};if let Some(rb)=v.field("regions").and_then(|f|f.as_block()){for j in 0..rb.len(){let Some(r)=rb.element(j) else{continue};let rn=r.read_string_id("region name").unwrap_or_default();let pn=r.field("permutations").and_then(|f|f.as_block()).and_then(|pb|pb.element(0)).and_then(|p|p.read_string_id("permutation name")).unwrap_or_default();if !rn.is_empty()&&!pn.is_empty(){needed.insert((rn.to_ascii_lowercase(),pn.to_ascii_lowercase()));}}}}}
    // class-based resolution
    let modkey=format!("{key}-model"); let usmap=Usmap::meteorite().unwrap();
    let mut das=Vec::new();
    for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with(".uasset"){continue};let Some(h)=hdrp(a,&e.path) else{continue};if !h.exports_class(da_c){continue};if h.imported_package_names.iter().any(|p|norm(p).ends_with(&modkey)){das.push(norm(&e.path).rsplit('/').next().unwrap().strip_suffix(".uasset").unwrap().to_string());}}}
    let mut regions=MeshSyncRegions::default();
    for da in &das{let dal=norm(da);for a in &ar{for e in a.entries(){let n=norm(&e.path);if !n.ends_with(".uasset"){continue};let Some(h)=hdrp(a,&e.path) else{continue};if !h.imported_package_names.iter().any(|p|norm(p).ends_with(&dal)){continue};if !(h.exports_class(comp_c)||h.exports_class(comp_base_c)){continue};let Ok(b)=a.read(&e.path) else{continue};let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};let Some(c)=h.find_export_of_class(comp_c).or_else(||h.find_export_of_class(comp_base_c)) else{continue};let s=h.summary.header_size as usize+c.cooked_serial_offset as usize;let end=s+c.cooked_serial_size as usize;let Some(exp)=b.get(s..end) else{continue};let names=h.name_map.copy_raw_names();if let Ok(rr)=MeshSyncRegions::from_component_export(exp,&names,&usmap){if rr.is_world(){for sr in rr.regions{let ri=match regions.regions.iter().position(|r|r.name.eq_ignore_ascii_case(&sr.name)){Some(i)=>i,None=>{regions.regions.push(blam_tags::iostore::unversioned::Region{name:sr.name.clone(),permutations:vec![]});regions.regions.len()-1}};for sp in sr.permutations{regions.regions[ri].permutations.push(sp);}}}}}}}
    if std::env::var("MESHSYNC").is_ok(){
        println!("=== mesh-sync RuntimeRegions (region -> perm -> [skeletal] [static]) ===");
        for r in &regions.regions{
            for p in &r.permutations{
                let sk:Vec<&str>=p.skeletal_meshes.iter().map(|m|m.asset.as_str()).collect();
                let st:Vec<&str>=p.static_meshes.iter().map(|m|m.asset.as_str()).collect();
                if !sk.is_empty()||!st.is_empty(){ println!("  {}/{}: SK{:?} SM{:?}",r.name,p.name,sk,st); }
                else { println!("  {}/{}: (empty)",r.name,p.name); }
            }
        }
        return;
    }
    // collect (with per-package cache, mirroring the product path) + phase timers
    let mut sk=Vec::new(); let mut sm=Vec::new();
    let mut skc:std::collections::HashMap<String,Option<(Arc<SkeletalMesh>,Vec<String>)>>=std::collections::HashMap::new();
    let mut smc:std::collections::HashMap<String,Option<(Arc<StaticMesh>,Vec<String>)>>=std::collections::HashMap::new();
    let (mut t_read,mut t_bulk,mut t_dec)=(std::time::Duration::ZERO,std::time::Duration::ZERO,std::time::Duration::ZERO);
    let (mut n_read,mut n_bulk)=(0usize,0usize);
    let t_collect=std::time::Instant::now();
    for (region,perm) in &needed{
        for m in regions.skeletal_meshes(region,perm){
            if excluded(&m.package,&m.asset) && std::env::var("EXCL").is_ok(){continue;}
            if std::env::var("XF").is_ok() && !m.rel_transform.is_identity(){eprintln!("[XF] {}/{} {} rel rot={:?} trans={:?} scale={:?}",region,perm,m.asset,m.rel_transform.rotation,m.rel_transform.translation,m.rel_transform.scale);}
            if !skc.contains_key(&m.package){let t=std::time::Instant::now();let b=read_pkg(&m.package);t_read+=t.elapsed();n_read+=1;
                let v=b.and_then(|b|{FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).ok().map(|h|(b,h))}).and_then(|(b,h)|{let names=h.name_map.copy_raw_names();let t=std::time::Instant::now();let r=SkeletalMesh::from_package(&b,&names,h.summary.header_size as usize).ok();t_dec+=t.elapsed();r.map(|mesh|(Arc::new(mesh),eff_mats(&h,&[])))});
                skc.insert(m.package.clone(),v);}
            if let Some(Some((mesh,mats)))=skc.get(&m.package){sk.push((region.clone(),perm.clone(),m.asset.clone(),mesh.clone(),mats.clone()));}
        }
        for m in regions.static_meshes(region,perm){
            if excluded(&m.package,&m.asset) && std::env::var("EXCL").is_ok(){continue;}
            if !smc.contains_key(&m.package){let t=std::time::Instant::now();let b=read_pkg(&m.package);t_read+=t.elapsed();n_read+=1;
                let tb=std::time::Instant::now();let ub=read_bulk(&m.package);t_bulk+=tb.elapsed();n_bulk+=1;
                let v=b.and_then(|b|{FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).ok().map(|h|(b,h))}).and_then(|(b,h)|{let t=std::time::Instant::now();let r=StaticMesh::from_package_preferring_nanite(&b,h.summary.header_size as usize,ub.as_deref()).ok();t_dec+=t.elapsed();r.map(|mesh|(Arc::new(mesh),eff_mats(&h,&[])))});
                smc.insert(m.package.clone(),v);}
            if let Some(Some((mesh,mats)))=smc.get(&m.package){sm.push((region.clone(),perm.clone(),m.asset.clone(),mesh.clone(),m.parent_bone.clone(),mats.clone(),m.rel_transform));}
        }
    }
    eprintln!("[T] collect {:.2}s = read {:.2}s ({n_read}) + bulk {:.2}s ({n_bulk}) + decode {:.2}s ; {} uniq SK, {} uniq SM",t_collect.elapsed().as_secs_f32(),t_read.as_secs_f32(),t_bulk.as_secs_f32(),t_dec.as_secs_f32(),skc.len(),smc.len());
    let parts:Vec<UeMeshPart>=sk.iter().map(|(r,p,n,m,mt)|UeMeshPart{mesh:&**m,region:r.clone(),permutation:p.clone(),name:n.clone(),material_names:mt.clone()}).collect();
    let sparts:Vec<UeStaticPart>=sm.iter().map(|(r,p,n,m,b,mt,x)|UeStaticPart{mesh:&**m,bone_name:b.clone(),region:r.clone(),permutation:p.clone(),name:n.clone(),material_names:mt.clone(),rel_transform:*x,world_anchor:None}).collect();
    println!("{key}: {} sk + {} sm parts",parts.len(),sparts.len());
    if std::env::var("PARTS").is_ok(){
        println!("=== SKELETAL parts (region/perm | asset | verts | bbox centroid) ===");
        for pt in &parts{
            let m=pt.mesh; let n=m.vertices.len();
            let mut mn=[f32::MAX;3];let mut mx=[f32::MIN;3];
            for v in &m.vertices{for k in 0..3{mn[k]=mn[k].min(v.position[k]);mx[k]=mx[k].max(v.position[k]);}}
            let c=[(mn[0]+mx[0])/2.0,(mn[1]+mx[1])/2.0,(mn[2]+mx[2])/2.0];
            println!("  {:10}/{:14} {:32} v={:6} ctr[{:6.2},{:6.2},{:6.2}] ext[{:5.1},{:5.1},{:5.1}]",
                pt.region,pt.permutation,pt.name,n,c[0],c[1],c[2],mx[0]-mn[0],mx[1]-mn[1],mx[2]-mn[2]);
        }
        for pt in &sparts{
            let m=pt.mesh; let n=m.vertices.len();
            println!("  STATIC {:10}/{:14} {:28} v={:6} bone={}",pt.region,pt.permutation,pt.name,n,pt.bone_name);
        }
    }
    if std::env::var("BONES").is_ok(){
        let mut node_names=std::collections::HashSet::new();
        if let Some(nb)=skel.root().field_path("nodes").and_then(|f|f.as_block()){
            for i in 0..nb.len(){ if let Some(e)=nb.element(i){ if let Some(n)=e.read_string_id("name").or_else(||e.read_string("name")){ node_names.insert(n.to_ascii_lowercase()); } } }
        }
        println!("=== BONE-MAPPING audit (skeleton_model has {} nodes) ===",node_names.len());
        for (pkg,v) in &skc{
            if let Some((mesh,_))=v{
                let total=mesh.bones.len();
                let matched=mesh.bones.iter().filter(|b|node_names.contains(&b.name.to_ascii_lowercase())).count();
                let mut used=std::collections::HashSet::new();
                for vt in &mesh.vertices{ for inf in &vt.influences{ used.insert(inf.bone as usize); } }
                let used_unmatched:Vec<String>=used.iter().filter_map(|&bi|mesh.bones.get(bi)).filter(|b|!node_names.contains(&b.name.to_ascii_lowercase())).map(|b|b.name.clone()).collect();
                let flag=if used_unmatched.is_empty(){"OK"}else{"!! WEIGHTED BONES MISSING FROM SKELETON — bind to node 0"};
                let r2=|n:&str|mesh.bones.iter().find(|b|b.name.eq_ignore_ascii_case(n)).map(|b|{let r=b.rest_rotation;let t=b.rest_translation;(format!("{}",b.name),[(r[0]*100.0).round()/100.0,(r[1]*100.0).round()/100.0,(r[2]*100.0).round()/100.0,(r[3]*100.0).round()/100.0],[(t[0]).round(),(t[1]).round(),(t[2]).round()])});
                println!("  {:44} b{}/{} w{} {} | bone0={} {:?} root_m={:?}",pkg.rsplit('/').next().unwrap_or(pkg),matched,total,used.len(),flag,mesh.bones.first().map(|b|b.name.as_str()).unwrap_or("?"),mesh.bones.first().map(|b|[(b.rest_rotation[0]*100.0).round()/100.0,(b.rest_rotation[1]*100.0).round()/100.0,(b.rest_rotation[2]*100.0).round()/100.0,(b.rest_rotation[3]*100.0).round()/100.0]),r2("root_m").or_else(||r2("pedestal")));
            }
        }
    }
    let t_rm=std::time::Instant::now();
    let (_rm,rmeshes)=blam_tags::render_model::RenderModel::from_ue_meshes(&parts,&sparts,&[],&skel).expect("rm");
    let rmtris:usize=rmeshes.iter().map(|m|m.vertices.len()).sum();
    eprintln!("[T] RenderModel bake {:.2}s ({} meshes, {rmtris} verts)",t_rm.elapsed().as_secs_f32(),rmeshes.len());
    let t_jms=std::time::Instant::now();
    let jms=JmsFile::from_ue_meshes(&parts,&sparts,&[],&skel).expect("jms");
    eprintln!("[T] jms bake {:.2}s",t_jms.elapsed().as_secs_f32());
    println!("jms: {} nodes, {} materials, {} markers, {} verts, {} tris",jms.nodes.len(),jms.materials.len(),jms.markers.len(),jms.vertices.len(),jms.triangles.len());
    // check for empty/space material names
    for (i,m) in jms.materials.iter().enumerate(){if m.name.trim().is_empty()||m.name.contains('\n'){println!("  SUSPECT material[{i}] name={:?} mat={:?}",m.name,m.material_name);}}
    let ver:u16=std::env::args().nth(2).and_then(|s|s.parse().ok()).unwrap_or(8213);
    let out=format!("/private/tmp/claude-501/-Users-camden-Source-Baboon-local/4803b682-de10-4887-907a-9f81ad3d13d0/scratchpad/{}.jms",key.replace('/',"_"));
    let t_w=std::time::Instant::now();
    let mut w=BufWriter::new(std::fs::File::create(&out).unwrap());
    jms.write(&mut w,ver).unwrap();
    drop(w);
    eprintln!("[T] jms WRITE {:.2}s ({} verts, {} tris)",t_w.elapsed().as_secs_f32(),jms.vertices.len(),jms.triangles.len());
    println!("wrote {out} (v{ver})");
    // Validate the modern (8211+) grammar the importer parses: strip comment/blank
    // lines, then read tokens per section and confirm every int field is an int.
    if ver>=8211{
        let text=std::fs::read_to_string(&out).unwrap();
        let toks:Vec<String>=text.lines().map(|l|l.trim().to_string()).filter(|l|!l.is_empty()&&!l.starts_with(';')).collect();
        let mut p=0usize;
        macro_rules! int{($what:expr)=>{{let t=toks.get(p).unwrap_or_else(||panic!("EOF expecting int {}",$what));p+=1;t.parse::<i64>().unwrap_or_else(|_|panic!("NON-INT where {} expected at token {}: {:?}",$what,p-1,t))}}}
        macro_rules! skip{($n:expr)=>{{p+=$n;}}}
        let _v=int!("version");
        let nn=int!("node_count"); for _ in 0..nn{skip!(1); let _=int!("node parent"); skip!(2);}
        let nm=int!("material_count"); for _ in 0..nm{skip!(2);}
        let nmk=int!("marker_count"); for _ in 0..nmk{skip!(1); let _=int!("marker node"); skip!(3);}
        let nx=int!("xref_count"); for _ in 0..nx{skip!(2);}
        let ni=int!("instance_marker_count"); for _ in 0..ni{skip!(4);}
        let nv=int!("vertex_count");
        for vi in 0..nv{
            skip!(2);
            let ic=int!("influence_count"); if !(0..=8).contains(&ic){panic!("vertex {vi} bad influence_count {ic}");}
            for _ in 0..ic{let _=int!("influence idx"); skip!(1);}
            let uc=int!("uv_count"); skip!(uc as usize);
            skip!(1);
        }
        let nt=int!("triangle_count");
        for _ in 0..nt{let _=int!("tri material"); skip!(1);}
        println!("VALIDATED modern grammar: {nn} nodes, {nm} mats, {nmk} markers, {nv} verts, {nt} tris — {p}/{} tokens, all int fields OK",toks.len());
    }
}
