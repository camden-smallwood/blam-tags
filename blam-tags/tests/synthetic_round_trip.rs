//! Every group of every game, built from the definitions, filled, written,
//! read back and written again.
//!
//! The corpus round-trip tests need editing kits, which CI does not have and
//! the repository cannot ship. These tags are made here instead (see
//! `common::synthetic`): a new tag of each group with elements in its blocks
//! and a value in every leaf field, so the writers and readers see every
//! field type, nested blocks, tag references, string ids, data, pageable
//! resources, Halo CE's big-endian body and Halo 2's block headers. The two
//! writes must be byte-identical and the tag read back must hold the values
//! that were set.
//!
//! What this cannot reach: an older Halo 2 struct version (a new element is
//! always the newest variant; the older ones come only from tags on disk),
//! legacy Halo 2 forms (`ambl`/`LAMB`/`MLAB`), monolithic `xsync` resources,
//! and the raw-only types with no value to set (Halo 2 `pointer` and
//! `vertex_buffer`, Halo 4 `non_cache_runtime_value`).

mod common;

use std::collections::{BTreeMap, BTreeSet};

use blam_tags::classic::ClassicEngine;
use common::synthetic::{self, FillPlan};

/// Groups that fail the round trip, as `(game, "fourcc group: problem")`.
/// Empty: every group of every game passes. A failure found later is listed
/// here with its cause until it is fixed, so the suite keeps guarding every
/// other group.
const KNOWN_FAILURES: &[(&str, &str)] = &[];

/// Round-trip every group of `game`, and check the fill did real work.
fn round_trip_game(game: &str, minimum_elements: usize) {
    let mut failures = BTreeSet::new();
    let mut elements = 0;
    let mut set = 0;
    for (tag, group) in synthetic::groups(game) {
        let mut new = synthetic::new_tag(game, &group);
        let stats = synthetic::fill(&mut new, FillPlan::default());
        elements += stats.elements;
        set += stats.set.values().sum::<usize>();
        if let Some(problem) = synthetic::round_trip(game, &group, &new) {
            failures.insert(format!("{tag} {group}: {problem}"));
        }
    }
    let known: BTreeSet<String> = KNOWN_FAILURES
        .iter()
        .filter(|(known_game, _)| *known_game == game)
        .map(|(_, failure)| (*failure).to_owned())
        .collect();
    assert_eq!(failures, known, "{game}: round-trip failures");
    assert!(elements >= minimum_elements, "{game}: only {elements} elements added");
    assert!(set >= minimum_elements * 4, "{game}: only {set} fields set");
}

#[test]
fn halo_ce_groups_round_trip() {
    round_trip_game("haloce_mcc", 1000);
}

#[test]
fn halo_2_groups_round_trip() {
    round_trip_game("halo2_mcc", 3500);
}

#[test]
fn halo_3_groups_round_trip() {
    round_trip_game("halo3_mcc", 5000);
}

#[test]
fn halo_3_odst_groups_round_trip() {
    round_trip_game("halo3odst_mcc", 5000);
}

#[test]
fn halo_reach_groups_round_trip() {
    round_trip_game("haloreach_mcc", 7000);
}

#[test]
fn halo_4_groups_round_trip() {
    round_trip_game("halo4_mcc", 8000);
}

#[test]
fn halo_2_anniversary_groups_round_trip() {
    round_trip_game("halo2amp_mcc", 8000);
}

#[test]
fn campaign_evolved_groups_round_trip() {
    round_trip_game("haloce_evolved", 4000);
}

/// The round trip notices a value that changed without changing the shape:
/// a float in a filled tag's bytes is altered, the tag still reads, and the
/// comparison against the original reports it. Without this, a comparison
/// that compared nothing would pass every game above.
#[test]
fn the_round_trip_check_sees_a_changed_value() {
    for game in ["halo3_mcc", "haloce_mcc", "halo2_mcc"] {
        let mut tag = synthetic::new_tag(game, "sound_environment");
        synthetic::fill(&mut tag, FillPlan::default());
        let bytes = tag.write_to_bytes().unwrap();
        let original = synthetic::dump(&tag);

        // A filled `real` is 0.25 * n + 0.125 for a small n: unique enough
        // to find its bytes in the body.
        let real = original
            .iter()
            .find_map(|(_, value)| value.strip_prefix("Some(Real(").and_then(|v| v.strip_suffix("))")))
            .map(|v| v.parse::<f32>().unwrap())
            .expect("a real field");
        let encode = |value: f32| match synthetic::classic_engine(game) {
            Some(ClassicEngine::HaloCe) => value.to_be_bytes(),
            _ => value.to_le_bytes(),
        };
        let at = bytes.windows(4).position(|w| w == encode(real)).expect("the real's bytes");
        let mut changed = bytes.clone();
        changed[at..at + 4].copy_from_slice(&encode(real + 100.0));

        let read = synthetic::read_back(game, "sound_environment", &changed).expect("still reads");
        assert_ne!(synthetic::dump(&read), original, "{game}: a changed real went unnoticed");
        let read = synthetic::read_back(game, "sound_environment", &bytes).unwrap();
        assert_eq!(synthetic::dump(&read), original, "{game}: the unchanged bytes differ");
    }
}

/// `TagFile::new_classic` writes the header tool.exe writes: every byte but
/// the checksum (40..44) matches what most kit tags of the group carry — zero
/// name, group, header size, group version, `00 FF`, engine. Halo CE's
/// `old tags` folder is left out: those were written by an older tool that
/// stored the body size at 48, and `color_table` and `string_list` exist
/// only there. Halo 2's are compared with the kit tags saved in the current
/// (`BLM!`) form only. Needs the Halo CE and Halo 2 kits.
#[test]
fn new_classic_headers_match_the_kits() {
    for (env, game) in [("BLAM_TEST_HCEEK", "haloce_mcc"), ("BLAM_TEST_H2EK", "halo2_mcc")] {
        let Some(kit) = common::kit(env) else { continue };
        // Every header of each extension, checksum blanked, counted.
        let mut headers: BTreeMap<String, BTreeMap<Vec<u8>, usize>> = BTreeMap::new();
        let mut stack = vec![kit.tags()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for path in entries.flatten().map(|e| e.path()) {
                if path.is_dir() {
                    if path.file_name().is_none_or(|name| name != "old tags") {
                        stack.push(path);
                    }
                    continue;
                }
                let Some(extension) = path.extension().and_then(|x| x.to_str()) else { continue };
                let mut header = vec![0u8; 64];
                let Ok(mut file) = std::fs::File::open(&path) else { continue };
                if std::io::Read::read_exact(&mut file, &mut header).is_err() {
                    continue;
                }
                // Halo 2 kit tags were saved by four tool generations; a new
                // tag is the current (`BLM!`) form, so compare with those.
                if game == "halo2_mcc" && &header[60..64] != b"!MLB" {
                    continue;
                }
                header[40..44].fill(0);
                *headers.entry(extension.to_ascii_lowercase()).or_default().entry(header).or_default() += 1;
            }
        }
        let mut compared = 0;
        let mut mismatches = Vec::new();
        for (tag, group) in synthetic::groups(game) {
            let Some(counts) = headers.get(&group.to_ascii_lowercase()) else { continue };
            let (common_header, _) = counts.iter().max_by_key(|(_, count)| **count).unwrap();
            let mut new = synthetic::new_tag(game, &group).write_to_bytes().unwrap();
            new.truncate(64);
            new[40..44].fill(0);
            compared += 1;
            if &new != common_header {
                mismatches.push(format!("{tag} {group}: new {:02x?} kit {:02x?}", &new[36..], &common_header[36..]));
            }
        }
        assert!(mismatches.is_empty(), "{game}:\n{}", mismatches.join("\n"));
        assert!(compared > 40, "{game}: only {compared} groups had a kit tag");
    }
}
