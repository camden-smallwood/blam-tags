//! Gate: can every `FUnversionedHeader` in the shipped corpus be *regenerated*
//! byte-exactly from the property set alone?
//!
//! This is the first half of the write path, and the one that decides whether a
//! writer must retain original bytes or can rebuild them.
//! [`emit_header`] is a literal port of `FUnversionedHeaderBuilder`
//! (UnversionedPropertySerialization.cpp:795); this runs it against every export
//! of every package and compares against what the cooker actually wrote.
//!
//! Two exports are expected to be skipped: `RigVM` and `RigHierarchy` override
//! `Serialize` without calling `Super`, so they carry no property block at all
//! and their first bytes are not a header.
//!
//! Run: `ce_header_roundtrip`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::block::{emit_header, parse_header};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Classes whose `Serialize` never calls `Super`, so they have no header.
const NO_PROPERTY_BLOCK: &[&str] = &["RigVM", "RigHierarchy"];

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
    match ScriptObjects::load(format!("{PAKS}/global.utoc")) {
        Ok(so) => {
            for e in so.entries() {
                if let Some(p) = so.resolve(e.global_index.raw_index()) {
                    by_hash.insert(e.global_index.raw_index(), p.to_string());
                }
            }
        }
        Err(e) => {
            eprintln!("no script-object table ({e:#}); cannot resolve classes");
            std::process::exit(2);
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

    let (mut total, mut same, mut skipped) = (0usize, 0usize, 0usize);
    let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
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
            for ex in &h.export_map {
                // Only exports whose class the `.usmap` describes: a
                // Blueprint-generated class's schema comes from its own FField
                // chain, and the schema length is what makes an empty block's
                // header reproducible.
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if NO_PROPERTY_BLOCK.contains(&short) {
                    skipped += 1;
                    continue;
                }
                let Some(flat) = usmap.flattened_properties(short) else { continue };
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let Ok((header, used)) = parse_header(body) else { continue };
                total += 1;
                let regen = emit_header(&header, flat.len());
                if regen == body[..used] {
                    same += 1;
                } else {
                    *by_class.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}\n    orig  {:02x?}\n    regen {:02x?}",
                            h.package_name(),
                            &body[..used],
                            regen
                        ));
                    }
                }
            }
        }
    }

    println!("headers examined     {total}");
    println!("regenerated exactly  {same} ({:.4}%)", 100.0 * same as f64 / total.max(1) as f64);
    println!("differ               {}", total - same);
    println!("skipped (no block)   {skipped}  {NO_PROPERTY_BLOCK:?}");
    if same != total {
        println!("\nby class:");
        let mut v: Vec<_> = by_class.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(15) {
            println!("  {n:>7}  {c}");
        }
        for s in &samples {
            println!("\n{s}");
        }
        std::process::exit(1);
    }
}
