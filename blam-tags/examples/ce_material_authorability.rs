//! How much of the material space can be authored without a shader compiler.
//!
//! A `UMaterial` always writes inline shader maps, so a new one is out of reach
//! without Unreal. A `UMaterialInstance` writes them *only* when
//! `bHasStaticPermutationResource` is set — the condition lives in the property
//! block, not the tail (see `MaterialChainTail::read`). An instance without it
//! is a pure property block over a parent that already shipped, which is exactly
//! the kind of package this crate can already write.
//!
//! The share of *shipped* instances that set the flag is not a cap on what can
//! be authored — a new instance chooses whether to override a static parameter.
//! What it measures is how much of the look depends on static permutations, and
//! therefore how much is reachable only by inheriting one.
//!
//! Which makes the second question the important one: can an instance parent to
//! another *instance*? If so, a new instance can sit under one that already has
//! the permutation it wants, inherit the compiled shader, and override only
//! dynamic parameters.
//!
//! Run: cargo run --release --features iostore --example ce_material_authorability

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Cursor;

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{PropValue, read_export_struct};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

const INSTANCE_CLASSES: &[&str] = &[
    "MaterialInstanceConstant",
    "LandscapeMaterialInstanceConstant",
];

/// Pull `ParameterInfo.Name` out of a static-parameter container without
/// assuming which shape it takes.
fn collect_switch_names(v: &PropValue, out: &mut BTreeMap<String, usize>) {
    match v {
        PropValue::Array(a) => {
            for x in a {
                collect_switch_names(x, out);
            }
        }
        PropValue::Struct(fields) => {
            for (k, val) in fields {
                if k == "ParameterInfo" {
                    if let PropValue::Struct(inner) = val {
                        for (ik, iv) in inner {
                            if ik == "Name" {
                                if let PropValue::Name(n) = iv {
                                    *out.entry(n.as_str().to_string()).or_default() += 1;
                                }
                            }
                        }
                    }
                }
                collect_switch_names(val, out);
            }
        }
        _ => {}
    }
}

fn arr_len(v: Option<&PropValue>) -> usize {
    match v {
        Some(PropValue::Array(a)) => a.len(),
        _ => 0,
    }
}

struct Found {
    path: String,
    utoc: std::path::PathBuf,
    class: String,
}

fn main() {
    let usmap = Usmap::meteorite().expect("bundled usmap");

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    for m in std::fs::read_dir(UHT)
        .expect("UHT dump")
        .filter_map(|e| e.ok())
    {
        if !m.path().is_dir() {
            continue;
        }
        let module = m.file_name().to_string_lossy().to_string();
        for sub in ["Public", "Private", "Classes"] {
            let Ok(rd) = std::fs::read_dir(format!("{UHT}/{module}/{sub}")) else {
                continue;
            };
            for f in rd.filter_map(|e| e.ok()) {
                let n = f.file_name().to_string_lossy().to_string();
                let Some(stem) = n.strip_suffix(".h") else {
                    continue;
                };
                by_hash
                    .entry(
                        FPackageObjectIndex::create_script_import(&format!(
                            "/Script/{module}.{stem}"
                        ))
                        .raw_index(),
                    )
                    .or_insert_with(|| stem.to_string());
            }
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
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

    // Pass 1: classify every package, and remember the ids of materials and of
    // material instances so a parent reference can be told apart later.
    let mut material_ids: HashSet<u64> = HashSet::new();
    let mut instance_ids: HashSet<u64> = HashSet::new();
    let mut found: Vec<Found> = Vec::new();
    let mut materials = 0usize;

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            if !e.path.to_ascii_lowercase().ends_with(".uasset") {
                continue;
            }
            let Ok(bytes) = a.read_prefix(&e.path, 64 * 1024) else {
                continue;
            };
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            let leaf = hdr
                .package_name()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let Some(ex) = hdr
                .export_map
                .iter()
                .find(|ex| hdr.name_map.get(ex.object_name).to_ascii_lowercase() == leaf)
                .or_else(|| hdr.export_map.first())
            else {
                continue;
            };
            let Some(class) = by_hash.get(&ex.class_index.raw_index()) else {
                continue;
            };
            let id = FPackageId::from_name(&hdr.package_name()).0;
            if class == "Material" {
                materials += 1;
                material_ids.insert(id);
            } else if INSTANCE_CLASSES.contains(&class.as_str()) {
                instance_ids.insert(id);
                found.push(Found {
                    path: e.path.clone(),
                    utoc: utoc.clone(),
                    class: class.clone(),
                });
            }
        }
    }

    // Pass 2: decode each instance and ask what it needs.
    let (mut static_perm, mut plain, mut decode_fail) = (0usize, 0usize, 0usize);
    let mut param_hist: BTreeMap<&'static str, BTreeMap<usize, usize>> = BTreeMap::new();
    let mut switch_names: BTreeMap<String, usize> = BTreeMap::new();
    let mut switch_props: BTreeMap<String, usize> = BTreeMap::new();
    let (mut over_instance, mut over_material, mut over_neither) = (0usize, 0usize, 0usize);
    let mut static_over_instance = 0usize;
    let mut overriding_none = 0usize;

    let mut open: HashMap<std::path::PathBuf, IoStoreArchive> = HashMap::new();
    for f in &found {
        let a = open
            .entry(f.utoc.clone())
            .or_insert_with(|| IoStoreArchive::open(&f.utoc).expect("reopen"));
        let Ok(bytes) = a.read(&f.path) else {
            decode_fail += 1;
            continue;
        };
        let Ok(hdr) =
            FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        else {
            decode_fail += 1;
            continue;
        };
        let leaf = hdr
            .package_name()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let Some(ex) = hdr
            .export_map
            .iter()
            .find(|ex| hdr.name_map.get(ex.object_name).to_ascii_lowercase() == leaf)
            .or_else(|| hdr.export_map.first())
        else {
            continue;
        };
        let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
        let Some(body) = bytes.get(start..start + ex.cooked_serial_size as usize) else {
            decode_fail += 1;
            continue;
        };
        let names = hdr.name_map.copy_raw_names();
        let Ok(block) = read_export_struct(body, &names, &usmap, &f.class) else {
            decode_fail += 1;
            continue;
        };

        let is_static = matches!(
            block.get("bHasStaticPermutationResource"),
            Some(PropValue::Bool(true))
        );
        if is_static {
            static_perm += 1;
        } else {
            plain += 1;
        }
        let s = arr_len(block.get("ScalarParameterValues"));
        let v = arr_len(block.get("VectorParameterValues"));
        let t = arr_len(block.get("TextureParameterValues"));
        *param_hist
            .entry("scalar")
            .or_default()
            .entry(s)
            .or_default() += 1;
        *param_hist
            .entry("vector")
            .or_default()
            .entry(v)
            .or_default() += 1;
        *param_hist
            .entry("texture")
            .or_default()
            .entry(t)
            .or_default() += 1;
        if s + v + t == 0 {
            overriding_none += 1;
        }
        for key in [
            "StaticParameters",
            "StaticParametersRuntime",
            "StaticSwitchParameters",
        ] {
            if let Some(sv) = block.get(key) {
                *switch_props.entry(key.to_string()).or_default() += 1;
                collect_switch_names(sv, &mut switch_names);
            }
        }

        // What it sits on. The parent is a hard import, so its package is in the
        // imported list; classify by the sets built in pass 1. An instance whose
        // imports include another instance is chained — the case that lets a new
        // instance inherit a compiled permutation instead of needing its own.
        let has_inst = hdr
            .imported_packages
            .iter()
            .any(|p| instance_ids.contains(&p.0));
        let has_mat = hdr
            .imported_packages
            .iter()
            .any(|p| material_ids.contains(&p.0));
        if has_inst {
            over_instance += 1;
            if is_static {
                static_over_instance += 1;
            }
        } else if has_mat {
            over_material += 1;
        } else {
            over_neither += 1;
        }
    }

    let instances = found.len();
    println!("UMaterial (always carries compiled shaders)  : {materials}");
    println!("UMaterialInstance*                           : {instances}");
    println!("   decode failed                             : {decode_fail}");
    println!("   bHasStaticPermutationResource = true      : {static_perm}");
    println!("   plain parameter overrides only            : {plain}");
    println!("   overriding no parameters at all           : {overriding_none}");
    println!("\n== what each instance sits on (by imported package) ==");
    println!("   imports another material INSTANCE         : {over_instance}");
    println!("      of those, also carry their own perm    : {static_over_instance}");
    println!("   imports a base Material only              : {over_material}");
    println!("   imports neither                           : {over_neither}");

    println!("\n== which property carries static parameters ==");
    for (k, c) in &switch_props {
        println!("   {c:>6}  {k}");
    }
    println!(
        "\n== most-used static switch names (top 20 of {}) ==",
        switch_names.len()
    );
    let mut sw: Vec<_> = switch_names.iter().collect();
    sw.sort_by(|a, b| b.1.cmp(a.1));
    for (n, c) in sw.iter().take(20) {
        println!("   {c:>6}  {n}");
    }

    for (kind, hist) in &param_hist {
        let mut top: Vec<_> = hist.iter().collect();
        top.sort_by_key(|(n, _)| **n);
        let shown: Vec<String> = top
            .iter()
            .take(8)
            .map(|(n, c)| format!("{n}:{c}"))
            .collect();
        println!("\n{kind} overrides per instance: {}", shown.join("  "));
        println!(
            "   max on one instance: {}",
            top.last().map(|(n, _)| **n).unwrap_or(0)
        );
    }
}
