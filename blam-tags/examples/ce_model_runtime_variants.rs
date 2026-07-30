//! Is `RuntimeVariants` on `BlamModelTagDataAsset` derivable from the model
//! tag's own `variants` block? Tests the hypothesis
//!   RuntimeVariants[i] = { VariantName: variants[i].name,
//!                          Permutations: { region.name -> region.permutations[0] } }
//! across every shipped `model` tag, and also reports what
//! `ModelRegionStringTable` / `RegionTable` / `Variants` actually carry.
//!
//! Run: cargo run --release --features iostore --example ce_model_runtime_variants

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{PropValue, read_export_struct};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::{Model, TagFile};

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    let (mut n, mut exact, mut order_only, mut mismatch) = (0, 0, 0, 0);
    let mut no_runtime = 0usize;
    let mut has_variants_prop = 0usize;
    let mut has_regiontable = 0usize;
    let mut regiontable_samples: Vec<String> = Vec::new();
    let mut rst_targets: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut no_rst: Vec<String> = Vec::new();
    let mut samples: Vec<String> = Vec::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else {
            continue;
        };
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with("-model.uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.first() else {
                continue;
            };
            let names = h.name_map.copy_raw_names();
            let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = (off + ex.cooked_serial_size as usize).min(ua.len());
            let Ok(props) =
                read_export_struct(&ua[off..end], &names, &usmap, "BlamModelTagDataAsset")
            else {
                continue;
            };
            let Ok(blob) = a.read(&e.path.replace(".uasset", ".ubulk")) else {
                continue;
            };
            let Ok(tag) = TagFile::read_from_bytes(&blob) else {
                continue;
            };
            let Ok(model) = Model::from_tag(&tag) else {
                continue;
            };
            n += 1;
            let pkg = h.package_name();

            if props.contains_key("Variants") {
                has_variants_prop += 1;
            }
            if let Some(PropValue::Array(rt)) = props.get("RegionTable") {
                has_regiontable += 1;
                if regiontable_samples.len() < 5 {
                    regiontable_samples.push(format!(
                        "{pkg}: {:?}",
                        rt.iter().take(8).collect::<Vec<_>>()
                    ));
                }
            }
            match props.get("ModelRegionStringTable") {
                Some(PropValue::Object(i)) if *i < 0 => {
                    let t = h
                        .import_map
                        .get((-*i - 1) as usize)
                        .and_then(|im| im.package_import())
                        .and_then(|r| {
                            h.imported_package_names
                                .get(r.imported_package_index as usize)
                        })
                        .cloned()
                        .unwrap_or_default();
                    rst_targets.entry(t).or_default().push(pkg.clone());
                }
                _ => no_rst.push(pkg.clone()),
            }

            // --- RuntimeVariants derivation test ---
            let Some(PropValue::Array(rt)) = props.get("RuntimeVariants") else {
                no_runtime += 1;
                continue;
            };
            let cooked: Vec<(String, BTreeMap<String, String>)> = rt
                .iter()
                .filter_map(|v| {
                    let s = v.as_struct()?;
                    let name = match s.get("VariantName")? {
                        PropValue::Name(x) => x.as_str().to_string(),
                        _ => return None,
                    };
                    let mut perms = BTreeMap::new();
                    if let Some(PropValue::Map(m)) = s.get("Permutations") {
                        for (k, val) in m {
                            if let (PropValue::Name(k), PropValue::Name(v)) = (k, val) {
                                perms.insert(k.as_str().to_string(), v.as_str().to_string());
                            }
                        }
                    }
                    Some((name, perms))
                })
                .collect();

            let derived: Vec<(String, BTreeMap<String, String>)> = model
                .variants
                .iter()
                .map(|v| {
                    let mut perms = BTreeMap::new();
                    for r in &v.regions {
                        // Two cases the reader would otherwise conflate. A
                        // region with *zero* permutations is omitted from the
                        // map; a region whose first permutation has an empty
                        // name is kept and written as "None". Treating both as
                        // "None" costs one tag, and skipping both costs 23.
                        if r.permutation_names.is_empty() {
                            continue;
                        }
                        let p = r.permutation_names[0].clone();
                        perms.insert(
                            r.name.clone(),
                            if p.is_empty() { "None".to_string() } else { p },
                        );
                    }
                    (v.name.clone(), perms)
                })
                .collect();

            if cooked == derived {
                exact += 1;
            } else {
                let cs: BTreeSet<_> = cooked.iter().collect();
                let ds: BTreeSet<_> = derived.iter().collect();
                if cs == ds {
                    order_only += 1;
                } else {
                    mismatch += 1;
                    if samples.len() < 6 {
                        let only_c: Vec<_> = cs.difference(&ds).take(3).collect();
                        let only_d: Vec<_> = ds.difference(&cs).take(3).collect();
                        samples.push(format!(
                            "{pkg}\n    cooked {} entries, derived {} entries\n    only cooked: {only_c:?}\n    only derived: {only_d:?}",
                            cooked.len(), derived.len()
                        ));
                    }
                }
            }
        }
    }

    println!("{n} model tags");
    println!("  RuntimeVariants absent      : {no_runtime}");
    println!("  derivation exact (+order)   : {exact}");
    println!("  same set, different order   : {order_only}");
    println!("  genuine mismatch            : {mismatch}");
    println!("  serialize `Variants` too    : {has_variants_prop}");
    println!("  serialize `RegionTable`     : {has_regiontable}");
    for s in &regiontable_samples {
        println!("      {s}");
    }
    println!(
        "\n  ModelRegionStringTable: {} distinct targets, {} tags with none",
        rst_targets.len(),
        no_rst.len()
    );
    let mut ts: Vec<_> = rst_targets.iter().collect();
    ts.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (t, tags) in ts.iter().take(12) {
        println!("      {:>4}  {t}\n            e.g. {}", tags.len(), tags[0]);
    }
    println!(
        "      (tags with NO string table, e.g.) {:?}",
        no_rst.iter().take(5).collect::<Vec<_>>()
    );

    println!("\n-- mismatch samples --");
    for s in &samples {
        println!("{s}\n");
    }
}
