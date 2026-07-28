//! Validate the cooked `UAkAudioEvent` decoder across every event asset in the
//! Campaign Evolved build: decode `FWwiseLocalizedEventCookedData` and check
//! each event's `EventId` against the Wwise FNV-1 hash of its own `DebugName`.
//! A wrong field offset cannot survive that check, so the pass rate is a real
//! correctness measure rather than a "didn't crash" count.
//!
//! Run:
//!   cargo run --release -p blam-tags --features "iostore audio" --example ce_event_sweep -- [step]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::wwise_event::read_event_cooked_data;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() -> anyhow::Result<()> {
    let step: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let usmap = Usmap::meteorite()?;

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let (mut total, mut decoded, mut fnv_ok, mut no_cooked) = (0usize, 0usize, 0usize, 0usize);
    let (mut media_total, mut langs) = (0usize, BTreeMap::<String, usize>::new());
    let mut errs: BTreeMap<String, usize> = BTreeMap::new();
    let mut mismatches: Vec<String> = Vec::new();

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        let paths: Vec<String> = a
            .entries()
            .iter()
            .map(|e| e.path.replace('\\', "/"))
            .filter(|p| {
                let l = p.to_ascii_lowercase();
                l.ends_with(".uasset")
                    && (l.contains("/wwise/events/") || l.contains("/wwiseaudio/events/"))
            })
            .collect();
        for p in paths.iter().step_by(step) {
            total += 1;
            let Ok(bytes) = a.read(p) else {
                *errs.entry("io".into()).or_default() += 1;
                continue;
            };
            let Ok(hdr) = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
            else {
                *errs.entry("zen".into()).or_default() += 1;
                continue;
            };
            let names = hdr.name_map.copy_raw_names();
            let Some(ex) = hdr.export_map.first() else { continue };
            let start = hdr.summary.header_size as usize + ex.cooked_serial_offset as usize;
            let end = start + ex.cooked_serial_size as usize;
            let Some(body) = bytes.get(start..end) else {
                *errs.entry("slice".into()).or_default() += 1;
                continue;
            };

            let Ok(c) = read_event_cooked_data(body, &names, &usmap) else {
                no_cooked += 1;
                continue;
            };
            decoded += 1;

            let (dbg, id) = (c.event_name.clone(), c.event_id);
            if blam_tags::audio::wwise::hash_name(&dbg) == id {
                fnv_ok += 1;
            } else if mismatches.len() < 8 {
                mismatches.push(format!("{p}  DebugName={dbg} EventId={id}"));
            }

            for l in &c.languages {
                *langs.entry(l.clone()).or_default() += 1;
            }
            media_total += c.media.len();
        }
    }

    println!("event assets scanned : {total} (every {step})");
    println!("cooked data decoded  : {decoded}");
    println!("  EventId == FNV     : {fnv_ok}");
    println!("  no cooked data     : {no_cooked}");
    println!("media entries        : {media_total}");
    println!("errors               : {errs:?}");
    let mut l: Vec<_> = langs.into_iter().collect();
    l.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    println!("languages            : {l:?}");
    for m in &mismatches {
        println!("  FNV MISMATCH {m}");
    }
    Ok(())
}
