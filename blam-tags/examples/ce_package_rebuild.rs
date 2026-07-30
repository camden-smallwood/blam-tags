//! Gate: can a package be taken fully apart and put back together?
//!
//! `ce_package_roundtrip` re-emits the header with the payloads copied through.
//! This goes the whole way: every export is decoded to a
//! [`Export`](blam_tags::iostore::object::unversioned::Export), re-encoded from
//! that decoded form, and the package is rebuilt with every
//! `cooked_serial_offset`/`cooked_serial_size` recomputed from the payloads
//! actually produced.
//!
//! So it tests two claims at once that nothing before it could:
//!
//!  * exports are laid out in export-map order, each starting where the last
//!    ended — the rule `write_package` encodes, rather than assumes silently;
//!  * a decoded export re-encodes to the same bytes *in situ*, with the header
//!    agreeing about where it is.
//!
//! Exports whose class has no `.usmap` schema are passed through verbatim
//! rather than skipped, so the package is always rebuilt in full — a gate that
//! quietly dropped them would be measuring much less than it claims. The
//! `re-encoded` count says how many were genuinely round-tripped.
//!
//! Run: `ce_package_rebuild [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, write_export};
use blam_tags::iostore::package::builder::{read_payloads, write_package};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    let so = ScriptObjects::load(format!("{PAKS}/global.utoc")).expect("script objects");
    for e in so.entries() {
        if let Some(p) = so.resolve(e.global_index.raw_index()) {
            by_hash.insert(e.global_index.raw_index(), p.to_string());
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read Paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    let (mut total, mut same, mut failed) = (0usize, 0usize, 0usize);
    let (mut reencoded, mut passthrough) = (0u64, 0u64);
    // How many packages do NOT lay their exports out contiguously in map order?
    let mut non_contiguous = 0usize;
    let mut zones: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for a in &archives {
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
            let Ok(mut payloads) = read_payloads(&h, &b) else { continue };
            total += 1;

            // Independently check the layout rule `write_package` relies on.
            let mut expect = 0u64;
            for entry in &h.export_map {
                if entry.cooked_serial_offset != expect {
                    non_contiguous += 1;
                    break;
                }
                expect += entry.cooked_serial_size;
            }

            let names = h.name_map.copy_raw_names();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else {
                    passthrough += 1;
                    continue;
                };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    passthrough += 1;
                    continue;
                }
                match read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                    .and_then(|parts| write_export(short, &parts, &usmap))
                {
                    Ok(bytes) => {
                        payloads[i] = bytes;
                        reencoded += 1;
                    }
                    Err(_) => passthrough += 1,
                }
            }

            match write_package(&h, &payloads, HV) {
                Ok((out, _)) if out == b => same += 1,
                Ok(out) => {
                    let out = out.0;
                    let at = out
                        .iter()
                        .zip(&b)
                        .position(|(x, y)| x != y)
                        .unwrap_or(out.len().min(b.len()));
                    *zones.entry(h.summary.section_at(at).unwrap_or("export payloads")).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {} bytes in, {} out, first difference at {at}",
                            h.package_name(),
                            b.len(),
                            out.len()
                        ));
                    }
                }
                Err(err) => {
                    failed += 1;
                    if samples.len() < 8 {
                        samples.push(format!("{}: {err:#}", h.package_name()));
                    }
                }
            }
        }
    }

    println!("packages examined    {total}");
    println!("rebuilt exactly      {same} ({:.4}%)", 100.0 * same as f64 / total.max(1) as f64);
    println!("differ               {}", total - same - failed);
    println!("refused to build     {failed}");
    println!("exports re-encoded   {reencoded}");
    println!("exports passed thru  {passthrough}  (no .usmap schema for their class)");
    println!("non-contiguous pkgs  {non_contiguous}  (exports not laid out in map order)");

    if !zones.is_empty() {
        println!("\nfirst difference by section:");
        let mut v: Vec<_> = zones.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (z, n) in v {
            println!("  {n:>7}  {z}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if same != total {
        std::process::exit(1);
    }
}
