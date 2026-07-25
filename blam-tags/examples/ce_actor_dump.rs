//! Dump a cooked actor package's names/imports/exports to find how a MetaHuman
//! head is selected (metadata, not name-matching).
//!
//! cargo run --release -p blam-tags --features iostore --example ce_actor_dump -- <PaksDir> <actor-basename>

use std::error::Error;
use std::ffi::OsStr;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() -> Result<(), Box<dyn Error>> {
    let paks = PathBuf::from(std::env::args().nth(1).ok_or("usage: <PaksDir> <basename>")?);
    let want = std::env::args().nth(2).ok_or("usage: <PaksDir> <basename>")?.to_ascii_lowercase();

    let mut utocs = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { walk(&p, out); }
            else if p.extension() == Some(OsStr::new("utoc")) { out.push(p); }
        }
    }
    walk(&paks, &mut utocs);
    utocs.sort();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase();
            if !p.ends_with(".uasset") { continue; }
            let base = p.rsplit('/').next().unwrap_or(&p).trim_end_matches(".uasset");
            if base != want { continue; }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None) else { continue };
            println!("### {} ({})", e.path, u.file_stem().and_then(|s|s.to_str()).unwrap_or("?"));
            println!("\n-- imported packages ({}) --", hdr.imported_package_names.len());
            for ip in &hdr.imported_package_names {
                println!("  {ip}");
            }
            let names = hdr.name_map.copy_raw_names();
            println!("\n-- name table ({}) --", names.len());
            for n in &names {
                println!("  {n}");
            }
            println!("\n-- exports ({}) --", hdr.export_map.len());
            for (i, ex) in hdr.export_map.iter().enumerate() {
                println!("  [{i}] serial_size={} name_idx={:?}", ex.cooked_serial_size, ex.object_name);
            }
            return Ok(());
        }
    }
    eprintln!("actor '{want}' not found");
    Ok(())
}
