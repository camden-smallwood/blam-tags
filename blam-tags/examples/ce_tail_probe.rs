//! Where does an export's *whole* decode stop, and what is left over?
//!
//! Runs `read_export_with_trailer` (property block + UObject trailer + every
//! modeled class tail) and hexdumps the bytes that remain, so the next
//! unmodeled field can be read directly instead of inferred.
//!
//! Run: ce_tail_probe <Class|/Script/Module.Class> [package-substring]
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_with_trailer, ExportContext};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    // Classes the shipped binary lacks entirely, so no dump of it can carry them.
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let arg = std::env::args().nth(1).expect("class");
    let path = if arg.contains('.') { arg.clone() } else { format!("/Script/Engine.{arg}") };
    let cls = arg.rsplit('.').next().unwrap().to_string();
    let idx = FPackageObjectIndex::create_script_import(&path);
    let filter = std::env::args().nth(2);

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
            // Every matching export, not just the first — a package can hold
            // several of a class and only the later ones may still have a tail.
            for ex in h.export_map.iter().filter(|x| x.class_index == idx) {
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> = h.bulk_data.iter().map(|d| (d.serial_offset, d.serial_size)).collect();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            if off >= b.len() { continue }
            let body = &b[off..end];
            // BLAM_DUMP_FROM/BLAM_DUMP_LEN: hexdump an arbitrary window of the
            // export body, for hand-checking a layout away from its tail.
            if let Ok(from) = std::env::var("BLAM_DUMP_FROM") {
                let from: usize = from.parse().unwrap_or(0);
                let len: usize = std::env::var("BLAM_DUMP_LEN").ok()
                    .and_then(|v| v.parse().ok()).unwrap_or(128);
                println!("--- {} bytes {from}..{}", h.package_name(), from + len);
                for (i, ch) in body[from.min(body.len())..(from + len).min(body.len())].chunks(16).enumerate() {
                    print!("  {:04x}: ", from + i * 16);
                    for x in ch { print!("{x:02x} ") }
                    println!();
                }
            }
            let hdr = format!("\n=== {} ({} bytes)", h.package_name(), body.len());
            if std::env::var("BLAM_ONLY_TAILS").is_err() { println!("{hdr}"); }
            match read_export_with_trailer(body, &names, &usmap, &cls, ex.object_flags, &ExportContext::new(&bulk)) {
                Ok((_, used)) => {
                    let left = body.len() - used.min(body.len());
                    // BLAM_ONLY_TAILS: skip exports that already account fully,
                    // so the next unmodeled variant is what gets shown.
                    if left == 0 && std::env::var("BLAM_ONLY_TAILS").is_ok() { continue }
                    if std::env::var("BLAM_ONLY_TAILS").is_ok() { println!("{hdr}"); }
                    println!("  consumed {used} of {}, {} left", body.len(), left);
                    let tail = &body[used.min(body.len())..];
                    for (i, chunk) in tail.chunks(16).take(8).enumerate() {
                        print!("  {:04x}: ", used + i * 16);
                        for x in chunk { print!("{x:02x} ") }
                        println!();
                    }
                }
                Err(err) => println!("  ERROR: {err:#}"),
            }
            shown += 1;
            if shown >= 2 { return }
            }
        }
    }
}
