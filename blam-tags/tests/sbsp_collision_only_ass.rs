//! Collision-only structure BSPs export their instanced collision.
//!
//! Halo: Campaign Evolved is a Blam/Unreal hybrid — the Blam tag owns
//! collision and placement, Unreal owns everything rendered — so its
//! `scenario_structure_bsp` tags carry no render geometry at all:
//! `render geometry/per mesh temporary` is empty and every mesh reports
//! `index buffer index = -1`. Before the collision fallback, exporting
//! one produced a near-empty ASS (on `c10/level_a`: 1 object, 198
//! vertices, out of 775 instanced definitions holding 206,652 collision
//! surfaces placed 3,492 times).
//!
//! The fallback is gated on the whole `per mesh temporary` block being
//! empty rather than on individual definitions, because H3/ODST/Reach/H4
//! also have definitions whose render mesh is missing (51 of 428 on
//! Reach's `cex_beaver_creek`) and emitting collision for just those
//! would change their long-standing output. Both halves of that are
//! asserted here.
//!
//! CE ships its tags inside UE5 paks, so there is no loose corpus to
//! point at by convention and nothing small enough to commit (the
//! smallest shipped CE BSP is ~9.9 MB). Point `BLAM_TEST_CE_SBSP` at an
//! extracted `.scenario_structure_bsp` to run the first test; the Reach
//! half self-locates a kit and both skip gracefully when absent.

use std::path::PathBuf;

use blam_tags::{AssFile, AssObjectPayload, TagFile};

/// An extracted Campaign Evolved BSP, via `BLAM_TEST_CE_SBSP`.
fn ce_sbsp() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var("BLAM_TEST_CE_SBSP").ok()?);
    p.is_file().then_some(p)
}

/// A Reach BSP from an HREK install, if one is on this machine.
fn reach_sbsp() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BLAM_TEST_REACH_SBSP") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let roots = [
        "C:/Program Files (x86)/Steam/steamapps/common/HREK",
        "D:/SteamLibrary/steamapps/common/HREK",
    ];
    let rel = "tags/levels/dlc/cex_beaver_creek/cex_beaver_creek.scenario_structure_bsp";
    roots
        .iter()
        .map(|r| PathBuf::from(r).join(rel))
        .find(|p| p.is_file())
}

fn has_collision_material(ass: &AssFile) -> bool {
    ass.materials.iter().any(|m| m.name == "@collision_only")
}

/// A BSP with no render geometry sources its definition meshes from
/// collision, so the export carries the level rather than just the
/// structure collision hull.
#[test]
fn campaign_evolved_bsp_exports_instanced_collision() {
    let Some(path) = ce_sbsp() else {
        eprintln!("skipping: set BLAM_TEST_CE_SBSP to an extracted Campaign Evolved .scenario_structure_bsp");
        return;
    };
    let tag = TagFile::read(&path).expect("read CE sbsp");

    // Precondition: this really is a collision-only BSP.
    let pmt_len = tag
        .root()
        .field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .expect("render geometry/per mesh temporary");
    assert_eq!(
        pmt_len, 0,
        "{} has render geometry — not the collision-only shape this test covers",
        path.display()
    );
    let defs = tag
        .root()
        .field_path("resource interface/raw_resources[0]/raw_items/instanced geometries definitions")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .unwrap_or(0);
    assert!(defs > 0, "no instanced geometry definitions to recover");

    let ass = AssFile::from_scenario_structure_bsp(&tag).expect("build ASS");

    // One object per definition that had collision, plus the structure
    // collision BSP — versus exactly 1 (the structure BSP) before.
    assert!(
        ass.objects.len() > 1,
        "collision-only BSP produced {} object(s) — the definition fallback did not run",
        ass.objects.len()
    );
    let total_verts: usize = ass.objects.iter().map(|o| o.vertices_len()).sum();
    assert!(
        total_verts > 1000,
        "only {total_verts} vertices recovered from {defs} definitions"
    );
    // Every placement should reach a real object, so instances outnumber
    // objects on any level that reuses its geometry.
    assert!(
        ass.instances.len() > ass.objects.len(),
        "{} instances for {} objects — placements are not being emitted",
        ass.instances.len(),
        ass.objects.len()
    );
    assert!(
        has_collision_material(&ass),
        "collision-sourced meshes must carry the @collision_only marker material"
    );
}

/// The fallback must not engage on an engine that does ship render
/// geometry, even though such BSPs also contain definitions with no
/// usable render mesh.
#[test]
fn reach_bsp_still_exports_render_geometry() {
    let Some(path) = reach_sbsp() else {
        eprintln!("skipping: no HREK install found (set BLAM_TEST_REACH_SBSP to override)");
        return;
    };
    let tag = TagFile::read(&path).expect("read Reach sbsp");

    let pmt_len = tag
        .root()
        .field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .expect("render geometry/per mesh temporary");
    assert!(pmt_len > 0, "expected Reach to carry inline render buffers");

    let ass = AssFile::from_scenario_structure_bsp(&tag).expect("build ASS");

    // Render-sourced meshes carry real per-vertex data; a collision
    // fallback here would show up as objects whose triangles are all
    // on the @collision_only material.
    let coll_idx = ass
        .materials
        .iter()
        .position(|m| m.name == "@collision_only")
        .map(|i| i as i32);
    let render_tris: usize = ass
        .objects
        .iter()
        .map(|o| match &o.payload {
            AssObjectPayload::Mesh { triangles, .. } => triangles
                .iter()
                .filter(|t| Some(t.material) != coll_idx)
                .count(),
            _ => 0,
        })
        .sum();
    assert!(
        render_tris > 1000,
        "expected render-sourced triangles on a Reach BSP, found {render_tris}"
    );
}
