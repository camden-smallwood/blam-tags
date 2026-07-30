//! Decode a BP's BlamMeshSynchronizationComponent template: are the mesh
//! references soft paths (rewritable strings) or import-map entries?
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn brief(v:&PropValue, d:usize)->String{
    let pad="  ".repeat(d);
    match v {
        PropValue::Map(m)=>{ let mut s=format!("Map[{}]\n",m.len());
            for (k,val) in m.iter().take(6){ s+=&format!("{pad}  {} => {}\n", brief(k,0), brief(val,d+1)); } s }
        PropValue::Array(a)=>{ let mut s=format!("Array[{}]\n",a.len());
            for x in a.iter().take(4){ s+=&format!("{pad}  - {}\n", brief(x,d+1)); } s }
        PropValue::Struct(st)=>{ let mut s=String::from("{\n");
            for (k,val) in st.iter(){ s+=&format!("{pad}    {k}: {}\n", brief(val,d+2)); } s+&format!("{pad}  }}") }
        PropValue::SoftObject(p)=>format!("SOFT('{}')", p.package),
        PropValue::Name(n)=>format!("'{n}'"),
        PropValue::Object(i)=>format!("HARD(Object({i}))"),
        other=>{ let s=format!("{other:?}"); s.chars().take(70).collect() }
    }
}
fn main(){
    let usmap=Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let want=std::env::args().nth(1).unwrap_or_else(||"bp_brutebipedactor.uasset".into()).to_ascii_lowercase();
    let comp_cls=FPackageObjectIndex::create_script_import("/Script/BlamSynchronization.BlamMeshSynchronizationComponent");
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    for utoc in &u {
        let Ok(a)=IoStoreArchive::open(utoc) else{continue};
        let Some(rel)=a.entries().iter().find(|e|e.path.to_ascii_lowercase().replace('\\',"/").ends_with(&want)).map(|e|e.path.clone()) else{continue};
        let b=a.read(&rel).unwrap();
        let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b),None,CV,HV,None).unwrap();
        let names=h.name_map.copy_raw_names();
        println!("=== {rel}\n{} exports, {} imported packages", h.export_map.len(), h.imported_package_names.len());
        for (i,ex) in h.export_map.iter().enumerate(){
            if ex.class_index!=comp_cls {continue}
            let off=h.summary.header_size as usize+ex.cooked_serial_offset as usize;
            let end=(off+ex.cooked_serial_size as usize).min(b.len());
            println!("\n-- export[{i}] {} ({} bytes) as BlamMeshSynchronizationComponent",
                h.name_map.get(ex.object_name), ex.cooked_serial_size);
            match read_export_struct(&b[off..end],&names,&usmap,"BlamMeshSynchronizationComponent"){
                Ok(p)=>for (k,v) in p { println!("   {k} = {}", brief(&v,1)); },
                Err(e)=>println!("   decode failed: {e}"),
            }
        }
        println!("\n-- imported packages --");
        for n in &h.imported_package_names { println!("   {n}"); }
        return;
    }
    eprintln!("not found");
}
