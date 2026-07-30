//! Which property types does our `ShouldSaveAsZero` derivation get wrong?
//!
//! The cooked file records, per present property, whether it was zero-masked.
//! the writer's own rule re-derives that from the value. For an
//! unmodified block the two must agree everywhere — so tallying the
//! disagreements by property *type* says exactly which types the derivation is
//! wrong about, instead of guessing from the engine source which flags a given
//! `FProperty` subclass carries.
//!
//! `over` = we would mask what the cooker wrote longhand.
//! `under` = we would write longhand what the cooker masked.
//!
//! Run: `ce_zero_mask_census [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{is_masked, read_export_struct_len};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::{PropertyType, Usmap};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// A coarse name for a property type — the thing we dispatch zero-maskability
/// on, without the inner types that do not affect it.
fn kind(ty: &PropertyType) -> String {
    match ty {
        PropertyType::Bool => "Bool".into(),
        PropertyType::Int8 => "Int8".into(),
        PropertyType::Int16 => "Int16".into(),
        PropertyType::Int => "Int".into(),
        PropertyType::Int64 => "Int64".into(),
        PropertyType::UInt16 => "UInt16".into(),
        PropertyType::UInt32 => "UInt32".into(),
        PropertyType::UInt64 => "UInt64".into(),
        PropertyType::Byte { enum_name: Some(_) } => "Byte(enum)".into(),
        PropertyType::Byte { .. } => "Byte".into(),
        PropertyType::Float => "Float".into(),
        PropertyType::Double => "Double".into(),
        PropertyType::Name => "Name".into(),
        PropertyType::Str => "Str".into(),
        PropertyType::Utf8Str | PropertyType::AnsiStr => "Str(other)".into(),
        PropertyType::Enum { .. } => "Enum".into(),
        PropertyType::Object => "Object".into(),
        PropertyType::WeakObject => "WeakObject".into(),
        PropertyType::LazyObject => "LazyObject".into(),
        PropertyType::Interface => "Interface".into(),
        PropertyType::SoftObject => "SoftObject".into(),
        PropertyType::AssetObject => "AssetObject".into(),
        PropertyType::Struct(n) => format!("Struct({n})"),
        PropertyType::Array(_) => "Array".into(),
        PropertyType::Set(_) => "Set".into(),
        PropertyType::Map(..) => "Map".into(),
        PropertyType::Delegate => "Delegate".into(),
        PropertyType::MulticastDelegate => "MulticastDelegate".into(),
        PropertyType::FieldPath => "FieldPath".into(),
        PropertyType::Text => "Text".into(),
        PropertyType::Optional(_) => "Optional".into(),
        PropertyType::Unknown(_) => "Unknown".into(),
    }
}

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
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    // Which *declaration* over-masks, not just which type. A type-level count
    // says "1,795 object properties disagree"; a declaration-level one says
    // which property to go read, which is the difference between a number and
    // a lead.
    let mut over_by_decl: BTreeMap<String, u64> = BTreeMap::new();
    // kind -> (agree, over-mask, under-mask)
    let mut tally: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();

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
                let Some(flat) = usmap.flattened_owned_slots(short) else { continue };
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let Ok((block, _)) = read_export_struct_len(&b[off..end], &names, &usmap, short)
                else {
                    continue;
                };
                for entry in &block.entries {
                    let Some(slot) = entry.slot else { continue };
                    let Some((prop, _, owner)) = flat.get(slot.index as usize) else { continue };
                    // Ask the *writer's* rule, not a re-derivation of it. This census
                    // reported a residue the writer never had for exactly as
                    // long as it computed its own answer here.
                    let derived = is_masked(
                        &prop.ty,
                        &prop.name,
                        owner,
                        slot.zero_masked,
                        &entry.value,
                        &usmap,
                    );
                    let t = tally.entry(kind(&prop.ty)).or_default();
                    match (slot.zero_masked, derived) {
                        (a, b) if a == b => t.0 += 1,
                        (false, true) => {
                            t.1 += 1;
                            *over_by_decl
                                .entry(format!("{owner}::{} ({})", prop.name, kind(&prop.ty)))
                                .or_default() += 1u64;
                        }
                        (true, false) => t.2 += 1,
                        _ => unreachable!(),
                    }
                }
            }
        }
    }

    let (mut ag, mut ov, mut un) = (0u64, 0u64, 0u64);
    for (a, o, u) in tally.values() {
        ag += a;
        ov += o;
        un += u;
    }
    println!("entries examined {}", ag + ov + un);
    println!("agree            {ag}");
    println!("over-mask        {ov}   (cooker wrote bytes, we would mask)");
    println!("under-mask       {un}   (cooker masked, we would write bytes)");

    let mut rows: Vec<_> = tally.iter().filter(|(_, (_, o, u))| *o > 0 || *u > 0).collect();
    rows.sort_by_key(|(_, (_, o, u))| std::cmp::Reverse(o + u));
    if !over_by_decl.is_empty() {
        println!("\nover-masking declarations ({} distinct):", over_by_decl.len());
        let mut v: Vec<_> = over_by_decl.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (k, n) in v.iter().take(30) {
            println!("  {n:>10}  {k}");
        }
    }
    println!("\n{:<34} {:>12} {:>12} {:>12}", "type", "agree", "over", "under");
    for (k, (a, o, u)) in rows.iter().take(30) {
        println!("{k:<34} {a:>12} {o:>12} {u:>12}");
    }
}
