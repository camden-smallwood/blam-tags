//! How does tool decide which cluster a leaf belongs to?
//!
//! Diagnostic. Run with `--ignored --nocapture`.
//!
//! The `leaves` block is one `char_integer` per leaf naming its cluster,
//! and it is exactly parallel to the collision BSP's leaves — measured,
//! not assumed: guardian has 11,305 of each, armory 12,343, isolation
//! 19,594. So the question is a function from a point in space to a
//! cluster, and shipped levels are the answer key.
//!
//! This scores candidate rules against that key before any of them go
//! into the importer.

use std::path::{Path, PathBuf};

use blam_tags::TagFile;

fn h3ek() -> Option<PathBuf> {
    ["D:/SteamLibrary/steamapps/common", "C:/Program Files (x86)/Steam/steamapps/common"]
        .iter()
        .map(|r| PathBuf::from(r).join("H3EK"))
        .find(|p| p.join("tags").is_dir())
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

#[test]
#[ignore = "diagnostic; run with --ignored"]
fn how_tool_assigns_leaves_to_clusters() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: need an H3EK install");
        return;
    };
    let limit: usize =
        std::env::var("BLAM_SBSP_MODELS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);

    let (mut total, mut in_bounds_hit, mut nearest_hit, mut unreached) = (0usize, 0, 0, 0);

    for path in walk(&kit.join("tags"), "scenario_structure_bsp").iter().take(limit) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let Ok(tag) = TagFile::read(path) else { continue };
        let root = tag.root();

        // Tool's answer key.
        let Some(leaves) = root.field_path("leaves").and_then(|f| f.as_block()) else { continue };
        let truth: Vec<i32> = (0..leaves.len())
            .map(|i| {
                leaves
                    .element(i)
                    .and_then(|e| e.read_int_any("cluster"))
                    .map(|v| v as i32)
                    .unwrap_or(-1)
            })
            .collect();

        // Cluster bounds.
        let Some(cl) = root.field_path("clusters").and_then(|f| f.as_block()) else { continue };
        let mut boxes: Vec<[[f32; 2]; 3]> = Vec::new();
        for i in 0..cl.len() {
            let Some(e) = cl.element(i) else { continue };
            let mut b = [[0.0f32; 2]; 3];
            for (k, nm) in ["bounds x", "bounds y", "bounds z"].iter().enumerate() {
                let bd = e.read_real_bounds(nm);
                b[k] = [bd.lower, bd.upper];
            }
            boxes.push(b);
        }
        if boxes.is_empty() {
            continue;
        }

        // The world box, so the descent has somewhere to start.
        let mut world = [[0.0f32; 2]; 3];
        for (k, nm) in ["world bounds x", "world bounds y", "world bounds z"].iter().enumerate() {
            let bd = root.read_real_bounds(nm);
            world[k] = [bd.lower, bd.upper];
        }

        let Some(coll) = root
            .field_path("resource interface/raw_resources[0]/raw_items/collision bsp")
            .and_then(|f| f.as_block())
            .and_then(|b| b.element(0))
        else {
            continue;
        };
        let Some(centres) = blam_tags::collision_verify::leaf_centres(&coll, world) else {
            continue;
        };
        if centres.len() != truth.len() {
            eprintln!("{name}: {} leaf centres for {} leaves", centres.len(), truth.len());
            continue;
        }

        // The exact chain: a leaf indexes collision surfaces, a
        // structure surface maps to render triangles, and a mapping
        // entry names the section — which is the mesh a cluster owns.
        // No geometry test at all, if it holds.
        let section_of_cluster: Vec<i32> = (0..cl.len())
            .map(|i| {
                cl.element(i)
                    .and_then(|e| e.read_int_any("mesh index"))
                    .map(|v| v as i32)
                    .unwrap_or(-1)
            })
            .collect();
        let mapping = root
            .field_path("structure surface to triangle mapping")
            .and_then(|f| f.as_block());
        let surfaces_small = root.field_path("structure surfaces").and_then(|f| f.as_block());
        let surfaces_large = root.field_path("large structure surfaces").and_then(|f| f.as_block());
        let surf_len = surfaces_small.as_ref().map(|b| b.len()).unwrap_or(0)
            + surfaces_large.as_ref().map(|b| b.len()).unwrap_or(0);
        let leaf_surfaces = blam_tags::collision_verify::leaf_surface_lists(&coll);
        let section_for_surface = |si: usize| -> Option<i32> {
            let (first, count) = if let Some(b) = surfaces_small.as_ref().filter(|b| si < b.len()) {
                let e = b.element(si)?;
                (
                    e.read_int_any("first_structure_surface_to_triangle_mapping_index")? as i64,
                    e.read_int_any("structure_surface_to_triangle_mapping_count")? as i64,
                )
            } else {
                let b = surfaces_large.as_ref()?;
                let e = b.element(si)?;
                (
                    e.read_int_any("first_structure_surface_to_triangle_mapping_index")? as i64,
                    e.read_int_any("structure_surface_to_triangle_mapping_count")? as i64,
                )
            };
            if count <= 0 {
                return None;
            }
            let m = mapping.as_ref()?;
            let e = m.element(first as usize)?;
            e.read_int_any("section_index").map(|v| v as i32)
        };

        let mut chain_hit = 0usize;
        let mut chain_tried = 0usize;
        let mut no_surfaces = 0usize;
        if let Some(ls) = &leaf_surfaces {
            for (i, want) in truth.iter().enumerate() {
                let Some(list) = ls.get(i) else { continue };
                if list.is_empty() {
                    no_surfaces += 1;
                    continue;
                }
                let Some(sec) = list
                    .iter()
                    .find_map(|&sfi| section_for_surface(sfi as usize))
                else {
                    continue;
                };
                chain_tried += 1;
                let via = section_of_cluster.iter().position(|s| *s == sec).map(|v| v as i32);
                if via == Some(*want) {
                    chain_hit += 1;
                }
            }
        }
        eprintln!(
            "  {name}: surface->section->cluster {:.1}% of {chain_tried} tried; {no_surfaces} leaves index no surface; {surf_len} structure surfaces",
            100.0 * chain_hit as f64 / chain_tried.max(1) as f64
        );

        // Rule C needs the geometry each cluster owns, not its box.
        // A cluster's mesh is named by `mesh index`, and the positions
        // live in the render geometry's per-mesh vertices.
        let mut cluster_points: Vec<Vec<[f32; 3]>> = Vec::new();
        for &mi in &section_of_cluster {
            let mut pts = Vec::new();
            if mi >= 0 {
                if let Some(pmt) = root
                    .field_path("render geometry/per mesh temporary")
                    .and_then(|f| f.as_block())
                    .and_then(|b| b.element(mi as usize))
                    .and_then(|e| e.field("raw vertices"))
                    .and_then(|f| f.as_block())
                {
                    // Sampled: a cluster mesh can hold tens of thousands
                    // of vertices and the shape is what matters here.
                    let step = 1;
                    for k in (0..pmt.len()).step_by(step) {
                        if let Some(v) = pmt.element(k) {
                            let p = v.read_point3d("position");
                            pts.push([p.x, p.y, p.z]);
                        }
                    }
                }
            }
            cluster_points.push(pts);
        }

        let (mut t, mut a, mut b, mut u) = (0usize, 0, 0, 0);
        let mut near_geom_hit = 0usize;
        let (mut small_hit, mut negatives) = (0usize, 0usize);
        for (i, want) in truth.iter().enumerate() {
            let Some(p) = centres[i] else {
                u += 1;
                continue;
            };
            t += 1;
            // Rule A: the cluster whose bounds contain the point, first
            // one wins.
            let inside = boxes.iter().position(|bx| {
                (0..3).all(|k| p[k] >= bx[k][0] - 0.01 && p[k] <= bx[k][1] + 0.01)
            });
            if inside.map(|v| v as i32) == Some(*want) {
                a += 1;
            }
            // Rule A2: of the boxes containing the point, the
            // smallest. Cluster boxes overlap heavily, so "the first
            // one" is an arbitrary choice among several right answers.
            let smallest = boxes
                .iter()
                .enumerate()
                .filter(|(_, bx)| {
                    (0..3).all(|k| p[k] >= bx[k][0] - 0.01 && p[k] <= bx[k][1] + 0.01)
                })
                .min_by(|(_, x), (_, y)| {
                    let vol = |bx: &[[f32; 2]; 3]| {
                        (0..3).map(|k| (bx[k][1] - bx[k][0]).max(0.0)).product::<f32>()
                    };
                    vol(x).total_cmp(&vol(y))
                })
                .map(|(i, _)| i as i32);
            if smallest == Some(*want) {
                small_hit += 1;
            }
            if *want < 0 {
                negatives += 1;
            }
            // Rule C: the cluster whose own geometry comes nearest.
            let near_geom = cluster_points
                .iter()
                .enumerate()
                .filter(|(_, pts)| !pts.is_empty())
                .min_by(|(_, x), (_, y)| {
                    let d = |pts: &Vec<[f32; 3]>| {
                        pts.iter()
                            .map(|q| {
                                (0..3).map(|k| (p[k] - q[k]) * (p[k] - q[k])).sum::<f32>()
                            })
                            .fold(f32::MAX, f32::min)
                    };
                    d(x).total_cmp(&d(y))
                })
                .map(|(i, _)| i as i32);
            if near_geom == Some(*want) {
                near_geom_hit += 1;
            }
            // Rule B: the cluster whose box centre is nearest.
            let near = boxes
                .iter()
                .enumerate()
                .min_by(|(_, x), (_, y)| {
                    let d = |bx: &[[f32; 2]; 3]| {
                        (0..3)
                            .map(|k| {
                                let c = 0.5 * (bx[k][0] + bx[k][1]);
                                (p[k] - c) * (p[k] - c)
                            })
                            .sum::<f32>()
                    };
                    d(x).total_cmp(&d(y))
                })
                .map(|(i, _)| i as i32);
            if near == Some(*want) {
                b += 1;
            }
        }
        eprintln!(
            "{name}: {} leaves, {} clusters; first-box {:.1}%, smallest-box {:.1}%, nearest-centre {:.1}%, NEAREST-GEOMETRY {:.1}%, {negatives} truth are negative, {u} unreached",
            truth.len(),
            boxes.len(),
            100.0 * a as f64 / t.max(1) as f64,
            100.0 * small_hit as f64 / t.max(1) as f64,
            100.0 * b as f64 / t.max(1) as f64,
            100.0 * near_geom_hit as f64 / t.max(1) as f64
        );
        total += t;
        in_bounds_hit += a;
        nearest_hit += b;
        unreached += u;
    }

    eprintln!(
        "over {total} leaves: inside-bounds {:.1}%, nearest-centre {:.1}%, {unreached} unreached",
        100.0 * in_bounds_hit as f64 / total.max(1) as f64,
        100.0 * nearest_hit as f64 / total.max(1) as f64
    );
}
