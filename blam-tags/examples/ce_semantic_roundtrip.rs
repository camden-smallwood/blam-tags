//! Gate: `decode(encode(decode(x))) == decode(x)` for every export.
//!
//! This is the contract every model must meet, and the only one a lossy payload
//! can. Byte-identity is stronger and `ce_export_roundtrip` measures it, but BC
//! texture compression discards information by construction — decoding a mip and
//! re-compressing cannot reproduce the original blocks. What can always be
//! required is that the *data* survives: whatever could be read out originally
//! still reads out after writing.
//!
//! It is deliberately built before the models it will judge. Right now every
//! tail is a retained span, so this passes trivially — which is the point. It is
//! the harness that says whether each conversion preserved meaning, and a
//! conversion that breaks it is a model that lost something.
//!
//! Reports separately the exports that are byte-identical too, because a class
//! *without* a lossy payload should be, and one that quietly stops being is a
//! regression rather than an accepted cost.
//!
//! Run: `ce_semantic_roundtrip [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{ExportContext, read_export_in, write_export_in};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::package::builder::read_payloads;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";


fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let world = World::open(PAKS, usmap).expect("mount Paks");
    let usmap = world.usmap();

    let (mut total, mut semantic_ok, mut byte_ok, mut failed) = (0u64, 0u64, 0u64, 0u64);
    let mut lossy: BTreeMap<String, u64> = BTreeMap::new();
    let mut broke: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for a in world.archives() {
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
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let resolver = world.resolver(&h, &b, &names);
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = world.class_path(ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let Ok(first) = read_export_in(&payloads[i], &names, usmap, short, ex.object_flags, &ctx)
                else {
                    continue;
                };
                total += 1;

                let Ok(bytes) = write_export_in(short, &first, usmap, Some(&resolver)) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    continue;
                };
                let Ok(second) = read_export_in(&bytes, &names, usmap, short, ex.object_flags, &ctx) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: re-reading our own output failed",
                            h.package_name()
                        ));
                    }
                    continue;
                };

                if first.semantic_eq(&second) {
                    semantic_ok += 1;
                    if bytes == payloads[i] {
                        byte_ok += 1;
                    } else {
                        *lossy.entry(short.to_string()).or_default() += 1;
                        if samples.len() < 4 {
                            let at = bytes
                                .iter()
                                .zip(&payloads[i])
                                .position(|(x, y)| x != y)
                                .unwrap_or(bytes.len().min(payloads[i].len()));
                            let lo = at.saturating_sub(12);
                            samples.push(format!(
                                "{} :: {short}[{i}] value-stable, bytes differ\n    {} in, {} out, first difference at {at}\n    orig {:02x?}\n    ours {:02x?}",
                                h.package_name(),
                                payloads[i].len(),
                                bytes.len(),
                                &payloads[i][lo..(at + 12).min(payloads[i].len())],
                                &bytes[lo..(at + 12).min(bytes.len())],
                            ));
                        }
                    }
                } else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: value did not survive the round trip",
                            h.package_name()
                        ));
                    }
                }
            }
        }
    }

    println!("exports examined      {total}");
    println!(
        "semantically stable   {semantic_ok} ({:.4}%)   <- the contract",
        100.0 * semantic_ok as f64 / total.max(1) as f64
    );
    println!(
        "  also byte-identical {byte_ok} ({:.4}%)",
        100.0 * byte_ok as f64 / total.max(1) as f64
    );
    println!("  value-stable only   {}", semantic_ok - byte_ok);
    println!("broken                {failed}");

    if !lossy.is_empty() {
        println!("\nsemantically stable but no longer byte-identical:");
        println!("  (each must be a class that genuinely contains a lossy codec)");
        let mut v: Vec<_> = lossy.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    if !broke.is_empty() {
        println!("\nbroken by class:");
        let mut v: Vec<_> = broke.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
