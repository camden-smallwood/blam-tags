//! Dump a mesh-sync component export's serial bytes + package name map to files.
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
    let suf=std::env::args().nth(1).unwrap().to_ascii_lowercase();
    let out=std::env::args().nth(2).unwrap();
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let b=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok())).expect("bp");
    let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).unwrap();
    let names=h.name_map.copy_raw_names();
    let hs=h.summary.header_size as usize;
    let c=h.export_map.iter().find(|e|h.name_map.get(e.object_name).contains("MeshSynchronization")).unwrap();
    let s=hs+c.cooked_serial_offset as usize; let e=s+c.cooked_serial_size as usize;
    std::fs::write(format!("{out}.bin"), &b[s..e]).unwrap();
    std::fs::write(format!("{out}.names.txt"), names.join("\n")).unwrap();
    eprintln!("wrote {} bytes, {} names", e-s, names.len());
}
