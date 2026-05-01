//! ASS (Bungie Amalgam) static-scene export from `scenario_structure_bsp` tags.
//!
//! ASS is the level-geometry counterpart to JMS — same family, but
//! authored for static scene structure rather than rigged objects.
//! H3 targets version 7. Sections: HEADER, MATERIALS, OBJECTS,
//! INSTANCES.
//!
//! [`AssFile::from_scenario_structure_bsp`] reconstructs a complete
//! ASS scene from a parsed `scenario_structure_bsp` tag:
//!
//! - cluster MESHes (one per cluster, identity-transform INSTANCE);
//! - per-IGD-def MESHes + per-placement INSTANCEs for instanced
//!   geometry;
//! - cluster portals (each as a `+portal_N`-named MESH, fan-triangulated);
//! - weather polyhedra (convex hull from plane set, as `+weather_N`);
//! - sbsp markers (SPHERE primitives matching construct's
//!   `frame construct` convention);
//! - environment_objects (xref-only OBJECTs pointing at scenery
//!   palette tag-refs);
//! - structure collision BSP merged into a single `@CollideOnly` MESH.
//!
//! Real lighting and per-material BM_LIGHTING_* metadata are
//! contributed by [`AssFile::add_lights_from_stli`] — fed an
//! `.scenario_structure_lighting_info` (.stli) tag the caller pairs
//! against the sbsp via the scenario's `structure_bsps[]` entry.
//!
//! sbsp's `render geometry` struct is structurally identical to
//! render_model's, so the per-mesh data path reuses the shared
//! [`crate::geometry`] helpers (`CompressionBounds`, `strip_to_list`,
//! bounds-decompression). What's different from JMS export: clusters
//! replace regions/perms, materials don't get `(slot) perm region`
//! expansion (no perm/region cells in sbsp), and triangles emit
//! per-OBJECT rather than into one global list.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::geometry::{read_compression_bounds_at, CompressionBounds, SCALE};
use crate::math::{RealPlane3d, RealPoint3d, RealQuaternion, RealRgbColor, RealVector3d};

// SCALE constant lives in crate::geometry (re-exported above).

/// ASS export errors.
#[derive(Debug)]
pub enum AssError {
    MissingField(&'static str),
    Io(io::Error),
}

impl std::fmt::Display for AssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingField(p) => write!(f, "scenario_structure_bsp is missing required field: {p}"),
            Self::Io(e) => write!(f, "ASS write failed: {e}"),
        }
    }
}

impl std::error::Error for AssError {}

impl From<io::Error> for AssError {
    fn from(e: io::Error) -> Self { Self::Io(e) }
}

/// ASS material entry. The `lightmap_variant` is the artist-assigned
/// lightmap-resolution group label (e.g. `"lm5"`); we leave it empty
/// since the tag doesn't carry it. `bm_strings` are the per-material
/// metadata lines: every material gets a `BM_FLAGS` placeholder + a
/// real `BM_LMRES` line carrying the lightmap resolution from the
/// sbsp material's `properties[type=0]`. Emissive materials gain
/// `BM_LIGHTING_BASIC` / `_ATTEN` / `_FRUS` lines layered on by
/// [`AssFile::add_lights_from_stli`] from the paired stli's
/// `material info[i]` block.
#[derive(Debug, Clone)]
pub struct AssMaterial {
    pub name: String,
    pub lightmap_variant: String,
    pub bm_strings: Vec<String>,
}

/// One vertex of an ASS MESH object. UV is `(u, v, w)` — the w
/// component was added in v5 and is always emitted; we set it to 0.
#[derive(Debug, Clone)]
pub struct AssVertex {
    pub position: RealPoint3d,
    pub normal: RealVector3d,
    pub color: RealRgbColor,
    /// `(node_index, weight)` pairs — empty for static geometry,
    /// populated for skinned meshes.
    pub node_set: Vec<(i32, f32)>,
    /// Texture coordinates as `(u, v, w)` triples. ASS v5 added the
    /// `w` component (always 0 for the 2D textures we emit), so we
    /// model the triple as a [`RealPoint3d`] in `(u, v, w)` order.
    pub uvs: Vec<RealPoint3d>,
}

/// ASS triangle: material slot + 3 vertex indices into [`AssObject::vertices`].
#[derive(Debug, Clone)]
pub struct AssTriangle {
    pub material: i32,
    pub v: [u32; 3],
}

/// One ASS object. The class determines the per-object payload —
/// MESH carries vertices+triangles; light classes carry the light
/// data block; primitive classes carry their own dimensions.
#[derive(Debug, Clone)]
pub struct AssObject {
    pub xref_filepath: String,
    pub xref_objectname: String,
    pub payload: AssObjectPayload,
}

/// Per-class data carried inline on each ASS OBJECT.
#[derive(Debug, Clone)]
pub enum AssObjectPayload {
    Mesh {
        vertices: Vec<AssVertex>,
        triangles: Vec<AssTriangle>,
    },
    /// `GENERIC_LIGHT` with a sub-class (`SPOT_LGT` / `DIRECT_LGT` /
    /// `OMNI_LGT` / `AMBIENT_LGT`). Per-light parameters in the
    /// sub-struct.
    GenericLight(AssLight),
    /// `SPHERE` primitive — `material_index` (-1 for no material) +
    /// `radius` (cm).
    Sphere { material: i32, radius: f32 },
    // Box / Pill / Bone primitives can be added if a future caller needs them.
}

/// ASS light parameters (shared across light sub-classes).
#[derive(Debug, Clone)]
pub struct AssLight {
    pub kind: AssLightKind,
    pub color: RealRgbColor,
    pub intensity: f32,
    /// Cone angles in DEGREES (tag stores radians; converted on read).
    pub hotspot_size: f32,
    pub hotspot_falloff: f32,
    pub use_near_attenuation: bool,
    pub near_atten_min: f32,
    pub near_atten_max: f32,
    pub use_far_attenuation: bool,
    pub far_atten_min: f32,
    pub far_atten_max: f32,
    /// Spot shape — 1=circle, 0=rectangle.
    pub shape: i32,
    pub aspect: f32,
}

/// `GENERIC_LIGHT` sub-class — written verbatim as the OBJECT's
/// class line in the ASS file (`SPOT_LGT` / `DIRECT_LGT` / etc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssLightKind {
    SpotLgt,
    DirectLgt,
    OmniLgt,
    AmbientLgt,
}

impl AssLightKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SpotLgt => "SPOT_LGT",
            Self::DirectLgt => "DIRECT_LGT",
            Self::OmniLgt => "OMNI_LGT",
            Self::AmbientLgt => "AMBIENT_LGT",
        }
    }
}

impl AssObject {
    /// Top-level class label as written into the ASS file.
    fn class_str(&self) -> &'static str {
        match &self.payload {
            AssObjectPayload::Mesh { .. } => "MESH",
            AssObjectPayload::GenericLight(_) => "GENERIC_LIGHT",
            AssObjectPayload::Sphere { .. } => "SPHERE",
        }
    }

    /// Convenience: build an empty MESH object.
    pub fn empty_mesh() -> Self {
        Self {
            xref_filepath: String::new(),
            xref_objectname: String::new(),
            payload: AssObjectPayload::Mesh { vertices: Vec::new(), triangles: Vec::new() },
        }
    }

    /// Vertex count for MESH payloads (`0` for non-MESH).
    pub fn vertices_len(&self) -> usize {
        match &self.payload {
            AssObjectPayload::Mesh { vertices, .. } => vertices.len(),
            _ => 0,
        }
    }

    /// Triangle count for MESH payloads (`0` for non-MESH).
    pub fn triangles_len(&self) -> usize {
        match &self.payload {
            AssObjectPayload::Mesh { triangles, .. } => triangles.len(),
            _ => 0,
        }
    }
}

/// One ASS instance — a placement of an Object at a transform. For
/// the cluster pass we emit one Instance per Object at world origin
/// with identity rotation and unit scale.
#[derive(Debug, Clone)]
pub struct AssInstance {
    pub object_index: i32,
    pub name: String,
    pub unique_id: i32,
    pub parent_id: i32,
    pub inheritance_flag: i32,
    pub local_rotation: RealQuaternion,
    pub local_translation: RealPoint3d,
    pub local_scale: f32,
    pub pivot_rotation: RealQuaternion,
    pub pivot_translation: RealPoint3d,
    pub pivot_scale: f32,
    pub bone_groups: Vec<i32>,
}

impl Default for AssInstance {
    fn default() -> Self {
        Self {
            object_index: 0, name: String::new(), unique_id: 0,
            parent_id: -1, inheritance_flag: 0,
            local_rotation: RealQuaternion::IDENTITY,
            local_translation: RealPoint3d::ZERO,
            local_scale: 1.0,
            pivot_rotation: RealQuaternion::IDENTITY,
            pivot_translation: RealPoint3d::ZERO,
            pivot_scale: 1.0,
            bone_groups: Vec::new(),
        }
    }
}

/// A reconstructed ASS file in memory.
#[derive(Debug, Clone, Default)]
pub struct AssFile {
    pub header_tool: String,
    pub header_tool_version: String,
    pub header_user: String,
    pub header_machine: String,
    pub materials: Vec<AssMaterial>,
    pub objects: Vec<AssObject>,
    pub instances: Vec<AssInstance>,
}

impl AssFile {
    /// Walk a parsed `scenario_structure_bsp` tag and reconstruct an
    /// ASS scene. Returns a complete `AssFile` ready for [`Self::write`]
    /// — call [`Self::add_lights_from_stli`] afterwards to layer in
    /// real lighting from the paired stli tag.
    ///
    /// OBJECTs emitted:
    /// - one MESH per cluster (vertices already in world units, no
    ///   compression bounds applied)
    /// - one MESH per `instanced geometries definitions[]` entry
    ///   (definition-local space, decompressed against the def's own
    ///   `compression index`; content-deduped so byte-identical defs
    ///   collapse to one shared OBJECT)
    /// - one MESH per cluster portal (`+portal_N` name, fan-triangulated
    ///   from `cluster portals[i].vertices`)
    /// - one MESH per weather polyhedron (`+weather_N` name, convex hull
    ///   recovered via triple-plane intersection of the polyhedron's
    ///   bounding planes)
    /// - one MESH for the structure collision BSP (`@CollideOnly`
    ///   instance over an `@collision_only` material — surfaces walked
    ///   via the shared edge-ring algorithm in [`crate::geometry`])
    /// - one SPHERE per sbsp marker (matching the H3 source-tree
    ///   `frame construct` convention; marker name is carried on the
    ///   per-instance INSTANCE record, not the OBJECT)
    /// - one xref-only OBJECT per `environment_object_palette[]` entry
    ///
    /// INSTANCEs emitted:
    /// - INSTANCE 0 is always "Scene Root" (`object_index = -1`); every
    ///   subsequent record uses it as the parent
    /// - one identity-transform INSTANCE per cluster MESH
    /// - one INSTANCE per `instanced geometry instances[]` placement
    ///   with the per-placement transform (3-vec3 forward/left/up
    ///   rotation matrix → quaternion, position × 100 cm, uniform scale)
    /// - one INSTANCE per portal / weather polyhedron / collision BSP
    /// - one INSTANCE per sbsp marker (rotation+position from the tag,
    ///   parented to Scene Root)
    /// - one INSTANCE per `environment_objects[]` placement (xref to the
    ///   palette OBJECT, transform from the placement)
    ///
    /// Materials gain `+portal` / `+weather` / `@collision_only` marker
    /// entries on demand so Tool.exe re-extracts each recompile-only
    /// category back into its proper tag block.
    ///
    /// sbsp `render geometry` is structurally identical to render_model's,
    /// so the per-mesh data path reuses the shared [`crate::geometry`]
    /// helpers (`CompressionBounds`, generic field readers, BSP edge-ring
    /// walker). The triangle-strip → list converter [`crate::geometry`]
    /// also exposes is *not* used here — H3 sbsp meshes are always
    /// triangle lists despite some carrying a misleading `index buffer
    /// type = triangle strip` enum value.
    pub fn from_scenario_structure_bsp(tag: &TagFile) -> Result<Self, AssError> {
        let root = tag.root();
        let materials = read_materials(&root)?;

        let clusters = root.field_path("clusters").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("clusters"))?;
        let meshes = root.field_path("render geometry/meshes").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("render geometry/meshes"))?;
        let pmt = root.field_path("render geometry/per mesh temporary").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("render geometry/per mesh temporary"))?;

        let mut objects: Vec<AssObject> = Vec::new();
        let mut instances: Vec<AssInstance> = Vec::new();

        // INSTANCE 0 is always "Scene Root" — a parent-only marker
        // (object_index = -1) that all geometry/light placements
        // chain off via parent_id. Matches the H3 source-tree
        // authoring convention from Maya/Max where every instance
        // is a child of the world Scene Root node.
        instances.push(AssInstance {
            object_index: -1,
            name: "Scene Root".to_owned(),
            unique_id: 0,
            parent_id: -1,
            inheritance_flag: 0,
            local_rotation: RealQuaternion::IDENTITY,
            local_translation: RealPoint3d::ZERO,
            local_scale: 1.0,
            pivot_rotation: RealQuaternion::IDENTITY,
            pivot_translation: RealPoint3d::ZERO,
            pivot_scale: 1.0,
            bone_groups: Vec::new(),
        });

        // Clusters → MESH OBJECTs at origin.
        // Per the H3 sbsp partitioning rule (verified empirically by
        // the H3 Blender Toolset's `_mesh_decoder.py`): a mesh whose
        // index is >= `render geometry/compression info` count is a
        // CLUSTER mesh, vertices already in world units — no
        // compression bounds applied (identity). The bounds-compressed
        // path is for instanced geometries (handled below, where each
        // def carries its own `compression index`).
        let cluster_bounds = CompressionBounds::identity();
        for ci in 0..clusters.len() {
            let cluster = clusters.element(ci).unwrap();
            let mesh_idx = cluster.read_int_any("mesh index").unwrap_or(-1);
            if mesh_idx < 0 || (mesh_idx as usize) >= meshes.len() { continue; }
            let mesh = meshes.element(mesh_idx as usize).unwrap();
            if (mesh_idx as usize) >= pmt.len() { continue; }
            let mesh_pmt = pmt.element(mesh_idx as usize).unwrap();

            let object = build_cluster_object(&mesh, &mesh_pmt, &cluster_bounds, false)?;
            if object.vertices_len() == 0 { continue; }
            let object_index = objects.len() as i32;
            objects.push(object);
            instances.push(AssInstance {
                object_index,
                name: format!("cluster_{ci}"),
                unique_id: instances.len() as i32,
                parent_id: 0, // Scene Root
                ..Default::default()
            });
        }
        // From here on, materials may grow as we encounter
        // recompile-only categories (portals, weather, collision)
        // that need their own `+portal` / `+weather` / `@collision`
        // marker materials so Tool.exe re-extracts them into the
        // right tag blocks.
        let mut materials = materials;

        // Cluster portals. Each portal is a convex polygon separating
        // two clusters. To round-trip through Tool.exe we emit each
        // portal as a `+portal`-named MESH so the compiler re-extracts
        // it into `cluster_portals[]`. Vertices are stored in
        // world-space (ASS cm = tag world units × 100).
        let portal_mat_idx = ensure_special_material(&mut materials, "+portal") as i32;
        if let Some(portals) = root.field_path("cluster portals").and_then(|f| f.as_block()) {
            for pi in 0..portals.len() {
                let portal = portals.element(pi).unwrap();
                let verts_block = match portal.field("vertices").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                if verts_block.len() < 3 { continue; }
                let mut verts: Vec<AssVertex> = Vec::with_capacity(verts_block.len());
                for vi in 0..verts_block.len() {
                    let pe = verts_block.element(vi).unwrap();
                    let p = pe.read_point3d("point");
                    verts.push(AssVertex {
                        position: p * SCALE,
                        normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
                        color: RealRgbColor::default(),
                        node_set: Vec::new(),
                        uvs: vec![RealPoint3d::ZERO],
                    });
                }
                // Triangle-fan the convex polygon.
                let mut tris: Vec<AssTriangle> = Vec::with_capacity(verts.len().saturating_sub(2));
                for k in 1..verts.len() - 1 {
                    tris.push(AssTriangle {
                        material: portal_mat_idx,
                        v: [0, k as u32, k as u32 + 1],
                    });
                }
                let object_index = objects.len() as i32;
                objects.push(AssObject {
                    xref_filepath: String::new(),
                    xref_objectname: String::new(),
                    payload: AssObjectPayload::Mesh { vertices: verts, triangles: tris },
                });
                instances.push(AssInstance {
                    object_index,
                    name: format!("+portal_{pi}"),
                    unique_id: instances.len() as i32,
                    parent_id: 0, // Scene Root
                    ..Default::default()
                });
            }
        }

        // Instanced geometries definitions → OBJECTs +
        // instanced geometry instances → INSTANCEs.
        // Definitions live in the raw_resources block (an inline
        // resource container that holds collision BSP + instance
        // defs, distinct from the api-resource pageables).
        let defs = root.field_path("resource interface/raw_resources[0]/raw_items/instanced geometries definitions")
            .and_then(|f| f.as_block());
        let inst_block = root.field_path("instanced geometry instances")
            .and_then(|f| f.as_block());
        if let (Some(defs), Some(inst_block)) = (defs, inst_block) {
            // Build one OBJECT per definition, with content-based
            // deduplication so two definitions whose decompressed
            // (vertex, triangle) data is byte-identical share a
            // single OBJECT. This matches what source ASS files
            // emit — the artist-side toolchain dedupes by mesh
            // content because Maya/Max materializes shared meshes
            // as instances pointing at one underlying mesh.
            let mut def_object_index: Vec<Option<i32>> = vec![None; defs.len()];
            let mut content_to_object_index: std::collections::HashMap<Vec<u8>, i32> = std::collections::HashMap::new();
            for di in 0..defs.len() {
                let def = defs.element(di).unwrap();
                let mesh_idx = def.read_int_any("mesh index").unwrap_or(-1);
                let comp_idx = def.read_int_any("compression index").unwrap_or(0).max(0) as usize;
                if mesh_idx < 0 || (mesh_idx as usize) >= meshes.len() { continue; }
                if (mesh_idx as usize) >= pmt.len() { continue; }
                let bounds = read_compression_bounds_at(&root, comp_idx);
                // Compression-bounds chirality: when an ODD number of
                // axes have negative span (mx < mn), the unpacker's
                // Jacobian flips sign and triangle winding inverts vs
                // the stored vertex normals. Detect + swap b/c per
                // triangle inside build_cluster_object. Rare in
                // shipped Guardian content but a documented
                // safety-net per the H3 Blender Toolset's decoder.
                let flip_winding = compute_axis_flip(&bounds);
                let mesh = meshes.element(mesh_idx as usize).unwrap();
                let mesh_pmt = pmt.element(mesh_idx as usize).unwrap();
                let object = build_cluster_object(&mesh, &mesh_pmt, &bounds, flip_winding)?;
                if object.vertices_len() == 0 { continue; }
                let key = object_content_key(&object);
                if let Some(&existing) = content_to_object_index.get(&key) {
                    def_object_index[di] = Some(existing);
                } else {
                    let idx = objects.len() as i32;
                    content_to_object_index.insert(key, idx);
                    def_object_index[di] = Some(idx);
                    objects.push(object);
                }
            }

            // Now walk placements; emit one INSTANCE per placement
            // pointing at the definition's object with the placement's
            // 3-vec3-rotation + position + scale transform.
            for ii in 0..inst_block.len() {
                let inst = inst_block.element(ii).unwrap();
                let def_idx = inst.read_int_any("instance definition").unwrap_or(-1);
                if def_idx < 0 || (def_idx as usize) >= def_object_index.len() { continue; }
                let Some(object_index) = def_object_index[def_idx as usize] else { continue; };
                let scale = inst.read_real("scale").unwrap_or(1.0);
                // Schema: forward/left/up are `real_vector_3d`,
                // position is `real_point_3d`.
                let f = inst.read_vec3("forward");
                let l = inst.read_vec3("left");
                let u = inst.read_vec3("up");
                let p = inst.read_point3d("position");
                let rot = RealQuaternion::from_basis_columns(f, l, u);
                let name = inst.read_string_id("name").unwrap_or_else(|| format!("instance_{ii}"));
                instances.push(AssInstance {
                    object_index,
                    name,
                    unique_id: instances.len() as i32,
                    parent_id: 0, // Scene Root
                    inheritance_flag: 0,
                    local_rotation: rot,
                    local_translation: p * SCALE,
                    local_scale: scale,
                    pivot_rotation: RealQuaternion::IDENTITY,
                    pivot_translation: RealPoint3d::ZERO,
                    pivot_scale: 1.0,
                    bone_groups: Vec::new(),
                });
            }
        }

        // Weather polyhedra. Each polyhedron is a convex region
        // defined by a set of bounding planes (`ax+by+cz+d=0`,
        // normal points outward, "inside" is `n·p + d <= 0`). To
        // emit as a MESH for re-compilation, we recover the region's
        // vertices via triple-plane intersections, filter to those
        // inside ALL other planes, then fan-triangulate per face.
        // Each polyhedron becomes one `+weather`-named MESH so
        // Tool.exe re-extracts it on recompile. Verified rare in H3
        // MP corpus (only s3d_lockout has any).
        let weather_mat_idx = ensure_special_material(&mut materials, "+weather") as i32;
        if let Some(wp_block) = root.field_path("weather polyhedra").and_then(|f| f.as_block()) {
            for wi in 0..wp_block.len() {
                let wp = wp_block.element(wi).unwrap();
                let planes_block = match wp.field("planes").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let mut planes: Vec<RealPlane3d> = Vec::with_capacity(planes_block.len());
                for pi in 0..planes_block.len() {
                    let pe = planes_block.element(pi).unwrap();
                    planes.push(pe.read_plane3d("plane"));
                }
                if planes.len() < 4 { continue; }
                let (verts, tris) = polyhedron_from_planes(&planes, weather_mat_idx);
                if verts.is_empty() { continue; }
                let object_index = objects.len() as i32;
                objects.push(AssObject {
                    xref_filepath: String::new(),
                    xref_objectname: String::new(),
                    payload: AssObjectPayload::Mesh { vertices: verts, triangles: tris },
                });
                instances.push(AssInstance {
                    object_index,
                    name: format!("+weather_{wi}"),
                    unique_id: instances.len() as i32,
                    parent_id: 0,
                    ..Default::default()
                });
            }
        }

        // sbsp markers. Each marker becomes a SPHERE primitive OBJECT
        // (matching the H3 source convention where construct emits
        // `'frame construct'` as a SPHERE marker) plus one INSTANCE
        // carrying the marker name + transform. Marker OBJECTs use
        // parent=-1 (no material) and a default 10cm radius. Tool.exe
        // re-extracts INSTANCEs of named SPHEREs into sbsp.markers on
        // recompile.
        if let Some(markers_block) = root.field_path("markers").and_then(|f| f.as_block()) {
            for mi in 0..markers_block.len() {
                let m = markers_block.element(mi).unwrap();
                let name = m.read_string_id("name").unwrap_or_else(|| format!("marker_{mi}"));
                let pos = m.read_point3d("position");
                let rot = m.read_quat("rotation");
                let object_index = objects.len() as i32;
                objects.push(AssObject {
                    xref_filepath: String::new(),
                    xref_objectname: String::new(),
                    payload: AssObjectPayload::Sphere { material: -1, radius: 10.0 },
                });
                instances.push(AssInstance {
                    object_index,
                    name,
                    unique_id: instances.len() as i32,
                    parent_id: 0,
                    inheritance_flag: 0,
                    local_rotation: rot,
                    local_translation: pos * SCALE,
                    local_scale: 1.0,
                    pivot_rotation: RealQuaternion::IDENTITY,
                    pivot_translation: RealPoint3d::ZERO,
                    pivot_scale: 1.0,
                    bone_groups: Vec::new(),
                });
            }
        }

        // Environment objects. These are sbsp-level scenery placements
        // (one per `environment_objects[i]` pointing into
        // `environment_object_palette[]`). Emit each as an XREF OBJECT
        // — no inline geometry, just `xref_filepath` and
        // `xref_objectname` pointing at the scenery tag — plus one
        // INSTANCE per placement carrying the transform. Tool.exe
        // re-resolves the xref on recompile via the scenery tag-ref.
        let env_objects = root.field_path("environment objects").and_then(|f| f.as_block());
        let env_palette = root.field_path("environment object palette").and_then(|f| f.as_block());
        if let (Some(eo), Some(ep)) = (env_objects, env_palette) {
            // Build OBJECT per palette entry (xref to scenery).
            let mut palette_object_index: Vec<Option<i32>> = vec![None; ep.len()];
            for pi in 0..ep.len() {
                let pal = ep.element(pi).unwrap();
                let xref = pal.read_tag_ref_path("object").unwrap_or_default();
                if xref.is_empty() { continue; }
                let xref_name = Path::new(&xref.replace('\\', "/"))
                    .file_stem().and_then(|s| s.to_str()).unwrap_or("env_object").to_owned();
                palette_object_index[pi] = Some(objects.len() as i32);
                objects.push(AssObject {
                    xref_filepath: xref,
                    xref_objectname: xref_name,
                    payload: AssObjectPayload::Mesh { vertices: Vec::new(), triangles: Vec::new() },
                });
            }
            for ei in 0..eo.len() {
                let placement = eo.element(ei).unwrap();
                let pi = placement.read_int_any("palette index").unwrap_or(-1);
                if pi < 0 || (pi as usize) >= palette_object_index.len() { continue; }
                let Some(object_index) = palette_object_index[pi as usize] else { continue; };
                let pos = placement.read_point3d("position");
                let rot = placement.read_quat("rotation");
                let scale = placement.read_real("scale").unwrap_or(1.0);
                let name = placement.read_string_id("name").unwrap_or_else(|| format!("env_object_{ei}"));
                instances.push(AssInstance {
                    object_index,
                    name,
                    unique_id: instances.len() as i32,
                    parent_id: 0,
                    inheritance_flag: 0,
                    local_rotation: rot,
                    local_translation: pos * SCALE,
                    local_scale: scale,
                    pivot_rotation: RealQuaternion::IDENTITY,
                    pivot_translation: RealPoint3d::ZERO,
                    pivot_scale: 1.0,
                    bone_groups: Vec::new(),
                });
            }
        }

        // Structure collision BSP. Lives at
        // `resource interface/raw_resources[0]/raw_items/collision bsp`.
        // Same shape as collision_model BSPs — surfaces walk an
        // edge ring (each edge belongs to two surfaces; matching
        // side decides start-vs-end vertex emission). Emit as a
        // single MESH OBJECT with `@collision_only`-named material
        // so Tool.exe re-extracts it into the tag's collision BSP
        // on recompile. Reuses crate::geometry::walk_surface_ring.
        if let Some(coll_block) = root.field_path("resource interface/raw_resources[0]/raw_items/collision bsp")
            .and_then(|f| f.as_block())
        {
            let coll_mat_idx = ensure_special_material(&mut materials, "@collision_only") as i32;
            let mut coll_verts: Vec<AssVertex> = Vec::new();
            let mut coll_tris: Vec<AssTriangle> = Vec::new();
            let mut next_index: u32 = 0;
            for ci in 0..coll_block.len() {
                let bsp = coll_block.element(ci).unwrap();
                let surfaces = match bsp.field("surfaces").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let edges = match bsp.field("edges").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let bsp_verts = match bsp.field("vertices").and_then(|f| f.as_block()) { Some(b) => b, None => continue };
                let edge_cache: Vec<crate::geometry::EdgeRow> = (0..edges.len()).map(|k| {
                    let e = edges.element(k).unwrap();
                    crate::geometry::EdgeRow {
                        start_vertex: e.read_int_any("start vertex").unwrap_or(-1) as i32,
                        end_vertex: e.read_int_any("end vertex").unwrap_or(-1) as i32,
                        forward_edge: e.read_int_any("forward edge").unwrap_or(-1) as i32,
                        reverse_edge: e.read_int_any("reverse edge").unwrap_or(-1) as i32,
                        left_surface: e.read_int_any("left surface").unwrap_or(-1) as i32,
                        right_surface: e.read_int_any("right surface").unwrap_or(-1) as i32,
                    }
                }).collect();
                let bsp_points: Vec<RealPoint3d> = (0..bsp_verts.len()).map(|k| {
                    bsp_verts.element(k).unwrap().read_point3d("point") * SCALE
                }).collect();
                for si in 0..surfaces.len() {
                    let surface = surfaces.element(si).unwrap();
                    let first_edge = surface.read_int_any("first edge").unwrap_or(-1) as i32;
                    if first_edge < 0 { continue; }
                    let polygon = crate::geometry::walk_surface_ring(si as i32, first_edge, &edge_cache);
                    if polygon.len() < 3 { continue; }
                    // Triangle-fan the convex polygon.
                    let base_for_fan = next_index;
                    for &vi in &polygon {
                        let pos = bsp_points.get(vi as usize).copied().unwrap_or(RealPoint3d::ZERO);
                        coll_verts.push(AssVertex {
                            position: pos,
                            normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
                            color: RealRgbColor::default(),
                            node_set: Vec::new(),
                            uvs: vec![RealPoint3d::ZERO],
                        });
                    }
                    let n = polygon.len() as u32;
                    for k in 1..n - 1 {
                        coll_tris.push(AssTriangle {
                            material: coll_mat_idx,
                            v: [base_for_fan, base_for_fan + k, base_for_fan + k + 1],
                        });
                    }
                    next_index += n;
                }
            }
            if !coll_verts.is_empty() {
                let object_index = objects.len() as i32;
                objects.push(AssObject {
                    xref_filepath: String::new(),
                    xref_objectname: String::new(),
                    payload: AssObjectPayload::Mesh { vertices: coll_verts, triangles: coll_tris },
                });
                instances.push(AssInstance {
                    object_index,
                    name: "@CollideOnly".to_owned(),
                    unique_id: instances.len() as i32,
                    parent_id: 0,
                    ..Default::default()
                });
            }
        }

        Ok(Self {
            header_tool: "blam-tags".to_owned(),
            header_tool_version: "0.1".to_owned(),
            header_user: "blam-tag-shell".to_owned(),
            header_machine: String::new(),
            materials,
            objects,
            instances,
        })
    }

    /// Walk a parsed `render_model` tag and reconstruct an ASS scene
    /// suitable for re-import via Tool. Counterpart to
    /// [`crate::JmsFile::from_render_model`] for the same input —
    /// ASS is the format Bungie's pipeline used for any render_model
    /// that needs **instance geometry** (modular characters like the
    /// brute, decorators, level objects).
    ///
    /// Empirically across halo3_mcc: every render_model with
    /// `instance mesh index >= 0` was sourced from a `.ass` file (141
    /// of them); every `.jms`-sourced render_model has no instance
    /// geometry. JMS literally has no `INSTANCES` section to carry
    /// the per-placement transforms, so going round-trip-through-Tool
    /// for these requires emitting ASS.
    ///
    /// OBJECTs emitted:
    /// - one MESH per `(region, permutation, mesh_index_off)` cell
    ///   — same content-grouping the JMS path uses, but each cell is
    ///   its own OBJECT here (vs JMS where everything lives in a
    ///   global vertex/triangle pool keyed by material).
    /// - one MESH for the instance mesh (`meshes[instance mesh index]`)
    ///   when `instance mesh index >= 0` — geometry is the union of
    ///   all subparts, vertices in mesh-local space.
    /// - one SPHERE per `marker_groups[].markers[]` entry. SPHERE
    ///   class with `# <group>` instance name parented to the marker's
    ///   `node_index` bone. Mirrors the H3 source-tree convention.
    ///
    /// INSTANCEs emitted:
    /// - INSTANCE 0 is "Scene Root" (object_index = -1).
    /// - one frame INSTANCE per render_model node (bones), parented
    ///   through the node hierarchy. These carry no mesh — they're
    ///   the armature scaffolding Tool uses to round-trip the
    ///   skeleton on re-compile.
    /// - one INSTANCE per region-permutation MESH parented to its
    ///   rigid-fallback bone (or Scene Root if none).
    /// - one INSTANCE per `instance placements[]` entry — each
    ///   placement spawns the instance-mesh OBJECT at the placement's
    ///   `(forward, left, up, position) × scale` matrix, parented to
    ///   `node_index`. **This is the round-trip-critical bit Tool
    ///   re-extracts back into `instance placements[]` on recompile.**
    ///
    /// Materials carry `(slot) <perm> <region>` labels (same convention
    /// as the JMS path / H3 Blender exporter); the slot is a 1-based
    /// counter. The instance-mesh material gets `(slot) <placement_name>`
    /// per-placement so each piece is independently identifiable in
    /// the artist scene.
    pub fn from_render_model(tag: &TagFile) -> Result<Self, AssError> {
        let root = tag.root();

        let mut materials: Vec<AssMaterial> = Vec::new();
        let mut objects: Vec<AssObject> = Vec::new();
        let mut instances: Vec<AssInstance> = Vec::new();

        // INSTANCE 0: Scene Root.
        instances.push(AssInstance {
            object_index: -1,
            name: "Scene Root".to_owned(),
            unique_id: 0,
            parent_id: -1,
            inheritance_flag: 0,
            ..Default::default()
        });

        // Bones: emit one frame INSTANCE per render_model node, with
        // parent_id chained through the node hierarchy. Indices into
        // `instances` for each node are recorded in `node_to_instance`
        // so later mesh placements (regions/permutations + instance
        // placements + markers) can parent themselves to the right bone.
        let nodes = read_rm_nodes_local(&root)?;
        let mut node_to_instance: Vec<i32> = vec![-1; nodes.len()];
        for (i, n) in nodes.iter().enumerate() {
            // Parent INSTANCE: Scene Root (0) for root nodes; otherwise
            // the bone's parent's INSTANCE. The render_model nodes block
            // stores parent-before-child, so node_to_instance[parent]
            // is always populated when we get here.
            let parent_inst = if n.parent < 0 {
                0
            } else {
                node_to_instance.get(n.parent as usize).copied().unwrap_or(0)
            };
            let inst_idx = instances.len() as i32;
            instances.push(AssInstance {
                object_index: -1,
                name: n.name.clone(),
                unique_id: inst_idx,
                parent_id: parent_inst,
                inheritance_flag: 0,
                local_rotation: n.rotation,
                local_translation: n.translation * SCALE,
                local_scale: 1.0,
                pivot_rotation: RealQuaternion::IDENTITY,
                pivot_translation: RealPoint3d::ZERO,
                pivot_scale: 1.0,
                bone_groups: Vec::new(),
            });
            node_to_instance[i] = inst_idx;
        }

        // Cache geometry blocks once.
        let bounds = crate::geometry::read_compression_bounds(&root);
        let meshes = root.field_path("render geometry/meshes").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("render geometry/meshes"))?;
        let pmt = root.field_path("render geometry/per mesh temporary").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("render geometry/per mesh temporary"))?;
        let mats_block = root.field_path("materials").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("materials"))?;
        let regions_block = root.field_path("regions").and_then(|f| f.as_block())
            .ok_or(AssError::MissingField("regions"))?;

        // Walk regions × permutations and emit one MESH OBJECT per
        // mesh referenced by a permutation, plus one INSTANCE per
        // (region, permutation) cell parented to the rigid-fallback
        // bone where applicable.
        for ri in 0..regions_block.len() {
            let region = regions_block.element(ri).unwrap();
            let region_name = region.read_string_id("name").unwrap_or_default();
            let perms = match region.field("permutations").and_then(|f| f.as_block()) {
                Some(b) => b,
                None => continue,
            };
            for pi in 0..perms.len() {
                let perm = perms.element(pi).unwrap();
                let perm_name = perm.read_string_id("name").unwrap_or_default();
                let mesh_idx = perm.read_int_any("mesh index").unwrap_or(-1);
                let mesh_count = perm.read_int_any("mesh count").unwrap_or(0);
                if mesh_idx < 0 || mesh_count <= 0 { continue; }

                for mi_off in 0..mesh_count as usize {
                    let mi = mesh_idx as usize + mi_off;
                    if mi >= meshes.len() || mi >= pmt.len() { continue; }
                    let mesh = meshes.element(mi).unwrap();
                    let mesh_pmt = pmt.element(mi).unwrap();
                    let cell_label = format!("{} {}", perm_name, region_name);
                    let object = build_render_model_object(
                        &mesh, &mesh_pmt, &mats_block, &bounds, &mut materials, &cell_label,
                    )?;
                    if object.vertices_len() == 0 { continue; }
                    let object_index = objects.len() as i32;
                    objects.push(object);

                    // Rigid-fallback parenting: if the mesh has a
                    // `rigid node index >= 0`, parent the INSTANCE
                    // there; otherwise hang it off Scene Root.
                    let rigid_node = mesh.read_int_any("rigid node index")
                        .map(|v| v as i32).filter(|&v| v >= 0);
                    let parent_inst = rigid_node
                        .and_then(|n| node_to_instance.get(n as usize).copied())
                        .filter(|&v| v >= 0)
                        .unwrap_or(0);
                    instances.push(AssInstance {
                        object_index,
                        name: format!("{}:{}", region_name, perm_name),
                        unique_id: instances.len() as i32,
                        parent_id: parent_inst,
                        ..Default::default()
                    });
                }
            }
        }

        // Instance placements — the round-trip-critical bit.
        // `instance mesh index` points at one mesh whose subparts hold
        // the modular geometry; each `instance placements[i]` is paired
        // with `subparts[i]` and spawns the whole instance mesh at its
        // placement matrix, parented to `node_index`.
        let instance_mesh_index = root.read_int_any("instance mesh index").unwrap_or(-1);
        if instance_mesh_index >= 0 {
            let imi = instance_mesh_index as usize;
            if imi < meshes.len() && imi < pmt.len() {
                let placements = root.field("instance placements").and_then(|f| f.as_block());
                if let Some(placements) = placements {
                    if !placements.is_empty() {
                        // One MESH OBJECT for the whole instance mesh,
                        // covering all subparts. Per-placement subpart
                        // selection lives on the INSTANCE side (we can't
                        // express subpart filtering in ASS directly —
                        // Tool's compiler will re-derive subparts on
                        // recompile by walking placements).
                        let mesh = meshes.element(imi).unwrap();
                        let mesh_pmt = pmt.element(imi).unwrap();
                        let object = build_render_model_object(
                            &mesh, &mesh_pmt, &mats_block, &bounds, &mut materials,
                            "instance_mesh",
                        )?;
                        let imi_object_index = if object.vertices_len() > 0 {
                            let idx = objects.len() as i32;
                            objects.push(object);
                            Some(idx)
                        } else {
                            None
                        };

                        if let Some(object_index) = imi_object_index {
                            for ii in 0..placements.len() {
                                let placement = placements.element(ii).unwrap();
                                let name = placement.read_string_id("name")
                                    .unwrap_or_else(|| format!("instance_{ii}"));
                                let node_idx = placement.read_int_any("node_index")
                                    .map(|v| v as i32).unwrap_or(-1);
                                let scale = placement.read_real("scale").unwrap_or(1.0);
                                let f = placement.read_vec3("forward");
                                let l = placement.read_vec3("left");
                                let u = placement.read_vec3("up");
                                let p = placement.read_point3d("position");
                                let rot = RealQuaternion::from_basis_columns(f, l, u);
                                let parent_inst = if node_idx >= 0 {
                                    node_to_instance.get(node_idx as usize).copied()
                                        .filter(|&v| v >= 0).unwrap_or(0)
                                } else {
                                    0
                                };
                                instances.push(AssInstance {
                                    object_index,
                                    name,
                                    unique_id: instances.len() as i32,
                                    parent_id: parent_inst,
                                    inheritance_flag: 0,
                                    local_rotation: rot,
                                    local_translation: p * SCALE,
                                    local_scale: scale,
                                    pivot_rotation: RealQuaternion::IDENTITY,
                                    pivot_translation: RealPoint3d::ZERO,
                                    pivot_scale: 1.0,
                                    bone_groups: Vec::new(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Markers — render_model markers live in `marker groups[].markers[]`.
        // Emit each as a SPHERE primitive parented to its bone, with
        // the `#<group>` source-tree name convention.
        if let Some(groups) = root.field("marker groups").and_then(|f| f.as_block()) {
            for gi in 0..groups.len() {
                let group = groups.element(gi).unwrap();
                let group_name = group.read_string_id("name").unwrap_or_default();
                let markers = match group.field("markers").and_then(|f| f.as_block()) {
                    Some(b) => b, None => continue,
                };
                for mi in 0..markers.len() {
                    let m = markers.element(mi).unwrap();
                    let node_idx = m.read_int_any("node index")
                        .map(|v| v as i32).unwrap_or(-1);
                    let translation = m.read_point3d("translation");
                    let rotation = m.read_quat("rotation");
                    let radius = m.read_real("scale").unwrap_or(0.01);
                    let object_index = objects.len() as i32;
                    objects.push(AssObject {
                        xref_filepath: String::new(),
                        xref_objectname: String::new(),
                        payload: AssObjectPayload::Sphere {
                            material: -1,
                            radius: radius * SCALE,
                        },
                    });
                    let parent_inst = if node_idx >= 0 {
                        node_to_instance.get(node_idx as usize).copied()
                            .filter(|&v| v >= 0).unwrap_or(0)
                    } else {
                        0
                    };
                    instances.push(AssInstance {
                        object_index,
                        name: format!("#{}", group_name),
                        unique_id: instances.len() as i32,
                        parent_id: parent_inst,
                        inheritance_flag: 0,
                        local_rotation: rotation,
                        local_translation: translation * SCALE,
                        local_scale: 1.0,
                        pivot_rotation: RealQuaternion::IDENTITY,
                        pivot_translation: RealPoint3d::ZERO,
                        pivot_scale: 1.0,
                        bone_groups: Vec::new(),
                    });
                }
            }
        }

        Ok(Self {
            header_tool: "MAX".to_owned(),
            header_tool_version: "8.0".to_owned(),
            header_user: "blam-tags".to_owned(),
            header_machine: String::new(),
            materials,
            objects,
            instances,
        })
    }

    /// Write the ASS as version 7 (H3) text format.
    pub fn write<W: Write>(&self, w: &mut W) -> Result<(), AssError> {
        writeln!(w, ";### HEADER ###")?;
        writeln!(w, "7")?;
        writeln!(w, "\"{}\"", self.header_tool)?;
        writeln!(w, "\"{}\"", self.header_tool_version)?;
        writeln!(w, "\"{}\"", self.header_user)?;
        writeln!(w, "\"{}\"", self.header_machine)?;
        writeln!(w)?;

        writeln!(w, ";### MATERIALS ###")?;
        writeln!(w, "{}", self.materials.len())?;
        for (i, m) in self.materials.iter().enumerate() {
            writeln!(w)?;
            writeln!(w, ";MATERIAL {i}")?;
            writeln!(w, "\"{}\"", m.name)?;
            writeln!(w, "\"{}\"", m.lightmap_variant)?;
            writeln!(w, "{}", m.bm_strings.len())?;
            for s in &m.bm_strings {
                writeln!(w, "\"{s}\"")?;
            }
        }
        writeln!(w)?;

        writeln!(w, ";### OBJECTS ###")?;
        writeln!(w, "{}", self.objects.len())?;
        for (i, obj) in self.objects.iter().enumerate() {
            // OBJECT format per the H3 Blender exporter
            // (Halo-Asset-Blender-Development-Toolset
            // build_asset.py, write_objects). Class-specific payload
            // dispatch — MESH carries vertex/triangle data,
            // GENERIC_LIGHT carries the light parameter block,
            // SPHERE carries radius+material.
            writeln!(w)?;
            writeln!(w, ";OBJECT {i}")?;
            writeln!(w, "\"{}\"", obj.class_str())?;
            writeln!(w, "\"{}\"", obj.xref_filepath)?;
            writeln!(w, "\"{}\"", obj.xref_objectname)?;
            match &obj.payload {
                AssObjectPayload::Mesh { vertices, triangles } => {
                    write!(w, "{}", vertices.len())?;
                    for v in vertices {
                        write!(w, "\n")?;
                        write_floats(w, &v.position.to_array())?;
                        write_floats(w, &v.normal.to_array())?;
                        write_floats(w, &[v.color.red, v.color.green, v.color.blue])?;
                        write!(w, "{}", v.node_set.len())?;
                        for (idx, weight) in &v.node_set {
                            write!(w, "\n{}\t{:.10}", idx, weight)?;
                        }
                        write!(w, "\n{}", v.uvs.len())?;
                        for uv in &v.uvs {
                            write!(w, "\n{:.10}\t{:.10}\t{:.10}\n", uv.x, uv.y, uv.z)?;
                        }
                    }
                    write!(w, "\n{}", triangles.len())?;
                    for t in triangles {
                        write!(w, "\n{}\t\t{}\t{}\t{}", t.material, t.v[0], t.v[1], t.v[2])?;
                    }
                    writeln!(w)?;
                }
                AssObjectPayload::GenericLight(l) => {
                    // SPOT/DIRECT/OMNI/AMBIENT class line, then
                    // color, intensity, hotspot, falloff,
                    // use_near, near_min, near_max, use_far,
                    // far_min, far_max, shape, aspect.
                    writeln!(w, "\"{}\"", l.kind.as_str())?;
                    write_floats(w, &[l.color.red, l.color.green, l.color.blue])?;
                    writeln!(w, "{:.10}", l.intensity)?;
                    writeln!(w, "{:.10}", l.hotspot_size)?;
                    writeln!(w, "{:.10}", l.hotspot_falloff)?;
                    writeln!(w, "{}", if l.use_near_attenuation { 1 } else { 0 })?;
                    writeln!(w, "{:.10}", l.near_atten_min)?;
                    writeln!(w, "{:.10}", l.near_atten_max)?;
                    writeln!(w, "{}", if l.use_far_attenuation { 1 } else { 0 })?;
                    writeln!(w, "{:.10}", l.far_atten_min)?;
                    writeln!(w, "{:.10}", l.far_atten_max)?;
                    writeln!(w, "{}", l.shape)?;
                    writeln!(w, "{:.10}", l.aspect)?;
                }
                AssObjectPayload::Sphere { material, radius } => {
                    writeln!(w, "{}", material)?;
                    writeln!(w, "{:.10}", radius)?;
                }
            }
        }
        writeln!(w)?;

        writeln!(w, ";### INSTANCES ###")?;
        writeln!(w, "{}", self.instances.len())?;
        for (i, inst) in self.instances.iter().enumerate() {
            writeln!(w)?;
            writeln!(w, ";INSTANCE {i}")?;
            writeln!(w, "{}", inst.object_index)?;
            writeln!(w, "\"{}\"", inst.name)?;
            writeln!(w, "{}", inst.unique_id)?;
            writeln!(w, "{}", inst.parent_id)?;
            writeln!(w, "{}", inst.inheritance_flag)?;
            write_floats(w, &inst.local_rotation.to_array())?;
            write_floats(w, &inst.local_translation.to_array())?;
            writeln!(w, "{:.10}", inst.local_scale)?;
            write_floats(w, &inst.pivot_rotation.to_array())?;
            write_floats(w, &inst.pivot_translation.to_array())?;
            writeln!(w, "{:.10}", inst.pivot_scale)?;
            for node_index in &inst.bone_groups {
                writeln!(w, "{node_index}")?;
            }
        }
        Ok(())
    }

    /// Append the lights from a parsed `scenario_structure_lighting_info`
    /// (.stli) tag to this AssFile. Each light definition becomes one
    /// `GENERIC_LIGHT` OBJECT carrying the light parameters (color,
    /// intensity, cone angles in DEGREES, attenuation bounds in cm).
    /// Each light instance becomes one INSTANCE pointing at its
    /// definition's object, with the placement transform built from
    /// `(forward, left=cross(up,forward), up)` columns.
    ///
    /// In the H3 source-tree workflow each scenario_structure_bsp
    /// pairs with one stli per `scenario.structure_bsps[i]` ref;
    /// callers load both and feed the stli here.
    pub fn add_lights_from_stli(&mut self, stli: &TagFile) -> Result<(), AssError> {
        let root = stli.root();

        // First pass: layer per-material emissive/attenuation/frustum
        // data from `material info[i]` onto our existing materials
        // (indexed-aligned with sbsp.materials). Append BM_LIGHTING_*
        // strings only when emissive_power > 0 (matches the H3
        // Blender exporter's `if material.ass_jms.power > 0:` gate).
        if let Some(mi_block) = root.field_path("material info").and_then(|f| f.as_block()) {
            for i in 0..mi_block.len() {
                if i >= self.materials.len() { break; }
                let mi = mi_block.element(i).unwrap();
                let power = mi.read_real("emissive power").unwrap_or(0.0);
                if power <= 0.0 { continue; }
                let color = mi.read_rgb("emissive color");
                let quality = mi.read_real("emissive quality").unwrap_or(0.0);
                let focus = mi.read_real("emissive focus").unwrap_or(0.0);
                let mat_flags = mi.read_int_any("flags").unwrap_or(0);
                let attenuation_enabled = (mat_flags & 0x0001) != 0;
                let atten_falloff = mi.read_real("attenuation falloff").unwrap_or(0.0);
                let atten_cutoff = mi.read_real("attenuation cutoff").unwrap_or(0.0);
                let frustum_blend = mi.read_real("frustum blend").unwrap_or(0.0);
                // Frustum angles stored as `angle` (radians) but
                // ASS writes degrees — mirror the cone-angle
                // convention used for stli lights.
                let frustum_falloff = mi.read_real("frustum falloff angle").unwrap_or(0.0).to_degrees();
                let frustum_cutoff = mi.read_real("frustum cutoffoff angle")
                    .or_else(|| mi.read_real("frustum cutoff angle"))
                    .unwrap_or(0.0).to_degrees();
                self.materials[i].bm_strings.push(format!(
                    "BM_LIGHTING_BASIC {:.10} {:.10} {:.10} {:.10} {:.10} 0 {:.10}",
                    power, color.red, color.green, color.blue, quality, focus,
                ));
                self.materials[i].bm_strings.push(format!(
                    "BM_LIGHTING_ATTEN {} {:.10} {:.10}",
                    if attenuation_enabled { 1 } else { 0 },
                    atten_falloff * SCALE, atten_cutoff * SCALE,
                ));
                self.materials[i].bm_strings.push(format!(
                    "BM_LIGHTING_FRUS {:.10} {:.10} {:.10}",
                    frustum_blend, frustum_falloff, frustum_cutoff,
                ));
            }
        }

        let defs = root.field_path("generic light definitions").and_then(|f| f.as_block());
        let insts = root.field_path("generic light instances").and_then(|f| f.as_block());
        let (defs, insts) = match (defs, insts) {
            (Some(d), Some(i)) => (d, i),
            _ => return Ok(()),  // No light blocks — silent skip
        };

        // Definition_index → object_index in self.objects[].
        let mut def_object_index: Vec<Option<i32>> = vec![None; defs.len()];
        for di in 0..defs.len() {
            let d = defs.element(di).unwrap();
            let kind = match d.read_int_any("type").unwrap_or(0) {
                0 => AssLightKind::OmniLgt,
                1 => AssLightKind::SpotLgt,
                2 => AssLightKind::DirectLgt,
                3 => AssLightKind::AmbientLgt,
                _ => AssLightKind::OmniLgt,
            };
            let color = d.read_rgb("color");
            let intensity = d.read_real("intensity").unwrap_or(0.0);
            // Tag stores cone angles in radians; ASS writes degrees.
            let hotspot_size = d.read_real("hotspot size").unwrap_or(0.0).to_degrees();
            let hotspot_falloff = d.read_real("hotspot falloff size").unwrap_or(0.0).to_degrees();
            let flags = d.read_int_any("flags").unwrap_or(0);
            let use_near = (flags & 0x0001) != 0;
            let use_far = (flags & 0x0002) != 0;
            let near = d.read_real_bounds("near attenuation bounds");
            let far = d.read_real_bounds("far attenuation bounds");
            let shape = d.read_int_any("shape").unwrap_or(1) as i32;
            let aspect = d.read_real("aspect").unwrap_or(1.0);

            let light = AssLight {
                kind, color, intensity,
                hotspot_size, hotspot_falloff,
                use_near_attenuation: use_near,
                near_atten_min: near.lower * SCALE,
                near_atten_max: near.upper * SCALE,
                use_far_attenuation: use_far,
                far_atten_min: far.lower * SCALE,
                far_atten_max: far.upper * SCALE,
                shape, aspect,
            };
            def_object_index[di] = Some(self.objects.len() as i32);
            self.objects.push(AssObject {
                xref_filepath: String::new(),
                xref_objectname: String::new(),
                payload: AssObjectPayload::GenericLight(light),
            });
        }

        // Per-instance INSTANCE records. Build the rotation from
        // (forward, left=cross(up, forward), up) — Halo stli only
        // stores forward+up; left is derived. Forward and up are
        // unit vectors, so cross is also unit (assuming orthonormal).
        for ii in 0..insts.len() {
            let inst = insts.element(ii).unwrap();
            let def_idx = inst.read_int_any("definition index").unwrap_or(-1);
            if def_idx < 0 || (def_idx as usize) >= def_object_index.len() { continue; }
            let Some(object_index) = def_object_index[def_idx as usize] else { continue; };
            // Schema: origin is `real_point_3d`, forward/up are `real_vector_3d`.
            let origin = inst.read_point3d("origin");
            let forward = inst.read_vec3("forward");
            let up = inst.read_vec3("up");
            let left = up.cross(forward);
            let rot = RealQuaternion::from_basis_columns(forward, left, up);
            self.instances.push(AssInstance {
                object_index,
                name: format!("light_{ii}"),
                unique_id: self.instances.len() as i32,
                parent_id: 0, // Scene Root
                inheritance_flag: 0,
                local_rotation: rot,
                local_translation: origin * SCALE,
                local_scale: 1.0,
                pivot_rotation: RealQuaternion::IDENTITY,
                pivot_translation: RealPoint3d::ZERO,
                pivot_scale: 1.0,
                bone_groups: Vec::new(),
            });
        }
        Ok(())
    }
}

//================================================================================
// Walkers
//================================================================================

fn read_materials(root: &TagStruct<'_>) -> Result<Vec<AssMaterial>, AssError> {
    let block = root.field_path("materials").and_then(|f| f.as_block())
        .ok_or(AssError::MissingField("materials"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let m = block.element(i).unwrap();
        let path = m.read_tag_ref_path("render method").unwrap_or_default();
        let shader_name = Path::new(&path.replace('\\', "/"))
            .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned();
        // Walk per-material properties[] — type enum 0 carries the
        // lightmap resolution (the only one we map for now;
        // photon_fidelity etc don't have explicit tag fields and
        // default to the artist convention `1` + all zeros).
        let mut lightmap_res: f32 = 1.0;
        if let Some(props) = m.field("properties").and_then(|f| f.as_block()) {
            for p in 0..props.len() {
                let prop = props.element(p).unwrap();
                let prop_type = prop.read_int_any("type").unwrap_or(-1);
                if prop_type == 0 {
                    if let Some(v) = prop.read_real("real-value") {
                        lightmap_res = v;
                    }
                }
            }
        }
        out.push(AssMaterial {
            name: shader_name,
            lightmap_variant: String::new(),
            bm_strings: vec![
                "BM_FLAGS 0000000000000000000000".to_owned(),
                format_bm_lmres(lightmap_res),
            ],
        });
    }
    Ok(out)
}

/// Build the BM_LMRES line for a material. Format (v4+, 11 numbers):
/// `BM_LMRES <res> <photon_fidelity> <2sided_tint(3)> <override(1)> <additive(3)> <gel(1)> <ignore_default_res(1)>`
/// The tag only carries resolution; everything else gets the
/// artist-default values that match shipped sources (photon_fidelity=1,
/// all other parameters zero).
fn format_bm_lmres(res: f32) -> String {
    format!(
        "BM_LMRES {:.10} 1 0.0000000000 0.0000000000 0.0000000000 0 0.0000000000 0.0000000000 0.0000000000 0 0",
        res,
    )
}

fn build_cluster_object(
    mesh: &TagStruct<'_>,
    mesh_pmt: &TagStruct<'_>,
    bounds: &CompressionBounds,
    flip_winding: bool,
) -> Result<AssObject, AssError> {
    let raw_v = mesh_pmt.field("raw vertices").and_then(|f| f.as_block());
    let raw_i = mesh_pmt.field("raw indices").and_then(|f| f.as_block());
    let parts = mesh.field("parts").and_then(|f| f.as_block());
    let subparts = mesh.field("subparts").and_then(|f| f.as_block());
    let (raw_v, raw_i, parts) = match (raw_v, raw_i, parts) {
        (Some(v), Some(i), Some(p)) => (v, i, p),
        _ => return Ok(empty_mesh()),
    };

    let indices: Vec<u16> = (0..raw_i.len())
        .filter_map(|k| raw_i.element(k))
        .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
        .collect();

    // H3 sbsp meshes are ALWAYS triangle lists — the schema's
    // `index buffer type` enum labels some meshes as "triangle strip"
    // (value 5), but probing of Guardian confirms list interpretation
    // scores 1.000 face-normal correlation while strip scores ~0.50
    // (random). We ignore the field and hardcode list mode for sbsp;
    // render_model still uses strips and lives in the JMS path
    // unaffected.

    // Walk subparts inside each part for index ranges. Each part
    // owns a contiguous (subpart_start, subpart_count) slice of the
    // subparts block; each subpart has its own (index_start,
    // index_count) and inherits the parent part's material. Empty
    // subparts (index_count == 0) drop out — preserves LOD grouping
    // without emitting zero-tri drawables. Plugin reference:
    // _mesh_decoder.py::_collect_parts.
    let mut tri_pool: Vec<(i32, u16, u16, u16)> = Vec::new();
    for pi in 0..parts.len() {
        let part = parts.element(pi).unwrap();
        let material_index = part.read_int_any("render method index").unwrap_or(0) as i32;
        let sub_start = part.read_int_any("subpart start").unwrap_or(0);
        let sub_count = part.read_int_any("subpart count").unwrap_or(0);
        // Read the part's own (start, count) too as a fallback when
        // subparts isn't present or is empty for this part.
        let part_start_i = part.read_int_any("index start").unwrap_or(0);
        let part_count_i = part.read_int_any("index count").unwrap_or(0);

        let mut emit_range = |start_i: i64, count_i: i64| {
            if count_i <= 0 { return; }
            let start = (start_i as i16 as u16) as usize;
            let count = count_i as usize;
            if start >= indices.len() { return; }
            let end = (start + count).min(indices.len());
            let slice = &indices[start..end];
            for chunk in slice.chunks_exact(3) {
                tri_pool.push((material_index, chunk[0], chunk[1], chunk[2]));
            }
        };

        if let Some(sps) = subparts.as_ref() {
            if sub_count > 0 {
                for sub_off in 0..sub_count as usize {
                    let si = sub_start as usize + sub_off;
                    if si >= sps.len() { break; }
                    let sp = sps.element(si).unwrap();
                    let s = sp.read_int_any("index start").unwrap_or(0);
                    let c = sp.read_int_any("index count").unwrap_or(0);
                    emit_range(s, c);
                }
                continue;
            }
        }
        // No subparts → fall back to the part's own range.
        emit_range(part_start_i, part_count_i);
    }

    if flip_winding {
        for (_, _a, b, c) in tri_pool.iter_mut() {
            std::mem::swap(b, c);
        }
    }

    // ASS triangles reference vertices by index, so we share them
    // across the whole cluster mesh (unlike JMS where every triangle
    // owns its own three vertex copies). Walk the unique vertex
    // indices, build a remap table, then translate triangles.
    let mut vertex_remap: HashMap<u16, u32> = HashMap::new();
    let mut vertices: Vec<AssVertex> = Vec::new();
    let mut triangles: Vec<AssTriangle> = Vec::with_capacity(tri_pool.len());
    for (mat, a, b, c) in tri_pool {
        let va = remap_vertex(&mut vertex_remap, &mut vertices, &raw_v, a, bounds);
        let vb = remap_vertex(&mut vertex_remap, &mut vertices, &raw_v, b, bounds);
        let vc = remap_vertex(&mut vertex_remap, &mut vertices, &raw_v, c, bounds);
        triangles.push(AssTriangle { material: mat, v: [va, vb, vc] });
    }

    Ok(AssObject {
        xref_filepath: String::new(),
        xref_objectname: String::new(),
        payload: AssObjectPayload::Mesh { vertices, triangles },
    })
}

fn remap_vertex(
    map: &mut HashMap<u16, u32>,
    out: &mut Vec<AssVertex>,
    raw_v: &crate::api::TagBlock<'_>,
    src_idx: u16,
    bounds: &CompressionBounds,
) -> u32 {
    if let Some(&existing) = map.get(&src_idx) { return existing; }
    let new_idx = out.len() as u32;
    let v = raw_v.element(src_idx as usize)
        .map(|e| read_vertex(&e, bounds))
        .unwrap_or_else(default_vertex);
    out.push(v);
    map.insert(src_idx, new_idx);
    new_idx
}

fn read_vertex(v: &TagStruct<'_>, bounds: &CompressionBounds) -> AssVertex {
    let raw_pos = v.read_point3d("position");
    let position = bounds.decompress_position(raw_pos) * SCALE;
    // The "normal" schema field is `real_point_3d` despite being a
    // direction.
    let normal = v.read_point3d("normal").as_vector();
    let raw_uv = v.read_point2d("texcoord");
    let uv = bounds.decompress_texcoord(raw_uv);
    AssVertex {
        position,
        normal,
        color: RealRgbColor::default(),
        node_set: Vec::new(),
        // V-flip + zero w (v5+ convention).
        uvs: vec![RealPoint3d { x: uv.x, y: 1.0 - uv.y, z: 0.0 }],
    }
}

fn empty_mesh() -> AssObject { AssObject::empty_mesh() }

fn default_vertex() -> AssVertex {
    AssVertex {
        position: RealPoint3d::ZERO,
        normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
        color: RealRgbColor::default(),
        node_set: Vec::new(),
        uvs: vec![RealPoint3d::ZERO],
    }
}


/// Reconstruct a convex polyhedron's mesh from its bounding planes
/// (each plane: `[i, j, k, d]` with `n·p + d = 0` and inside region
/// is `n·p + d <= 0`). Computes triple-plane intersections, filters
/// to those inside ALL planes (within an epsilon), then per face
/// gathers on-plane vertices, sorts radially around the face centroid,
/// and fan-triangulates. Vertices come out in centimeters.
fn polyhedron_from_planes(planes: &[RealPlane3d], material_index: i32) -> (Vec<AssVertex>, Vec<AssTriangle>) {
    let n = planes.len();
    if n < 4 { return (Vec::new(), Vec::new()); }

    // 1. Triple-plane intersections. `RealPlane3d::triple_intersection`
    // already honors Halo's `n·p + d = 0` convention.
    let mut candidates: Vec<RealPoint3d> = Vec::new();
    let eps = 1e-3_f32;
    for i in 0..n {
        for j in (i + 1)..n {
            for k in (j + 1)..n {
                if let Some(p) = RealPlane3d::triple_intersection(planes[i], planes[j], planes[k]) {
                    // Filter: inside the region within epsilon
                    // (`n·p + d <= 0` for every plane).
                    let mut inside = true;
                    for plane in planes {
                        let d = plane.normal().dot(p.as_vector()) + plane.d;
                        if d > eps { inside = false; break; }
                    }
                    if inside { candidates.push(p); }
                }
            }
        }
    }
    if candidates.len() < 4 { return (Vec::new(), Vec::new()); }

    // Dedup vertices that are within epsilon of an existing one,
    // preserving insertion order so face triangulation stays stable.
    let mut unique: Vec<RealPoint3d> = Vec::new();
    let dedup_eps_sq = (eps * 10.0).powi(2);
    for c in &candidates {
        let mut dup = false;
        for u in &unique {
            if c.distance_squared_to(*u) < dedup_eps_sq { dup = true; break; }
        }
        if !dup { unique.push(*c); }
    }

    // Build vertex list (× SCALE for cm).
    let vertices: Vec<AssVertex> = unique.iter().map(|p| AssVertex {
        position: *p * SCALE,
        normal: RealVector3d { i: 0.0, j: 0.0, k: 1.0 },
        color: RealRgbColor::default(),
        node_set: Vec::new(),
        uvs: vec![RealPoint3d::ZERO],
    }).collect();

    // 2. Per face, gather vertices that lie on this plane (within
    // a slightly looser epsilon since the dedup step may have
    // shifted positions). Sort radially around the centroid using
    // an in-plane basis. Fan-triangulate.
    let mut tris: Vec<AssTriangle> = Vec::new();
    let face_eps = eps * 100.0;
    for plane in planes {
        let normal = plane.normal();
        let mut on_plane: Vec<u32> = Vec::new();
        for (vi, p) in unique.iter().enumerate() {
            let d = (normal.dot(p.as_vector()) + plane.d).abs();
            if d < face_eps { on_plane.push(vi as u32); }
        }
        if on_plane.len() < 3 { continue; }
        // Centroid + in-plane basis.
        let mut centroid = RealPoint3d::ZERO;
        for &vi in &on_plane {
            let p = unique[vi as usize];
            centroid = RealPoint3d {
                x: centroid.x + p.x,
                y: centroid.y + p.y,
                z: centroid.z + p.z,
            };
        }
        let inv = 1.0 / on_plane.len() as f32;
        centroid = centroid * inv;
        // Pick any reference axis perpendicular to normal.
        let perp_seed = if normal.i.abs() < 0.9 {
            RealVector3d { i: 1.0, j: 0.0, k: 0.0 }
        } else {
            RealVector3d { i: 0.0, j: 1.0, k: 0.0 }
        };
        let u_axis = normal.cross(perp_seed).normalized();
        let v_axis = normal.cross(u_axis).normalized();
        // Sort by angle.
        let mut with_angle: Vec<(f32, u32)> = on_plane.iter().map(|&vi| {
            let offset = unique[vi as usize] - centroid;
            let u = u_axis.dot(offset);
            let v = v_axis.dot(offset);
            (v.atan2(u), vi)
        }).collect();
        with_angle.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let sorted: Vec<u32> = with_angle.into_iter().map(|(_, vi)| vi).collect();
        // Fan from first vertex.
        for k in 1..sorted.len() - 1 {
            tris.push(AssTriangle {
                material: material_index,
                v: [sorted[0], sorted[k], sorted[k + 1]],
            });
        }
    }
    (vertices, tris)
}

/// Find or append a "special" material (used by recompile-marker
/// meshes — `+portal`, `+weather`, `@collision_only`, etc). Returns
/// the material's final index. The marker name itself goes into the
/// material's `name` slot so Tool.exe re-recognises it on import.
fn ensure_special_material(materials: &mut Vec<AssMaterial>, marker: &str) -> usize {
    if let Some(idx) = materials.iter().position(|m| m.name == marker) {
        return idx;
    }
    materials.push(AssMaterial {
        name: marker.to_owned(),
        lightmap_variant: String::new(),
        bm_strings: vec![
            "BM_FLAGS 0000000000000000000000".to_owned(),
            "BM_LMRES 1.0000000000 1 0.0000000000 0.0000000000 0.0000000000 0 0.0000000000 0.0000000000 0.0000000000 0 0".to_owned(),
        ],
    });
    materials.len() - 1
}

/// Build a content-comparison key for an AssObject's MESH payload.
/// Two definitions whose decompressed vertices+triangles are
/// byte-identical produce the same key, so we collapse them to one
/// shared OBJECT. Non-MESH payloads return an empty key (no
/// dedup — lights/spheres are all kept distinct).
fn object_content_key(obj: &AssObject) -> Vec<u8> {
    match &obj.payload {
        AssObjectPayload::Mesh { vertices, triangles } => {
            let mut key = Vec::with_capacity(vertices.len() * 12 + triangles.len() * 16);
            for v in vertices {
                key.extend_from_slice(&v.position.x.to_le_bytes());
                key.extend_from_slice(&v.position.y.to_le_bytes());
                key.extend_from_slice(&v.position.z.to_le_bytes());
            }
            for t in triangles {
                key.extend_from_slice(&t.material.to_le_bytes());
                key.extend_from_slice(&t.v[0].to_le_bytes());
                key.extend_from_slice(&t.v[1].to_le_bytes());
                key.extend_from_slice(&t.v[2].to_le_bytes());
            }
            key
        }
        _ => Vec::new(),
    }
}

/// Detect odd-count axis flips on a CompressionBounds. When an odd
/// number of `(min, max)` axis pairs has `max < min`, the
/// position-unpacker's Jacobian has negative determinant and stored
/// triangle winding inverts vs the vertex normals. Caller swaps b/c
/// per triangle to compensate.
fn compute_axis_flip(b: &CompressionBounds) -> bool {
    if !b.pos_compressed { return false; }
    let flips = (b.px_max < b.px_min) as u32
        + (b.py_max < b.py_min) as u32
        + (b.pz_max < b.pz_min) as u32;
    flips % 2 == 1
}

fn write_floats<W: Write>(w: &mut W, values: &[f32]) -> io::Result<()> {
    for (i, v) in values.iter().enumerate() {
        let v = if *v == -0.0 { 0.0 } else { *v };
        if i + 1 < values.len() {
            write!(w, "{:.10}\t", v)?;
        } else {
            writeln!(w, "{:.10}", v)?;
        }
    }
    Ok(())
}

/// One render_model node in the parent-relative form the tag stores.
/// Used by [`AssFile::from_render_model`] to emit one bone INSTANCE
/// per node, parented through the node hierarchy. Same shape as
/// [`crate::JmsNode`] but kept private here so the ass module
/// doesn't have to depend on the jms module.
#[derive(Debug, Clone)]
struct RmNode {
    name: String,
    parent: i32,
    rotation: RealQuaternion,
    translation: RealPoint3d,
}

fn read_rm_nodes_local(root: &TagStruct<'_>) -> Result<Vec<RmNode>, AssError> {
    let block = root.field_path("nodes").and_then(|f| f.as_block())
        .ok_or(AssError::MissingField("nodes"))?;
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        let name = n.read_string_id("name").unwrap_or_default();
        let parent = n.read_block_index("parent node") as i32;
        let default = n.field("default").and_then(|f| f.as_struct());
        let (rotation, translation) = if let Some(d) = default {
            (d.read_quat("rotation"), d.read_point3d("translation"))
        } else {
            (RealQuaternion::IDENTITY, RealPoint3d::ZERO)
        };
        out.push(RmNode { name, parent, rotation, translation });
    }
    Ok(out)
}

/// Build one MESH OBJECT for a render_model mesh, walking
/// `parts × subparts` and converting strips → lists. Mirrors
/// [`crate::JmsFile::from_render_model`]'s `build_geometry` but
/// emits ASS vertices/triangles into a single object instead of a
/// global pool.
///
/// `cell_label` becomes the "(slot) <label>" material_name suffix —
/// for region/permutation cells we pass `"<perm> <region>"`; for the
/// instance mesh we pass a placement-derived label.
fn build_render_model_object(
    mesh: &TagStruct<'_>,
    mesh_pmt: &TagStruct<'_>,
    mats_block: &crate::api::TagBlock<'_>,
    bounds: &CompressionBounds,
    materials: &mut Vec<AssMaterial>,
    cell_label: &str,
) -> Result<AssObject, AssError> {
    let raw_v = mesh_pmt.field("raw vertices").and_then(|f| f.as_block());
    let raw_i = mesh_pmt.field("raw indices").and_then(|f| f.as_block());
    let parts = mesh.field("parts").and_then(|f| f.as_block());
    let (raw_v, raw_i, parts) = match (raw_v, raw_i, parts) {
        (Some(v), Some(i), Some(p)) => (v, i, p),
        _ => return Ok(empty_mesh()),
    };

    let indices: Vec<u16> = (0..raw_i.len())
        .filter_map(|k| raw_i.element(k))
        .map(|e| e.read_int_any("word").unwrap_or(0) as u16)
        .collect();

    // render_model meshes are TRIANGLE STRIPS (default for H3 MCC),
    // unlike sbsp which is always lists. The schema enum is
    // `index buffer type` — value 5 = strip.
    let is_strip = mesh.field("index buffer type")
        .and_then(|f| f.value())
        .map(|v| matches!(v, crate::TagFieldData::CharEnum { name: Some(n), .. } if n == "triangle strip"))
        .unwrap_or(true);

    let mut vertex_remap: HashMap<u16, u32> = HashMap::new();
    let mut vertices: Vec<AssVertex> = Vec::new();
    let mut triangles: Vec<AssTriangle> = Vec::new();

    for pi in 0..parts.len() {
        let part = parts.element(pi).unwrap();
        let shader_idx = part.read_int_any("render method index").unwrap_or(0);
        let shader_name = if shader_idx >= 0 && (shader_idx as usize) < mats_block.len() {
            let m = mats_block.element(shader_idx as usize).unwrap();
            let path = m.read_tag_ref_path("render method").unwrap_or_default();
            Path::new(&path.replace('\\', "/"))
                .file_stem().and_then(|s| s.to_str()).unwrap_or("default").to_owned()
        } else {
            "default".to_owned()
        };

        // Material lookup / append: same dedup rule as the JMS path —
        // `(shader_name, cell_label)` cells share a material slot.
        let material_index = match materials.iter().position(|m|
            m.name == shader_name && m.lightmap_variant == cell_label
        ) {
            Some(idx) => idx as i32,
            None => {
                let _slot = materials.len() + 1;
                materials.push(AssMaterial {
                    name: shader_name,
                    lightmap_variant: cell_label.to_owned(),
                    bm_strings: Vec::new(),
                });
                (materials.len() - 1) as i32
            }
        };

        // Subparts → index ranges (LOD-grouped). Falls back to the
        // part's own (start, count) when subparts is missing or the
        // part has no subpart range.
        let part_start_i = part.read_int_any("index start").unwrap_or(0);
        let part_count_i = part.read_int_any("index count").unwrap_or(0);
        let sub_start = part.read_int_any("subpart start").unwrap_or(0);
        let sub_count = part.read_int_any("subpart count").unwrap_or(0);
        let subparts = mesh.field("subparts").and_then(|f| f.as_block());

        let emit_range = |start_i: i64, count_i: i64,
                           vertices: &mut Vec<AssVertex>,
                           triangles: &mut Vec<AssTriangle>,
                           vertex_remap: &mut HashMap<u16, u32>| {
            if count_i <= 0 { return; }
            // `index start` is signed-i16 but functionally u16 — wrap.
            let start = (start_i as i16 as u16) as usize;
            let count = count_i as usize;
            if start >= indices.len() { return; }
            let end = (start + count).min(indices.len());
            let slice = &indices[start..end];
            let tris: Vec<(u16, u16, u16)> = if is_strip {
                crate::geometry::strip_to_list(slice)
            } else {
                slice.chunks_exact(3).map(|c| (c[0], c[1], c[2])).collect()
            };
            for (a, b, c) in tris {
                let va = remap_vertex(vertex_remap, vertices, &raw_v, a, bounds);
                let vb = remap_vertex(vertex_remap, vertices, &raw_v, b, bounds);
                let vc = remap_vertex(vertex_remap, vertices, &raw_v, c, bounds);
                triangles.push(AssTriangle { material: material_index, v: [va, vb, vc] });
            }
        };

        if let Some(sps) = subparts.as_ref() {
            if sub_count > 0 {
                for sub_off in 0..sub_count as usize {
                    let si = sub_start as usize + sub_off;
                    if si >= sps.len() { break; }
                    let sp = sps.element(si).unwrap();
                    let s = sp.read_int_any("index start").unwrap_or(0);
                    let c = sp.read_int_any("index count").unwrap_or(0);
                    emit_range(s, c, &mut vertices, &mut triangles, &mut vertex_remap);
                }
                continue;
            }
        }
        emit_range(part_start_i, part_count_i, &mut vertices, &mut triangles, &mut vertex_remap);
    }

    Ok(AssObject {
        xref_filepath: String::new(),
        xref_objectname: String::new(),
        payload: AssObjectPayload::Mesh { vertices, triangles },
    })
}
