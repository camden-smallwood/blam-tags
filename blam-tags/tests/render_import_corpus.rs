//! Build `render_model` tags from shipped sources and compare against
//! what `tool.exe` produced from the same bytes.
//!
//! Every shipped `.render_model` carries its source JMS in the `info`
//! stream, so this is a matched comparison. The strongest check is the
//! **compression bounds**: they are the exact per-axis min/max of the
//! source vertices scaled by 0.01 with V flipped, so agreement there
//! means the scale, the flip and the welding all landed correctly.

use std::path::{Path, PathBuf};

use blam_tags::jms::JmsFile;
use blam_tags::render_import::{render_model_from_jms, RenderError, RenderOptions};
use blam_tags::weld::{weld, WeldTolerances, WeldVertex};
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
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/render_model.json");
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

/// Total `raw vertices` across a tag's meshes — the number our welder
/// has to reproduce, because it is what the vertex limit counts.
fn tag_vertex_count(tag: &TagFile) -> usize {
    let Some(b) = tag
        .root()
        .field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
    else {
        return 0;
    };
    b.iter()
        .filter_map(|el| el.field("raw vertices").and_then(|f| f.as_block()).map(|v| v.len()))
        .sum()
}

fn block_len(tag: &TagFile, name: &str) -> usize {
    tag.root().field_path(name).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0)
}

/// The six position-bound floats, read as the three `(min,max)` pairs
/// they actually are.
fn position_bounds(tag: &TagFile) -> Option<[f32; 6]> {
    let ci = tag.root().field_path("render geometry/compression info").and_then(|f| f.as_block())?;
    let el = ci.element(0)?;
    let a = el.read_point3d("position bounds 0");
    let b = el.read_point3d("position bounds 1");
    Some([a.x, a.y, a.z, b.x, b.y, b.z])
}

#[test]
fn we_rebuild_shipped_render_models_from_their_own_source() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and definitions/halo3_mcc/render_model.json");
        return;
    };

    let opts = RenderOptions::default();
    let mut compared = 0usize;
    let (mut meshes_ok, mut regions_ok, mut nodes_ok, mut mats_ok, mut bounds_ok) = (0, 0, 0, 0, 0);
    let mut refused: Vec<String> = Vec::new();
    let mut too_large = 0usize;
    let mut worst_bound = 0.0f32;
    let (mut weld_ok, mut weld_compared) = (0usize, 0usize);
    let mut weld_ratio_sum = 0.0f64;
    let mut weld_ratio_worst = 0.0f64;
    let mut weld_ratio_best = f64::MAX;
    let mut rows: Vec<String> = Vec::new();

    for tag_path in walk(&kit.join("tags"), "render_model").iter().take(120) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };
        let want_meshes = block_len(&tag, "render geometry/meshes");
        if want_meshes == 0 {
            continue;
        }
        let want_regions = block_len(&tag, "regions");
        let want_nodes = block_len(&tag, "nodes");
        let want_mats = block_len(&tag, "materials");

        match render_model_from_jms(&jms, &schema, &opts) {
            Ok((built, report)) => {
                compared += 1;
                if report.meshes == want_meshes {
                    meshes_ok += 1;
                }
                if report.regions == want_regions {
                    regions_ok += 1;
                }
                if report.nodes == want_nodes {
                    nodes_ok += 1;
                }
                if report.materials == want_mats {
                    mats_ok += 1;
                }
                let want_verts = tag_vertex_count(&tag);
                if want_verts > 0 {
                    weld_compared += 1;
                    let r = report.welded_vertices as f64 / want_verts as f64;
                    weld_ratio_sum += r;
                    weld_ratio_worst = weld_ratio_worst.max(r);
                    weld_ratio_best = weld_ratio_best.min(r);
                    if (r - 1.0).abs() < 0.02 {
                        weld_ok += 1;
                    }
                }
                // The bounds are the exact min/max of the source, so this
                // is the check that the scale, the V flip and the weld
                // all landed.
                if let (Some(a), Some(b)) = (position_bounds(&tag), position_bounds(&built)) {
                    let mut worst = 0.0f32;
                    for k in 0..6 {
                        worst = worst.max((a[k] - b[k]).abs());
                    }
                    worst_bound = worst_bound.max(worst);
                    if worst < 1e-4 {
                        bounds_ok += 1;
                    }
                }
                // It must survive a write and re-read.
                let bytes = built.write_to_bytes().expect("serialize");
                let back = TagFile::read_from_bytes(&bytes)
                    .unwrap_or_else(|e| panic!("{}: rebuilt tag does not parse: {e}",
                        tag_path.display()));
                assert_eq!(
                    block_len(&back, "render geometry/meshes"),
                    report.meshes,
                    "{}: meshes lost in the round trip",
                    tag_path.display()
                );
                if rows.len() < 10 {
                    rows.push(format!(
                        "{:<38} meshes {}/{}  regions {}/{}  nodes {}/{}  verts {}->{}",
                        tag_path.file_name().unwrap_or_default().to_string_lossy(),
                        report.meshes, want_meshes,
                        report.regions, want_regions,
                        report.nodes, want_nodes,
                        report.source_vertices, report.welded_vertices
                    ));
                }
            }
            Err(RenderError::MeshTooLarge { .. }) | Err(RenderError::TooManyIndices { .. }) => {
                too_large += 1;
            }
            Err(e) => refused.push(format!(
                "{}: {e}",
                tag_path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }

    eprintln!("rebuilt {compared} render_models from their own source");
    eprintln!("  refused as too large for one mesh: {too_large}");
    eprintln!("  refused for other reasons: {}", refused.len());
    for r in refused.iter().take(8) {
        eprintln!("    {r}");
    }
    if compared > 0 {
        let pc = |x: usize| 100.0 * x as f64 / compared as f64;
        eprintln!("  mesh count matches:     {meshes_ok}/{compared} ({:.0}%)", pc(meshes_ok));
        eprintln!("  region count matches:   {regions_ok}/{compared} ({:.0}%)", pc(regions_ok));
        eprintln!("  node count matches:     {nodes_ok}/{compared} ({:.0}%)", pc(nodes_ok));
        eprintln!("  material count matches: {mats_ok}/{compared} ({:.0}%)", pc(mats_ok));
        eprintln!("  position bounds match:  {bounds_ok}/{compared} ({:.0}%)", pc(bounds_ok));
        eprintln!("  worst bound error: {worst_bound:.8}");
    }
    if weld_compared > 0 {
        eprintln!(
            "  welded vertices vs tool: {weld_ok}/{weld_compared} within 2%               (mean {:.3}x, best {:.3}x, worst {:.3}x)",
            weld_ratio_sum / weld_compared as f64,
            weld_ratio_best,
            weld_ratio_worst
        );
    }
    for r in &rows {
        eprintln!("  {r}");
    }

    assert!(compared > 0, "nothing was rebuilt — the harness is broken, not the writer");
    assert!(
        refused.is_empty(),
        "{} models failed for unexpected reasons:\n{}",
        refused.len(),
        refused.iter().take(5).cloned().collect::<Vec<_>>().join("\n")
    );
    assert!(
        meshes_ok * 10 >= compared * 9,
        "mesh counts should match on almost every model: {meshes_ok}/{compared}"
    );
    assert!(
        bounds_ok * 10 >= compared * 9,
        "compression bounds should match on almost every model: {bounds_ok}/{compared}"
    );
}

/// How many indices does a source triangle actually cost?
///
/// `SplitBudget` converts its ceilings into a source-triangle limit, so
/// an index budget needs the same kind of ratio `vertices_per_triangle`
/// already has. A triangle costs one index inside a long strip and three
/// in a strip of its own, so the figure is a property of the stripifier,
/// not of the model, and it has to be measured rather than reasoned
/// about.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "fits a constant against the kit; run with --ignored"]
fn fit_indices_per_triangle() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };
    let Some(schema) = schema() else {
        eprintln!("skipping: no render_model schema");
        return;
    };

    // Every loose render JMS in the kit, plus the JMS baked into shipped
    // render_model tags, so the sample is not just hand-authored content.
    let mut sources: Vec<(String, JmsFile)> = Vec::new();
    for path in walk(&kit.join("data"), "JMS") {
        if !path.parent().is_some_and(|d| d.ends_with("render")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Ok((jms, _)) = JmsFile::parse(&text) {
            sources.push((path.file_name().unwrap().to_string_lossy().into_owned(), jms));
        }
    }

    let opts = RenderOptions::default();
    // (triangles, indices, indices/triangle, indices/vertex, file)
    let mut rows: Vec<(usize, usize, f64, f64, String)> = Vec::new();
    let mut refused = 0usize;
    for (name, jms) in &sources {
        match render_model_from_jms(jms, &schema, &opts) {
            Ok((_, report)) => {
                for &(tris, verts, idx) in &report.per_mesh {
                    if tris > 0 && verts > 0 {
                        rows.push((
                            tris,
                            idx,
                            idx as f64 / tris as f64,
                            idx as f64 / verts as f64,
                            name.clone(),
                        ));
                    }
                }
            }
            Err(_) => refused += 1,
        }
    }

    println!("{} meshes from {} files ({refused} refused)", rows.len(), sources.len());
    if rows.is_empty() {
        return;
    }

    // Only meshes big enough to ever need splitting decide the constant;
    // a four-triangle mesh strips badly and does not matter.
    for floor in [0usize, 100, 1_000, 5_000, 10_000] {
        let big: Vec<&(usize, usize, f64, f64, String)> =
            rows.iter().filter(|r| r.0 >= floor).collect();
        if big.is_empty() {
            continue;
        }
        let worst = big.iter().max_by(|a, b| a.2.total_cmp(&b.2)).unwrap();
        let mean: f64 = big.iter().map(|r| r.2).sum::<f64>() / big.len() as f64;
        // Indices per *vertex* as well: that is the denominator
        // `00_corpus_ground_truth.md` uses for the shipped corpus, where
        // the median is 1.769 and the p10 1.516. Per-triangle and
        // per-vertex ratios are not comparable to each other.
        let mut per_vertex: Vec<f64> = big.iter().map(|r| r.3).collect();
        per_vertex.sort_unstable_by(f64::total_cmp);
        let median_v = per_vertex[per_vertex.len() / 2];
        let p90_v = per_vertex[per_vertex.len() * 9 / 10];
        println!(
            "  >= {floor:>6} tris: {:>5} meshes  idx/tri mean {:.3} max {:.3}  |  \
             idx/vert median {:.3} p90 {:.3}  (worst {} tris -> {} idx, {})",
            big.len(),
            mean,
            worst.2,
            median_v,
            p90_v,
            worst.0,
            worst.1,
            worst.4
        );
    }

    // The largest meshes, which are the ones the budget has to hold.
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    println!("largest meshes:");
    for (tris, idx, ratio, per_vertex, name) in rows.iter().take(12) {
        println!(
            "  {tris:>6} tris -> {idx:>6} indices  idx/tri {ratio:.3}  idx/vert {per_vertex:.3}  \
             {name}"
        );
    }
}

/// This stripifier against tool's, on the same geometry.
///
/// The ratios in `00_corpus_ground_truth.md` are tool's, measured on
/// shipped meshes; the ratio measured here is this importer's, measured
/// on the kit's loose JMS. Those are different meshes, and a mesh's
/// topology moves the ratio further than any stripifier does — tool
/// itself spans 1.52 to 2.21 across its own content. Comparing the two
/// numbers therefore says very little.
///
/// A shipped `render_model` carries the JMS it was built from in its
/// `info` stream, so the honest comparison is available: build that same
/// source here and count indices against the ones tool actually wrote.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "compares against the shipped corpus; run with --ignored"]
fn indices_against_tools_own_output() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and the render_model schema");
        return;
    };

    /// Every index tool wrote into this tag.
    ///
    /// Shipped geometry lives in a resource, so `raw indices` is not in
    /// the tag — but each part records its own run length, and those
    /// cover the buffer apart from the degenerate indices bridging one
    /// part to the next. Both sides of this comparison carry that same
    /// small overhead, so it does not favour either.
    ///
    /// `index count` is a signed word holding an unsigned value, so a run
    /// over 32,767 arrives negative.
    fn shipped_indices(tag: &TagFile) -> Option<usize> {
        shipped_counts(tag).map(|(i, ..)| i)
    }

    /// Tool's own totals: indices, vertices, meshes, parts.
    fn shipped_counts(tag: &TagFile) -> Option<(usize, usize, usize, usize)> {
        let root = tag.root();
        let meshes = root.field_path("render geometry/meshes")?.as_block()?;
        let (mut total, mut verts, mut nparts) = (0usize, 0usize, 0usize);
        for mesh in meshes.iter() {
            let parts = mesh.field("parts").and_then(|f| f.as_block())?;
            nparts += parts.len();
            for part in parts.iter() {
                let raw = part.read_int_any("index count")?;
                total += (raw as i64 & 0xFFFF) as usize;
                if let Some(v) = part.read_int_any("budget vertex count") {
                    verts += (v as i64 & 0xFFFF) as usize;
                }
            }
        }
        (total > 0).then_some((total, verts, meshes.len(), nparts))
    }

    let opts = RenderOptions::default();
    let (mut compared, mut mine_total, mut theirs_total) = (0usize, 0usize, 0usize);
    let mut stale = 0usize;
    let mut ratios: Vec<(f64, usize, String)> = Vec::new();
    let mut per_vertex: Vec<f64> = Vec::new();
    // How many vertices this welder keeps against tool's, on the same
    // source. A strip cannot cross a split vertex, so over-splitting
    // caps strip length however good the walk is.
    let mut vertex_ratio: Vec<f64> = Vec::new();
    let mut vertex_ratio_single: Vec<f64> = Vec::new();
    // Indices per source triangle, for both. Unlike per-vertex this
    // cannot be moved by welding differently, so it is the one that
    // compares the stripifiers rather than the welders.
    let mut mine_per_tri: Vec<f64> = Vec::new();
    let mut tool_per_tri: Vec<f64> = Vec::new();
    let mut refused = 0usize;

    for path in walk(&kit.join("tags"), "render_model").iter().take(400) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let Some((theirs, their_verts, their_meshes, their_parts)) = shipped_counts(&tag)
        else {
            continue;
        };
        let Some(jms) = source_jms(&tag) else { continue };

        let Ok((_, report)) = render_model_from_jms(&jms, &schema, &opts) else {
            refused += 1;
            continue;
        };
        if report.meshes == 0 {
            continue;
        }
        // The `info` stream holds the source of the last import, which is
        // not always the source of this tag: ported and hand-edited
        // content carries a stale one. A mesh cannot hold more vertices
        // than its triangles have corners, so a tag claiming more than
        // that was built from something else — `butterfly_b` claims 24
        // vertices for a 6-triangle JMS, which is 18 corners.
        //
        // Deriving tool's triangle count from its index count instead
        // does not work: that needs one strip per part, and tool averages
        // 1.826 indices per triangle, so it rejects everything except the
        // models tool happened to strip perfectly.
        if their_verts > 3 * report.triangles {
            stale += 1;
            continue;
        }
        compared += 1;
        mine_total += report.indices;
        theirs_total += theirs;
        for &(_, verts, idx) in &report.per_mesh {
            if verts > 0 {
                per_vertex.push(idx as f64 / verts as f64);
            }
        }
        if their_verts > 0 {
            let r = report.welded_vertices as f64 / their_verts as f64;
            vertex_ratio.push(r);
            // Tool's vertex total is summed over parts, so a vertex used
            // by two parts could in principle be counted twice. Where
            // every mesh has exactly one part there is nothing to double
            // count, so this subset says whether the metric is sound.
            if their_parts == their_meshes {
                vertex_ratio_single.push(r);
            }
        }
        let tris: usize = report.triangles;
        if tris > 0 {
            mine_per_tri.push(report.indices as f64 / tris as f64);
            tool_per_tri.push(theirs as f64 / tris as f64);
        }
        ratios.push((
            report.indices as f64 / theirs as f64,
            theirs,
            format!(
                "{}  [{} tris -> {} verts; meshes {} v {}; materials {} v parts {}]",
                path.file_name().unwrap().to_string_lossy(),
                report.triangles,
                report.welded_vertices,
                report.meshes,
                their_meshes,
                report.materials,
                their_parts
            ),
        ));
    }

    println!(
        "compared {compared} render_models against their own source          ({refused} refused, {stale} skipped as the tag does not match its own info stream)"
    );
    if compared == 0 {
        return;
    }
    println!(
        "  tool wrote {theirs_total} indices, this wrote {mine_total} — {:.1}% of tool's",
        100.0 * mine_total as f64 / theirs_total as f64
    );
    // Indices per vertex, on the same content tool's own 1.769 median
    // was measured on — which is what the reachable vertex count, and so
    // the whole ceiling argument, is expressed in.
    per_vertex.sort_unstable_by(f64::total_cmp);
    if !per_vertex.is_empty() {
        println!(
            "  indices per vertex, this importer on shipped sources: median {:.3}, p10 {:.3}, \
             p90 {:.3}  (tool: 1.769 / 1.516 / 2.213)",
            per_vertex[per_vertex.len() / 2],
            per_vertex[per_vertex.len() / 10],
            per_vertex[per_vertex.len() * 9 / 10],
        );
    }

    // If the one-part subset agrees with the whole, summing per-part
    // counts is not double counting and the target is sound.
    vertex_ratio_single.sort_unstable_by(f64::total_cmp);
    if !vertex_ratio_single.is_empty() {
        println!(
            "  same, over the {} models whose meshes have a single part: median {:.3}",
            vertex_ratio_single.len(),
            vertex_ratio_single[vertex_ratio_single.len() / 2],
        );
    }

    vertex_ratio.sort_unstable_by(f64::total_cmp);
    if !vertex_ratio.is_empty() {
        println!(
            "  welded vertices as a fraction of tool's: median {:.3}, p90 {:.3}, max {:.3}",
            vertex_ratio[vertex_ratio.len() / 2],
            vertex_ratio[vertex_ratio.len() * 9 / 10],
            vertex_ratio[vertex_ratio.len() - 1],
        );
    }

    mine_per_tri.sort_unstable_by(f64::total_cmp);
    tool_per_tri.sort_unstable_by(f64::total_cmp);
    if !mine_per_tri.is_empty() {
        let m = mine_per_tri[mine_per_tri.len() / 2];
        let t = tool_per_tri[tool_per_tri.len() / 2];
        println!("  indices per source triangle: this {m:.3}, tool {t:.3}");
        println!(
            "  so one section holds about {:.0} triangles here against tool's {:.0}              on the 65535-index budget",
            65_535.0 / m,
            65_535.0 / t
        );
    }

    ratios.sort_by(|a, b| a.0.total_cmp(&b.0));
    let median = ratios[ratios.len() / 2].0;
    let p90 = ratios[ratios.len() * 9 / 10].0;
    println!("  per model: median {:.3}x tool, p90 {:.3}x", median, p90);
    println!("  worst five:");
    for (r, theirs, name) in ratios.iter().rev().take(5) {
        println!("    {r:.3}x  (tool {theirs} indices)  {name}");
    }
    println!("  best five:");
    for (r, theirs, name) in ratios.iter().take(5) {
        println!("    {r:.3}x  (tool {theirs} indices)  {name}");
    }
}

/// Does a tag's `info` stream JMS describe the geometry that built it?
///
/// `compression info` holds the position bounds tool quantised the
/// vertices against, so a source that really built the tag reproduces
/// them to rounding. The six floats are three consecutive `(min, max)`
/// pairs — not two corner points — and pairing them by axis makes
/// matching geometry look mismatched.
fn source_matches_tag(tag: &TagFile, jms: &JmsFile) -> bool {
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
    // A true match lands at 1e-8 or below; a stale source is orders out.
    // Scaled by the extent so large models are not judged by absolutes.
    let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a])).max(1e-6);
    (0..3).all(|a| {
        let (p, q) = pairs[a];
        let (tlo, thi) = (p.min(q), p.max(q));
        ((tlo - lo[a]).abs().max((thi - hi[a]).abs()) / extent) < 1e-5
    })
}

/// Which criterion is splitting vertices tool keeps together?
///
/// On `reach_flak_cannon` tool welds 15,656 source triangles down to
/// 10,231 vertices and this welds the same geometry to 15,405 — half
/// again as many, which caps strip length before the stripifier gets a
/// say. The vertex stage merges within a point on two criteria, texture
/// coordinates and normal angle, so relaxing each in turn says which one
/// is responsible rather than leaving it to be guessed.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "diagnostic; needs an H3EK install"]
fn what_splits_vertices_tool_keeps() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    // A few models spanning the range: the worst, a middling one, and
    // one where this importer already matches.
    let wanted = [
        ("reach_flak_cannon.render_model", 10_231usize),
        ("h2a_shotgun.render_model", 0),
        ("guardian.render_model", 0),
    ];

    for path in walk(&kit.join("tags"), "render_model") {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let Some(&(_, tool_verts)) = wanted.iter().find(|(w, _)| *w == name) else { continue };
        let Ok(tag) = TagFile::read(&path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };

        let scale = blam_tags::render_import::JMS_TO_WORLD;
        let source: Vec<WeldVertex> = jms
            .vertices
            .iter()
            .map(|v| WeldVertex {
                position: blam_tags::math::RealPoint3d {
                    x: v.position.x * scale,
                    y: v.position.y * scale,
                    z: v.position.z * scale,
                },
                normal: v.normal,
                texcoords: v
                    .uvs
                    .iter()
                    .map(|t| blam_tags::math::RealPoint2d { x: t.x, y: 1.0 - t.y })
                    .collect(),
                influences: v.node_sets.clone(),
                color: v.color,
            })
            .collect();

        // Tool's own total, where it is known, else whatever the tag says.
        let theirs = if tool_verts > 0 {
            tool_verts
        } else {
            let root = tag.root();
            root.field_path("render geometry/meshes")
                .and_then(|f| f.as_block())
                .map(|ms| {
                    ms.iter()
                        .filter_map(|m| m.field("parts").and_then(|f| f.as_block()))
                        .flat_map(|ps| {
                            ps.iter()
                                .filter_map(|p| p.read_int_any("budget vertex count"))
                                .collect::<Vec<_>>()
                        })
                        .map(|v| (v as i64 & 0xFFFF) as usize)
                        .sum::<usize>()
                })
                .unwrap_or(0)
        };

        let base = WeldTolerances::precise();
        let count = |t: WeldTolerances| weld(&source, &t).vertices.len();

        let both = count(base);
        let no_normal = count(WeldTolerances { normal_degrees: 180.0, ..base });
        let no_texcoord = count(WeldTolerances { texcoord: [1e9; 4], ..base });
        let positions_only =
            count(WeldTolerances { normal_degrees: 180.0, texcoord: [1e9; 4], ..base });

        // How close to unit length are the source normals? The merge
        // test is a raw dot product against cos(1 degree) = 0.999848, so
        // a normal even slightly short makes two identical directions
        // fail to weld.
        let mut short = 0usize;
        let mut worst = 1.0f32;
        for v in &source {
            let l = (v.normal.i * v.normal.i + v.normal.j * v.normal.j + v.normal.k * v.normal.k)
                .sqrt();
            if (l - 1.0).abs() > 7.6e-5 {
                short += 1;
            }
            if (l - 1.0).abs() > (worst - 1.0f32).abs() {
                worst = l;
            }
        }

        println!("{name}");
        println!(
            "  normals off unit by >7.6e-5: {short} of {} (worst length {worst:.6})",
            source.len()
        );
        println!("  source vertices          {}", source.len());
        println!("  tool                     {theirs}");
        println!("  this, as configured      {both}");
        println!("  ignoring normals         {no_normal}");
        println!("  ignoring texcoords       {no_texcoord}");
        println!("  positions only           {positions_only}");
    }
}

/// Fit the vertex-stage tolerances against tool's own vertex counts.
///
/// The tolerances in [`blam_tags::weld`] were read off the binary and
/// have never been checked against what tool actually produces. They can
/// be: a shipped `render_model` carries the JMS it was built from, and
/// its parts carry vertex counts.
///
/// Restricted to models whose geometry is a **single mesh with a single
/// part**, because tool's vertex total is summed per part and a vertex
/// used by two parts could otherwise be counted twice. That subset is
/// small but unambiguous, and the ambiguity is exactly what made the
/// corpus-wide 0.888 figure untrustworthy — the same measurement over
/// this subset gives 0.979.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "fits constants against the kit; run with --ignored"]
fn fit_weld_tolerances_against_tool() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    // (source vertices, tool's vertex count, name)
    let mut cases: Vec<(Vec<WeldVertex>, usize, String)> = Vec::new();
    for path in walk(&kit.join("tags"), "render_model").iter().take(400) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();
        let Some(meshes) = root.field_path("render geometry/meshes").and_then(|f| f.as_block())
        else {
            continue;
        };
        if meshes.len() != 1 {
            continue;
        }
        let Some(parts) = meshes.iter().next().and_then(|m| m.field("parts")?.as_block()) else {
            continue;
        };
        if parts.len() != 1 {
            continue;
        }
        let Some(theirs) = parts
            .iter()
            .next()
            .and_then(|p| p.read_int_any("budget vertex count"))
            .map(|v| (v as i64 & 0xFFFF) as usize)
        else {
            continue;
        };
        if theirs == 0 {
            continue;
        }
        let Some(jms) = source_jms(&tag) else { continue };

        let scale = blam_tags::render_import::JMS_TO_WORLD;
        let source: Vec<WeldVertex> = jms
            .vertices
            .iter()
            .map(|v| WeldVertex {
                position: blam_tags::math::RealPoint3d {
                    x: v.position.x * scale,
                    y: v.position.y * scale,
                    z: v.position.z * scale,
                },
                normal: v.normal,
                texcoords: v
                    .uvs
                    .iter()
                    .map(|t| blam_tags::math::RealPoint2d { x: t.x, y: 1.0 - t.y })
                    .collect(),
                influences: v.node_sets.clone(),
                color: v.color,
            })
            .collect();
        if source.is_empty() {
            continue;
        }
        cases.push((source, theirs, path.file_name().unwrap().to_string_lossy().into_owned()));
    }

    println!("fitting against {} single-mesh single-part models", cases.len());
    if cases.is_empty() {
        return;
    }

    let base = WeldTolerances::precise();
    println!("{:>8} {:>10}   {:>8} {:>8} {:>8}", "normal", "texcoord", "median", "within2%", "mean");
    let mut best: Option<(f64, f32, f32)> = None;
    for &deg in &[0.5f32, 1.0, 2.0, 5.0, 10.0, 20.0, 45.0, 89.0, 180.0] {
        for &tc in &[0.000488281f32, 0.0009765625, 0.001953125] {
            let tol = WeldTolerances {
                normal_degrees: deg,
                texcoord: [tc, tc, 0.0, 0.0],
                ..base
            };
            let mut ratios: Vec<f64> = cases
                .iter()
                .map(|(src, theirs, _)| weld(src, &tol).vertices.len() as f64 / *theirs as f64)
                .collect();
            ratios.sort_unstable_by(f64::total_cmp);
            let median = ratios[ratios.len() / 2];
            let within = ratios.iter().filter(|r| (**r - 1.0).abs() <= 0.02).count();
            let mean: f64 = ratios.iter().sum::<f64>() / ratios.len() as f64;
            println!("{deg:>8} {tc:>10.7}   {median:>8.3} {within:>8} {mean:>8.3}");
            // Closest median to parity wins; ties to the tighter normal.
            let score = (median - 1.0).abs();
            if best.is_none_or(|(b, _, _)| score < b) {
                best = Some((score, deg, tc));
            }
        }
    }
    if let Some((_, deg, tc)) = best {
        println!("best: normal {deg} degrees, texcoord {tc:.7}");
    }
}

/// Is tool splitting on an attribute this welder never looks at?
///
/// No normal or texcoord tolerance in the swept range moves the median
/// off 0.888 — this welder keeps about 11% fewer vertices than tool on
/// models where the count is unambiguous, and neither tightening nor
/// loosening changes that. So the difference is not a tolerance; it is
/// something tool compares and [`same_vertex`] does not. A JMS vertex
/// also carries a tangent, a binormal and a colour.
///
/// Channels 2 and 3 always get a tolerance of zero, meaning exact
/// equality, so packing an attribute into one of them makes the existing
/// predicate split on it without changing the library to find out.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "diagnostic; needs an H3EK install"]
fn does_tool_split_on_tangents_or_colour() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    let mut cases: Vec<(JmsFile, usize, String)> = Vec::new();
    let mut mismatched = 0usize;
    for path in walk(&kit.join("tags"), "render_model").iter().take(400) {
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();
        let Some(meshes) = root.field_path("render geometry/meshes").and_then(|f| f.as_block())
        else {
            continue;
        };
        if meshes.len() != 1 {
            continue;
        }
        let Some(parts) = meshes.iter().next().and_then(|m| m.field("parts")?.as_block()) else {
            continue;
        };
        if parts.len() != 1 {
            continue;
        }
        let Some(theirs) = parts
            .iter()
            .next()
            .and_then(|p| p.read_int_any("budget vertex count"))
            .map(|v| (v as i64 & 0xFFFF) as usize)
            .filter(|v| *v > 0)
        else {
            continue;
        };
        let Some(jms) = source_jms(&tag) else { continue };

        // Only compare against a source that actually built the tag.
        if jms.triangles.is_empty() || !source_matches_tag(&tag, &jms) {
            mismatched += 1;
            continue;
        }
        cases.push((jms, theirs, path.file_name().unwrap().to_string_lossy().into_owned()));
    }
    println!(
        "{} single-mesh single-part models ({mismatched} dropped: their compression bounds do not match their source, so the tag holds other geometry)",
        cases.len()
    );
    if cases.is_empty() {
        return;
    }

    // Are these attributes even populated?
    let (mut has_tangent, mut has_colour, mut total) = (0usize, 0usize, 0usize);
    for (jms, _, _) in &cases {
        for v in &jms.vertices {
            total += 1;
            if v.tangent.is_some_and(|t| t.i != 0.0 || t.j != 0.0 || t.k != 0.0) {
                has_tangent += 1;
            }
            if v.color.is_some() {
                has_colour += 1;
            }
        }
    }
    println!("  of {total} source vertices: {has_tangent} carry a tangent, {has_colour} a colour");

    let scale = blam_tags::render_import::JMS_TO_WORLD;
    let build = |jms: &JmsFile, extra: u8| -> Vec<WeldVertex> {
        jms.vertices
            .iter()
            .map(|v| {
                let mut texcoords: Vec<blam_tags::math::RealPoint2d> = v
                    .uvs
                    .iter()
                    .map(|t| blam_tags::math::RealPoint2d { x: t.x, y: 1.0 - t.y })
                    .collect();
                texcoords.resize(2, blam_tags::math::RealPoint2d { x: 0.0, y: 0.0 });
                match extra {
                    // Channel 2 has a zero tolerance, so anything put
                    // there must match exactly for two vertices to merge.
                    1 => {
                        let t = v.tangent.unwrap_or(blam_tags::math::RealVector3d {
                            i: 0.0,
                            j: 0.0,
                            k: 0.0,
                        });
                        texcoords.push(blam_tags::math::RealPoint2d { x: t.i, y: t.j });
                    }
                    2 => {
                        let b = v.binormal.unwrap_or(blam_tags::math::RealVector3d {
                            i: 0.0,
                            j: 0.0,
                            k: 0.0,
                        });
                        texcoords.push(blam_tags::math::RealPoint2d { x: b.i, y: b.j });
                    }
                    3 => {
                        let c = v.color.unwrap_or(blam_tags::math::RealPoint3d {
                            x: 0.0,
                            y: 0.0,
                            z: 0.0,
                        });
                        texcoords.push(blam_tags::math::RealPoint2d { x: c.x, y: c.y });
                    }
                    _ => {}
                }
                WeldVertex {
                    position: blam_tags::math::RealPoint3d {
                        x: v.position.x * scale,
                        y: v.position.y * scale,
                        z: v.position.z * scale,
                    },
                    normal: v.normal,
                    texcoords,
                    influences: v.node_sets.clone(),
                    color: v.color,
                }
            })
            .collect()
    };

    // The real configuration: `import_render_model`'s precise tolerance
    // plus the coarse one, which scales with the model, and the second
    // pass per section. Using `precise()` here measured a welder nobody
    // runs.
    let tol_for = |jms: &JmsFile| -> WeldTolerances {
        let s = blam_tags::render_import::JMS_TO_WORLD;
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &jms.vertices {
            for (a, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                lo[a] = lo[a].min(c * s);
                hi[a] = hi[a].max(c * s);
            }
        }
        let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a]));
        WeldTolerances::for_render_model(extent)
    };
    let tol = WeldTolerances::precise();
    for (label, extra) in
        [("baseline", 0u8), ("+ tangent", 1), ("+ binormal", 2), ("+ colour", 3)]
    {
        let mut ratios: Vec<f64> = cases
            .iter()
            .map(|(jms, theirs, _)| {
                weld(&build(jms, extra), &tol_for(jms)).vertices.len() as f64 / *theirs as f64
            })
            .collect();
        ratios.sort_unstable_by(f64::total_cmp);
        let median = ratios[ratios.len() / 2];
        let within = ratios.iter().filter(|r| (**r - 1.0).abs() <= 0.02).count();
        println!("  {label:<12} median {median:.3}  within 2%: {within}/{}", ratios.len());
    }

    // Does the driver's second pass — one weld_points per section at 70%
    // of both tolerances — actually merge anything? It is *tighter* than
    // the first, so it can only catch points that the first pass's
    // position averaging moved newly within reach.
    {
        let (mut differ, mut saved) = (0usize, 0i64);
        for (jms, _, _) in &cases {
            let src = build(jms, 0);
            let secs: Vec<i32> = vec![0; src.len()];
            let a = weld(&src, &tol_for(jms)).vertices.len();
            let b = blam_tags::weld::weld_sectioned(&src, &tol_for(jms), &[], &secs)
                .vertices
                .len();
            if a != b {
                differ += 1;
            }
            saved += a as i64 - b as i64;
        }
        println!(
            "  second pass (per section, 70%): changes {differ} of {} models, {saved} vertices",
            cases.len()
        );
    }

    // Which models agree and which do not, with enough shape to see what
    // separates them.
    let mut rows: Vec<(f64, usize, usize, usize, String)> = cases
        .iter()
        .map(|(jms, theirs, name)| {
            let mine = weld(&build(jms, 0), &tol).vertices.len();
            (mine as f64 / *theirs as f64, mine, *theirs, jms.triangles.len(), name.clone())
        })
        .collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    println!("  per model (ratio, mine, tool, triangles):");
    for (r, mine, theirs, tris, name) in &rows {
        println!("    {r:.3}  {mine:>6} v {theirs:>6}  {tris:>6} tris  {name}");
    }

    // A median hides what a tolerance does to any one model. Sweep the
    // ones furthest from tool on their own.
    // Positions are welded in world units here, after the 0.01 scale, so
    // the 1/32768 constant read off the binary is a hundred times looser
    // in real terms than the same number applied to the JMS's inches.
    // If tool welds before scaling, its effective tolerance is 3.05e-7 of
    // a world unit and it keeps far more points than this does.
    // How many texture coordinate sets does the parser actually hand the
    // welder? Every channel is compared, so a set that never arrives is a
    // criterion that never splits, and this would merge vertices tool
    // keeps apart.
    println!("  uv sets per model:");
    for (_, _, theirs, _, name) in rows.iter().take(6).chain(rows.iter().rev().take(2)) {
        let Some((jms, _, _)) = cases.iter().find(|(_, _, nm)| nm == name) else { continue };
        let mut hist: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
        for v in &jms.vertices {
            *hist.entry(v.uvs.len()).or_default() += 1;
        }
        println!("    {:<44} [{theirs}]  {hist:?}", name);
    }

    println!("  position tolerance, per model (tool's count in brackets):");
    let positions = [3.05e-7f32, 3.05e-6, 3.05e-5, 9.77e-4];
    print!("    {:<44}", "model [tool]");
    for pos in positions {
        print!(" {pos:>10.2e}");
    }
    println!();
    for (_, _, theirs, _, name) in rows.iter().take(6).chain(rows.iter().rev().take(2)) {
        let Some((jms, _, _)) = cases.iter().find(|(_, _, nm)| nm == name) else { continue };
        print!("    {:<44}", format!("{name} [{theirs}]"));
        for pos in positions {
            let t = WeldTolerances { position: pos, ..tol };
            print!(" {:>10}", weld(&build(jms, 0), &t).vertices.len());
        }
        println!();
    }

    println!("  normal tolerance, per model (tool's count in brackets):");
    let degrees = [0.02f32, 0.1, 0.5, 1.0, 5.0, 45.0];
    print!("    {:<44}", "model [tool]");
    for d in degrees {
        print!(" {d:>8}");
    }
    println!();
    for (_, _, theirs, _, name) in rows.iter().take(6).chain(rows.iter().rev().take(2)) {
        let Some((jms, _, _)) = cases.iter().find(|(_, _, nm)| nm == name) else { continue };
        print!("    {:<44}", format!("{name} [{theirs}]"));
        for d in degrees {
            let t = WeldTolerances { normal_degrees: d, ..tol };
            print!(" {:>8}", weld(&build(jms, 0), &t).vertices.len());
        }
        println!();
    }
}

/// Is a tag's `info` stream JMS the geometry that built it?
///
/// The vertex-count comparisons against tool only mean anything if the
/// two sides are the same model, and some tags provably fail that:
/// `butterfly_b` holds 24 vertices for a 6-triangle source, which has 18
/// corners. `h2a_magnum` is the case that matters and is not so blatant —
/// this welds its source to 2,469 vertices against tool's 6,056.
///
/// The tag carries its own answer. `compression info` holds the position
/// and texcoord bounds the vertices were quantised against, and those
/// come straight off the geometry tool actually built. If they match the
/// JMS's own bounds the two are the same geometry; if they do not, the
/// source is stale and the comparison was never valid.
///
/// Diagnostic. Run with `--ignored --nocapture`.
#[test]
#[ignore = "diagnostic; needs an H3EK install"]
fn does_the_info_stream_jms_match_the_tag() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };
    let wanted = [
        "h2a_magnum.render_model",
        "ark_cheap.render_model",
        "halo_reveal_matte_shot_16.render_model",
        "bird_small_multi.render_model",
        "butterfly_b.render_model",
        "guardian.render_model",
        "reach_flak_cannon.render_model",
    ];

    for path in walk(&kit.join("tags"), "render_model") {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if !wanted.contains(&name.as_str()) {
            continue;
        }
        let Ok(tag) = TagFile::read(&path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };
        let root = tag.root();
        let Some(ci) = root.field_path("render geometry/compression info").and_then(|f| f.as_block())
        else {
            continue;
        };
        let Some(first) = ci.iter().next() else { continue };
        let p0 = first.read_point3d("position bounds 0");
        let p1 = first.read_point3d("position bounds 1");
        let (b0, b1) = ((p0.x, p0.y, p0.z), (p1.x, p1.y, p1.z));

        // The JMS's own bounds, at the 0.01 scale the importer applies.
        let s = blam_tags::render_import::JMS_TO_WORLD;
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &jms.vertices {
            for (a, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                lo[a] = lo[a].min(c * s);
                hi[a] = hi[a].max(c * s);
            }
        }

        // The six floats are three consecutive (min, max) pairs, not two
        // corner points — `00_corpus_ground_truth.md` records this and
        // pairing them by axis instead makes matching geometry look
        // mismatched, which is exactly the mistake that produced the
        // "stale source" reading.
        let pairs = [(b0.0, b0.1), (b0.2, b1.0), (b1.1, b1.2)];
        println!("{name}  ({} triangles, {} vertices in the JMS)", jms.triangles.len(), jms.vertices.len());
        let mut worst = 0.0f32;
        for a in 0..3 {
            let (p, q) = pairs[a];
            let (tlo, thi) = (p.min(q), p.max(q));
            let d = (tlo - lo[a]).abs().max((thi - hi[a]).abs());
            worst = worst.max(d);
            println!(
                "  axis {a}: tag [{tlo:>12.7}, {thi:>12.7}]   jms [{:>12.7}, {:>12.7}]   diff {d:.2e}",
                lo[a], hi[a]
            );
        }
        // The bounds are stored as f32 of the same numbers, so a real
        // match is exact to rounding; anything above a whisker means the
        // tag was quantised against different geometry.
        println!(
            "  -> worst axis difference {worst:.3e}  {}",
            if worst < 1e-6 { "MATCH: same geometry" } else { "MISMATCH: the source is not what built this tag" }
        );
    }
}
