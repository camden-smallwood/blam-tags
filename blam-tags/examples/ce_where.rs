//! Which container does a cooked path live in? Provenance for citing findings.
use std::sync::Arc;
use blam_tags::iostore::IoStoreArchive;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
 let pat=norm(&std::env::args().nth(1).unwrap());
 let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
 for path in &u{ let Ok(a)=IoStoreArchive::open(path) else{continue}; let a=Arc::new(a);
   for e in a.entries(){ let n=norm(&e.path); if n.contains(&pat){
     println!("{}\n   {}  (chunk {})",path.file_name().unwrap().to_string_lossy(),e.path,e.chunk_index);
   }}}
}
