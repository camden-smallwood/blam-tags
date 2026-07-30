//! Gate: does a *modeled* class tail reproduce the bytes it replaces?
//!
//! This is the property that makes Phase 4 safe to do incrementally. A tail
//! model is normally something you have to trust — it is a reading of engine
//! source against a binary blob, and a subtly wrong one produces plausible
//! values. Here the bytes are already known to be correct, because the export
//! round-trip copies them through verbatim. So converting a class from a
//! retained span into a model is checkable *against the span*: decode the tail,
//! re-emit it, and require the two to be identical.
//!
//! A class only appears here once it has a model, so the count grows as Phase 4
//! proceeds and never has to be taken on faith.
//!
//! Run: `ce_tail_model_roundtrip [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{TailContext, has_schema, read_export, roundtrip_tail};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::package::builder::read_payloads;

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
    // An export whose class is a Blueprint-generated one is reached through a
    // package import, not the global script objects. Without this it has no
    // schema and is skipped *before* being counted — 89,762 of the corpus's
    // 1,243,749 exports, which is why every gate used to say 1,153,987.
    let (registered, no_layout) = world.register_generated_classes();
    println!("registered {registered} generated classes ({no_layout} without a layout)");
    let usmap = world.usmap();

    // class -> (seen, exact, failed, bytes modeled)
    let mut stats: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
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
            // Texture mips reference their payloads through the package's
            // bulk-data map, so the model needs it as context.
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(short) = world.class_key(&h, ex.class_index) else { continue };
                let short = short.as_str();
                // Deliberately *not* filtered by `MODELED_TAILS`: families are
                // dispatched by inheritance chain, so most modeled classes are
                // never named in that list. `roundtrip_tail` returning `None` is
                // the authority on "no model yet".
                if !has_schema(short, usmap) {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, usmap, short, ex.object_flags)
                else {
                    continue;
                };
                if parts.tail.is_empty() {
                    continue;
                }
                let s = stats.entry(short.to_string()).or_default();
                s.0 += 1;
                // `URigVM` and `URigHierarchy` write no property block at all —
                // skipping them here is what made two exports look unmodeled
                // when nothing had ever asked.
                let empty = Default::default();
                let block = parts.properties().unwrap_or(&empty);
                let resolver = world.resolver(&h, &b, &names);
                let ctx = TailContext {
                    bulk_data: &bulk,
                    origin: payloads[i].len() - parts.tail.len(),
                    usmap: usmap,
                    resolver: Some(&resolver),
                    object_flags: ex.object_flags,
                };
                match roundtrip_tail(short, &parts.tail, &names, block, ctx) {
                    Some(Ok(out)) if out == parts.tail => {
                        s.1 += 1;
                        s.3 += parts.tail.len() as u64;
                    }
                    Some(Ok(out)) => {
                        s.2 += 1;
                        if samples.len() < 6 {
                            let at = out
                                .iter()
                                .zip(&parts.tail)
                                .position(|(x, y)| x != y)
                                .unwrap_or(out.len().min(parts.tail.len()));
                            samples.push(format!(
                                "{} :: {short}[{i}] {} bytes in, {} out, first difference at {at}\n    orig {:02x?}\n    ours {:02x?}",
                                h.package_name(),
                                parts.tail.len(),
                                out.len(),
                                &parts.tail[at.saturating_sub(4)..(at + 8).min(parts.tail.len())],
                                &out[at.saturating_sub(4)..(at + 8).min(out.len())],
                            ));
                        }
                    }
                    Some(Err(err)) => {
                        s.2 += 1;
                        if samples.len() < 6 {
                            samples.push(format!("{} :: {short}[{i}]: {err:#}", h.package_name()));
                        }
                    }
                    None => {}
                }
            }
        }
    }

    println!("{:<32} {:>12} {:>12} {:>10} {:>12}", "class", "tails", "exact", "failed", "bytes");
    let (mut t, mut e, mut f, mut by) = (0u64, 0u64, 0u64, 0u64);
    // A class with no model at all reports zero exact and zero failed. Counting
    // those in the denominator makes the pass rate look like a failure rate, so
    // they are reported as their own bucket.
    let mut unmodeled = 0u64;
    let mut unmodeled_classes = 0u64;
    let mut unmodeled_names: Vec<String> = Vec::new();
    for (class, (seen, exact, failed, bytes)) in &stats {
        if *exact == 0 && *failed == 0 {
            unmodeled += seen;
            unmodeled_classes += 1;
            unmodeled_names.push(format!("{class} ({seen})"));
            continue;
        }
        println!("{class:<44} {seen:>10} {exact:>10} {failed:>8} {bytes:>13}");
        t += seen;
        e += exact;
        f += failed;
        by += bytes;
    }
    println!(
        "\n{e} of {t} modeled tails exact ({:.4}%), {f} failed, {by} bytes now regenerated rather than retained\nno model yet: {unmodeled} tails across {unmodeled_classes} classes{names}",
        100.0 * e as f64 / t.max(1) as f64,
        names = if unmodeled_names.is_empty() {
            String::new()
        } else {
            format!(" — {}", unmodeled_names.join(", "))
        },
    );
    for s in &samples {
        println!("\n{s}");
    }
    if e != t || t == 0 {
        std::process::exit(1);
    }
}
