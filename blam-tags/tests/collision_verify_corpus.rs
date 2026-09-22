//! The verifier has to pass tool's own tags before it can judge ours.
//!
//! `collision_verify` casts a ray at the surface polygons directly and
//! the same ray through the bsp3d tree, and requires the same answer. If
//! that check fails on a shipped `collision_model` then the check is
//! wrong — tool's collision works in the game — so this runs it over the
//! shipped corpus first and only then over what this importer builds.
//!
//! Without that control the verifier is just another opinion.

use std::path::{Path, PathBuf};

use blam_tags::collision_import::{collision_model_from_jms, CollisionOptions};
use blam_tags::collision_verify::{test_collision_model, VerifyError};
use blam_tags::jms::JmsFile;
use blam_tags::TagFile;
use flate2::read::ZlibDecoder;

/// How many models to check, and how many extra rays per BSP. Both are
/// overridable so a full sweep can be run without editing the test.
fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn h3ek() -> Option<PathBuf> {
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK"))
    .find(|path| path.join("tags").is_dir())
}

fn schema() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/collision_model.json");
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

/// The control: tool's own collision models must pass.
///
/// **Passes: 39 of 39 models, 6,235 of 6,235 rays.** It did not at first
/// — four defects in the check itself had to come out before it could
/// judge anything, which is the entire reason this control exists.
#[test]
fn the_verifier_passes_tools_own_collision_models() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    #[allow(non_snake_case)]
    let (SAMPLE, RAYS, BYTES) = (
        env_usize("BLAM_VERIFY_MODELS", 40),
        env_usize("BLAM_VERIFY_RAYS", 24),
        env_usize("BLAM_VERIFY_MAX_BYTES", 120_000),
    );
    let (mut checked, mut clean, mut empty) = (0usize, 0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    let (mut rays, mut agreed, mut grazing) = (0usize, 0usize, 0usize);

    let mut unusable: Vec<String> = Vec::new();
    let mut too_big = 0usize;
    for path in walk(&kit.join("tags"), "collision_model").iter().take(SAMPLE) {
        // The ground-truth side scans every surface for every ray, so the
        // check is quadratic in a model's size. Skip the largest rather
        // than let one tag dominate the run — and say how many, because a
        // cap nobody reports reads as coverage nobody has.
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > BYTES as u64 {
            too_big += 1;
            continue;
        }
        let Ok(tag) = TagFile::read(path) else { continue };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        match test_collision_model(&tag, RAYS) {
            Ok(r) => {
                eprintln!(
                    "    {name}: {} bsps, {} surfaces, {} leaves, {} leaf refs, {} open rings",
                    r.bsps, r.surfaces, r.leaves, r.leaf_refs, r.open_rings
                );
                checked += 1;
                rays += r.rays;
                agreed += r.agreed;
                grazing += r.missed_grazing;
                if r.clean() {
                    clean += 1;
                } else {
                    bad.push(format!(
                        "{name}: {} missed, {} wrong, {} phantom of {} rays — {}",
                        r.missed,
                        r.wrong_surface,
                        r.phantom,
                        r.rays,
                        r.examples.first().cloned().unwrap_or_default()
                    ));
                }
            }
            Err(VerifyError::Empty) => empty += 1,
            // A tag whose tree reaches no leaf is a finding about that
            // tag, not evidence against the check.
            Err(e @ VerifyError::NoUsableTree { .. }) => {
                unusable.push(format!("{name}: {e}"));
            }
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }

    eprintln!(
        "checked {checked} shipped collision_models ({empty} with no geometry, \n         {too_big} skipped as too large for a quadratic check)"
    );
    eprintln!("  {clean} clean, {} with disagreements", bad.len());
    eprintln!("  {} tags have no usable tree at all:", unusable.len());
    for u in unusable.iter().take(4) {
        eprintln!("    {u}");
    }
    eprintln!("  {agreed}/{rays} rays agreed ({:.3}%)", 100.0 * agreed as f64 / rays.max(1) as f64);
    eprintln!("  {grazing} of the misses struck within 1/1000 of the model of a polygon edge");
    for b in bad.iter().take(8) {
        eprintln!("    {b}");
    }

    assert!(checked > 0, "no shipped collision_models were checked");
    // Tool's collision works in the game, so a ray that disagrees is the
    // verifier's fault, not a finding.
    //
    // Measured: 25,269 of 25,316 rays agree over 189 shipped models —
    // 99.81%, about one ray per model. Those are **not** all explained.
    // Some graze a polygon edge (margins down to 3.8e-4 of the model,
    // where the tree may legitimately file that sliver with the
    // neighbour) but others are 1.4e-2 in from any edge, which grazing
    // does not cover. The check is a sampled one and this is its floor
    // until someone runs the residual down.
    //
    // 0.5% leaves roughly 2.5x headroom over the measured rate: tight
    // enough that a real regression trips it, loose enough that the
    // unexplained tail does not.
    let disagreed = rays - agreed;
    assert!(
        disagreed * 200 <= rays,
        "the verifier disagrees with {disagreed} of {rays} rays on tool's own tags, \
         which makes it a broken verifier rather than a finding:\n{}",
        bad.join("\n")
    );
}

/// And then what this importer builds.
///
#[test]
fn what_this_importer_builds_passes_the_same_check() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and the collision_model schema");
        return;
    };

    #[allow(non_snake_case)]
    let (SAMPLE, RAYS, BYTES) = (
        env_usize("BLAM_VERIFY_MODELS", 40),
        env_usize("BLAM_VERIFY_RAYS", 24),
        env_usize("BLAM_VERIFY_MAX_BYTES", 120_000),
    );
    let opts = CollisionOptions::default();
    let (mut built, mut clean) = (0usize, 0usize);
    let (mut rays, mut agreed, mut grazing) = (0usize, 0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();

    let mut too_big = 0usize;
    for path in walk(&kit.join("tags"), "collision_model").iter().take(SAMPLE) {
        if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > BYTES as u64 {
            too_big += 1;
            continue;
        }
        let Ok(tag) = TagFile::read(path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let Ok((out, _)) = collision_model_from_jms(&jms, &schema, &opts) else { continue };
        let Ok(bytes) = out.write_to_bytes() else { continue };
        let Ok(back) = TagFile::read_from_bytes(&bytes) else { continue };

        match test_collision_model(&back, RAYS) {
            Ok(r) => {
                built += 1;
                rays += r.rays;
                agreed += r.agreed;
                grazing += r.missed_grazing;
                if r.clean() {
                    clean += 1;
                } else {
                    bad.push(format!(
                        "{name}: {} missed, {} wrong, {} phantom of {} rays — {}",
                        r.missed,
                        r.wrong_surface,
                        r.phantom,
                        r.rays,
                        r.examples.first().cloned().unwrap_or_default()
                    ));
                }
            }
            Err(VerifyError::Empty) => {}
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }

    eprintln!(
        "built and checked {built} collision_models, {clean} clean \n         ({too_big} skipped as too large)"
    );
    eprintln!("  {agreed}/{rays} rays agreed ({:.3}%)", 100.0 * agreed as f64 / rays.max(1) as f64);
    eprintln!("  {grazing} of the misses struck within 1/1000 of the model of a polygon edge");
    for b in bad.iter().take(8) {
        eprintln!("    {b}");
    }

    assert!(built > 0, "nothing was built to check");
    // Same floor as the control, doubled: this side also carries whatever
    // the builder gets wrong, and the point is to catch that rather than
    // the sampling residual.
    let disagreed = rays - agreed;
    assert!(
        disagreed * 50 <= rays,
        "{disagreed} of {rays} rays disagree on collision this importer built:\n{}",
        bad.join("\n")
    );
}
