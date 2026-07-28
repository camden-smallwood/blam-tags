//! Census of ALL cooked content in Campaign Evolved: package count per pak,
//! and export-class histogram (native classes resolved by reversing the
//! ScriptImport hash against the UHT header dump).
//!
//! Answers "what is actually in this game, and how big is each modding
//! surface".
//!
//! Run: cargo run --release --features iostore --example ce_content_census

use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    // native class hash -> "/Script/Module.Class"
    let mut by_hash: HashMap<u64, String> = HashMap::new();
    for m in std::fs::read_dir(UHT).unwrap().filter_map(|e| e.ok()) {
        if !m.path().is_dir() {
            continue;
        }
        let module = m.file_name().to_string_lossy().to_string();
        for sub in ["Public", "Private", "Classes"] {
            let Ok(rd) = std::fs::read_dir(format!("{UHT}/{module}/{sub}")) else { continue };
            for f in rd.filter_map(|e| e.ok()) {
                let n = f.file_name().to_string_lossy().to_string();
                let Some(stem) = n.strip_suffix(".h") else { continue };
                let path = format!("/Script/{module}.{stem}");
                by_hash
                    .entry(FPackageObjectIndex::create_script_import(&path).raw_index())
                    .or_insert(path);
            }
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .collect();
    utocs.sort();

    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    let mut roots: BTreeMap<String, usize> = BTreeMap::new();
    let mut per_pak: Vec<(String, usize, usize, u64)> = Vec::new();
    let mut total_pkgs = 0usize;
    let mut unknown_class = 0usize;

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let label = u.file_stem().unwrap().to_string_lossy().to_string();
        let mut pkgs = 0usize;
        let mut bytes = 0u64;
        for e in a.entries() {
            bytes += a.uncompressed_len(&e.path).unwrap_or(0);
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") && !lower.ends_with(".umap") {
                continue;
            }
            pkgs += 1;
            total_pkgs += 1;
            // /game/<root>/...
            if let Some(rest) = lower.split_once("/content/").map(|(_, r)| r) {
                let root = rest.split('/').next().unwrap_or("").to_string();
                *roots.entry(root).or_default() += 1;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            for ex in &h.export_map {
                let k = by_hash
                    .get(&ex.class_index.raw_index())
                    .cloned()
                    .unwrap_or_else(|| {
                        unknown_class += 1;
                        if ex.class_index.package_import().is_some() {
                            "<blueprint-generated class>".to_string()
                        } else {
                            "<unknown native class>".to_string()
                        }
                    });
                *classes.entry(k).or_default() += 1;
            }
        }
        per_pak.push((label, pkgs, a.entries().len(), bytes));
    }

    println!("== paks ==");
    per_pak.sort_by_key(|p| std::cmp::Reverse(p.3));
    for (l, p, e, b) in &per_pak {
        println!("{l:<28} {p:>7} packages  {e:>8} chunks  {:>9.1} MiB", *b as f64 / 1048576.0);
    }
    println!("\ntotal packages: {total_pkgs}");

    println!("\n== /Game roots ==");
    let mut rs: Vec<_> = roots.iter().collect();
    rs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (r, n) in rs {
        println!("{n:>8}  {r}");
    }

    println!("\n== export classes ({} distinct, {unknown_class} unresolved) ==", classes.len());
    let mut cs: Vec<_> = classes.iter().collect();
    cs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (c, n) in cs.iter() {
        println!("{n:>8}  {c}");
    }
}
