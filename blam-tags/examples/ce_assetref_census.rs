//! What does every tag's `AssetReference` actually point at? Grouped by tag
//! group, listing the distinct target packages and how many tags share each —
//! i.e. how reusable an existing Blueprint/asset is when authoring a NEW tag.
//!
//! Run: cargo run --release --features iostore --example ce_assetref_census [group]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let only = std::env::args().nth(1);
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // group -> target package -> [tag names]
    let mut by_group: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
    let mut no_ref: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((tagname, group)) = stem.rsplit_once('-') else { continue };
            if let Some(o) = &only {
                if group != o {
                    continue;
                }
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let class = class_for_group(group);
            let names = h.name_map.copy_raw_names();
            let Some(ex) = h.export_map.first() else { continue };
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(b.len());
            if off >= b.len() {
                continue;
            }
            let Ok(props) = read_export_struct(&b[off..end], &names, &usmap, &class) else { continue };
            match props.get("AssetReference") {
                Some(PropValue::Object(i)) if *i < 0 => {
                    let idx = (-*i - 1) as usize;
                    let target = h
                        .import_map
                        .get(idx)
                        .and_then(|im| im.package_import())
                        .and_then(|r| h.imported_package_names.get(r.imported_package_index as usize))
                        .cloned()
                        .unwrap_or_else(|| "<unresolved>".into());
                    by_group
                        .entry(group.to_string())
                        .or_default()
                        .entry(target)
                        .or_default()
                        .push(tagname.to_string());
                }
                _ => {
                    if usmap
                        .flattened_properties(&class)
                        .map(|p| p.iter().any(|q| q.name == "AssetReference"))
                        .unwrap_or(false)
                    {
                        no_ref.entry(group.to_string()).or_default().push(tagname.to_string());
                    }
                }
            }
        }
    }

    for (g, targets) in &by_group {
        let total: usize = targets.values().map(|v| v.len()).sum();
        println!("\n== {g} ({total} tags with AssetReference, {} distinct targets) ==", targets.len());
        let mut ts: Vec<_> = targets.iter().collect();
        ts.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
        for (t, tags) in ts.iter().take(30) {
            let sample: Vec<&str> = tags.iter().take(4).map(|s| s.as_str()).collect();
            println!("  {:>4}  {t}\n         e.g. {}", tags.len(), sample.join(", "));
        }
        if let Some(missing) = no_ref.get(g) {
            println!("  ---- {} tags of this group have NO AssetReference: {}",
                missing.len(),
                missing.iter().take(8).cloned().collect::<Vec<_>>().join(", "));
        }
    }
    for (g, missing) in &no_ref {
        if !by_group.contains_key(g) {
            println!("\n== {g} == ALL {} tags lack AssetReference", missing.len());
        }
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
