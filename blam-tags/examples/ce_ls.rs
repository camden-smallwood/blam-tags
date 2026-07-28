use std::sync::Arc;
use blam_tags::iostore::IoStoreArchive;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn norm(p:&str)->String{p.to_ascii_lowercase().replace('\\',"/")}
fn main(){
 let pat=norm(&std::env::args().nth(1).unwrap_or_default());
 let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path())).filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc"))).filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();u.sort();
 let ar:Vec<Arc<IoStoreArchive>>=u.iter().filter_map(|u|IoStoreArchive::open(u).ok().map(Arc::new)).collect();
 let mut s=std::collections::BTreeSet::new();
 for a in &ar{for e in a.entries(){let n=norm(&e.path);if n.contains(&pat){s.insert(n);}}}
 for p in &s{println!("{p}");}
 eprintln!("({} matches)",s.len());
}
