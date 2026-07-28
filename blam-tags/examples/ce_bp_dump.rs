use std::io::{Cursor, Write};use std::sync::Arc;
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
    let out=std::env::args().nth(2).unwrap_or_else(||"/private/tmp/claude-501/-Users-camden-Source-Baboon-local/4803b682-de10-4887-907a-9f81ad3d13d0/scratchpad/bp".into());
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
    let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
    let b=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).ends_with(&suf)).and_then(|e|a.read(&e.path).ok())).expect("bp");
    let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).unwrap();
    let hs=h.summary.header_size as usize;
    let ex=&h.export_map[0];
    let start=hs+ex.cooked_serial_offset as usize; let end=start+ex.cooked_serial_size as usize;
    std::fs::File::create(format!("{out}.export0.bin")).unwrap().write_all(&b[start..end]).unwrap();
    let mut nf=std::fs::File::create(format!("{out}.names.txt")).unwrap();
    for n in h.name_map.copy_raw_names(){ writeln!(nf,"{n}").unwrap(); }
    println!("export0: {} bytes -> {out}.export0.bin ; {} names -> {out}.names.txt", end-start, h.name_map.copy_raw_names().len());
    println!("export0 class object_name={}", h.name_map.get(ex.object_name));
}
