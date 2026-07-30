//! Where does native-tail modeling actually stop, and how much is behind it?
//!
//! The tail dispatcher has 49 sites that rewind and decline to continue. Each is
//! a deliberate boundary, but as a list of 49 they say nothing about which ones
//! matter: one that fires on every `StaticMesh` is the frontier, one that has
//! never fired is a hypothetical.
//!
//! Reports per stopping class: how many exports it stops, and how many bytes are
//! left unmodeled behind it. That ordering is what says where Phase 4 work is
//! worth doing — and what says which of the 49 could simply be deleted.
//!
//! Run: `ce_tail_stop_census [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{ExportContext, has_schema, walk_export};
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

    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    // An export whose class is a Blueprint-generated one is reached through a
    // package import, not the global script objects. Without this it has no
    // schema and is skipped *before* being counted — 89,762 of the corpus's
    // 1,243,749 exports, which is why every gate used to say 1,153,987.
    let (registered, no_layout) = world.register_generated_classes();
    println!("registered {registered} generated classes ({no_layout} without a layout)");
    let usmap = world.usmap();

    // stopping class -> (exports stopped, bytes left unmodeled)
    let mut stops: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut walked, mut complete) = (0u64, 0u64);
    let mut unmodeled_total = 0u64;
    // A declining arm is not the only way a tail goes unread. An arm that
    // returns "kept going" while leaving bytes behind reports no stop at all,
    // and the walk looks complete: `UMaterial` left four bytes unconsumed on
    // every one of 1,397 exports and this census called them all fully modeled.
    // Bytes consumed is the claim worth making, so it is measured separately.
    let mut short_by_class: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    // The bytes themselves, for the first example of each class -- a count says
    // an arm is short, the bytes say what it is short *of*.
    let mut short_sample: BTreeMap<String, String> = BTreeMap::new();
    let (mut to_the_end, mut leftover_total) = (0u64, 0u64);

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
            let Ok(payloads) = read_payloads(&h, &b) else {
                continue;
            };
            let names = h.name_map.copy_raw_names();
            // The bulk-data map is not optional context: `BodySetup` and friends
            // resolve their cooked payloads through it, so omitting it makes
            // them fail every time and attributes their whole tail to a "stop"
            // that is really a missing argument.
            let bulk: Vec<(i64, i64)> = h
                .bulk_data
                .iter()
                .map(|b| (b.serial_offset, b.serial_size))
                .collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = world.class_key(&h, ex.class_index) else {
                    continue;
                };
                let class = class.as_str();
                let short = class.rsplit('.').next().unwrap_or(class);
                if !has_schema(short, usmap) {
                    continue;
                }
                let resolver = world.resolver(&h, &b, &names);
                let ctx = ExportContext {
                    bulk_data: &bulk,
                    resolver: Some(&resolver),
                };
                let Ok(walk) =
                    walk_export(&payloads[i], &names, usmap, short, ex.object_flags, &ctx)
                else {
                    continue;
                };
                walked += 1;
                let leftover = payloads[i].len().saturating_sub(walk.consumed);
                if leftover == 0 {
                    to_the_end += 1;
                } else if walk.stopped.is_none() {
                    // Only interesting when nothing declined -- a stop already
                    // accounts for its own remainder.
                    let e = short_by_class.entry(short.to_string()).or_default();
                    e.0 += 1;
                    e.1 += leftover as u64;
                    leftover_total += leftover as u64;
                    short_sample.entry(short.to_string()).or_insert_with(|| {
                        format!(
                            "{} left {leftover} of {} bytes; unread tail {:02x?}",
                            h.package_name(),
                            payloads[i].len(),
                            &payloads[i][walk.consumed..],
                        )
                    });
                }
                match walk.stopped {
                    None => complete += 1,
                    Some(stop) => {
                        let e = stops.entry(stop.class).or_default();
                        e.0 += 1;
                        e.1 += stop.remaining as u64;
                        unmodeled_total += stop.remaining as u64;
                    }
                }
            }
        }
    }

    println!("exports walked        {walked}");
    println!(
        "chain fully modeled   {complete} ({:.2}%)",
        100.0 * complete as f64 / walked.max(1) as f64
    );
    println!("stopped early         {}", walked - complete);
    println!(
        "bytes behind a stop   {unmodeled_total} ({:.2} GiB)",
        unmodeled_total as f64 / (1u64 << 30) as f64
    );
    println!();
    println!(
        "consumed to the end   {to_the_end} ({:.4}%)   <- the stronger claim",
        100.0 * to_the_end as f64 / walked.max(1) as f64
    );
    let short: u64 = short_by_class.values().map(|(n, _)| n).sum();
    println!(
        "silently short        {short} ({leftover_total} bytes across {} classes)",
        short_by_class.len()
    );

    if !short_by_class.is_empty() {
        println!("\nwalked without declining but left bytes behind:");
        let mut v: Vec<_> = short_by_class.iter().collect();
        v.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
        for (c, (n, b)) in v.iter().take(25) {
            println!("  {c:<44} {n:>10} exports {b:>12} bytes");
            if let Some(s) = short_sample.get(*c) {
                println!("      {s}");
            }
        }
    }

    let mut rows: Vec<_> = stops.iter().collect();
    rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    println!(
        "\n{:<40} {:>12} {:>16}",
        "stopping class", "exports", "unmodeled bytes"
    );
    for (class, (n, bytes)) in rows.iter().take(25) {
        println!("{class:<40} {n:>12} {bytes:>16}");
    }
    println!("\n{} distinct stopping classes", stops.len());
}
