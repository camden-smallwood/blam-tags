//! Which audio asset does a weapon Blueprint actually *invoke* when it fires?
//!
//! A Blueprint's import table lists everything it references, including assets
//! left behind when the BP was duplicated from another weapon — so imports
//! alone can't identify the firing sound. This walks the compiled Kismet
//! bytecode of every UFunction in the package and reports which imports are
//! referenced from which function, so the firing path can be read structurally.
//!
//! Run: cargo run --release --features iostore --example ce_bp_audio_binding -- <pkg-substr> [filter]

use std::collections::BTreeMap;
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::unversioned::read_ufunction_script;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn norm(p: &str) -> String {
    p.to_ascii_lowercase().replace('\\', "/")
}

fn main() -> anyhow::Result<()> {
    let want = norm(&std::env::args().nth(1).expect("usage: <pkg-substr> [filter]"));
    let filter = std::env::args().nth(2).map(|f| f.to_ascii_lowercase());

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<Arc<IoStoreArchive>> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok().map(Arc::new)).collect();

    let bytes = archives
        .iter()
        .find_map(|a| {
            a.entries()
                .iter()
                .find(|e| norm(&e.path).contains(&want) && norm(&e.path).ends_with(".uasset"))
                .and_then(|e| a.read(&e.path).ok())
        })
        .ok_or_else(|| anyhow::anyhow!("no package matching {want:?}"))?;

    let h = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let names = h.name_map.copy_raw_names();
    let header_size = h.summary.header_size as usize;

    // Import slot -> owning package name. A package import encodes an index
    // into `imported_package_names`; script imports (/Script/...) are hashes
    // with no local name, so they stay unlabelled.
    let mut import_name: BTreeMap<usize, String> = BTreeMap::new();
    for (i, idx) in h.import_map.iter().enumerate() {
        if let Some(r) = idx.package_import()
            && let Some(pkg) = h.imported_package_names.get(r.imported_package_index as usize)
        {
            import_name.insert(i, pkg.clone());
        }
    }
    println!("{} imports ({} resolvable to a package)", h.import_map.len(), import_name.len());

    // Candidate imports of interest, and the FPackageIndex each would appear as
    // in bytecode: negative, 1-based.
    let targets: Vec<(i32, usize, &String)> = import_name
        .iter()
        .filter(|(_, n)| filter.as_ref().is_none_or(|f| n.to_ascii_lowercase().contains(f)))
        .map(|(i, n)| (-(*i as i32) - 1, *i, n))
        .collect();
    println!("tracking {} import(s) matching filter {filter:?}\n", targets.len());

    let mut hits: BTreeMap<&String, Vec<String>> = BTreeMap::new();

    for ex in &h.export_map {
        let fname = h.name_map.get(ex.object_name);
        let s = header_size + ex.cooked_serial_offset as usize;
        let e = s + ex.cooked_serial_size as usize;
        let Some(payload) = bytes.get(s..e) else { continue };
        // Raw pass first: a reference in the export's serialized bytes but not
        // in its bytecode means the asset sits in a property default, not code.
        for (code, _i, name) in &targets {
            let pat = code.to_le_bytes();
            if payload.windows(4).any(|w| w == pat) {
                hits.entry(name).or_default().push(format!("{fname} [raw export bytes]"));
            }
        }
        let Ok(script) = read_ufunction_script(payload, &names) else { continue };
        if script.is_empty() {
            continue;
        }
        // Object references inside compiled bytecode are FPackageIndex i32s.
        // Scan every alignment: the surrounding opcodes vary, but a match on
        // the exact index is already strong evidence of a reference.
        for (code, _i, name) in &targets {
            let pat = code.to_le_bytes();
            let count = script.windows(4).filter(|w| *w == pat).count();
            if count > 0 {
                hits.entry(name).or_default().push(format!("{fname} (x{count})"));
            }
        }
    }

    if hits.is_empty() {
        println!("no tracked import was referenced from any UFunction bytecode");
    }
    for (name, fns) in &hits {
        println!("{name}");
        for f in fns {
            println!("      referenced from {f}");
        }
    }
    Ok(())
}
