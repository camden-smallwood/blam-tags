//! Structural invariants + inferability checks for every remaining
//! property-bearing tag class.
//!
//!  * export count / bulk-map entry count / bulk flags across all 12k tags
//!  * how many import-map slots are Null, and how many imports no dependency
//!    bundle entry references (i.e. are they load-bearing or vestigial?)
//!  * `sound` / `sound_looping` / `sound_combiner` AssetReference vs tag path
//!  * `cinematic` AssetReference vs tag path
//!  * `damage_response_definition` AssetReference targets
//!  * `effect` bSpawnPerInstance
//!  * `player_model_customization_globals` maps
//!
//! Run: cargo run --release --features iostore --example ce_tag_invariants

use std::collections::{BTreeMap, BTreeSet};
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
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut export_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut bulk_counts: BTreeMap<usize, usize> = BTreeMap::new();
    let mut bulk_flags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut obj_flags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut pkg_flags: BTreeMap<u32, usize> = BTreeMap::new();
    let mut n = 0usize;

    // import accounting
    let mut imports_total = 0usize;
    let mut imports_null = 0usize;
    let mut imports_in_dep = 0usize;
    let mut imports_used_by_props = 0usize;
    let mut vestigial_targets: BTreeMap<String, usize> = BTreeMap::new();

    // per-group probes
    let mut sound_same_leaf = 0usize;
    let mut sound_total = 0usize;
    let mut sound_samples: Vec<String> = Vec::new();
    let mut cine_same_leaf = 0usize;
    let mut cine_total = 0usize;
    let mut cine_samples: Vec<String> = Vec::new();
    let mut spawn_per_instance: Vec<String> = Vec::new();
    let mut drd_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut pmcg: Vec<String> = Vec::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((tagleaf, group)) = stem.rsplit_once('-') else { continue };
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                continue;
            };
            n += 1;
            *export_counts.entry(h.export_map.len()).or_default() += 1;
            *bulk_counts.entry(h.bulk_data.len()).or_default() += 1;
            for b in &h.bulk_data {
                *bulk_flags.entry(b.flags).or_default() += 1;
            }
            *pkg_flags.entry(h.summary.package_flags).or_default() += 1;
            for ex in &h.export_map {
                *obj_flags.entry(ex.object_flags).or_default() += 1;
            }

            let dep: BTreeSet<i32> = h
                .dependency_bundle_entries
                .iter()
                .map(|d| d.local_import_or_export_index.index)
                .collect();
            imports_total += h.import_map.len();
            imports_null += h.import_map.iter().filter(|i| i.is_null()).count();

            let Some(ex) = h.export_map.first() else { continue };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(ua.len());
            if off >= ua.len() {
                continue;
            }
            let class = class_for_group(group);
            let props = read_export_struct(&ua[off..end], &names, &usmap, &class)
                .or_else(|_| read_export_struct(&ua[off..end], &names, &usmap, "BlamTagDataAssetBase"))
                .unwrap_or_default();

            // which imports do the properties actually name?
            let mut used: BTreeSet<i32> = BTreeSet::new();
            collect_objs(&props, &mut used);
            imports_used_by_props += used.len();
            for i in 0..h.import_map.len() as i32 {
                let pi = -i - 1;
                if dep.contains(&pi) {
                    imports_in_dep += 1;
                } else if !h.import_map[i as usize].is_null() && !used.contains(&pi) {
                    if let Some(t) = h
                        .import_map[i as usize]
                        .package_import()
                        .and_then(|r| h.imported_package_names.get(r.imported_package_index as usize))
                    {
                        *vestigial_targets
                            .entry(t.split('/').take(3).collect::<Vec<_>>().join("/"))
                            .or_default() += 1;
                    } else {
                        *vestigial_targets.entry("<script import>".into()).or_default() += 1;
                    }
                }
            }

            let target = |v: &PropValue| -> Option<String> {
                let PropValue::Object(i) = v else { return None };
                if *i >= 0 {
                    return None;
                }
                h.import_map
                    .get((-*i - 1) as usize)
                    .and_then(|im| im.package_import())
                    .and_then(|r| h.imported_package_names.get(r.imported_package_index as usize))
                    .cloned()
            };

            match group {
                "sound" | "sound_looping" | "sound_combiner" => {
                    if let Some(t) = props.get("AssetReference").and_then(&target) {
                        sound_total += 1;
                        let leaf = t.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
                        if leaf == tagleaf || leaf == format!("{tagleaf}-sound") {
                            sound_same_leaf += 1;
                        } else if sound_samples.len() < 8 {
                            sound_samples.push(format!("    tag {tagleaf:<44} -> {t}"));
                        }
                    }
                }
                "cinematic" => {
                    if let Some(t) = props.get("AssetReference").and_then(&target) {
                        cine_total += 1;
                        let leaf = t.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
                        if leaf.contains(tagleaf) || tagleaf.contains(leaf.trim_start_matches("ls_")) {
                            cine_same_leaf += 1;
                        } else if cine_samples.len() < 6 {
                            cine_samples.push(format!("    tag {tagleaf:<40} -> {t}"));
                        }
                    }
                }
                "damage_response_definition" => {
                    if let Some(t) = props.get("AssetReference").and_then(&target) {
                        drd_targets.entry(t).or_default().push(tagleaf.to_string());
                    }
                }
                "effect" => {
                    if let Some(PropValue::Bool(b)) = props.get("bSpawnPerInstance") {
                        spawn_per_instance.push(format!("    {b}  {}", h.package_name()));
                    }
                }
                "player_model_customization_globals" => {
                    for (k, v) in &props {
                        pmcg.push(format!("    {k} = {}", summarize(v, &h)));
                    }
                }
                _ => {}
            }
        }
    }

    println!("== structural invariants over {n} tag packages ==");
    println!("export counts        : {export_counts:?}");
    println!("bulk map entry counts: {bulk_counts:?}");
    println!("bulk flags           : {bulk_flags:?}");
    println!("object_flags         : {:?}", obj_flags.iter().map(|(k, v)| (format!("0x{k:x}"), v)).collect::<Vec<_>>());
    println!("package_flags        : {:?}", pkg_flags.iter().map(|(k, v)| (format!("0x{k:x}"), v)).collect::<Vec<_>>());
    println!("\n== import accounting ==");
    println!("total import slots            : {imports_total}");
    println!("  Null slots                  : {imports_null}");
    println!("  named by a dependency bundle: {imports_in_dep}");
    println!("  named by a property         : {imports_used_by_props}");
    println!("  neither (vestigial), by root:");
    let mut vs: Vec<_> = vestigial_targets.iter().collect();
    vs.sort_by_key(|(_, v)| std::cmp::Reverse(**v));
    for (k, v) in vs.iter().take(12) {
        println!("      {v:>7}  {k}");
    }

    println!("\n== sound/sound_looping/sound_combiner AssetReference ==");
    println!("{sound_same_leaf}/{sound_total} target package leaf == tag leaf name");
    for s in &sound_samples {
        println!("{s}");
    }
    println!("\n== cinematic AssetReference ==");
    println!("{cine_same_leaf}/{cine_total} leaf-name correlated");
    for s in &cine_samples {
        println!("{s}");
    }
    println!("\n== damage_response_definition AssetReference ==");
    for (t, tags) in &drd_targets {
        println!("    {:>3}  {t}\n         {:?}", tags.len(), tags);
    }
    println!("\n== effect bSpawnPerInstance ({}) ==", spawn_per_instance.len());
    for s in &spawn_per_instance {
        println!("{s}");
    }
    println!("\n== player_model_customization_globals ==");
    for s in &pmcg {
        println!("{s}");
    }
}

fn collect_objs(props: &BTreeMap<String, PropValue>, out: &mut BTreeSet<i32>) {
    fn walk(v: &PropValue, out: &mut BTreeSet<i32>) {
        match v {
            PropValue::Object(i) => {
                out.insert(*i);
            }
            PropValue::Array(a) => a.iter().for_each(|x| walk(x, out)),
            PropValue::Map(m) => m.iter().for_each(|(k, v)| {
                walk(k, out);
                walk(v, out)
            }),
            PropValue::Struct(s) => s.values().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    props.values().for_each(|v| walk(v, out));
}

fn summarize(v: &PropValue, _h: &FZenPackageHeader) -> String {
    match v {
        PropValue::Map(m) => format!(
            "Map[{}] e.g. {:?}",
            m.len(),
            m.iter().take(3).collect::<Vec<_>>()
        ),
        other => format!("{other:?}").chars().take(300).collect(),
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
