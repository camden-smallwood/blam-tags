//! Every reader of a render model's vertices decompresses them by the mesh's
//! vertex type, as the engine does, not by the `compression flags` word the
//! engine never reads. A rigid mesh whose flags are 0 (zanzibar `dome_light`
//! ships this way) holds positions normalized to 0..1 all the same.
//!
//! The tag is built from a synthetic JMS through the render-model importer
//! against the Halo 3 definitions, so no shipped tag is involved.

use blam_tags::ass::{AssFile, AssObjectPayload};
use blam_tags::jms::{JmsFile, JmsMaterial, JmsNode, JmsTriangle, JmsVertex};
use blam_tags::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealVector3d};
use blam_tags::render_model::RenderModel;
use blam_tags::{TagFieldData, TagFile};

const SCHEMA: &str = "../definitions/halo3_mcc/render_model.json";

fn vertex(x: f32, y: f32, z: f32) -> JmsVertex {
    JmsVertex {
        position: RealPoint3d { x, y, z },
        normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
        tangent: None,
        binormal: None,
        node_sets: vec![(0, 1.0)],
        uvs: vec![RealPoint2d { x: 0.25, y: 0.75 }],
        color: None,
    }
}

/// One rigid triangle spanning x = 500..700 JMS units (5..7 world units).
fn rigid_triangle() -> TagFile {
    let jms = JmsFile {
        nodes: vec![JmsNode {
            name: "root".into(),
            parent: -1,
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d { x: 0.0, y: 0.0, z: 0.0 },
        }],
        materials: vec![JmsMaterial { name: "mat".into(), material_name: "default default".into() }],
        vertices: vec![vertex(500.0, 0.0, 0.0), vertex(700.0, 0.0, 0.0), vertex(500.0, 200.0, 50.0)],
        triangles: vec![JmsTriangle { material: 0, v: [0, 1, 2], region: 0 }],
        ..Default::default()
    };
    let (tag, _) = blam_tags::render_import::render_model_from_jms(&jms, SCHEMA.as_ref(), &Default::default())
        .expect("render import");
    let vertex_type = tag.root().field_path("render geometry/meshes[0]/vertex type").and_then(|f| f.value());
    assert!(matches!(vertex_type, Some(TagFieldData::CharEnum { value: 1, .. })), "{vertex_type:?}");
    tag
}

fn clear_compression_flags(tag: &mut TagFile) {
    tag.root_mut()
        .field_path_mut("render geometry/compression info[0]/compression flags")
        .expect("compression flags")
        .set(TagFieldData::WordFlags { value: 0, names: Vec::new() })
        .expect("word flags");
}

fn max(values: impl Iterator<Item = f32>) -> f32 {
    values.fold(f32::MIN, f32::max)
}

/// The largest x of the preview, the JMS export and the ASS export, the
/// exports divided back to world units.
fn extents(tag: &TagFile) -> (f32, f32, f32) {
    let preview = RenderModel::derive_render_meshes(tag).expect("preview");
    let jms = JmsFile::from_render_model(tag).expect("jms");
    let ass = AssFile::from_render_model(tag).expect("ass");
    let ass_x = ass
        .objects
        .iter()
        .filter_map(|object| match &object.payload {
            AssObjectPayload::Mesh { vertices, .. } => Some(max(vertices.iter().map(|v| v.position.x))),
            _ => None,
        })
        .fold(f32::MIN, f32::max);
    (
        max(preview[0].vertices.iter().map(|v| v.position.x)),
        max(jms.vertices.iter().map(|v| v.position.x)) / 100.0,
        ass_x / 100.0,
    )
}

/// With the flags set (as the importer writes them) every reader agrees.
#[test]
fn compressed_flags_set_every_reader_agrees() {
    let (preview, jms, ass) = extents(&rigid_triangle());
    let agree = [preview, jms, ass].iter().all(|x| (x - 7.0).abs() < 0.01);
    assert!(agree, "max x: preview {preview}, jms {jms}, ass {ass}; expected 7");
}

/// With the flags cleared, the preview already followed the vertex type; the
/// JMS and ASS exports read the raw 0..1 positions (1.0 instead of 7.0).
#[test]
fn compressed_flags_clear_every_reader_still_agrees() {
    let mut tag = rigid_triangle();
    clear_compression_flags(&mut tag);
    let (preview, jms, ass) = extents(&tag);
    let agree = [preview, jms, ass].iter().all(|x| (x - 7.0).abs() < 0.01);
    assert!(agree, "max x: preview {preview}, jms {jms}, ass {ass}; expected 7");
}
