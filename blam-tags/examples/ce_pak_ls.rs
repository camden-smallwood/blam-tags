use blam_tags::iostore::pak::PakSet;
const PAKS:&str="/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn main()->anyhow::Result<()>{
 let pat=std::env::args().nth(1).unwrap_or_default().to_ascii_lowercase();
 let set=PakSet::open_dir(PAKS)?;
 let mut n=0; for p in set.paths(){ if p.to_ascii_lowercase().contains(&pat){println!("{p}"); n+=1; if n>20{break;}} }
 eprintln!("({n} shown across {} paks)",set.len()); Ok(())
}
