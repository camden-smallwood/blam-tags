//! Dump a UE5 Zen package in full: package name, exports (name+resolved),
//! imported packages, the ENTIRE local name map, and a hex+ascii view of
//! the export (property) body — to reverse the tag<->mesh binding stored
//! in `DA_*_MeshSynchronization` / `DA_*_Regions` without a .usmap.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_da_dump -- \
//!     "<paks>" <uasset-suffix e.g. da_elite_meshsynchronization.uasset> [hexbytes=1024]

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let suffix = args.next().unwrap_or_else(|| "da_elite_meshsynchronization.uasset".to_string()).to_ascii_lowercase();
    let hexbytes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1024);

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut found: Option<Vec<u8>> = None;
    let mut found_path = String::new();
    'outer: for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix) {
                found_path = e.path.clone();
                found = a.read(&e.path).ok();
                break 'outer;
            }
        }
    }
    let Some(bytes) = found else {
        eprintln!("no entry ending with {suffix:?}");
        std::process::exit(1);
    };
    println!("path: {found_path}  ({} bytes)", bytes.len());

    let mut cur = Cursor::new(&bytes);
    let hdr = match FZenPackageHeader::deserialize(&mut cur, None, CV, HV, None) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("parse failed: {e}");
            std::process::exit(1);
        }
    };

    println!("package: {}", hdr.package_name());
    println!("header_size: {}", hdr.summary.header_size);
    println!("\nexports ({}):", hdr.export_map.len());
    for (i, ex) in hdr.export_map.iter().enumerate().take(24) {
        println!("  [{i}] {}", hdr.name_map.get(ex.object_name));
    }

    println!("\nimported packages ({}):", hdr.imported_package_names.len());
    for p in &hdr.imported_package_names {
        println!("  <- {p}");
    }

    let names = hdr.name_map.copy_raw_names();
    println!("\nname map ({} names):", names.len());
    for (i, n) in names.iter().enumerate() {
        println!("  [{i:>3}] {n}");
    }

    // Export/property body hex+ascii
    let start = hdr.summary.header_size as usize;
    if start < bytes.len() {
        let end = (start + hexbytes).min(bytes.len());
        println!("\nexport body [{start}..{end}] hex+ascii:");
        hexdump(&bytes[start..end], start);
    }
}

fn hexdump(data: &[u8], base: usize) {
    for (row, chunk) in data.chunks(16).enumerate() {
        let off = base + row * 16;
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
            .collect();
        println!("  {off:08x}  {:<48}  {ascii}", hex.join(" "));
    }
}
