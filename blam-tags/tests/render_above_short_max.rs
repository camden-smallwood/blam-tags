//! A mesh with more vertices than a signed word can count.
//!
//! The per-mesh ceiling is 65,535 — `raw_vertex_block`'s `max_count` is
//! `UNSIGNED_SHORT_MAX`. The importer used to stop at 32,767, which is
//! `SHORT_MAX`, and that cap belongs to `subpart_block` rather than to
//! vertices: it refused half of what the format holds.
//!
//! Raising a constant is easy and proves nothing, because past 32,767
//! every index written is a negative `short` and the question is whether
//! anything reads one back as signed. That has already cost this crate
//! twice — a surface's `first edge` and a part's `index count` both went
//! negative and were silently read as "nothing here". So this builds a
//! model over the old line, writes it, reads it back, and checks the
//! geometry survived.

use blam_tags::jms::{JmsFile, JmsMaterial, JmsNode, JmsTriangle, JmsVertex};
use blam_tags::math::{RealPoint2d, RealPoint3d, RealVector3d};
use blam_tags::render_import::{render_model_from_jms, RenderOptions};
use blam_tags::TagFile;
use std::path::PathBuf;

fn schema() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/render_model.json");
    p.exists().then_some(p)
}

/// A strip of quads, `quads` wide, as one region and one material.
///
/// Every vertex is distinct — no two share a position — so the welder
/// cannot quietly pull the count back under the old limit and make the
/// test pass for the wrong reason.
fn ribbon(quads: usize) -> JmsFile {
    let mut jms = JmsFile {
        nodes: vec![JmsNode {
            name: "frame".into(),
            parent: -1,
            rotation: Default::default(),
            translation: RealPoint3d { x: 0.0, y: 0.0, z: 0.0 },
        }],
        materials: vec![JmsMaterial {
            name: "surface".into(),
            material_name: "(1) default default".into(),
        }],
        ..Default::default()
    };

    for i in 0..=quads {
        let x = i as f32 * 0.37;
        for k in 0..2 {
            jms.vertices.push(JmsVertex {
                position: RealPoint3d { x, y: k as f32 * 1.9, z: (i % 7) as f32 * 0.11 },
                normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
                tangent: None,
                binormal: None,
                node_sets: vec![(0, 1.0)],
                uvs: vec![RealPoint2d { x: i as f32 * 0.01, y: k as f32 }],
                color: None,
            });
        }
    }
    for i in 0..quads {
        let a = (i * 2) as u32;
        jms.triangles.push(JmsTriangle { region: 0, material: 0, v: [a, a + 1, a + 2] });
        jms.triangles.push(JmsTriangle { region: 0, material: 0, v: [a + 1, a + 3, a + 2] });
    }
    jms
}

#[test]
fn a_mesh_may_hold_more_vertices_than_a_signed_word_counts() {
    let Some(schema) = schema() else {
        eprintln!("skipping: need definitions/halo3_mcc/render_model.json");
        return;
    };

    // 20,000 quads is 40,002 vertices — comfortably past 32,767 and
    // inside the format's 65,535.
    let jms = ribbon(20_000);
    assert!(jms.vertices.len() > 32_767, "the model has to cross the old line");

    let (tag, report) = match render_model_from_jms(&jms, &schema, &RenderOptions::default()) {
        Ok(v) => v,
        Err(e) => panic!("{} vertices refused: {e}", jms.vertices.len()),
    };
    eprintln!(
        "{} source vertices -> {} meshes, largest {} vertices",
        jms.vertices.len(),
        report.meshes,
        report.largest_mesh_vertices
    );
    assert!(
        report.largest_mesh_vertices > 32_767,
        "the geometry was split below the old cap, so this proves nothing: largest mesh had {}",
        report.largest_mesh_vertices
    );

    // Read it back and count what the index buffer actually addresses.
    let bytes = tag.write_to_bytes().expect("serialise");
    let back = TagFile::read_from_bytes(&bytes).expect("parse back");
    let root = back.root();

    let verts = root
        .field_path("render geometry/per mesh temporary[0]/raw vertices")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .unwrap_or(0);
    assert_eq!(verts, report.largest_mesh_vertices, "the vertices did not survive the trip");

    // Every part has to name a run the reader can use. A negative count
    // is the failure this is looking for: it reads as an empty range and
    // the geometry vanishes with no error anywhere.
    let parts = root
        .field_path("render geometry/meshes[0]/parts")
        .and_then(|f| f.as_block())
        .expect("parts");
    assert!(parts.len() > 0, "no parts");
    let mut total = 0usize;
    let mut highest = 0usize;
    for i in 0..parts.len() {
        let p = parts.element(i).expect("part");
        let start = p.read_int_any("index start").unwrap_or(-1);
        let count = p.read_int_any("index count").unwrap_or(-1);
        assert!(count > 0, "part {i} names {count} indices, which reads as an empty range");
        total += count as usize;
        highest = highest.max((start + count) as usize);
    }
    eprintln!("  {} parts covering {total} indices, reaching {highest}", parts.len());

    // And the indices themselves: past 32,767 they are stored as
    // negative shorts, so a reader taking them signed sees nonsense.
    let words = root
        .field_path("render geometry/per mesh temporary[0]/raw indices")
        .and_then(|f| f.as_block())
        .expect("index buffer");
    let mut seen_above = 0usize;
    for i in 0..words.len() {
        let raw = words.element(i).and_then(|e| e.read_int_any("word")).unwrap_or(0);
        let idx = raw as i64 as u16 as usize;
        assert!(idx < verts, "index {i} addresses vertex {idx} of {verts}");
        if idx > 32_767 {
            seen_above += 1;
        }
    }
    eprintln!("  {} of {} indices address a vertex past 32,767", seen_above, words.len());
    assert!(
        seen_above > 0,
        "no index reached past 32,767, so nothing here was actually exercised"
    );
}
