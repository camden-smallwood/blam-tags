//! Where does a RigLogic DNA stream end?
//!
//! `UDNAAsset::Serialize` reads two DNA streams back to back, but the container
//! records no total length. Generation 2 version 5 carries a section index with
//! per-section sizes, so its end is computable; version 1 carries only a table
//! of eight offsets. This prints the header of every DNA stream in an export and
//! locates every `DNA` signature in it, so the real boundary can be read off
//! rather than assumed.
//!
//! Run: ce_dna_layout [package-substring]
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn be32(s: &[u8]) -> usize {
    u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize
}
fn be16(s: &[u8]) -> usize {
    u16::from_be_bytes([s[0], s[1]]) as usize
}

fn main() {
    let want = std::env::args().nth(1).unwrap_or_default();
    let idx = FPackageObjectIndex::create_script_import("/Script/RigLogicModule.DNAAsset");
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut shown = 0;
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
            let name = h.package_name();
            if !want.is_empty() && !name.to_ascii_lowercase().contains(&want.to_ascii_lowercase()) {
                continue;
            }
            let Some(ex) = h.export_map.iter().find(|x| x.class_index == idx) else { continue };
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            if off >= end {
                continue;
            }
            let body = &b[off..end];
            // Every place a DNA stream could begin.
            let sigs: Vec<usize> =
                (0..body.len().saturating_sub(3)).filter(|&i| &body[i..i + 3] == b"DNA").collect();
            println!("\n=== {name} ({} bytes)", body.len());
            println!("  DNA signatures at {sigs:?}");
            for &s in &sigs {
                if s + 11 > body.len() {
                    continue;
                }
                let generation = be16(&body[s + 3..]);
                let ver = be16(&body[s + 5..]);
                print!("  @{s}: generation {generation} version {ver}");
                if ver >= 5 {
                    let count = be32(&body[s + 7..]);
                    let mut max_end = 0;
                    let mut ids = Vec::new();
                    for i in 0..count.min(64) {
                        let p = s + 11 + i * 16;
                        if p + 16 > body.len() {
                            break;
                        }
                        ids.push(String::from_utf8_lossy(&body[p..p + 4]).to_string());
                        max_end = max_end.max(be32(&body[p + 8..]) + be32(&body[p + 12..]));
                    }
                    println!(
                        ", {count} sections {ids:?}, ends at {} (abs {})",
                        max_end,
                        s + max_end
                    );
                } else {
                    let offs: Vec<usize> =
                        (0..8).map(|i| be32(&body[s + 7 + i * 4..])).collect();
                    println!(", 8 offsets {offs:?}, last abs {}", s + offs[7]);
                }
            }
            shown += 1;
            if shown >= 3 {
                return;
            }
        }
    }
}
