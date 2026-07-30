//! Why do the remaining virtual textures differ by one field?
//!
//! `ce_tail_model_roundtrip` reports 8,937 `Texture2D` tails that re-encode to
//! exactly the right *length* with a single 4-byte field reading zero where the
//! cook wrote a value. One length-preserving mistake repeated 8,937 times is a
//! systematic misreading, not a long tail, and the cheapest way to find it is to
//! put the model's fields next to the bytes rather than re-read the serializer.
//!
//! Dumps, for the first few failures: where the difference is, what the model
//! decoded, and the raw bytes either side.
//!
//! It found it in one run. The differing field was the `NAME_None` that
//! terminates the pixel-format list: its *text* is "None", but this package's
//! name map puts "None" at index 3444, and the writer was emitting
//! `FName::none()` — index 0. Same length, four wrong bytes, 8,937 times. An
//! `FName` is an index and a number; the text is a rendering of it.
//!
//! Kept because the next length-preserving disagreement wants the same tool.
//!
//! Run: `ce_vt_diff [usmap-path]`
use std::collections::HashMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, roundtrip_tail, TailContext};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    let so = ScriptObjects::load(format!("{PAKS}/global.utoc")).expect("script objects");
    for e in so.entries() {
        if let Some(p) = so.resolve(e.global_index.raw_index()) {
            by_hash.insert(e.global_index.raw_index(), p.to_string());
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read Paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut shown = 0;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if shown >= 3 {
                return;
            }
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                if shown >= 3 {
                    return;
                }
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let want = std::env::args().nth(2).unwrap_or_else(|| "Texture2D".into());
                if short != want {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties() else { continue };
                if parts.tail.is_empty() {
                    continue;
                }
                let origin = payloads[i].len() - parts.tail.len();
                let ctx = TailContext {
                    bulk_data: &bulk,
                    origin,
                    usmap: &usmap,
                    resolver: None,
                    object_flags: ex.object_flags,
                };
                let out = match roundtrip_tail(short, &parts.tail, &names, block, ctx) {
                    Some(Ok(o)) => o,
                    Some(Err(e)) => {
                        shown += 1;
                        println!("\n=== {} :: {short}[{i}] === {e:#}", h.package_name());
                        println!("tail {} bytes; last 24: {:02x?}", parts.tail.len(),
                            &parts.tail[parts.tail.len().saturating_sub(24)..]);
                        continue;
                    }
                    None => continue,
                };
                if out == parts.tail {
                    continue;
                }
                let at = out
                    .iter()
                    .zip(&parts.tail)
                    .position(|(x, y)| x != y)
                    .unwrap_or(out.len().min(parts.tail.len()));
                shown += 1;
                println!("\n=== {} :: {short}[{i}] ===", h.package_name());
                println!("tail {} bytes, export payload {} bytes, tail origin {origin}", parts.tail.len(), payloads[i].len());
                println!("first difference at {at} ({} from the end)", parts.tail.len() - at);
                let lo2 = at.saturating_sub(48);
                println!("orig[{lo2}..] {:02x?}", &parts.tail[lo2..]);
                println!("ours[{lo2}..] {:02x?}", &out[lo2..]);
                println!("bulk map ({} entries):", bulk.len());
                for (n, (off, size)) in bulk.iter().enumerate().take(12) {
                    println!("  [{n}] offset {off} size {size}  (tail-relative {})", *off - origin as i64);
                }
            }
        }
    }
}
