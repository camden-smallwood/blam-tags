//! Is a `BlamModelRegionStringTable`'s content derivable from the model tag?
//! Hypothesis: Regions = union of variant region names (in first-seen order),
//! Permutations = union of designated permutation names.
//!
//! Run: cargo run --release --features iostore --example ce_rst_derivation

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::{Model, TagFile};

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
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

    // pass 1: every BlamModelRegionStringTable's content, by package name
    let mut tables: HashMap<String, (Vec<String>, Vec<String>)> = HashMap::new();
    // pass 1b: model tag package -> (string table package, model tag blob path)
    let mut models: Vec<(String, String, String, usize)> = Vec::new(); // (model pkg, rst pkg, ubulk rel, archive idx)
    let archives: Vec<IoStoreArchive> = utocs
        .iter()
        .filter_map(|u| IoStoreArchive::open(u).ok())
        .collect();

    for (ai, a) in archives.iter().enumerate() {
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") {
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

            // BlamModelRegionStringTable exports have this class hash
            if ex.class_index
                == blam_tags::iostore::ue_types::FPackageObjectIndex::create_script_import(
                    "/Script/BlamSynchronization.BlamModelRegionStringTable",
                )
            {
                if let Ok(p) = read_export_struct(&ua[off..end], &names, &usmap, "BlamModelRegionStringTable") {
                    let g = |k: &str| -> Vec<String> {
                        match p.get(k) {
                            Some(PropValue::Array(v)) => v
                                .iter()
                                .filter_map(|x| match x {
                                    PropValue::Name(n) => Some(n.as_str().to_string()),
                                    _ => None,
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    };
                    tables.insert(h.package_name().to_ascii_lowercase(), (g("Regions"), g("Permutations")));
                }
                continue;
            }

            if !lower.ends_with("-model.uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let Ok(props) = read_export_struct(&ua[off..end], &names, &usmap, "BlamModelTagDataAsset")
            else {
                continue;
            };
            if let Some(PropValue::Object(i)) = props.get("ModelRegionStringTable") {
                if *i < 0 {
                    if let Some(t) = h
                        .import_map
                        .get((-*i - 1) as usize)
                        .and_then(|im| im.package_import())
                        .and_then(|r| h.imported_package_names.get(r.imported_package_index as usize))
                    {
                        models.push((
                            h.package_name(),
                            t.to_ascii_lowercase(),
                            e.path.replace(".uasset", ".ubulk"),
                            ai,
                        ));
                    }
                }
            }
        }
    }
    eprintln!("{} string tables, {} models referencing one", tables.len(), models.len());

    let (mut set_match, mut order_match, mut miss, mut no_table) = (0, 0, 0, 0);
    let mut shared = 0usize;
    let mut samples: Vec<String> = Vec::new();
    let mut usage: BTreeMap<String, usize> = BTreeMap::new();

    for (mpkg, rst, ubulk, ai) in &models {
        *usage.entry(rst.clone()).or_default() += 1;
        let Some((regions, perms)) = tables.get(rst) else {
            no_table += 1;
            continue;
        };
        let Ok(blob) = archives[*ai].read(ubulk) else { continue };
        let Ok(tag) = TagFile::read_from_bytes(&blob) else { continue };
        let Ok(model) = Model::from_tag(&tag) else { continue };

        let mut d_regions: Vec<String> = Vec::new();
        let mut d_perms: Vec<String> = Vec::new();
        for v in &model.variants {
            for r in &v.regions {
                if !r.name.is_empty() && !d_regions.contains(&r.name) {
                    d_regions.push(r.name.clone());
                }
                for p in &r.permutation_names {
                    if !p.is_empty() && !d_perms.contains(p) {
                        d_perms.push(p.clone());
                    }
                }
            }
        }
        let same_set = {
            let a: std::collections::BTreeSet<_> = regions.iter().collect();
            let b: std::collections::BTreeSet<_> = d_regions.iter().collect();
            let c: std::collections::BTreeSet<_> = perms.iter().collect();
            let d: std::collections::BTreeSet<_> = d_perms.iter().collect();
            a == b && c == d
        };
        if *regions == d_regions && *perms == d_perms {
            order_match += 1;
        } else if same_set {
            set_match += 1;
        } else {
            miss += 1;
            if usage[rst] > 1 {
                shared += 1;
            }
            if samples.len() < 6 {
                samples.push(format!(
                    "{mpkg}\n    table {rst} (used by {} models)\n    table regions  : {:?}\n    derived regions: {:?}\n    table perms {} / derived perms {}",
                    usage[rst], regions, d_regions, perms.len(), d_perms.len()
                ));
            }
        }
    }
    println!("\nRegions+Permutations derivation from the model tag's variants:");
    println!("  exact incl. order : {order_match}");
    println!("  same set, reordered: {set_match}");
    println!("  mismatch           : {miss}  (of which {shared} use a SHARED table)");
    println!("  table not found    : {no_table}");
    println!("\n-- samples --");
    for s in &samples {
        println!("{s}\n");
    }
}
