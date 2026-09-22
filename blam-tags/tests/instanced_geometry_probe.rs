//! What happens to instanced geometry across the trip.
//!
//! Diagnostic. Run with `--ignored --nocapture`.
//!
//! The round-trip test keeps `instanced_geometry` off because the
//! geometry does not come back unchanged with it on. This one turns it
//! on and asks what specifically differs, which is the thing that has to
//! be known before it can be fixed.

use std::path::{Path, PathBuf};

use blam_tags::ass::{AssFile, AssObjectPayload};
use blam_tags::sbsp_import::{role_of_material, MeshRole, SbspOptions};
use blam_tags::TagFile;

fn h3ek() -> Option<PathBuf> {
    ["D:/SteamLibrary/steamapps/common", "C:/Program Files (x86)/Steam/steamapps/common"]
        .iter()
        .map(|r| PathBuf::from(r).join("H3EK"))
        .find(|p| p.join("tags").is_dir())
}

fn schema() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/scenario_structure_bsp.json")
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

type Mesh<'a> = (&'a Vec<blam_tags::ass::AssVertex>, &'a Vec<blam_tags::ass::AssTriangle>);

fn render_meshes(ass: &AssFile) -> Vec<Mesh<'_>> {
    ass.objects
        .iter()
        .filter_map(|o| match &o.payload {
            AssObjectPayload::Mesh { vertices, triangles }
                if !vertices.is_empty() && !triangles.is_empty() =>
            {
                let role = triangles
                    .first()
                    .and_then(|t| ass.materials.get(t.material.max(0) as usize))
                    .map(|m| role_of_material(&m.name))
                    .unwrap_or(MeshRole::Render);
                (role == MeshRole::Render).then_some((vertices, triangles))
            }
            _ => None,
        })
        .collect()
}

/// The object index of each render mesh, in the same order.
fn render_object_indices(ass: &AssFile) -> Vec<usize> {
    ass.objects
        .iter()
        .enumerate()
        .filter_map(|(i, o)| match &o.payload {
            AssObjectPayload::Mesh { vertices, triangles }
                if !vertices.is_empty() && !triangles.is_empty() =>
            {
                let role = triangles
                    .first()
                    .and_then(|t| ass.materials.get(t.material.max(0) as usize))
                    .map(|m| role_of_material(&m.name))
                    .unwrap_or(MeshRole::Render);
                (role == MeshRole::Render).then_some(i)
            }
            _ => None,
        })
        .collect()
}

/// The whole scene as placed: every instance, with its object's geometry
/// carried through its own transform.
///
/// This is the invariant that actually holds. Object-for-object equality
/// does not: a level repeats a prop, the trip through world units
/// quantises two near-identical copies onto the same values, and the
/// exporter merges them into one definition placed twice. That is what
/// instancing is for, and the scene is unchanged — only the object count
/// moved. Comparing placed geometry sees through that, and still catches
/// a prop that genuinely went missing or landed somewhere else.
fn placed_scene(ass: &AssFile) -> Vec<(usize, usize, Vec<[f32; 3]>, usize)> {
    let mut out = Vec::new();
    for (_ii, inst) in ass.instances.iter().enumerate() {
        if inst.object_index < 0 {
            continue;
        }
        let Some(o) = ass.objects.get(inst.object_index as usize) else { continue };
        let AssObjectPayload::Mesh { vertices, triangles } = &o.payload else { continue };
        if vertices.is_empty() || triangles.is_empty() {
            continue;
        }
        let role = triangles
            .first()
            .and_then(|t| ass.materials.get(t.material.max(0) as usize))
            .map(|m| role_of_material(&m.name))
            .unwrap_or(MeshRole::Render);
        if role != MeshRole::Render {
            continue;
        }

        let q = &inst.local_rotation;
        let (x, y, z, w) = (q.i, q.j, q.k, q.w);
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        let r = [
            [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
            [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
            [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
        ];
        let s = inst.local_scale;
        let t = &inst.local_translation;

        let mut pts: Vec<[f32; 3]> = Vec::with_capacity(vertices.len());
        for v in vertices {
            let p = [v.position.x * s, v.position.y * s, v.position.z * s];
            let mut q = [0.0f32; 3];
            for a in 0..3 {
                q[a] = r[a][0] * p[0] + r[a][1] * p[1] + r[a][2] * p[2] + [t.x, t.y, t.z][a];
            }
            pts.push(q);
        }
        out.push((vertices.len(), triangles.len(), pts, inst.unique_id.max(0) as usize));
    }
    out
}

/// A mesh's shape, independent of where it sits: vertex and triangle
/// counts, and its extent. Enough to pair a mesh with itself across the
/// trip without assuming the order held.
fn shape(m: &Mesh<'_>) -> (usize, usize) {
    (m.0.len(), m.1.len())
}

fn centre(m: &Mesh<'_>) -> [f32; 3] {
    let inv = 1.0 / m.0.len().max(1) as f32;
    m.0.iter().fold([0.0f32; 3], |a, v| {
        [a[0] + v.position.x * inv, a[1] + v.position.y * inv, a[2] + v.position.z * inv]
    })
}

#[test]
#[ignore = "diagnostic; run with --ignored"]
fn what_differs_about_instanced_geometry() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: need an H3EK install");
        return;
    };
    let schema = schema();
    let limit: usize =
        std::env::var("BLAM_SBSP_MODELS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);

    for path in walk(&kit.join("tags"), "scenario_structure_bsp").iter().take(limit) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        let Ok(tag) = TagFile::read(path) else { continue };
        let Ok(source) = AssFile::from_scenario_structure_bsp(&tag) else { continue };

        let opts = SbspOptions { instanced_geometry: true };
        let (built, report) =
            match blam_tags::sbsp_import::structure_bsp_from_ass_with(&source, &schema, opts) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{name}: refused — {e}");
                    continue;
                }
            };
        let Ok(bytes) = built.write_to_bytes() else { continue };
        let Ok(reread) = TagFile::read_from_bytes(&bytes) else { continue };
        let Ok(back) = AssFile::from_scenario_structure_bsp(&reread) else { continue };

        let a = render_meshes(&source);
        let obj_index = render_object_indices(&source);
        let b = render_meshes(&back);
        eprintln!(
            "{name}: {} meshes in, {} out; {} definitions, {} placements",
            a.len(),
            b.len(),
            report.instance_definitions,
            report.instance_placements
        );

        // Matched on content, within the tolerance the trip actually
        // has, and against every candidate rather than the first.
        //
        // Two wrong instruments were tried before this one. Pairing by
        // vertex and triangle counts mispairs whenever a scene holds
        // several meshes the same size — a level full of repeated props
        // does constantly — and then reports the mismatch it caused
        // itself. Keying on exact float bits fails everything, because
        // positions go to world units and back and land a fraction of a
        // millimetre away; it called `anchor_point` broken, and that
        // scene has no instanced geometry at all.
        //
        // So: group the returned meshes by shape, and for each source
        // mesh accept any unused candidate whose vertices, texture
        // coordinates and triangles all agree.
        let same = |x: &Mesh<'_>, y: &Mesh<'_>| -> bool {
            if x.0.len() != y.0.len() || x.1.len() != y.1.len() {
                return false;
            }
            for (p, q) in x.0.iter().zip(y.0.iter()) {
                if (p.position.x - q.position.x).abs() > 0.05
                    || (p.position.y - q.position.y).abs() > 0.05
                    || (p.position.z - q.position.z).abs() > 0.05
                {
                    return false;
                }
                if let (Some(u1), Some(u2)) = (p.uvs.first(), q.uvs.first()) {
                    if (u1.x - u2.x).abs() > 1e-4 || (u1.y - u2.y).abs() > 1e-4 {
                        return false;
                    }
                }
            }
            x.1.iter().zip(y.1.iter()).all(|(p, q)| p.v == q.v && p.material == q.material)
        };

        let mut by_shape: std::collections::HashMap<(usize, usize), Vec<usize>> =
            Default::default();
        for (j, m) in b.iter().enumerate() {
            by_shape.entry(shape(m)).or_default().push(j);
        }
        let mut used = vec![false; b.len()];
        let mut unpaired = 0usize;
        let mut shown = 0usize;
        for (i, m) in a.iter().enumerate() {
            let hit = by_shape
                .get(&shape(m))
                .and_then(|cands| cands.iter().copied().find(|&j| !used[j] && same(m, &b[j])));
            match hit {
                Some(j) => used[j] = true,
                None => {
                    unpaired += 1;
                    if shown < 5 {
                        let c = centre(m);
                        let obj = obj_index.get(i).copied().unwrap_or(usize::MAX);
                        let places: Vec<&blam_tags::ass::AssInstance> = source
                            .instances
                            .iter()
                            .filter(|inst| inst.object_index == obj as i32)
                            .collect();
                        let moved = places.iter().any(|inst| {
                            let t = &inst.local_translation;
                            let q = &inst.local_rotation;
                            (inst.local_scale - 1.0).abs() > 1e-5
                                || t.x.abs() > 1e-5
                                || t.y.abs() > 1e-5
                                || t.z.abs() > 1e-5
                                || q.i.abs() > 1e-5
                                || q.j.abs() > 1e-5
                                || q.k.abs() > 1e-5
                                || (q.w.abs() - 1.0).abs() > 1e-5
                        });
                        // Does another source mesh carry the same
                        // geometry? The exporter collapses definitions
                        // whose vertex and triangle data match, so a twin
                        // is the difference between "lost" and "merged".
                        let twins = a
                            .iter()
                            .enumerate()
                            .filter(|(k, o)| *k != i && same(m, o))
                            .count();
                        let twins_no_uv = a
                            .iter()
                            .enumerate()
                            .filter(|(k, o)| {
                                *k != i
                                    && o.0.len() == m.0.len()
                                    && o.1.len() == m.1.len()
                                    && o.0.iter().zip(m.0.iter()).all(|(p, q)| {
                                        (p.position.x - q.position.x).abs() <= 0.05
                                            && (p.position.y - q.position.y).abs() <= 0.05
                                            && (p.position.z - q.position.z).abs() <= 0.05
                                    })
                                    && o.1.iter().zip(m.1.iter()).all(|(p, q)| p.v == q.v)
                            })
                            .count();
                        eprintln!(
                            "  source mesh {i} (object {obj}): {twins} exact twins, {twins_no_uv} twins ignoring UVs; {} verts {} tris at ({:.1},{:.1},{:.1}); {} placements, moved={moved} — did not come back",
                            m.0.len(),
                            m.1.len(),
                            c[0],
                            c[1],
                            c[2],
                            places.len()
                        );
                        shown += 1;
                    }
                }
            }
        }
        let moved = used.iter().filter(|u| !**u).count();
        // And the check that matters: the same scene, placed.
        let (ps, pb) = (placed_scene(&source), placed_scene(&back));
        // Paired by shape and then by best fit, and reported as a
        // distance. A quantised key only says "different"; what matters
        // is whether a prop is a rounding error away from where it was
        // or somewhere else entirely.
        let mut taken = vec![false; pb.len()];
        let mut worst = 0.0f32;
        let mut unplaced = 0usize;
        for (vc, tc, a_pts, uid) in &ps {
            let mut best: Option<(usize, f32)> = None;
            for (j, (vc2, tc2, b_pts, _)) in pb.iter().enumerate() {
                if taken[j] || vc2 != vc || tc2 != tc {
                    continue;
                }
                let d = a_pts.iter().zip(b_pts.iter()).fold(0.0f32, |acc, (p, q)| {
                    acc.max((p[0] - q[0]).abs()).max((p[1] - q[1]).abs()).max((p[2] - q[2]).abs())
                });
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((j, d));
                }
            }
            match best {
                Some((j, d)) => {
                    taken[j] = true;
                    worst = worst.max(d);
                }
                None => {
                    unplaced += 1;
                    if let Some(inst) =
                        source.instances.iter().find(|i| i.unique_id.max(0) as usize == *uid)
                    {
                        let q = &inst.local_rotation;
                        let t = &inst.local_translation;
                        eprintln!(
                            "    unplaced: object {} shape {vc}v/{tc}t scale {:.4} quat ({:.4},{:.4},{:.4},{:.4}) pos ({:.1},{:.1},{:.1})",
                            inst.object_index, inst.local_scale, q.i, q.j, q.k, q.w, t.x, t.y, t.z
                        );
                    }
                }
            }
        }
        eprintln!(
            "  placed scene: {} in, {} out; {unplaced} unplaced, worst vertex moved {worst:.4} cm",
            ps.len(),
            pb.len()
        );
        let worst = 0.0f32;
        let _ = worst;
        eprintln!("  {unpaired} source meshes did not come back; {moved} returned meshes matched nothing");
    }
}
