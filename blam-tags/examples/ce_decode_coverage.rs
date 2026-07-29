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
//!  * **`PropValue::Native`** — the *fixed-size* native structs. `FVector`,
//!    `FGuid`, `FQuat` and friends are held as raw bytes and decoded on demand
//!    by helpers like `MeshTransform::from_prop`. They round-trip perfectly and
//!    are not typed, which is exactly the distinction this gate exists to make.
//!
//! Run: `ce_decode_coverage [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, BlockLayout, PropValue, PropertyBlock};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct Untyped {
    tail: u64,
    native_struct_span: u64,
    fixed_native: u64,
    raw: u64,
}

fn walk(v: &PropValue, u: &mut Untyped, by_struct: &mut BTreeMap<String, u64>, depth: usize) {
    if depth > 24 {
        return;
    }
    match v {
        PropValue::Native(b) => {
            u.fixed_native += b.len() as u64;
        }
        PropValue::Raw(b) => u.raw += b.len() as u64,
        PropValue::Struct(block) => {
            if let BlockLayout::Native { name, bytes } = &block.layout {
                u.native_struct_span += bytes.len() as u64;
                *by_struct.entry(name.to_string()).or_default() += bytes.len() as u64;
            }
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

    let mut u = Untyped::default();
    let mut by_struct: BTreeMap<String, u64> = BTreeMap::new();
    let mut tail_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_bytes = 0u64;

    for arc in &utocs {
        let Ok(a) = IoStoreArchive::open(arc) else { continue };
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
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                total_bytes += payloads[i].len() as u64;
                u.tail += parts.tail.len() as u64;
                if !parts.tail.is_empty() {
                    *tail_by_class.entry(short.to_string()).or_default() +=
                        parts.tail.len() as u64;
                }
                if let Some(block) = parts.block.as_ref() {
                    walk_block(block, &mut u, &mut by_struct);
                }
            }
        }
    }

    let untyped = u.tail + u.native_struct_span + u.fixed_native + u.raw;
    let typed = total_bytes.saturating_sub(untyped);

    println!("export bytes total     {total_bytes:>14}");
    println!(
        "behind a typed model   {typed:>14}  ({:.4}%)   <- the number Level 2 moves",
        100.0 * typed as f64 / total_bytes.max(1) as f64
    );
    println!("still a byte blob      {untyped:>14}  ({:.4}%)", 100.0 * untyped as f64 / total_bytes.max(1) as f64);
    println!();
    println!("  class tails          {:>14}  ({} classes)", u.tail, tail_by_class.len());
    println!("  hand-written structs {:>14}  ({} structs)", u.native_struct_span, by_struct.len());
    println!("  fixed native structs {:>14}  (FVector/FGuid/FQuat/... as raw bytes)", u.fixed_native);
    println!("  unmodeled (Raw)      {:>14}", u.raw);
}
