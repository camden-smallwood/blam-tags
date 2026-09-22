//! The splitter, and the premise it rests on, against shipped content.
//!
//! The whole approach assumes one thing: **a JMS section is exactly a
//! distinct `(lod, permutation, region)` triple from the material
//! definition lines.** If that is wrong, every split lands in the wrong
//! place. The check is direct — group a shipped JMS that way, count the
//! groups, and compare against the mesh count of the `.render_model`
//! `tool.exe` actually built from it. Nothing about the reasoning is
//! taken on trust.
//!
//! Then the splitter itself: it must preserve every triangle and vertex,
//! leave in-budget files untouched, and leave nothing over budget.
//!
//! Skips gracefully with no kit installed. Point `BLAM_TEST_H3EK` at an
//! install, or let it find one in a Steam library.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use blam_tags::jms::JmsFile;
use blam_tags::jms_split::{
    split_oversized_sections, MaterialLabel, SplitBudget, SplitError,
};
use blam_tags::TagFile;

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

fn jms_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = blam_tags::convert::walk_files(root)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.len() >= 4 && n[n.len() - 4..].eq_ignore_ascii_case(".jms"))
        })
        .collect();
    out.sort();
    out
}

/// Count distinct sections the way the splitter does.
fn section_sizes(jms: &JmsFile) -> BTreeMap<(String, String, String), usize> {
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
    let mut out = BTreeMap::new();
    for tri in &jms.triangles {
        if let Some(label) = labels.get(tri.material as usize) {
            *out.entry(label.section_key()).or_insert(0usize) += 1;
        }
    }
    out
}

/// `data/<path>/render/x.jms` paired with `tags/<path>/y.render_model`,
/// where each side has exactly one candidate.
fn matched_pairs(kit: &Path) -> Vec<(PathBuf, PathBuf, String)> {
    let (data, tags) = (kit.join("data"), kit.join("tags"));
    let mut out = Vec::new();
    for jms in jms_files(&data) {
        let Some(dir) = jms.parent() else { continue };
        if !dir.file_name().is_some_and(|n| n.eq_ignore_ascii_case("render")) {
            continue;
        }
        let Some(parent) = dir.parent() else { continue };
        let Ok(rel) = parent.strip_prefix(&data) else { continue };
        let tag_dir = tags.join(rel);
        if !tag_dir.is_dir() {
            continue;
        }
        let siblings: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.len() >= 4 && n[n.len() - 4..].eq_ignore_ascii_case(".jms"))
            })
            .collect();
        let models: Vec<PathBuf> = std::fs::read_dir(&tag_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|e| e.to_str()).is_some_and(|e| e == "render_model")
            })
            .collect();
        if siblings.len() == 1 && models.len() == 1 {
            out.push((jms.clone(), models[0].clone(), rel.display().to_string()));
        }
    }
    out
}

/// **The premise.** Sections derived from material labels must equal the
/// meshes `tool.exe` built.
#[test]
fn material_labels_predict_the_built_section_count() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let pairs = matched_pairs(&kit);
    if pairs.is_empty() {
        eprintln!("skipping: no matched JMS/render_model pairs");
        return;
    }

    let mut agree = 0usize;
    let mut disagree: Vec<String> = Vec::new();
    for (jms_path, tag_path, name) in &pairs {
        let Ok(text) = std::fs::read_to_string(jms_path) else { continue };
        let Ok((jms, _)) = JmsFile::parse(&text) else { continue };
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let root = tag.root();
        let Some(meshes) =
            root.field_path("render geometry/meshes").and_then(|f| f.as_block())
        else {
            continue;
        };
        let predicted = section_sizes(&jms).len();
        if predicted == meshes.len() {
            agree += 1;
        } else {
            disagree.push(format!("{name}: predicted {predicted} sections, tag has {}", meshes.len()));
        }
    }

    eprintln!("section-count prediction: {agree} agree, {} disagree, of {} pairs",
        disagree.len(), pairs.len());
    for d in disagree.iter().take(10) {
        eprintln!("  {d}");
    }
    assert!(agree > 0, "no pairs were comparable — the harness is broken, not the premise");
    assert!(
        disagree.is_empty(),
        "{} of {} models disagree; the section rule is wrong somewhere:\n{}",
        disagree.len(),
        agree + disagree.len(),
        disagree.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}

/// Splitting preserves geometry exactly and respects the budget.
#[test]
fn splitting_the_corpus_preserves_everything() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let files = jms_files(&kit.join("data"));
    if files.is_empty() {
        eprintln!("skipping: no JMS files");
        return;
    }

    let budget = SplitBudget::default();
    let limit = budget.triangles_per_section();
    let mut untouched = 0usize;
    let mut changed: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    let mut largest_section = 0usize;
    let mut problems: Vec<String> = Vec::new();

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Ok((mut jms, _)) = JmsFile::parse(&text) else { continue };
        let name = path.strip_prefix(kit.join("data")).unwrap_or(path).display().to_string();

        let before = section_sizes(&jms);
        largest_section = largest_section.max(before.values().copied().max().unwrap_or(0));
        let (tris_before, verts_before) = (jms.triangles.len(), jms.vertices.len());
        let mats_before = jms.materials.len();

        match split_oversized_sections(&mut jms, &budget) {
            Ok(report) if !report.changed() => {
                untouched += 1;
                // A no-op must really be a no-op.
                if jms.materials.len() != mats_before {
                    problems.push(format!("{name}: unchanged report but materials grew"));
                }
            }
            Ok(report) => {
                changed.push(format!(
                    "{name}: {} section(s) split, {} -> {} sections, {} -> {} regions",
                    report.splits.len(),
                    report.sections_before,
                    report.sections_after,
                    report.regions_before,
                    report.regions_after
                ));
                if report.largest_section_triangles > limit {
                    problems.push(format!(
                        "{name}: still over budget after splitting ({} > {limit})",
                        report.largest_section_triangles
                    ));
                }
            }
            Err(SplitError::TooManyRegions { needed, .. }) => {
                refused.push(format!("{name}: would need {needed} regions"));
                continue;
            }
            Err(e) => {
                problems.push(format!("{name}: {e}"));
                continue;
            }
        }

        // Geometry is never touched, only which section owns it.
        if jms.triangles.len() != tris_before {
            problems.push(format!("{name}: triangle count changed"));
        }
        if jms.vertices.len() != verts_before {
            problems.push(format!("{name}: vertex count changed"));
        }
        for (i, t) in jms.triangles.iter().enumerate() {
            if t.material < 0 || t.material as usize >= jms.materials.len() {
                problems.push(format!("{name}: triangle {i} material {} dangling", t.material));
                break;
            }
        }
        // Splitting must never merge two sections into one.
        let after = section_sizes(&jms);
        if after.len() < before.len() {
            problems.push(format!("{name}: sections went from {} to {}", before.len(), after.len()));
        }
    }

    eprintln!("budget: {limit} source triangles per section (ratio {})", budget.vertices_per_triangle);
    eprintln!("largest section in the shipped corpus: {largest_section} triangles");
    eprintln!("{untouched} files untouched, {} split, {} refused", changed.len(), refused.len());
    for c in changed.iter().take(10) {
        eprintln!("  split  {c}");
    }
    for r in refused.iter().take(10) {
        eprintln!("  refuse {r}");
    }

    assert!(
        problems.is_empty(),
        "{} problems, first 10:\n{}",
        problems.len(),
        problems.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
    // Shipped content imports today, so almost none of it should need
    // splitting. If this ever drops sharply the default ratio has drifted
    // into over-splitting.
    assert!(
        untouched * 100 >= files.len() * 95,
        "only {untouched} of {} shipped files were left alone — the default ratio is \
         over-splitting content that already imports",
        files.len()
    );
}

/// A split file still parses, and its sections are all within budget.
#[test]
fn a_split_file_survives_a_write_and_reparse() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    // Force splitting on real content by shrinking the budget hard, so
    // this exercises the write path on genuine artist data rather than a
    // synthetic mesh.
    let budget = SplitBudget {
        max_vertices_per_section: 4_000,
        vertices_per_triangle: 1.6,
        ..Default::default()
    };
    let limit = budget.triangles_per_section();

    let mut exercised = 0usize;
    for path in jms_files(&kit.join("data")).into_iter().take(60) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok((mut jms, version)) = JmsFile::parse(&text) else { continue };
        let tris_before = jms.triangles.len();
        let Ok(report) = split_oversized_sections(&mut jms, &budget) else { continue };
        if !report.changed() {
            continue;
        }
        exercised += 1;

        let mut buf = Vec::new();
        jms.write(&mut buf, version).expect("split file must write");
        let text = String::from_utf8(buf).expect("writer emits UTF-8");
        let (back, _) = JmsFile::parse(&text).expect("split file must re-parse");

        assert_eq!(back.triangles.len(), tris_before, "{}: triangles lost", path.display());
        for (key, n) in section_sizes(&back) {
            assert!(n <= limit, "{}: section {key:?} has {n} triangles, over {limit}", path.display());
        }
    }
    eprintln!("write/reparse exercised on {exercised} split files");
    assert!(exercised > 0, "no file split even at a 4000-vertex budget — check the harness");
}
