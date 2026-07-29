//! Gate: can every *export* in the shipped corpus be taken apart and put back
//! together byte-exactly?
//!
//! `ce_block_roundtrip` proves the property block. This proves the whole
//! serial range: block, `UObject` trailer, and the natively serialized tail
//! that follows. The tail is still bytes — modeling it is Phase 4 — so what
//! this actually measures is that the *decomposition* is exact and that nothing
//! is lost at the seams between the three parts.
//!
//! That makes it a weaker claim than the block gate on its own, and a
//! deliberately useful one: it is the harness every future tail model gets
//! checked against. Convert one class from a span to a model, re-run this, and
//! the count says whether the model is lossless against the bytes it replaced.
//!
//! Run: `ce_export_roundtrip [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, write_export, Trailer};
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
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
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
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    let (mut total, mut same, mut unreadable, mut unwritable) = (0usize, 0usize, 0usize, 0usize);
    let mut differ_by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut trailers: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut tail_bytes = 0u64;
    let mut samples: Vec<String> = Vec::new();

    for a in &archives {
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
            let names = h.name_map.copy_raw_names();
            for ex in &h.export_map {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let Ok(parts) = read_export(body, &names, &usmap, short, ex.object_flags) else {
                    unreadable += 1;
                    continue;
                };
                total += 1;
                tail_bytes += parts.tail.len() as u64;
                *trailers
                    .entry(match parts.trailer {
                        Trailer::Absent => "absent",
                        Trailer::NoGuid => "no-guid",
                        Trailer::Guid(_) => "guid",
                    })
                    .or_default() += 1;
                match write_export(short, &parts, &usmap) {
                    Ok(out) if out == body => same += 1,
                    Ok(out) => {
                        *differ_by_class.entry(short.to_string()).or_default() += 1;
                        if samples.len() < 8 {
                            let at = out
                                .iter()
                                .zip(body)
                                .position(|(x, y)| x != y)
                                .unwrap_or(out.len().min(body.len()));
                            samples.push(format!(
                                "{} :: {short}\n    {} bytes in, {} out, first difference at {at}",
                                h.package_name(),
                                body.len(),
                                out.len()
                            ));
                        }
                    }
                    Err(err) => {
                        unwritable += 1;
                        if samples.len() < 8 {
                            samples.push(format!("{short}: {:#}", err));
                        }
                    }
                }
            }
        }
    }

    println!("exports examined     {total}");
    println!("rebuilt exactly      {same} ({:.4}%)", 100.0 * same as f64 / total.max(1) as f64);
    println!("differ               {}", total - same - unwritable);
    println!("refused to write     {unwritable}");
    println!("unreadable (skipped) {unreadable}");
    println!("tail bytes retained  {tail_bytes} ({:.2} GiB, Phase 4)", tail_bytes as f64 / (1 << 30) as f64);
    println!("trailers             {trailers:?}");

    if !differ_by_class.is_empty() {
        println!("\ndiffering by class:");
        let mut v: Vec<_> = differ_by_class.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(15) {
            println!("  {n:>7}  {c}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if same != total {
        std::process::exit(1);
    }
}
