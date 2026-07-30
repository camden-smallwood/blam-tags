//! Decisive test of the authoring rule: is `CookedAssetsReferencedByTag` in a
//! tag's `.uasset` exactly the set of tag references inside its `.ubulk` blob?
//!
//! For each shipped tag: parse the blob, collect every non-null `tag_reference`
//! as `/Game/Tags/<path>-<group>`, and diff against the cooked array. Reports
//! how many tags match exactly, plus the shape of any deltas.
//!
//! Run: cargo run --release --features iostore --example ce_cooked_refs_vs_blob [limit]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::api::TagStruct;
use blam_tags::fields::{TagFieldData, TagFieldType};
use blam_tags::TagFile;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let limit: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut exact = 0usize;
    let mut checked = 0usize;
    let mut blob_parse_fail = 0usize;
    let mut only_in_cooked: BTreeMap<String, usize> = BTreeMap::new();
    let mut deltas: Vec<String> = Vec::new();
    let mut missing_total = 0usize;
    let mut extra_total = 0usize;

    'outer: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let ubulk_path = e.path.replace(".uasset", ".ubulk");
            if !a.contains(&ubulk_path) {
                continue;
            }
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.first() else { continue };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(ua.len());
            if off >= ua.len() {
                continue;
            }
            let class = class_for_group(group);
            let Ok(props) = read_export_struct(&ua[off..end], &names, &usmap, &class) else {
                continue;
            };

            // cooked set
            let mut cooked: BTreeSet<String> = BTreeSet::new();
            if let Some(PropValue::Array(items)) = props.get("CookedAssetsReferencedByTag") {
                for it in items {
                    if let PropValue::Object(i) = it {
                        if *i < 0 {
                            if let Some(p) = h
                                .import_map
                                .get((-*i - 1) as usize)
                                .and_then(|im| im.package_import())
                                .and_then(|r| {
                                    h.imported_package_names.get(r.imported_package_index as usize)
                                })
                            {
                                cooked.insert(strip_group(&p.to_ascii_lowercase()));
                            }
                        }
                    }
                }
            }

            // blob set
            let Ok(blob) = a.read(&ubulk_path) else { continue };
            let tag = match TagFile::read_from_bytes(&blob) {
                Ok(t) => t,
                Err(_) => {
                    blob_parse_fail += 1;
                    continue;
                }
            };
            let mut blob_refs: BTreeSet<String> = BTreeSet::new();
            collect_refs(&tag.root(), &mut blob_refs);

            checked += 1;
            let missing: Vec<&String> = blob_refs.difference(&cooked).collect();
            let extra: Vec<&String> = cooked.difference(&blob_refs).collect();
            if missing.is_empty() && extra.is_empty() {
                exact += 1;
            } else {
                missing_total += missing.len();
                extra_total += extra.len();
                for x in &extra {
                    let root = x.split('/').take(3).collect::<Vec<_>>().join("/");
                    *only_in_cooked.entry(root).or_default() += 1;
                }
                if deltas.len() < 12 {
                    deltas.push(format!(
                        "{}\n    only in blob ({}): {:?}\n    only in cooked ({}): {:?}",
                        h.package_name(),
                        missing.len(),
                        missing.iter().take(5).collect::<Vec<_>>(),
                        extra.len(),
                        extra.iter().take(5).collect::<Vec<_>>()
                    ));
                }
            }
            if checked >= limit {
                break 'outer;
            }
        }
    }

    println!("checked {checked} tags ({blob_parse_fail} blob parse failures)");
    println!("exact match: {exact} ({:.1}%)", exact as f64 * 100.0 / checked.max(1) as f64);
    println!("total refs only-in-blob: {missing_total}, only-in-cooked: {extra_total}");
    println!("\n-- only-in-cooked package roots --");
    for (k, v) in &only_in_cooked {
        println!("{v:>7}  {k}");
    }
    println!("\n-- sample deltas --");
    for d in &deltas {
        println!("{d}\n");
    }
}

fn collect_refs(s: &TagStruct, out: &mut BTreeSet<String>) {
    for f in s.fields_all() {
        match f.field_type() {
            TagFieldType::TagReference => {
                if let Some(TagFieldData::TagReference(r)) = f.value() {
                    if let Some((_, path)) = r.group_tag_and_name {
                        let p = path.replace('\u{0}', "");
                        let p = p.trim().replace('\\', "/").to_ascii_lowercase();
                        if !p.is_empty() {
                            out.insert(format!("/game/tags/{p}"));
                        }
                    }
                }
            }
            TagFieldType::Struct => {
                if let Some(sub) = f.as_struct() { collect_refs(&sub, out); }
            }
            TagFieldType::Block => {
                if let Some(b) = f.as_block() { for el in b.iter() { collect_refs(&el, out); } }
            }
            TagFieldType::Array => {
                if let Some(a) = f.as_array() { for el in a.iter() { collect_refs(&el, out); } }
            }
            _ => {}
        }
    }
}

/// `/game/tags/foo/bar-biped` -> `/game/tags/foo/bar`
fn strip_group(p: &str) -> String {
    match p.rsplit_once('-') {
        Some((head, _)) if !head.is_empty() && !head.ends_with('/') => head.to_string(),
        _ => p.to_string(),
    }
}

fn class_for_group(group: &str) -> String {
    let mut out = String::from("Blam");
    for part in group.split('_') {
        let mut c = part.chars();
        if let Some(f) = c.next() {
            out.push(f.to_ascii_uppercase());
            out.push_str(c.as_str());
        }
    }
    out.push_str("TagDataAsset");
    out
}
