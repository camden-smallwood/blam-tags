//! Exhaustive sweep of every package carrying a BlamMeshSynchronizationComponent:
//! export composition, whether mesh refs are ALWAYS soft, which refs are hard,
//! Kismet presence, and the DA/AnimationClass linkage.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn walk(v:&PropValue, soft:&mut usize, hard:&mut usize, softpaths:&mut BTreeSet<String>, key:&str, hardkeys:&mut BTreeMap<String,usize>){
    match v {
        PropValue::SoftObject(p)=>{ *soft+=1; if !p.package.as_str().is_empty(){ softpaths.insert(p.package.as_str().to_string()); } }
        PropValue::Object(i)=>{ if *i!=0 { *hard+=1; *hardkeys.entry(key.into()).or_default()+=1; } }
        PropValue::Array(a)=>for x in a { walk(x,soft,hard,softpaths,key,hardkeys) },
        PropValue::Map(m)=>for (k,val) in m { walk(k,soft,hard,softpaths,key,hardkeys); walk(val,soft,hard,softpaths,key,hardkeys) },
        PropValue::Struct(s)=>for (kk,val) in s { walk(val,soft,hard,softpaths,kk,hardkeys) },
        _=>{}
    }
}
fn main(){
    let usmap=Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let comp=FPackageObjectIndex::create_script_import("/Script/BlamSynchronization.BlamMeshSynchronizationComponent");
    let func=FPackageObjectIndex::create_script_import("/Script/CoreUObject.Function");
    let bpgc=FPackageObjectIndex::create_script_import("/Script/Engine.BlueprintGeneratedClass");
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let (mut pkgs,mut comps,mut decode_ok,mut decode_fail)=(0usize,0usize,0usize,0usize);
    let (mut soft_total,mut hard_total)=(0usize,0usize);
    let mut hardkeys:BTreeMap<String,usize>=BTreeMap::new();
    let mut softroots:BTreeMap<String,usize>=BTreeMap::new();
    let mut props_seen:BTreeMap<String,usize>=BTreeMap::new();
    let mut with_kismet=0usize; let mut kismet_counts:Vec<usize>=Vec::new();
    let mut exportcounts:Vec<usize>=Vec::new();
    let mut regions_hist:BTreeMap<usize,usize>=BTreeMap::new();
    let mut no_runtime=0usize;
    for utoc in &u {
        let Ok(a)=IoStoreArchive::open(utoc) else{continue};
        for e in a.entries(){
            let lo=e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset"){continue}
            let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b),None,CV,HV,None) else{continue};
            if !h.export_map.iter().any(|x|x.class_index==comp){continue}
            pkgs+=1; exportcounts.push(h.export_map.len());
            let nf=h.export_map.iter().filter(|x|x.class_index==func).count();
            if nf>0 {with_kismet+=1; kismet_counts.push(nf);}
            let _=bpgc;
            let names=h.name_map.copy_raw_names();
            for ex in h.export_map.iter().filter(|x|x.class_index==comp){
                comps+=1;
                let off=h.summary.header_size as usize+ex.cooked_serial_offset as usize;
                let end=(off+ex.cooked_serial_size as usize).min(b.len());
                if off>=b.len(){continue}
                match read_export_struct(&b[off..end],&names,&usmap,"BlamMeshSynchronizationComponent"){
                    Ok(p)=>{
                        decode_ok+=1;
                        for k in p.keys(){ *props_seen.entry(k.to_string()).or_default()+=1; }
                        match p.get("RuntimeRegions"){
                            Some(PropValue::Map(m))=>{*regions_hist.entry(m.len()).or_default()+=1;}
                            _=>{no_runtime+=1;}
                        }
                        for (k,v) in &p {
                            let mut sp=BTreeSet::new();
                            walk(v,&mut soft_total,&mut hard_total,&mut sp,k,&mut hardkeys);
                            for s in sp { *softroots.entry(s.split('/').take(3).collect::<Vec<_>>().join("/")).or_default()+=1; }
                        }
                    }
                    Err(_)=>{decode_fail+=1;}
                }
            }
        }
    }
    println!("packages with a mesh-sync component : {pkgs}");
    println!("components                          : {comps} (decoded {decode_ok}, failed {decode_fail})");
    exportcounts.sort();
    println!("exports per package                 : min {} median {} max {}",
        exportcounts.first().unwrap_or(&0), exportcounts.get(exportcounts.len()/2).unwrap_or(&0), exportcounts.last().unwrap_or(&0));
    kismet_counts.sort();
    println!("packages with Kismet UFunctions     : {with_kismet}/{pkgs}  (median {} funcs)",
        kismet_counts.get(kismet_counts.len()/2).unwrap_or(&0));
    println!("\nproperties serialized on the component:");
    for (k,v) in &props_seen { println!("   {k:34} {v}"); }
    println!("\nRuntimeRegions region-count histogram: {regions_hist:?}   (components with none: {no_runtime})");
    println!("\nSOFT object refs total: {soft_total}");
    println!("HARD object refs total: {hard_total}   by property: {hardkeys:?}");
    println!("\nsoft-path roots:");
    let mut sr:Vec<_>=softroots.iter().collect(); sr.sort_by_key(|(_,n)|std::cmp::Reverse(**n));
    for (k,v) in sr.iter().take(18){ println!("   {v:>6}  {k}"); }
}
