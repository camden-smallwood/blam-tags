//! What on the Unreal side is derivable, and what has to be authored?
//!
//! The tag half has derivation rules verified over all 12,291 tags. The Unreal
//! half has none, which matters because authoring an object means producing both.
//! This measures the three links that stand between a tag and its presentation:
//!
//!  1. `BlamMeshSynchronizationDataAsset::ModelTag` — is the DA↔model-tag
//!     relationship one-to-one, and is either name derivable from the other?
//!  2. `BlamModelTagDataAsset::ModelRegionStringTable` — derivable, or authored?
//!     If authored, is there a shared default a new tag could point at?
//!  3. The tag path → Unreal content path convention. `objects\weapons\rifle\
//!     shotgun` lives beside `/Game/Weapons/Rifle/shotgun`. Is that a rule or a
//!     habit? It decides whether a tool can *place* new assets or must ask.
//!
//! Run: cargo run --release --features iostore --example ce_unreal_side_rules

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let usmap = Usmap::meteorite().expect("usmap");

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    for m in std::fs::read_dir(UHT).expect("UHT").filter_map(|e| e.ok()) {
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

    // DA package -> the model-tag package it names, and the reverse.
    let mut da_to_model: BTreeMap<String, String> = BTreeMap::new();
    let mut model_to_das: BTreeMap<String, Vec<String>> = BTreeMap::new();
    // Every tag package path, for the path-convention test.
    let mut tag_dirs: BTreeSet<String> = BTreeSet::new();
    let mut content_dirs: BTreeSet<String> = BTreeSet::new();
    let (mut n_da, mut n_no_modeltag, mut n_decode_fail) = (0usize, 0usize, 0usize);

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.ends_with(".uasset") {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            let pkg = hdr.package_name();
            let lpkg = pkg.to_ascii_lowercase();
            if let Some(d) = lpkg.rsplit_once('/').map(|(d, _)| d.to_string()) {
                if lpkg.starts_with("/game/tags/") {
                    tag_dirs.insert(d);
                } else if lpkg.starts_with("/game/") {
                    content_dirs.insert(d);
                }
            }

            // The package's class is the class of the export *named after the
            // package*, not export[0]. Keyed on the first export, a mesh-sync
            // data asset is invisible and this reports zero of them.
            let leaf = pkg
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            let Some(ex) = hdr
                .export_map
                .iter()
                .find(|x| hdr.name_map.get(x.object_name).to_ascii_lowercase() == leaf)
                .or_else(|| hdr.export_map.first())
            else {
                continue;
            };
            let Some(class) = by_hash.get(&ex.class_index.raw_index()) else {
                continue;
            };
            if class != "BlamMeshSynchronizationDataAsset" {
                continue;
            }
            n_da += 1;
            let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let Some(body) = bytes.get(start..start + ex.cooked_serial_size as usize) else {
                continue;
            };
            let names = hdr.name_map.copy_raw_names();
            let block = match read_export_struct(body, &names, &usmap, class) {
                Ok(b) => b,
                Err(e) => {
                    n_decode_fail += 1;
                    eprintln!("DA decode fail {pkg}: {e:?}");
                    continue;
                }
            };
            // `ModelTag` is a hard object reference; the package it lives in is
            // the one imported name that looks like a model tag.
            if !matches!(block.get("ModelTag"), Some(PropValue::Object(i)) if *i != 0) {
                n_no_modeltag += 1;
                continue;
            }
            // `ModelTag` is a hard *package* import, so the model tag's package
            // name is in `imported_package_names` -- not in this package's own
            // name map, which holds only names its own properties use.
            let model = hdr
                .imported_package_names
                .iter()
                .find(|n| n.to_ascii_lowercase().ends_with("-model"))
                .cloned()
                .unwrap_or_default();
            if model.is_empty() {
                continue;
            }
            da_to_model.insert(pkg.clone(), model.clone());
            model_to_das.entry(model).or_default().push(pkg);
        }
    }

    println!("== 1. mesh-sync DataAsset -> model tag ==");
    println!("   DA packages seen               : {n_da}  (decode failed {n_decode_fail}, no ModelTag {n_no_modeltag})");
    println!("   data assets naming a model tag : {}", da_to_model.len());
    println!("   distinct model tags named      : {}", model_to_das.len());
    let shared: Vec<_> = model_to_das.iter().filter(|(_, v)| v.len() > 1).collect();
    println!("   model tags named by >1 DA      : {}", shared.len());
    for (m, das) in shared.iter().take(5) {
        println!("      {m}  <- {} DAs", das.len());
    }
    // Is the DA's leaf derivable from the model tag's leaf?
    let mut leaf_rule = 0usize;
    for (da, model) in &da_to_model {
        let dl = da.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
        let ml = model
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
            .replace("-model", "");
        if dl.contains(&ml)
            || ml.contains(
                dl.trim_start_matches("da_")
                    .trim_end_matches("_meshsynchronization"),
            )
        {
            leaf_rule += 1;
        }
    }
    println!(
        "   DA leaf name relates to model leaf: {}/{}",
        leaf_rule,
        da_to_model.len()
    );

    println!("\n== 2. tag path -> content path convention ==");
    // For each tag dir under /game/tags/, does a content dir exist at the same
    // relative path with the /tags prefix dropped?
    let (mut exact, mut none) = (0usize, 0usize);
    let mut examples: Vec<String> = Vec::new();
    for d in &tag_dirs {
        // Tags are rooted under an extra `objects/` (and friends) that the
        // content tree does not carry, so test both with and without it.
        let rel = d.trim_start_matches("/game/tags/");
        let stripped = rel
            .strip_prefix("objects/")
            .or_else(|| rel.strip_prefix("levels/"))
            .unwrap_or(rel);
        if content_dirs.contains(&format!("/game/{rel}"))
            || content_dirs.contains(&format!("/game/{stripped}"))
        {
            exact += 1;
        } else {
            none += 1;
            if examples.len() < 5 {
                examples.push(d.clone());
            }
        }
    }
    println!("   tag directories                : {}", tag_dirs.len());
    println!("   with a same-path content dir   : {exact}");
    println!("   with none                      : {none}");
    for e in &examples {
        println!("      no content twin: {e}");
    }
    println!(
        "\n   -> the tag tree and the content tree are {}",
        if exact * 4 > tag_dirs.len() {
            "parallel"
        } else {
            "NOT parallel"
        }
    );
}
