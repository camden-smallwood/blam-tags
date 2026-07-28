//! What does a Kismet-FREE mesh-sync Blueprint contain? These are candidates
//! for synthesis rather than cloning. Histogram their export classes.
use std::collections::BTreeMap;
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main(){
    let mut by_hash:BTreeMap<u64,String>=BTreeMap::new();
    for m in std::fs::read_dir(UHT).unwrap().filter_map(|e|e.ok()){
        if !m.path().is_dir(){continue}
        let module=m.file_name().to_string_lossy().to_string();
        for sub in ["Public","Private","Classes"]{
            let Ok(rd)=std::fs::read_dir(format!("{UHT}/{module}/{sub}")) else{continue};
            for f in rd.filter_map(|e|e.ok()){
                let n=f.file_name().to_string_lossy().to_string();
                if let Some(s)=n.strip_suffix(".h"){
                    let p=format!("/Script/{module}.{s}");
                    by_hash.entry(FPackageObjectIndex::create_script_import(&p).raw_index()).or_insert(p);
                }
            }
        }
    }
    let comp=FPackageObjectIndex::create_script_import("/Script/BlamSynchronization.BlamMeshSynchronizationComponent");
    let func=FPackageObjectIndex::create_script_import("/Script/CoreUObject.Function");
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut cls:BTreeMap<String,usize>=BTreeMap::new();
    let mut n=0usize; let mut smallest:Vec<(usize,String,usize)>=Vec::new();
    for utoc in &u {
        let Ok(a)=IoStoreArchive::open(utoc) else{continue};
        for e in a.entries(){
            if !e.path.to_ascii_lowercase().ends_with(".uasset"){continue}
            let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b),None,CV,HV,None) else{continue};
            if !h.export_map.iter().any(|x|x.class_index==comp){continue}
            if h.export_map.iter().any(|x|x.class_index==func){continue}   // Kismet-free only
            n+=1;
            smallest.push((h.export_map.len(), h.package_name(), b.len()));
            for ex in &h.export_map{
                let k=by_hash.get(&ex.class_index.raw_index()).cloned()
                    .unwrap_or_else(||if ex.class_index.package_import().is_some(){"<BP-generated class>".into()}else{format!("<unknown {:016X}>",ex.class_index.raw_index())});
                *cls.entry(k).or_default()+=1;
            }
        }
    }
    println!("Kismet-free mesh-sync Blueprints: {n}");
    println!("\nexport classes across them:");
    let mut v:Vec<_>=cls.iter().collect(); v.sort_by_key(|(_,c)|std::cmp::Reverse(**c));
    for (k,c) in v.iter().take(30){ println!("   {c:>5}  {k}"); }
    smallest.sort();
    println!("\nsmallest by export count:");
    for (e,p,sz) in smallest.iter().take(10){ println!("   {e:>3} exports {sz:>7} bytes  {p}"); }
}
