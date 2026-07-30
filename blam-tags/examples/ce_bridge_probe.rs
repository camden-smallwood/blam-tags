//! Where the tag->Unreal bridge physically lives, measured over every tag.
//!
//! A Campaign Evolved tag is one cooked package: the Blam tag body is the
//! `.ubulk`, and the `.uasset` export is a `UBlam*TagDataAsset` whose property
//! block is small. `UBlamObjectTagDataAsset::AssetReference` --
//! `TSubclassOf<ABlamObjectActor>` -- is declared on that wrapper, not in the
//! tag body, so it is invisible to any editor that only reads `.ubulk`.
//!
//! This prints, per wrapper class: how many tags carry each property, and what
//! the reference actually resolves to. That is the whole bridge surface, and
//! the answer to "what would an editor have to write to rebind a tag".
//!
//! Run: cargo run --release --features iostore --example ce_bridge_probe [class-substr]

use std::collections::BTreeMap;
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

/// A one-line rendering of what a property points at, for the histogram.
fn describe(v: &PropValue) -> String {
    match v {
        PropValue::Object(0) => "<null object>".into(),
        PropValue::Object(_) => "<object ref>".into(),
        PropValue::SoftObject(p) if p.is_empty() => "<empty soft>".into(),
        PropValue::SoftObject(p) => p.package.as_str().to_string(),
        PropValue::Array(a) => format!("<array[{}]>", a.len()),
        PropValue::Map(m) => format!("<map[{}]>", m.len()),
        other => format!("{other:?}").chars().take(60).collect(),
    }
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default().to_ascii_lowercase();
    let usmap = Usmap::meteorite().expect("bundled usmap");

    // ScriptImport hash -> class name, for every reflected class in the dump.
    // The header file's stem is the class name, which is what the `/Script/
    // Module.Class` path the hash is taken over is built from.
    let mut by_hash: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    for m in std::fs::read_dir(UHT).expect("UHT dump").filter_map(|e| e.ok()) {
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
                    .or_insert_with(|| stem.to_string());
            }
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // wrapper class -> property -> (count, target histogram)
    let mut per_class: BTreeMap<String, (usize, BTreeMap<String, (usize, BTreeMap<String, usize>)>)> =
        BTreeMap::new();
    let (mut tags, mut decoded, mut failed) = (0usize, 0usize, 0usize);
    let (mut unknown_class, mut no_schema) = (0usize, 0usize);
    let mut skipped_groups: BTreeMap<String, usize> = BTreeMap::new();

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.contains("/tags/") || !lp.ends_with(".uasset") {
                continue;
            }
            tags += 1;
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                failed += 1;
                continue;
            };
            let Some(ex) = hdr.export_map.first() else { continue };
            // The wrapper's class is a ScriptImport -- it lives in `global.utoc`'s
            // ScriptObjects chunk, not in this package's name map, which holds
            // only the tag's own name and package path. So the class cannot be
            // read out of the package; it has to be recognised by hash.
            let Some(class) = by_hash.get(&ex.class_index.raw_index()).cloned() else {
                // Name the skip rather than folding it into a total: a gate that
                // drops rows before counting them reports a coverage it never had.
                failed += 1;
                unknown_class += 1;
                let group = e
                    .path
                    .rsplit('-')
                    .next()
                    .and_then(|g| g.strip_suffix(".uasset"))
                    .unwrap_or("<unknown>")
                    .to_string();
                *skipped_groups.entry(group).or_default() += 1;
                continue;
            };
            if !filter.is_empty() && !class.to_ascii_lowercase().contains(&filter) {
                continue;
            }

            let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let Some(body) = bytes.get(start..start + ex.cooked_serial_size as usize) else {
                failed += 1;
                continue;
            };
            let names = hdr.name_map.copy_raw_names();
            let block = match read_export_struct(body, &names, &usmap, &class) {
                Ok(b) => b,
                Err(e) => {
                    failed += 1;
                    no_schema += 1;
                    eprintln!("no schema for {class}: {e:?}");
                    continue;
                }
            };
            decoded += 1;

            let slot = per_class.entry(class).or_default();
            slot.0 += 1;
            for (name, value) in block.iter() {
                let p = slot.1.entry(name.to_string()).or_default();
                p.0 += 1;
                *p.1.entry(describe(value)).or_default() += 1;
            }
        }
    }

    println!("tag .uasset seen {tags}, wrapper decoded {decoded}, failed {failed}");
    println!("  of those failures: {unknown_class} class not in the UHT dump, {no_schema} no .usmap schema");
    for (g, c) in &skipped_groups {
        println!("      skipped group {g}: {c}");
    }
    println!();
    for (class, (n, props)) in &per_class {
        println!("== {class}  ({n} tags) ==");
        if props.is_empty() {
            println!("     (no properties -- the wrapper carries nothing)");
        }
        for (prop, (count, targets)) in props {
            println!("   {count:>6}  {prop}");
            let mut t: Vec<_> = targets.iter().collect();
            t.sort_by(|a, b| b.1.cmp(a.1));
            for (target, c) in t.iter().take(4) {
                println!("            {c:>5}  {target}");
            }
            if t.len() > 4 {
                println!("            ... {} more distinct", t.len() - 4);
            }
        }
        println!();
    }
}
