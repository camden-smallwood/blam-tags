//! JMS → `render_model`, without `tool.exe`.
//!
//! # The shape of it
//!
//! A JMS has no mesh concept: one flat vertex list, one flat triangle
//! list, and a material index per triangle. Sections come entirely from
//! the material *definition line* — every distinct
//! `(lod, permutation, region)` triple becomes one mesh. Verified against
//! the shipped corpus: grouping this way reproduces the built tag's mesh
//! count exactly on 86 of 87 comparable models.
//!
//! The pipeline is:
//!
//! 1. **Weld** the whole model at once ([`crate::weld`]) — the point
//!    welder runs across sections, so it cannot be done per mesh.
//! 2. **Tangent space** ([`crate::tangent`]) over the welded vertices.
//! 3. **Split into meshes** by material label, remapping to a local
//!    vertex list per mesh.
//! 4. **Stripify** ([`crate::strip`]) — H3 ships triangle strips
//!    exclusively.
//! 5. **Encode** positions and texcoords against one model-global
//!    bounding box.
//! 6. Write the tag.
//!
//! # Encoding
//!
//! There is no GPU packing on the PC MCC path — `raw vertices` are plain
//! `float32`. The only compression is an affine remap onto `[0,1]`:
//!
//! ```text
//! stored = (world - lo) / (hi - lo)
//! ```
//!
//! with `lo`/`hi` the **exact per-axis min and max over every vertex of
//! every mesh, after welding** — no padding and no snapping — then
//! The bounds are taken from the vertices as they arrive, **before**
//! welding. The welder moves a survivor to the mean of everything on it,
//! which pulls the extremes inward; tool's own compression bounds show no
//! such pull, reproducing the raw JMS bounding box to 1e-9 on every model
//! whose source matches its tag. Computing them from welded positions
//! agreed with tool on 86 of 110 shipped models; from the source, 101.
//!
//! `hi = max(hi, lo + 0.01)`. That last clamp is not an edge case: 25 of
//! 200 sampled tags have an axis exactly 0.01 wide, because decals and
//! panels are flat and without it the divide is by zero.
//!
//! `compression info` is **model-global**. The block permits 65,536
//! elements but the `mesh` block has no field selecting one, every
//! shipped tag has exactly one, and the writer asserts index 0 — so one
//! is written and the bounds cover the whole model.
//!
//! Positions scale by 0.01 on the way in, and the texcoord V is flipped
//! (`v := 1 - v`). Both proven against shipped tags to the last digit.
//!
//! # What is not written
//!
//! **PRT.** Ambient transfer is solved here — see [`crate::prt`] — and
//! written as `PRT vertex type = 1` with the coefficients in
//! `per_mesh_prt_data`. Linear and quadratic are not: those are the same
//! visibility projected onto more spherical-harmonic bands, and together
//! they are 31% of shipped meshes against ambient's 48%.
//!
//! Passing `prt_samples: None` writes `PRT vertex type = 0`
//! (`No PRT`), which 21% of shipped meshes also use, so it is
//! a shape the engine already handles — but a model imported this way
//! will not have PRT self-shadowing until that is built.
//!
//! Also not written: instance placements, sky lights, volume samples,
//! and the `errors` block.

use std::collections::BTreeMap;
use std::path::Path;

use crate::jms::JmsFile;
use crate::jms_split::{material_base_name, material_is_precise, MaterialLabel};
use crate::math::{RealPoint2d, RealPoint3d, RealVector3d};
use crate::strip::stripify;
use crate::tangent;
use crate::weld::{weld_sectioned, WeldTolerances, WeldVertex};
use crate::{TagFieldData, TagFile};

/// JMS units are hundredths of a world unit.
pub const JMS_TO_WORLD: f32 = 0.01;
/// The minimum width the compression bounds are widened to.
pub const MIN_BOUND_WIDTH: f32 = 0.01;
/// The per-mesh vertex ceiling.
///
/// 65,535, which is what the format allows: `raw_vertex_block`'s
/// `max_count` is `UNSIGNED_SHORT_MAX`. The 32,767 this used to sit at
/// is `SHORT_MAX`, and that cap belongs to `subpart_block` rather than
/// to vertices — half the real ceiling, refusing models the format
/// holds perfectly well.
///
/// An index may then reach 65,534. `0xFFFF` stays out of reach as the
/// strip-restart sentinel, which is exactly where the packer draws its
/// own line.
pub const MAX_VERTICES_PER_MESH: usize = 65_535;
/// A mesh gets at most this many indices.
pub const MAX_INDICES_PER_MESH: usize = 65_535;
/// The most indices one part may name.
///
/// `index count` is a `short_integer`. Past 32,767 it writes negative,
/// and a reader that treats a non-positive count as an empty range drops
/// the run without a word — the same silent loss that cost a mesh on the
/// structure side. Even, so a strip chunk keeps its winding parity.
const MAX_INDICES_PER_PART: usize = 32_766;

/// How to interpret the JMS.
#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub scale: f32,
    /// Weld tolerances. See [`crate::weld`].
    pub weld: WeldTolerances,
    /// Refuse a mesh above this many welded vertices. Defaults to
    /// `tool.exe`'s own limit; the format permits 65,535.
    pub max_vertices_per_mesh: usize,
    /// Rays per vertex for the ambient PRT solve, or `None` to write
    /// every mesh as `No PRT`.
    ///
    /// 21% of shipped meshes really are `No PRT`, so that is a legitimate
    /// output and not just a stub — but 48% are `PRT Ambient`, which is
    /// what this computes.
    pub prt_samples: Option<usize>,
    /// Spherical-harmonic order for the PRT solve: 0 ambient, 1 linear,
    /// 2 quadratic. Shipped meshes are 48% ambient, 8% linear and 23%
    /// quadratic, so ambient is both the cheapest and the commonest.
    pub prt_order: u32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            scale: JMS_TO_WORLD,
            weld: WeldTolerances::precise(),
            max_vertices_per_mesh: MAX_VERTICES_PER_MESH,
            prt_samples: Some(64),
            prt_order: 0,
        }
    }
}

/// Why a `render_model` could not be written.
#[derive(Debug, Clone, PartialEq)]
pub enum RenderError {
    Schema(String),
    MissingField(String),
    /// A mesh exceeds the per-section vertex limit. Split it in the
    /// source — see [`crate::jms_split`].
    MeshTooLarge { region: String, permutation: String, vertices: usize, max: usize },
    /// A mesh needs more indices than the format can hold.
    TooManyIndices { region: String, permutation: String, indices: usize },
    /// More regions than the engine allows. This one is real: three
    /// enforcement sites including the runtime.
    TooManyRegions(usize),
    /// More nodes than a u8 node map can address.
    TooManyNodes(usize),
    BadMaterial(i32),
    Empty,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(m) => write!(f, "{m}"),
            Self::MissingField(m) => write!(f, "the schema has no field {m:?}"),
            Self::MeshTooLarge { region, permutation, vertices, max } => write!(
                f,
                "mesh '{permutation} {region}' has {vertices} welded vertices, over the {max} \
                 limit. Split it across extra regions — `split-jms` does this."
            ),
            Self::TooManyIndices { region, permutation, indices } => write!(
                f,
                "mesh '{permutation} {region}' needs {indices} indices, over the 65535 a mesh \
                 can hold. Split it across extra regions."
            ),
            Self::TooManyRegions(n) => {
                write!(f, "{n} regions, but the engine allows 16")
            }
            Self::TooManyNodes(n) => write!(f, "{n} nodes, but node maps are u8 and allow 255"),
            Self::BadMaterial(i) => write!(f, "material index {i} does not exist"),
            Self::Empty => write!(f, "no triangles to import"),
        }
    }
}

impl std::error::Error for RenderError {}

type R<T> = Result<T, RenderError>;

/// What [`render_model_from_jms`] produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderReport {
    pub regions: usize,
    pub permutations: usize,
    pub meshes: usize,
    pub materials: usize,
    pub nodes: usize,
    pub markers: usize,
    /// Vertices before and after welding.
    pub source_vertices: usize,
    pub welded_vertices: usize,
    pub triangles: usize,
    pub indices: usize,
    pub largest_mesh_vertices: usize,
    /// Per built mesh, `(source triangles, welded vertices, indices)`.
    ///
    /// The index run is what binds first on dense meshes, and how many
    /// indices a triangle costs depends on how well it strips. Recorded
    /// so [`crate::jms_split::SplitBudget::indices_per_triangle`] can be
    /// fitted against real content rather than assumed.
    pub per_mesh: Vec<(usize, usize, usize)>,
    /// Vertices the PRT solve covered, and their mean visibility. Zero
    /// vertices means no PRT was written.
    pub prt_vertices: usize,
    pub prt_mean_visibility: f32,
    /// 0 ambient, 1 linear, 2 quadratic.
    pub prt_order: u32,
    pub skipped: Vec<String>,
}

/// One mesh's geometry, after welding and splitting.
struct Mesh {
    region: String,
    permutation: String,
    /// Indices into the global welded vertex list.
    vertices: Vec<u32>,
    /// One part per material used, in first-use order.
    parts: Vec<(i16, Vec<[u32; 3]>)>,
}

/// Build a `render_model` tag from a JMS scene.
pub fn render_model_from_jms(
    jms: &JmsFile,
    schema: &Path,
    opts: &RenderOptions,
) -> R<(TagFile, RenderReport)> {
    if jms.triangles.is_empty() {
        return Err(RenderError::Empty);
    }
    if jms.nodes.len() > 255 {
        return Err(RenderError::TooManyNodes(jms.nodes.len()));
    }
    let mut report = RenderReport {
        source_vertices: jms.vertices.len(),
        triangles: jms.triangles.len(),
        ..Default::default()
    };

    // ---- weld -------------------------------------------------------
    // Scale first: the tolerances are in world units.
    let s = opts.scale;
    let source: Vec<WeldVertex> = jms
        .vertices
        .iter()
        .map(|v| WeldVertex {
            position: RealPoint3d { x: v.position.x * s, y: v.position.y * s, z: v.position.z * s },
            normal: v.normal,
            // V is flipped on the way in, and it is flipped here rather
            // than at encode time so the welder compares what the tag
            // will actually store.
            texcoords: v.uvs.iter().map(|t| RealPoint2d { x: t.x, y: 1.0 - t.y }).collect(),
            influences: v.node_sets.clone(),
            color: v.color,
        })
        .collect();

    let extent = {
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for v in &source {
            for (a, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                lo[a] = lo[a].min(c);
                hi[a] = hi[a].max(c);
            }
        }
        (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a]))
    };
    // Which section each vertex belongs to, for the welder's second
    // pass. A vertex used by two sections belongs to neither, which is
    // what tool does to a point the moment a weld crosses a section.
    let section_of_vertex: Vec<i32> = {
        let labels: Vec<MaterialLabel> =
            jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
        let mut keys: Vec<(String, String, String)> = Vec::new();
        let mut of = vec![i32::MIN; jms.vertices.len()];
        for tri in &jms.triangles {
            let Some(label) = labels.get(usize::try_from(tri.material).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let key = label.section_key();
            let idx = match keys.iter().position(|k| *k == key) {
                Some(i) => i as i32,
                None => {
                    keys.push(key);
                    keys.len() as i32 - 1
                }
            };
            for &v in &tri.v {
                let Some(slot) = of.get_mut(v as usize) else { continue };
                *slot = if *slot == i32::MIN || *slot == idx { idx } else { -1 };
            }
        }
        of.iter().map(|s| if *s == i32::MIN { -1 } else { *s }).collect()
    };

    // Which vertices came from a precise material. `sub_1400FE9E0`:
    // for each triangle whose material carries the flag, mark all three
    // corners — and their points — precise, so they weld only at the
    // tight tolerance.
    let precise_vertex: Vec<bool> = {
        let precise_material: Vec<bool> =
            jms.materials.iter().map(|m| material_is_precise(&m.name)).collect();
        let mut of = vec![false; jms.vertices.len()];
        for tri in &jms.triangles {
            if !precise_material
                .get(usize::try_from(tri.material).unwrap_or(usize::MAX))
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
            for &v in &tri.v {
                if let Some(slot) = of.get_mut(v as usize) {
                    *slot = true;
                }
            }
        }
        of
    };

    // The coarse tolerance scales with the model, so it can only be
    // settled once the geometry is in hand.
    let tol = WeldTolerances {
        coarse_position: crate::weld::coarse_position_tolerance(extent),
        ..opts.weld
    };
    let welded = weld_sectioned(&source, &tol, &precise_vertex, &section_of_vertex);
    report.welded_vertices = welded.vertices.len();

    // ---- tangent space ----------------------------------------------
    let positions: Vec<RealPoint3d> = welded.vertices.iter().map(|v| v.position).collect();
    let normals: Vec<RealVector3d> = welded.vertices.iter().map(|v| v.normal).collect();
    let uv0: Vec<RealPoint2d> = welded
        .vertices
        .iter()
        .map(|v| v.texcoords.first().copied().unwrap_or(RealPoint2d { x: 0.0, y: 0.0 }))
        .collect();
    let global_tris: Vec<[u32; 3]> = jms
        .triangles
        .iter()
        .map(|t| {
            [
                welded.remap[t.v[0] as usize],
                welded.remap[t.v[1] as usize],
                welded.remap[t.v[2] as usize],
            ]
        })
        .collect();
    let basis = tangent::build(&positions, &normals, &uv0, &global_tris);

    // ---- split into meshes ------------------------------------------
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();

    // Section key -> triangles, in first-use order so output is stable.
    let mut order: Vec<(String, String, String)> = Vec::new();
    let mut buckets: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    let mut discarded = 0usize;
    for (i, tri) in jms.triangles.iter().enumerate() {
        // Tool treats an out-of-range material as a *repair*, not a
        // failure: it reports "had N triangles with invalid material
        // indices" and drops them. Match that rather than refusing a
        // model Tool would import.
        let Some(label) = labels.get(usize::try_from(tri.material).unwrap_or(usize::MAX)) else {
            discarded += 1;
            continue;
        };
        let key = label.section_key();
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(i);
    }

    if discarded > 0 {
        report.skipped.push(format!(
            "{discarded} triangle(s) discarded for an invalid material index"
        ));
    }

    let mut meshes: Vec<Mesh> = Vec::new();
    for key in &order {
        let tri_ids = &buckets[key];
        // Local vertex list, in first-use order.
        let mut local: BTreeMap<u32, u32> = BTreeMap::new();
        let mut vertices: Vec<u32> = Vec::new();
        let mut by_material: BTreeMap<i16, Vec<[u32; 3]>> = BTreeMap::new();
        let mut material_order: Vec<i16> = Vec::new();

        for &t in tri_ids {
            let g = global_tris[t];
            let mut l = [0u32; 3];
            for k in 0..3 {
                let next = vertices.len() as u32;
                let idx = *local.entry(g[k]).or_insert_with(|| {
                    vertices.push(g[k]);
                    next
                });
                l[k] = idx;
            }
            let m = jms.triangles[t].material as i16;
            if !by_material.contains_key(&m) {
                material_order.push(m);
            }
            by_material.entry(m).or_default().push(l);
        }

        if vertices.len() > opts.max_vertices_per_mesh {
            return Err(RenderError::MeshTooLarge {
                region: key.2.clone(),
                permutation: key.1.clone(),
                vertices: vertices.len(),
                max: opts.max_vertices_per_mesh,
            });
        }

        let parts: Vec<(i16, Vec<[u32; 3]>)> =
            material_order.iter().map(|m| (*m, by_material[m].clone())).collect();
        meshes.push(Mesh {
            region: key.2.clone(),
            permutation: key.1.clone(),
            vertices,
            parts,
        });
    }
    report.meshes = meshes.len();
    report.largest_mesh_vertices = meshes.iter().map(|m| m.vertices.len()).max().unwrap_or(0);

    // Only materials a triangle actually references become tag
    // materials, and part indices are remapped onto that compacted list.
    let mut used: Vec<i16> = Vec::new();
    for m in &meshes {
        for (mat, _) in &m.parts {
            if !used.contains(mat) {
                used.push(*mat);
            }
        }
    }
    used.sort_unstable();
    // Two JMS materials can name one shader. The symbol runs on a name
    // are flags — `rubber` and `rubber %` are the same material with
    // different bits set — so a tag material is one distinct *name*, not
    // one distinct JMS entry. Compacting by entry emitted 17 materials
    // for masterchief where tool emits 11.
    let base_of = |m: i16| -> &str {
        jms.materials
            .get(m.max(0) as usize)
            .map(|x| material_base_name(&x.name))
            .unwrap_or("")
    };
    let mut names: Vec<&str> = Vec::new();
    for &m in &used {
        let b = base_of(m);
        if !names.contains(&b) {
            names.push(b);
        }
    }
    let material_slot = |m: i16| -> i16 {
        let b = base_of(m);
        names.iter().position(|u| *u == b).map(|p| p as i16).unwrap_or(0)
    };
    for m in &mut meshes {
        for (mat, _) in &mut m.parts {
            *mat = material_slot(*mat);
        }
    }

    // ---- compression bounds, over every welded vertex ---------------
    // Only vertices a mesh actually references. A welded vertex left
    // behind by a discarded triangle would otherwise stretch the box,
    // and the bounds are supposed to be the exact extent of what ships.
    let mut referenced = vec![false; welded.vertices.len()];
    for m in &meshes {
        for &g in &m.vertices {
            referenced[g as usize] = true;
        }
    }
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    let (mut ulo, mut uhi) = ([f32::MAX; 2], [f32::MIN; 2]);
    // Positions come from *before* welding. The welder moves a survivor
    // to the mean of everything on it, which pulls the extremes inward,
    // and tool's own bounds do not show that — they reproduce the raw
    // JMS bounding box to 1e-9 on every model whose source matches its
    // tag. Taking them from the welded positions was invisible while the
    // only stage was the tight one and moved almost nothing; adding the
    // coarse stage made it visible as 24 models whose bounds no longer
    // matched tool's.
    for (i, sv) in source.iter().enumerate() {
        let w = welded.remap[i] as usize;
        if !referenced.get(w).copied().unwrap_or(false) {
            continue;
        }
        for (a, c) in [sv.position.x, sv.position.y, sv.position.z].into_iter().enumerate() {
            lo[a] = lo[a].min(c);
            hi[a] = hi[a].max(c);
        }
    }
    for (vi, v) in welded.vertices.iter().enumerate() {
        if !referenced[vi] {
            continue;
        }
        if let Some(t) = v.texcoords.first() {
            ulo[0] = ulo[0].min(t.x);
            uhi[0] = uhi[0].max(t.x);
            ulo[1] = ulo[1].min(t.y);
            uhi[1] = uhi[1].max(t.y);
        }
    }
    for a in 0..3 {
        if !lo[a].is_finite() {
            lo[a] = 0.0;
            hi[a] = MIN_BOUND_WIDTH;
        }
        hi[a] = hi[a].max(lo[a] + MIN_BOUND_WIDTH);
    }
    for a in 0..2 {
        if !ulo[a].is_finite() {
            ulo[a] = 0.0;
            uhi[a] = MIN_BOUND_WIDTH;
        }
        uhi[a] = uhi[a].max(ulo[a] + MIN_BOUND_WIDTH);
    }

    // ---- write ------------------------------------------------------
    let mut tag = TagFile::new(schema).map_err(|e| {
        RenderError::Schema(format!("cannot create a render_model from {}: {e}", schema.display()))
    })?;

    write_nodes(&mut tag, jms)?;
    report.nodes = jms.nodes.len();
    report.markers = write_markers(&mut tag, jms)?;
    report.materials = write_materials(&mut tag, names.len())?;
    write_regions(&mut tag, &meshes, &mut report)?;
    write_compression(&mut tag, lo, hi, ulo, uhi)?;

    // Ambient transfer, solved once over the whole model rather than
    // per mesh: a vertex is shadowed by everything around it, not only by
    // the mesh it happens to belong to.
    let order = opts.prt_order.min(2);
    let visibility: Option<Vec<Vec<f32>>> = opts.prt_samples.filter(|s| *s > 0).map(|samples| {
        let pos: Vec<RealPoint3d> = welded.vertices.iter().map(|v| v.position).collect();
        let nrm: Vec<RealVector3d> = welded.vertices.iter().map(|v| v.normal).collect();
        crate::prt::sh_transfer(
            &pos,
            &nrm,
            &global_tris,
            order,
            &crate::prt::PrtOptions { samples, ..Default::default() },
        )
    });
    if let Some(v) = &visibility {
        // The order-0 coefficient over Y00 is the visibility, whatever
        // the order — a useful single number to report.
        report.prt_mean_visibility =
            v.iter().map(|c| c[0] / crate::prt::Y00).sum::<f32>() / v.len().max(1) as f32;
        report.prt_vertices = v.len();
        report.prt_order = order;
    }

    let mut total_indices = 0usize;
    for (i, mesh) in meshes.iter().enumerate() {
        let indices =
            write_mesh(
                &mut tag,
                mesh,
                &welded,
                &basis,
                jms,
                lo,
                hi,
                ulo,
                uhi,
                i,
                if visibility.is_some() { order as i8 + 1 } else { 0 },
            )?;
        let triangles: usize = mesh.parts.iter().map(|(_, t)| t.len()).sum();
        report.per_mesh.push((triangles, mesh.vertices.len(), indices));
        total_indices += indices;
    }
    report.indices = total_indices;

    // The coefficients themselves, in each mesh's own vertex order.
    if let Some(visibility) = &visibility {
        let mut root = tag.root_mut();
        let mut fld = root
            .field_path_mut("render geometry/per_mesh_prt_data")
            .ok_or_else(|| RenderError::MissingField("render geometry/per_mesh_prt_data".into()))?;
        let mut block = fld
            .as_block_mut()
            .ok_or_else(|| RenderError::MissingField("per_mesh_prt_data".into()))?;
        for mesh in &meshes {
            let vis: Vec<Vec<f32>> =
                mesh.vertices.iter().map(|g| visibility[*g as usize].clone()).collect();
            let bytes = crate::prt::sh_pca_data(&vis);
            let i = block.add_element();
            let mut e = block.element_mut(i).expect("just added");
            if let Some(mut data) = e.field_mut("mesh pca data") {
                let _ = data.set(TagFieldData::Data(bytes.clone()));
            }
        }
    }

    // `runtime flags = 0x4` on 193 of 200 sampled tags; bit 0 is never
    // set on any of them.
    {
        let mut root = tag.root_mut();
        let mut fld = root
            .field_path_mut("render geometry/runtime flags")
            .ok_or_else(|| RenderError::MissingField("render geometry/runtime flags".into()))?;
        let _ = fld.set(TagFieldData::LongFlags { value: 4, names: Vec::new() });
    }
    if visibility.is_none() {
        report.skipped.push("PRT — meshes are written as `No PRT`".into());
    }

    Ok((tag, report))
}

// ---------------------------------------------------------------- helpers

fn with_block<T>(
    root: &mut crate::TagStructMut<'_>,
    path: &str,
    f: impl FnOnce(&mut crate::TagBlockMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = root
        .field_path_mut(path)
        .ok_or_else(|| RenderError::MissingField(path.into()))?;
    let mut blk = fld
        .as_block_mut()
        .ok_or_else(|| RenderError::MissingField(format!("{path} (not a block)")))?;
    f(&mut blk)
}

fn try_set(el: &mut crate::TagStructMut<'_>, field: &str, v: TagFieldData) -> bool {
    match el.field_mut(field) {
        Some(mut f) => f.set(v).is_ok(),
        None => false,
    }
}

fn string_id(v: &str) -> TagFieldData {
    TagFieldData::StringId(crate::fields::StringIdData { string: v.to_owned() })
}

fn point3(p: RealPoint3d) -> TagFieldData {
    TagFieldData::RealPoint3d(p)
}

fn write_nodes(tag: &mut TagFile, jms: &JmsFile) -> R<()> {
    let n = jms.nodes.len();
    let mut first_child = vec![-1i16; n.max(1)];
    let mut sibling = vec![-1i16; n.max(1)];
    for i in (0..n).rev() {
        let p = jms.nodes[i].parent;
        if p >= 0 && (p as usize) < n {
            sibling[i] = first_child[p as usize];
            first_child[p as usize] = i as i16;
        }
    }
    let mut root = tag.root_mut();
    with_block(&mut root, "nodes", |block| {
        for (i, node) in jms.nodes.iter().enumerate() {
            let idx = block.add_element();
            let mut el = block.element_mut(idx).expect("just added");
            try_set(&mut el, "name", string_id(&node.name.to_ascii_lowercase()));
            try_set(&mut el, "parent node", TagFieldData::ShortBlockIndex(node.parent));
            try_set(&mut el, "first child node", TagFieldData::ShortBlockIndex(first_child[i]));
            try_set(&mut el, "next sibling node", TagFieldData::ShortBlockIndex(sibling[i]));
            // JMS node transforms are world-space bind pose; the tag
            // wants them local to the parent, so chain the inverse.
            let (t, r) = local_transform(jms, i);
            try_set(&mut el, "default translation", point3(t));
            try_set(&mut el, "default rotation", TagFieldData::RealQuaternion(r));
            try_set(&mut el, "inverse scale", TagFieldData::Real(1.0));
            try_set(&mut el, "distance from parent", TagFieldData::Real(0.0));
        }
        Ok(())
    })
}

/// A node's transform relative to its parent, from JMS's world-space one.
fn local_transform(jms: &JmsFile, i: usize) -> (RealPoint3d, crate::math::RealQuaternion) {
    let node = &jms.nodes[i];
    let world_t = RealPoint3d {
        x: node.translation.x * JMS_TO_WORLD,
        y: node.translation.y * JMS_TO_WORLD,
        z: node.translation.z * JMS_TO_WORLD,
    };
    let p = node.parent;
    if p < 0 || (p as usize) >= jms.nodes.len() {
        return (world_t, node.rotation);
    }
    let parent = &jms.nodes[p as usize];
    let pt = RealPoint3d {
        x: parent.translation.x * JMS_TO_WORLD,
        y: parent.translation.y * JMS_TO_WORLD,
        z: parent.translation.z * JMS_TO_WORLD,
    };
    // inverse(parent) * child, for a rotation-plus-translation.
    let pq = parent.rotation;
    let inv = crate::math::RealQuaternion { i: -pq.i, j: -pq.j, k: -pq.k, w: pq.w };
    let d = RealPoint3d { x: world_t.x - pt.x, y: world_t.y - pt.y, z: world_t.z - pt.z };
    (rotate(inv, d), mul(inv, node.rotation))
}

fn mul(a: crate::math::RealQuaternion, b: crate::math::RealQuaternion) -> crate::math::RealQuaternion {
    crate::math::RealQuaternion {
        i: a.w * b.i + a.i * b.w + a.j * b.k - a.k * b.j,
        j: a.w * b.j - a.i * b.k + a.j * b.w + a.k * b.i,
        k: a.w * b.k + a.i * b.j - a.j * b.i + a.k * b.w,
        w: a.w * b.w - a.i * b.i - a.j * b.j - a.k * b.k,
    }
}

fn rotate(q: crate::math::RealQuaternion, p: RealPoint3d) -> RealPoint3d {
    let (x, y, z, w) = (q.i, q.j, q.k, q.w);
    let m = [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ];
    RealPoint3d {
        x: m[0][0] * p.x + m[0][1] * p.y + m[0][2] * p.z,
        y: m[1][0] * p.x + m[1][1] * p.y + m[1][2] * p.z,
        z: m[2][0] * p.x + m[2][1] * p.y + m[2][2] * p.z,
    }
}

fn write_markers(tag: &mut TagFile, jms: &JmsFile) -> R<usize> {
    if jms.markers.is_empty() {
        return Ok(0);
    }
    // One group per distinct marker name; Halo's markers are named and a
    // group collects the instances of one name.
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, m) in jms.markers.iter().enumerate() {
        let name = m.name.to_ascii_lowercase();
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => v.push(i),
            None => groups.push((name, vec![i])),
        }
    }
    let count = jms.markers.len();
    let mut root = tag.root_mut();
    with_block(&mut root, "marker groups", |block| {
        for (name, members) in &groups {
            let gi = block.add_element();
            let mut g = block.element_mut(gi).expect("just added");
            try_set(&mut g, "name", string_id(name));
            let mut fld = g
                .field_mut("markers")
                .ok_or_else(|| RenderError::MissingField("markers".into()))?;
            let mut mb = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("markers".into()))?;
            for &mi in members {
                let m = &jms.markers[mi];
                let i = mb.add_element();
                let mut el = mb.element_mut(i).expect("just added");
                try_set(&mut el, "region index", TagFieldData::CharInteger(-1));
                try_set(&mut el, "permutation index", TagFieldData::CharInteger(-1));
                try_set(
                    &mut el,
                    "node index",
                    TagFieldData::CharInteger(i8::try_from(m.node_index).unwrap_or(0)),
                );
                try_set(
                    &mut el,
                    "translation",
                    point3(RealPoint3d {
                        x: m.translation.x * JMS_TO_WORLD,
                        y: m.translation.y * JMS_TO_WORLD,
                        z: m.translation.z * JMS_TO_WORLD,
                    }),
                );
                try_set(&mut el, "rotation", TagFieldData::RealQuaternion(m.rotation));
                // The JMS calls it a radius; the tag calls it scale, and
                // it takes the same 0.01 factor.
                try_set(&mut el, "scale", TagFieldData::Real(m.radius * JMS_TO_WORLD));
            }
        }
        Ok(count)
    })
}

fn write_materials(tag: &mut TagFile, count: usize) -> R<usize> {
    let n = count;
    let mut root = tag.root_mut();
    with_block(&mut root, "materials", |block| {
        for i in 0..count {
            let idx = block.add_element();
            let mut el = block.element_mut(idx).expect("just added");
            // The shader reference is left null: a JMS names a material,
            // not a tag path, and Tool resolves it against the shader
            // directory. Nothing here can invent that mapping.
            try_set(&mut el, "imported material index", TagFieldData::LongInteger(i as i32));
            try_set(&mut el, "breakable surface index", TagFieldData::CharInteger(-1));
        }
        Ok(n)
    })
}

fn write_regions(tag: &mut TagFile, meshes: &[Mesh], report: &mut RenderReport) -> R<()> {
    // Region -> its permutations, in first-use order.
    let mut regions: Vec<(String, Vec<(String, usize)>)> = Vec::new();
    for (mi, m) in meshes.iter().enumerate() {
        match regions.iter_mut().find(|(n, _)| *n == m.region) {
            Some((_, perms)) => perms.push((m.permutation.clone(), mi)),
            None => regions.push((m.region.clone(), vec![(m.permutation.clone(), mi)])),
        }
    }
    if regions.len() > 16 {
        return Err(RenderError::TooManyRegions(regions.len()));
    }
    report.regions = regions.len();
    report.permutations = regions.iter().map(|(_, p)| p.len()).sum();

    let mut root = tag.root_mut();
    with_block(&mut root, "regions", |block| {
        for (name, perms) in &regions {
            let ri = block.add_element();
            let mut r = block.element_mut(ri).expect("just added");
            try_set(&mut r, "name", string_id(name));
            let mut fld = r
                .field_mut("permutations")
                .ok_or_else(|| RenderError::MissingField("permutations".into()))?;
            let mut pb = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("permutations".into()))?;
            for (pname, mesh_index) in perms {
                let pi = pb.add_element();
                let mut p = pb.element_mut(pi).expect("just added");
                try_set(&mut p, "name", string_id(pname));
                try_set(&mut p, "mesh index", TagFieldData::ShortInteger(*mesh_index as i16));
                // Hardwired to 1 by Tool, and 462 of 462 shipped
                // permutations agree. The draw path carries one mesh per
                // region and never reads a count above 1.
                try_set(&mut p, "mesh count", TagFieldData::ShortInteger(1));
            }
        }
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn write_compression(
    tag: &mut TagFile,
    lo: [f32; 3],
    hi: [f32; 3],
    ulo: [f32; 2],
    uhi: [f32; 2],
) -> R<()> {
    let mut root = tag.root_mut();
    with_block(&mut root, "render geometry/compression info", |block| {
        let i = block.add_element();
        let mut el = block.element_mut(i).expect("just added");
        // 0x3 = compressed position + compressed texcoord, on 160 of 160
        // sampled tags.
        try_set(&mut el, "compression flags", TagFieldData::WordFlags { value: 3, names: Vec::new() });
        // These are three (min,max) PAIRS packed as two point3d, not two
        // corners of a box. Reading them as corners fails on 97.5% of
        // shipped tags.
        try_set(&mut el, "position bounds 0", point3(RealPoint3d { x: lo[0], y: hi[0], z: lo[1] }));
        try_set(&mut el, "position bounds 1", point3(RealPoint3d { x: hi[1], y: lo[2], z: hi[2] }));
        try_set(
            &mut el,
            "texcoord bounds 0",
            TagFieldData::RealPoint2d(RealPoint2d { x: ulo[0], y: uhi[0] }),
        );
        try_set(
            &mut el,
            "texcoord bounds 1",
            TagFieldData::RealPoint2d(RealPoint2d { x: ulo[1], y: uhi[1] }),
        );
        Ok(())
    })
}

#[allow(clippy::too_many_arguments)]
fn write_mesh(
    tag: &mut TagFile,
    mesh: &Mesh,
    welded: &crate::weld::Welded,
    basis: &[tangent::TangentBasis],
    jms: &JmsFile,
    lo: [f32; 3],
    hi: [f32; 3],
    ulo: [f32; 2],
    uhi: [f32; 2],
    _mesh_index: usize,
    // 0 = `No PRT`, else the order plus one.
    prt: i8,
) -> R<usize> {
    // Parts, in the order their materials were first used; each part's
    // triangles are stripified into one run and the runs concatenated,
    // so a part is a contiguous index window.
    let mut indices: Vec<u32> = Vec::new();
    let mut parts: Vec<(i16, usize, usize)> = Vec::new(); // material, start, count
    for (material, tris) in &mesh.parts {
        let start = indices.len();
        let run = stripify(tris);
        // A part must begin on an even triangle boundary or its winding
        // inverts, same rule as stitching two strips.
        if start % 2 != 0 && !indices.is_empty() {
            let last = *indices.last().expect("non-empty");
            indices.push(last);
        }
        let start = indices.len();
        if !indices.is_empty() && !run.is_empty() {
            let last = *indices.last().expect("non-empty");
            indices.push(last);
            indices.push(run[0]);
            if indices.len() % 2 != 0 {
                indices.push(run[0]);
            }
        }
        let real_start = indices.len();
        indices.extend_from_slice(&run);
        let _ = start;
        // One part per chunk of the run, so `index count` stays inside a
        // signed word. The chunks overlap by two indices and begin at an
        // even offset: a strip's triangle `i` is `(i, i+1, i+2)` with its
        // winding flipped on odd `i`, so an even start keeps the parity
        // and the overlap keeps the two triangles that span the join.
        let len = indices.len() - real_start;
        let mut at = 0usize;
        while at < len {
            let take = (len - at).min(MAX_INDICES_PER_PART);
            parts.push((*material, real_start + at, take));
            if at + take >= len {
                break;
            }
            at += take - 2;
            if at % 2 != 0 {
                at -= 1;
            }
        }
    }

    if indices.len() > MAX_INDICES_PER_MESH {
        return Err(RenderError::TooManyIndices {
            region: mesh.region.clone(),
            permutation: mesh.permutation.clone(),
            indices: indices.len(),
        });
    }

    // Is every vertex bound to exactly one node, and the same one?
    let mut rigid_node: Option<i16> = None;
    let mut is_rigid = true;
    for &g in &mesh.vertices {
        let inf = &welded.vertices[g as usize].influences;
        if inf.len() != 1 {
            is_rigid = false;
            break;
        }
        match rigid_node {
            None => rigid_node = Some(inf[0].0),
            Some(n) if n == inf[0].0 => {}
            Some(_) => {
                is_rigid = false;
                break;
            }
        }
    }
    let vertex_type: i16 = if is_rigid { 1 } else { 2 };

    let n_indices = indices.len();
    let mut root = tag.root_mut();

    with_block(&mut root, "render geometry/meshes", |block| {
        let mi = block.add_element();
        let mut el = block.element_mut(mi).expect("just added");
        try_set(&mut el, "index buffer index", TagFieldData::ShortInteger(-1));
        try_set(&mut el, "index buffer tessellation", TagFieldData::ShortInteger(-1));
        try_set(
            &mut el,
            "rigid node index",
            TagFieldData::CharInteger(if is_rigid {
                i8::try_from(rigid_node.unwrap_or(0)).unwrap_or(0)
            } else {
                -1
            }),
        );
        try_set(&mut el, "vertex type", TagFieldData::CharEnum { value: vertex_type as i8, name: None });
        // PRT is not computed here. 76 of 313 shipped meshes also carry
        // `No PRT`, so the shape is one the engine already handles.
        try_set(&mut el, "PRT vertex type", TagFieldData::CharEnum { value: 0, name: None });
        // 313 of 313 shipped meshes are triangle strips.
        try_set(&mut el, "index buffer type", TagFieldData::CharEnum { value: 5, name: None });
        // 1 = PRT Ambient, 0 = No PRT. Ambient is what the solver
        // produces; the rest of the enum is linear and quadratic.
        try_set(
            &mut el,
            "PRT vertex type",
            TagFieldData::CharEnum { value: prt, name: None },
        );

        {
            let mut fld = el
                .field_mut("parts")
                .ok_or_else(|| RenderError::MissingField("parts".into()))?;
            let mut pb = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("parts".into()))?;
            for (pi, (material, start, count)) in parts.iter().enumerate() {
                let i = pb.add_element();
                let mut p = pb.element_mut(i).expect("just added");
                try_set(&mut p, "render method index", TagFieldData::ShortBlockIndex(*material));
                try_set(&mut p, "transparent sorting index", TagFieldData::ShortBlockIndex(-1));
                try_set(&mut p, "index start", TagFieldData::ShortInteger(*start as i16));
                try_set(&mut p, "index count", TagFieldData::ShortInteger(*count as i16));
                try_set(&mut p, "subpart start", TagFieldData::ShortInteger(pi as i16));
                try_set(&mut p, "subpart count", TagFieldData::ShortInteger(1));
                try_set(&mut p, "part type", TagFieldData::CharInteger(2));
                try_set(
                    &mut p,
                    "budget vertex count",
                    TagFieldData::ShortInteger(mesh.vertices.len().min(65535) as i16),
                );
            }
        }
        {
            let mut fld = el
                .field_mut("subparts")
                .ok_or_else(|| RenderError::MissingField("subparts".into()))?;
            let mut sb = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("subparts".into()))?;
            for (pi, (_m, start, count)) in parts.iter().enumerate() {
                let i = sb.add_element();
                let mut s = sb.element_mut(i).expect("just added");
                try_set(&mut s, "index start", TagFieldData::ShortInteger(*start as i16));
                try_set(&mut s, "index count", TagFieldData::ShortInteger(*count as i16));
                try_set(&mut s, "part index", TagFieldData::ShortBlockIndex(pi as i16));
                try_set(
                    &mut s,
                    "budget vertex count",
                    TagFieldData::ShortInteger(mesh.vertices.len().min(65535) as i16),
                );
            }
        }
        Ok(())
    })?;

    // The geometry itself lives inline, in `per mesh temporary`.
    let span = |a: usize| (hi[a] - lo[a]).max(f32::MIN_POSITIVE);
    let uspan = |a: usize| (uhi[a] - ulo[a]).max(f32::MIN_POSITIVE);
    let mut root = tag.root_mut();
    with_block(&mut root, "render geometry/per mesh temporary", |block| {
        let ti = block.add_element();
        let mut el = block.element_mut(ti).expect("just added");
        // Bit 0 says the index run is strips, which it is.
        try_set(&mut el, "flags", TagFieldData::LongFlags { value: 1, names: Vec::new() });
        {
            let mut fld = el
                .field_mut("raw vertices")
                .ok_or_else(|| RenderError::MissingField("raw vertices".into()))?;
            let mut vb = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("raw vertices".into()))?;
            for &g in &mesh.vertices {
                let v = &welded.vertices[g as usize];
                let b = basis[g as usize];
                let i = vb.add_element();
                let mut e = vb.element_mut(i).expect("just added");
                try_set(
                    &mut e,
                    "position",
                    point3(RealPoint3d {
                        x: (v.position.x - lo[0]) / span(0),
                        y: (v.position.y - lo[1]) / span(1),
                        z: (v.position.z - lo[2]) / span(2),
                    }),
                );
                let t0 = v.texcoords.first().copied().unwrap_or(RealPoint2d { x: 0.0, y: 0.0 });
                try_set(
                    &mut e,
                    "texcoord",
                    TagFieldData::RealPoint2d(RealPoint2d {
                        x: (t0.x - ulo[0]) / uspan(0),
                        y: (t0.y - ulo[1]) / uspan(1),
                    }),
                );
                try_set(&mut e, "normal", point3(RealPoint3d {
                    x: v.normal.i, y: v.normal.j, z: v.normal.k,
                }));
                // Halo's `binormal` is the classic tangent and its
                // `tangent` is the bitangent. The names are swapped.
                try_set(&mut e, "binormal", point3(RealPoint3d {
                    x: b.tangent.i, y: b.tangent.j, z: b.tangent.k,
                }));
                try_set(&mut e, "tangent", point3(RealPoint3d {
                    x: b.bitangent.i, y: b.bitangent.j, z: b.bitangent.k,
                }));
                write_skin(&mut e, &v.influences);
                if let Some(c) = v.color {
                    try_set(&mut e, "vertex color", point3(c));
                }
            }
        }
        {
            let mut fld = el
                .field_mut("raw indices")
                .ok_or_else(|| RenderError::MissingField("raw indices".into()))?;
            let mut ib = fld
                .as_block_mut()
                .ok_or_else(|| RenderError::MissingField("raw indices".into()))?;
            for idx in &indices {
                let i = ib.add_element();
                let mut e = ib.element_mut(i).expect("just added");
                try_set(&mut e, "word", TagFieldData::ShortInteger(*idx as i16));
            }
        }
        Ok(())
    })?;

    let _ = jms;
    Ok(n_indices)
}

/// Node indices are four `char`s with `-1` for none; weights are four
/// floats. Both are arrays, so they are written element by element.
fn write_skin(el: &mut crate::TagStructMut<'_>, influences: &[(i16, f32)]) {
    if let Some(mut fld) = el.field_mut("node indices") {
        if let Some(mut arr) = fld.as_array_mut() {
            for k in 0..4 {
                let Some(mut slot) = arr.element_mut(k) else { continue };
                let v = influences.get(k).map(|(n, _)| *n as i8).unwrap_or(-1);
                try_set(&mut slot, "node index", TagFieldData::CharInteger(v));
            }
        }
    }
    if let Some(mut fld) = el.field_mut("node weights") {
        if let Some(mut arr) = fld.as_array_mut() {
            for k in 0..4 {
                let Some(mut slot) = arr.element_mut(k) else { continue };
                let w = influences.get(k).map(|(_, w)| *w).unwrap_or(0.0);
                try_set(&mut slot, "node weight", TagFieldData::Real(w));
            }
        }
    }
}
