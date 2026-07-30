//! What ORDER are dependency-bundle entries in? Test candidate rules against
//! every shipped tag: property-serialisation order, reversed schema order
//! (base-class props first), and import-index order.
use std::collections::BTreeMap;
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;
fn objs(v: &PropValue, out: &mut Vec<i32>) {
    match v {
        PropValue::Object(i) => out.push(*i),
        PropValue::Array(a) => a.iter().for_each(|x| objs(x, out)),
        PropValue::Map(m) => m.iter().for_each(|(k, v)| { objs(k, out); objs(v, out) }),
        PropValue::Struct(s) => s.values().for_each(|x| objs(x, out)),
        _ => {}
    }
}
fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let (mut n, mut alpha, mut rev, mut byidx, mut none) = (0, 0, 0, 0, 0);
    let mut n_multi = 0;
    let mut samples = Vec::new();
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") { continue }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None) else { continue };
            if h.dependency_bundle_entries.len() < 2 { continue }
            let Some(ex) = h.export_map.first() else { continue };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(ua.len());
            let mut cls = String::from("Blam");
            for p in group.split('_') { let mut c = p.chars(); if let Some(f) = c.next() { cls.push(f.to_ascii_uppercase()); cls.push_str(c.as_str()); } }
            cls.push_str("TagDataAsset");
            let props = match read_export_struct(&ua[off..end], &names, &usmap, &cls) {
                Ok(p) => p,
                Err(_) => match read_export_struct(&ua[off..end], &names, &usmap, "BlamTagDataAssetBase") { Ok(p) => p, Err(_) => continue },
            };
            n += 1;
            let actual: Vec<i32> = h.dependency_bundle_entries.iter().map(|d| d.local_import_or_export_index.index).collect();
            // 1. serialisation order = usmap flattened order (derived first)
            let flat: Vec<String> = usmap.flattened_properties(&cls).map(|v| v.iter().map(|p| p.name.clone()).collect()).unwrap_or_default();
            let mut ser = Vec::new();
            for name in &flat { if let Some(v) = props.get(name) { objs(v, &mut ser) } }
            // 2. reverse: base-class props first
            let mut revo = Vec::new();
            for name in flat.iter().rev() { if let Some(v) = props.get(name) { objs(v, &mut revo) } }
            // 3. alphabetical prop name order (BTreeMap order)
            let mut alp = Vec::new();
            for (_, v) in &props { objs(v, &mut alp) }
            // only tags with >1 object-valued property discriminate the rules
            let nprops = flat.iter().filter(|nm| props.get(*nm).map(|v| { let mut t=Vec::new(); objs(v,&mut t); !t.is_empty() }).unwrap_or(false)).count();
            if nprops < 2 { continue }
            n_multi += 1;
            let rot: Vec<i32> = if ser.is_empty() { vec![] } else { ser[1..].iter().copied().chain(std::iter::once(ser[0])).collect() };
            let mut idx = actual.clone(); idx.sort();
            if actual == ser { alpha += 1 }
            else if actual == revo { rev += 1 }
            else if actual == rot { byidx += 1 }
            else { none += 1; if samples.len() < 5 {
                samples.push(format!("{}\n   actual {:?}\n   ser    {:?}\n   rev    {:?}\n   alpha  {:?}",
                    h.package_name(), &actual[..actual.len().min(8)], &ser[..ser.len().min(8)], &revo[..revo.len().min(8)], &alp[..alp.len().min(8)])) } }
            let _ = idx;
        }
    }
    println!("{n} tags with >=2 dependency entries; {n_multi} with >=2 object-valued properties");
    println!("  == usmap serialisation order (derived-first) : {alpha}");
    println!("  == reversed (base-class props first)         : {rev}");
    println!("  == serialisation order ROTATED (first->last) : {byidx}");
    println!("  none of the above                            : {none}");
    for s in &samples { println!("\n{s}") }
    let _: BTreeMap<(), ()> = BTreeMap::new();
}
