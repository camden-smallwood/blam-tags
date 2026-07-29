//! Generic per-class asset anatomy: for each named export class, sweep every
//! package that exports it and report the authoring-relevant shape — export
//! count, bulk entries, property surface with types, and which refs are hard
//! imports vs soft paths.
//!
//! Run: ce_asset_anatomy <Class[,Class...]> [limit-per-class]
use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct_len, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
#[derive(Default)]
struct Stat{
    n:usize, decoded:usize, failed:usize,
    exports:Vec<usize>, bulk:BTreeMap<usize,usize>, bulkflags:BTreeMap<u32,usize>,
    props:BTreeMap<String,(usize,BTreeSet<&'static str>)>,
    hard:BTreeMap<String,usize>, soft:BTreeMap<String,usize>,
    serial:Vec<usize>, names:Vec<usize>, imports:Vec<usize>,
    objflags:BTreeMap<u32,usize>, pkgflags:BTreeMap<u32,usize>,
    /// (package, error) for every export that failed to decode — a bare failure
    /// count says coverage is short without saying what to go look at.
    errors:Vec<(String,String)>,
    /// How the property walk landed: exactly at the end, at the end bar zero
    /// padding, or short of it (a native tail — or an undetected desync).
    exact:usize, padded:usize, native_tail:usize, tail_bytes:Vec<usize>,
}
fn kindof(v:&PropValue)->&'static str{ match v{
    PropValue::Bool(_)=>"bool",PropValue::Int(_)=>"int",PropValue::Float(_)=>"float",
    PropValue::Name(_)=>"name",PropValue::Str(_)=>"str",PropValue::Object(_)=>"obj",
    PropValue::SoftObject(_)=>"soft",PropValue::Array(_)=>"arr",PropValue::Map(_)=>"map",
    PropValue::Struct(_)=>"struct",PropValue::Native(_)=>"native",PropValue::Delegate{..}=>"delegate",PropValue::MulticastDelegate(_)=>"multicast",PropValue::FieldPath{..}=>"fieldpath",PropValue::Unset=>"unset",PropValue::Raw(_)=>"raw",PropValue::WithRemovals{inner,..}=>kindof(inner)} }
fn refs(v:&PropValue, key:&str, hard:&mut BTreeMap<String,usize>, soft:&mut BTreeMap<String,usize>){
    match v{
        PropValue::Object(i)=>{ if *i!=0 { *hard.entry(key.into()).or_default()+=1; } }
        PropValue::SoftObject(_)=>{ *soft.entry(key.into()).or_default()+=1; }
        PropValue::Array(a)=>for x in a{refs(x,key,hard,soft)},
        PropValue::Map(m)=>for (k,val) in m{refs(k,key,hard,soft);refs(val,key,hard,soft)},
        PropValue::Struct(s)=>for (kk,val) in s{refs(val,&format!("{key}.{kk}"),hard,soft)},
        PropValue::WithRemovals{removals,inner}=>{ if let Some(r)=removals { for x in r{refs(x,key,hard,soft)} } refs(inner,key,hard,soft) }
        _=>{}
    }
}
fn main(){
    let usmap=Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let arg=std::env::args().nth(1).unwrap_or_else(||"Texture2D".into());
    let limit:usize=std::env::args().nth(2).and_then(|s|s.parse().ok()).unwrap_or(usize::MAX);
    let classes:Vec<String>=arg.split(',').map(|s|s.trim().to_string()).collect();
    let want:Vec<(String,FPackageObjectIndex)>=classes.iter().map(|c|{
        let p=if c.contains('.'){c.clone()}else{format!("/Script/Engine.{c}")};
        (c.clone(),FPackageObjectIndex::create_script_import(&p))
    }).collect();
    let mut u:Vec<_>=std::fs::read_dir(PAKS).unwrap().filter_map(|e|e.ok().map(|e|e.path()))
        .filter(|p|p.extension().is_some_and(|x|x.eq_ignore_ascii_case("utoc")))
        .filter(|p|!p.file_name().is_some_and(|n|n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut stats:BTreeMap<String,Stat>=BTreeMap::new();
    for utoc in &u {
        let Ok(a)=IoStoreArchive::open(utoc) else{continue};
        for e in a.entries(){
            let lo=e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset")&&!lo.ends_with(".umap"){continue}
            let Ok(b)=a.read(&e.path) else{continue};
            let Ok(h)=FZenPackageHeader::deserialize(&mut Cursor::new(&b),None,CV,HV,None) else{continue};
            for (cname,cidx) in &want{
                let Some(ex)=h.export_map.iter().find(|x|x.class_index==*cidx) else{continue};
                let st=stats.entry(cname.clone()).or_default();
                if st.n>=limit {continue}
                st.n+=1;
                st.exports.push(h.export_map.len());
                *st.bulk.entry(h.bulk_data.len()).or_default()+=1;
                for bd in &h.bulk_data{*st.bulkflags.entry(bd.flags).or_default()+=1;}
                st.names.push(h.name_map.copy_raw_names().len());
                st.imports.push(h.import_map.len());
                st.serial.push(ex.cooked_serial_size as usize);
                *st.objflags.entry(ex.object_flags).or_default()+=1;
                *st.pkgflags.entry(h.summary.package_flags).or_default()+=1;
                let names=h.name_map.copy_raw_names();
                let off=h.summary.header_size as usize+ex.cooked_serial_offset as usize;
                let end=(off+ex.cooked_serial_size as usize).min(b.len());
                if off>=b.len(){continue}
                match read_export_struct_len(&b[off..end],&names,&usmap,cname.rsplit('.').next().unwrap()){
                    Ok((p,used))=>{ st.decoded+=1;
                        // Where the property walk stopped relative to the export:
                        // exact, zero padding, or a native tail (or a desync).
                        let tail=&b[off..end][used..];
                        if tail.is_empty(){st.exact+=1}
                        else if tail.iter().all(|&x|x==0){st.padded+=1}
                        else {st.native_tail+=1; st.tail_bytes.push(tail.len());}
                        for (k,v) in &p{
                            let e2=st.props.entry(k.to_string()).or_insert((0,BTreeSet::new()));
                            e2.0+=1; e2.1.insert(kindof(v));
                            refs(v,k,&mut st.hard,&mut st.soft);
                        } }
                    Err(e)=>{st.failed+=1; st.errors.push((h.package_name(),format!("{e:#}")));}
                }
            }
        }
    }
    for (c,s) in &stats{
        let mut ex=s.exports.clone(); ex.sort();
        let mut se=s.serial.clone(); se.sort();
        println!("\n{}", "=".repeat(70));
        println!("{c}: {} packages ({} decoded, {} failed)", s.n, s.decoded, s.failed);
        println!("  exports/pkg   min {} med {} max {}", ex.first().unwrap_or(&0), ex.get(ex.len()/2).unwrap_or(&0), ex.last().unwrap_or(&0));
        println!("  export bytes  min {} med {} max {}", se.first().unwrap_or(&0), se.get(se.len()/2).unwrap_or(&0), se.last().unwrap_or(&0));
        println!("  bulk entries  {:?}   flags {:?}", s.bulk, s.bulkflags);
        println!("  obj_flags {:?}  pkg_flags {:?}",
            s.objflags.iter().map(|(k,v)|(format!("{k:x}"),v)).collect::<Vec<_>>(),
            s.pkgflags.iter().map(|(k,v)|(format!("{k:x}"),v)).collect::<Vec<_>>());
        let mut tb=s.tail_bytes.clone(); tb.sort();
        println!("  walk ended  exact {} | zero-padded {} | short by non-zero tail {} (med {} bytes)",
            s.exact, s.padded, s.native_tail, tb.get(tb.len()/2).unwrap_or(&0));
        println!("  properties ({}):", s.props.len());
        let mut pv:Vec<_>=s.props.iter().collect(); pv.sort_by_key(|(_,(n,_))|std::cmp::Reverse(*n));
        for (k,(n,kinds)) in pv.iter().take(40){ println!("     {n:>7}  {k:38} {:?}", kinds); }
        println!("  HARD refs: {:?}", s.hard);
        println!("  SOFT refs: {:?}", s.soft);
        if !s.errors.is_empty(){
            println!("  FAILURES ({}):", s.errors.len());
            for (pkg,err) in s.errors.iter().take(20){ println!("     {pkg}\n        {err}"); }
        }
    }
}
