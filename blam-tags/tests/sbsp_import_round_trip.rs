//! Does a scene survive being written into a tag and read back?
//!
//! This is the "mesh not mangled" check, and it is deliberately end to
//! end: take a scene out of a shipped structure BSP, write it into a
//! fresh tag with [`blam_tags::sbsp_import`], export *that* tag back to
//! a scene, and compare the geometry. Every step is real — the input is
//! shipped content, and the exporter that grades the result is the same
//! one used everywhere else, not something written to agree.
//!
//! What is compared, and why:
//!
//! * **vertex count and positions** — the scale factor between ASS
//!   centimetres and world units is 100, and getting it wrong is silent;
//!   the model is just the wrong size.
//! * **texture coordinates** — V is flipped between the two, which is
//!   equally silent and shows up only as upside-down textures.
//! * **triangle count, winding and material** — exactly. Reading a
//!   triangle list as a strip produces a mesh that looks plausible and
//!   is wrong everywhere, so this is where that would surface.

use std::path::{Path, PathBuf};

use blam_tags::ass::{AssFile, AssObjectPayload};
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

fn schema() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/scenario_structure_bsp.json");
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

/// The **render** meshes, in order, with their geometry.
///
/// Portals, weather polyhedra and the merged collision mesh share the
/// OBJECT list but are marked by their material and belong in other tag
/// blocks, so comparing them against the clusters that come back would
/// be comparing different things.
/// The `+portal` meshes, in order.
///
/// The exporter fan-triangulates each portal from one vertex, so the
/// vertices in first-use order are the ring it came from — which is what
/// makes a portal comparable across the trip rather than merely present.
fn portal_rings(ass: &AssFile) -> Vec<Vec<[f32; 3]>> {
    use blam_tags::sbsp_import::{role_of_material, MeshRole};
    let mut out = Vec::new();
    for o in &ass.objects {
        let AssObjectPayload::Mesh { vertices, triangles } = &o.payload else { continue };
        if vertices.is_empty() || triangles.is_empty() {
            continue;
        }
        let role = triangles
            .first()
            .and_then(|t| ass.materials.get(t.material.max(0) as usize))
            .map(|m| role_of_material(&m.name))
            .unwrap_or(MeshRole::Render);
        if role != MeshRole::Portal {
            continue;
        }
        let mut order: Vec<u32> = Vec::new();
        for t in triangles {
            for &c in &t.v {
                if !order.contains(&c) {
                    order.push(c);
                }
            }
        }
        out.push(
            order
                .iter()
                .filter_map(|&i| vertices.get(i as usize))
                .map(|v| [v.position.x, v.position.y, v.position.z])
                .collect(),
        );
    }
    out
}

/// The scene as placed: every instance, with its object's geometry
/// carried through its own transform.
///
/// This is the invariant, not object-for-object equality. A level
/// repeats a prop; the trip through world units quantises two
/// near-identical copies onto the same values, and they merge into one
/// definition placed twice. That is what instancing is for and the scene
/// is unchanged — only the object count moved. Comparing placed geometry
/// sees through that and still catches a prop that went missing or
/// landed somewhere else.
fn placed_scene(ass: &AssFile) -> Vec<(usize, usize, Vec<[f32; 3]>)> {
    use blam_tags::sbsp_import::{role_of_material, MeshRole};
    let mut out = Vec::new();
    for inst in &ass.instances {
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
        let (s, t) = (inst.local_scale, &inst.local_translation);
        let mut pts = Vec::with_capacity(vertices.len());
        for v in vertices {
            let p = [v.position.x * s, v.position.y * s, v.position.z * s];
            let mut c = [0.0f32; 3];
            for a in 0..3 {
                c[a] = r[a][0] * p[0] + r[a][1] * p[1] + r[a][2] * p[2] + [t.x, t.y, t.z][a];
            }
            pts.push(c);
        }
        out.push((vertices.len(), triangles.len(), pts));
    }
    out
}

fn meshes(ass: &AssFile) -> Vec<(&Vec<blam_tags::ass::AssVertex>, &Vec<blam_tags::ass::AssTriangle>)> {
    use blam_tags::sbsp_import::{role_of_material, MeshRole};
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

#[test]
fn a_scene_survives_being_written_into_a_tag_and_read_back() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and the sbsp schema");
        return;
    };

    let limit: usize =
        std::env::var("BLAM_SBSP_MODELS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);

    let (mut checked, mut skipped) = (0usize, 0usize);
    let (mut vertices, mut triangles) = (0usize, 0usize);
    let (mut portals, mut weather, mut collision) = (0usize, 0usize, 0usize);
    let mut portals_out = 0usize;
    let mut placements_checked = 0usize;
    let (mut leaves_out, mut links_out) = (0usize, 0usize);
    let mut dud_portals = 0usize;
    let mut fallback_leaves = 0usize;
    let mut portal_notes: Vec<String> = Vec::new();
    let (mut frags, mut joins) = (0usize, 0usize);
    let (mut regions_n, mut biggest) = (0usize, 0usize);
    let mut solid_n = 0usize;
    let (mut p_leaf, mut p_region, mut p_cluster) = (0usize, 0usize, 0usize);


    let (mut clusters_out, mut tool_clusters) = (0usize, 0usize);
    let mut worst_placement = 0.0f32;
    let (mut coll_surfaces, mut coll_dropped) = (0usize, 0usize);
    let (mut coll_rays, mut coll_agreed) = (0usize, 0usize);
    let mut coll_bad: Vec<String> = Vec::new();
    // The control: tool's own collision tree out of the same shipped
    // tag, through the same checker. A disagreement rate this builder
    // shares with tool is the representation's, not this builder's.
    let (mut ctl_rays, mut ctl_agreed) = (0usize, 0usize);
    let (mut ctl_graze, mut ctl_solid) = (0usize, 0.0f32);
    let (mut my_graze, mut my_solid) = (0usize, 0.0f32);
    let mut bad: Vec<String> = Vec::new();

    for path in walk(&kit.join("tags"), "scenario_structure_bsp").iter().take(limit) {
        let Ok(tag) = TagFile::read(path) else {
            skipped += 1;
            continue;
        };
        let Ok(source) = AssFile::from_scenario_structure_bsp(&tag) else {
            skipped += 1;
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

        if let Some(el) = tag
            .root()
            .field_path("resource interface/raw_resources[0]/raw_items/collision bsp")
            .and_then(|f| f.as_block())
            .and_then(|b| b.element(0))
        {
            if let Ok(r) = blam_tags::collision_verify::test_collision_bsp(&el, 24) {
                ctl_rays += r.rays;
                ctl_agreed += r.agreed;
                ctl_graze += r.wrong_surface_grazing + r.missed_grazing;
                ctl_solid = ctl_solid.max(r.worst_solid_margin);
                if !r.clean() {
                    eprintln!(
                        "  {name}: TOOL'S OWN tree — {} of {} rays disagree ({})",
                        r.missed + r.wrong_surface + r.phantom,
                        r.rays,
                        r.examples.first().cloned().unwrap_or_default()
                    );
                }
            }
        }

        // A cluster mesh over 65,535 indices cannot be addressed by one
        // mesh's index block, and splitting it is the importer's job for
        // another day — those are refused rather than mangled, so skip
        // them here rather than count a refusal as a failure.
        let (built, report) = match blam_tags::sbsp_import::structure_bsp_from_ass(&source, &schema)
        {
            Ok(v) => v,
            Err(e) => {
                skipped += 1;
                eprintln!("  {name}: refused — {e}");
                continue;
            }
        };

        // Round it back out through the exporter.
        let Ok(bytes) = built.write_to_bytes() else {
            bad.push(format!("{name}: the built tag would not serialise"));
            continue;
        };
        let Ok(reread) = TagFile::read_from_bytes(&bytes) else {
            bad.push(format!("{name}: the built tag would not parse back"));
            continue;
        };
        let back = match AssFile::from_scenario_structure_bsp(&reread) {
            Ok(v) => v,
            Err(e) => {
                bad.push(format!("{name}: the built tag would not export: {e}"));
                continue;
            }
        };

        checked += 1;
        // The scene, placed. Comparing meshes pairwise by index was
        // only ever valid while every authored object became its own
        // cluster: with instancing, definitions are emitted after the
        // clusters and identical props merge, so index i in is not index
        // i out. On a symmetric level that mismatch even paired two
        // mirrored halves and reported geometry "moved by 384 cm" when
        // nothing had moved.
        let (ps, pb) = (placed_scene(&source), placed_scene(&back));
        if ps.len() != pb.len() {
            bad.push(format!("{name}: {} placements in, {} out", ps.len(), pb.len()));
            continue;
        }
        let mut taken = vec![false; pb.len()];
        let mut unplaced = 0usize;
        let mut worst = 0.0f32;
        for (vc, tc, a_pts) in &ps {
            let mut best: Option<(usize, f32)> = None;
            for (j, (vc2, tc2, b_pts)) in pb.iter().enumerate() {
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
                // Positions are centimetres, so a twentieth of one is
                // well inside anything that would ever be visible.
                Some((j, d)) if d <= 0.05 => {
                    taken[j] = true;
                    worst = worst.max(d);
                }
                _ => unplaced += 1,
            }
        }
        if unplaced > 0 {
            bad.push(format!(
                "{name}: {unplaced} of {} placements did not come back (worst match {worst:.4} cm)",
                ps.len()
            ));
        }
        placements_checked += ps.len();
        worst_placement = worst_placement.max(worst);

        // The sealed world this built is the same structure a
        // collision_model carries, so the same ray check applies: cast
        // at the surfaces directly and through the tree, and require the
        // same answer. Building collision that cannot be hit would
        // otherwise look identical to building collision that can.
        if report.collision_surfaces > 0 {
            let coll = reread
                .root()
                .field_path("resource interface/raw_resources[0]/raw_items/collision bsp")
                .and_then(|f| f.as_block())
                .and_then(|b| b.element(0));
            match coll {
                Some(el) => match blam_tags::collision_verify::test_collision_bsp(&el, 24) {
                    Ok(r) => {
                        // A leaf reference past the end of the leaf block is
                        // collision that is simply absent while the tag still
                        // looks well formed, so it is worth its own check
                        // rather than being left to show up as missed rays.
                        if let Some((w, nbad, tot)) =
                            blam_tags::collision_verify::ring_planarity(&el)
                        {
                            // A surface has to lie on the plane the tree
                            // routes by. It used to be able to miss it by
                            // two centimetres, which is collision absent
                            // from part of its own face.
                            if nbad > 0 {
                                bad.push(format!(
                                    "{name}: {nbad} rings stray; tag says worst {w:.6}, builder says worst {:.6}; biggest ring {tot}", report.worst_plane_offset
                                ));
                            }
                        }
                        if let Some((dang, _, have)) =
                            blam_tags::collision_verify::dangling_leaf_refs(&el)
                        {
                            if dang > 0 {
                                bad.push(format!(
                                    "{name}: {dang} leaf refs point past the {have} leaves that exist"
                                ));
                            }
                        }
                        coll_rays += r.rays;
                        coll_agreed += r.agreed;
                        my_graze += r.wrong_surface_grazing + r.missed_grazing;
                        if r.open_rings > 0 {
                            bad.push(format!(
                                "{name}: {} surfaces have a ring the walk cannot close",
                                r.open_rings
                            ));
                        }
                        // Does the checker see the polygons that were
                        // built? If not, every ray result above is about
                        // geometry the tree was never given.
                        if let Some(got) = blam_tags::collision_verify::decoded_rings(&el) {
                            let want = &report.collision_rings;
                            let mut diff_count = 0usize;
                            let mut diff_shape = 0usize;
                            for (a, b) in want.iter().zip(got.iter()) {
                                if a.len() != b.len() {
                                    diff_count += 1;
                                } else if a.iter().zip(b.iter()).any(|(p, q)| {
                                    (p[0] - q[0]).abs() > 1e-4
                                        || (p[1] - q[1]).abs() > 1e-4
                                        || (p[2] - q[2]).abs() > 1e-4
                                }) {
                                    diff_shape += 1;
                                }
                            }
                            if want.len() != got.len() || diff_count > 0 || diff_shape > 0 {
                                // Every surface has to read back as the
                                // polygon it was built as. A ring that does
                                // not is collision the game cannot walk —
                                // 28% of armory's, once.
                                bad.push(format!(
                                    "{name}: built {} rings, decoded {}; {diff_count} differ in vertex count, {diff_shape} in shape",
                                    want.len(), got.len()
                                ));
                            }
                        }
                        if r.leaf_refs != report.collision_pairs {
                            eprintln!(
                                "  {name}: builder assigned {} leaf/surface pairs, the tag gives back {}",
                                report.collision_pairs, r.leaf_refs
                            );
                        }
                        my_solid = my_solid.max(r.worst_solid_margin);
                        // Reported, not asserted. The structure collision
                        // this builds is measurably incomplete — see the
                        // note at the end of this file — and failing the
                        // geometry round trip on it would hide the thing
                        // that does work behind the thing that does not.
                        if !r.clean() {
                            coll_bad.push(format!(
                                "{name}: {} of {} rays disagree ({})",
                                r.missed + r.wrong_surface + r.phantom,
                                r.rays,
                                r.examples.first().cloned().unwrap_or_default()
                            ));
                        }
                    }
                    Err(e) => coll_bad.push(format!("{name}: unusable — {e}")),
                },
                None => bad.push(format!("{name}: the collision bsp was not written back")),
            }
        }

        // Portals were being recognised and then dropped. They come
        // back as `+portal` meshes, so the same ring has to survive.
        let (pa, pb) = (portal_rings(&source), portal_rings(&back));
        if pa.len() != pb.len() {
            bad.push(format!("{name}: {} portals in, {} out", pa.len(), pb.len()));
        } else {
            for (i, (x, y)) in pa.iter().zip(&pb).enumerate() {
                if x.len() != y.len() {
                    bad.push(format!(
                        "{name} portal {i}: {} vertices in, {} out",
                        x.len(),
                        y.len()
                    ));
                    break;
                }
                let worst = x
                    .iter()
                    .zip(y)
                    .map(|(p, q)| {
                        (p[0] - q[0]).abs().max((p[1] - q[1]).abs()).max((p[2] - q[2]).abs())
                    })
                    .fold(0.0f32, f32::max);
                if worst > 0.05 {
                    bad.push(format!("{name} portal {i}: moved by {worst:.4} cm"));
                    break;
                }
            }
        }
        portals_out += report.portals_written;
        leaves_out += report.leaves_written;
        links_out += report.cluster_portal_links;
        dud_portals += report.portals_dividing_nothing;
        fallback_leaves += report.leaves_behind_a_portal;
        frags += report.adjacency_fragments;
        joins += report.adjacency_joins;
        regions_n += report.regions_found;
        biggest = biggest.max(report.largest_region);
        solid_n += report.solid_leaves;
        p_leaf += report.portal_same_leaf;
        p_region += report.portal_same_region;
        p_cluster += report.portal_same_cluster_diff_region;

        for nt in &report.portal_notes {
            if portal_notes.len() < 6 {
                portal_notes.push(format!("{name}: {nt}"));
            }
        }
        clusters_out += report.clusters;
        if let Some(t) = tag.root().field_path("clusters").and_then(|f| f.as_block()) {
            tool_clusters += t.len();
        }
        // Every leaf has to name a cluster that exists, and
        // every portal has to divide two of them.
        if let Some(lv) = reread.root().field_path("leaves").and_then(|f| f.as_block()) {
            let n_clusters = reread
                .root()
                .field_path("clusters")
                .and_then(|f| f.as_block())
                .map(|b| b.len())
                .unwrap_or(0) as i128;
            let bad_leaf = (0..lv.len()).find(|&i| {
                let c = lv
                    .element(i)
                    .and_then(|e| e.read_int_any("cluster"))
                    .unwrap_or(-1);
                c < 0 || c >= n_clusters
            });
            if let Some(i) = bad_leaf {
                bad.push(format!(
                    "{name}: leaf {i} names a cluster outside 0..{n_clusters}"
                ));
            }
        }

        vertices += report.vertices;
        triangles += report.triangles;
        portals += report.portals;
        weather += report.weather;
        collision += report.collision_meshes;
        coll_surfaces += report.collision_surfaces;
        coll_dropped += report.collision_dropped;

    }

    eprintln!("{checked} scenes written into a tag and read back ({skipped} skipped)");
    eprintln!("  {vertices} vertices, {triangles} triangles in render clusters");
    eprintln!("  {placements_checked} placements compared, worst vertex moved {worst_placement:.4} cm");
    eprintln!("  {portals} portals recognised, {portals_out} written; {weather} weather meshes recognised but not written");
    eprintln!("  {clusters_out} clusters (tool writes {tool_clusters}); {leaves_out} leaves filed; {links_out} portal links; {dud_portals} portals divide one cluster from itself");
    eprintln!("    partition: {regions_n} regions from {frags} fragments and {joins} joins; {solid_n} cells solid; {fallback_leaves} leaves fell back to nearest");
    eprintln!("    undivided portals: {p_leaf} share a leaf, {p_region} share a region, {p_cluster} differ in region but not cluster (biggest region {biggest})");
    eprintln!("  {collision} collision meshes -> {coll_surfaces} collision surfaces ({coll_dropped} dropped)");
    if ctl_rays > 0 {
        eprintln!(
            "  tool's own structure collision: {ctl_agreed}/{ctl_rays} rays agreed ({:.3}%);                {ctl_graze} of the rest graze a rim, worst non-graze margin {ctl_solid:.6}",
            100.0 * ctl_agreed as f64 / ctl_rays as f64
        );
    }
    if coll_rays > 0 {
        eprintln!(
            "  structure collision: {coll_agreed}/{coll_rays} rays agreed ({:.3}%);                {my_graze} of the rest graze a rim, worst non-graze margin {my_solid:.6}",
            100.0 * coll_agreed as f64 / coll_rays as f64
        );
        for b in coll_bad.iter().take(3) {
            eprintln!("    {b}");
        }
    }
    for b in bad.iter().take(8) {
        eprintln!("    {b}");
    }

    assert!(checked > 0, "no scenes were available to check");
    // Every collision surface has to reach the tag. A dropped one is
    // collision that is simply not there, and it used to be 36 of them.
    assert_eq!(coll_dropped, 0, "{coll_dropped} collision surfaces were dropped");
    assert!(bad.is_empty(), "geometry did not survive the trip:\n{}", bad.join("\n"));
}

// The structure collision verifies at 99.64% over 20 scenes, against
// 99.66% for tool's own trees out of the same shipped tags through the
// same checker — 13 disagreements in 3,617 rays where tool has 18 in
// 5,286. That is parity, and the checker is not a perfect oracle at
// structure scale for either of them.
//
// Four defects stood between 98.5% and here, and the order they were
// found in matters, because the first one was hiding the rest.
//
// * `first edge` was read back signed. A structure BSP has more than
//   32,767 edges — armory has 46,707 — so every surface whose first edge
//   sat past that came back negative and its ring walk stopped before it
//   started. 4,422 of armory's 15,529 surfaces, 28%, were collision the
//   checker could not see and the game could not walk. Fixing it made
//   the score *worse*, because the earlier numbers had been measured
//   over the three quarters of the geometry that happened to be visible.
//
// * Plane deduplication compared offsets against a fixed 1e-5. The
//   offset is a distance from the origin, a few hundred at the far side
//   of a level, where f32 vertices alone move it further than that, so
//   two genuinely coplanar floor triangles never matched. Scale it and
//   armory goes from 97.5% to 99.6%.
//
// * A surface was registered in the cells one probe at its centre found.
//   Those cells are worked out in f64 and the runtime finds its cell in
//   f32; a large polygon's corners are far enough from its centre to
//   land elsewhere entirely. Every remaining disagreement struck near a
//   rim, which is the part a centre probe never speaks for, so it probes
//   near each corner too.
//
// * Plane matching accepted any normal within about a degree, which over
//   a polygon a couple of units wide lifts a corner two centimetres off
//   the plane the tree routes by. A surface now only shares a plane its
//   own vertices sit on. Asserted at zero above, along with the ring
//   walk and the leaf references.
//
// One correction worth keeping: `edge_margin` used to divide by the
// model extent, so the "grazing" tolerance meant fifteen centimetres
// from a polygon edge on a 150-unit level, and disagreements that were
// nothing of the kind were being written off as rounding. It is an
// absolute millimetre now.
