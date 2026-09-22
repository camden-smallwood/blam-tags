//! Build `physics_model` tags from shipped sources and compare against
//! what `tool.exe` produced from the same bytes.
//!
//! Every shipped `.physics_model` carries its source JMS in the `info`
//! stream, so this is a matched comparison, not a synthetic one: same
//! input, and Tool's answer is already in the file.
//!
//! The counts we can compare are the ones a writer decides — how many
//! rigid bodies, materials, regions and shapes of each kind. Byte
//! equality is not the goal and is not reachable: Havok's hull differs
//! from ours at the margins, and two of the serialized header fields are
//! live pointers.
//!
//! Skips gracefully with no kit installed. Point `BLAM_TEST_H3EK` at an
//! install, or let it find one in a Steam library.

use std::path::{Path, PathBuf};

use blam_tags::jms::JmsFile;
use blam_tags::physics_import::{physics_model_from_jms, PhysicsError, PhysicsOptions};
use blam_tags::TagFile;
use flate2::read::ZlibDecoder;

fn h3ek() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("BLAM_TEST_H3EK") {
        let path = PathBuf::from(path);
        return path.is_dir().then_some(path);
    }
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "C:/Program Files/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK"))
    .find(|path| path.join("data").is_dir() && path.join("tags").is_dir())
}

fn schema() -> Option<PathBuf> {
    // Integration tests run with the *crate* as cwd, so reach the
    // workspace-level `definitions/` through the manifest directory —
    // same convention as `tmpl_layout_ground_truth.rs`.
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/physics_model.json");
    p.exists().then_some(p)
}

fn walk(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x == ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Recover the single source JMS a tag was built from, if it has one.
fn source_jms(tag: &TagFile) -> Option<JmsFile> {
    let info = tag.import_info()?;
    let files = info.field("files").and_then(|f| f.as_block())?;
    let mut found: Vec<Vec<u8>> = Vec::new();
    for file in files.iter() {
        if !file.read_string("path").unwrap_or_default().to_ascii_lowercase().ends_with(".jms") {
            continue;
        }
        let Some(z) = file.field("zipped data").and_then(|f| f.as_data()) else { continue };
        let mut out = Vec::new();
        if std::io::Read::read_to_end(&mut ZlibDecoder::new(z), &mut out).is_ok() {
            found.push(out);
        }
    }
    if found.len() != 1 {
        return None;
    }
    let text = String::from_utf8(found.remove(0)).ok()?;
    JmsFile::parse(&text).ok().map(|(j, _)| j)
}

fn block_len(tag: &TagFile, name: &str) -> usize {
    tag.root().field_path(name).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0)
}

#[test]
fn we_rebuild_shipped_physics_models_from_their_own_source() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and definitions/halo3_mcc/physics_model.json");
        return;
    };

    let opts = PhysicsOptions::default();
    let mut compared = 0usize;
    let mut mopp_refused = 0usize;
    let mut other_refused: Vec<String> = Vec::new();
    let (mut bodies_ok, mut mats_ok, mut regions_ok, mut shapes_ok) = (0, 0, 0, 0);
    let mut rows: Vec<String> = Vec::new();

    for tag_path in walk(&kit.join("tags"), "physics_model").iter().take(150) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };

        let want_bodies = block_len(&tag, "rigid bodies");
        let want_mats = block_len(&tag, "materials");
        let want_regions = block_len(&tag, "regions");
        let want_shapes = block_len(&tag, "spheres")
            + block_len(&tag, "pills")
            + block_len(&tag, "boxes")
            + block_len(&tag, "polyhedra");
        if want_shapes == 0 {
            continue;
        }

        match physics_model_from_jms(&jms, &schema, &opts) {
            Ok((built, report)) => {
                compared += 1;
                let got_shapes =
                    report.spheres + report.pills + report.boxes + report.polyhedra;
                if report.rigid_bodies == want_bodies {
                    bodies_ok += 1;
                }
                if report.materials == want_mats {
                    mats_ok += 1;
                }
                if report.regions == want_regions {
                    regions_ok += 1;
                }
                if got_shapes == want_shapes {
                    shapes_ok += 1;
                }
                // The tag we built must survive a write and re-read.
                let bytes = built.write_to_bytes().expect("a built tag must serialize");
                let back = TagFile::read_from_bytes(&bytes)
                    .unwrap_or_else(|e| panic!("{}: rebuilt tag does not parse: {e}",
                        tag_path.display()));
                assert_eq!(
                    block_len(&back, "rigid bodies"),
                    report.rigid_bodies,
                    "{}: rigid bodies lost in the round trip",
                    tag_path.display()
                );
                if rows.len() < 12 {
                    rows.push(format!(
                        "{:<40} bodies {}/{}  mats {}/{}  regions {}/{}  shapes {}/{}",
                        tag_path.file_name().unwrap_or_default().to_string_lossy(),
                        report.rigid_bodies, want_bodies,
                        report.materials, want_mats,
                        report.regions, want_regions,
                        got_shapes, want_shapes
                    ));
                }
                mopp_refused += report.bodies_without_mopp;
            }
            Err(e) => other_refused.push(format!(
                "{}: {e}",
                tag_path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }

    eprintln!("rebuilt {compared} physics_models from their own source");
    eprintln!("  bodies shipping a list with no MOPP (tool would compile one): {mopp_refused}");
    eprintln!("  refused for other reasons: {}", other_refused.len());
    for r in other_refused.iter().take(8) {
        eprintln!("    {r}");
    }
    if compared > 0 {
        let pc = |x: usize| 100.0 * x as f64 / compared as f64;
        eprintln!("  rigid body count matches: {bodies_ok}/{compared} ({:.0}%)", pc(bodies_ok));
        eprintln!("  material count matches:   {mats_ok}/{compared} ({:.0}%)", pc(mats_ok));
        eprintln!("  region count matches:     {regions_ok}/{compared} ({:.0}%)", pc(regions_ok));
        eprintln!("  shape count matches:      {shapes_ok}/{compared} ({:.0}%)", pc(shapes_ok));
    }
    for r in &rows {
        eprintln!("  {r}");
    }

    assert!(compared > 0, "nothing was rebuilt — the harness is broken, not the writer");
    // A failure that is not "needs a MOPP" means the writer hit something
    // it did not understand, which is a defect rather than a limitation.
    assert!(
        other_refused.is_empty(),
        "{} models failed for reasons other than MOPP:\n{}",
        other_refused.len(),
        other_refused.iter().take(5).cloned().collect::<Vec<_>>().join("\n")
    );
    // Shape count is the one we fully control: every non-null primitive
    // in the source must appear in the tag.
    assert!(
        shapes_ok * 10 >= compared * 9,
        "shape counts should match on almost every model: {shapes_ok}/{compared}"
    );
}

/// A built tag must be structurally valid on its own terms.
#[test]
fn a_built_tag_has_the_right_header_and_survives_a_round_trip() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and the definitions");
        return;
    };
    let Some(jms_path) = walk(&kit.join("data"), "JMS")
        .into_iter()
        .find(|p| p.parent().is_some_and(|d| d.ends_with("physics")))
    else {
        eprintln!("skipping: no physics JMS found");
        return;
    };
    let text = std::fs::read_to_string(&jms_path).expect("read the JMS");
    let (jms, _) = JmsFile::parse(&text).expect("parse the JMS");

    let (tag, report) = match physics_model_from_jms(&jms, &schema, &PhysicsOptions::default()) {
        Ok(v) => v,
        Err(e) => panic!("{}: {e}", jms_path.display()),
    };
    eprintln!("{}: {report:?}", jms_path.display());

    let bytes = tag.write_to_bytes().expect("serialize");
    let back = TagFile::read_from_bytes(&bytes).expect("re-read");
    let group = back.group();
    assert_eq!(blam_tags::format_group_tag(group.tag), "phmo");
    assert!(report.rigid_bodies > 0, "a physics model needs at least one rigid body");
    assert!(block_len(&back, "nodes") > 0, "and at least one node");
}

/// Shapes must land where the artist put them.
///
/// Counts cannot catch a missing transform — a shape at the origin is
/// still a shape — so compare actual positions against the shipped tag.
/// This is the check that was missing when the writer silently placed
/// every sphere at the origin; clippy caught that, a test should.
#[test]
fn shape_positions_match_the_shipped_tag() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and the definitions");
        return;
    };

    /// Every sphere's world position in a physics_model, sorted so the
    /// comparison does not depend on emission order.
    fn sphere_positions(tag: &TagFile) -> Vec<[f32; 3]> {
        let mut out = Vec::new();
        if let Some(b) = tag.root().field_path("spheres").and_then(|f| f.as_block()) {
            for el in b.iter() {
                if let Some(ts) = el.descend("translate shape") {
                    let t = ts.read_vec3("translation");
                    out.push([t.i, t.j, t.k]);
                }
            }
        }
        out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    let mut checked = 0usize;
    let mut matched = 0usize;
    let mut worst = 0.0f32;
    let mut examples: Vec<String> = Vec::new();

    for tag_path in walk(&kit.join("tags"), "physics_model").iter().take(150) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let want = sphere_positions(&tag);
        if want.is_empty() {
            continue;
        }
        let Some(jms) = source_jms(&tag) else { continue };
        let Ok((built, _)) = physics_model_from_jms(&jms, &schema, &PhysicsOptions::default())
        else {
            continue;
        };
        let got = sphere_positions(&built);
        if got.len() != want.len() {
            continue;
        }
        checked += 1;
        let mut worst_here = 0.0f32;
        for (a, b) in got.iter().zip(&want) {
            for k in 0..3 {
                worst_here = worst_here.max((a[k] - b[k]).abs());
            }
        }
        worst = worst.max(worst_here);
        if worst_here < 1e-4 {
            matched += 1;
        } else if examples.len() < 6 {
            examples.push(format!(
                "{}: worst axis error {worst_here:.6}",
                tag_path.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }

    eprintln!("sphere positions checked on {checked} models, {matched} match within 1e-4");
    eprintln!("worst axis error across all: {worst:.8}");
    for e in &examples {
        eprintln!("  {e}");
    }
    assert!(checked > 0, "no model had spheres to compare — the harness is broken");
    assert!(
        matched * 10 >= checked * 9,
        "sphere positions should match on almost every model: {matched}/{checked}"
    );
}
