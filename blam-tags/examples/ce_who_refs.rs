use std::io::Cursor; use std::sync::Arc;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let target=std::env::args().nth(1).unwrap_or_else(||"marine_torso_01".into()).to_ascii_lowercase();
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect(); u.sort();
    let mut hits=0; let mut scanned=0;
    for path in &u { let Ok(a)=IoStoreArchive::open(path) else{continue}; let a=Arc::new(a);
      for e in a.entries(){ let n=norm(&e.path);
        if !n.ends_with(".uasset"){continue;}
        let base=n.rsplit('/').next().unwrap_or("");
        // Only parse plausible mapping holders (data assets / registries / tables / blueprints).
        if !(base.starts_with("da_")||base.starts_with("dt_")||base.contains("registry")||base.contains("customiz")||base.contains("loadout")||base.contains("variant")||base.contains("meshsync")||base.starts_with("bp_")||base.starts_with("abp_")){continue;}
        let Ok(b)=a.read(&e.path) else{continue}; scanned+=1;
        let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None) else{continue};
        let imp=h.imported_package_names.iter().filter(|p|norm(p).contains(&target)).count();
        let nm=h.name_map.copy_raw_names().iter().filter(|s|s.to_ascii_lowercase().contains(&target)).count();
        if imp>0||nm>0 { println!("{base}  import={imp} namemap={nm}  @ {n}"); hits+=1; }
    }}
    println!("scanned {scanned} data-asset-ish files; {hits} reference '{target}'");
}
