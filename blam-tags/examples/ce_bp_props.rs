//! Decode a cooked package's exports via usmap and print any property whose
//! value mentions a given substring — used to find which *slot* on a Blueprint
//! holds an asset, rather than inferring it from the import table.
use std::io::Cursor; use std::sync::Arc;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::unversioned::read_export_struct;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::IoStoreArchive;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV:EIoStoreTocVersion=EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV:EIoContainerHeaderVersion=EIoContainerHeaderVersion::SoftPackageReferences;
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
 let suf=norm(&std::env::args().nth(1).unwrap());
 let cls=std::env::args().nth(2);
 let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
 let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
 let usmap=Usmap::meteorite().unwrap();
 if suf=="x" && let Some(c)=&cls{
   match usmap.flattened_properties(c){
     Some(ps)=>{println!("usmap class '{c}' has {} properties:",ps.len()); for p in ps{println!("   {} : {:?}",p.name,p.ty);} }
     None=>{println!("usmap has no class '{c}'; near matches:");
       let l=c.to_ascii_lowercase();
       for n in usmap.structs.iter().map(|s|&s.name){ if n.to_ascii_lowercase().contains(&l){println!("   {n}");} } }
   }
   return;
 }
 let b=ar.iter().find_map(|a|a.entries().iter().find(|e|norm(&e.path).contains(&suf)&&norm(&e.path).ends_with(".uasset")).and_then(|e|a.read(&e.path).ok())).expect("pkg");
 let h=FZenPackageHeader::deserialize(&mut Cursor::new(&b[..]),None,CV,HV,None).unwrap();
 let names=h.name_map.copy_raw_names(); let hs=h.summary.header_size as usize;
 // Legend: Object(-N) in decoded properties is import slot N-1.
 for (i,idx) in h.import_map.iter().enumerate(){
   if let Some(r)=idx.package_import()
     && let Some(pkg)=h.imported_package_names.get(r.imported_package_index as usize){
     println!("import Object({}) = {pkg}",-(i as i32)-1);
   }
 }
 for ex in &h.export_map{
   let name=h.name_map.get(ex.object_name);
   // Class name comes from the import table when the class is a native import.
   let s=hs+ex.cooked_serial_offset as usize; let e=s+ex.cooked_serial_size as usize;
   let Some(payload)=b.get(s..e) else {continue};
   // Try the export's own name minus the _GEN_VARIABLE suffix as the class.
   // Export names carry instance suffixes (`_GEN_VARIABLE`, `_0`); strip them
   // to recover the native class the usmap knows.
   let base=name.trim_end_matches("_GEN_VARIABLE");
   let trimmed=base.rsplit_once('_').map(|(h,t)|if t.chars().all(|c|c.is_ascii_digit()){h}else{base}).unwrap_or(base);
   // An explicit class overrides the name-based guess: cooked tag data assets
   // are named after the tag, not their class.
   let forced=cls.as_deref();
   let Some(guess)=forced.into_iter().chain([base,trimmed]).find(|c|usmap.get(c).is_some()) else {continue};
   match read_export_struct(payload,&names,&usmap,guess){
     Ok(props)=>{ println!("\n=== export {name} (class {guess}) — {} props",props.len());
       for (k,v) in &props{ println!("   {k} = {v:?}"); } }
     Err(err)=>println!("\n=== export {name} (class {guess}) decode failed: {err}"),
   }
 }
}
