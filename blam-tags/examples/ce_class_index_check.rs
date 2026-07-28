//! Why does a hash-based class lookup find no exports?
//!
//! `ce_tail_probe` resolves its target class by hashing `/Script/Module.Class`
//! into an `FPackageObjectIndex` and comparing that against each export's
//! `class_index`. `ce_coverage_matrix` instead resolves through the global
//! `ScriptObjects` table. If those two disagree the probe silently reports
//! nothing, which reads as "this class has no tails" rather than "the lookup is
//! broken". This prints both so the disagreement is visible.
//!
//! Run: ce_class_index_check <Module.Class> [more...]
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let targets: Vec<String> = std::env::args().skip(1).collect();

    // What the script-object table says.
    let so = ScriptObjects::load(format!("{PAKS}/global.utoc")).expect("script objects");
    for t in &targets {
        let path = format!("/Script/{t}");
        let hashed = FPackageObjectIndex::create_script_import(&path);
        let found = so.entries().iter().find_map(|e| {
            let raw = e.global_index.raw_index();
            (so.resolve(raw) == Some(path.as_str())).then_some(raw)
        });
        println!("{path}");
        println!(
            "  create_script_import raw = {:#018x}  full = {:#018x}  kind = {:?}",
            hashed.raw_index(),
            hashed.value().unwrap_or(0),
            hashed.kind()
        );
        match found {
            Some(i) => println!("  ScriptObjects        raw = {i:#018x}"),
            None => println!("  ScriptObjects        raw = <absent from table>"),
        }
    }

    // How many exports in the corpus actually carry each index.
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let wanted: Vec<(String, u64)> = targets
        .iter()
        .map(|t| {
            let p = format!("/Script/{t}");
            (p.clone(), FPackageObjectIndex::create_script_import(&p).raw_index())
        })
        .collect();
    let mut hits = vec![0usize; wanted.len()];
    let mut first_seen: Vec<Option<(u64, String)>> = vec![None; wanted.len()];
    let mut pkgs = 0usize;
    let mut exports = 0usize;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            pkgs += 1;
            for ex in &h.export_map {
                exports += 1;
                for (i, (_, raw)) in wanted.iter().enumerate() {
                    if ex.class_index.raw_index() == *raw {
                        hits[i] += 1;
                        if first_seen[i].is_none() {
                            first_seen[i] = Some((
                                ex.class_index.value().unwrap_or(0),
                                format!("{:?}", ex.class_index.kind()),
                            ));
                        }
                    }
                }
            }
        }
    }
    println!("\nscanned {pkgs} packages / {exports} exports");
    for (i, (p, _)) in wanted.iter().enumerate() {
        println!("  {p}: {} exports by raw_index", hits[i]);
        if let Some((full, kind)) = &first_seen[i] {
            println!("      on-disk class_index full = {full:#018x}  kind = {kind}");
        }
    }
}
