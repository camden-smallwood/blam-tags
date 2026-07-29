//! Does the extracted native-`bool` table agree with what the cooker did?
//!
//! `blam_tags::iostore::object::native_bool` is scraped from the game's own
//! `UHTHeaderDump`, which makes it evidence about the *declarations*. The
//! shipped packages are evidence about the *encoder*. They should agree, and
//! this measures whether they do — because the table is only worth wiring into
//! `CanSerializeAsZero` if it reproduces the masking the cooker chose.
//!
//! Two directions, and they fail differently:
//!
//!  * **masked but not native** — the cooker zero-masked a `bool` the table
//!    calls a bitfield. The table is wrong, and trusting it would make us write
//!    that property longhand where the cooker masked it.
//!  * **native and zero but not masked** — the table calls it a real `bool`, its
//!    value is zero, and the cooker still wrote it longhand. Trusting the table
//!    would make us *start* masking something the cooker did not.
//!
//! The first is a correctness problem; the second would silently change the
//! bytes of every affected export.
//!
//! Run: `ce_native_bool_check [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::native_bool::{bool_is_known, is_native_bool};
use blam_tags::iostore::object::unversioned::{read_export, PropValue, PropertyBlock};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::{PropertyType, Usmap};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct Tally {
    bools: u64,
    known: u64,
    masked: u64,
    /// The cooker masked it; the table says it is not a native bool.
    masked_but_not_native: u64,
    /// The table says native, the value is zero, the cooker wrote it longhand.
    native_zero_but_unmasked: u64,
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

    let mut t = Tally::default();
    // Offender -> count, so a disagreement names the declaration to go read.
    let mut wrong_masked: BTreeMap<String, u64> = BTreeMap::new();
    let mut wrong_unmasked: BTreeMap<String, u64> = BTreeMap::new();
    let mut unknown: BTreeMap<String, u64> = BTreeMap::new();

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
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let Some(slots) = usmap.flattened_owned_slots(short) else { continue };
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.block.as_ref() else { continue };
                scan(block, &slots, &mut t, &mut wrong_masked, &mut wrong_unmasked, &mut unknown);
            }
        }
    }

    println!("bool entries examined     {}", t.bools);
    println!(
        "  declared in the dump    {} ({:.4}%)",
        t.known,
        100.0 * t.known as f64 / t.bools.max(1) as f64
    );
    println!("  zero-masked by cooker   {}", t.masked);
    println!();
    println!("disagreements:");
    println!("  masked but not native   {}", t.masked_but_not_native);
    println!("  native+zero, unmasked   {}", t.native_zero_but_unmasked);

    for (label, m) in
        [("masked but not native", &wrong_masked), ("native+zero but unmasked", &wrong_unmasked)]
    {
        if m.is_empty() {
            continue;
        }
        println!("\n{label}, by declaration:");
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (k, n) in v.iter().take(20) {
            println!("  {n:>10}  {k}");
        }
    }
    if !unknown.is_empty() {
        println!("\nbool properties the dump never declared ({}):", unknown.len());
        let mut v: Vec<_> = unknown.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (k, n) in v.iter().take(15) {
            println!("  {n:>10}  {k}");
        }
    }
}

fn scan(
    block: &PropertyBlock,
    slots: &[(&blam_tags::iostore::usmap::UsmapProperty, u8, &str)],
    t: &mut Tally,
    wrong_masked: &mut BTreeMap<String, u64>,
    wrong_unmasked: &mut BTreeMap<String, u64>,
    unknown: &mut BTreeMap<String, u64>,
) {
    for e in &block.entries {
        let Some(slot) = e.slot else { continue };
        let Some((prop, _, owner)) = slots.get(slot.index as usize) else { continue };
        if !matches!(prop.ty, PropertyType::Bool) {
            continue;
        }
        t.bools += 1;
        let key = format!("{owner}::{}", prop.name);
        let native = is_native_bool(owner, &prop.name);
        if bool_is_known(owner, &prop.name) {
            t.known += 1;
        } else {
            *unknown.entry(key.clone()).or_default() += 1;
        }
        if slot.zero_masked {
            t.masked += 1;
            if !native {
                t.masked_but_not_native += 1;
                *wrong_masked.entry(key).or_default() += 1;
            }
        } else if native && matches!(e.value, PropValue::Bool(false)) {
            t.native_zero_but_unmasked += 1;
            *wrong_unmasked.entry(key).or_default() += 1;
        }
    }
}
