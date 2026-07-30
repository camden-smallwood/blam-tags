//! Semantically decode a CE tag's `.uasset`: what class the export is, what
//! properties it actually serializes, what it imports, and the raw body.
use std::io::Cursor;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::unversioned::read_export_struct;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let suffix = std::env::args().nth(1)
        .unwrap_or_else(|| "pelican-skeleton_model.uasset".into()).to_ascii_lowercase();
    let class = std::env::args().nth(2)
        .unwrap_or_else(|| "BlamSkeletonModelTagDataAsset".into());
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("parse usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let Some(rel) = a.entries().iter()
            .find(|e| e.path.to_ascii_lowercase().replace('\\',"/").ends_with(&suffix))
            .map(|e| e.path.clone()) else { continue };
        let ua = a.read(&rel).unwrap();
        let h = FZenPackageHeader::deserialize(&mut Cursor::new(&ua[..]), None, CV, HV, None).unwrap();
        let names = h.name_map.copy_raw_names();
        println!("{rel}\n");

        println!("import map ({} entries) — FPackageObjectIndex:", h.import_map.len());
        for (i, im) in h.import_map.iter().enumerate() {
            println!("    [{i}] {im:?}");
        }
        println!();
        for (i, e) in h.export_map.iter().enumerate() {
            println!("export[{i}]:");
            println!("    object_name        = {:?}", h.name_map.get(e.object_name).to_string());
            println!("    class_index        = {:?}", e.class_index);
            println!("    outer_index        = {:?}", e.outer_index);
            println!("    super_index        = {:?}", e.super_index);
            println!("    template_index     = {:?}", e.template_index);
            println!("    object_flags       = 0x{:x}", e.object_flags);
            println!("    public_export_hash = 0x{:x}", e.public_export_hash);
            println!("    cooked_serial_size = {}", e.cooked_serial_size);
        }
        println!();

        let body = &ua[h.summary.header_size as usize..];
        println!("export body ({} bytes): {}", body.len(),
            body.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));

        let serial = e_body(&h, &ua);
        println!("\ndecoding {} bytes as class {class}:", serial.len());
        match read_export_struct(serial, &names, &usmap, &class) {
            Ok(props) if props.is_empty() => println!("    (no properties serialized — all at class defaults)"),
            Ok(props) => for (k, v) in props { println!("    {k} = {v:?}"); },
            Err(err) => println!("    decode failed: {err}"),
        }
        if let Some(s) = usmap.get(&class) {
            println!("\nusmap schema for {class}: super={:?}, {} own properties",
                s.super_name, s.properties.len());
            for p in &s.properties { println!("    {} : {:?}", p.name, p.ty); }
        } else {
            println!("\n{class} not in usmap");
        }
        return;
    }
    eprintln!("not found: {suffix}");
}

fn e_body<'a>(h: &FZenPackageHeader, ua: &'a [u8]) -> &'a [u8] {
    let start = h.summary.header_size as usize;
    let size = h.export_map.first().map(|e| e.cooked_serial_size as usize).unwrap_or(0);
    &ua[start..(start + size).min(ua.len())]
}
