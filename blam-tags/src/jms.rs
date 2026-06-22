//! JMS (Bungie Joint Model Skeleton) export from `render_model` tags.
//!
//! Reconstructs a JMS-format static-geometry asset from a parsed
//! `render_model`. Targets the H3 / Reach MCC source-style tag
//! pipeline where every render mesh stores its vertex/index buffers
//! inline under `render geometry/per mesh temporary[i]` (no `tgrc`
//! resource indirection). Cache-built map files would need a
//! different code path — see `reference_tagtool_jms_extraction.md`
//! for the contrast.
//!
//! Two-stage decompression on positions and texcoords: raw values
//! live in `[0,1]` quantized space and must be linear-decompressed
//! against `render geometry/compression info[0]` *before* the
//! world-units → JMS-cm ×100 scale. The 6 position-bounds floats are
//! packed across two `real_point_3d` fields as the sequential tuple
//! `[xmin, xmax, ymin, ymax, zmin, zmax]` (NOT min/max corners).
//!
//! Materials are walked region × permutation × mesh.parts, mirroring
//! the H3 Blender exporter (`build_asset.py:write_materials_8205`):
//! one entry per `(shader, "{perm} {region}")` cell, with
//! `material_name` formatted as `(<slot>) <perm> <region>`. The
//! `slot` value is a deterministic 1-based counter; the artist's
//! original `(N)` is `bpy.data.materials.find()` from their Blender
//! scene and unrecoverable from the tag, but it's round-trip
//! metadata only and the H3 importer's parser doesn't act on it.
//!
//! Markers flatten `marker_groups[i].markers[j]` keeping every
//! variant — same shape TagTool emits.
//!
//! Triangle strips are split on the `0xFFFF` restart sentinel and
//! converted per-segment with parity-correct winding plus
//! degenerate-triangle filtering (any window with two equal indices
//! is dropped). Transparent parts (`part_type = 4`) typically
//! contain double-sided geometry baked in by the importer (each
//! triangle once per winding); JMS export keeps both copies, same
//! as TagTool — dedupe is the caller's choice.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::geometry::{
    read_compression_bounds, strip_to_list, strip_to_list_u32, walk_surface_ring,
    CompressionBounds, EdgeRow, SCALE,
};
use crate::math::{RealPoint3d, RealQuaternion, RealVector3d};

/// JMS export errors. Most failures during a corpus sweep land in
/// `MissingField` (schema-shape variation) or `Io` (write-out).
#[derive(Debug)]
pub enum JmsError {
    /// A required field couldn't be located on the parsed tag —
    /// either the schema doesn't have it or the tag instance left it
    /// empty. Carries the dotted field path for diagnosis.
    MissingField(&'static str),
    /// Io error from the JMS writer.
    Io(io::Error),
}

impl std::fmt::Display for JmsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(p) => write!(f, "render_model is missing required field: {p}"),
            Self::Io(e) => write!(f, "JMS write failed: {e}"),
        }
    }
}

impl std::error::Error for JmsError {}

impl From<io::Error> for JmsError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}

/// JMS skeletal node (bone). `parent` is `-1` for roots.
#[derive(Debug, Clone)]
pub struct JmsNode {
    pub name: String,
    pub parent: i16,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
}

/// JMS material entry. `name` is the shader basename (with attribute
/// symbols where applicable); `material_name` is the
/// `(slot) [lod] perm region` cell label.
#[derive(Debug, Clone)]
pub struct JmsMaterial {
    pub name: String,
    pub material_name: String,
}

/// JMS marker (one per marker_group variant). `radius = -1.0`
/// matches the embedded-source convention for "unset radius".
#[derive(Debug, Clone)]
pub struct JmsMarker {
    pub name: String,
    pub node_index: i16,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub radius: f32,
}

/// JMS vertex entry. JMS doesn't share vertices across triangles —
/// each triangle owns a fresh 3-tuple of vertex entries.
#[derive(Debug, Clone)]
pub struct JmsVertex {
    pub position: RealPoint3d,
    pub normal: RealVector3d,
    /// Authored tangent-space basis, when the source carries it (H2 rendered
    /// vertices do; CE / lightmap vertices don't). Lets consumers use the
    /// engine's exact tangent frame for normal mapping instead of deriving one.
    pub tangent: Option<RealVector3d>,
    pub binormal: Option<RealVector3d>,
    pub node_sets: Vec<(i16, f32)>,
    pub uvs: Vec<crate::math::RealPoint2d>,
}

/// JMS triangle: material slot + 3 vertex indices into [`JmsFile::vertices`].
/// `region` indexes [`JmsFile::regions`] and is only emitted by the older
/// (Halo CE, 8198) triangle format — it stays 0 for the modern format,
/// which folds region into the material slot label.
#[derive(Debug, Clone)]
pub struct JmsTriangle {
    pub material: i32,
    pub v: [u32; 3],
    pub region: i32,
}

/// JMS sphere collision primitive. `parent` is a node index, `material`
/// indexes into [`JmsFile::materials`].
#[derive(Debug, Clone)]
pub struct JmsSphere {
    pub name: String,
    pub parent: i32,
    pub material: i32,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub radius: f32,
}

/// JMS axis-aligned-in-local-space box. `width`/`length`/`height` are
/// FULL extents (twice the half-extents the tag stores).
#[derive(Debug, Clone)]
pub struct JmsBox {
    pub name: String,
    pub parent: i32,
    pub material: i32,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub width: f32,
    pub length: f32,
    pub height: f32,
}

/// JMS capsule (Halo "pill"). Anchored at the bottom-cap center.
#[derive(Debug, Clone)]
pub struct JmsCapsule {
    pub name: String,
    pub parent: i32,
    pub material: i32,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub height: f32,
    pub radius: f32,
}

/// JMS convex shape — explicit per-vertex polyhedron.
#[derive(Debug, Clone)]
pub struct JmsConvex {
    pub name: String,
    pub parent: i32,
    pub material: i32,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub vertices: Vec<RealPoint3d>,
}

/// JMS ragdoll constraint between two bodies.
#[derive(Debug, Clone)]
pub struct JmsRagdoll {
    pub name: String,
    pub attached: i32,
    pub referenced: i32,
    pub attached_rotation: RealQuaternion,
    pub attached_translation: RealPoint3d,
    pub referenced_rotation: RealQuaternion,
    pub referenced_translation: RealPoint3d,
    pub min_twist: f32, pub max_twist: f32,
    pub min_cone: f32, pub max_cone: f32,
    pub min_plane: f32, pub max_plane: f32,
    pub friction_limit: f32,
}

/// JMS hinge constraint (covers `hinge_constraints` and
/// `limited_hinge_constraints` variants — `is_limited` distinguishes).
#[derive(Debug, Clone)]
pub struct JmsHinge {
    pub name: String,
    pub body_a: i32,
    pub body_b: i32,
    pub a_rotation: RealQuaternion,
    pub a_translation: RealPoint3d,
    pub b_rotation: RealQuaternion,
    pub b_translation: RealPoint3d,
    pub is_limited: i32,
    pub friction_limit: f32,
    pub min_angle: f32,
    pub max_angle: f32,
}

/// A reconstructed JMS file in memory — the full set of sections
/// JMS export emits, ready for [`Self::write`] or for inspection by
/// validators. Render-model fields (`nodes`/`materials`/`markers`/
/// `vertices`/`triangles`) populate from `from_render_model`;
/// collision/physics fields populate from `from_collision_model` and
/// `from_physics_model`. Any can be combined into a single JmsFile
/// for an `.hlmt` (model) export.
#[derive(Debug, Clone, Default)]
pub struct JmsFile {
    pub nodes: Vec<JmsNode>,
    pub materials: Vec<JmsMaterial>,
    /// Region names — only populated/emitted for the older (Halo CE,
    /// 8197) format, which has a dedicated REGIONS section. Empty for the
    /// modern format (region is encoded in the material slot label).
    pub regions: Vec<String>,
    pub markers: Vec<JmsMarker>,
    pub vertices: Vec<JmsVertex>,
    pub triangles: Vec<JmsTriangle>,
    pub spheres: Vec<JmsSphere>,
    pub boxes: Vec<JmsBox>,
    pub capsules: Vec<JmsCapsule>,
    pub convex_shapes: Vec<JmsConvex>,
    pub ragdolls: Vec<JmsRagdoll>,
    pub hinges: Vec<JmsHinge>,
}

impl JmsFile {
    /// Walk a parsed `render_model` tag and reconstruct the JMS
    /// scene from its inline geometry (`per mesh temporary[*]`),
    /// nodes, marker_groups, and region/permutation/material walk.
    pub fn from_render_model(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        // The tag stores `default rotation/translation` LOCAL to each
        // node's parent. JMS expects nodes in WORLD-space bind pose,
        // so chain locals forward through parent pointers. Forward
        // chaining works because the tag stores nodes parent-before-
        // child. Markers, by contrast, stay local-to-their-attached-
        // node in JMS — the importer composes them via the bone
        // parent at scene-construction time. Same convention TagTool
        // / Foundry / the H3 Blender exporter all use.
        let local_nodes = read_nodes(&root)?;
        let world_nodes = chain_local_to_world(&local_nodes);
        let bounds = read_compression_bounds(&root);
        let (mut materials, part_material_map, mesh_emit_order) = build_materials(&root)?;
        let markers = read_markers(&root)?;
        let (mut vertices, mut triangles) = build_geometry(
            &root, &part_material_map, &mesh_emit_order, &bounds,
        )?;
        // Append per-instance-placement geometry. Mirrors Foundry's
        // `render_model.py` instance walk: each `instance placements[i]`
        // pairs with `meshes[instance_mesh_index].subparts[i]`, gets its
        // own (forward,left,up,position+scale) transform, and binds to a
        // single bone via `node_index`. Without this, characters whose
        // modular armor (gauntlets, helmets, etc.) lives in the instance
        // mesh — like the brute — extract with all attachments missing.
        // TagTool extracts this only for `VertexType.Decorator`; we
        // run it for every render_model that has placements.
        append_instance_geometry(&root, &mut materials, &mut vertices, &mut triangles, &bounds)?;
        Ok(Self { nodes: world_nodes, materials, markers, vertices, triangles, ..Default::default() })
    }

    /// Walk a Halo 2 `render_model` and reconstruct the JMS scene.
    ///
    /// Halo 2 stores render geometry differently from Halo 3: per-section
    /// under `sections[i]/section data[0]/section/{parts, raw vertices,
    /// strip indices}` rather than `render geometry/per mesh temporary`.
    /// `regions[]/permutations[]` carry per-LOD `Lx section index` fields
    /// (L1 = super low … L6 = hollywood/highest; we export the highest
    /// available, walking L6→L1). Vertices are decompressed
    /// floats — the per-section `geometry compression` bounds are
    /// vestigial X360 metadata, so no dequantization is applied. Triangle
    /// strips index the section's own `raw vertices`; each part owns a
    /// `[strip start .. strip start + strip length]` sub-range and a
    /// material. Node binding follows the section's classification:
    /// worldspace/rigid bind every vertex to the section's single `rigid
    /// node`; rigid-boned/skinned use the per-vertex node indices/weights
    /// (node-map remap is not yet applied — H2 sections in the corpus
    /// carry `node map size == 0`, i.e. already-global indices).
    pub fn from_h2_render_model(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        let world_nodes = chain_local_to_world(&read_nodes(&root)?);
        let markers = read_markers(&root)?;

        let mats_block = root.field_path("materials").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("materials"))?;
        let regions_block = root.field_path("regions").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("regions"))?;
        let sections_block = root.field_path("sections").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("sections"))?;

        let mut materials: Vec<JmsMaterial> = Vec::new();
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();
        let mut emitted_sections: Vec<i32> = Vec::new();

        for ri in 0..regions_block.len() {
            let region = regions_block.element(ri).unwrap();
            let region_name = region.read_string_id("name").unwrap_or_default();
            let perms = match region.field("permutations").and_then(|f| f.as_block()) {
                Some(b) => b, None => continue,
            };
            for pi in 0..perms.len() {
                let perm = perms.element(pi).unwrap();
                let perm_name = perm.read_string_id("name").unwrap_or_default();
                // Export the highest-detail LOD whose section index is in
                // range. In Halo 2 the `Lx` slots run *low→high*: L1 is
                // "super low", L6 is "hollywood" (the highest detail), so
                // walk them HIGH→LOW and take the first valid one. Some
                // perms carry stale/garbage values in unused slots (e.g.
                // 27765 on a 1-section model), so require the index to
                // actually address a section, not merely be >= 0.
                let nsec = sections_block.len();
                let sec_idx = ["L6 section index", "L5 section index", "L4 section index",
                               "L3 section index", "L2 section index", "L1 section index"]
                    .iter()
                    .find_map(|f| perm.read_int_any(f).map(|v| v as i32)
                        .filter(|&v| v >= 0 && (v as usize) < nsec))
                    .unwrap_or(-1);
                if sec_idx < 0 { continue; }
                // A section may be referenced by several (region, perm)
                // pairs; emit it once (first reference wins the label).
                if emitted_sections.contains(&sec_idx) { continue; }
                emitted_sections.push(sec_idx);

                let section = sections_block.element(sec_idx as usize).unwrap();
                let classification = section
                    .read_int_any("global_geometry_classification_enum_definition")
                    .unwrap_or(1) as i32;
                let rigid_node = section.read_int_any("rigid node").map(|v| v as i16).unwrap_or(-1);

                let Some(sd_elem) = section
                    .field("section data").and_then(|f| f.as_block())
                    .and_then(|b| b.element(0))
                else { continue };
                let Some(sd) = sd_elem.field("section").and_then(|f| f.as_struct())
                else { continue };

                // Halo 2 stores per-vertex bone indices LOCAL to the
                // section; the section's `node map` remaps them to global
                // skeleton nodes (Reclaimer's `nodeMap[blendIndex]`). Without
                // this the mesh skins to the wrong bones — invisible at the
                // bind pose, but every animation explodes the model. An empty
                // node map means the indices are already global.
                let node_map: Vec<i16> = sd_elem
                    .field("node map").and_then(|f| f.as_block())
                    .map(|b| (0..b.len())
                        .filter_map(|k| b.element(k))
                        .map(|e| e.read_int_any("node index").unwrap_or(-1) as i16)
                        .collect())
                    .unwrap_or_default();
                let remap = |idx: i16| -> i16 {
                    match usize::try_from(idx) {
                        Ok(i) if !node_map.is_empty() && i < node_map.len() => node_map[i],
                        _ => idx,
                    }
                };

                let raw_v = match sd.field("raw vertices").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                let strip = match sd.field("strip indices").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                let parts = match sd.field("parts").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };

                // H2 strip indices are u16 with a `0xFFFF` restart
                // sentinel between subparts (use the u16 strip decoder,
                // NOT the u32 one whose sentinel is `0xFFFFFFFF`).
                let strip_idx: Vec<u16> = (0..strip.len())
                    .filter_map(|k| strip.element(k))
                    .map(|e| e.read_int_any("index").unwrap_or(0) as u16)
                    .collect();

                for part_i in 0..parts.len() {
                    let part = parts.element(part_i).unwrap();
                    let mat_idx = part.read_int_any("material").unwrap_or(0);
                    let shader_name = if mat_idx >= 0 && (mat_idx as usize) < mats_block.len() {
                        let m = mats_block.element(mat_idx as usize).unwrap();
                        let path = m.read_tag_ref_path("shader").unwrap_or_default();
                        Path::new(&path.replace('\\', "/"))
                            .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned()
                    } else {
                        "default".to_owned()
                    };
                    let cell_label = format!("{perm_name} {region_name}");
                    let jms_mat = match materials.iter().position(|m|
                        m.name == shader_name && m.material_name.ends_with(&cell_label)
                    ) {
                        Some(idx) => idx as i32,
                        None => {
                            let slot = materials.len() + 1;
                            materials.push(JmsMaterial {
                                name: shader_name,
                                material_name: format!("({slot}) {cell_label}"),
                            });
                            (materials.len() - 1) as i32
                        }
                    };

                    let start = part.read_int_any("strip start index").unwrap_or(0).max(0) as usize;
                    let len = part.read_int_any("strip length").unwrap_or(0).max(0) as usize;
                    if start >= strip_idx.len() { continue; }
                    let end = (start + len).min(strip_idx.len());
                    for (a, b, c) in strip_to_list(&strip_idx[start..end]) {
                        let base = vertices.len() as u32;
                        for vi in [a, b, c] {
                            let Some(v) = raw_v.element(vi as usize) else { continue };
                            let mut jv = read_h2_vertex(&v);
                            // Classification 0/1 = worldspace/rigid: bind
                            // the whole section to its single rigid node.
                            // Skinned/rigid-boned: remap each per-vertex
                            // local bone index through the section node map.
                            if classification <= 1 {
                                jv.node_sets = vec![(remap(rigid_node.max(0)), 1.0)];
                            } else {
                                for ns in jv.node_sets.iter_mut() {
                                    ns.0 = remap(ns.0);
                                }
                                if jv.node_sets.is_empty() && rigid_node >= 0 {
                                    jv.node_sets.push((remap(rigid_node), 1.0));
                                }
                            }
                            vertices.push(jv);
                        }
                        triangles.push(JmsTriangle { material: jms_mat, v: [base, base + 1, base + 2], region: 0 });
                    }
                }
            }
        }
        Ok(Self { nodes: world_nodes, materials, markers, vertices, triangles, ..Default::default() })
    }

    /// Walk a Halo CE `gbxmodel` and reconstruct the JMS scene.
    ///
    /// Halo 1 geometry is `geometries[g]/parts[p]` selected per region/
    /// permutation by a LOD geometry index (`super high` down to `super
    /// low`; we export the highest available). Each part carries an
    /// `uncompressed vertices` block — full float position/normal/texcoord
    /// + two node indices and weights — so no dequantization is needed
    /// (the parallel `compressed vertices` block is the 32-bit-packed
    /// alternate). `triangle data` is a triangle strip stored as 3-index
    /// chunks with `-1` (`0xFFFF`) restart/padding. Node indices are
    /// global unless the `parts have local nodes` flag is set (local node
    /// maps are not yet applied). Materials come from `shaders[]`.
    pub fn from_gbxmodel(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        let world_nodes = chain_local_to_world(&read_nodes(&root)?);

        let shaders_block = root.field_path("shaders").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("shaders"))?;
        let regions_block = root.field_path("regions").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("regions"))?;
        let geometries_block = root.field_path("geometries").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("geometries"))?;

        // Halo CE: one JMS material per shader (region is a SEPARATE
        // section + a per-triangle index, not folded into the material as
        // the modern format does). Material i ↔ shaders[i]; a part's
        // `shader index` is therefore its material index directly.
        let mut materials: Vec<JmsMaterial> = Vec::with_capacity(shaders_block.len());
        for si in 0..shaders_block.len() {
            let s = shaders_block.element(si).unwrap();
            let path = s.read_tag_ref_path("shader").unwrap_or_default();
            let name = Path::new(&path.replace('\\', "/"))
                .file_stem().and_then(|x| x.to_str()).unwrap_or("default").to_owned();
            materials.push(JmsMaterial { name, material_name: "<none>".to_owned() });
        }

        let mut regions: Vec<String> = Vec::with_capacity(regions_block.len());
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();

        for ri in 0..regions_block.len() {
            let region = regions_block.element(ri).unwrap();
            // Keep region indices aligned with `ri` (push every region,
            // even geometry-less ones).
            regions.push(region.read_string("name").unwrap_or_default());
            let perms = match region.field("permutations").and_then(|f| f.as_block()) {
                Some(b) => b, None => continue,
            };
            for pi in 0..perms.len() {
                let perm = perms.element(pi).unwrap();
                let ngeo = geometries_block.len();
                let geo_idx = ["super high", "high", "medium", "low", "super low"]
                    .iter()
                    .find_map(|f| perm.read_int_any(f).map(|v| v as i32)
                        .filter(|&v| v >= 0 && (v as usize) < ngeo))
                    .unwrap_or(-1);
                if geo_idx < 0 { continue; }
                let geo = geometries_block.element(geo_idx as usize).unwrap();
                let parts = match geo.field("parts").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                for part_i in 0..parts.len() {
                    let part = parts.element(part_i).unwrap();
                    let mat = part.read_int_any("shader index").unwrap_or(0).max(0) as i32;

                    let uv = match part.field("uncompressed vertices").and_then(|f| f.as_block()) {
                        Some(b) => b, None => continue,
                    };
                    let td = match part.field("triangle data").and_then(|f| f.as_block()) {
                        Some(b) => b, None => continue,
                    };
                    // Flatten the triangle-data chunks (each holds 3 `indices`)
                    // into one strip; `-1` becomes the 0xFFFF restart sentinel.
                    let mut strip: Vec<u16> = Vec::with_capacity(td.len() * 3);
                    for k in 0..td.len() {
                        let t = td.element(k).unwrap();
                        for f in t.fields() {
                            if let Some(TagFieldData::ShortInteger(i)) = f.value() {
                                strip.push(i as u16);
                            }
                        }
                    }
                    for (a, b, c) in strip_to_list(&strip) {
                        let base = vertices.len() as u32;
                        // CE gbxmodel triangles wind opposite to their stored
                        // vertex normals, so emit them reversed (`a, c, b`) to
                        // make winding agree with the normals — matching the
                        // General-101 Blender toolset, which reverses CE model
                        // triangles "to fix facing normals".
                        for vi in [a, c, b] {
                            let Some(v) = uv.element(vi as usize) else { continue };
                            vertices.push(read_ce_vertex(&v));
                        }
                        triangles.push(JmsTriangle {
                            material: mat,
                            v: [base, base + 1, base + 2],
                            region: ri as i32,
                        });
                    }
                }
            }
        }
        Ok(Self { nodes: world_nodes, materials, regions, vertices, triangles, ..Default::default() })
    }

    /// Walk a parsed `collision_model` tag and reconstruct the JMS
    /// scene from its BSP geometry. Vertices stay in their BSP's
    /// local space — pass a `render_model`-derived skeleton via
    /// [`Self::from_collision_model_with_skeleton`] if you want
    /// world-space placement (which is what embedded source JMSes
    /// carry).
    pub fn from_collision_model(tag: &TagFile) -> Result<Self, JmsError> {
        Self::build_collision_model(tag, None)
    }

    /// Same as [`Self::from_collision_model`] but composes each
    /// BSP's vertices through the skeleton's world-space transforms
    /// (chained from the render_model's local-space `default
    /// rotation`/`translation`). The lookup matches BSP node names
    /// against the supplied skeleton's node names — bones not found
    /// in the skeleton stay in BSP-local space.
    pub fn from_collision_model_with_skeleton(
        tag: &TagFile,
        skeleton: &[JmsNode],
    ) -> Result<Self, JmsError> {
        Self::build_collision_model(tag, Some(skeleton))
    }

    fn build_collision_model(tag: &TagFile, skeleton: Option<&[JmsNode]>) -> Result<Self, JmsError> {
        let root = tag.root();
        let nodes = read_phmo_nodes(&root)?;
        // Build name → world-transform map from the skeleton (if
        // provided). The skeleton is expected to be in world space
        // (e.g. the result of `from_render_model`).
        let bone_xform: Option<std::collections::HashMap<String, (RealQuaternion, RealPoint3d)>> =
            skeleton.map(|nodes| {
                nodes.iter().map(|n| (n.name.clone(), (n.rotation, n.translation))).collect()
            });
        let materials_block = root.field_path("materials").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("materials"))?;
        let regions_block = root.field_path("regions").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("regions"))?;

        let mut materials: Vec<JmsMaterial> = Vec::new();
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();

        for ri in 0..regions_block.len() {
            let region = regions_block.element(ri).unwrap();
            let region_name = region.read_string_id("name").unwrap_or_default();
            let perms = match region.field("permutations").and_then(|f| f.as_block()) {
                Some(b) => b, None => continue,
            };
            for pi in 0..perms.len() {
                let perm = perms.element(pi).unwrap();
                let perm_name = perm.read_string_id("name").unwrap_or_default();
                let bsps = match perm.field("bsps").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                for bi in 0..bsps.len() {
                    let bsp_elem = bsps.element(bi).unwrap();
                    let node_idx = bsp_elem.read_int_any("node index").map(|v| v as i16).unwrap_or(-1);
                    let bsp = match bsp_elem.field("bsp").and_then(|f| f.as_struct()) { Some(s) => s, None => continue };
                    let surfaces = match bsp.field("surfaces").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                    let edges = match bsp.field("edges").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                    let bsp_verts = match bsp.field("vertices").and_then(|f| f.as_block()) { Some(b) => b, None => continue };

                    // World transform for this BSP — looked up by
                    // the BSP's bone NAME in the supplied skeleton
                    // (collision_model nodes carry no transforms).
                    // None means we leave vertices in BSP-local
                    // space; matches what `from_collision_model`
                    // gives without a skeleton.
                    let bone_world = if let (Some(map), Some(node_block)) = (
                        bone_xform.as_ref(),
                        Some(&nodes),
                    ) {
                        node_block.get(node_idx as usize)
                            .map(|n| n.name.as_str())
                            .and_then(|name| map.get(name))
                            .copied()
                    } else { None };

                    let cell_label = format!("{} {}", perm_name, region_name);
                    Self::emit_collision_bsp(
                        &surfaces, &edges, &bsp_verts, node_idx, bone_world,
                        &materials_block, &cell_label,
                        &mut materials, &mut vertices, &mut triangles,
                    );
                }
            }
        }

        Ok(Self { nodes, materials, vertices, triangles, ..Default::default() })
    }

    /// Walk a parsed Halo CE `model_collision_geometry` tag and
    /// reconstruct the JMS scene. CE stores collision BSPs per-node
    /// (`nodes[i]/bsps[j]`) with the surface/edge/vertex blocks
    /// directly inside each `bsps` element — there is no
    /// region/permutation/`bsp`-wrapper nesting and no skeleton
    /// composition (CE collision vertices are already in node-local
    /// space, and the node's own bind transform is not stored here).
    pub fn from_model_collision_geometry(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        let nodes = read_nodes(&root).or_else(|_| read_phmo_nodes(&root))?;
        let materials_block = root.field_path("materials").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("materials"))?;
        let nodes_block = root.field_path("nodes").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("nodes"))?;

        let mut materials: Vec<JmsMaterial> = Vec::new();
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();

        for ni in 0..nodes_block.len() {
            let node = nodes_block.element(ni).unwrap();
            let node_name = node.read_string_id("name").or_else(|| node.read_string("name")).unwrap_or_default();
            let bsps = match node.field("bsps").and_then(|f| f.as_block()) {
                Some(b) => b, None => continue,
            };
            for bi in 0..bsps.len() {
                let bsp = bsps.element(bi).unwrap();
                let surfaces = match bsp.field("surfaces").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let edges = match bsp.field("edges").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let bsp_verts = match bsp.field("vertices").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                Self::emit_collision_bsp(
                    &surfaces, &edges, &bsp_verts, ni as i16, None,
                    &materials_block, &node_name,
                    &mut materials, &mut vertices, &mut triangles,
                );
            }
        }

        Ok(Self { nodes, materials, vertices, triangles, ..Default::default() })
    }

    /// Emit triangles for one collision BSP (a `surfaces`/`edges`/
    /// `vertices` triple) into the shared material/vertex/triangle
    /// accumulators. Shared by the H2/H3 `collision_model` walker and
    /// the CE `model_collision_geometry` walker — the only structural
    /// differences (per-node vs per-region nesting, point-vs-vector
    /// vertices, string-id-vs-string material names, index widths) are
    /// handled by the readers here, which accept either form.
    #[allow(clippy::too_many_arguments)]
    fn emit_collision_bsp(
        surfaces: &crate::api::TagBlock<'_>,
        edges: &crate::api::TagBlock<'_>,
        bsp_verts: &crate::api::TagBlock<'_>,
        node_idx: i16,
        bone_world: Option<(RealQuaternion, RealPoint3d)>,
        materials_block: &crate::api::TagBlock<'_>,
        cell_label: &str,
        materials: &mut Vec<JmsMaterial>,
        vertices: &mut Vec<JmsVertex>,
        triangles: &mut Vec<JmsTriangle>,
    ) {
        // Build a (start_vertex, end_vertex, forward, reverse,
        // left_surface, right_surface) cache to avoid hammering the
        // as_struct API in the hot edge-walk loop.
        let edge_cache: Vec<EdgeRow> = (0..edges.len()).map(|k| {
            let e = edges.element(k).unwrap();
            EdgeRow {
                start_vertex: e.read_int_any("start vertex").unwrap_or(-1) as i32,
                end_vertex: e.read_int_any("end vertex").unwrap_or(-1) as i32,
                forward_edge: e.read_int_any("forward edge").unwrap_or(-1) as i32,
                reverse_edge: e.read_int_any("reverse edge").unwrap_or(-1) as i32,
                left_surface: e.read_int_any("left surface").unwrap_or(-1) as i32,
                right_surface: e.read_int_any("right surface").unwrap_or(-1) as i32,
            }
        }).collect();

        // CE stores `point` as real_vector_3d, H2/H3 as real_point_3d
        // — read_point_or_vec accepts either.
        let vert_points: Vec<RealPoint3d> = (0..bsp_verts.len()).map(|k| {
            let local = read_point_or_vec(&bsp_verts.element(k).unwrap(), "point") * SCALE;
            if let Some((rot, trans)) = bone_world {
                // World = bone_translation + bone_rotation.rotate(local)
                trans + rot * local.as_vector()
            } else {
                local
            }
        }).collect();

        for si in 0..surfaces.len() {
            let surface = surfaces.element(si).unwrap();
            let first_edge = surface.read_int_any("first edge").unwrap_or(-1) as i32;
            if first_edge < 0 { continue; }
            let surface_material = surface.read_int_any("material").unwrap_or(-1) as i32;

            // Edge-ring walk.
            let polygon = walk_surface_ring(si as i32, first_edge, &edge_cache);
            if polygon.len() < 3 { continue; }

            // Look up shader name for this surface's material.
            // H2/H3 store it as a string_id, CE as an inline string.
            let shader_name = if surface_material >= 0 && (surface_material as usize) < materials_block.len() {
                let m = materials_block.element(surface_material as usize).unwrap();
                // collision_model materials carry a `name`; structure-BSP
                // collision materials instead carry a `shader` tag_reference
                // — accept either, using the shader tag's basename.
                m.read_string_id("name").or_else(|| m.read_string("name"))
                    .or_else(|| m.read_tag_ref_path("shader").map(|p| {
                        p.rsplit(['\\', '/']).next().unwrap_or(&p).to_owned()
                    }))
                    .unwrap_or_default()
            } else {
                "default".to_owned()
            };
            let jms_idx = match materials.iter().position(|m|
                m.name == shader_name && m.material_name.ends_with(cell_label)
            ) {
                Some(i) => i as i32,
                None => {
                    let slot = materials.len() + 1;
                    materials.push(JmsMaterial {
                        name: shader_name,
                        material_name: format!("({}) {}", slot, cell_label),
                    });
                    (materials.len() - 1) as i32
                }
            };

            // Triangle-fan the convex polygon.
            for k in 1..polygon.len() - 1 {
                let a = polygon[0];
                let b = polygon[k];
                let c = polygon[k + 1];
                let base = vertices.len() as u32;
                for &vi in &[a, b, c] {
                    let pos = vert_points.get(vi as usize).copied().unwrap_or(RealPoint3d::ZERO);
                    vertices.push(JmsVertex {
                        position: pos,
                        normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
                        tangent: None,
                        binormal: None,
                        node_sets: vec![(node_idx, 1.0)],
                        uvs: vec![crate::math::RealPoint2d::ZERO],
                    });
                }
                triangles.push(JmsTriangle {
                    material: jms_idx,
                    v: [base, base + 1, base + 2],
                    region: 0,
                });
            }
        }
    }

    /// Reconstruct the render JMS for a Halo CE
    /// `scenario_structure_bsp`. CE level geometry lives in
    /// `lightmaps[i]/materials[j]`: each material carries its own
    /// `uncompressed vertices` blob (an array of 56-byte
    /// position/normal/binormal/tangent/uv vertices) and a
    /// `[surfaces, surfaces+surface count)` range into the top-level
    /// `surfaces` triangle list, whose `vertex0/1/2 index` are local
    /// to that material's vertex array. Emits one JMS mesh with a
    /// single `frame` node (CE structure JMS form: no skeleton, no
    /// regions) and per-shader materials. Vertex floats are read
    /// big-endian to match the CE engine.
    pub fn from_scenario_structure_bsp_ce(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        let global_surfaces = root.field_path("surfaces").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("surfaces"))?;
        let lightmaps = root.field_path("lightmaps").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("lightmaps"))?;

        // CE structure JMS still needs a node — a single root `frame`.
        let nodes = vec![JmsNode {
            name: "frame".to_owned(),
            parent: -1,
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
        }];
        let mut materials: Vec<JmsMaterial> = Vec::new();
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();

        for li in 0..lightmaps.len() {
            let lm = lightmaps.element(li).unwrap();
            let mats = match lm.field("materials").and_then(|f| f.as_block()) {
                Some(b) => b, None => continue,
            };
            for mi in 0..mats.len() {
                let material = mats.element(mi).unwrap();
                let nverts = material.field("rendered vertices").and_then(|f| f.as_struct())
                    .and_then(|s| s.read_int_any("vertex count")).unwrap_or(0) as usize;
                let surf_start = material.read_int_any("surfaces").unwrap_or(0) as i64;
                let surf_count = material.read_int_any("surface count").unwrap_or(0) as i64;
                let blob = match material.field("uncompressed vertices").and_then(|f| f.as_data()) {
                    Some(b) => b, None => continue,
                };
                // The rendered vertex is 56 bytes — position(3)
                // normal(3) binormal(3) tangent(3) uv(2), 14 floats.
                // The blob holds two CONTIGUOUS arrays: the rendered
                // vertices (56 B each) followed by the lightmap
                // vertices (normal(3)+uv(2), 20 B each) when present —
                // so blob.len() is 56*n or 76*n, but the rendered
                // array is always the leading 56*n bytes at stride 56.
                // (Matches invader's `uncompressed_vertices[v]` indexing
                // by sizeof(UncompressedRenderedVertex)=56.) Floats are
                // little-endian — the vertex blob keeps original LE byte
                // order even though CE's structured fields are big-endian.
                if nverts == 0 { continue; }
                const STRIDE: usize = 56;
                let avail = blob.len() / STRIDE;
                let n = nverts.min(avail);
                let base = vertices.len() as u32;
                for v in 0..n {
                    let o = v * STRIDE;
                    let f = |k: usize| {
                        let p = o + k * 4;
                        f32::from_le_bytes([blob[p], blob[p + 1], blob[p + 2], blob[p + 3]])
                    };
                    vertices.push(JmsVertex {
                        position: RealPoint3d { x: f(0) * SCALE, y: f(1) * SCALE, z: f(2) * SCALE },
                        normal: RealVector3d { i: f(3), j: f(4), k: f(5) },
                        // 56-byte rendered vertex: binormal floats 6-8, tangent 9-11.
                        binormal: Some(RealVector3d { i: f(6), j: f(7), k: f(8) }),
                        tangent: Some(RealVector3d { i: f(9), j: f(10), k: f(11) }),
                        node_sets: vec![(0, 1.0)],
                        uvs: vec![crate::math::RealPoint2d { x: f(12), y: f(13) }],
                    });
                }

                // Material slot, keyed by shader basename.
                let shader_name = material.read_tag_ref_path("shader")
                    .map(|p| p.rsplit(['\\', '/']).next().unwrap_or(&p).to_owned())
                    .unwrap_or_else(|| "default".to_owned());
                let jms_idx = match materials.iter().position(|m| m.name == shader_name) {
                    Some(i) => i as i32,
                    None => {
                        let slot = materials.len() + 1;
                        materials.push(JmsMaterial {
                            name: shader_name.clone(),
                            material_name: format!("({}) {}", slot, shader_name),
                        });
                        (materials.len() - 1) as i32
                    }
                };

                for si in surf_start..(surf_start + surf_count) {
                    if si < 0 || si as usize >= global_surfaces.len() { continue; }
                    let s = global_surfaces.element(si as usize).unwrap();
                    let v0 = s.read_int_any("vertex0 index").unwrap_or(-1);
                    let v1 = s.read_int_any("vertex1 index").unwrap_or(-1);
                    let v2 = s.read_int_any("vertex2 index").unwrap_or(-1);
                    if v0 < 0 || v1 < 0 || v2 < 0 { continue; }
                    let (v0, v1, v2) = (v0 as u32, v1 as u32, v2 as u32);
                    if (v0 as usize) >= n || (v1 as usize) >= n || (v2 as usize) >= n { continue; }
                    triangles.push(JmsTriangle {
                        material: jms_idx,
                        v: [base + v0, base + v1, base + v2],
                        region: 0,
                    });
                }
            }
        }

        Ok(Self { nodes, materials, vertices, triangles, ..Default::default() })
    }

    /// Reconstruct the collision JMS for a Halo CE
    /// `scenario_structure_bsp`. The structure's collision lives in the
    /// `collision bsp` block (planes/surfaces/edges/vertices, the same
    /// edge-ring shape as `model_collision_geometry`); material names
    /// come from the `collision materials` block's `shader` tag-refs.
    /// Vertices stay in world space (BSP geometry is already there).
    pub fn from_scenario_structure_bsp_ce_collision(tag: &TagFile) -> Result<Self, JmsError> {
        let root = tag.root();
        let materials_block = root.field_path("collision materials").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("collision materials"))?;
        let coll_bsps = root.field_path("collision bsp").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("collision bsp"))?;

        let nodes = vec![JmsNode {
            name: "frame".to_owned(),
            parent: -1,
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
        }];
        let mut materials: Vec<JmsMaterial> = Vec::new();
        let mut vertices: Vec<JmsVertex> = Vec::new();
        let mut triangles: Vec<JmsTriangle> = Vec::new();

        for bi in 0..coll_bsps.len() {
            let bsp = coll_bsps.element(bi).unwrap();
            let surfaces = match bsp.field("surfaces").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
            let edges = match bsp.field("edges").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
            let bsp_verts = match bsp.field("vertices").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
            Self::emit_collision_bsp(
                &surfaces, &edges, &bsp_verts, 0, None,
                &materials_block, "collision",
                &mut materials, &mut vertices, &mut triangles,
            );
        }

        Ok(Self { nodes, materials, vertices, triangles, ..Default::default() })
    }

    /// Walk a parsed `physics_model` tag and reconstruct the JMS
    /// scene from its Havok shape primitives + ragdoll/hinge
    /// constraints. Without a skeleton, the emitted nodes carry
    /// only names + tree links (identity transforms) — pass a
    /// render_model-derived skeleton via
    /// [`Self::from_physics_model_with_skeleton`] to populate
    /// world-space bind-pose transforms for the JMS importer.
    pub fn from_physics_model(tag: &TagFile) -> Result<Self, JmsError> {
        Self::build_physics_model(tag, None)
    }

    /// Same as [`Self::from_physics_model`] but layers the supplied
    /// skeleton's world-space transforms onto the phmo's nodes,
    /// matched by name. Bones not found in the skeleton stay at
    /// identity. Use the skeleton from a sibling `render_model`
    /// (via `JmsFile::from_render_model`).
    pub fn from_physics_model_with_skeleton(
        tag: &TagFile,
        skeleton: &[JmsNode],
    ) -> Result<Self, JmsError> {
        Self::build_physics_model(tag, Some(skeleton))
    }

    fn build_physics_model(tag: &TagFile, skeleton: Option<&[JmsNode]>) -> Result<Self, JmsError> {
        let root = tag.root();
        let mut nodes = read_phmo_nodes(&root)?;
        if let Some(skel) = skeleton {
            let by_name: std::collections::HashMap<&str, &JmsNode> =
                skel.iter().map(|n| (n.name.as_str(), n)).collect();
            for n in nodes.iter_mut() {
                if let Some(src) = by_name.get(n.name.as_str()) {
                    n.rotation = src.rotation;
                    n.translation = src.translation;
                }
            }
        }
        let materials = read_phmo_materials(&root)?;
        let parent_lookup = build_phmo_parent_lookup(&root);
        let spheres = read_phmo_spheres(&root, &parent_lookup);
        let boxes = read_phmo_boxes(&root, &parent_lookup);
        let capsules = read_phmo_pills(&root, &parent_lookup);
        let convex_shapes = read_phmo_polyhedra(&root, &parent_lookup);
        let ragdolls = read_phmo_ragdolls(&root);
        let mut hinges = read_phmo_hinges(&root, false);
        hinges.extend(read_phmo_hinges(&root, true));
        Ok(Self {
            nodes,
            materials,
            spheres,
            boxes,
            capsules,
            convex_shapes,
            ragdolls,
            hinges,
            ..Default::default()
        })
    }

    /// Reconstruct the JMS from a Halo 2 `physics_model`. Same shape
    /// set as H3 (spheres / pills / boxes / polyhedra + ragdoll/hinge
    /// constraints) but H2 stores each shape FLAT (name / material /
    /// radius / transform directly on the shape block, with no `base` /
    /// `box shape` / `capsule shape` substructs) and references nodes by
    /// the shape's `name` string_id matching a node name (its rigid
    /// bodies carry no shape reference). Constraints, nodes and
    /// materials reuse the shared phmo readers.
    pub fn from_physics_model_h2(tag: &TagFile) -> Result<Self, JmsError> {
        Self::build_physics_model_h2(tag, None)
    }

    /// Same as [`Self::from_physics_model_h2`] but overlays the supplied
    /// skeleton's world-space transforms onto the phmo nodes (matched by
    /// name), exactly like [`Self::from_physics_model_with_skeleton`].
    pub fn from_physics_model_h2_with_skeleton(
        tag: &TagFile,
        skeleton: &[JmsNode],
    ) -> Result<Self, JmsError> {
        Self::build_physics_model_h2(tag, Some(skeleton))
    }

    fn build_physics_model_h2(tag: &TagFile, skeleton: Option<&[JmsNode]>) -> Result<Self, JmsError> {
        let root = tag.root();
        let nodes = read_phmo_nodes(&root)?;
        let nodes = if let Some(skel) = skeleton {
            let by_name: std::collections::HashMap<&str, &JmsNode> =
                skel.iter().map(|n| (n.name.as_str(), n)).collect();
            nodes.into_iter().map(|mut n| {
                if let Some(src) = by_name.get(n.name.as_str()) {
                    n.rotation = src.rotation;
                    n.translation = src.translation;
                }
                n
            }).collect()
        } else {
            nodes
        };
        let materials = read_phmo_materials(&root)?;
        // Primary parenting: each H2 rigid_body carries node + a
        // (shape_type, shape_index) reference — the same scheme the H3
        // reader uses. Our generated def leaves that 4-byte reference as
        // an unnamed pointer field, so read it from the element bytes
        // (shape_type @ +56, shape_index @ +58 in the v1/144-byte rigid
        // body; verified against masterchief). Map (type, index) → node.
        // Falls back to name-matching (shape name == bone name) when the
        // reference is unavailable (e.g. an older v0 rigid-body layout).
        let (parent_map, default_node) = build_h2_shape_parent_map(&root);
        let name_to_node: std::collections::HashMap<String, i32> = nodes.iter()
            .enumerate().map(|(i, n)| (n.name.clone(), i as i32)).collect();
        let spheres = read_phmo_h2_spheres(&root, &parent_map, &name_to_node, default_node);
        let boxes = read_phmo_h2_boxes(&root, &parent_map, &name_to_node, default_node);
        let capsules = read_phmo_h2_pills(&root, &parent_map, &name_to_node, default_node);
        let convex_shapes = read_phmo_h2_polyhedra(&root, &parent_map, &name_to_node, default_node);
        let ragdolls = read_phmo_ragdolls(&root);
        let mut hinges = read_phmo_hinges(&root, false);
        hinges.extend(read_phmo_hinges(&root, true));
        Ok(Self {
            nodes,
            materials,
            spheres,
            boxes,
            capsules,
            convex_shapes,
            ragdolls,
            hinges,
            ..Default::default()
        })
    }

    /// Write the JMS as version 8213 text format (the H3 source
    /// convention) into `w`. Layout matches the embedded-source
    /// section ordering exactly so byte-diffs against artist
    /// originals stay focused on the data, not boilerplate.
    /// Serialize as JMS text at the given format `version`. Use
    /// [`crate::game::Game::jms_version`] to pick it per engine: 8200
    /// (Halo CE), 8210 (Halo 2), 8213 (Halo 3+). The in-memory section
    /// data is version-neutral; this method emits the version-correct
    /// field layout. Currently the 8210/8213 deltas (vertex color,
    /// trailing SKYLIGHT section) are handled; the older 8200 layout
    /// (region section, child/sibling nodes, 2-node vertices) is a
    /// separate path added with the Halo CE reader.
    pub fn write<W: Write>(&self, w: &mut W, version: u16) -> Result<(), JmsError> {
        // The old (Halo CE, <= 8200) format is structurally different — a
        // bare numeric layout with child/sibling node links, a separate
        // REGIONS section, two-influence vertices, and per-triangle region
        // indices — so it has its own writer.
        if version <= 8200 {
            return self.write_jms_old(w, version);
        }
        // 8211+ (Halo 3) appends a per-vertex color triple; 8205 (Halo 2)
        // does not.
        let has_vertex_color = version >= 8211;
        writeln!(w, ";### VERSION ###")?;
        writeln!(w, "{version}")?;
        writeln!(w)?;

        // (modern format continues below)
        self.write_modern_after_version(w, version, has_vertex_color)
    }

    /// Old (Halo CE, <= 8200) bare JMS: no comment scaffolding, child/
    /// sibling node links (8197), name+texture materials (8197), a
    /// dedicated REGIONS section (8197), two-influence vertices (8199:
    /// node0 / pos / normal / node1 / node1-weight / uv / unused), and
    /// per-triangle region indices (8198).
    fn write_jms_old<W: Write>(&self, w: &mut W, version: u16) -> Result<(), JmsError> {
        writeln!(w, "{version}")?;
        writeln!(w, "0")?; // node list checksum (unused by importers)

        let (children, siblings) = derive_child_sibling(&self.nodes);
        writeln!(w, "{}", self.nodes.len())?;
        for (i, n) in self.nodes.iter().enumerate() {
            writeln!(w, "{}", n.name)?;
            writeln!(w, "{}", children[i])?;
            writeln!(w, "{}", siblings[i])?;
            write_floats(w, &n.rotation.to_array())?;
            write_floats(w, &n.translation.to_array())?;
        }

        writeln!(w, "{}", self.materials.len())?;
        for m in &self.materials {
            writeln!(w, "{}", m.name)?;
            writeln!(w, "{}", m.material_name)?; // 8197 "texture path" slot
        }

        writeln!(w, "{}", self.markers.len())?;
        for m in &self.markers {
            writeln!(w, "{}", m.name)?;
            writeln!(w, "-1")?; // region (markers aren't region-scoped here)
            writeln!(w, "{}", m.node_index)?;
            write_floats(w, &m.rotation.to_array())?;
            write_floats(w, &m.translation.to_array())?;
            write_floats(w, &[m.radius])?;
        }

        writeln!(w, "{}", self.regions.len())?;
        for r in &self.regions {
            writeln!(w, "{r}")?;
        }

        writeln!(w, "{}", self.vertices.len())?;
        for v in &self.vertices {
            let n0 = v.node_sets.first().copied().unwrap_or((-1, 0.0));
            let n1 = v.node_sets.get(1).copied().unwrap_or((-1, 0.0));
            writeln!(w, "{}", n0.0)?;
            write_floats(w, &v.position.to_array())?;
            write_floats(w, &v.normal.to_array())?;
            writeln!(w, "{}", n1.0)?;
            write_floats(w, &[n1.1])?;
            let uv = v.uvs.first().map(|u| u.to_array()).unwrap_or([0.0, 0.0]);
            write_floats(w, &uv)?;
            writeln!(w, "0")?; // unused flag
        }

        writeln!(w, "{}", self.triangles.len())?;
        for t in &self.triangles {
            writeln!(w, "{}", t.region)?;
            writeln!(w, "{}", t.material)?;
            writeln!(w, "{}\t{}\t{}", t.v[0], t.v[1], t.v[2])?;
        }
        Ok(())
    }

    fn write_modern_after_version<W: Write>(
        &self, w: &mut W, version: u16, has_vertex_color: bool,
    ) -> Result<(), JmsError> {
        writeln!(w, ";### NODES ###")?;
        writeln!(w, "{}", self.nodes.len())?;
        writeln!(w, ";\t<name>")?;
        writeln!(w, ";\t<parent node index>")?;
        writeln!(w, ";\t<default rotation <i,j,k,w>>")?;
        writeln!(w, ";\t<default translation <x,y,z>>")?;
        writeln!(w)?;
        for (i, n) in self.nodes.iter().enumerate() {
            writeln!(w, ";NODE {i}")?;
            writeln!(w, "{}", n.name)?;
            writeln!(w, "{}", n.parent)?;
            write_floats(w, &n.rotation.to_array())?;
            write_floats(w, &n.translation.to_array())?;
            writeln!(w)?;
        }

        writeln!(w, ";### MATERIALS ###")?;
        writeln!(w, "{}", self.materials.len())?;
        writeln!(w, ";\t<name>")?;
        writeln!(w, ";\t<material name>")?;
        writeln!(w)?;
        for (i, m) in self.materials.iter().enumerate() {
            writeln!(w, ";MATERIAL {i}")?;
            writeln!(w, "{}", m.name)?;
            writeln!(w, "{}", m.material_name)?;
            writeln!(w)?;
        }

        writeln!(w, ";### MARKERS ###")?;
        writeln!(w, "{}", self.markers.len())?;
        writeln!(w, ";\t<name>")?;
        writeln!(w, ";\t<node index>")?;
        writeln!(w, ";\t<rotation <i,j,k,w>>")?;
        writeln!(w, ";\t<translation <x,y,z>>")?;
        writeln!(w, ";\t<radius>")?;
        writeln!(w)?;
        for (i, m) in self.markers.iter().enumerate() {
            writeln!(w, ";MARKER {i}")?;
            writeln!(w, "{}", m.name)?;
            writeln!(w, "{}", m.node_index)?;
            write_floats(w, &m.rotation.to_array())?;
            write_floats(w, &m.translation.to_array())?;
            write_floats(w, &[m.radius])?;
            writeln!(w)?;
        }

        writeln!(w, ";### INSTANCE XREF PATHS ###")?;
        writeln!(w, "0")?;
        writeln!(w, ";\t<path>")?;
        writeln!(w, ";\t<name>")?;
        writeln!(w)?;

        writeln!(w, ";### INSTANCE MARKERS ###")?;
        writeln!(w, "0")?;
        writeln!(w, ";\t<name>")?;
        writeln!(w, ";\t<unique identifier>")?;
        writeln!(w, ";\t<path index>")?;
        writeln!(w, ";\t<rotation <i,j,k,w>>")?;
        writeln!(w, ";\t<translation <x,y,z>>")?;
        writeln!(w)?;

        writeln!(w, ";### VERTICES ###")?;
        writeln!(w, "{}", self.vertices.len())?;
        writeln!(w, ";\t<position>")?;
        writeln!(w, ";\t<normal>")?;
        writeln!(w, ";\t<node influences count>")?;
        writeln!(w, ";\t\t<node influences <index, weight>>")?;
        writeln!(w, ";\t\t<...>")?;
        writeln!(w, ";\t<texture coordinate count>")?;
        writeln!(w, ";\t\t<texture coordinates <u,v>>")?;
        writeln!(w, ";\t\t<...>")?;
        if has_vertex_color {
            writeln!(w, ";\t\t<vertex color <r,g,b>>")?;
            writeln!(w, ";\t\t<...>")?;
        }
        writeln!(w)?;
        for (i, v) in self.vertices.iter().enumerate() {
            writeln!(w, ";VERTEX {i}")?;
            write_floats(w, &v.position.to_array())?;
            write_floats(w, &v.normal.to_array())?;
            writeln!(w, "{}", v.node_sets.len())?;
            for (idx, wt) in &v.node_sets {
                writeln!(w, "{}", idx)?;
                write_floats(w, &[*wt])?;
            }
            writeln!(w, "{}", v.uvs.len())?;
            for uv in &v.uvs {
                write_floats(w, &uv.to_array())?;
            }
            if has_vertex_color {
                write_floats(w, &[0.0, 0.0, 0.0])?; // vertex color always zero per TagTool
            }
            writeln!(w)?;
        }

        writeln!(w, ";### TRIANGLES ###")?;
        writeln!(w, "{}", self.triangles.len())?;
        writeln!(w, ";\t<material index>")?;
        writeln!(w, ";\t<vertex indices <v0,v1,v2>>")?;
        writeln!(w)?;
        for (i, t) in self.triangles.iter().enumerate() {
            writeln!(w, ";TRIANGLE {i}")?;
            writeln!(w, "{}", t.material)?;
            writeln!(w, "{}\t{}\t{}", t.v[0], t.v[1], t.v[2])?;
            writeln!(w)?;
        }

        // Phmo / coll trailing sections. Section headers + helper
        // comment lines mirror the embedded source JMS layout
        // exactly so byte diffs stay focused on data. Sections that
        // we don't currently populate (CAR_WHEEL, POINT_TO_POINT,
        // PRISMATIC, BOUNDING SPHERE, SKYLIGHT) emit empty.

        writeln!(w, ";### SPHERES ###")?;
        writeln!(w, "{}", self.spheres.len())?;
        for h in ["<name>", "<parent>", "<material>", "<rotation <i,j,k,w>>", "<translation <x,y,z>>", "<radius>"] {
            writeln!(w, ";\t{h}")?;
        }
        writeln!(w)?;
        for (i, s) in self.spheres.iter().enumerate() {
            writeln!(w, ";SPHERE {i}")?;
            writeln!(w, "{}", s.name)?;
            writeln!(w, "{}", s.parent)?;
            writeln!(w, "{}", s.material)?;
            write_floats(w, &s.rotation.to_array())?;
            write_floats(w, &s.translation.to_array())?;
            write_floats(w, &[s.radius])?;
            writeln!(w)?;
        }

        writeln!(w, ";### BOXES ###")?;
        writeln!(w, "{}", self.boxes.len())?;
        for h in ["<name>", "<parent>", "<material>", "<rotation <i,j,k,w>>", "<translation <x,y,z>>", "<width (x)>", "<length (y)>", "<height (z)>"] {
            writeln!(w, ";\t{h}")?;
        }
        writeln!(w)?;
        for (i, b) in self.boxes.iter().enumerate() {
            writeln!(w, ";BOX {i}")?;
            writeln!(w, "{}", b.name)?;
            writeln!(w, "{}", b.parent)?;
            writeln!(w, "{}", b.material)?;
            write_floats(w, &b.rotation.to_array())?;
            write_floats(w, &b.translation.to_array())?;
            write_floats(w, &[b.width])?;
            write_floats(w, &[b.length])?;
            write_floats(w, &[b.height])?;
            writeln!(w)?;
        }

        writeln!(w, ";### CAPSULES ###")?;
        writeln!(w, "{}", self.capsules.len())?;
        for h in ["<name>", "<parent>", "<material>", "<rotation <i,j,k,w>>", "<translation <x,y,z>>", "<height>", "<radius>"] {
            writeln!(w, ";\t{h}")?;
        }
        writeln!(w)?;
        for (i, c) in self.capsules.iter().enumerate() {
            writeln!(w, ";CAPSULE {i}")?;
            writeln!(w, "{}", c.name)?;
            writeln!(w, "{}", c.parent)?;
            writeln!(w, "{}", c.material)?;
            write_floats(w, &c.rotation.to_array())?;
            write_floats(w, &c.translation.to_array())?;
            write_floats(w, &[c.height])?;
            write_floats(w, &[c.radius])?;
            writeln!(w)?;
        }

        writeln!(w, ";### CONVEX SHAPES ###")?;
        writeln!(w, "{}", self.convex_shapes.len())?;
        // The 8213 source variant we observed (masterchief_ragdoll.jms)
        // omits the "height" field that 8207 carried — emit
        // name/parent/material/rotation/translation/vertex_count then
        // the vertex list directly.
        for h in ["<name>", "<parent>", "<material>", "<rotation <i,j,k,w>>", "<translation <x,y,z>>", "<vertex count>", "<...vertices>"] {
            writeln!(w, ";\t{h}")?;
        }
        writeln!(w)?;
        for (i, c) in self.convex_shapes.iter().enumerate() {
            writeln!(w, ";CONVEX SHAPE {i}")?;
            writeln!(w, "{}", c.name)?;
            writeln!(w, "{}", c.parent)?;
            writeln!(w, "{}", c.material)?;
            write_floats(w, &c.rotation.to_array())?;
            write_floats(w, &c.translation.to_array())?;
            writeln!(w, "{}", c.vertices.len())?;
            for v in &c.vertices {
                write_floats(w, &v.to_array())?;
            }
            writeln!(w)?;
        }

        // The ragdoll `<friction limit>` (max friction torque) is an 8213
        // (Halo 3) addition — the 8210 (Halo 2) ragdoll format omits it.
        let ragdoll_has_friction = version >= 8213;
        writeln!(w, ";### RAGDOLLS ###")?;
        writeln!(w, "{}", self.ragdolls.len())?;
        {
            let mut headers: Vec<&str> = vec!["<name>", "<attached index>", "<referenced index>", "<attached transform>", "<reference transform>", "<min twist>", "<max twist>", "<min cone>", "<max cone>", "<min plane>", "<max plane>"];
            if ragdoll_has_friction { headers.push("<friction limit>"); }
            for h in headers { writeln!(w, ";\t{h}")?; }
        }
        writeln!(w)?;
        for (i, r) in self.ragdolls.iter().enumerate() {
            writeln!(w, ";RAGDOLL {i}")?;
            writeln!(w, "{}", r.name)?;
            writeln!(w, "{}", r.attached)?;
            writeln!(w, "{}", r.referenced)?;
            write_floats(w, &r.attached_rotation.to_array())?;
            write_floats(w, &r.attached_translation.to_array())?;
            write_floats(w, &r.referenced_rotation.to_array())?;
            write_floats(w, &r.referenced_translation.to_array())?;
            write_floats(w, &[r.min_twist])?;
            write_floats(w, &[r.max_twist])?;
            write_floats(w, &[r.min_cone])?;
            write_floats(w, &[r.max_cone])?;
            write_floats(w, &[r.min_plane])?;
            write_floats(w, &[r.max_plane])?;
            if ragdoll_has_friction {
                write_floats(w, &[r.friction_limit])?;
            }
            writeln!(w)?;
        }

        writeln!(w, ";### HINGES ###")?;
        writeln!(w, "{}", self.hinges.len())?;
        for h in ["<name>", "<body A index>", "<body B index>", "<body A transform>", "<body B transform>", "<is limited>", "<friction limit>", "<min angle>", "<max angle"] {
            writeln!(w, ";\t{h}")?;
        }
        writeln!(w)?;
        for (i, h) in self.hinges.iter().enumerate() {
            writeln!(w, ";HINGE {i}")?;
            writeln!(w, "{}", h.name)?;
            writeln!(w, "{}", h.body_a)?;
            writeln!(w, "{}", h.body_b)?;
            write_floats(w, &h.a_rotation.to_array())?;
            write_floats(w, &h.a_translation.to_array())?;
            write_floats(w, &h.b_rotation.to_array())?;
            write_floats(w, &h.b_translation.to_array())?;
            writeln!(w, "{}", h.is_limited)?;
            write_floats(w, &[h.friction_limit])?;
            write_floats(w, &[h.min_angle])?;
            write_floats(w, &[h.max_angle])?;
            writeln!(w)?;
        }

        // Sections we don't currently populate stay empty. SKYLIGHT is
        // a Halo 3 (8213) addition — omit it for older versions.
        for (name, helps) in EMPTY_SECTIONS_TRAILING {
            if *name == "SKYLIGHT" && version < 8213 {
                continue;
            }
            writeln!(w, ";### {name} ###")?;
            writeln!(w, "0")?;
            for h in *helps { writeln!(w, ";\t{h}")?; }
            writeln!(w)?;
        }
        writeln!(w)?;
        Ok(())
    }
}

//================================================================================
// Node / material / marker / geometry walkers
//================================================================================

/// Read a 3-component field that may be declared as either
/// `real_point_3d` or `real_vector_3d` (the classic engines differ from
/// gen3+ on several geometry fields).
fn read_point_or_vec(s: &TagStruct<'_>, name: &str) -> RealPoint3d {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealPoint3d(p)) => p,
        Some(TagFieldData::RealVector3d(v)) => RealPoint3d { x: v.i, y: v.j, z: v.k },
        _ => RealPoint3d::ZERO,
    }
}

/// Derive the (first-child, next-sibling) index pair for each node from
/// the flat parent links — the form the old 8197 JMS node section uses.
/// `-1` where there is no child / sibling.
fn derive_child_sibling(nodes: &[JmsNode]) -> (Vec<i32>, Vec<i32>) {
    let n = nodes.len();
    let mut child = vec![-1i32; n];
    let mut sibling = vec![-1i32; n];
    for i in 0..n {
        // First child: the first node whose parent is `i`.
        for j in 0..n {
            if nodes[j].parent as i32 == i as i32 {
                child[i] = j as i32;
                break;
            }
        }
        // Next sibling: the next node after `i` sharing its parent.
        let p = nodes[i].parent;
        for j in (i + 1)..n {
            if nodes[j].parent == p {
                sibling[i] = j as i32;
                break;
            }
        }
    }
    (child, sibling)
}

fn read_nodes(root: &TagStruct<'_>) -> Result<Vec<JmsNode>, JmsError> {
    let block = root.field_path("nodes").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("nodes"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        out.push(JmsNode {
            // H2/H3 store the node name as a string_id; Halo CE uses a
            // 32-byte inline `string` — accept either.
            name: n.read_string_id("name").or_else(|| n.read_string("name")).unwrap_or_default(),
            parent: n.read_block_index("parent node"),
            rotation: n.read_quat("default rotation"),
            // H2/H3 declare `default translation` as real_point_3d; Halo CE
            // as real_vector_3d — accept either.
            translation: read_point_or_vec(&n, "default translation") * SCALE,
        });
    }
    Ok(out)
}

/// Convert per-node local transforms (parent-relative, as the tag
/// stores them) to world transforms (root-relative, as JMS expects).
/// Forward-iteration works because the tag stores nodes
/// parent-before-child. Mirrors Foundry's
/// `RenderArmature.{create_bone, parent_bone}` matrix chain in
/// `connected_geometry.py:621-645`, just expressed with quaternions
/// directly instead of via 4×4 matrices: same composition rule
/// `world = parent_world * local`.
fn chain_local_to_world(local: &[JmsNode]) -> Vec<JmsNode> {
    let mut out: Vec<JmsNode> = Vec::with_capacity(local.len());
    for (i, n) in local.iter().enumerate() {
        let world = if n.parent < 0 || (n.parent as usize) >= i {
            // Root or forward reference (shouldn't happen in
            // well-formed tags) — treat as already-world.
            n.clone()
        } else {
            let parent = &out[n.parent as usize];
            JmsNode {
                name: n.name.clone(),
                parent: n.parent,
                rotation: parent.rotation * n.rotation,
                translation: parent.translation + parent.rotation * n.translation.as_vector(),
            }
        };
        out.push(world);
    }
    out
}

//================================================================================
// collision_model walkers
//================================================================================

//================================================================================
// physics_model walkers
//================================================================================

/// Read the physics_model nodes block (parallel structure to
/// render_model nodes — same `name`/`parent`/`sibling`/`child` shape).
/// JMS stores nodes as world-space bind pose, but the physics_model
/// nodes block has only names + tree links (no transforms), so we
/// emit them with identity transforms; bones are placed by the
/// caller's render_model when combining into a model.
fn read_phmo_nodes(root: &TagStruct<'_>) -> Result<Vec<JmsNode>, JmsError> {
    let block = root.field_path("nodes").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("nodes"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        out.push(JmsNode {
            name: n.read_string_id("name").unwrap_or_default(),
            parent: n.read_block_index("parent"),
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
        });
    }
    Ok(out)
}

fn read_phmo_materials(root: &TagStruct<'_>) -> Result<Vec<JmsMaterial>, JmsError> {
    let block = root.field_path("materials").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("materials"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        let name = m.read_string_id("name").unwrap_or_default();
        // physics_model materials carry a separate `global material
        // name` but the JMS material_name slot is the same name,
        // matching TagTool's 1:1 copy.
        out.push(JmsMaterial {
            name: name.clone(),
            material_name: name,
        });
    }
    Ok(out)
}

/// Each rigid_body references one shape via `(shape_type, shape)`.
/// Build a map keyed by (shape_type_value, shape_index) → node_index
/// so the per-shape walks can attach each shape to the right node.
/// Shape-type enum values verified by inspecting H3 phmo tags:
/// 0=sphere, 1=pill (=capsule), 2=box, 3=triangle, 4=polyhedron,
/// (higher values for multi-sphere/list/mopp not yet seen). See
/// `SHAPE_TYPE_*` constants below.
fn build_phmo_parent_lookup(root: &TagStruct<'_>) -> std::collections::HashMap<(i64, i64), i32> {
    let mut out = std::collections::HashMap::new();
    let Some(rbs) = root.field_path("rigid bodies").and_then(|f| f.as_block()) else { return out; };
    for i in 0..rbs.len() {
        let rb = rbs.element(i).unwrap();
        let node_idx = rb.read_int_any("node").map(|v| v as i32).unwrap_or(-1);
        let Some(sr) = rb.field("shape reference").and_then(|f| f.as_struct()) else { continue; };
        let Some(shape_type) = sr.read_int_any("shape type") else { continue; };
        let Some(shape_idx) = sr.read_int_any("shape") else { continue; };
        out.insert((shape_type as i64, shape_idx as i64), node_idx);
    }
    out
}

fn parent_for(parent_lookup: &std::collections::HashMap<(i64, i64), i32>, shape_type: i64, idx: usize) -> i32 {
    parent_lookup.get(&(shape_type, idx as i64)).copied().unwrap_or(-1)
}

const SHAPE_TYPE_SPHERE: i64 = 0;
const SHAPE_TYPE_PILL: i64 = 1;
const SHAPE_TYPE_BOX: i64 = 2;
const SHAPE_TYPE_POLYHEDRON: i64 = 4;

fn read_phmo_spheres(root: &TagStruct<'_>, parents: &std::collections::HashMap<(i64, i64), i32>) -> Vec<JmsSphere> {
    let Some(block) = root.field_path("spheres").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let s = block.element(i).unwrap();
        let base = match s.field("base").and_then(|f| f.as_struct()) { Some(b) => b, None => continue };
        // Sphere has no per-shape rotation/translation — TagTool
        // outputs identity. Radius lives at `sphere/radius` (sibling
        // to `base`).
        out.push(JmsSphere {
            name: base.read_string_id("name").unwrap_or_default(),
            parent: parent_for(parents, SHAPE_TYPE_SPHERE, i),
            material: base.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
            radius: s.read_real("radius").unwrap_or(0.0) * SCALE,
        });
    }
    out
}

fn read_phmo_boxes(root: &TagStruct<'_>, parents: &std::collections::HashMap<(i64, i64), i32>) -> Vec<JmsBox> {
    let Some(block) = root.field_path("boxes").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let b = block.element(i).unwrap();
        let base = match b.field("base").and_then(|f| f.as_struct()) { Some(s) => s, None => continue };
        let cts = match b.field("convex transform shape").and_then(|f| f.as_struct()) { Some(c) => c, None => continue };
        // The box-specific half_extents lives at top-level on the
        // box block (sibling to `box shape`), as `half extents` —
        // 3-vec3 in world units. The Havok convex skin-width radius
        // is at `box shape/radius` and the source JMS adds it to
        // every half-extent before doubling: each face of the box
        // grows by one radius (typically 0.0164 wu = 1.64cm = the
        // standard Halo convex radius). JMS dimension formula:
        //   side = (half_extent + radius) × 2 × 100
        let half = b.read_vec3("half extents");
        let convex_radius = b.field("box shape").and_then(|f| f.as_struct())
            .and_then(|bs| bs.read_real("radius"))
            .unwrap_or(0.0);
        out.push(JmsBox {
            name: base.read_string_id("name").unwrap_or_default(),
            parent: parent_for(parents, SHAPE_TYPE_BOX, i),
            material: base.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: rotation_from_basis(&cts),
            translation: cts.read_vec3("translation").as_point() * SCALE,
            width:  (half.i + convex_radius) * 2.0 * SCALE,
            length: (half.j + convex_radius) * 2.0 * SCALE,
            height: (half.k + convex_radius) * 2.0 * SCALE,
        });
    }
    out
}

fn read_phmo_pills(root: &TagStruct<'_>, parents: &std::collections::HashMap<(i64, i64), i32>) -> Vec<JmsCapsule> {
    let Some(block) = root.field_path("pills").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let p = block.element(i).unwrap();
        let base = match p.field("base").and_then(|f| f.as_struct()) { Some(s) => s, None => continue };
        // Pill radius lives at `capsule shape/radius` (sibling to
        // `capsule shape/base`, which is a generic Havok shape base).
        let radius = p.field("capsule shape").and_then(|f| f.as_struct())
            .and_then(|cs| cs.read_real("radius"))
            .unwrap_or(0.0);
        let bottom = p.read_vec3("bottom");
        let top = p.read_vec3("top");
        // TagTool pill anchor: translation = bottom + normalized(bottom - top) * radius
        let dir = bottom - top;
        let unit = dir.normalized();
        let anchor = bottom + unit * radius;
        let height = (top - bottom).length() * SCALE;
        // Orientation from the `top - bottom` axis (TagTool's
        // `QuaternionFromVector` with reference up = (0, 0, -1)).
        let axis = top - bottom;
        let rot = RealQuaternion::shortest_arc(
            RealVector3d { i: 0.0, j: 0.0, k: -1.0 },
            axis,
        );
        out.push(JmsCapsule {
            name: base.read_string_id("name").unwrap_or_default(),
            parent: parent_for(parents, SHAPE_TYPE_PILL, i),
            material: base.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: rot,
            translation: anchor.as_point() * SCALE,
            height,
            radius: radius * SCALE,
        });
    }
    out
}

fn read_phmo_polyhedra(root: &TagStruct<'_>, parents: &std::collections::HashMap<(i64, i64), i32>) -> Vec<JmsConvex> {
    let Some(block) = root.field_path("polyhedra").and_then(|f| f.as_block()) else { return Vec::new(); };
    let four_vectors = root.field_path("polyhedron four vectors").and_then(|f| f.as_block());
    let mut out = Vec::with_capacity(block.len());
    let mut fv_offset: usize = 0;
    for i in 0..block.len() {
        let p = block.element(i).unwrap();
        let base = match p.field("base").and_then(|f| f.as_struct()) { Some(s) => s, None => continue };
        // `four vectors size` is at the polyhedron top level, not
        // inside `polyhedron shape` (which only carries base + radius).
        let fv_size = p.read_int_any("four vectors size").unwrap_or(0) as usize;
        let mut verts: Vec<RealPoint3d> = Vec::new();
        if let Some(fvb) = &four_vectors {
            for k in 0..fv_size {
                let Some(fv) = fvb.element(fv_offset + k) else { continue };
                let xv = fv.read_vec3("four vectors x");
                let yv = fv.read_vec3("four vectors y");
                let zv = fv.read_vec3("four vectors z");
                let xw = fv.read_real("havok w four vectors x").unwrap_or(0.0);
                let yw = fv.read_real("havok w four vectors y").unwrap_or(0.0);
                let zw = fv.read_real("havok w four vectors z").unwrap_or(0.0);
                // 4 vertices packed: (x.i, y.i, z.i), (x.j, y.j, z.j),
                // (x.k, y.k, z.k), (x_w, y_w, z_w)
                verts.push(RealPoint3d { x: xv.i, y: yv.i, z: zv.i } * SCALE);
                verts.push(RealPoint3d { x: xv.j, y: yv.j, z: zv.j } * SCALE);
                verts.push(RealPoint3d { x: xv.k, y: yv.k, z: zv.k } * SCALE);
                verts.push(RealPoint3d { x: xw, y: yw, z: zw } * SCALE);
            }
        }
        // Dedupe duplicates (the 4-vector packing left padding when
        // the actual vertex count isn't a multiple of 4).
        let mut seen = std::collections::HashSet::new();
        verts.retain(|v| {
            let key = (v.x.to_bits(), v.y.to_bits(), v.z.to_bits());
            seen.insert(key)
        });
        // Polyhedron transform is identity — vertices are absolute.
        out.push(JmsConvex {
            name: base.read_string_id("name").unwrap_or_default(),
            parent: parent_for(parents, SHAPE_TYPE_POLYHEDRON, i),
            material: base.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
            vertices: verts,
        });
        fv_offset += fv_size;
    }
    out
}

/// Build the `(shape_type, shape_index) → node` map from the H2
/// `rigid bodies` block. Each rigid body carries its node index plus a
/// shape reference our generated def exposes only as an unnamed pointer
/// field — read it straight from the element bytes: `shape_type` is the
/// i16 at +56 and `shape_index` the i16 at +58 (the v1/144-byte rigid
/// body layout). Skips bodies whose element is too short (older v0).
/// Returns `(map, default_node)`. `default_node` is the lone rigid
/// body's node when there is exactly one — for scenery whose shapes are
/// wrapped in a list/mopp (type 14/15), the child shapes are runtime
/// havok pointers not modeled on disk, so the shape-ref map can't reach
/// them; but a single-rigid-body model's shapes all belong to that one
/// node, so it's the correct fallback. Multi-body models leave it `-1`.
fn build_h2_shape_parent_map(root: &TagStruct<'_>) -> (std::collections::HashMap<(i64, i64), i32>, i32) {
    let mut out = std::collections::HashMap::new();
    let Some(rbs) = root.field_path("rigid bodies").and_then(|f| f.as_block()) else { return (out, -1); };
    let mut nodes_seen: std::collections::HashSet<i32> = std::collections::HashSet::new();
    for i in 0..rbs.len() {
        let rb = rbs.element(i).unwrap();
        let node = rb.read_int_any("node").map(|v| v as i32).unwrap_or(-1);
        nodes_seen.insert(node);
        let raw = rb.raw();
        if raw.len() < 60 { continue; }
        let shape_type = i16::from_le_bytes([raw[56], raw[57]]) as i64;
        let shape_index = i16::from_le_bytes([raw[58], raw[59]]) as i64;
        if shape_index >= 0 {
            out.insert((shape_type, shape_index), node);
        }
    }
    let default_node = if nodes_seen.len() == 1 { *nodes_seen.iter().next().unwrap() } else { -1 };
    (out, default_node)
}

/// Parent-node index + name for a Halo 2 shape: prefer the rigid-body
/// shape reference (`(shape_type, index) → node`); fall back to matching
/// the shape's `name` string_id against the node names (works for
/// character ragdolls whose shapes carry bone names).
fn h2_shape_parent(
    s: &TagStruct<'_>,
    shape_type: i64,
    index: usize,
    parent_map: &std::collections::HashMap<(i64, i64), i32>,
    name_to_node: &std::collections::HashMap<String, i32>,
    default_node: i32,
) -> (String, i32) {
    let name = s.read_string_id("name").unwrap_or_default();
    let parent = parent_map.get(&(shape_type, index as i64)).copied()
        .or_else(|| name_to_node.get(&name).copied())
        .unwrap_or(default_node);
    (name, parent)
}

fn read_phmo_h2_spheres(root: &TagStruct<'_>, parent_map: &std::collections::HashMap<(i64, i64), i32>, name_to_node: &std::collections::HashMap<String, i32>, default_node: i32) -> Vec<JmsSphere> {
    let Some(block) = root.field_path("spheres").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let s = block.element(i).unwrap();
        let (name, parent) = h2_shape_parent(&s, SHAPE_TYPE_SPHERE, i, parent_map, name_to_node, default_node);
        out.push(JmsSphere {
            name,
            parent,
            material: s.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: rotation_from_basis(&s),
            translation: s.read_vec3("translation").as_point() * SCALE,
            radius: s.read_real("radius").unwrap_or(0.0) * SCALE,
        });
    }
    out
}

fn read_phmo_h2_boxes(root: &TagStruct<'_>, parent_map: &std::collections::HashMap<(i64, i64), i32>, name_to_node: &std::collections::HashMap<String, i32>, default_node: i32) -> Vec<JmsBox> {
    let Some(block) = root.field_path("boxes").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let b = block.element(i).unwrap();
        let (name, parent) = h2_shape_parent(&b, SHAPE_TYPE_BOX, i, parent_map, name_to_node, default_node);
        // Flat layout: `half extents` + convex skin `radius` + the
        // `rotation i/j/k` + `translation` are all on the box block.
        // Same dimension formula as H3: side = (half + radius) × 2 × 100.
        let half = b.read_vec3("half extents");
        let convex_radius = b.read_real("radius").unwrap_or(0.0);
        out.push(JmsBox {
            name,
            parent,
            material: b.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: rotation_from_basis(&b),
            translation: b.read_vec3("translation").as_point() * SCALE,
            width:  (half.i + convex_radius) * 2.0 * SCALE,
            length: (half.j + convex_radius) * 2.0 * SCALE,
            height: (half.k + convex_radius) * 2.0 * SCALE,
        });
    }
    out
}

fn read_phmo_h2_pills(root: &TagStruct<'_>, parent_map: &std::collections::HashMap<(i64, i64), i32>, name_to_node: &std::collections::HashMap<String, i32>, default_node: i32) -> Vec<JmsCapsule> {
    let Some(block) = root.field_path("pills").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let p = block.element(i).unwrap();
        let (name, parent) = h2_shape_parent(&p, SHAPE_TYPE_PILL, i, parent_map, name_to_node, default_node);
        let radius = p.read_real("radius").unwrap_or(0.0);
        let bottom = p.read_vec3("bottom");
        let top = p.read_vec3("top");
        // Same anchor/orientation math as the H3 pill reader.
        let dir = bottom - top;
        let anchor = bottom + dir.normalized() * radius;
        let height = (top - bottom).length() * SCALE;
        let rot = RealQuaternion::shortest_arc(
            RealVector3d { i: 0.0, j: 0.0, k: -1.0 },
            top - bottom,
        );
        out.push(JmsCapsule {
            name,
            parent,
            material: p.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: rot,
            translation: anchor.as_point() * SCALE,
            height,
            radius: radius * SCALE,
        });
    }
    out
}

fn read_phmo_h2_polyhedra(root: &TagStruct<'_>, parent_map: &std::collections::HashMap<(i64, i64), i32>, name_to_node: &std::collections::HashMap<String, i32>, default_node: i32) -> Vec<JmsConvex> {
    let Some(block) = root.field_path("polyhedra").and_then(|f| f.as_block()) else { return Vec::new(); };
    // H2 keeps the packed Havok four-vectors in the top-level
    // `polyhedron four vectors` block (like H3), each polyhedron's
    // `four vectors size` advancing a running offset. NOTE: H2 only
    // names the x/y/z vec3 of each four-vector group — the 4th packed
    // vertex (the Havok `w` lane) sits in unnamed skip bytes, so we
    // recover 3 of every 4 hull vertices. Polyhedra are rare in phmo
    // (most shapes are sphere/pill/box); the convex hull is slightly
    // under-sampled but positioned correctly.
    let four_vectors = root.field_path("polyhedron four vectors").and_then(|f| f.as_block());
    let mut out = Vec::with_capacity(block.len());
    let mut fv_offset: usize = 0;
    for i in 0..block.len() {
        let p = block.element(i).unwrap();
        let (name, parent) = h2_shape_parent(&p, SHAPE_TYPE_POLYHEDRON, i, parent_map, name_to_node, default_node);
        let fv_size = p.read_int_any("four vectors size").unwrap_or(0).max(0) as usize;
        let mut verts: Vec<RealPoint3d> = Vec::new();
        if let Some(fvb) = &four_vectors {
            for k in 0..fv_size {
                let Some(fv) = fvb.element(fv_offset + k) else { continue };
                let xv = fv.read_vec3("four vectors x");
                let yv = fv.read_vec3("four vectors y");
                let zv = fv.read_vec3("four vectors z");
                verts.push(RealPoint3d { x: xv.i, y: yv.i, z: zv.i } * SCALE);
                verts.push(RealPoint3d { x: xv.j, y: yv.j, z: zv.j } * SCALE);
                verts.push(RealPoint3d { x: xv.k, y: yv.k, z: zv.k } * SCALE);
            }
        }
        let mut seen = std::collections::HashSet::new();
        verts.retain(|v| seen.insert((v.x.to_bits(), v.y.to_bits(), v.z.to_bits())));
        out.push(JmsConvex {
            name,
            parent,
            material: p.read_int_any("material").map(|v| v as i32).unwrap_or(0),
            rotation: RealQuaternion::IDENTITY,
            translation: RealPoint3d::ZERO,
            vertices: verts,
        });
        fv_offset += fv_size;
    }
    out
}

fn read_phmo_ragdolls(root: &TagStruct<'_>) -> Vec<JmsRagdoll> {
    let Some(block) = root.field_path("ragdoll constraints").and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let r = block.element(i).unwrap();
        let bodies = match r.field("constraint bodies").and_then(|f| f.as_struct()) { Some(b) => b, None => continue };
        let (a_rot, a_trans) = constraint_frame(&bodies, "a");
        let (b_rot, b_trans) = constraint_frame(&bodies, "b");
        out.push(JmsRagdoll {
            name: bodies.read_string_id("name").unwrap_or_default(),
            attached: bodies.read_int_any("node a").map(|v| v as i32).unwrap_or(-1),
            referenced: bodies.read_int_any("node b").map(|v| v as i32).unwrap_or(-1),
            // TagTool negates the ragdoll-derived quat — verified
            // against the masterchief embedded source: e.g. b_head's
            // tag matrix gives q=(0.6995, 0.1043, 0.1043, 0.6995),
            // source has (-0.6995, -0.1043, -0.1043, -0.6995).
            attached_rotation: -a_rot,
            attached_translation: a_trans,
            referenced_rotation: -b_rot,
            referenced_translation: b_trans,
            min_twist: r.read_real("min twist").unwrap_or(0.0),
            max_twist: r.read_real("max twist").unwrap_or(0.0),
            min_cone: r.read_real("min cone").unwrap_or(0.0),
            max_cone: r.read_real("max cone").unwrap_or(0.0),
            min_plane: r.read_real("min plane").unwrap_or(0.0),
            max_plane: r.read_real("max plane").unwrap_or(0.0),
            // The schema field carries a typo in MCC — `max friciton torque`.
            friction_limit: r.read_real("max friciton torque")
                .or_else(|| r.read_real("max friction torque"))
                .unwrap_or(0.0),
        });
    }
    out
}

fn read_phmo_hinges(root: &TagStruct<'_>, limited: bool) -> Vec<JmsHinge> {
    let block_name = if limited { "limited hinge constraints" } else { "hinge constraints" };
    let Some(block) = root.field_path(block_name).and_then(|f| f.as_block()) else { return Vec::new(); };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let h = block.element(i).unwrap();
        let bodies = match h.field("constraint bodies").and_then(|f| f.as_struct()) { Some(b) => b, None => continue };
        let (a_rot, a_trans) = constraint_frame(&bodies, "a");
        let (b_rot, b_trans) = constraint_frame(&bodies, "b");
        out.push(JmsHinge {
            name: bodies.read_string_id("name").unwrap_or_default(),
            body_a: bodies.read_int_any("node a").map(|v| v as i32).unwrap_or(-1),
            body_b: bodies.read_int_any("node b").map(|v| v as i32).unwrap_or(-1),
            // Hinges (per TagTool) are NOT negated — only ragdolls.
            a_rotation: a_rot,
            a_translation: a_trans,
            b_rotation: b_rot,
            b_translation: b_trans,
            is_limited: if limited { 1 } else { 0 },
            friction_limit: h.read_real("limit friction").unwrap_or(0.0),
            min_angle: h.read_real("limit min angle").unwrap_or(0.0),
            max_angle: h.read_real("limit max angle").unwrap_or(0.0),
        });
    }
    out
}

/// Build (rotation_quat, translation) from a constraint frame's
/// `<side> forward / left / up / position` vectors. Side is `"a"` or
/// `"b"`. Matches Foundry's column-major construction
/// (connected_geometry.py:689-694): forward in column 0, left in
/// column 1, up in column 2.
fn constraint_frame(bodies: &TagStruct<'_>, side: &str) -> (RealQuaternion, RealPoint3d) {
    // Schema: forward/left/up are `real_vector_3d`, position is `real_point_3d`.
    let f = bodies.read_vec3(&format!("{side} forward"));
    let l = bodies.read_vec3(&format!("{side} left"));
    let u = bodies.read_vec3(&format!("{side} up"));
    let p = bodies.read_point3d(&format!("{side} position"));
    let rot = RealQuaternion::from_basis_columns(f, l, u);
    (rot, p * SCALE)
}

/// Build a quaternion from a `convex transform shape` struct's
/// rotation_i/j/k row vectors (Havok stores rotation as 3 vec3 rows).
fn rotation_from_basis(cts: &TagStruct<'_>) -> RealQuaternion {
    let row_i = cts.read_vec3("rotation i");
    let row_j = cts.read_vec3("rotation j");
    let row_k = cts.read_vec3("rotation k");
    // Rows form the rotation matrix; columns are forward/left/up.
    RealQuaternion::from_basis_columns(
        RealVector3d { i: row_i.i, j: row_j.i, k: row_k.i },
        RealVector3d { i: row_i.j, j: row_j.j, k: row_k.j },
        RealVector3d { i: row_i.k, j: row_j.k, k: row_k.k },
    )
}

fn read_markers(root: &TagStruct<'_>) -> Result<Vec<JmsMarker>, JmsError> {
    let block = root.field_path("marker groups").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("marker groups"))?;
    let mut out = Vec::new();
    for i in 0..block.len() {
        let g = block.element(i).unwrap();
        let group_name = g.read_string_id("name").unwrap_or_default();
        let inner = match g.field("markers").and_then(|f| f.as_block()) {
            Some(b) => b, None => continue,
        };
        for j in 0..inner.len() {
            let m = inner.element(j).unwrap();
            out.push(JmsMarker {
                name: group_name.clone(),
                node_index: m.read_int_any("node index").unwrap_or(-1) as i16,
                rotation: m.read_quat("rotation"),
                translation: m.read_point3d("translation") * SCALE,
                radius: -1.0,
            });
        }
    }
    Ok(out)
}

/// Region × permutation walker that builds:
/// - the JMS material list (one per unique `(shader, perm-region)` cell)
/// - a `(mesh_index, part_index) → jms_material_index` lookup
/// - the mesh-emit order (only meshes referenced by some `(region, perm)`)
fn build_materials(root: &TagStruct<'_>)
    -> Result<(Vec<JmsMaterial>, HashMap<(usize, usize), i32>, Vec<usize>), JmsError>
{
    let mats_block = root.field_path("materials").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("materials"))?;
    let regions_block = root.field_path("regions").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("regions"))?;
    let meshes = root.field_path("render geometry/meshes").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("render geometry/meshes"))?;

    let mut materials: Vec<JmsMaterial> = Vec::new();
    let mut part_material_map: HashMap<(usize, usize), i32> = HashMap::new();
    let mut mesh_emit_order: Vec<usize> = Vec::new();

    for ri in 0..regions_block.len() {
        let region = regions_block.element(ri).unwrap();
        let region_name = region.read_string_id("name").unwrap_or_default();
        let perms = match region.field("permutations").and_then(|f| f.as_block()) {
            Some(b) => b, None => continue,
        };
        for pi in 0..perms.len() {
            let perm = perms.element(pi).unwrap();
            let perm_name = perm.read_string_id("name").unwrap_or_default();
            let mesh_idx = perm.read_int_any("mesh index").unwrap_or(-1);
            let mesh_count = perm.read_int_any("mesh count").unwrap_or(0);
            if mesh_idx < 0 || mesh_count <= 0 { continue; }
            for mi_off in 0..mesh_count as usize {
                let mi = mesh_idx as usize + mi_off;
                if mi >= meshes.len() { continue; }
                if !mesh_emit_order.contains(&mi) {
                    mesh_emit_order.push(mi);
                }
                let mesh = meshes.element(mi).unwrap();
                let parts = match mesh.field("parts").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                for part_i in 0..parts.len() {
                    let part = parts.element(part_i).unwrap();
                    let shader_idx = part.read_int_any("render method index").unwrap_or(0);
                    let shader_name = if shader_idx >= 0 && (shader_idx as usize) < mats_block.len() {
                        let m = mats_block.element(shader_idx as usize).unwrap();
                        let path = m.read_tag_ref_path("render method").unwrap_or_default();
                        Path::new(&path.replace('\\', "/"))
                            .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned()
                    } else {
                        "default".to_owned()
                    };
                    let cell_label = format!("{} {}", perm_name, region_name);
                    let jms_idx = match materials.iter().position(|m|
                        m.name == shader_name && m.material_name.ends_with(&cell_label)
                    ) {
                        Some(idx) => idx as i32,
                        None => {
                            let slot = materials.len() + 1;
                            materials.push(JmsMaterial {
                                name: shader_name,
                                material_name: format!("({}) {}", slot, cell_label),
                            });
                            (materials.len() - 1) as i32
                        }
                    };
                    part_material_map.insert((mi, part_i), jms_idx);
                }
            }
        }
    }
    Ok((materials, part_material_map, mesh_emit_order))
}

fn build_geometry(
    root: &TagStruct<'_>,
    part_material_map: &HashMap<(usize, usize), i32>,
    mesh_emit_order: &[usize],
    bounds: &CompressionBounds,
) -> Result<(Vec<JmsVertex>, Vec<JmsTriangle>), JmsError> {
    let pmt_block = root.field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("render geometry/per mesh temporary"))?;
    let meshes_block = root.field_path("render geometry/meshes")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("render geometry/meshes"))?;

    let mut vertices: Vec<JmsVertex> = Vec::new();
    let mut triangles: Vec<JmsTriangle> = Vec::new();

    for &mi in mesh_emit_order {
        if mi >= pmt_block.len() { continue; }
        let pmt = pmt_block.element(mi).unwrap();
        let mesh = meshes_block.element(mi).unwrap();

        // Defensive rigid fallback (see crate-level doc note).
        let vt = mesh.field("vertex type").and_then(|f| f.value()).map(|v| match v {
            TagFieldData::CharEnum { value, .. } => value as i32, _ => -1,
        }).unwrap_or(-1);
        let rigid_fallback_node = if matches!(vt, 1 | 5) {
            mesh.read_int_any("rigid node index").map(|v| v as i16).filter(|&v| v >= 0)
        } else { None };

        let raw_v = pmt.field("raw vertices").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("per mesh temporary[i]/raw vertices"))?;
        // `raw indices` is u16; `raw indices32` is the parallel u32
        // slot used by meshes too big to address with 16-bit indices
        // (e.g. bigmuthafucka with 103k unique vertices). Read whichever
        // is populated, widen both to u32 — the JMS output side uses
        // u32 vertex indices already (`JmsTriangle.v: [u32; 3]`), so
        // there's no downstream truncation concern.
        let raw_i_u16 = pmt.field("raw indices").and_then(|f| f.as_block());
        let raw_i_u32 = pmt.field("raw indices32").and_then(|f| f.as_block());
        let raw_u16_len = raw_i_u16.as_ref().map(|b| b.len()).unwrap_or(0);
        let raw_u32_len = raw_i_u32.as_ref().map(|b| b.len()).unwrap_or(0);
        let indices: Vec<u32> = if raw_u16_len > 0 {
            let raw_i = raw_i_u16.unwrap();
            (0..raw_i.len())
                .filter_map(|k| raw_i.element(k))
                .map(|e| e.read_int_any("word").unwrap_or(0) as u32 & 0xFFFF)
                .collect()
        } else if raw_u32_len > 0 {
            let raw_i = raw_i_u32.unwrap();
            (0..raw_i.len())
                .filter_map(|k| raw_i.element(k))
                .map(|e| e.read_int_any("dword").unwrap_or(0) as u32)
                .collect()
        } else {
            return Err(JmsError::MissingField("per mesh temporary[i]/raw indices"));
        };

        // Default to "triangle strip" — what every MCC render mesh
        // observed uses. The schema enum value 5 = triangle strip.
        let is_strip = mesh.field("index buffer type")
            .and_then(|f| f.value())
            .map(|v| matches!(v, TagFieldData::CharEnum { name: Some(n), .. } if n == "triangle strip"))
            .unwrap_or(true);

        let parts = mesh.field("parts").and_then(|f| f.as_block())
            .ok_or(JmsError::MissingField("meshes[i]/parts"))?;
        for pi in 0..parts.len() {
            let part = parts.element(pi).unwrap();
            let material_index = part_material_map.get(&(mi, pi)).copied().unwrap_or(0);
            // `index start` / `index count` field types differ per
            // engine: H3 declares them as `short_integer` (signed
            // i16, where values >32767 wrap to negative); H4 widened
            // them to `long_integer` (signed i32, no wrap below 2^31).
            // If the raw value is negative, fall back to the H3 u16
            // wrap; otherwise use it directly. This handles both
            // builds without per-engine branching.
            let start_i = part.read_int_any("index start").unwrap_or(0);
            let count_i = part.read_int_any("index count").unwrap_or(0);
            if count_i <= 0 { continue; }
            let start = if start_i < 0 {
                (start_i as i16 as u16) as usize
            } else {
                start_i as usize
            };
            let count = count_i as usize;
            if start >= indices.len() { continue; }
            let end = (start + count).min(indices.len());
            let part_indices = &indices[start..end];

            let tris: Vec<(u32, u32, u32)> = if is_strip {
                strip_to_list_u32(part_indices)
            } else {
                part_indices.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect()
            };

            for (a, b, c) in tris {
                let base = vertices.len() as u32;
                for vi in [a, b, c] {
                    let Some(v) = raw_v.element(vi as usize) else { continue; };
                    let mut jv = read_vertex(&v, bounds);
                    if jv.node_sets.is_empty() {
                        if let Some(node) = rigid_fallback_node {
                            jv.node_sets.push((node, 1.0));
                        }
                    }
                    vertices.push(jv);
                }
                triangles.push(JmsTriangle {
                    material: material_index,
                    v: [base, base + 1, base + 2],
                                region: 0,
                });
            }
        }
    }
    Ok((vertices, triangles))
}

/// Walk `instance placements[]` and bake each as additional triangles
/// referencing `meshes[instance_mesh_index].subparts[i]`. No-op when
/// `instance mesh index < 0` or there are no placements.
///
/// Per-placement transform mirrors Foundry's `InstancePlacement.matrix`:
/// the 3×3 rotation has `(forward, left, up)` as columns and `position`
/// as the translation column. `scale` is applied to the vertex before
/// rotation. Vertex weights are overridden to a single bone — the
/// placement's `node_index` — since the runtime engine attaches the
/// instance to that bone rather than the original mesh's skin weights.
///
/// Material naming: each placement gets a unique JMS material slot whose
/// `material_name` is `(slot) <placement_name>`, so they appear as
/// distinct named pieces in Blender. Shader name is inherited from the
/// subpart's referenced `parts[].render method index`.
fn append_instance_geometry(
    root: &TagStruct<'_>,
    materials: &mut Vec<JmsMaterial>,
    vertices: &mut Vec<JmsVertex>,
    triangles: &mut Vec<JmsTriangle>,
    bounds: &CompressionBounds,
) -> Result<(), JmsError> {
    let instance_mesh_index = root.read_int_any("instance mesh index").unwrap_or(-1);
    if instance_mesh_index < 0 { return Ok(()); }
    let instance_mesh_index = instance_mesh_index as usize;

    let placements = match root.field("instance placements").and_then(|f| f.as_block()) {
        Some(b) if !b.is_empty() => b,
        _ => return Ok(()),
    };

    let mats_block = root.field_path("materials").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("materials"))?;
    let meshes_block = root.field_path("render geometry/meshes")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("render geometry/meshes"))?;
    let pmt_block = root.field_path("render geometry/per mesh temporary")
        .and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("render geometry/per mesh temporary"))?;

    if instance_mesh_index >= meshes_block.len() || instance_mesh_index >= pmt_block.len() {
        return Ok(());
    }
    let mesh = meshes_block.element(instance_mesh_index).unwrap();
    let pmt = pmt_block.element(instance_mesh_index).unwrap();

    let raw_v = pmt.field("raw vertices").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("per mesh temporary[i]/raw vertices"))?;
    let raw_i_u16 = pmt.field("raw indices").and_then(|f| f.as_block());
    let raw_i_u32 = pmt.field("raw indices32").and_then(|f| f.as_block());
    let raw_u16_len = raw_i_u16.as_ref().map(|b| b.len()).unwrap_or(0);
    let raw_u32_len = raw_i_u32.as_ref().map(|b| b.len()).unwrap_or(0);
    let indices: Vec<u32> = if raw_u16_len > 0 {
        let raw_i = raw_i_u16.unwrap();
        (0..raw_i.len())
            .filter_map(|k| raw_i.element(k))
            .map(|e| e.read_int_any("word").unwrap_or(0) as u32 & 0xFFFF)
            .collect()
    } else if raw_u32_len > 0 {
        let raw_i = raw_i_u32.unwrap();
        (0..raw_i.len())
            .filter_map(|k| raw_i.element(k))
            .map(|e| e.read_int_any("dword").unwrap_or(0) as u32)
            .collect()
    } else {
        return Err(JmsError::MissingField("per mesh temporary[i]/raw indices"));
    };
    let is_strip = mesh.field("index buffer type")
        .and_then(|f| f.value())
        .map(|v| matches!(v, TagFieldData::CharEnum { name: Some(n), .. } if n == "triangle strip"))
        .unwrap_or(true);

    let parts = mesh.field("parts").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("meshes[i]/parts"))?;
    let subparts = mesh.field("subparts").and_then(|f| f.as_block())
        .ok_or(JmsError::MissingField("meshes[i]/subparts"))?;

    for ii in 0..placements.len() {
        let placement = placements.element(ii).unwrap();
        let name = placement.read_string_id("name").unwrap_or_else(|| format!("instance_{ii}"));
        let node_index = placement.read_int_any("node_index").map(|v| v as i16).unwrap_or(-1);
        let scale = placement.read_real("scale").unwrap_or(1.0);
        let forward = placement.read_vec3("forward");
        let left = placement.read_vec3("left");
        let up = placement.read_vec3("up");
        let position = placement.read_point3d("position") * SCALE;

        // Pair instance i with subpart i. Skip silently if the runtime
        // tag has fewer subparts than placements (defensive — should
        // never happen in practice).
        let subpart = match subparts.element(ii) { Some(s) => s, None => continue };
        let part_index = subpart.read_int_any("part index").unwrap_or(-1);
        let start_i = subpart.read_int_any("index start").unwrap_or(0);
        let count_i = subpart.read_int_any("index count").unwrap_or(0);
        if count_i <= 0 { continue; }
        // H3: short_integer (i16, may wrap negative); H4: long_integer
        // (i32, no wrap < 2^31). See `build_geometry` for the same fix.
        let start = if start_i < 0 {
            (start_i as i16 as u16) as usize
        } else {
            start_i as usize
        };
        let count = count_i as usize;
        if start >= indices.len() { continue; }
        let end = (start + count).min(indices.len());
        let part_indices = &indices[start..end];

        // Resolve the shader name via parts[part_index].render method
        // index. Falls back to "default" so we never lose triangles
        // even on malformed tags.
        let shader_name = if part_index >= 0 && (part_index as usize) < parts.len() {
            let part = parts.element(part_index as usize).unwrap();
            let shader_idx = part.read_int_any("render method index").unwrap_or(0);
            if shader_idx >= 0 && (shader_idx as usize) < mats_block.len() {
                let m = mats_block.element(shader_idx as usize).unwrap();
                let path = m.read_tag_ref_path("render method").unwrap_or_default();
                Path::new(&path.replace('\\', "/"))
                    .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned()
            } else { "default".to_owned() }
        } else { "default".to_owned() };

        let slot = materials.len() + 1;
        let material_index = materials.len() as i32;
        materials.push(JmsMaterial {
            name: shader_name,
            material_name: format!("({}) {}", slot, name),
        });

        let tris: Vec<(u32, u32, u32)> = if is_strip {
            strip_to_list_u32(part_indices)
        } else {
            part_indices.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect()
        };

        for (a, b, c) in tris {
            let base = vertices.len() as u32;
            for vi in [a, b, c] {
                let Some(v) = raw_v.element(vi as usize) else { continue; };
                let mut jv = read_vertex(&v, bounds);
                // Transform vertex by placement basis. Foundry packs
                // `(forward, left, up)` as columns of the 3×3 rotation,
                // i.e. `new = forward*x + left*y + up*z + position`,
                // with the vertex pre-scaled.
                let p = jv.position;
                let sx = p.x * scale; let sy = p.y * scale; let sz = p.z * scale;
                jv.position = crate::math::RealPoint3d {
                    x: forward.i * sx + left.i * sy + up.i * sz + position.x,
                    y: forward.j * sx + left.j * sy + up.j * sz + position.y,
                    z: forward.k * sx + left.k * sy + up.k * sz + position.z,
                };
                let n = jv.normal;
                jv.normal = crate::math::RealVector3d {
                    i: forward.i * n.i + left.i * n.j + up.i * n.k,
                    j: forward.j * n.i + left.j * n.j + up.j * n.k,
                    k: forward.k * n.i + left.k * n.j + up.k * n.k,
                };
                // Override skin weights — instance is rigidly attached
                // to its placement bone, regardless of mesh-N's original
                // multi-bone weights.
                jv.node_sets.clear();
                if node_index >= 0 {
                    jv.node_sets.push((node_index, 1.0));
                }
                vertices.push(jv);
            }
            triangles.push(JmsTriangle {
                material: material_index,
                v: [base, base + 1, base + 2],
                                region: 0,
            });
        }
    }
    Ok(())
}

//================================================================================
// raw_vertex_block reader
//================================================================================

fn read_vertex(v: &TagStruct<'_>, bounds: &CompressionBounds) -> JmsVertex {
    let raw_pos = v.read_point3d("position");
    let position = bounds.decompress_position(raw_pos) * SCALE;
    // The "normal" schema field is `real_point_3d` despite being a
    // direction — JMS exporters treat it as a vector once read.
    let normal = v.read_point3d("normal").as_vector();
    let raw_uv = v.read_point2d("texcoord");
    let texcoord = bounds.decompress_texcoord(raw_uv);
    let mut node_sets = Vec::with_capacity(4);
    if let (Some(idx_arr), Some(wt_arr)) = (
        v.field("node indices").and_then(|f| f.as_array()),
        v.field("node weights").and_then(|f| f.as_array()),
    ) {
        for k in 0..idx_arr.len().min(wt_arr.len()) {
            let idx_e = idx_arr.element(k).unwrap();
            let wt_e = wt_arr.element(k).unwrap();
            // H3 declares the array element as char_integer (signed
            // i8); H4 switched it to byte_integer (unsigned u8). Same
            // wire byte either way — pick whichever variant the
            // schema currently surfaces.
            let idx = idx_e.fields().next().and_then(|f| f.value())
                .and_then(|v| match v {
                    TagFieldData::CharInteger(c) => Some(c as i16),
                    TagFieldData::ByteInteger(b) => Some(b as i16),
                    _ => None,
                })
                .unwrap_or(-1);
            let wt = wt_e.fields().next().and_then(|f| f.value())
                .and_then(|v| if let TagFieldData::Real(r) = v { Some(r) } else { None })
                .unwrap_or(0.0);
            if wt > 0.0 { node_sets.push((idx, wt)); }
        }
    }
    JmsVertex {
        position, normal, node_sets,
        tangent: None, binormal: None,
        uvs: vec![crate::math::RealPoint2d { x: texcoord.x, y: 1.0 - texcoord.y }],
    }
}

/// Read one Halo 2 `raw vertices[]` element into a JMS vertex. Positions
/// and texcoords are already decompressed floats (the per-section
/// compression bounds are vestigial), so no dequantization is applied.
/// Node influences come from the `(NEW)` or `(OLD)` index arrays paired
/// with `node weights`, selected by `use new node indices`; weights of
/// zero are dropped. The caller overrides these for rigid sections.
pub(crate) fn read_h2_vertex(v: &TagStruct<'_>) -> JmsVertex {
    let position = v.read_point3d("position") * SCALE;
    // H2 declares the vertex normal as `real_vector_3d` (Halo 3 used
    // `real_point_3d`).
    let normal = v.read_vec3("normal");
    let uv = v.read_point2d("texcoord");
    let use_new = v.read_int_any("use new node indices").unwrap_or(1) != 0;
    let (idx_field, idx_elem) = if use_new {
        ("node indices (NEW)", "node index (NEW)")
    } else {
        ("node indices (OLD)", "node index (OLD)")
    };
    let mut node_sets = Vec::with_capacity(4);
    if let (Some(ia), Some(wa)) = (
        v.field(idx_field).and_then(|f| f.as_array()),
        v.field("node weights").and_then(|f| f.as_array()),
    ) {
        for k in 0..ia.len().min(wa.len()) {
            let idx = ia.element(k).and_then(|e| e.read_int_any(idx_elem)).unwrap_or(-1) as i16;
            let wt = wa.element(k).and_then(|e| e.read_real("node_weight")).unwrap_or(0.0);
            if wt > 0.0 && idx >= 0 {
                node_sets.push((idx, wt));
            }
        }
    }
    // H2 raw vertices carry the engine's authored tangent-space basis.
    let read_basis = |name: &str| {
        let b = v.read_vec3(name);
        (b.i * b.i + b.j * b.j + b.k * b.k > 0.25).then_some(b)
    };
    JmsVertex {
        position,
        normal,
        tangent: read_basis("tangent"),
        binormal: read_basis("binormal"),
        node_sets,
        uvs: vec![crate::math::RealPoint2d { x: uv.x, y: 1.0 - uv.y }],
    }
}

/// Read one Halo CE `uncompressed vertices[]` element into a JMS vertex.
/// Position is a `real_vector_3d` (Halo 1's convention); node binding is
/// the fixed two-influence `node0/node1` index+weight pair.
fn read_ce_vertex(v: &TagStruct<'_>) -> JmsVertex {
    let p = v.read_vec3("position");
    let position = RealPoint3d { x: p.i, y: p.j, z: p.k } * SCALE;
    let normal = v.read_vec3("normal");
    let uv = v.read_point2d("texture coords");
    let mut node_sets = Vec::with_capacity(2);
    for (idx_f, wt_f) in [("node0 index", "node0 weight"), ("node1 index", "node1 weight")] {
        let idx = v.read_int_any(idx_f).unwrap_or(-1) as i16;
        let wt = v.read_real(wt_f).unwrap_or(0.0);
        if idx >= 0 && wt > 0.0 {
            node_sets.push((idx, wt));
        }
    }
    JmsVertex {
        position,
        normal,
        tangent: None,
        binormal: None,
        node_sets,
        uvs: vec![crate::math::RealPoint2d { x: uv.x, y: 1.0 - uv.y }],
    }
}

//================================================================================
// Writer helpers
//================================================================================

fn write_floats<W: Write>(w: &mut W, values: &[f32]) -> io::Result<()> {
    for (i, v) in values.iter().enumerate() {
        let v = if *v == -0.0 { 0.0 } else { *v };
        if i + 1 < values.len() { write!(w, "{:.10}\t", v)?; }
        else                    { writeln!(w, "{:.10}", v)?; }
    }
    Ok(())
}

const EMPTY_SECTIONS_TRAILING: &[(&str, &[&str])] = &[
    ("CAR_WHEEL", &["<name>", "<chassis index>", "<wheel index>", "<chassis transform>", "<wheel transform>", "<suspension transform>", "<suspension min limit>", "<suspension max limit>"]),
    ("POINT_TO_POINT", &["<name>", "<body A index>", "<body B index>", "<body A transform>", "<body B transform>", "<constraint type>", "<x min>", "<x max>", "<y min>", "<y max>", "<z min>", "<z max>", "<spring length>"]),
    ("PRISMATIC", &["<name>", "<body A index>", "<body B index>", "<body A transform>", "<body B transform>", "<is limited>", "<friction limit>", "<min limit>", "<max limit>"]),
    ("BOUNDING SPHERE", &["<translation <x,y,z>>", "<radius>"]),
    ("SKYLIGHT", &["<direction <x,y,z>>", "<radiant intensity <x,y,z>>", "<solid angle>"]),
];
