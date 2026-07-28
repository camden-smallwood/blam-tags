//! Derive the true on-disk layout of a cooked `UFunction` export.
//!
//! `UStruct::Serialize` (Class.cpp, 5.5.4) writes `SuperStruct`, a
//! `TArray<UField*> ChildArray`, then `SerializeProperties` (an `int32` count
//! followed by that many `FField`s, each a type-name `FName` plus
//! `FProperty::Serialize`), then the Kismet script. `UFunction` appends
//! `FunctionFlags`, an `int16 RepOffset` when `FUNC_Net`, and always
//! `EventGraphFunction` + `EventGraphCallOffset`.
//!
//! The existing reader *probes* pad offsets to find the field count, which
//! silently accepts a wrong interpretation. This dumps the raw bytes next to a
//! name-resolved reading of every 4- and 8-byte window, so the real field
//! boundaries can be read off directly instead of guessed.
//!
//! Run: ce_ufunction_layout <package-substring> [export-index]
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    b.get(o..o + 4).map(|s| i32::from_le_bytes(s.try_into().unwrap()))
}

/// Read offset `o` as an `FName` (index + number) and resolve it, if it lands
/// inside the package name map and looks like a real entry.
fn name_at(b: &[u8], o: usize, names: &[String]) -> Option<String> {
    let idx = i32_at(b, o)?;
    let num = i32_at(b, o + 4)?;
    // Index 0 is the package's own name in most CE packages; every run of
    // zero bytes would otherwise "resolve" to it and bury the real annotations.
    if idx <= 0 || num < 0 || num > 4096 {
        return None;
    }
    let base = names.get(idx as usize)?;
    if base.len() > 48 {
        return None;
    }
    Some(if num > 0 { format!("{base}_{}", num - 1) } else { base.clone() })
}

fn main() {
    let want = std::env::args().nth(1).expect("package substring");
    let pick: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let idx = FPackageObjectIndex::create_script_import("/Script/CoreUObject.Function");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            if !h.package_name().to_ascii_lowercase().ends_with(&want.to_ascii_lowercase()) {
                continue;
            }
            let names = h.name_map.copy_raw_names();
            let fns: Vec<_> = h.export_map.iter().filter(|x| x.class_index == idx).collect();
            if fns.is_empty() {
                continue;
            }
            println!("{} — {} Function exports", h.package_name(), fns.len());
            let Some(ex) = fns.get(pick) else { return };
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            let body = &b[off..end];
            println!("export #{pick}, {} bytes, flags {:#x}\n", body.len(), ex.object_flags);

            // Raw bytes, 16 to a line, with any FName that starts at each offset.
            for (i, chunk) in body.chunks(16).enumerate() {
                let base = i * 16;
                print!("{base:04x}: ");
                for x in chunk {
                    print!("{x:02x} ");
                }
                for _ in chunk.len()..16 {
                    print!("   ");
                }
                // Annotate any offset in this line that reads as a plausible name.
                let mut ann = Vec::new();
                for k in 0..chunk.len() {
                    if let Some(n) = name_at(body, base + k, &names) {
                        if !n.is_empty() && n != "None" {
                            ann.push(format!("+{k}={n}"));
                        }
                    }
                }
                println!(" {}", ann.join(" "));
            }
            println!("\nname map ({} entries):", names.len());
            for (i, n) in names.iter().enumerate() {
                println!("  {i:3} {n}");
            }
            return;
        }
    }
    println!("no matching package");
}
