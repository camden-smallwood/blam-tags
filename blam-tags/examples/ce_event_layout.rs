//! Reverse the on-disk layout of a cooked `UAkAudioEvent` export.
//!
//! The unversioned-property header on these exports is a single fragment that
//! skips all 8 reflected properties, so `EventCookedData` is NOT written via
//! reflection — the Wwise plugin serializes it natively. This probe anchors on
//! values we already know from the package name map (media ids parsed out of
//! `Media/<nn>/<id>.wem`, the bank id, name-map indices) and reports where each
//! appears in the body, which reveals the record layout.
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_event_layout -- <event-substr>

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn find_u32(body: &[u8], v: u32) -> Vec<usize> {
    let b = v.to_le_bytes();
    (0..body.len().saturating_sub(3)).filter(|&i| body[i..i + 4] == b).collect()
}

fn main() -> anyhow::Result<()> {
    let want = std::env::args().nth(1).expect("usage: <event-substr>").to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut bytes = None;
    let mut found = String::new();
    'outer: for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let p = e.path.replace('\\', "/").to_ascii_lowercase();
            if p.contains("/events/") && p.ends_with(".uasset") && p.contains(&want) {
                found = e.path.clone();
                bytes = a.read(&e.path).ok();
                break 'outer;
            }
        }
    }
    let bytes = bytes.ok_or_else(|| anyhow::anyhow!("no event matching {want:?}"))?;
    println!("asset: {found}");

    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
        .map_err(|e| anyhow::anyhow!("zen: {e}"))?;
    let names = hdr.name_map.copy_raw_names();
    let ex = hdr.export_map.first().unwrap();
    let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
    let body = &bytes[start..start + ex.cooked_serial_size as usize];

    println!("\nname map:");
    for (i, n) in names.iter().enumerate() {
        println!("  [{i:>2}] {n}");
    }

    println!("\nbody ({} bytes):", body.len());
    for (r, c) in body.chunks(16).enumerate() {
        let hex: Vec<String> = c.iter().map(|b| format!("{b:02x}")).collect();
        let asc: String =
            c.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        println!("  {:04x}  {:<48}  {asc}", r * 16, hex.join(" "));
    }

    // Anchor 1: media ids parsed out of the name-map paths.
    println!("\nanchors (little-endian u32 hits in body):");
    for (i, n) in names.iter().enumerate() {
        let stem = n.rsplit('/').next().unwrap_or(n);
        let Some(num) = stem.split('.').next() else { continue };
        if !(n.ends_with(".wem") || n.ends_with(".bnk")) {
            continue;
        }
        let Ok(v) = num.parse::<u32>() else { continue };
        println!("  name[{i}] {n}  id={v} (0x{v:08x})  at {:?}", find_u32(body, v));
    }

    // Anchor 2: FName references are (nameIndex u32, number u32) pairs, so a
    // small index appearing at a 4-byte slot followed by 0 is likely an FName.
    println!("\nplausible FName slots (idx<{} followed by number 0):", names.len());
    for o in (0..body.len().saturating_sub(8)).step_by(1) {
        let idx = u32::from_le_bytes(body[o..o + 4].try_into().unwrap());
        let num = u32::from_le_bytes(body[o + 4..o + 8].try_into().unwrap());
        if (idx as usize) < names.len() && num == 0 {
            println!("  @{o:#06x} -> name[{idx}] = {}", names[idx as usize]);
        }
    }
    Ok(())
}
