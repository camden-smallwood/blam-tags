use std::io::Cursor;use std::sync::Arc;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
    let suf=std::env::args().nth(1).unwrap_or_else(||"bp_basemarinebipedactor.uasset".into()).to_ascii_lowercase();
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let b=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok())).expect("bp");
    let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).unwrap();
    println!("total {} bytes, header_size {}, {} exports", b.len(), h.summary.header_size, h.export_map.len());
    for (i,ex) in h.export_map.iter().enumerate(){
        println!("  [{i}] {:<45} serial_size={} offset={}", h.name_map.get(ex.object_name), ex.cooked_serial_size, ex.cooked_serial_offset);
    }
}
