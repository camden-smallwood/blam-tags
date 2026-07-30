//! Sweep EVERY cooked tag package in Campaign Evolved and tabulate, per tag
//! group, what the `.uasset` wrapper actually carries: export class, export
//! count, which properties are serialized, and what kinds of packages the
//! `AssetReference` / `CookedAssetsReferencedByTag` entries point at.
//!
//! This is the authoring spec for "what must a NEW tag's .uasset contain".
//!
//! Run: cargo run --release --features iostore --example ce_tag_pkg_sweep

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::read_export_struct;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct GroupStat {
    count: usize,
    class_hashes: BTreeSet<String>,
    export_counts: BTreeSet<usize>,
    /// property name -> how many tags of this group serialize it
    props: BTreeMap<String, usize>,
    /// distinct non-/Game/Tags/ package prefixes seen in AssetReference
    asset_ref_targets: BTreeMap<String, usize>,
    with_ubulk: usize,
    no_ubulk: usize,
    min_header: u32,
    max_header: u32,
    max_imports: usize,
    /// imports that no dependency-bundle entry references
    total_imports: usize,
    total_dep_imports: usize,
    null_imports: usize,
}

fn main() {
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("parse usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut stats: BTreeMap<String, GroupStat> = BTreeMap::new();
    let mut total = 0usize;
    let mut decode_fail = 0usize;

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let ubulks: BTreeSet<String> = a
            .entries()
            .iter()
            .filter(|e| e.path.to_ascii_lowercase().ends_with(".ubulk"))
            .map(|e| e.path.to_ascii_lowercase())
            .collect();
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let group = group.to_string();
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
            else {
                continue;
            };
            total += 1;
            let st = stats.entry(group.clone()).or_default();
            st.count += 1;
            if st.min_header == 0 || h.summary.header_size < st.min_header {
                st.min_header = h.summary.header_size;
            }
            st.max_header = st.max_header.max(h.summary.header_size);
            st.max_imports = st.max_imports.max(h.import_map.len());
            st.total_imports += h.import_map.len();
            st.total_dep_imports += h.dependency_bundle_entries.len();
            st.null_imports += h.import_map.iter().filter(|i| i.is_null()).count();
            st.export_counts.insert(h.export_map.len());
            if ubulks.contains(&lower.replace(".uasset", ".ubulk")) {
                st.with_ubulk += 1;
            } else {
                st.no_ubulk += 1;
            }
            for ex in &h.export_map {
                st.class_hashes.insert(format!("{:X}", ex.class_index.raw_index()));
            }

            let class = class_for_group(&group);
            let names = h.name_map.copy_raw_names();
            let start = h.summary.header_size as usize;
            if let Some(ex) = h.export_map.first() {
                let off = start + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(bytes.len());
                if off < bytes.len() {
                    match read_export_struct(&bytes[off..end], &names, &usmap, &class) {
                        Ok(props) => {
                            for (k, v) in &props {
                                *st.props.entry(k.to_string()).or_default() += 1;
                                if k == "AssetReference" {
                                    if let Some(idx) = obj_index(v) {
                                        if let Some(p) = import_pkg(&h, idx) {
                                            *st.asset_ref_targets
                                                .entry(prefix_of(&p))
                                                .or_default() += 1;
                                        }
                                    }
                                }
                            }
                        }
                        Err(_) => decode_fail += 1,
                    }
                }
            }
        }
    }

    println!("swept {total} tag packages across {} groups ({decode_fail} decode failures)\n", stats.len());
    println!("{:<42} {:>5} {:>6} {:>7} {:>7} {:>6} {:>6}  properties / assetref targets",
        "group (class)", "count", "ubulk", "hdr min", "hdr max", "imp", "dep");
    for (g, s) in &stats {
        let class = class_for_group(g);
        let props: Vec<String> = s
            .props
            .iter()
            .map(|(k, n)| if *n == s.count { k.clone() } else { format!("{k}({n})") })
            .collect();
        println!(
            "{:<42} {:>5} {:>6} {:>7} {:>7} {:>6} {:>6}  {}",
            format!("{g} ({class})"),
            s.count,
            format!("{}/{}", s.with_ubulk, s.count),
            s.min_header,
            s.max_header,
            s.max_imports,
            s.total_dep_imports,
            props.join(", ")
        );
        if !s.asset_ref_targets.is_empty() {
            println!("{:>44}AssetReference -> {:?}", "", s.asset_ref_targets);
        }
        if s.export_counts.len() > 1 || !s.export_counts.contains(&1) {
            println!("{:>44}export counts: {:?}", "", s.export_counts);
        }
        if s.class_hashes.len() > 1 {
            println!("{:>44}class hashes: {:?}", "", s.class_hashes);
        }
        if s.null_imports > 0 {
            println!("{:>44}null imports: {} of {}", "", s.null_imports, s.total_imports);
        }
    }
}

fn obj_index(v: &blam_tags::iostore::unversioned::PropValue) -> Option<i32> {
    use blam_tags::iostore::unversioned::PropValue;
    match v {
        PropValue::Object(i) => Some(*i),
        _ => None,
    }
}

fn import_pkg(h: &FZenPackageHeader, package_index: i32) -> Option<String> {
    if package_index >= 0 {
        return None;
    }
    let i = (-package_index - 1) as usize;
    let im = h.import_map.get(i)?;
    let r = im.package_import()?;
    h.imported_package_names
        .get(r.imported_package_index as usize)
        .cloned()
}

fn prefix_of(p: &str) -> String {
    let parts: Vec<&str> = p.split('/').collect();
    if parts.len() > 3 {
        parts[..3].join("/") + "/…"
    } else {
        p.to_string()
    }
}

/// `biped` -> `BlamBipedTagDataAsset`
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
