//! Gate: what share of an export's bytes sit behind a *typed model* rather than
//! a byte blob?
//!
//! This is the number Level 2 moves. `ce_export_roundtrip` says the bytes come
//! back; it says nothing about whether anything understood them, and a codec
//! that round-trips 4.77 GiB of `Vec<u8>` scores 100% on it while modeling
//! nothing.
//!
//! Four populations are untyped today, and they are not all in the plan:
//!
//!  * **`Export.tail`** — the class tails, retained spans.
//!  * **`BlockLayout::Native`** — hand-written structs, decoded into fields but
//!    written from their span.
//!  * **`PropValue::Raw`** — values the reader declines to interpret.
//!  * **`NativeStruct::Opaque`** — a fixed-size native struct whose size is
//!    known but whose fields are not modeled yet. The rest of that population
//!    is typed as of work item A2, which is what took this gate off its
//!    starting number.
//!
//! Run: `ce_decode_coverage [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{
    ExportBlock,
    read_export_in, roundtrip_tail, ExportContext,
    PropValue, PropertyBlock, TailContext,
};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

#[derive(Default)]
struct Untyped {
    tail: u64,
    native_struct_span: u64,
    fixed_native: u64,
    /// Payloads inside an otherwise-typed hand-written struct — currently only
    /// `FInstancedPropertyBag`, whose values are laid out by the bag's own
    /// descriptors and which nothing in the corpus ships enough of to model.
    hand_written: u64,
    raw: u64,
}

fn walk(v: &PropValue, u: &mut Untyped, by_struct: &mut BTreeMap<String, u64>, depth: usize) {
    if depth > 24 {
        return;
    }
    match v {
        // Typed now (work item A2) — only an unmodeled `Opaque` still counts.
        PropValue::Native(n) => u.fixed_native += n.untyped_bytes() as u64,
        // Typed as of work item A; nothing left untyped inside one.
        PropValue::HandWritten(h) => u.hand_written += h.untyped_bytes() as u64,
        PropValue::Raw(b) => u.raw += b.len() as u64,
        PropValue::Struct(block) => {
            for (_, inner) in block.iter() {
                walk(inner, u, by_struct, depth + 1);
            }
        }
        PropValue::Array(items) => items.iter().for_each(|x| walk(x, u, by_struct, depth + 1)),
        PropValue::Map(m) => m.iter().for_each(|(k, val)| {
            walk(k, u, by_struct, depth + 1);
            walk(val, u, by_struct, depth + 1);
        }),
        PropValue::WithRemovals { removals, inner } => {
            if let Some(r) = removals {
                r.iter().for_each(|x| walk(x, u, by_struct, depth + 1));
            }
            walk(inner, u, by_struct, depth + 1);
        }
        _ => {}
    }
}

fn walk_block(b: &PropertyBlock, u: &mut Untyped, by_struct: &mut BTreeMap<String, u64>) {
    for (_, v) in b.iter() {
        walk(v, u, by_struct, 0);
    }
}


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
    let mut u = Untyped::default();
    let mut by_struct: BTreeMap<String, u64> = BTreeMap::new();
    let mut tail_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut modeled_tail = 0u64;
    let mut unreflected_exports = 0u64;
    let mut unreflected_by_class: BTreeMap<String, u64> = BTreeMap::new();

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
            let pkg_bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let pkg_resolver = world.resolver(&h, &b, &names);
            let read_ctx =
                ExportContext { bulk_data: &pkg_bulk, resolver: Some(&pkg_resolver) };
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(short) = world.class_key(&h, ex.class_index) else { continue };
                let short = short.as_str();
                let Ok(parts) = read_export_in(&payloads[i], &names, usmap, short, ex.object_flags, &read_ctx)
                else {
                    continue;
                };
                total_bytes += payloads[i].len() as u64;
                // A tail with a model is *not* a blob. This gate predates
                // `tail_models` and counted every tail as one, which reported
                // 12.72% typed while 4.77 GiB of it had been converted.
                if !parts.tail.is_empty() {
                    let empty = Default::default();
                    let block = parts.properties().unwrap_or(&empty);
                    let bulk: Vec<(i64, i64)> =
                        h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
                    let resolver = world.resolver(&h, &b, &names);
                    let ctx = TailContext {
                        bulk_data: &bulk,
                        origin: payloads[i].len() - parts.tail.len(),
                        usmap: usmap,
                        resolver: Some(&resolver),
                        object_flags: ex.object_flags,
                    };
                    match roundtrip_tail(short, &parts.tail, &names, block, ctx) {
                        Some(Ok(_)) => modeled_tail += parts.tail.len() as u64,
                        // Either no model, or a model that needs context this
                        // gate does not supply — reported, not hidden.
                        _ => {
                            u.tail += parts.tail.len() as u64;
                            *tail_by_class.entry(short.to_string()).or_default() +=
                                parts.tail.len() as u64;
                        }
                    }
                }
                match &parts.block {
                    ExportBlock::Reflected(block) => walk_block(block, &mut u, &mut by_struct),
                    ExportBlock::NotSerialized => {}
                    // The 19 exports of the three `XGTGPerformanceOverlayTool`
                    // classes, whose module ships no reflection data at all.
                    // Counted as untyped rather than skipped: a filter here is
                    // what let the gate call this corpus fully covered while
                    // never looking at them.
                    ExportBlock::Unreflected(un) => {
                        unreflected_exports += 1;
                        u.raw += un.rest.len() as u64;
                        *unreflected_by_class.entry(short.to_string()).or_insert(0u64) +=
                            un.rest.len() as u64;
                    }
                }
            }
        }
    }

    if unreflected_exports > 0 {
        println!("\nno reflection data anywhere ({unreflected_exports} exports):");
        for (c, n) in &unreflected_by_class {
            println!("  {n:>8} B  {c}");
        }
        println!("  (see `UnreflectedBlock` — the declaring module is not shipped)");
    }

    let untyped = u.tail + u.native_struct_span + u.fixed_native + u.hand_written + u.raw;
    let typed = total_bytes.saturating_sub(untyped);

    println!("export bytes total     {total_bytes:>14}");
    println!(
        "behind a typed model   {typed:>14}  ({:.4}%)   <- the number Level 2 moves",
        100.0 * typed as f64 / total_bytes.max(1) as f64
    );
    println!("still a byte blob      {untyped:>14}  ({:.4}%)", 100.0 * untyped as f64 / total_bytes.max(1) as f64);
    println!();
    println!("  class tails          {:>14}  ({} classes with no model here)", u.tail, tail_by_class.len());
    println!();
    println!("  modeled tail bytes   {modeled_tail:>14}");
    println!(
        "  NOTE: bytes *inside* a modeled tail that are still `Vec<u8>` — Nanite pages, Chaos"
    );
    println!(
        "        geometry, shader bytecode, block-compressed mips, `TArray<uint8>` — are counted"
    );
    println!("        as modeled here. They are leaf data with no interior UE exposes either.");
    println!("  hand-written structs {:>14}  ({} structs)", u.native_struct_span, by_struct.len());
    println!("  unmodeled natives    {:>14}  (NativeStruct::Opaque)", u.fixed_native);
    println!("  property-bag payload {:>14}  (laid out by its own descriptors)", u.hand_written);
    println!("  unmodeled (Raw)      {:>14}", u.raw);
    if !tail_by_class.is_empty() {
        println!("\nclasses whose tail this gate could not model:");
        let mut v: Vec<_> = tail_by_class.iter().collect();
        v.sort_by_key(|(_, b)| std::cmp::Reverse(**b));
        for (c, b) in v {
            println!("  {b:>12}  {c}");
        }
    }
}
