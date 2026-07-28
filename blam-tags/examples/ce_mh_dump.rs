//! Dump DT_MetaHumanHeads rows (key, Type, Head, FacialHair) to confirm the
//! generic-vs-hero row naming for the model-preview generic-head fallback.
//!
//! cargo run --release -p blam-tags --features iostore --example ce_mh_dump -- <PaksDir>

use std::error::Error;
use std::ffi::OsStr;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_datatable, read_userdefined_struct_layout, ExportContext, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn find_uasset(archives: &[IoStoreArchive], basename: &str) -> Option<Vec<u8>> {
    let want = basename.to_ascii_lowercase();
    for a in archives {
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase();
            if p.ends_with(".uasset")
                && p.rsplit('/').next().unwrap_or(&p).trim_end_matches(".uasset") == want
            {
                return a.read(&e.path).ok();
            }
        }
    }
    None
}

fn export0<'a>(bytes: &'a [u8], hdr: &FZenPackageHeader) -> Option<&'a [u8]> {
    let ex = hdr.export_map.first()?;
    let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
    bytes.get(start..start + ex.cooked_serial_size as usize)
}

fn decode(
    archives: &[IoStoreArchive],
    struct_basename: &str,
    struct_name: &str,
    table_basename: &str,
) -> Option<Vec<(String, std::collections::BTreeMap<String, PropValue>)>> {
    let mut usmap = Usmap::meteorite().ok()?;
    let sbytes = find_uasset(archives, struct_basename)?;
    let shdr = FZenPackageHeader::deserialize(&mut Cursor::new(&sbytes[..]), None, CV, HV, None).ok()?;
    let sctx = ExportContext::new(&[]);
    let props = read_userdefined_struct_layout(
        export0(&sbytes, &shdr)?,
        &shdr.name_map.copy_raw_names(),
        &usmap,
        shdr.export_map.first()?.object_flags,
        &sctx,
    )
    .ok()?;
    usmap.register_struct(struct_name, None, props);
    let dbytes = find_uasset(archives, table_basename)?;
    let dhdr = FZenPackageHeader::deserialize(&mut Cursor::new(&dbytes[..]), None, CV, HV, None).ok()?;
    read_datatable(
        export0(&dbytes, &dhdr)?,
        &dhdr.name_map.copy_raw_names(),
        &usmap,
        struct_name,
        dhdr.export_map.first()?.object_flags,
    )
    .ok()
}

fn main() -> Result<(), Box<dyn Error>> {
    let paks = PathBuf::from(std::env::args().nth(1).ok_or("usage: ce_mh_dump <PaksDir>")?);
    let mut utocs = Vec::new();
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension() == Some(OsStr::new("utoc")) {
                out.push(p);
            }
        }
    }
    walk(&paks, &mut utocs);
    utocs.sort();
    let archives: Vec<IoStoreArchive> = utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    // List actors importing BPC_MetaHumanCreator (the human marker) + derived key.
    println!("\n===== HUMAN ACTORS (import BPC_MetaHumanCreator) =====");
    let mut human_keys: Vec<String> = Vec::new();
    for a in &archives {
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase();
            if !p.ends_with(".uasset") || !p.contains("actor") {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None) else { continue };
            let is_human = hdr
                .imported_package_names
                .iter()
                .any(|ip| ip.rsplit('/').next().unwrap_or(ip).eq_ignore_ascii_case("BPC_MetaHumanCreator"));
            if !is_human {
                continue;
            }
            let base = p.rsplit('/').next().unwrap_or(&p).trim_end_matches(".uasset");
            let key = base.strip_prefix("bp_").unwrap_or(base);
            let key = key.strip_suffix("bipedactor").unwrap_or(key);
            human_keys.push(format!("{base}  ->  key='{key}'"));
        }
    }
    human_keys.sort();
    human_keys.dedup();
    for k in &human_keys {
        println!("  {k}");
    }

    for (label, sb, sn, tb) in [
        ("HEADS", "S_MetaHumanHeads", "S_MetaHumanHeads", "DT_MetaHumanHeads"),
        ("HELMETS", "S_MetaHumanHelmets", "S_MetaHumanHelmets", "DT_MetaHumanHelmets"),
    ] {
        println!("\n===== {label} ({tb}) =====");
        match decode(&archives, sb, sn, tb) {
            Some(rows) => {
                println!("{} rows", rows.len());
                for (key, fields) in &rows {
                    let ty = fields.get("Type").and_then(PropValue::as_str).unwrap_or("?");
                    let head = fields
                        .get("Head")
                        .or_else(|| fields.get("Mesh"))
                        .and_then(PropValue::as_soft_object)
                        .map(|s| format!("{}", s.asset))
                        .unwrap_or_default();
                    let hair = fields
                        .get("FacialHair")
                        .and_then(PropValue::as_array)
                        .map(|a| a.len())
                        .unwrap_or(0);
                    println!("  {key:24} type={ty:8} mesh={head:36} hair={hair}");
                }
            }
            None => println!("  (decode failed / table not found)"),
        }
    }
    Ok(())
}
