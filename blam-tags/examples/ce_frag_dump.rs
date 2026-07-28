//! Decode one tag export's FUnversionedHeader fragment-by-fragment against the
//! flattened schema, showing exactly which schema slots are zero-masked.
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::read_export_struct;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn cls(g:&str)->String{let mut o=String::from("Blam");for p in g.split('_'){let mut c=p.chars();if let Some(f)=c.next(){o.push(f.to_ascii_uppercase());o.push_str(c.as_str());}}o+"TagDataAsset"}
fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    for want in std::env::args().skip(1) {
        let want=want.to_ascii_lowercase();
        let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
        u.sort();
        for utoc in &u {
            let Ok(a)=IoStoreArchive::open(utoc) else { continue };
            let Some(rel)=a.entries().iter().find(|e| e.path.to_ascii_lowercase().replace('\\',"/").ends_with(&want)).map(|e|e.path.clone()) else { continue };
            let ua=a.read(&rel).unwrap();
            let h=FZenPackageHeader::deserialize(&mut Cursor::new(&ua),None,CV,HV,None).unwrap();
            let ex=h.export_map.first().unwrap();
            let off=h.summary.header_size as usize+ex.cooked_serial_offset as usize;
            let end=(off+ex.cooked_serial_size as usize).min(ua.len());
            let body=&ua[off..end];
            let stem=rel.to_ascii_lowercase(); let stem=stem.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let g=stem.rsplit_once('-').unwrap().1; let c=cls(g);
            println!("=== {rel}\n    class {c}, body {} bytes", body.len());
            print!("    raw: "); for b in body.iter().take(48) { print!("{b:02x} ") } println!();
            if let Some(flat)=usmap.flattened_properties(&c) {
                println!("    flattened schema ({}):", flat.len());
                for (i,p) in flat.iter().enumerate() { println!("       [{i}] {} : {:?}", p.name, p.ty); }
            }
            // fragments
            let mut o=0usize; let mut n=0; let mut zeros=0usize;
            println!("    fragments:");
            loop {
                if o+2>body.len(){break}
                let f=u16::from_le_bytes([body[o],body[o+1]]); o+=2; n+=1;
                let skip=(f & 0x7f) as usize; let has_z=(f&0x0200)!=0; let last=(f&0x0100)!=0; let vnum=(f>>10) as usize;
                println!("       #{n} raw={f:#06x} skip={skip} values={vnum} has_zeroes={has_z} last={last}");
                if has_z { zeros+=vnum }
                if last||n>32 {break}
            }
            println!("    zero-mask bits: {zeros}");
            match read_export_struct(body,&h.name_map.copy_raw_names(),&usmap,&c) {
                Ok(p)=>{ println!("    decoded ({} props):",p.len()); for (k,v) in p { let s=format!("{v:?}"); println!("       {k} = {}", &s[..s.len().min(90)]); } }
                Err(e)=>println!("    decode failed: {e}"),
            }
            break;
        }
    }
}
