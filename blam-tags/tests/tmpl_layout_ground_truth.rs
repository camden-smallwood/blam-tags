//! What the editing kits actually ship for a `tmpl` group, measured.
//!
//! A `tmpl` custom field stands in for a template group's ancestor body — for the
//! fx groups that is `render_method_struct_definition`, 100 bytes in Reach and 64
//! in Halo 3. `TagLayout::from_json` accounts for it as a *byte count* stored in
//! the field's `definition` slot (`schema.rs`, `tmpl_expansion_size`), which
//! leaves those bytes anonymous: nothing names them, `WireClass::of(Custom)`
//! skips them when two games are compared, and a `custom` field has no field
//! index for the render method's `options`/`parameters`/`postprocess` blocks to
//! hang a sub-chunk from.
//!
//! That looks like something to fix by splicing the ancestor struct in instead.
//! It is not — or at least not in the layout a tag is *built* with. These tests
//! record why, so the next attempt does not repeat it:
//!
//! 1. A shipped Reach particle's own `blay` **also** keeps the render method as
//!    an unnamed slot. Splicing a named `shader` struct into the JSON layout adds
//!    a field the kit does not have, which is exactly the divergence
//!    `NATIVE_LAYOUT_TEMPLATE_GROUPS` exists to work around in Baboon.
//! 2. The dumped JSON does not agree with the shipped layout for `particle` in
//!    the first place — a real H3EK particle root is 424 bytes where
//!    `halo3_mcc/particle.json` declares 404.
//!
//! So making the template body addressable has to happen *beside* the built
//! layout (as metadata the comparison and conversion layers can read), not by
//! changing the layout itself.
//!
//! Self-skips without a kit. A green run on a machine with no kits proves
//! nothing here.

use std::path::PathBuf;

use blam_tags::{TagFieldType, TagFile};

fn kit_tags(env_var: &str, kit: &str) -> Option<PathBuf> {
    if let Ok(path) = std::env::var(env_var) {
        let path = PathBuf::from(path);
        return path.is_dir().then_some(path);
    }
    [
        "C:/Program Files (x86)/Steam/steamapps/common",
        "C:/Program Files/Steam/steamapps/common",
        "D:/SteamLibrary/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join(kit).join("tags"))
    .find(|path| path.is_dir())
}

fn definitions() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions")
}

/// First tag with `extension` anywhere under `tags`, searched depth-first so the
/// same kit always yields the same tag.
fn find_one(tags: &PathBuf, extension: &str) -> Option<PathBuf> {
    let mut stack = vec![tags.clone()];
    let mut best: Option<PathBuf> = None;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut names: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        for path in names {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension)
                && best.as_ref().is_none_or(|b| &path < b)
            {
                best = Some(path);
            }
        }
    }
    best
}

/// Halo Reach's own particle layout leaves the render method unnamed.
///
/// If this ever starts failing because a `shader` struct appeared, the kits
/// changed their mind and splicing becomes the right answer after all.
#[test]
fn a_shipped_reach_particle_keeps_its_render_method_in_an_unnamed_slot() {
    let Some(tags) = kit_tags("BLAM_TEST_HREK", "HREK") else {
        eprintln!("skipping: no HREK editing kit (set BLAM_TEST_HREK to its `tags` directory)");
        return;
    };
    let Some(path) = find_one(&tags, "particle") else {
        eprintln!("skipping: HREK has no .particle tag under {}", tags.display());
        return;
    };
    let tag = TagFile::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

    let struct_fields: Vec<String> = tag
        .root()
        .fields()
        .filter(|field| field.field_type() == TagFieldType::Struct)
        .map(|field| field.name().to_owned())
        .collect();
    assert!(
        struct_fields.iter().any(|name| name.starts_with("actual shader")),
        "{}: expected an `actual shader?` struct, got {struct_fields:?}",
        path.display()
    );
    assert!(
        !struct_fields.iter().any(|name| name == "shader"),
        "{}: the shipped layout now names the render method `shader` — splicing \
         it into the JSON layout would agree with the kit after all. Fields: \
         {struct_fields:?}",
        path.display()
    );
    // The bytes are there either way; only the naming differs.
    assert_eq!(tag.root().definition().size(), 496, "{}", path.display());
}

/// Every `.particle` under `tags`, depth-first.
fn find_all(tags: &PathBuf, extension: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![tags.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for path in entries.flatten().map(|e| e.path()) {
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some(extension) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A kit ships one group at several layout revisions, so "the" shipped size is
/// not a single number.
///
/// This started as a claim that the dumped `halo3_mcc/particle.json` was 20
/// bytes short of what H3EK ships — one tag's root really is 424 where the JSON
/// declares 404. Surveying the whole kit shows why that was the wrong reading:
/// H3EK's particles carry **more than one** root size, because every tag embeds
/// the `blay` it was authored against and the group grew over the kit's life.
///
/// What the dump has to satisfy, then, is that it matches one of the revisions
/// actually in use — not all of them. Anything that changes the JSON-built size
/// (a `tmpl` splice, say) has to keep that true, which is what this pins.
#[test]
fn h3ek_ships_particles_at_several_root_sizes_and_the_dump_matches_one() {
    let Some(tags) = kit_tags("BLAM_TEST_H3EK", "H3EK") else {
        eprintln!("skipping: no H3EK editing kit (set BLAM_TEST_H3EK to its `tags` directory)");
        return;
    };
    let paths = find_all(&tags, "particle");
    if paths.is_empty() {
        eprintln!("skipping: H3EK has no .particle tag under {}", tags.display());
        return;
    }

    let mut sizes: std::collections::BTreeMap<usize, usize> = Default::default();
    let mut unreadable = 0usize;
    for path in &paths {
        match TagFile::read(path) {
            Ok(tag) => *sizes.entry(tag.root().definition().size()).or_default() += 1,
            Err(_) => unreadable += 1,
        }
    }
    eprintln!(
        "H3EK .particle: {} tags, {unreadable} unreadable, root sizes {sizes:?}",
        paths.len()
    );

    let schema = definitions().join("halo3_mcc/particle.json");
    let built = TagFile::new(&schema).unwrap_or_else(|e| panic!("{}: {e}", schema.display()));
    let built_size = built.root().definition().size();
    assert_eq!(built_size, 404, "the dumped schema changed");
    assert!(
        sizes.contains_key(&built_size),
        "the dumped Halo 3 particle root is {built_size} bytes, which no shipped \
         tag uses; H3EK ships {sizes:?}"
    );
    assert!(
        sizes.len() > 1,
        "expected H3EK to ship more than one particle layout revision, saw {sizes:?}"
    );
}
/// The layout now says *which* template a hole stands for, not just how wide.
///
/// This is the metadata half of the problem this file records: the bytes stay an
/// anonymous range in the layout a tag is built with — no kit divergence — but a
/// comparison or a converter can finally ask what fills them.
///
/// Halo 3 and Reach resolve the **same** template, `?rmp`, at **different**
/// widths: 64 bytes against 100. That is the fact worth having. Without it a
/// comparison skipped the hole entirely and reported Halo 3's whole render method
/// as absent from Reach; with it, the two are visibly the same body at two sizes,
/// which is a reinterpretation problem rather than a missing-data one.
#[test]
fn a_tmpl_hole_records_which_template_fills_it() {
    let holes = |game: &str, group: &str| {
        let path = definitions().join(game).join(format!("{group}.json"));
        let layout = blam_tags::TagLayout::from_json(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        layout
            .tmpl_holes
            .iter()
            .map(|hole| (blam_tags::format_group_tag(hole.group_tag), hole.size))
            .collect::<Vec<_>>()
    };

    assert_eq!(holes("halo3_mcc", "particle"), vec![("?rmp".to_owned(), 64)]);
    assert_eq!(holes("haloreach_mcc", "particle"), vec![("?rmp".to_owned(), 100)]);
    // Each fx group has its own render-method template, so the identity really
    // does distinguish them rather than being one constant wearing three hats.
    assert_eq!(holes("haloreach_mcc", "beam_system"), vec![("rmb".to_owned(), 100)]);
    assert_eq!(holes("haloreach_mcc", "decal_system"), vec![("rmd".to_owned(), 100)]);
}

/// A template that expands to nothing is still recorded.
///
/// Halo 4's `particle` carries two holes: `mat` at zero bytes and `?rmp` at 100.
/// A zero-width hole occupies no space and so cannot be found by arithmetic, but
/// it names a template, and "no such template" and "a template with no inherited
/// body" are different claims. Recording only the non-zero ones would have made
/// them indistinguishable.
#[test]
fn a_zero_width_template_hole_is_still_named() {
    let path = definitions().join("halo4_mcc/particle.json");
    let layout = blam_tags::TagLayout::from_json(&path).unwrap();
    let named = layout
        .tmpl_holes
        .iter()
        .map(|hole| (blam_tags::format_group_tag(hole.group_tag), hole.size))
        .collect::<Vec<_>>();
    assert_eq!(named, vec![("mat".to_owned(), 0), ("?rmp".to_owned(), 100)]);
}

/// A layout parsed from a shipped tag's own `blay` has no template metadata.
///
/// Asserted rather than left implicit, because the emptiness is the honest answer
/// and not an oversight: a `blay` records that a hole exists and how wide it is,
/// but never which template it came from. Anything that wants the identity has to
/// go back to the schema. Kit-gated — it needs a real tag.
#[test]
fn a_shipped_layout_carries_no_template_metadata() {
    let Some(reach) = kit_tags("BLAM_TEST_HREK", "HREK") else {
        eprintln!("skipping: no HREK");
        return;
    };
    let Some(path) = find_one(&reach, "particle") else {
        eprintln!("skipping: HREK ships no .particle");
        return;
    };
    let tag = TagFile::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let customs: Vec<_> = tag
        .root()
        .definition()
        .fields()
        .filter(|field| field.field_type() == TagFieldType::Custom)
        .collect();
    // The hole itself is still there — the bytes did not go away.
    assert!(
        !customs.is_empty(),
        "the shipped particle root should still carry its custom hole"
    );
    assert!(
        customs.iter().all(|field| field.template_hole().is_none()),
        "a blay-built layout should claim no template provenance"
    );
}
