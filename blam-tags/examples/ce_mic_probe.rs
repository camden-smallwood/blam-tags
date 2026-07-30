//! Why do MaterialInstanceConstant / Material exports fail to decode?
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::read_export_struct;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn main(){
    let usmap=Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    // Accept either a bare Engine class or a full script path
    // (`/Script/Niagara.NiagaraSystem`); the usmap is keyed by the short name.
    let arg=std::env::args().nth(1).unwrap_or_else(||"MaterialInstanceConstant".into());
    // `?substring` lists matching usmap struct names instead of probing exports.
    if let Some(q)=arg.strip_prefix('?'){
        let ql=q.to_ascii_lowercase();
        let mut hits:Vec<_>=usmap.structs.iter().map(|s|s.name.as_str()).filter(|n|n.to_ascii_lowercase().contains(&ql)).collect();
        hits.sort();
        for n in hits{ println!("{n}"); }
        return;
    }
    let path=if arg.contains('.'){arg.clone()}else{format!("/Script/Engine.{arg}")};
    let cls=arg.rsplit('.').next().unwrap().to_string();
    let idx=FPackageObjectIndex::create_script_import(&path);
    // Optional package-name substring, to go straight at a known failure.
    let filter=std::env::args().nth(2);
    if let Some(s)=usmap.get(&cls){
        println!("usmap {cls}: super={:?}, {} own props", s.super_name, s.properties.len());
        for p in &s.properties{ println!("   {} : {:?}", p.name, p.ty); }
        if let Some(f)=usmap.flattened_properties(&cls){
            println!("   flattened: {} props", f.len());
            for (i,p) in f.iter().enumerate().take(30){ println!("     [{i}] {} : {:?}", p.name, p.ty); }
        }
    } else { println!("{cls} NOT in usmap"); }
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut shown=0;
    for utoc in &u {
        let Ok(a)=IoStoreArchive::open(utoc) else{continue};
        for e in a.entries(){
            let lo=e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset")&&!lo.ends_with(".umap"){continue}
            let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b),None,CV,HV,None) else{continue};
            if let Some(f)=&filter{ if !h.package_name().to_ascii_lowercase().contains(&f.to_ascii_lowercase()){continue} }
            let Some(ex)=h.export_map.iter().find(|x|x.class_index==idx) else{continue};
            let names=h.name_map.copy_raw_names();
            let off=h.summary.header_size as usize+ex.cooked_serial_offset as usize;
            let end=(off+ex.cooked_serial_size as usize).min(b.len());
            if off>=b.len(){continue}
            let body=&b[off..end];
            println!("\n=== {} ({} bytes)", h.package_name(), body.len());
            for (i,bd) in h.bulk_data.iter().enumerate().take(6){
                println!("  bulk[{i}] off {} size {} flags {:x}", bd.serial_offset, bd.serial_size, bd.flags);
            }
            // Whole export body to disk, so any offset the trace names can be
            // inspected directly instead of only the first few lines.
            if let Ok(p)=std::env::var("BLAM_DUMP_FILE"){ std::fs::write(&p,body).unwrap(); println!("  (body written to {p})"); }
            for (i,chunk) in body.chunks(16).take(24).enumerate(){
                print!("  {:04x}: ", i*16);
                for x in chunk{print!("{x:02x} ")}
                println!();
            }
            match read_export_struct(body,&names,&usmap,&cls){
                Ok(p)=>{println!("  decoded {} props: {:?}", p.len(), p.keys().collect::<Vec<_>>());}
                Err(err)=>println!("  ERROR: {err}"),
            }
            shown+=1;
            // When dumping, stop at the first match so the bytes on disk are the
            // same export the trace above describes.
            if shown>=3 || std::env::var("BLAM_DUMP_FILE").is_ok() {return}
        }
    }
}
