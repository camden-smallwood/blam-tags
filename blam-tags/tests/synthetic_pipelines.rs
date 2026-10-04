//! The geometry pipelines, end to end, on inputs built in the test.
//!
//! Every other test of these importers and exporters is kit-gated: it
//! takes shipped tags or the kit's own source files as input, and it
//! skips without them. That leaves CI with nothing between a change to
//! `sbsp_import`, `ass`, `collision_verify` or `extract` and a release.
//!
//! Shipped content cannot be committed, so the inputs here are made from
//! code: one box, written as a JMS or an ASS scene, and the tag
//! definitions under `definitions/`. A box is small enough to know every
//! answer by hand — eight corners, six faces, twelve triangles, and a
//! bounding box chosen lopsided so a dropped offset or a swapped axis
//! moves a number this asserts on.
//!
//! What a box cannot show, and the kit-gated suites still own: welding
//! and splitting under real density, PRT, portals and clusters partitioned
//! by visibility, instancing of repeated props, and agreement with tool's
//! own output byte for byte.

use std::path::PathBuf;

use blam_tags::ass::{AssFile, AssInstance, AssMaterial, AssObject, AssObjectPayload, AssTriangle, AssVertex};
use blam_tags::math::{RealPoint2d, RealPoint3d, RealQuaternion, RealRgbColor, RealVector3d};
use blam_tags::{JmsFile, JmsMaterial, JmsNode, JmsTriangle, JmsVertex, TagFile};

// ------------------------------------------------------------------ inputs

/// The box, in source units (centimetres): deliberately not centred and
/// not a cube, so each axis has its own extent.
const LO: [f32; 3] = [-50.0, -100.0, 0.0];
const HI: [f32; 3] = [150.0, 100.0, 300.0];

/// The same box in world units — source divided by 100.
const WORLD_LO: [f32; 3] = [-0.5, -1.0, 0.0];
const WORLD_HI: [f32; 3] = [1.5, 1.0, 3.0];

fn schema(game: &str, group: &str) -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions")
        .join(game)
        .join(format!("{group}.json"));
    assert!(p.exists(), "{} is missing — is the definitions submodule checked out?", p.display());
    p
}

/// The box as six quads, each wound counter-clockwise seen from outside,
/// with its outward normal. Four vertices a face, so normals and UVs are
/// per face the way a modelling package exports a hard-edged box.
fn box_faces() -> Vec<([[f32; 3]; 4], [f32; 3])> {
    let (l, h) = (LO, HI);
    vec![
        // -X
        ([[l[0], l[1], l[2]], [l[0], l[1], h[2]], [l[0], h[1], h[2]], [l[0], h[1], l[2]]], [-1.0, 0.0, 0.0]),
        // +X
        ([[h[0], l[1], l[2]], [h[0], h[1], l[2]], [h[0], h[1], h[2]], [h[0], l[1], h[2]]], [1.0, 0.0, 0.0]),
        // -Y
        ([[l[0], l[1], l[2]], [h[0], l[1], l[2]], [h[0], l[1], h[2]], [l[0], l[1], h[2]]], [0.0, -1.0, 0.0]),
        // +Y
        ([[l[0], h[1], l[2]], [l[0], h[1], h[2]], [h[0], h[1], h[2]], [h[0], h[1], l[2]]], [0.0, 1.0, 0.0]),
        // -Z
        ([[l[0], l[1], l[2]], [l[0], h[1], l[2]], [h[0], h[1], l[2]], [h[0], l[1], l[2]]], [0.0, 0.0, -1.0]),
        // +Z
        ([[l[0], l[1], h[2]], [h[0], l[1], h[2]], [h[0], h[1], h[2]], [l[0], h[1], h[2]]], [0.0, 0.0, 1.0]),
    ]
}

/// Texcoords for a face's four corners. Deliberately not the unit
/// square: that is symmetric under `v -> 1 - v`, so a dropped V flip
/// would map every corner onto another corner and pass unseen.
const QUAD_UV: [[f32; 2]; 4] = [[0.125, 0.25], [0.625, 0.25], [0.625, 0.875], [0.125, 0.875]];

/// The box as a one-node, one-material JMS.
fn box_jms() -> JmsFile {
    let mut jms = JmsFile::default();
    jms.nodes.push(JmsNode {
        name: "frame".into(),
        parent: -1,
        rotation: RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 },
        translation: RealPoint3d { x: 0.0, y: 0.0, z: 0.0 },
    });
    jms.materials.push(JmsMaterial { name: "box_mat".into(), material_name: "(1) default".into() });
    for (corners, n) in box_faces() {
        let base = jms.vertices.len() as u32;
        for (c, uv) in corners.iter().zip(QUAD_UV) {
            jms.vertices.push(JmsVertex {
                position: RealPoint3d { x: c[0], y: c[1], z: c[2] },
                normal: RealVector3d { i: n[0], j: n[1], k: n[2] },
                tangent: None,
                binormal: None,
                node_sets: vec![(0, 1.0)],
                uvs: vec![RealPoint2d { x: uv[0], y: uv[1] }],
                color: None,
            });
        }
        jms.triangles.push(JmsTriangle { material: 0, v: [base, base + 1, base + 2], region: 0 });
        jms.triangles.push(JmsTriangle { material: 0, v: [base, base + 2, base + 3], region: 0 });
    }
    jms
}

/// The box as an ASS scene: one material, one MESH, the scene root and
/// one instance placing the mesh at the origin.
fn box_ass() -> AssFile {
    let mut vertices = Vec::new();
    let mut triangles = Vec::new();
    for (corners, n) in box_faces() {
        let base = vertices.len() as u32;
        for (c, uv) in corners.iter().zip(QUAD_UV) {
            vertices.push(AssVertex {
                position: RealPoint3d { x: c[0], y: c[1], z: c[2] },
                normal: RealVector3d { i: n[0], j: n[1], k: n[2] },
                color: RealRgbColor { red: 1.0, green: 1.0, blue: 1.0 },
                node_set: Vec::new(),
                uvs: vec![RealPoint3d { x: uv[0], y: uv[1], z: 0.0 }],
            });
        }
        triangles.push(AssTriangle { material: 0, v: [base, base + 1, base + 2] });
        triangles.push(AssTriangle { material: 0, v: [base, base + 2, base + 3] });
    }
    AssFile {
        header_tool: "synthetic".into(),
        header_tool_version: "1".into(),
        header_user: "test".into(),
        header_machine: "test".into(),
        materials: vec![AssMaterial {
            name: "box_mat".into(),
            lightmap_variant: String::new(),
            bm_strings: Vec::new(),
        }],
        objects: vec![AssObject {
            xref_filepath: String::new(),
            xref_objectname: String::new(),
            payload: AssObjectPayload::Mesh { vertices, triangles },
        }],
        instances: vec![
            AssInstance { object_index: -1, name: "Scene Root".into(), unique_id: -1, ..Default::default() },
            AssInstance { object_index: 0, name: "box".into(), unique_id: 0, parent_id: -1, ..Default::default() },
        ],
    }
}

// --------------------------------------------------------------- helpers

/// Every MESH vertex position in a scene, as placed in the object.
fn mesh_positions(ass: &AssFile) -> Vec<[f32; 3]> {
    let mut out = Vec::new();
    for o in &ass.objects {
        if let AssObjectPayload::Mesh { vertices, .. } = &o.payload {
            out.extend(vertices.iter().map(|v| [v.position.x, v.position.y, v.position.z]));
        }
    }
    out
}

fn bounds_of(points: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in points {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    (lo, hi)
}

fn assert_close3(got: [f32; 3], want: [f32; 3], tol: f32, what: &str) {
    for a in 0..3 {
        assert!((got[a] - want[a]).abs() <= tol, "{what}: {got:?} vs {want:?}");
    }
}

/// The set of box corners a list of positions touches, so a test can say
/// "all eight, nothing else" whatever order or duplication a pipeline
/// chose.
fn distinct_corners(points: &[[f32; 3]], scale: f32, tol: f32) -> Vec<[f32; 3]> {
    let mut out: Vec<[f32; 3]> = Vec::new();
    for p in points {
        let p = [p[0] * scale, p[1] * scale, p[2] * scale];
        if !out.iter().any(|q| (0..3).all(|a| (q[a] - p[a]).abs() <= tol)) {
            out.push(p);
        }
    }
    out
}

fn is_box_corner(p: [f32; 3], tol: f32) -> bool {
    (0..3).all(|a| (p[a] - LO[a]).abs() <= tol || (p[a] - HI[a]).abs() <= tol)
}

fn block_len(tag: &TagFile, path: &str) -> usize {
    tag.root().field_path(path).and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or_else(|| {
        panic!("no block at `{path}`")
    })
}

/// The scene written as ASS text and parsed back, the way an importer
/// receives it — so the writer and the reader are on the path too.
fn through_text(ass: &AssFile) -> AssFile {
    let mut text = Vec::new();
    ass.write(&mut text).expect("write ASS");
    let text = String::from_utf8(text).expect("ASS is text");
    let (parsed, version) = blam_tags::ass_parse::parse(&text).expect("our own ASS must parse");
    assert_eq!(version, 7, "the default writer emits Halo 3's version");
    parsed
}

fn sbsp_from(ass: &AssFile) -> (TagFile, blam_tags::sbsp_import::SbspReport) {
    use blam_tags::sbsp_import::{structure_bsp_from_ass_with, SbspOptions};
    structure_bsp_from_ass_with(
        &through_text(ass),
        &schema("halo3_mcc", "scenario_structure_bsp"),
        SbspOptions::default(),
    )
    .expect("a closed box is a valid structure")
}

/// A triangle's normal points away from the box's centre — the winding
/// survived, rather than coming back inside out.
fn faces_outward(tri: [[f32; 3]; 3], centre: [f32; 3]) -> bool {
    let e1 = [tri[1][0] - tri[0][0], tri[1][1] - tri[0][1], tri[1][2] - tri[0][2]];
    let e2 = [tri[2][0] - tri[0][0], tri[2][1] - tri[0][1], tri[2][2] - tri[0][2]];
    let n = [e1[1] * e2[2] - e1[2] * e2[1], e1[2] * e2[0] - e1[0] * e2[2], e1[0] * e2[1] - e1[1] * e2[0]];
    let c = [
        (tri[0][0] + tri[1][0] + tri[2][0]) / 3.0 - centre[0],
        (tri[0][1] + tri[1][1] + tri[2][1]) / 3.0 - centre[1],
        (tri[0][2] + tri[1][2] + tri[2][2]) / 3.0 - centre[2],
    ];
    n[0] * c[0] + n[1] * c[1] + n[2] * c[2] > 0.0
}

const CENTRE: [f32; 3] = [(LO[0] + HI[0]) / 2.0, (LO[1] + HI[1]) / 2.0, (LO[2] + HI[2]) / 2.0];

// ----------------------------------------------------------- sbsp_import

/// A box scene becomes one cluster whose geometry, bounds and material
/// are the box's — and the sealed world built from it is the box's six
/// faces, each a four-cornered ring on the box.
#[test]
fn an_ass_box_becomes_a_structure_bsp() {
    let (tag, report) = sbsp_from(&box_ass());

    assert_eq!(report.materials, 1);
    assert_eq!(report.clusters, 1);
    assert_eq!(report.vertices, 24, "four vertices a face, none welded away");
    assert_eq!(report.triangles, 12);
    assert_eq!(report.skipped_objects, 0);
    assert_eq!((report.instance_definitions, report.instance_placements), (0, 0),
        "a mesh placed once at identity is a cluster, not instanced geometry");
    assert!(report.collision_from_render, "no collision mesh was marked, so render seals the world");

    // The two coplanar triangles of each face merge into one quad.
    assert_eq!(report.collision_surfaces, 6, "{:?}", report.collision_rings);
    assert_eq!(report.collision_dropped, 0);
    assert!(report.worst_plane_offset < 1e-4, "{}", report.worst_plane_offset);
    let mut normals: Vec<[i32; 3]> = Vec::new();
    for ring in &report.collision_rings {
        assert_eq!(ring.len(), 4, "a box face is a quad: {ring:?}");
        for p in ring {
            let cm = [p[0] * 100.0, p[1] * 100.0, p[2] * 100.0];
            assert!(is_box_corner(cm, 1e-3), "collision vertex {p:?} is not a box corner");
        }
        // The axis this face is flat in, and which side of the box.
        let (lo, hi) = bounds_of(ring);
        let flat: Vec<usize> = (0..3).filter(|&a| (hi[a] - lo[a]).abs() < 1e-5).collect();
        assert_eq!(flat.len(), 1, "a face is flat in exactly one axis: {ring:?}");
        let a = flat[0];
        let mut n = [0; 3];
        n[a] = if (lo[a] - WORLD_LO[a]).abs() < 1e-5 { -1 } else { 1 };
        normals.push(n);
    }
    normals.sort();
    assert_eq!(
        normals,
        vec![[-1, 0, 0], [0, -1, 0], [0, 0, -1], [0, 0, 1], [0, 1, 0], [1, 0, 0]],
        "one ring on each of the six sides"
    );

    // What is written into the tag, read back off it.
    assert_eq!(block_len(&tag, "materials"), 1);
    assert_eq!(block_len(&tag, "clusters"), 1);
    assert_eq!(block_len(&tag, "render geometry/meshes"), 1);
    let root = tag.root();
    let cluster = root.field_path("clusters").unwrap().as_block().unwrap().element(0).unwrap();
    for (a, name) in ["bounds x", "bounds y", "bounds z"].iter().enumerate() {
        let b = cluster.read_real_bounds(name);
        assert!(
            (b.lower - WORLD_LO[a]).abs() < 1e-5 && (b.upper - WORLD_HI[a]).abs() < 1e-5,
            "cluster {name} is {:?}, the box spans {}..{}",
            (b.lower, b.upper),
            WORLD_LO[a],
            WORLD_HI[a]
        );
    }
    for (a, name) in ["world bounds x", "world bounds y", "world bounds z"].iter().enumerate() {
        let b = root.read_real_bounds(name);
        assert!(
            b.lower <= WORLD_LO[a] + 1e-5 && b.upper >= WORLD_HI[a] - 1e-5,
            "{name} {:?} does not contain the box",
            (b.lower, b.upper)
        );
    }
}

/// Export the structure back to a scene and the box comes back: the
/// same vertices in the same centimetres, the same twelve triangles on
/// the same material, still wound outward.
#[test]
fn a_structure_bsp_exports_its_box_back() {
    let (tag, _) = sbsp_from(&box_ass());
    // Through bytes too: what is exported is what a reader would load.
    let tag = TagFile::read_from_bytes(&tag.write_to_bytes().unwrap()).expect("reread sbsp");
    let back = AssFile::from_scenario_structure_bsp(&tag).expect("export");

    let render: Vec<&AssObject> = back
        .objects
        .iter()
        .filter(|o| match &o.payload {
            AssObjectPayload::Mesh { triangles, .. } => triangles
                .first()
                .is_some_and(|t| back.materials[t.material as usize].name == "box_mat"),
            _ => false,
        })
        .collect();
    assert_eq!(render.len(), 1, "one cluster mesh");
    let AssObjectPayload::Mesh { vertices, triangles } = &render[0].payload else { unreachable!() };
    assert_eq!(vertices.len(), 24);
    assert_eq!(triangles.len(), 12);

    let positions: Vec<[f32; 3]> =
        vertices.iter().map(|v| [v.position.x, v.position.y, v.position.z]).collect();
    let (lo, hi) = bounds_of(&positions);
    assert_close3(lo, LO, 1e-2, "exported lower bound (cm)");
    assert_close3(hi, HI, 1e-2, "exported upper bound (cm)");
    assert_eq!(distinct_corners(&positions, 1.0, 1e-2).len(), 8, "all eight corners, nothing else");
    for p in &positions {
        assert!(is_box_corner(*p, 1e-2), "{p:?} is not a corner of the box");
    }
    for t in triangles {
        let tri = t.v.map(|i| positions[i as usize]);
        assert!(faces_outward(tri, CENTRE), "triangle {:?} came back inside out", t.v);
        // And the check can tell: the same triangle reversed is inward.
        assert!(!faces_outward([tri[0], tri[2], tri[1]], CENTRE));
    }

    // V is flipped on the way in and must be flipped back on the way
    // out: every face's UVs are still the corners it was authored with.
    for v in vertices {
        let uv = v.uvs.first().expect("a texcoord");
        assert!(
            QUAD_UV.iter().any(|q| (q[0] - uv.x).abs() < 1e-3 && (q[1] - uv.y).abs() < 1e-3),
            "texcoord {:?} is not one the box was authored with",
            (uv.x, uv.y)
        );
    }

    // The sealed world comes back as its own collision-only mesh.
    let coll: Vec<&AssObject> = back
        .objects
        .iter()
        .filter(|o| match &o.payload {
            AssObjectPayload::Mesh { triangles, .. } => triangles
                .first()
                .is_some_and(|t| back.materials[t.material as usize].name == "@collision_only"),
            _ => false,
        })
        .collect();
    assert_eq!(coll.len(), 1, "one collision mesh");
    let AssObjectPayload::Mesh { vertices: cv, triangles: ct } = &coll[0].payload else { unreachable!() };
    assert_eq!(ct.len(), 12, "six quads, fanned to two triangles each");
    let cpos: Vec<[f32; 3]> = cv.iter().map(|v| [v.position.x, v.position.y, v.position.z]).collect();
    assert_eq!(distinct_corners(&cpos, 1.0, 1e-2).len(), 8);
    for t in ct {
        assert!(faces_outward(t.v.map(|i| cpos[i as usize]), CENTRE), "collision triangle inside out");
    }
}

/// A single placement that moves its object is baked into the cluster:
/// the tag has nowhere else to carry a cluster's transform, so if it is
/// dropped the structure lands at the origin.
#[test]
fn a_moved_placement_is_baked_into_the_cluster() {
    let mut ass = box_ass();
    ass.instances[1].local_translation = RealPoint3d { x: 1000.0, y: -500.0, z: 0.0 };
    let (tag, report) = sbsp_from(&ass);
    assert_eq!(report.clusters, 1);
    assert_eq!(report.instance_placements, 0, "the only object stays a cluster");
    let cluster = tag.root().field_path("clusters").unwrap().as_block().unwrap().element(0).unwrap();
    let shift = [10.0, -5.0, 0.0];
    for (a, name) in ["bounds x", "bounds y", "bounds z"].iter().enumerate() {
        let b = cluster.read_real_bounds(name);
        assert!(
            (b.lower - (WORLD_LO[a] + shift[a])).abs() < 1e-4
                && (b.upper - (WORLD_HI[a] + shift[a])).abs() < 1e-4,
            "cluster {name} {:?} was not moved by {}",
            (b.lower, b.upper),
            shift[a]
        );
    }
}

/// An object placed twice is instanced geometry: one definition, two
/// placements, and the placements carry their translations back out.
#[test]
fn a_twice_placed_object_is_instanced_geometry() {
    let mut ass = box_ass();
    // A second, smaller object: a copy of the box scaled down, placed
    // twice beside the first.
    let mut prop = ass.objects[0].clone();
    if let AssObjectPayload::Mesh { vertices, .. } = &mut prop.payload {
        for v in vertices {
            v.position.x *= 0.1;
            v.position.y *= 0.1;
            v.position.z *= 0.1;
        }
    }
    ass.objects.push(prop);
    for (k, x) in [(2, 500.0f32), (3, 900.0)] {
        ass.instances.push(AssInstance {
            object_index: 1,
            name: format!("prop{k}"),
            unique_id: k,
            local_translation: RealPoint3d { x, y: 0.0, z: 0.0 },
            ..Default::default()
        });
    }
    let (tag, report) = sbsp_from(&ass);
    assert_eq!(report.clusters, 1, "the box stays the structure");
    assert_eq!(report.instance_definitions, 1);
    assert_eq!(report.instance_placements, 2);
    assert_eq!(block_len(&tag, "instanced geometry instances"), 2);

    let back = AssFile::from_scenario_structure_bsp(&tag).expect("export");
    let mut xs: Vec<f32> = back
        .instances
        .iter()
        .filter(|i| {
            i.object_index >= 0
                && back.objects[i.object_index as usize].vertices_len() == 24
                && i.local_translation.x.abs() > 1.0
        })
        .map(|i| i.local_translation.x)
        .collect();
    xs.sort_by(f32::total_cmp);
    assert_eq!(xs.len(), 2, "both placements come back: {:?}", back.instances.len());
    assert!((xs[0] - 500.0).abs() < 1e-2 && (xs[1] - 900.0).abs() < 1e-2, "placements at {xs:?}");
}

/// A scene with nothing to render is refused rather than written empty.
#[test]
fn a_scene_without_render_geometry_is_refused() {
    use blam_tags::sbsp_import::{structure_bsp_from_ass, SbspError};
    let mut ass = box_ass();
    ass.objects[0] = AssObject::empty_mesh();
    let err = structure_bsp_from_ass(&ass, &schema("halo3_mcc", "scenario_structure_bsp")).unwrap_err();
    assert_eq!(err, SbspError::Empty);
}

/// Each edge of a collision BSP as `(left surface, right surface)`.
fn edge_sides(bsp: &blam_tags::TagStruct<'_>) -> Vec<(i16, i16)> {
    bsp.field("edges")
        .and_then(|f| f.as_block())
        .expect("edges")
        .iter()
        .map(|e| {
            (
                e.read_int_any("left surface").unwrap() as i16,
                e.read_int_any("right surface").unwrap() as i16,
            )
        })
        .collect()
}

fn sealed_world(tag: &TagFile) -> blam_tags::TagStruct<'_> {
    tag.root()
        .field_path("resource interface/raw_resources[0]/raw_items/collision bsp")
        .and_then(|f| f.as_block())
        .and_then(|b| b.element(0))
        .expect("a structure collision BSP")
}

/// The sealed world's tree agrees with its surfaces: every ray the
/// verifier casts finds the same face through the tree as by testing the
/// polygons directly.
#[test]
fn the_structure_collision_of_a_box_verifies_clean() {
    let (tag, _) = sbsp_from(&box_ass());
    let report = blam_tags::collision_verify::test_collision_bsp(&sealed_world(&tag), 64)
        .expect("a box's collision can be tested");
    assert_eq!(report.bsps, 1);
    assert_eq!(report.surfaces, 6);
    // Six face planes, plus the splitting planes the tree adds where a
    // face plane will not separate a cell.
    assert!(report.planes >= 6, "{} planes", report.planes);
    assert_eq!(report.open_rings, 0);
    assert!(report.rays >= 6, "one ray a surface at least, got {}", report.rays);
    assert!(report.clean(), "{report:#?}");
    assert_eq!(report.agreed, report.rays);
}

/// The sealed world built from render geometry shares no edges.
///
/// Render vertices are split wherever a normal or texcoord changes —
/// every edge of a hard-edged box — and the structure collision is built
/// from those positions without welding them. So each face becomes its
/// own island: 24 edges, every one with a surface on one side only,
/// where the box has 12 edges with a face on each side.
///
/// Shipped Halo 3 structure BSPs do not look like that. Measured on the
/// local H3 kit with a throwaway reader: `levels/test/box` has 1,115
/// collision edges and `levels/multi/riverworld` 39,962, and in both
/// every edge has a surface on each side — zero one-sided. The
/// `collision_model` importer welds before it builds (see
/// `a_jms_box_becomes_a_collision_model_with_shared_edges`); the
/// structure path does not.
#[test]
#[ignore = "bug: sbsp_import builds the structure collision from unwelded render vertices, so no edge is shared"]
fn the_structure_collision_of_a_box_shares_its_edges() {
    let (tag, _) = sbsp_from(&box_ass());
    let sides = edge_sides(&sealed_world(&tag));
    let one_sided = sides.iter().filter(|(l, r)| *l < 0 || *r < 0).count();
    assert_eq!(sides.len(), 12, "a box has twelve edges");
    assert_eq!(one_sided, 0, "every box edge has a face on each side");
}

// ------------------------------------------------- render + collision JMS

fn render_from_box() -> (TagFile, blam_tags::render_import::RenderReport) {
    use blam_tags::render_import::{render_model_from_jms, RenderOptions};
    // No PRT: the solve is its own suite, and the box needs none of it.
    let opts = RenderOptions { prt_samples: None, ..Default::default() };
    render_model_from_jms(&box_jms(), &schema("halo3_mcc", "render_model"), &opts)
        .expect("a box is a valid render model")
}

fn collision_from_box() -> (TagFile, blam_tags::collision_import::CollisionReport) {
    use blam_tags::collision_import::{collision_model_from_jms, CollisionOptions};
    collision_model_from_jms(&box_jms(), &schema("halo3_mcc", "collision_model"), &CollisionOptions::default())
        .expect("a box is a valid collision model")
}

/// A render model from a box JMS exports back to a scene with the box
/// in it — through [`AssFile::from_render_model`] — at the right size,
/// the right way out, on the right material.
#[test]
fn a_jms_box_render_model_exports_to_ass() {
    let (tag, report) = render_from_box();
    assert_eq!(report.triangles, 12);
    assert_eq!(report.source_vertices, 24);
    assert_eq!(report.welded_vertices, 24, "per-face normals keep the faces' vertices apart");
    assert_eq!((report.regions, report.permutations, report.meshes), (1, 1, 1));
    let tag = TagFile::read_from_bytes(&tag.write_to_bytes().unwrap()).expect("reread render_model");

    let ass = AssFile::from_render_model(&tag).expect("export");
    assert_eq!(ass.materials.len(), 1, "{:?}", ass.materials.iter().map(|m| &m.name).collect::<Vec<_>>());
    // The JMS material name does not survive: the importer leaves the
    // shader reference null (a JMS names a material, not a tag path), so
    // there is nothing to name it by on the way out.
    let tris: usize = ass.objects.iter().map(|o| o.triangles_len()).sum();
    assert_eq!(tris, 12, "strips decoded back to the twelve triangles");
    let positions = mesh_positions(&ass);
    let (lo, hi) = bounds_of(&positions);
    assert_close3(lo, LO, 0.05, "lower bound (cm)");
    assert_close3(hi, HI, 0.05, "upper bound (cm)");
    assert_eq!(distinct_corners(&positions, 1.0, 0.05).len(), 8);
    for o in &ass.objects {
        let AssObjectPayload::Mesh { vertices, triangles } = &o.payload else { continue };
        let p: Vec<[f32; 3]> = vertices.iter().map(|v| [v.position.x, v.position.y, v.position.z]).collect();
        for t in triangles {
            assert!(faces_outward(t.v.map(|i| p[i as usize]), CENTRE), "triangle {:?} inside out", t.v);
        }
    }
}

/// The JMS a render model exports is the JMS it was built from: same
/// triangles, same corners, same node.
#[test]
fn a_jms_box_render_model_exports_its_jms_back() {
    let (tag, _) = render_from_box();
    let back = JmsFile::from_render_model(&tag).expect("export");
    assert_eq!(back.nodes.len(), 1);
    assert_eq!(back.nodes[0].name, "frame");
    assert_eq!(back.triangles.len(), 12);
    let positions: Vec<[f32; 3]> =
        back.vertices.iter().map(|v| [v.position.x, v.position.y, v.position.z]).collect();
    for p in &positions {
        assert!(is_box_corner(*p, 0.05), "{p:?} is not a box corner");
    }
    assert_eq!(distinct_corners(&positions, 1.0, 0.05).len(), 8);
    for t in &back.triangles {
        assert!(faces_outward(t.v.map(|i| positions[i as usize]), CENTRE), "triangle {:?} inside out", t.v);
    }
    // Through text, as a file on disk would be.
    let mut buf = Vec::new();
    back.write(&mut buf, 8213).unwrap();
    let (again, version) = JmsFile::parse(std::str::from_utf8(&buf).unwrap()).expect("parse our own JMS");
    assert_eq!(version, 8213);
    assert_eq!((again.vertices.len(), again.triangles.len()), (back.vertices.len(), 12));
}

/// The collision importer welds before it builds, so the box comes out
/// as a closed solid: six quads, twelve edges, each with a face on both
/// sides — and a tree the verifier agrees with on every ray.
#[test]
fn a_jms_box_becomes_a_collision_model_with_shared_edges() {
    let (tag, report) = collision_from_box();
    assert_eq!((report.regions, report.permutations, report.bsps), (1, 1, 1));
    assert_eq!(report.vertices, 8, "welded down to the box's corners");
    assert_eq!(report.surfaces, 6, "coplanar triangles merged into quads");
    assert_eq!(report.edges, 12);
    assert_eq!(report.dropped_surfaces, 0);

    let tag = TagFile::read_from_bytes(&tag.write_to_bytes().unwrap()).expect("reread collision_model");
    let root = tag.root();
    let bsp = root
        .field_path("regions[0]/permutations[0]/bsps[0]/bsp")
        .and_then(|f| f.as_struct())
        .expect("the box's bsp");
    let sides = edge_sides(&bsp);
    assert_eq!(sides.len(), 12);
    assert!(sides.iter().all(|(l, r)| *l >= 0 && *r >= 0), "an open edge on a closed box: {sides:?}");

    let verify = blam_tags::collision_verify::test_collision_model(&tag, 64).expect("testable");
    assert_eq!(verify.bsps, 1);
    assert_eq!(verify.surfaces, 6);
    assert_eq!(verify.open_rings, 0);
    assert!(verify.leaf_refs > 0);
    assert!(verify.clean(), "{verify:#?}");
    assert_eq!(verify.agreed, verify.rays);
    assert_eq!(verify.unreachable_surfaces, 0);

    let rings = blam_tags::collision_verify::decoded_rings(&bsp).expect("rings");
    assert_eq!(rings.len(), 6);
    for ring in &rings {
        assert_eq!(ring.len(), 4);
        for p in ring {
            assert!(is_box_corner([p[0] * 100.0, p[1] * 100.0, p[2] * 100.0], 1e-2), "{p:?}");
        }
    }
    let (worst, _, _) = blam_tags::collision_verify::ring_planarity(&bsp).expect("planarity");
    assert!(worst < 1e-4, "ring off its plane by {worst}");
    let (dangling, _, leaves) = blam_tags::collision_verify::dangling_leaf_refs(&bsp).expect("leaf refs");
    assert_eq!(dangling, 0);
    assert!(leaves > 0);
}

/// Replace an integer field with `v`, keeping its width.
fn set_int(el: &mut blam_tags::TagStructMut<'_>, name: &str, v: i64) {
    use blam_tags::TagFieldData as D;
    let mut f = el.field_mut(name).unwrap_or_else(|| panic!("no field {name}"));
    let new = match f.as_ref().value() {
        Some(D::CharInteger(_)) => D::CharInteger(v as i8),
        Some(D::ShortInteger(_)) => D::ShortInteger(v as i16),
        Some(D::LongInteger(_)) => D::LongInteger(v as i32),
        Some(D::Int64Integer(_)) => D::Int64Integer(v),
        other => panic!("{name} is not an integer: {other:?}"),
    };
    f.set(new).unwrap();
}

/// A tree whose leaves index nothing is caught: the surfaces are still
/// there to hit, the tree no longer finds them, and the verifier says
/// so instead of agreeing.
#[test]
fn the_verifier_catches_leaves_that_index_nothing() {
    let (mut tag, _) = collision_from_box();
    {
        let mut root = tag.root_mut();
        let mut f = root.field_path_mut("regions[0]/permutations[0]/bsps[0]/bsp/leaves").expect("leaves");
        let mut leaves = f.as_block_mut().expect("a block");
        assert!(leaves.len() > 0);
        for i in 0..leaves.len() {
            set_int(&mut leaves.element_mut(i).unwrap(), "bsp2d reference count", 0);
        }
    }
    let verify = blam_tags::collision_verify::test_collision_model(&tag, 64).expect("testable");
    assert!(!verify.clean(), "the verifier agreed with a tree that finds nothing: {verify:#?}");
    assert_eq!(verify.leaf_refs, 0);
    // Rays that miss the box agree trivially — nothing either way. Every
    // ray that strikes it is now a miss through the tree.
    assert!(verify.missed > 0, "{verify:#?}");
    assert_eq!(verify.agreed + verify.missed, verify.rays, "{verify:#?}");
    assert_eq!(verify.unreachable_surfaces, 6, "every face is now unreachable");
}

/// A tree that reaches no leaf at all is refused as untestable — the
/// shape `bird_quadwing` ships in — rather than counted as misses.
#[test]
fn the_verifier_refuses_a_tree_that_reaches_no_leaf() {
    use blam_tags::collision_verify::VerifyError;
    let (mut tag, _) = collision_from_box();
    {
        let mut root = tag.root_mut();
        let mut f =
            root.field_path_mut("regions[0]/permutations[0]/bsps[0]/bsp/bsp3d nodes").expect("nodes");
        let mut nodes = f.as_block_mut().expect("a block");
        for i in 0..nodes.len() {
            set_int(&mut nodes.element_mut(i).unwrap(), "node data designator", 0);
        }
    }
    match blam_tags::collision_verify::test_collision_model(&tag, 8) {
        Err(VerifyError::NoUsableTree { surfaces, .. }) => assert_eq!(surfaces, 6),
        other => panic!("expected NoUsableTree, got {other:?}"),
    }
}

/// A collision model with no BSPs has nothing to test.
#[test]
fn the_verifier_reports_an_empty_model_as_empty() {
    let tag = TagFile::new(schema("halo3_mcc", "collision_model")).unwrap();
    assert_eq!(
        blam_tags::collision_verify::test_collision_model(&tag, 8).unwrap_err(),
        blam_tags::collision_verify::VerifyError::Empty
    );
}
