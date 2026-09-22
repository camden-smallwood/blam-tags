//! What PRT do shipped `render_model` tags actually carry?
//!
//! `render_import` writes every mesh as `No PRT`, and the plan calls the
//! missing per-vertex transfer the largest remaining gap. Before solving
//! anything it is worth knowing what tool stores and where — the enum has
//! four values, the raw vertex record has no field for any of them, and
//! `per mesh prt data` is a separate block whose contents are opaque
//! bytes.
//!
//! Diagnostic. Run with `--ignored --nocapture`.

use std::path::{Path, PathBuf};

use blam_tags::TagFile;

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


fn source_jms(tag: &TagFile) -> Option<blam_tags::jms::JmsFile> {
    let info = tag.import_info()?;
    let files = info.field("files").and_then(|f| f.as_block())?;
    let mut found: Vec<Vec<u8>> = Vec::new();
    for file in files.iter() {
        if !file.read_string("path").unwrap_or_default().to_ascii_lowercase().ends_with(".jms") {
            continue;
        }
        let Some(z) = file.field("zipped data").and_then(|f| f.as_data()) else { continue };
        let mut out = Vec::new();
        if std::io::Read::read_to_end(&mut flate2::read::ZlibDecoder::new(z), &mut out).is_ok() {
            found.push(out);
        }
    }
    if found.len() != 1 {
        return None;
    }
    let text = String::from_utf8(found.remove(0)).ok()?;
    blam_tags::jms::JmsFile::parse(&text).ok().map(|(j, _)| j)
}

/// Only compare against a source that actually built the tag — the six
/// floats of `compression info` are three consecutive `(min, max)` pairs,
/// and a source that built the tag reproduces them to rounding.
fn source_matches_tag(tag: &TagFile, jms: &blam_tags::jms::JmsFile) -> bool {
    let root = tag.root();
    let Some(ci) = root.field_path("render geometry/compression info").and_then(|f| f.as_block())
    else {
        return false;
    };
    let Some(first) = ci.iter().next() else { return false };
    let p0 = first.read_point3d("position bounds 0");
    let p1 = first.read_point3d("position bounds 1");
    let pairs = [(p0.x, p0.y), (p0.z, p1.x), (p1.y, p1.z)];
    let s = blam_tags::render_import::JMS_TO_WORLD;
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &jms.vertices {
        for (a, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
            lo[a] = lo[a].min(c * s);
            hi[a] = hi[a].max(c * s);
        }
    }
    if !lo[0].is_finite() {
        return false;
    }
    let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a])).max(1e-6);
    (0..3).all(|a| {
        let (p, q) = pairs[a];
        let (tlo, thi) = (p.min(q), p.max(q));
        ((tlo - lo[a]).abs().max((thi - hi[a]).abs()) / extent) < 1e-5
    })
}

#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn what_prt_do_shipped_render_models_carry() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    const NAMES: [&str; 4] = ["No PRT", "PRT Ambient", "PRT Linear", "PRT Quadratic"];
    let mut by_type = [0usize; 8];
    let (mut meshes, mut models) = (0usize, 0usize);
    // Models that claim PRT, and whether their prt data block holds
    // anything.
    let (mut with_prt_data, mut prt_data_bytes) = (0usize, 0usize);
    let mut examples: Vec<String> = Vec::new();
    /// `(prt type, bytes per vertex, bytes, vertices)`
    let mut ratios: Vec<(usize, f64, usize, usize)> = Vec::new();

    for path in walk(&kit.join("tags"), "render_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(ms) = root.field_path("render geometry/meshes").and_then(|f| f.as_block()) else {
            continue;
        };
        models += 1;
        let mut model_types: Vec<i64> = Vec::new();
        for m in ms.iter() {
            let t = m.read_int_any("PRT vertex type").unwrap_or(0) as i64;
            by_type[(t.clamp(0, 7)) as usize] += 1;
            meshes += 1;
            model_types.push(t);
        }

        // The block is `per_mesh_prt_data`, underscores and all — asking
        // for "per mesh prt data" finds nothing and reads as "tool stores
        // no PRT", which is how this was misread the first time.
        let mut bytes = 0usize;
        let pd = root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block());
        if let Some(pd) = &pd {
            for e in pd.iter() {
                if let Some(data) = e.field("mesh pca data").and_then(|f| f.as_data()) {
                    bytes += data.len();
                }
            }
        }

        // Bytes against vertices, per mesh, which is what says how the
        // coefficients are laid out.
        if let Some(pd) = &pd {
            for (i, m) in ms.iter().enumerate() {
                let t = m.read_int_any("PRT vertex type").unwrap_or(0) as usize;
                if t == 0 || i >= pd.len() {
                    continue;
                }
                let Some(parts) = m.field("parts").and_then(|f| f.as_block()) else { continue };
                let verts: usize = parts
                    .iter()
                    .filter_map(|p| p.read_int_any("budget vertex count"))
                    .map(|v| (v as i64 & 0xFFFF) as usize)
                    .sum();
                let Some(el) = pd.iter().nth(i) else { continue };
                let b = el.field("mesh pca data").and_then(|f| f.as_data()).map(|d| d.len());
                if let (Some(b), true) = (b, verts > 0) {
                    ratios.push((t, b as f64 / verts as f64, b, verts));
                }
            }
        }
        if bytes > 0 {
            with_prt_data += 1;
            prt_data_bytes += bytes;
        }
        if model_types.iter().any(|t| *t != 0) && examples.len() < 10 {
            examples.push(format!(
                "{}: types {:?}, {bytes} bytes of pca data",
                path.file_name().unwrap().to_string_lossy(),
                model_types
            ));
        }
    }

    println!("{models} render_models, {meshes} meshes");
    for (i, n) in NAMES.iter().enumerate() {
        if by_type[i] > 0 {
            println!(
                "  {n:<14} {:6} meshes ({:.1}%)",
                by_type[i],
                100.0 * by_type[i] as f64 / meshes.max(1) as f64
            );
        }
    }
    for (i, c) in by_type.iter().enumerate().skip(4) {
        if *c > 0 {
            println!("  type {i:<10} {c:6} meshes (outside the enum)");
        }
    }
    println!("{with_prt_data} models carry per-mesh pca data, {prt_data_bytes} bytes total");
    for e in &examples {
        println!("  {e}");
    }

    // What do the numbers look like? For Ambient the record is three
    // floats — if they are equal it is a scalar visibility written per
    // channel, which is computable from geometry alone; if they differ
    // there is coloured bounce in there and it is not.
    {
        let mut equal = 0usize;
        let mut differ = 0usize;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        let mut shown = 0usize;
        for path in walk(&kit.join("tags"), "render_model").iter().take(400) {
            let Ok(tag) = TagFile::read(path) else { continue };
            let root = tag.root();
            let (Some(ms), Some(pd)) = (
                root.field_path("render geometry/meshes").and_then(|f| f.as_block()),
                root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block()),
            ) else {
                continue;
            };
            for (i, m) in ms.iter().enumerate() {
                if m.read_int_any("PRT vertex type").unwrap_or(0) != 1 {
                    continue;
                }
                let Some(el) = pd.iter().nth(i) else { continue };
                let Some(data) = el.field("mesh pca data").and_then(|f| f.as_data()) else {
                    continue;
                };
                let n = data.len() / 12;
                for k in 0..n.min(4000) {
                    let f = |j: usize| -> f32 {
                        let o = k * 12 + j * 4;
                        f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
                    };
                    let (r, g, b) = (f(0), f(1), f(2));
                    for v in [r, g, b] {
                        if v.is_finite() {
                            lo = lo.min(v);
                            hi = hi.max(v);
                        }
                    }
                    if (r - g).abs() < 1e-6 && (g - b).abs() < 1e-6 {
                        equal += 1;
                    } else {
                        differ += 1;
                    }
                    if shown < 6 && k < 3 {
                        println!("  sample vertex: {r:.6} {g:.6} {b:.6}");
                        shown += 1;
                    }
                }
            }
        }
        let total = equal + differ;
        println!(
            "PRT Ambient triples: {equal} of {total} have all three channels equal ({:.1}%)",
            100.0 * equal as f64 / total.max(1) as f64
        );
        println!("  value range {lo:.6} .. {hi:.6}");
    }

    println!("bytes of pca data per vertex, by PRT type:");
    for t in 1..4usize {
        let mut v: Vec<f64> = ratios.iter().filter(|r| r.0 == t).map(|r| r.1).collect();
        if v.is_empty() {
            continue;
        }
        v.sort_by(f64::total_cmp);
        let sample = ratios.iter().find(|r| r.0 == t).unwrap();
        println!(
            "  {:<14} n={:5}  min {:.3}  median {:.3}  max {:.3}   (e.g. {} bytes / {} vertices)",
            NAMES[t],
            v.len(),
            v[0],
            v[v.len() / 2],
            v[v.len() - 1],
            sample.2,
            sample.3
        );
    }
}

/// Does the solver agree with tool on the same geometry?
///
/// Per-vertex comparison is not available: a shipped tag keeps no vertex
/// positions — `raw vertices` is empty and the geometry lives in a
/// resource — so there is nothing to line the two vertex sets up by, and
/// this welder does not produce tool's vertex count anyway.
///
/// What can be compared is the **distribution**. Both sides are the
/// occlusion of the same shape, sampled at different points on it, so if
/// the solve is right the deciles should sit close together. Comparing
/// only the mean would hide a solver that is right on average and wrong
/// everywhere, so this looks at the whole curve.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "compares against the shipped corpus; run with --ignored"]
fn ambient_transfer_against_tools_own_values() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };
    let schema = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/render_model.json");
    if !schema.exists() {
        eprintln!("skipping: no render_model schema");
        return;
    }

    let deciles = |mut v: Vec<f32>| -> Vec<f32> {
        v.sort_by(f32::total_cmp);
        (0..=10).map(|i| v[(v.len() - 1) * i / 10]).collect()
    };

    let mut compared = 0usize;
    let mut worst: Vec<(f32, String)> = Vec::new();
    for path in walk(&kit.join("tags"), "render_model").iter().take(600) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();
        let (Some(ms), Some(pd)) = (
            root.field_path("render geometry/meshes").and_then(|f| f.as_block()),
            root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block()),
        ) else {
            continue;
        };
        // Only single-mesh models whose one mesh is ambient, so tool's
        // values and ours describe the same set of geometry.
        if ms.len() != 1 || pd.is_empty() {
            continue;
        }
        let Some(mesh) = ms.iter().next() else { continue };
        if mesh.read_int_any("PRT vertex type").unwrap_or(0) != 1 {
            continue;
        }
        let Some(jms) = source_jms(&tag) else { continue };
        if !source_matches_tag(&tag, &jms) {
            continue;
        }
        let Some(el) = pd.iter().next() else { continue };
        let Some(data) = el.field("mesh pca data").and_then(|f| f.as_data()) else { continue };
        if data.len() < 12 {
            continue;
        }

        // Tool's values, as visibility.
        let theirs: Vec<f32> = (0..data.len() / 12)
            .map(|k| {
                let o = k * 12;
                f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
                    / blam_tags::prt::Y00
            })
            .collect();

        // Ours, on the same source.
        let s = blam_tags::render_import::JMS_TO_WORLD;
        let positions: Vec<blam_tags::math::RealPoint3d> = jms
            .vertices
            .iter()
            .map(|v| blam_tags::math::RealPoint3d {
                x: v.position.x * s,
                y: v.position.y * s,
                z: v.position.z * s,
            })
            .collect();
        let normals: Vec<blam_tags::math::RealVector3d> =
            jms.vertices.iter().map(|v| v.normal).collect();
        let tris: Vec<[u32; 3]> = jms.triangles.iter().map(|t| t.v).collect();
        if tris.is_empty() || positions.len() > 40_000 {
            continue;
        }
        let ours = blam_tags::prt::ambient_transfer(
            &positions,
            &normals,
            &tris,
            &blam_tags::prt::PrtOptions::default(),
        );

        let a = deciles(theirs);
        let b = deciles(ours);
        let gap = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        compared += 1;
        if compared <= 6 || gap > 0.5 {
            println!("{}", path.file_name().unwrap().to_string_lossy());
            println!("  tool  {}", a.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>().join(" "));
            println!("  ours  {}", b.iter().map(|x| format!("{x:.3}")).collect::<Vec<_>>().join(" "));
            println!("  tool vertices {}, source vertices {}, triangles {}",
                     data.len() / 12, positions.len(), tris.len());
        }
        worst.push((gap, path.file_name().unwrap().to_string_lossy().into_owned()));
    }

    worst.sort_by(|x, y| x.0.total_cmp(&y.0));
    println!("compared {compared} single-mesh ambient models");
    if !worst.is_empty() {
        let med = worst[worst.len() / 2].0;
        println!("  worst decile gap: median {med:.3}, best {:.3}, worst {:.3}",
                 worst[0].0, worst[worst.len() - 1].0);
        for (g, name) in worst.iter().rev().take(3) {
            println!("    {g:.3}  {name}");
        }
    }
}

/// Which frame are the higher-order coefficients in?
///
/// For ambient it does not matter — one coefficient has no direction.
/// For linear it decides everything: in a **tangent** frame (normal at
/// +Z) an unoccluded vertex gives the same l=1 triple everywhere, and
/// in an **object** frame the triple points along that vertex's own
/// normal and varies across the mesh.
///
/// The prediction for the tangent frame is arithmetic, not a guess.
/// Under cosine-weighted sampling the estimator is the plain mean of
/// `Y_lm` over unoccluded directions, `Y10 = sqrt(3/4pi) cos(theta)`,
/// and the mean of `cos(theta)` under that weighting is 2/3 — so an open
/// vertex should read `0.4886 * 2/3 = 0.3257` on one l=1 axis and about
/// zero on the other two, with `Y00 = 0.2821` alongside it.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn what_frame_are_the_linear_coefficients_in() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    // Per coefficient slot, the values seen across many vertices.
    let mut slots: Vec<Vec<f32>> = vec![Vec::new(); 4];
    let mut shown = 0usize;
    for path in walk(&kit.join("tags"), "render_model").iter().take(500) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();
        let (Some(ms), Some(pd)) = (
            root.field_path("render geometry/meshes").and_then(|f| f.as_block()),
            root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block()),
        ) else {
            continue;
        };
        for (i, m) in ms.iter().enumerate() {
            if m.read_int_any("PRT vertex type").unwrap_or(0) != 2 {
                continue;
            }
            let Some(el) = pd.iter().nth(i) else { continue };
            let Some(data) = el.field("mesh pca data").and_then(|f| f.as_data()) else { continue };
            let stride = 48usize;
            for k in 0..(data.len() / stride).min(3000) {
                let f = |j: usize| -> f32 {
                    let o = k * stride + j * 4;
                    f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
                };
                // First channel only: the ambient case showed all three
                // carry the same numbers.
                for c in 0..4 {
                    slots[c].push(f(c));
                }
                if shown < 5 {
                    println!(
                        "  sample: {:.5} {:.5} {:.5} {:.5} | next four {:.5} {:.5} {:.5} {:.5}",
                        f(0), f(1), f(2), f(3), f(4), f(5), f(6), f(7)
                    );
                    shown += 1;
                }
            }
        }
    }

    println!("linear coefficient slots, over {} vertices:", slots[0].len());
    for (c, v) in slots.iter().enumerate() {
        if v.is_empty() {
            continue;
        }
        let mut v = v.clone();
        v.sort_by(f32::total_cmp);
        let mean: f32 = v.iter().sum::<f32>() / v.len() as f32;
        println!(
            "  slot {c}: min {:.5}  p10 {:.5}  median {:.5}  p90 {:.5}  max {:.5}  mean {:.5}",
            v[0], v[v.len() / 10], v[v.len() / 2], v[v.len() * 9 / 10], v[v.len() - 1], mean
        );
    }
    println!("  a tangent frame predicts a slot near 0.3257 and two near 0.0");
}

/// Does the linear solve agree with tool, slot by slot?
///
/// Same limitation as the ambient comparison — no vertex positions in a
/// shipped tag, so no per-vertex alignment — but the *shape* of each
/// coefficient's distribution is comparable, and for l=1 that shape is
/// the whole claim. If the frame were wrong the l=1 slots would come out
/// pinned near 0.3257 instead of spread about zero, and the deciles
/// would separate immediately.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "compares against the shipped corpus; run with --ignored"]
fn linear_transfer_against_tools_own_values() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    let deciles = |mut v: Vec<f32>| -> Vec<f32> {
        v.sort_by(f32::total_cmp);
        (0..=10).map(|i| v[(v.len() - 1) * i / 10]).collect()
    };

    let mut compared = 0usize;
    let mut gaps: Vec<f32> = Vec::new();
    for path in walk(&kit.join("tags"), "render_model").iter().take(600) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();
        let (Some(ms), Some(pd)) = (
            root.field_path("render geometry/meshes").and_then(|f| f.as_block()),
            root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block()),
        ) else {
            continue;
        };
        if ms.len() != 1 || pd.is_empty() {
            continue;
        }
        let Some(mesh) = ms.iter().next() else { continue };
        if mesh.read_int_any("PRT vertex type").unwrap_or(0) != 2 {
            continue;
        }
        let Some(jms) = source_jms(&tag) else { continue };
        if !source_matches_tag(&tag, &jms) {
            continue;
        }
        let Some(el) = pd.iter().next() else { continue };
        let Some(data) = el.field("mesh pca data").and_then(|f| f.as_data()) else { continue };
        if data.len() < 48 {
            continue;
        }

        let s = blam_tags::render_import::JMS_TO_WORLD;
        let positions: Vec<blam_tags::math::RealPoint3d> = jms
            .vertices
            .iter()
            .map(|v| blam_tags::math::RealPoint3d {
                x: v.position.x * s,
                y: v.position.y * s,
                z: v.position.z * s,
            })
            .collect();
        let normals: Vec<blam_tags::math::RealVector3d> =
            jms.vertices.iter().map(|v| v.normal).collect();
        let tris: Vec<[u32; 3]> = jms.triangles.iter().map(|t| t.v).collect();
        if tris.is_empty() || positions.len() > 30_000 {
            continue;
        }
        let ours = blam_tags::prt::sh_transfer(
            &positions,
            &normals,
            &tris,
            1,
            &blam_tags::prt::PrtOptions::default(),
        );

        compared += 1;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if compared <= 4 {
            println!("{name}");
        }
        for slot in 0..4usize {
            let theirs: Vec<f32> = (0..data.len() / 48)
                .map(|k| {
                    let o = k * 48 + slot * 4;
                    f32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]])
                })
                .collect();
            let mine: Vec<f32> = ours.iter().map(|c| c[slot]).collect();
            let a = deciles(theirs);
            let b = deciles(mine);
            let gap = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
            gaps.push(gap);
            if compared <= 4 {
                println!(
                    "  slot {slot}  tool {}",
                    a.iter().map(|x| format!("{x:+.3}")).collect::<Vec<_>>().join(" ")
                );
                println!(
                    "          ours {}",
                    b.iter().map(|x| format!("{x:+.3}")).collect::<Vec<_>>().join(" ")
                );
            }
        }
    }

    gaps.sort_by(f32::total_cmp);
    println!("compared {compared} single-mesh linear models");
    if !gaps.is_empty() {
        println!(
            "  per-slot decile gap: median {:.3}, p90 {:.3}, worst {:.3}",
            gaps[gaps.len() / 2],
            gaps[gaps.len() * 9 / 10],
            gaps[gaps.len() - 1]
        );
    }
}
