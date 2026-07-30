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
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{ExportContext, Trailer, read_export_in, write_export_in};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
    });
    let mut usmap = match std::fs::read(usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    // Without this, an export whose class is a Blueprint-generated one has no
    // schema and gets skipped *before* being counted — which is how this gate
    // reported 100% of 1,153,987 when the corpus is 1,243,749.
    let (registered, no_layout) = world.register_generated_classes();
    println!("registered {registered} generated classes ({no_layout} without a layout)");
    let usmap = world.usmap();

    let (mut total, mut same, mut unreadable, mut unwritable) = (0usize, 0usize, 0usize, 0usize);
    let mut unnamed = 0usize;
    let mut differ_by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut trailers: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut tail_bytes = 0u64;
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
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let resolver = world.resolver(&h, &b, &names);
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for ex in &h.export_map {
                total += 1;
                let Some(short) = world.class_key(&h, ex.class_index) else {
                    unnamed += 1;
                    continue;
                };
                let short = short.as_str();
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let Ok(parts) = read_export_in(body, &names, usmap, short, ex.object_flags, &ctx) else {
                    unreadable += 1;
                    continue;
                };
                tail_bytes += parts.tail.len() as u64;
                *trailers
                    .entry(match parts.trailer {
                        Trailer::Absent => "absent",
                        Trailer::NoGuid => "no-guid",
                        Trailer::Guid(_) => "guid",
                    })
                    .or_default() += 1;
                match write_export_in(short, &parts, usmap, Some(&resolver)) {
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
    println!("class unresolvable   {unnamed}");
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
