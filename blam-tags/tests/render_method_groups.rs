//! `RenderMethod::from_tag` accepts exactly the groups that inherit
//! `render_method`, in every gen3+ game's definitions.
//!
//! It used to accept a fixed list of Halo 3's subclasses, so ODST's and
//! Reach's `shader_screen`, Reach's glass, fur and mux shaders and H4's
//! waterfall were refused, and every tool built on it treated them as not
//! shaders. A new tag of every group is parsed here: one whose definition
//! names `rm  ` as its parent (or is `rm  ` itself) must parse, and carry a
//! material name for each `material name` field its root declares; every
//! other group, `render_method_definition` and its kin included, must be
//! refused.

mod common;

use blam_tags::render_method::{RenderMethod, RenderMethodError};
use common::synthetic::{classic_engine, groups, new_tag};

/// Every definitions folder of a game whose tags are gen3+ files.
fn gen3_games() -> Vec<String> {
    let mut games: Vec<String> = std::fs::read_dir(common::definitions_root())
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            (entry.path().join("_meta.json").is_file() && classic_engine(&name).is_none())
                .then_some(name)
        })
        .collect();
    games.sort();
    games
}

/// A group's definition: its parent group, and how many `material name`
/// string ids its root struct declares.
fn definition(game: &str, group: &str) -> (String, usize) {
    let path = common::definitions(game).join(format!("{group}.json"));
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let parent = json["parent_tag"].as_str().unwrap_or_default().to_owned();
    let root = json["blocks"][json["block"].as_str().unwrap()]["struct"].as_str().unwrap();
    let material_names = json["structs"][root]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|field| {
            let name = field["name"].as_str().unwrap_or_default();
            field["type"] == "string_id"
                && (name == "material name" || name.starts_with("material name "))
        })
        .count();
    (parent, material_names)
}

#[test]
fn every_render_method_subclass_parses_and_nothing_else_does() {
    let games = gen3_games();
    assert!(games.len() >= 4, "found only {games:?}");
    let mut subclasses = 0;
    let mut failures = Vec::new();
    for game in &games {
        for (fourcc, group) in groups(game) {
            let (parent, material_names) = definition(game, &group);
            let inherits = fourcc == "rm  " || parent == "rm  ";
            let tag = new_tag(game, &group);
            match (inherits, RenderMethod::from_tag(&tag)) {
                (true, Ok(render_method)) => {
                    subclasses += 1;
                    let expected = if fourcc == "rm  " { 0 } else { material_names };
                    if render_method.material_names.len() != expected {
                        failures.push(format!(
                            "{game} {group}: {} material names, its root declares {expected}",
                            render_method.material_names.len()
                        ));
                    }
                }
                (true, Err(error)) => failures.push(format!("{game} {group}: refused: {error}")),
                (false, Ok(_)) => failures.push(format!("{game} {group}: parsed, parent {parent:?}")),
                (false, Err(RenderMethodError::WrongGroup { .. })) => {}
                (false, Err(error)) => failures.push(format!("{game} {group}: {error}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // Halo 3 alone has 14; Reach's and H4's subclasses must have been seen.
    assert!(subclasses > 60, "only {subclasses} render method groups across {games:?}");
}
