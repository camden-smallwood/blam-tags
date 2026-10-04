//! What the engine assumes about the `definitions/` submodule, checked.
//!
//! Each game's definitions are dumped from that game's own executable, so they
//! are the authoritative schema — but nothing in the submodule checks itself,
//! and the engine reads them with lookups that quietly give up (a parent that
//! is not in `tag_index` simply ends the merge; a tag with a size that does not
//! add up fails only when someone happens to create one). These tests state
//! those assumptions over every group of every game, so a re-dump that breaks
//! one fails here rather than in whichever tool opens the affected group first.
//!
//! Known exceptions are listed by name with the reason, and the lists are
//! compared for equality: an exception that stops occurring fails too, so the
//! lists cannot quietly go stale.
//!
//! Gaps in the *data* that no test here can see, because the dump does not
//! carry them (re-dump wishlist, not failures):
//! - field defaults: a new tag is zero-filled, not the engine's defaults;
//! - struct `version` for gen3+ (only Halo 2 carries `struct_versions`);
//! - allowed groups on classic `tag_reference`s, and the kind of Halo 2
//!   customs;
//! - Halo 3's particle root is dumped at 404 bytes where shipped tags carry
//!   424 — the dump is internally consistent, so only a kit can tell;
//! - `ssfx`, a template Halo 3 and ODST structs name but no game indexes, so
//!   it expands to nothing.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use blam_tags::convert::{GameTagIndex, CAMPAIGN_EVOLVED_GENERATION};
use blam_tags::{TagFile, TagLayout};
use serde_json::Value;

/// Every definitions folder, with the generation `TagFile::new` stamps into a
/// new tag's header: `(build_version, build_number, version)`. The classic
/// games have no MCC generation and keep the header zeroed.
const GAMES: &[(&str, (i32, i32, u32))] = &[
    ("halo2_mcc", (0, 0, 0)),
    ("halo2amp_mcc", (1, 2, u32::MAX)),
    ("halo3_mcc", (1, 1, u32::MAX)),
    ("halo3odst_mcc", (1, 1, u32::MAX)),
    ("halo4_mcc", (1, 2, u32::MAX)),
    ("haloce_evolved", CAMPAIGN_EVOLVED_GENERATION),
    ("haloce_mcc", (0, 0, 0)),
    ("haloreach_mcc", (1, 2, u32::MAX)),
];

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

fn meta(game: &str) -> Value {
    read_json(&common::definitions(game).join("_meta.json"))
}

/// `tag_index` as `(fourcc, group name)` pairs, in file order.
fn tag_index(game: &str) -> Vec<(String, String)> {
    meta(game)["tag_index"]
        .as_object()
        .unwrap_or_else(|| panic!("{game}: _meta.json has no tag_index object"))
        .iter()
        .map(|(tag, name)| {
            let name = name.as_str().unwrap_or_else(|| panic!("{game} {tag}: name is not a string"));
            (tag.clone(), name.to_owned())
        })
        .collect()
}

/// The group files in a folder, by the exact bytes of their names. Read from
/// the directory listing rather than probed with `exists`, which on a
/// case-insensitive filesystem (macOS, Windows) would find `spawnsettings.json`
/// as happily as `SpawnSettings.json`.
fn group_files(game: &str) -> BTreeSet<String> {
    std::fs::read_dir(common::definitions(game))
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".json") && name != "_meta.json")
        .map(|name| name.trim_end_matches(".json").to_owned())
        .collect()
}

/// The list of games is complete: a new definitions folder has to be added
/// to `GAMES` (and so to every test below) rather than being skipped.
#[test]
fn every_definitions_folder_is_listed() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(common::definitions_root())
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().join("_meta.json").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    let listed: BTreeSet<String> = GAMES.iter().map(|(game, _)| (*game).to_owned()).collect();
    assert_eq!(on_disk, listed, "definitions folders and GAMES disagree");
}

/// `tag_index` and the group files match one to one, byte for byte, and each
/// file says it is the group the index files it under.
#[test]
fn tag_index_and_group_files_agree() {
    let mut problems = Vec::new();
    for (game, _) in GAMES {
        let index = tag_index(game);
        let files = group_files(game);
        let indexed: BTreeSet<String> = index.iter().map(|(_, name)| name.clone()).collect();
        if indexed.len() != index.len() {
            problems.push(format!("{game}: a group name is indexed under two FOURCCs"));
        }
        for name in indexed.difference(&files) {
            problems.push(format!("{game}: {name} is indexed but has no {name}.json"));
        }
        for name in files.difference(&indexed) {
            problems.push(format!("{game}: {name}.json is not in tag_index"));
        }
        for (tag, name) in &index {
            if !files.contains(name) {
                continue;
            }
            let group = read_json(&common::definitions(game).join(format!("{name}.json")));
            if group["tag"].as_str() != Some(tag) {
                problems.push(format!("{game}: {name}.json says tag {:?}, indexed as {tag:?}", group["tag"]));
            }
            if group["name"].as_str() != Some(name) {
                problems.push(format!("{game}: {name}.json says name {:?}", group["name"]));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

/// Seven Halo 4 / Halo 2 Anniversary MP groups have mixed-case names, and the
/// file names match. Every lookup the engine makes goes through the index's
/// own spelling, so it opens the right file on a case-sensitive filesystem;
/// this pins that the names really are mixed case (or the test proves
/// nothing) and that both of the engine's group-name tables spell them as the
/// files do.
#[test]
fn mixed_case_group_names_are_looked_up_with_their_own_case() {
    const MIXED: &[(&str, &str)] = &[
        ("ffgt", "GameEngineFirefightVariantTag"),
        ("iuii", "InfinityUIImages"),
        ("kccd", "KillCamCameraParamter"),
        ("mgee", "multiplayerEffects"),
        ("narg", "NarrativeGlobals"),
        ("sigd", "SuppressedIncident"),
        ("ssdf", "SpawnSettings"),
    ];
    for (game, _) in GAMES {
        let mixed: BTreeSet<(String, String)> = tag_index(game)
            .into_iter()
            .filter(|(_, name)| name.chars().any(|c| c.is_ascii_uppercase()))
            .collect();
        let expected: BTreeSet<(String, String)> = if matches!(*game, "halo4_mcc" | "halo2amp_mcc") {
            MIXED.iter().map(|(tag, name)| ((*tag).to_owned(), (*name).to_owned())).collect()
        } else {
            BTreeSet::new()
        };
        assert_eq!(mixed, expected, "{game}: mixed-case group names changed");

        let index = GameTagIndex::load(&common::definitions_root(), game).unwrap();
        let files = group_files(game);
        for (tag, name) in &expected {
            let group = blam_tags::parse_group_tag(tag).unwrap();
            assert_eq!(index.by_tag.get(&group), Some(name), "{game} {tag}: GameTagIndex");
            assert_eq!(
                blam_tags::paths::group_tag_to_extension(group),
                Some(name.as_str()),
                "{tag}: the cross-game table"
            );
            assert!(files.contains(name), "{game}: no file spelled {name}.json");
            assert!(!files.contains(&name.to_ascii_lowercase()), "{game}: {name} also exists lower-cased");
            // Building the group goes through the same spelling.
            TagLayout::from_json(common::definitions(game).join(format!("{name}.json")))
                .unwrap_or_else(|e| panic!("{game} {name}: {e}"));
        }
    }
}

/// The groups the cross-game static table (`paths::group_tag_to_extension`,
/// built from every game's index with Halo 3 winning a FOURCC collision) gets
/// wrong for their own game: `Some(name)` where it names another game's group,
/// `None` where it has no entry. Anything that needs a game's group name has
/// to use that game's `tag_index`; this list is what goes wrong when it
/// doesn't.
#[test]
fn the_cross_game_group_table_is_wrong_for_these_groups_only() {
    const WRONG: &[(&str, &str, &str, Option<&str>)] = &[
        // (game, fourcc, the game's own name, what the table says)
        ("halo2_mcc", "gldf", "chocolate_mountain", Some("cheap_light")),
        ("halo2amp_mcc", "hsc*", "hsc", Some("scenario_hs_source_file")),
        ("halo2amp_mcc", "ldsc", "load_screen", Some("load_screen_globals")),
        ("halo4_mcc", "hsc*", "hsc", Some("scenario_hs_source_file")),
        ("halo4_mcc", "ldsc", "load_screen", Some("load_screen_globals")),
        // Campaign Evolved's own groups were never merged into the table.
        ("haloce_evolved", "remx", "encounter_remix", None),
        ("haloce_evolved", "saud", "scenario_zone_sets_audibility", None),
        ("haloce_evolved", "scpf", "scenario_pathfinding", None),
        ("haloce_evolved", "sczc", "scenario_ai_zones_cache", None),
        ("haloce_evolved", "simg", "simulation_globals", None),
        ("haloce_evolved", "skel", "skeleton_model", None),
        ("haloce_evolved", "skug", "skull_globals", None),
        ("haloce_evolved", "spvs", "scenario_zone_sets_pvs", None),
        ("haloce_mcc", "coll", "model_collision_geometry", Some("collision_model")),
        ("haloce_mcc", "fog ", "fog", Some("planar_fog")),
        ("haloce_mcc", "mode", "model", Some("render_model")),
        ("haloce_mcc", "rain", "weather_particle_system", Some("rain_definition")),
        ("haloce_mcc", "smet", "shader_transparent_meter", Some("structure_meta")),
    ];
    let mut found = BTreeSet::new();
    for (game, _) in GAMES {
        for (tag, name) in tag_index(game) {
            let group = blam_tags::parse_group_tag(&tag).unwrap();
            let table = blam_tags::paths::group_tag_to_extension(group);
            if table != Some(name.as_str()) {
                found.insert((game.to_string(), tag, name, table.map(str::to_owned)));
            }
        }
    }
    let expected: BTreeSet<(String, String, String, Option<String>)> = WRONG
        .iter()
        .map(|(game, tag, name, table)| {
            ((*game).to_owned(), (*tag).to_owned(), (*name).to_owned(), table.map(str::to_owned))
        })
        .collect();
    assert_eq!(found, expected);
}

/// Every `parent_tag` is a group of the same game, and every chain ends.
/// `merge_parent_schemas` stops silently at a parent it cannot find, so a
/// broken chain shows up only as an "unknown reference" in some child.
#[test]
fn every_parent_chain_resolves() {
    let mut problems = Vec::new();
    let mut with_parents = 0;
    for (game, _) in GAMES {
        let index: BTreeMap<String, String> = tag_index(game).into_iter().collect();
        let parents: BTreeMap<String, Option<String>> = index
            .iter()
            .map(|(tag, name)| {
                let group = read_json(&common::definitions(game).join(format!("{name}.json")));
                (tag.clone(), group["parent_tag"].as_str().map(str::to_owned))
            })
            .collect();
        for tag in index.keys() {
            let mut seen = BTreeSet::from([tag.clone()]);
            let mut current = parents[tag].clone();
            if current.is_some() {
                with_parents += 1;
            }
            while let Some(parent) = current {
                if !index.contains_key(&parent) {
                    problems.push(format!("{game} {tag}: parent {parent:?} is not a group"));
                    break;
                }
                if !seen.insert(parent.clone()) {
                    problems.push(format!("{game} {tag}: parent chain loops at {parent:?}"));
                    break;
                }
                current = parents[&parent].clone();
            }
            // The engine walks at most 32 ancestors.
            if seen.len() > 32 {
                problems.push(format!("{game} {tag}: chain is {} long", seen.len()));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(with_parents > 200, "only {with_parents} groups have a parent; the walk tested little");
}

/// Groups that cannot be built, with the reason. Empty today: every group of
/// every game builds a layout and a new tag. Kept as a list so that a
/// definitions update that breaks one has to name it here.
const UNBUILDABLE: &[(&str, &str, &str)] = &[];

/// Every group builds a `TagLayout` and a new `TagFile`, and the new tag's
/// header carries the generation of the folder it was built from.
///
/// The folder decides, not `_meta.json`'s `game`: Campaign Evolved's says
/// `halocampaignevolved`, which no table knows, and reading it left a new
/// Campaign Evolved tag's header zeroed (a header no shipped tag has).
#[test]
fn every_group_builds_a_layout_and_a_new_tag() {
    // The premise of the folder rule: exactly one game's declared name is not
    // its folder's.
    let renamed: Vec<(String, String)> = GAMES
        .iter()
        .filter_map(|(game, _)| {
            let declared = meta(game)["game"].as_str().unwrap().to_owned();
            (declared != *game).then(|| (game.to_string(), declared))
        })
        .collect();
    assert_eq!(
        renamed,
        [("haloce_evolved".to_owned(), "halocampaignevolved".to_owned())],
        "_meta.json game names"
    );

    let mut failures = BTreeSet::new();
    let mut headers = Vec::new();
    let mut built = 0;
    for (game, generation) in GAMES {
        for (tag, name) in tag_index(game) {
            let path = common::definitions(game).join(format!("{name}.json"));
            if let Err(error) = TagLayout::from_json(&path) {
                failures.insert((game.to_string(), tag.clone(), format!("layout: {error}")));
                continue;
            }
            match TagFile::new(&path) {
                Ok(new) => {
                    built += 1;
                    let got = (new.header.build_version, new.header.build_number, new.header.version);
                    if got != *generation {
                        headers.push(format!("{game} {tag}: header {got:?}, expected {generation:?}"));
                    }
                    let group = blam_tags::parse_group_tag(&tag).unwrap();
                    if new.header.group_tag != group {
                        headers.push(format!("{game} {tag}: header group {:08x}", new.header.group_tag));
                    }
                }
                Err(error) => {
                    failures.insert((game.to_string(), tag.clone(), format!("new: {error}")));
                }
            }
        }
    }
    let expected: BTreeSet<(String, String, String)> = UNBUILDABLE
        .iter()
        .map(|(game, tag, why)| ((*game).to_owned(), (*tag).to_owned(), (*why).to_owned()))
        .collect();
    assert_eq!(failures, expected, "groups that fail to build");
    assert!(headers.is_empty(), "{}", headers.join("\n"));
    assert!(built > 1400, "only {built} groups built");
}

/// Structs whose fields do not add up to their declared size:
/// `(struct, declared, sum of its fields)`. Every one holds a `tmpl` custom —
/// a template whose inherited fields (a render method: 64 bytes in ODST, 100
/// in Reach and Halo 4) are described by another group, which the engine
/// inlines when it builds the layout (`schema::fold_template_bases` and the
/// expansion in `build_layout_from_schema`). Halo 3 and Campaign Evolved
/// inline those fields in the dump itself. Anything new here is a dump the
/// engine has not been taught about. A data gap, not an engine failure.
const TEMPLATE_HOLES: &[(&str, u64, u64)] = &[
    ("halo2amp_mcc/particle_struct_definition", 568, 468),
    ("halo3odst_mcc/beam_definition_block", 500, 436),
    ("halo3odst_mcc/contrail_definition_block", 600, 536),
    ("halo3odst_mcc/decal_definition_block", 116, 52),
    ("halo3odst_mcc/light_volume_definition_block", 360, 296),
    ("halo3odst_mcc/particle_struct_definition", 404, 340),
    ("halo4_mcc/particle_struct_definition", 568, 468),
    ("haloreach_mcc/beam_definition_block", 548, 448),
    ("haloreach_mcc/contrail_definition_block", 640, 540),
    ("haloreach_mcc/decal_definition_block", 152, 52),
    ("haloreach_mcc/light_volume_definition_block", 436, 336),
    ("haloreach_mcc/particle_struct_definition", 496, 396),
];

/// The engine's name for a JSON field type: the dump's snake case with
/// spaces, except where the engine kept Bungie's older name.
fn canonical_type_name(ty: &str) -> String {
    match ty {
        "short_bounds" => "short integer bounds".to_owned(),
        "tag_resource" => "pageable resource".to_owned(),
        "tag_interop" => "api interop".to_owned(),
        "non_cache_runtime_value" => "non-cache runtime value".to_owned(),
        _ => ty.replace('_', " "),
    }
}

/// Sum each struct's fields from the JSON — nested structs and arrays at
/// their declared sizes, pads at their byte counts, everything else at the
/// width the engine's type table gives it — and compare with the struct's
/// declared size. Independent of the layout build, which fixes up template
/// holes and reorders the tables, so it checks the data and the type table
/// rather than the engine's arithmetic against itself.
#[test]
fn struct_sizes_add_up() {
    let mut mismatches = BTreeSet::new();
    let mut unexplained = Vec::new();
    let mut unknown_types = BTreeSet::new();
    let mut checked = 0;
    for (game, _) in GAMES {
        // The engine's widths, by type name, from a layout it built.
        let mut widths: BTreeMap<String, u32> = BTreeMap::new();
        let mut structs: BTreeMap<String, Value> = BTreeMap::new();
        let mut arrays: BTreeMap<String, Value> = BTreeMap::new();
        for (_, name) in tag_index(game) {
            let path = common::definitions(game).join(format!("{name}.json"));
            let layout = TagLayout::from_json(&path).unwrap();
            for field_type in &layout.field_types {
                if let Some(type_name) = layout.get_string(field_type.name_offset) {
                    widths.insert(type_name.to_owned(), field_type.size);
                }
            }
            let group = read_json(&path);
            for (key, value) in group["structs"].as_object().into_iter().flatten() {
                structs.insert(format!("{name}/{key}"), value.clone());
            }
            for (key, value) in group["arrays"].as_object().into_iter().flatten() {
                arrays.insert(format!("{name}/{key}"), value.clone());
            }
        }
        // A struct or array is resolved within its own group first, then
        // anywhere in the game (a child names its ancestors' definitions).
        let by_bare_name = |map: &BTreeMap<String, Value>, group: &str, bare: &str| -> Option<Value> {
            map.get(&format!("{group}/{bare}"))
                .or_else(|| map.iter().find(|(key, _)| key.split_once('/').unwrap().1 == bare).map(|(_, v)| v))
                .cloned()
        };
        for (key, body) in &structs {
            let (group, struct_name) = key.split_once('/').unwrap();
            let declared = body["size"].as_u64().unwrap();
            let mut sum = 0u64;
            let mut has_template = false;
            for field in body["fields"].as_array().unwrap() {
                let ty = field["type"].as_str().unwrap();
                let definition = &field["definition"];
                sum += match ty {
                    "struct" => by_bare_name(&structs, group, definition.as_str().unwrap())
                        .map_or(0, |s| s["size"].as_u64().unwrap()),
                    "array" => {
                        let array = by_bare_name(&arrays, group, definition.as_str().unwrap()).unwrap();
                        let element = by_bare_name(&structs, group, array["struct"].as_str().unwrap()).unwrap();
                        array["count"].as_u64().unwrap() * element["size"].as_u64().unwrap()
                    }
                    "pad" | "skip" => definition.as_u64().unwrap(),
                    "custom" => {
                        has_template |= field["group_tag"].as_str() == Some("tmpl");
                        0
                    }
                    // Occupy nothing in the MCC form (and in Halo 2 V4, the
                    // only Halo 2 form the classic reader sizes from the
                    // layout; older Halo 2 tags give `useless_pad` its length
                    // at read time).
                    "explanation" | "useless_pad" | "terminator" => 0,
                    _ => match widths.get(&canonical_type_name(ty)) {
                        Some(width) => u64::from(*width),
                        None => {
                            unknown_types.insert(format!("{game}: {ty}"));
                            0
                        }
                    },
                };
            }
            checked += 1;
            if sum != declared {
                if !has_template {
                    unexplained.push(format!("{game}/{struct_name}: declared {declared}, fields sum to {sum}"));
                }
                mismatches.insert((format!("{game}/{struct_name}"), declared, sum));
            }
        }
    }
    assert!(unknown_types.is_empty(), "types with no width: {unknown_types:?}");
    assert!(unexplained.is_empty(), "no template explains:\n{}", unexplained.join("\n"));
    let listed: BTreeSet<(String, u64, u64)> =
        TEMPLATE_HOLES.iter().map(|(name, declared, sum)| ((*name).to_owned(), *declared, *sum)).collect();
    assert_eq!(mismatches, listed, "structs whose fields do not add up");
    assert!(checked > 10_000, "only {checked} structs checked");
}
