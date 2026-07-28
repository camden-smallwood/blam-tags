//! What distinguishes the three `object_flags` / two `package_flags` variants
//! seen across CE tag packages? Cross-tabulate against group, pak, and whether
//! the tag is referenced by anything.
//!
//! Run: cargo run --release --features iostore --example ce_tag_flag_variants

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // (object_flags, package_flags) -> group -> count, plus samples & paks
    let mut combos: BTreeMap<(u32, u32), (BTreeMap<String, usize>, BTreeMap<String, usize>, Vec<String>)> =
        BTreeMap::new();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let pak = u.file_stem().unwrap().to_string_lossy().to_string();
        for e in a.entries() {
            let lower = e.path.to_ascii_lowercase().replace('\\', "/");
            if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                continue;
            }
            let stem = lower.rsplit('/').next().unwrap().trim_end_matches(".uasset");
            let Some((_, group)) = stem.rsplit_once('-') else { continue };
            let Ok(ua) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&ua), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = h.export_map.first() else { continue };
            let key = (ex.object_flags, h.summary.package_flags);
            let slot = combos.entry(key).or_default();
            *slot.0.entry(group.to_string()).or_default() += 1;
            *slot.1.entry(pak.clone()).or_default() += 1;
            if slot.2.len() < 6 {
                slot.2.push(h.package_name());
            }
        }
    }

    for ((of, pf), (groups, paks, samples)) in &combos {
        let total: usize = groups.values().sum();
        println!("\n== object_flags 0x{of:x}  package_flags 0x{pf:x}  ({total} tags) ==");
        let mut gs: Vec<_> = groups.iter().collect();
        gs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        println!("  groups: {:?}", gs.iter().take(14).map(|(g, n)| format!("{g}×{n}")).collect::<Vec<_>>());
        println!("  paks  : {:?}", paks.iter().map(|(p, n)| format!("{p}×{n}")).collect::<Vec<_>>());
        for s in samples {
            println!("    {s}");
        }
    }
}
