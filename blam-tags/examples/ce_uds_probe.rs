//! Hand-check a `UUserDefinedStruct`: its recovered `FField` chain against the
//! bytes its own `Serialize` leaves behind (`UScriptStruct` flags + the default
//! struct instance written by `SerializeItem`).
//!
//! Run: ce_uds_probe [package-substring]
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_with_trailer, read_userdefined_struct_layout, ExportContext};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut usmap = Usmap::parse(blam_tags::iostore::usmap::METEORITE_USMAP).unwrap();
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let filter = std::env::args().nth(1);
    let limit: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(3);
    let idx = FPackageObjectIndex::create_script_import("/Script/CoreUObject.UserDefinedStruct");

    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();
    let mut shown = 0;
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            if let Some(f) = &filter {
                if !h.package_name().to_ascii_lowercase().contains(&f.to_ascii_lowercase()) { continue }
            }
            for ex in h.export_map.iter().filter(|x| x.class_index == idx) {
                let names = h.name_map.copy_raw_names();
                let bulk: Vec<(i64, i64)> = h.bulk_data.iter().map(|d| (d.serial_offset, d.serial_size)).collect();
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() { continue }
                let body = &b[off..end];
                println!("\n=== {} ({} bytes, flags {:#x})", h.package_name(), body.len(), ex.object_flags);
                match read_userdefined_struct_layout(body, &names, &usmap, ex.object_flags, &ExportContext::new(&bulk)) {
                    Ok(props) => {
                        println!("  field chain ({} props):", props.len());
                        for p in &props {
                            println!("    [{}] {} : {:?} x{}", p.schema_index, p.name, p.ty, p.array_dim);
                        }
                    }
                    Err(err) => println!("  chain ERROR: {err:#}"),
                }
                match read_export_with_trailer(body, &names, &usmap, "UserDefinedStruct", ex.object_flags, &ExportContext::new(&bulk)) {
                    Ok((_, used)) => {
                        println!("  consumed {used} of {}, {} left", body.len(), body.len() - used.min(body.len()));
                        let tail = &body[used.min(body.len())..];
                        for (i, chunk) in tail.chunks(16).enumerate().take(10) {
                            print!("  {:04x}: ", used + i * 16);
                            for x in chunk { print!("{x:02x} ") }
                            println!();
                        }
                    }
                    Err(err) => println!("  decode ERROR: {err:#}"),
                }
                shown += 1;
                if shown >= limit { return }
            }
        }
    }
}
