//! Mount Halo: Campaign Evolved's pakchunk0 IoStore container and validate that
//! its `.ubulk` entries are byte-complete Reach tags.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example mount_campaign_evolved -- \
//!     "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/pakchunk0-WinGDK.utoc" [sample]
//!
//! With no args it defaults to the path above. `sample` caps how many tags to
//! actually decode+parse (default 500); pass 0 to parse every tag.

use std::collections::BTreeMap;
use std::time::Instant;

use blam_tags::file::TagFile;
use blam_tags::iostore::{is_tag_payload, parse_ublock_stem, IoStoreArchive};

const DEFAULT_UTOC: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks/pakchunk0-WinGDK.utoc";

fn main() {
    let mut args = std::env::args().skip(1);
    let utoc = args.next().unwrap_or_else(|| DEFAULT_UTOC.to_string());
    let sample: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(500);

    let t0 = Instant::now();
    let archive = IoStoreArchive::open(&utoc).unwrap_or_else(|e| {
        eprintln!("failed to open {utoc}: {e}");
        std::process::exit(1);
    });
    println!(
        "opened {} in {:?}: {} total files, {} .ubulk entries",
        utoc,
        t0.elapsed(),
        archive.entries().len(),
        archive.ublock_entries().count()
    );

    // Bucket .ubulk tags by their group long-name.
    let mut by_group: BTreeMap<&str, usize> = BTreeMap::new();
    for e in archive.ublock_entries() {
        if let Some((_name, group)) = parse_ublock_stem(&e.path) {
            *by_group.entry(group).or_default() += 1;
        }
    }
    println!("distinct tag groups: {}", by_group.len());
    let mut groups: Vec<_> = by_group.iter().collect();
    groups.sort_by(|a, b| b.1.cmp(a.1));
    print!("top groups:");
    for (g, n) in groups.iter().take(12) {
        print!(" {g}={n}");
    }
    println!();

    // Candidate tags: `.ubulk` entries whose stem is `<name>-<group>` (this
    // alone excludes UE texture mips and Wwise VO `.ubulk` bulk data). The
    // parser's own `BLAM` check is the final arbiter of what's really a tag.
    let paths: Vec<String> = archive
        .ublock_entries()
        .filter(|e| parse_ublock_stem(&e.path).is_some())
        .map(|e| e.path.clone())
        .collect();
    println!("candidate tag .ubulk (stem has -group): {}", paths.len());
    let take = if sample == 0 { paths.len() } else { sample.min(paths.len()) };

    let t1 = Instant::now();
    let mut ok = 0usize; // real tag: BLAM payload that parsed
    let mut not_a_tag = 0usize; // decoded fine but not a tag (UE bulk data)
    let mut decode_err = 0usize; // Oodle/IoStore read failure — the thing we care about
    let mut tag_parse_err = 0usize; // real tag payload that failed to parse — also care
    let mut real_failures: Vec<(String, String)> = Vec::new();

    for path in paths.iter().take(take) {
        match archive.read(path) {
            Ok(bytes) => {
                if !is_tag_payload(&bytes) {
                    not_a_tag += 1;
                    continue;
                }
                match TagFile::read_from_bytes(&bytes) {
                    Ok(_tag) => ok += 1,
                    Err(e) => {
                        tag_parse_err += 1;
                        if real_failures.len() < 30 {
                            real_failures.push((path.clone(), format!("tag parse: {e}")));
                        }
                    }
                }
            }
            Err(e) => {
                decode_err += 1;
                if real_failures.len() < 30 {
                    real_failures.push((path.clone(), format!("decode: {e}")));
                }
            }
        }
    }

    println!(
        "swept {take} candidates in {:?}:\n  {ok} real tags OK\n  {not_a_tag} non-tag .ubulk (bulk data, correctly skipped)\n  {decode_err} DECODE errors\n  {tag_parse_err} TAG-PARSE errors",
        t1.elapsed()
    );
    if real_failures.is_empty() {
        println!("no genuine decode/parse failures on real tags \u{2705}");
    } else {
        println!("GENUINE failures (decode or real-tag parse):");
        for (p, e) in &real_failures {
            println!("  {p}\n    {e}");
        }
    }

    // Completeness (full mode only): probe every `.ubulk` the name filter
    // REJECTED, to prove the filter has no false negatives (real tags whose
    // stem lacks `-group`).
    if sample == 0 {
        let t2 = Instant::now();
        let mut missed = 0usize;
        let mut examples: Vec<String> = Vec::new();
        for e in archive.ublock_entries() {
            if parse_ublock_stem(&e.path).is_some() {
                continue;
            }
            if let Ok(bytes) = archive.read(&e.path) {
                if is_tag_payload(&bytes) {
                    missed += 1;
                    if examples.len() < 20 {
                        examples.push(e.path.clone());
                    }
                }
            }
        }
        println!(
            "completeness scan of non-candidate .ubulk in {:?}: {missed} stray tag payloads missed by the name filter",
            t2.elapsed()
        );
        for p in &examples {
            println!("  MISSED: {p}");
        }
    }
}
