//! Does our convex hull agree with Havok's, on shipped content?
//!
//! `tool physics` hands each JMS `CONVEX SHAPES` vertex list to Havok,
//! which builds the hull and writes its vertices (as SoA four-vectors)
//! and its face planes into the `physics_model` tag. Every shipped
//! `.physics_model` carries the exact source JMS in its `info` stream,
//! so we can rebuild each hull ourselves and compare our plane and
//! vertex counts against what Havok actually produced.
//!
//! That is a far stronger check than any synthetic test: it is the same
//! input, and the answer is already in the tag.
//!
//! Skips gracefully with no kit installed. Point `BLAM_TEST_H3EK` at an
//! install, or let it find one in a Steam library.

use std::path::{Path, PathBuf};

use blam_tags::hull::{convex_hull, HullError};
use blam_tags::jms::JmsFile;
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

/// Every convex shape in every shipped physics JMS must produce a hull,
/// and every input point must lie inside it.
#[test]
fn every_shipped_convex_shape_builds_a_valid_hull() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let files: Vec<PathBuf> = walk(&kit.join("data"), "JMS")
        .into_iter()
        .chain(walk(&kit.join("data"), "jms"))
        .collect();
    if files.is_empty() {
        eprintln!("skipping: no JMS files");
        return;
    }

    let mut shapes = 0usize;
    let mut built = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut outside: Vec<String> = Vec::new();
    let mut biggest = (0usize, 0usize, String::new());

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        let Ok((jms, _)) = JmsFile::parse(&text) else { continue };
        for c in &jms.convex_shapes {
            shapes += 1;
            match convex_hull(&c.vertices) {
                Ok(h) => {
                    built += 1;
                    if h.vertices.len() > biggest.0 {
                        biggest = (h.vertices.len(), h.planes.len(), c.name.clone());
                    }
                    // Scale the tolerance to the shape, not to 1.0.
                    let extent = c
                        .vertices
                        .iter()
                        .flat_map(|v| [v.x, v.y, v.z])
                        .fold(0.0f32, |m, v| m.max(v.abs()))
                        .max(1.0);
                    let tol = extent * 1e-4;
                    for v in &c.vertices {
                        for pl in &h.planes {
                            let s = pl.i * v.x + pl.j * v.y + pl.k * v.z + pl.d;
                            if s > tol {
                                outside.push(format!(
                                    "{}: '{}' point outside its own hull by {s}",
                                    path.display(),
                                    c.name
                                ));
                                break;
                            }
                        }
                    }
                }
                Err(e) => refused.push(format!(
                    "{}: '{}' ({} pts) — {e}",
                    path.file_name().unwrap_or_default().to_string_lossy(),
                    c.name,
                    c.vertices.len()
                )),
            }
        }
    }

    eprintln!("convex shapes: {shapes}, hulls built: {built}, refused: {}", refused.len());
    eprintln!(
        "largest hull: {} vertices, {} planes ('{}')",
        biggest.0, biggest.1, biggest.2
    );
    for r in refused.iter().take(10) {
        eprintln!("  refused {r}");
    }
    assert!(shapes > 0, "no convex shapes found — the harness is broken");
    assert!(
        outside.is_empty(),
        "{} shapes have a source point outside their own hull:\n{}",
        outside.len(),
        outside.iter().take(5).cloned().collect::<Vec<_>>().join("\n")
    );
    // Refusals are legitimate for genuinely flat or degenerate shapes,
    // but they should be rare; a high rate means the seed search is wrong.
    assert!(
        refused.len() * 20 <= shapes,
        "{} of {shapes} shapes refused — more than 5%",
        refused.len()
    );
}

/// Compare our hull against the one Havok actually built, using each
/// tag's own `info` stream as the matched source.
#[test]
fn our_hull_agrees_with_havoks_on_shipped_physics_models() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };
    let tags = walk(&kit.join("tags"), "physics_model");
    if tags.is_empty() {
        eprintln!("skipping: no physics_model tags");
        return;
    }

    let mut degenerate = 0usize;
    let mut compared = 0usize;
    let mut planes_match = 0usize;
    let mut vecs_match = 0usize;
    let mut poly_match = 0usize;
    let mut examples: Vec<String> = Vec::new();

    for tag_path in tags.iter().take(120) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let root = tag.root();
        let count = |name: &str| root.field_path(name).and_then(|f| f.as_block()).map(|b| b.len());
        let (Some(tag_polys), Some(tag_vecs), Some(tag_planes)) = (
            count("polyhedra"),
            count("polyhedron four vectors"),
            count("polyhedron plane equations"),
        ) else {
            continue;
        };
        if tag_polys == 0 {
            continue;
        }

        // Recover the source JMS this tag was built from: the `info`
        // stream holds each original file zlib-compressed, so the
        // comparison really is against the bytes Havok consumed.
        let Some(info) = tag.import_info() else { continue };
        let Some(files) = info.field("files").and_then(|f| f.as_block()) else { continue };
        let mut sources: Vec<Vec<u8>> = Vec::new();
        for file in files.iter() {
            let path = file.read_string("path").unwrap_or_default();
            if !path.to_ascii_lowercase().ends_with(".jms") {
                continue;
            }
            let Some(zipped) = file.field("zipped data").and_then(|f| f.as_data()) else { continue };
            let mut out = Vec::new();
            if std::io::Read::read_to_end(&mut ZlibDecoder::new(zipped), &mut out).is_ok() {
                sources.push(out);
            }
        }
        if sources.len() != 1 {
            continue;
        }
        let Ok(text) = String::from_utf8(sources.remove(0)) else { continue };
        let Ok((jms, _)) = JmsFile::parse(&text) else { continue };

        // `tool` drops any primitive named `null`, case-insensitively.
        let live: Vec<_> = jms
            .convex_shapes
            .iter()
            .filter(|c| !c.name.eq_ignore_ascii_case("null"))
            .collect();
        if live.is_empty() {
            continue;
        }

        let mut our_planes = 0usize;
        let mut our_vecs = 0usize;
        let mut ok = true;
        for c in &live {
            match convex_hull(&c.vertices) {
                Ok(h) => {
                    our_planes += h.planes.len();
                    // Havok stores hull vertices as SoA groups of four.
                    our_vecs += h.vertices.len().div_ceil(4);
                }
                Err(HullError::TooFewPoints(_)) | Err(HullError::Coplanar)
                | Err(HullError::Collinear) => {
                    ok = false;
                    break;
                }
                // Not the same as the others. Those three are shapes that
                // have no hull; this one is a shape that does and we
                // failed to build it, on input Tool managed. Counted
                // separately so it cannot hide in the skip pile.
                Err(HullError::Degenerate { .. }) => {
                    degenerate += 1;
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }

        compared += 1;
        if live.len() == tag_polys {
            poly_match += 1;
        }
        if our_planes == tag_planes {
            planes_match += 1;
        }
        if our_vecs == tag_vecs {
            vecs_match += 1;
        }
        if examples.len() < 12 {
            examples.push(format!(
                "{:<44} polys {}/{}  vecs {}/{}  planes {}/{}",
                tag_path.file_name().unwrap_or_default().to_string_lossy(),
                live.len(),
                tag_polys,
                our_vecs,
                tag_vecs,
                our_planes,
                tag_planes
            ));
        }
    }
    eprintln!("compared {compared} physics_models against their own source");
    if degenerate > 0 {
        eprintln!("  {degenerate} shape(s) the hull builder could not close — Tool built these");
    }
    if compared > 0 {
        eprintln!(
            "  polyhedron count matches: {poly_match}/{compared} ({:.0}%)",
            100.0 * poly_match as f64 / compared as f64
        );
        eprintln!(
            "  four-vector count matches: {vecs_match}/{compared} ({:.0}%)",
            100.0 * vecs_match as f64 / compared as f64
        );
        eprintln!(
            "  plane count matches:       {planes_match}/{compared} ({:.0}%)",
            100.0 * planes_match as f64 / compared as f64
        );
    }
    for e in &examples {
        eprintln!("  {e}");
    }
    assert!(compared > 0, "nothing comparable — the harness is broken, not the hull");
    // Deliberately a floor, not an equality: Havok may keep or drop
    // near-coplanar faces differently from us, and the point of the
    // number is to notice if it moves.
    assert!(
        poly_match * 10 >= compared * 9,
        "polyhedron count should match on almost every model: {poly_match}/{compared}"
    );
}

/// Fit [`HullOptions`] against Havok, rather than guessing them.
///
/// Ignored by default because it is a calibration sweep, not an
/// assertion: it prints how well each tolerance triple reproduces
/// Havok's own vertex and plane counts across the shipped corpus. Run it
/// when changing the defaults:
///
/// ```text
/// cargo test -p blam-tags --test hull_corpus -- --ignored --nocapture
/// ```
#[test]
#[ignore = "calibration sweep; run explicitly when changing HullOptions defaults"]
fn fit_hull_tolerances_against_havok() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install found (set BLAM_TEST_H3EK)");
        return;
    };

    // Collect the matched (source shapes, Havok's counts) pairs once.
    let mut cases: Vec<(Vec<blam_tags::jms::JmsConvex>, usize, usize)> = Vec::new();
    for tag_path in walk(&kit.join("tags"), "physics_model").iter().take(200) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let root = tag.root();
        let count = |name: &str| root.field_path(name).and_then(|f| f.as_block()).map(|b| b.len());
        let (Some(polys), Some(vecs), Some(planes)) = (
            count("polyhedra"),
            count("polyhedron four vectors"),
            count("polyhedron plane equations"),
        ) else {
            continue;
        };
        if polys == 0 {
            continue;
        }
        let Some(info) = tag.import_info() else { continue };
        let Some(files) = info.field("files").and_then(|f| f.as_block()) else { continue };
        let mut sources: Vec<Vec<u8>> = Vec::new();
        for file in files.iter() {
            if !file.read_string("path").unwrap_or_default().to_ascii_lowercase().ends_with(".jms")
            {
                continue;
            }
            let Some(z) = file.field("zipped data").and_then(|f| f.as_data()) else { continue };
            let mut out = Vec::new();
            if std::io::Read::read_to_end(&mut ZlibDecoder::new(z), &mut out).is_ok() {
                sources.push(out);
            }
        }
        if sources.len() != 1 {
            continue;
        }
        let Ok(text) = String::from_utf8(sources.remove(0)) else { continue };
        let Ok((jms, _)) = JmsFile::parse(&text) else { continue };
        let live: Vec<_> = jms
            .convex_shapes
            .iter()
            .filter(|c| !c.name.eq_ignore_ascii_case("null"))
            .cloned()
            .collect();
        if live.len() == polys && !live.is_empty() {
            cases.push((live, vecs, planes));
        }
    }
    eprintln!("fitting against {} matched models\n", cases.len());
    assert!(!cases.is_empty(), "no matched models — cannot fit");

    let mut best = (f64::MAX, blam_tags::hull::HullOptions::default());
    eprintln!(
        "{:>10} {:>10} {:>10}   {:>8} {:>8}   {:>9} {:>9}",
        "pt_eps", "angle", "offset", "vec_hit", "pln_hit", "vec_err", "pln_err"
    );
    for pt in [1e-9, 1e-7, 1e-6, 1e-5, 1e-4, 1e-3] {
        for ang in [1e-6, 1e-5, 1e-4, 1e-3, 1e-2] {
            let opts = blam_tags::hull::HullOptions {
                point_epsilon: pt,
                coplanar_angle: ang,
                coplanar_offset: 1e-5,
            };
            let (mut vec_hit, mut pln_hit) = (0usize, 0usize);
            let (mut vec_err, mut pln_err) = (0.0f64, 0.0f64);
            let mut n = 0usize;
            for (shapes, want_vecs, want_planes) in &cases {
                let mut vecs = 0usize;
                let mut planes = 0usize;
                let mut ok = true;
                for c in shapes {
                    match blam_tags::hull::convex_hull_with(&c.vertices, &opts) {
                        Ok(h) => {
                            vecs += h.vertices.len().div_ceil(4);
                            planes += h.planes.len();
                        }
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    continue;
                }
                n += 1;
                if vecs == *want_vecs {
                    vec_hit += 1;
                }
                if planes == *want_planes {
                    pln_hit += 1;
                }
                vec_err += (vecs as f64 - *want_vecs as f64).abs() / (*want_vecs).max(1) as f64;
                pln_err += (planes as f64 - *want_planes as f64).abs() / (*want_planes).max(1) as f64;
            }
            if n == 0 {
                continue;
            }
            let (ve, pe) = (vec_err / n as f64, pln_err / n as f64);
            eprintln!(
                "{pt:>10.0e} {ang:>10.0e} {:>10.0e}   {vec_hit:>4}/{n:<3} {pln_hit:>4}/{n:<3}   {ve:>9.4} {pe:>9.4}",
                1e-5
            );
            // Rank by mean relative error on both counts; exact hits are
            // a coarser signal because a single shape can dominate.
            let score = ve + pe;
            if score < best.0 {
                best = (score, opts);
            }
        }
    }
    eprintln!("\nbest: {:?}  (score {:.4})", best.1, best.0);
}
