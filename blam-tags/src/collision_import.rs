//! JMS → `collision_model`, without `tool.exe`.
//!
//! This is the only one of the three importers where the output cannot
//! be checked by looking at it. A wrong render_model is visibly wrong; a
//! wrong physics_model behaves oddly. **A wrong collision BSP loads
//! cleanly and silently drops the player through a floor.** So the
//! construction here is chosen to be *verifiable*, not to be
//! byte-identical to Tool.
//!
//! # What is reproduced exactly
//!
//! * **Plane selection cost.** `splits + |below − above|`, unit weights,
//!   no tunables, first candidate wins ties. Derived from the binary's
//!   `3T − A − B + |B − A|`, which is `2T + splits + |below − above|`
//!   with `2T` constant per node.
//! * **The classification epsilon**, `2⁻¹² = 0.000244140625`.
//! * **The chop epsilon**, `|n₁ × n₂| · 2⁻¹²` floored at `2.4414062e-6` —
//!   it scales with the angle between the surface and the splitting
//!   plane, so a near-tangent split does not shatter into slivers.
//! * **Every packed field**: the 64-bit `bsp3d_node` with its two
//!   unaligned sign-extended 24-bit children, and the shared s15+bit15
//!   encoding used by surface planes, bsp2d children and bsp2d
//!   references.
//!
//! # What is deliberately not reproduced
//!
//! Tool carries **two** parallel surface lists down the recursion — "bsp
//! geometry" chopped with the epsilon above, which plane selection sees,
//! and "connection geometry" chopped exactly, which the leaves see. This
//! carries one list and assigns surfaces to leaves afterwards, by
//! descending the finished tree from each surface's centroid pushed
//! just behind its own plane.
//!
//! That is a different construction, and it is the right trade because
//! it is **checkable**: the property a collision BSP has to have is that
//! a ray striking a surface reaches a leaf that references it, and this
//! establishes exactly that, per surface, by the same traversal the
//! runtime uses. [`verify`] runs it.
//!
//! # Limits that bind
//!
//! Per BSP, from the small format's field widths — the *blockdef* maxima
//! are larger and following them produces a tag that packs and then
//! crashes:
//!
//! | block | encoding limit |
//! |---|---|
//! | surfaces | 32,767 (s15 + flipped bit) |
//! | planes | 32,767 (read signed) |
//! | edges | 65,535 |
//! | vertices | 65,535 |
//! | bsp2d nodes | 32,767 |
//! | bsp3d nodes / leaves | 8,388,607 |
//!
//! A permutation may hold **64** BSPs, they all collide, and several may
//! share a `node_index` — so the real ceiling is 64 × 32,767 surfaces.
//! Depth must stay at or under **128**: Tool rejects deeper trees, and
//! the game's traversal stack is a fixed 128 entries that it overruns
//! without stopping.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::jms::JmsFile;
use crate::jms_split::MaterialLabel;
use crate::math::{RealPlane3d, RealPoint3d};
use crate::weld::{weld, WeldTolerances, WeldVertex};
use crate::{TagFieldData, TagFile};

/// JMS units are hundredths of a world unit.
pub const JMS_TO_WORLD: f32 = 0.01;
/// `2⁻¹²`, the classification epsilon.
pub const CLASSIFY_EPS: f64 = 0.000244140625;
/// The floor under the angle-scaled chop epsilon.
pub const CHOP_EPS_MIN: f64 = 2.4414062e-6;
/// Deeper than this and the game overruns its traversal stack.
pub const MAX_DEPTH: usize = 128;
/// s15 with a flag bit leaves this many indices.
pub const MAX_SURFACES: usize = 32_767;
/// A permutation holds at most this many BSPs, and they all collide.
pub const MAX_BSPS_PER_PERMUTATION: usize = 64;

/// How many vertices a surface may have.
///
/// Tool enforces this at `0x14014D2E3` (`cmp ecx,8 / setle`, signed)
/// inside its coplanar merge, and a walk of 11,384 shipped surfaces found
/// a maximum of exactly eight and never nine. Merging stops here and a
/// surface past it is refused.
///
/// A sweep of the whole tags folder does turn up 244 surfaces of 335,913
/// with nine or more. Those are not a reason to raise this: the walk
/// gives up after 32 steps, so its tail cannot tell a ten-sided surface
/// from a ring that never closed, and the folder holds tags ported from
/// other titles — `frigate_reach`, `heretic_banshee`, `h2a_juggernaut` —
/// which H3's importer never produced.
pub const MAX_RING: usize = 8;

/// The bound a surface is rejected at, which is the same one.
pub const MAX_RING_HARD: usize = MAX_RING;

/// How far from straight a merged corner may turn the wrong way before
/// the union counts as concave, as a sine. About 0.006 degrees.
const CONVEX_SIN_EPS: f64 = 1e-4;

/// How close to a 2D line counts as on it.
const EPS_2D: f64 = 1.0 / 8192.0;

/// How to interpret the JMS.
#[derive(Debug, Clone)]
pub struct CollisionOptions {
    pub scale: f32,
    /// Weld tolerances. The collision path passes a node-weight
    /// tolerance of 0, meaning skinning is not considered.
    pub weld: WeldTolerances,
}

impl Default for CollisionOptions {
    fn default() -> Self {
        Self {
            scale: JMS_TO_WORLD,
            weld: WeldTolerances {
                position: 0.00048828125,
                // No coarse stage: the same as tool's own behaviour when
                // the importer's 0x20 flag is set.
                //
                // `sub_1400D9E10` does pass 1/256 as the collision coarse
                // tolerance, and applying it here is destructive —
                // `stanchion_new2_collision.JMS` collapses from 17,905
                // surfaces to 3,522, losing 80% of the collision. Shipped
                // collision models carry ~18,000 surfaces for geometry of
                // that size, so tool plainly does not lose it, and
                // whatever that value means on tool's path it does not
                // mean "merge collision positions 0.4 inches apart" on
                // this one. Left at the precise tolerance until that is
                // understood; the welded output is the one the corpus
                // validates at 0 dropped and 0 unreachable.
                coarse_position: 0.00048828125,
                texcoord: [0.0; 4],
                normal_degrees: 180.0, // collision does not split on normals
                node_weight: 0.0,
            },
        }
    }
}

/// Why a `collision_model` could not be written.
#[derive(Debug, Clone, PartialEq)]
pub enum CollisionError {
    Schema(String),
    MissingField(String),
    /// A BSP exceeds what the small format can encode.
    TooLarge { what: &'static str, count: usize, max: usize },
    /// The tree got deeper than the game's traversal stack.
    TooDeep(usize),
    /// A surface has more than eight vertices. Nothing downstream checks
    /// this and every consumer uses a fixed eight-entry stack buffer.
    SurfaceTooComplex { surface: usize, vertices: usize },
    /// A surface the finished tree cannot reach — it would be invisible
    /// collision.
    Unreachable(usize),
    TooManyRegions(usize),
    Empty,
}

impl std::fmt::Display for CollisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(m) => write!(f, "{m}"),
            Self::MissingField(m) => write!(f, "the schema has no field {m:?}"),
            Self::TooLarge { what, count, max } => {
                write!(f, "{count} {what}, over the {max} the small BSP format can encode")
            }
            Self::TooDeep(d) => write!(
                f,
                "the BSP is {d} deep; over {MAX_DEPTH} overruns the game's traversal stack"
            ),
            Self::SurfaceTooComplex { surface, vertices } => write!(
                f,
                "surface {surface} has {vertices} vertices; every consumer uses a fixed 8-entry \
                 buffer and would overrun"
            ),
            Self::Unreachable(s) => write!(
                f,
                "surface {s} is not reachable through the finished tree — it would be collision \
                 that is simply not there"
            ),
            Self::TooManyRegions(n) => write!(f, "{n} regions, but the engine allows 16"),
            Self::Empty => write!(f, "no collision triangles to import"),
        }
    }
}

impl std::error::Error for CollisionError {}

type R<T> = Result<T, CollisionError>;

/// What [`collision_model_from_jms`] produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CollisionReport {
    pub regions: usize,
    pub permutations: usize,
    pub bsps: usize,
    pub materials: usize,
    pub nodes: usize,
    pub surfaces: usize,
    pub edges: usize,
    pub vertices: usize,
    pub planes: usize,
    pub bsp3d_nodes: usize,
    pub leaves: usize,
    pub bsp2d_nodes: usize,
    pub bsp2d_references: usize,
    pub max_depth: usize,
    /// Surfaces discarded because they overlap a coplanar neighbour and a
    /// bsp2d leaf can only hold one. Collision that will not be there —
    /// small in practice (0.6% on the largest shipped collision JMS) but
    /// reported rather than silently lost.
    pub dropped_surfaces: usize,
    pub skipped: Vec<String>,
}

// ------------------------------------------------------------- geometry

type V3 = [f64; 3];

fn sub3(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross3(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot3(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn len3(a: V3) -> f64 {
    dot3(a, a).sqrt()
}

/// One collision surface: a polygon, its plane, and its edge ring.
#[derive(Debug, Clone)]
struct Surface {
    /// Vertex indices, in winding order.
    ring: Vec<u32>,
    plane: u32,
    flipped: bool,
    material: i16,
    first_edge: u32,
}

/// The winged edge Halo uses. `forward` is the next edge in the *left*
/// surface's ring; `reverse` the next in the right's — which is exactly
/// what makes the ring walk `if left == me { forward } else { reverse }`.
#[derive(Debug, Clone, Copy)]
struct Edge {
    start: u32,
    end: u32,
    forward: u32,
    reverse: u32,
    left: i32,
    right: i32,
}

/// A whole BSP for one `(region, permutation, node)`.
#[derive(Debug, Default)]
struct Bsp {
    node_index: i16,
    vertices: Vec<RealPoint3d>,
    edges: Vec<Edge>,
    surfaces: Vec<Surface>,
    planes: Vec<RealPlane3d>,
    nodes3d: Vec<Node3d>,
    leaves: Vec<Leaf>,
    /// The surfaces with a fragment in each leaf's cell, recorded as the
    /// leaf is made. Parallel to `leaves`.
    leaf_surfaces: Vec<Vec<u32>>,
    bsp2d_nodes: Vec<Node2d>,
    bsp2d_refs: Vec<Ref2d>,
    /// Surfaces the 2D builder had to discard because no line separates
    /// them from a coplanar neighbour. A bsp2d leaf holds exactly one
    /// surface, so a group of mutually overlapping coplanar faces cannot
    /// all be represented. Tool hits the same wall and warns
    /// ("overlapping surfaces … may be caused by t-junctions or slivers").
    dropped: Vec<u32>,
    /// How many (leaf, surface) pairs the assignment decided on. The
    /// tag has to give all of them back; anything fewer is collision
    /// that was worked out and then not written.
    assigned_pairs: usize,
}

#[derive(Debug, Clone, Copy)]
struct Node3d {
    plane: u32,
    /// Encoded child: `>= 0` a node index, `< 0` a leaf as `!leaf`, or
    /// `i32::MIN` for none.
    back: i32,
    front: i32,
}

#[derive(Debug, Clone, Default)]
struct Leaf {
    flags: u8,
    first_ref: i32,
    ref_count: u16,
}

#[derive(Debug, Clone, Copy)]
struct Node2d {
    plane: [f32; 3], // i, j, d
    left: i32,       // >= 0 node, < 0 surface as !surface
    right: i32,
}

#[derive(Debug, Clone, Copy)]
struct Ref2d {
    plane: u32,
    flipped: bool,
    node: i32, // >= 0 bsp2d node, < 0 surface as !surface
}

/// Build a `collision_model` tag from a JMS scene.
pub fn collision_model_from_jms(
    jms: &JmsFile,
    schema: &Path,
    opts: &CollisionOptions,
) -> R<(TagFile, CollisionReport)> {
    if jms.triangles.is_empty() {
        return Err(CollisionError::Empty);
    }
    let mut report = CollisionReport::default();

    // ---- weld, once for the whole model -----------------------------
    let s = opts.scale;
    let source: Vec<WeldVertex> = jms
        .vertices
        .iter()
        .map(|v| WeldVertex {
            position: RealPoint3d { x: v.position.x * s, y: v.position.y * s, z: v.position.z * s },
            normal: v.normal,
            texcoords: Vec::new(),
            influences: v.node_sets.clone(),
            color: None,
        })
        .collect();
    let welded = weld(&source, &opts.weld);

    // ---- group triangles into BSPs ----------------------------------
    // A BSP is keyed by (region, permutation, node): each carries its own
    // `node_index` and they all collide, so several per permutation is
    // both legal and how multi-part objects work.
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
    let mut groups: BTreeMap<(String, String, i16), Vec<usize>> = BTreeMap::new();
    let mut order: Vec<(String, String, i16)> = Vec::new();
    let mut dropped = 0usize;
    for (i, tri) in jms.triangles.iter().enumerate() {
        let Some(label) = labels.get(usize::try_from(tri.material).unwrap_or(usize::MAX)) else {
            dropped += 1;
            continue;
        };
        // The node a triangle belongs to is its vertices' shared binding.
        let node = jms
            .vertices
            .get(tri.v[0] as usize)
            .and_then(|v| v.node_sets.first().map(|(n, _)| *n))
            .unwrap_or(0);
        let key = (label.region.clone(), label.permutation.clone(), node);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(i);
    }
    if dropped > 0 {
        report.skipped.push(format!("{dropped} triangle(s) with an invalid material index"));
    }

    // ---- build one BSP per group ------------------------------------
    let mut built: Vec<((String, String), Bsp)> = Vec::new();
    for key in &order {
        let tris = &groups[key];
        let mut bsp = build_geometry(jms, &welded, tris, key.2)?;
        build_tree(&mut bsp, MAX_DEPTH)?;
        report.surfaces += bsp.surfaces.len();
        report.edges += bsp.edges.len();
        report.vertices += bsp.vertices.len();
        report.planes += bsp.planes.len();
        report.bsp3d_nodes += bsp.nodes3d.len();
        report.leaves += bsp.leaves.len();
        report.bsp2d_nodes += bsp.bsp2d_nodes.len();
        report.bsp2d_references += bsp.bsp2d_refs.len();
        report.max_depth = report.max_depth.max(depth_of(&bsp, 0));
        report.dropped_surfaces += bsp.dropped.len();
        built.push(((key.0.clone(), key.1.clone()), bsp));
    }
    report.bsps = built.len();

    // ---- write ------------------------------------------------------
    let mut tag = TagFile::new(schema).map_err(|e| {
        CollisionError::Schema(format!(
            "cannot create a collision_model from {}: {e}",
            schema.display()
        ))
    })?;
    write_nodes(&mut tag, jms)?;
    report.nodes = jms.nodes.len();
    report.materials = write_materials(&mut tag, jms)?;
    write_regions(&mut tag, &built, &mut report)?;
    Ok((tag, report))
}

/// Surfaces, edges and vertices for one group of triangles.
fn build_geometry(
    jms: &JmsFile,
    welded: &crate::weld::Welded,
    tris: &[usize],
    node_index: i16,
) -> R<Bsp> {
    // Resolve the JMS's corners through the welder into local vertex
    // numbering, in first-use order. Everything past this point is
    // geometry rather than JMS, which is what lets a structure BSP reuse
    // the same builder.
    let mut local: BTreeMap<u32, u32> = BTreeMap::new();
    let mut positions: Vec<RealPoint3d> = Vec::new();
    let mut prepared: Vec<([u32; 3], i16)> = Vec::with_capacity(tris.len());
    for &t in tris {
        let tri = &jms.triangles[t];
        let mut r = [0u32; 3];
        for k in 0..3 {
            let g = welded.remap[tri.v[k] as usize];
            let next = positions.len() as u32;
            let idx = *local.entry(g).or_insert_with(|| {
                positions.push(welded.vertices[g as usize].position);
                next
            });
            r[k] = idx;
        }
        prepared.push((r, tri.material as i16));
    }
    build_geometry_from(&positions, &prepared, node_index)
}

/// The geometry half of the builder: winged edges, deduplicated planes
/// and coplanar merging over a plain triangle list.
fn build_geometry_from(
    positions: &[RealPoint3d],
    tris: &[([u32; 3], i16)],
    node_index: i16,
) -> R<Bsp> {
    build_geometry_from_with(positions, tris, node_index, true)
}

fn build_geometry_from_with(
    positions: &[RealPoint3d],
    tris: &[([u32; 3], i16)],
    node_index: i16,
    merge: bool,
) -> R<Bsp> {
    let mut bsp = Bsp { node_index, vertices: positions.to_vec(), ..Default::default() };

    let mut ring_of: Vec<[u32; 3]> = Vec::new();
    let mut material_of: Vec<i16> = Vec::new();
    for (r, material) in tris {
        // Degenerate triangles carry no surface.
        if r[0] == r[1] || r[1] == r[2] || r[0] == r[2] {
            continue;
        }
        ring_of.push(*r);
        material_of.push(*material);
    }
    if ring_of.is_empty() {
        return Err(CollisionError::Empty);
    }

    // Planes, deduplicated. `flipped` records that the surface faces the
    // plane's back, which is how two opposite-facing coplanar surfaces
    // share one plane record.
    let mut plane_keys: Vec<(V3, f64)> = Vec::new();
    for (si, r) in ring_of.iter().enumerate() {
        let p: Vec<V3> = r.iter().map(|&i| pt(&bsp.vertices[i as usize])).collect();
        let n = cross3(sub3(p[1], p[0]), sub3(p[2], p[0]));
        let l = len3(n);
        if l <= 0.0 {
            continue;
        }
        let n = [n[0] / l, n[1] / l, n[2] / l];
        let d = -dot3(n, p[0]);

        // Match an existing plane, in either orientation.
        //
        // Matching on the normal alone is not enough. The angular
        // tolerance here is about a degree, and a degree across a
        // polygon a couple of units wide lifts its far corner two
        // centimetres off the plane it has just been assigned to. The
        // tree routes by that plane, so the polygon ends up hittable
        // where nothing is indexed — collision missing from part of its
        // own face, which is exactly what the ray check was reporting.
        //
        // So a plane is only shared if *this* surface's own vertices sit
        // on it, inside the band the classifier treats as on-plane.
        let fits = |pn: &V3, pd: f64| -> bool {
            p.iter().all(|v| (dot3(*pn, *v) + pd).abs() <= CLASSIFY_EPS)
        };
        // How close two planes' offsets have to be to be the same plane.
        //
        // A fixed 1e-5 is the wrong shape: `d` is a distance from the
        // origin, so at the far side of a level it is a few hundred,
        // where f32 vertices alone move it by more than that. Two
        // genuinely coplanar floor triangles then never matched, every
        // surface got a plane to itself, and the tree grew a node for
        // each — armory built 15,952 planes where tool has 8,374, and
        // 110 levels of depth where tool has 38.
        //
        // `fits` is what actually keeps this honest: whatever the
        // offsets say, the surface's own vertices still have to sit on
        // the plane it joins.
        let d_tol = 1e-5f64.max(d.abs() * 8.0 * f32::EPSILON as f64);
        let mut found = None;
        for (pi, (pn, pd)) in plane_keys.iter().enumerate() {
            if dot3(*pn, n) > 0.9999 && (pd - d).abs() < d_tol && fits(pn, *pd) {
                found = Some((pi, false));
                break;
            }
            if dot3(*pn, n) < -0.9999 && (pd + d).abs() < d_tol && fits(pn, *pd) {
                found = Some((pi, true));
                break;
            }
        }
        let (plane, flipped) = match found {
            Some(v) => v,
            None => {
                plane_keys.push((n, d));
                bsp.planes.push(RealPlane3d {
                    i: n[0] as f32,
                    j: n[1] as f32,
                    k: n[2] as f32,
                    d: -d as f32,
                });
                (plane_keys.len() - 1, false)
            }
        };
        bsp.surfaces.push(Surface {
            ring: r.to_vec(),
            plane: plane as u32,
            flipped,
            material: material_of[si],
            first_edge: 0,
        });
    }

    if merge {
        merge_coplanar_surfaces(&mut bsp, MAX_RING);
    }

    if bsp.surfaces.len() > MAX_SURFACES {
        return Err(CollisionError::TooLarge {
            what: "surfaces",
            count: bsp.surfaces.len(),
            max: MAX_SURFACES,
        });
    }
    for (i, s) in bsp.surfaces.iter().enumerate() {
        if s.ring.len() > MAX_RING_HARD {
            return Err(CollisionError::SurfaceTooComplex { surface: i, vertices: s.ring.len() });
        }
    }

    build_edges(&mut bsp)?;
    Ok(bsp)
}

fn pt(p: &RealPoint3d) -> V3 {
    [p.x as f64, p.y as f64, p.z as f64]
}


/// Merge coplanar neighbours into convex polygons, the way Tool does.
///
/// One surface per triangle is not what the format holds. Of the 335,913
/// surfaces in the shipped corpus only 71.8% are triangles; 25.6% are
/// quads and the rest larger, and on `guardian.collision_model` the ring
/// sizes add back up to its JMS's triangle count exactly. Building
/// triangles produced 3410 surfaces there against Tool's 2193.
///
/// It is not only fidelity. Every surface the BSP still has to separate
/// is a fragment carried down each branch and a plane that must be
/// consumed before a leaf, so merging is what keeps the tree inside the
/// depth the runtime can walk.
///
/// Two surfaces merge only if they share a plane record, a facing and a
/// material, meet along one whole edge, and their union stays convex and
/// within `max_ring` vertices. Returns how many merges happened.
fn merge_coplanar_surfaces(bsp: &mut Bsp, max_ring: usize) -> usize {
    let mut groups: BTreeMap<(u32, bool, i16), Vec<usize>> = BTreeMap::new();
    for (i, s) in bsp.surfaces.iter().enumerate() {
        groups.entry((s.plane, s.flipped, s.material)).or_default().push(i);
    }

    let mut dead = vec![false; bsp.surfaces.len()];
    let mut merges = 0usize;

    for ((plane, flipped, _), members) in groups {
        if members.len() < 2 {
            continue;
        }
        // The outward normal of these surfaces, which is the plane's own
        // unless they sit on its back.
        let (pn, _) = plane_of(bsp, plane);
        let normal = if flipped { [-pn[0], -pn[1], -pn[2]] } else { pn };

        loop {
            // Directed edge -> the live member that traverses it. A
            // shared boundary shows up as one member holding `a -> b`
            // and its neighbour holding `b -> a`.
            let mut owner: BTreeMap<(u32, u32), usize> = BTreeMap::new();
            for &m in &members {
                if dead[m] {
                    continue;
                }
                let ring = &bsp.surfaces[m].ring;
                for k in 0..ring.len() {
                    owner.insert((ring[k], ring[(k + 1) % ring.len()]), m);
                }
            }

            // Merge as much as possible per pass, but never touch a
            // surface twice against a map that no longer describes it.
            let mut touched: BTreeSet<usize> = BTreeSet::new();
            for &m in &members {
                if dead[m] || touched.contains(&m) {
                    continue;
                }
                let ring = bsp.surfaces[m].ring.clone();
                for k in 0..ring.len() {
                    let (a, b) = (ring[k], ring[(k + 1) % ring.len()]);
                    let Some(&other) = owner.get(&(b, a)) else { continue };
                    if other == m || dead[other] || touched.contains(&other) {
                        continue;
                    }
                    let Some(joined) = splice_rings(&ring, &bsp.surfaces[other].ring, a, b) else {
                        continue;
                    };
                    if joined.len() > max_ring || !is_convex_ring(bsp, &joined, normal) {
                        continue;
                    }
                    // And the union has to be flat. Surfaces are grouped
                    // by plane *index*, and plane deduplication accepts
                    // normals within about a degree of each other, so two
                    // members of a group need not be exactly coplanar.
                    // Splicing them makes a ring that bulges off the
                    // plane the tree routes by: the polygon is then
                    // hittable where the tree has nothing indexed, which
                    // is a surface that cannot be collided with along
                    // part of its own face.
                    if !is_planar_ring(bsp, &joined, plane) {
                        continue;
                    }
                    bsp.surfaces[m].ring = joined;
                    dead[other] = true;
                    touched.insert(m);
                    touched.insert(other);
                    merges += 1;
                    break;
                }
            }
            if touched.is_empty() {
                break;
            }
        }
    }

    if merges > 0 {
        let kept: Vec<Surface> = bsp
            .surfaces
            .drain(..)
            .enumerate()
            .filter_map(|(i, s)| (!dead[i]).then_some(s))
            .collect();
        bsp.surfaces = kept;
    }
    merges
}

/// Does every vertex of a ring lie on the plane it claims?
///
/// Within the band the tree's own classifier treats as on the plane, so
/// a ring that passes here is one the descent cannot disagree about.
fn is_planar_ring(bsp: &Bsp, ring: &[u32], plane: u32) -> bool {
    let (n, d) = plane_of(bsp, plane);
    // Normalised first: `plane_of` hands back the plane as stored, and
    // `dot(n, p) + d` is a distance only when the normal has unit
    // length. Stored normals are unit today, so this changes no
    // decision — it is here so the epsilon means millimetres whatever
    // the plane looks like, rather than millimetres times a length
    // nobody checked.
    let len = len3(n);
    if len <= 0.0 {
        return false;
    }
    let n = [n[0] / len, n[1] / len, n[2] / len];
    let d = d / len;
    let pts: Vec<V3> = ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect();
    let (lo, hi) = range(&pts, n, d);
    lo >= -CLASSIFY_EPS && hi <= CLASSIFY_EPS
}

/// Join two rings across the edge they share, `a -> b` in `p` and
/// `b -> a` in `q`.
///
/// The union's boundary is `p`'s walk from `b` back round to `a`, then
/// `q`'s from `a` back round to `b` — the shared edge is the one piece
/// neither walk covers. `None` if the rings do not meet that way, or if
/// the result repeats a vertex, which means they share more than this one
/// edge and the union is not a simple polygon.
fn splice_rings(p: &[u32], q: &[u32], a: u32, b: u32) -> Option<Vec<u32>> {
    let i = p.iter().position(|&v| v == a)?;
    if p[(i + 1) % p.len()] != b {
        return None;
    }
    let j = q.iter().position(|&v| v == b)?;
    if q[(j + 1) % q.len()] != a {
        return None;
    }

    let mut out = Vec::with_capacity(p.len() + q.len() - 2);
    for k in 0..p.len() {
        out.push(p[(i + 1 + k) % p.len()]); // b .. a
    }
    for k in 2..q.len() {
        out.push(q[(j + k) % q.len()]); // past a, stopping before b
    }

    let mut seen = BTreeSet::new();
    if !out.iter().all(|v| seen.insert(*v)) {
        return None;
    }
    Some(out)
}

/// Does this ring turn the same way at every corner, seen from `normal`?
///
/// The test is on the sine of the turn, so it does not care how long the
/// edges are, and a corner that is merely straight passes — collinear
/// vertices are kept rather than dropped, because a neighbour may still
/// meet the surface there.
fn is_convex_ring(bsp: &Bsp, ring: &[u32], normal: V3) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let p: Vec<V3> = ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect();
    for k in 0..p.len() {
        let u = sub3(p[(k + 1) % p.len()], p[k]);
        let v = sub3(p[(k + 2) % p.len()], p[(k + 1) % p.len()]);
        let scale = len3(u) * len3(v);
        if scale <= 0.0 {
            continue;
        }
        if dot3(cross3(u, v), normal) < -CONVEX_SIN_EPS * scale {
            return false;
        }
    }
    true
}

/// The winged-edge ring. An edge is shared by at most two surfaces: the
/// one that traverses it `start -> end` is its **left**, the one that
/// traverses `end -> start` its **right**.
fn build_edges(bsp: &mut Bsp) -> R<()> {
    // One vertex pair can carry more than one edge record: a third
    // surface on the same pair, or a second one traversing it the same
    // way, needs a slot that is already taken.
    let mut by_pair: BTreeMap<(u32, u32), Vec<u32>> = BTreeMap::new();
    // Per surface, its ring's edges in order, so the links can be made
    // once every edge exists.
    let mut ring_edges: Vec<Vec<u32>> = vec![Vec::new(); bsp.surfaces.len()];

    for si in 0..bsp.surfaces.len() {
        let ring = bsp.surfaces[si].ring.clone();
        for k in 0..ring.len() {
            let a = ring[k];
            let b = ring[(k + 1) % ring.len()];
            let canon = if a < b { (a, b) } else { (b, a) };

            // Reuse an edge only if the slot this direction needs is free.
            let mut ei = None;
            if let Some(list) = by_pair.get(&canon) {
                for &cand in list {
                    let e = &bsp.edges[cand as usize];
                    let want_left = e.start == a && e.end == b;
                    if (want_left && e.left < 0) || (!want_left && e.right < 0) {
                        ei = Some(cand);
                        break;
                    }
                }
            }
            let ei = match ei {
                Some(e) => e,
                None => {
                    let e = bsp.edges.len() as u32;
                    bsp.edges.push(Edge {
                        start: a,
                        end: b,
                        forward: u32::MAX,
                        reverse: u32::MAX,
                        left: -1,
                        right: -1,
                    });
                    by_pair.entry(canon).or_default().push(e);
                    e
                }
            };
            let e = &mut bsp.edges[ei as usize];
            if e.start == a && e.end == b {
                e.left = si as i32;
            } else {
                e.right = si as i32;
            }
            ring_edges[si].push(ei);
        }
    }

    // Link each ring: the next edge after position k is at k+1.
    for si in 0..bsp.surfaces.len() {
        let edges = &ring_edges[si];
        for k in 0..edges.len() {
            let e = edges[k];
            let next = edges[(k + 1) % edges.len()];
            let edge = &mut bsp.edges[e as usize];
            if edge.left == si as i32 {
                edge.forward = next;
            } else {
                edge.reverse = next;
            }
        }
        bsp.surfaces[si].first_edge = edges[0];
    }

    // An edge with only one surface is a boundary; its unused link has to
    // point somewhere, and pointing at itself terminates a walk rather
    // than running off into another surface's ring.
    for (i, e) in bsp.edges.iter_mut().enumerate() {
        if e.forward == u32::MAX {
            e.forward = i as u32;
        }
        if e.reverse == u32::MAX {
            e.reverse = i as u32;
        }
    }

    if bsp.edges.len() > 65_535 {
        return Err(CollisionError::TooLarge {
            what: "edges",
            count: bsp.edges.len(),
            max: 65_535,
        });
    }
    if bsp.vertices.len() > 65_535 {
        return Err(CollisionError::TooLarge {
            what: "vertices",
            count: bsp.vertices.len(),
            max: 65_535,
        });
    }
    Ok(())
}

// ------------------------------------------------------------- the tree

/// A surface as the recursion sees it: its index, and its polygon clipped
/// by every plane above it.
#[derive(Clone)]
struct Frag {
    surface: u32,
    poly: Vec<V3>,
}

fn plane_of(bsp: &Bsp, i: u32) -> (V3, f64) {
    let p = &bsp.planes[i as usize];
    ([p.i as f64, p.j as f64, p.k as f64], -p.d as f64)
}

/// Signed distance range of a polygon to a plane.
fn range(poly: &[V3], n: V3, d: f64) -> (f64, f64) {
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for p in poly {
        let s = dot3(n, *p) + d;
        lo = lo.min(s);
        hi = hi.max(s);
    }
    (lo, hi)
}

fn build_tree(bsp: &mut Bsp, max_depth: usize) -> R<()> {
    build_tree_with(bsp, max_depth, &[], None)
}

/// Build the tree, optionally forcing some planes to divide the cells
/// they cross.
///
/// A portal is not a surface, so nothing in the collision geometry makes
/// the tree divide there — the two sides of a portal land in the *same*
/// leaf, and no rule that files leaves under clusters can then tell them
/// apart. Measured: 102 of `anchor_point`-through-`armory`'s portals had
/// one cell on both sides.
///
/// Forcing the portal planes fixes that at the root of it. A forced
/// plane is only used on a cell it actually crosses, so a level with
/// sixty portals does not become two-to-the-sixty cells; it splits the
/// handful of cells each portal passes through.
fn build_tree_with(
    bsp: &mut Bsp,
    max_depth: usize,
    forced: &[u32],
    world: Option<[[f64; 2]; 3]>,
) -> R<()> {
    let frags: Vec<Frag> = (0..bsp.surfaces.len())
        .map(|i| Frag {
            surface: i as u32,
            poly: bsp.surfaces[i].ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect(),
        })
        .collect();

    let box_ = world.unwrap_or([[-1.0e6, 1.0e6]; 3]);
    let root = recurse(bsp, frags, &mut Vec::new(), 0, max_depth, forced, box_)?;
    // The root must be a node; a single-leaf BSP is rejected by the
    // runtime, which only processes a leaf's references when a plane was
    // pushed on the way down.
    if root >= 0 {
        // `recurse` returns an encoded child; the root has to be node 0.
        if bsp.nodes3d.is_empty() {
            return Err(CollisionError::Empty);
        }
    }
    // The tree adds planes of its own where face planes will not
    // separate a cell, so these are only bounded once it is built.
    // `pack_node3d` carries the plane in 16 bits and each child in 24,
    // the top one being the leaf flag; past either the mask writes
    // something plausible instead of failing.
    if bsp.planes.len() > 65_535 {
        return Err(CollisionError::TooLarge {
            what: "planes",
            count: bsp.planes.len(),
            max: 65_535,
        });
    }
    if bsp.nodes3d.len() > 0x7F_FFFF {
        return Err(CollisionError::TooLarge {
            what: "bsp3d nodes",
            count: bsp.nodes3d.len(),
            max: 0x7F_FFFF,
        });
    }
    if bsp.leaves.len() > 0x7F_FFFF {
        return Err(CollisionError::TooLarge {
            what: "leaves",
            count: bsp.leaves.len(),
            max: 0x7F_FFFF,
        });
    }

    assign_surfaces_to_leaves(bsp)?;
    let d = depth_of(bsp, 0);
    if d > max_depth {
        return Err(CollisionError::TooDeep(d));
    }
    Ok(())
}

/// Flag a reference as a leaf or a surface rather than a node.
///
/// The format's convention: bit 31 in memory, which becomes bit 23 inside
/// a packed `bsp3d_node` child and bit 15 through the s15 transport. The
/// low bits stay a plain index — this is **not** a bitwise complement.
fn flag_ref(index: usize) -> i32 {
    ((index as u32) | 0x8000_0000) as i32
}

/// The index inside a flagged reference.
fn ref_index(encoded: i32) -> usize {
    (encoded as u32 & 0x7FFF_FFFF) as usize
}

/// Returns an encoded child: `>= 0` a node index, `< 0` a flagged leaf.
fn recurse(
    bsp: &mut Bsp,
    frags: Vec<Frag>,
    used: &mut Vec<u32>,
    depth: usize,
    max_depth: usize,
    forced: &[u32],
    box_: [[f64; 2]; 3],
) -> R<i32> {
    // A forced plane that crosses this cell goes first, whether or not
    // any geometry is here: the point of it is to divide *space*, so
    // that the two sides of a portal are never the same cell.
    if depth <= max_depth {
        if let Some(&fp) = forced
            .iter()
            .find(|p| !used.contains(p) && plane_crosses_box(bsp, **p, box_))
        {
            let (below_list, above_list) = chop(bsp, &frags, fp);
            let (lo_box, hi_box) = split_box(bsp, fp, box_);
            let index = bsp.nodes3d.len();
            bsp.nodes3d.push(Node3d { plane: fp, back: 0, front: 0 });
            used.push(fp);
            let back = recurse(bsp, below_list, used, depth + 1, max_depth, forced, lo_box)?;
            let front = recurse(bsp, above_list, used, depth + 1, max_depth, forced, hi_box)?;
            used.pop();
            bsp.nodes3d[index] = Node3d { plane: fp, back, front };
            return Ok(index as i32);
        }
    }
    if frags.is_empty() || depth > max_depth {
        return Ok(make_leaf(bsp, &frags));
    }

    // Candidate planes are the planes of the surfaces present.
    let mut candidates: Vec<u32> = frags.iter().map(|f| bsp.surfaces[f.surface as usize].plane).collect();
    candidates.sort_unstable();
    candidates.dedup();
    candidates.retain(|p| !used.contains(p));
    if candidates.is_empty() {
        return Ok(make_leaf(bsp, &frags));
    }

    // Depth is set by the larger child, so minimise that: a surface the
    // node cuts lands in both children, one it consumes lands in
    // neither. Ties go to whichever cuts less, then to the first
    // candidate, so the same input always builds the same tree.
    //
    // The classification here is deliberately the same rule the chop
    // below uses, epsilon included. Scoring with one rule and cutting
    // with another means the winner is chosen on a split that never
    // happens.
    let mut best: Option<(usize, usize, u32)> = None;
    for &c in &candidates {
        let (n, d) = plane_of(bsp, c);
        let (mut above, mut below, mut splits) = (0usize, 0usize, 0usize);
        for f in &frags {
            let sf = &bsp.surfaces[f.surface as usize];
            if sf.plane == c {
                // Rides down on the side its solid is on.
                if sf.flipped {
                    above += 1;
                } else {
                    below += 1;
                }
                continue;
            }
            let (sn, _) = plane_of(bsp, sf.plane);
            let eps = (len3(cross3(sn, n)) * CLASSIFY_EPS)
                .abs()
                .max(CHOP_EPS_MIN)
                .max(quantisation_eps(&f.poly));
            let (lo, hi) = range(&f.poly, n, d);
            if lo < -eps && hi > eps {
                splits += 1;
            } else {
                // Inside the band it goes to both sides, so it counts on
                // both.
                if hi <= eps {
                    below += 1;
                }
                if lo >= -eps {
                    above += 1;
                }
            }
        }
        let key = (above.max(below) + splits, splits);
        if best.is_none_or(|(bl, bs, _)| key < (bl, bs)) {
            best = Some((key.0, key.1, c));
        }
    }
    let (larger, _, face_plane) = best.expect("candidates is not empty");
    let mut plane = face_plane;

    // A cell no face plane separates is convex: every face has all the
    // others behind it, so the best any of them can do is consume its own
    // coplanar run and hand everything else to one child. Left alone that
    // peels one face per node — guardian's 180-triangle shield built a
    // list 180 deep. Cut it with a plane of our own instead.
    let mut synthetic = false;
    if larger >= frags.len() {
        if let Some(cut) = median_split_plane(bsp, &frags) {
            plane = cut;
            synthetic = true;
        }
    }
    let (mut below_list, mut above_list) = chop(bsp, &frags, plane);

    // Nothing lies on a plane we invented, so it consumes nothing and
    // never becomes unavailable further down. Its only claim on a node is
    // that it actually divided the cell; without that the recursion would
    // cut the same fragments forever. Give the plane back and use the
    // face plane after all — it always consumes its own coplanar run, so
    // it makes progress even when it separates nothing.
    if synthetic && (below_list.len() >= frags.len() || above_list.len() >= frags.len()) {
        bsp.planes.pop();
        plane = face_plane;
        let redone = chop(bsp, &frags, plane);
        below_list = redone.0;
        above_list = redone.1;
    }

    let index = bsp.nodes3d.len();
    bsp.nodes3d.push(Node3d { plane, back: 0, front: 0 });
    used.push(plane);
    let (lo_box, hi_box) = split_box(bsp, plane, box_);
    let back = recurse(bsp, below_list, used, depth + 1, max_depth, forced, lo_box)?;
    let front = recurse(bsp, above_list, used, depth + 1, max_depth, forced, hi_box)?;
    used.pop();
    bsp.nodes3d[index].back = back;
    bsp.nodes3d[index].front = front;
    Ok(index as i32)
}



/// Split a cell's fragments against a plane.
///
/// Surfaces lying on the plane go down the side they bound, so they reach
/// a leaf rather than stopping at this node. Everything else is clipped,
/// with an epsilon that scales with the angle between the surface and the
/// plane so a near-tangent cut does not shatter it; a surface inside that
/// band belongs to both sides.
fn chop(bsp: &Bsp, frags: &[Frag], plane: u32) -> (Vec<Frag>, Vec<Frag>) {
    let (n, d) = plane_of(bsp, plane);
    let mut below_list: Vec<Frag> = Vec::new();
    let mut above_list: Vec<Frag> = Vec::new();
    for f in frags {
        let sf = &bsp.surfaces[f.surface as usize];
        if sf.plane == plane {
            if sf.flipped {
                above_list.push(f.clone());
            } else {
                below_list.push(f.clone());
            }
            continue;
        }
        let (sn, _) = plane_of(bsp, sf.plane);
        let eps = (len3(cross3(sn, n)) * CLASSIFY_EPS)
            .abs()
            .max(CHOP_EPS_MIN)
            .max(quantisation_eps(&f.poly));
        let (lo, hi) = range(&f.poly, n, d);
        if lo < -eps && hi > eps {
            let back = clip(&f.poly, n, d, false);
            let front = clip(&f.poly, n, d, true);
            if back.len() >= 3 {
                below_list.push(Frag { surface: f.surface, poly: back });
            }
            if front.len() >= 3 {
                above_list.push(Frag { surface: f.surface, poly: front });
            }
        } else {
            if hi <= eps {
                below_list.push(f.clone());
            }
            if lo >= -eps {
                above_list.push(f.clone());
            }
        }
    }
    (below_list, above_list)
}

/// How far a polygon's own coordinates can move when written as f32.
///
/// The tree is decided here in f64 but stored — planes and vertices
/// alike — in f32, and the runtime descends using the stored values. At
/// level coordinates of a few hundred, consecutive f32 values are about
/// 3e-5 apart, so a plane can land that far from where this code put it.
/// Deciding a polygon's side with an epsilon finer than that is deciding
/// it on digits the tag cannot carry: the builder registers the surface
/// in one cell and the runtime looks in the other, and the collision is
/// missing from part of its own face.
///
/// A weapon sits at coordinates near 1 and needs no such slack, which is
/// why this is derived from the geometry rather than fixed.
fn quantisation_eps(poly: &[V3]) -> f64 {
    let mut m = 0.0f64;
    for v in poly {
        for k in 0..3 {
            m = m.max(v[k].abs());
        }
    }
    m * 4.0 * f32::EPSILON as f64
}

/// Does this plane pass through the box, rather than missing it?
fn plane_crosses_box(bsp: &Bsp, plane: u32, box_: [[f64; 2]; 3]) -> bool {
    let (n, d) = plane_of(bsp, plane);
    let (mut lo, mut hi) = (0.0f64, 0.0f64);
    for k in 0..3 {
        let (a, b) = (n[k] * box_[k][0], n[k] * box_[k][1]);
        lo += a.min(b);
        hi += a.max(b);
    }
    lo + d < -CLASSIFY_EPS && hi + d > CLASSIFY_EPS
}

/// The box split on its dominant axis, the same approximation the leaf
/// walk uses: an oblique plane does not cut a box into boxes, and these
/// are only used to ask whether a plane is worth applying.
fn split_box(bsp: &Bsp, plane: u32, box_: [[f64; 2]; 3]) -> ([[f64; 2]; 3], [[f64; 2]; 3]) {
    let (n, d) = plane_of(bsp, plane);
    let axis = (0..3).max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs())).unwrap_or(0);
    if n[axis].abs() <= 0.0 {
        return (box_, box_);
    }
    let mut at = -d;
    for k in 0..3 {
        if k != axis {
            at -= n[k] * 0.5 * (box_[k][0] + box_[k][1]);
        }
    }
    let cut = (at / n[axis]).clamp(box_[axis][0], box_[axis][1]);
    let (mut lo_box, mut hi_box) = (box_, box_);
    lo_box[axis][1] = cut;
    hi_box[axis][0] = cut;
    if n[axis] > 0.0 {
        (lo_box, hi_box)
    } else {
        (hi_box, lo_box)
    }
}

/// Make a leaf holding the surfaces whose fragments are in this cell.
///
/// A surface reaches every leaf its fragments do. Taking the single leaf
/// under its centroid instead leaves the rest of a cut surface referenced
/// by nothing, which is collision that silently is not there.
fn make_leaf(bsp: &mut Bsp, frags: &[Frag]) -> i32 {
    let mut surfaces: Vec<u32> = frags.iter().map(|f| f.surface).collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    let leaf = bsp.leaves.len();
    bsp.leaves.push(Leaf::default());
    bsp.leaf_surfaces.push(surfaces);
    flag_ref(leaf)
}

/// A plane to cut a cell that no face plane separates.
///
/// Across the longest axis of the fragments' bounds, at the median of
/// their centroids, so each side gets some of them however unevenly they
/// are spread. Returns the new plane's index, or `None` if the cell is
/// too small to be worth cutting or the cut would not divide it.
fn median_split_plane(bsp: &mut Bsp, frags: &[Frag]) -> Option<u32> {
    if frags.len() < 3 {
        return None;
    }
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    let mut centroids: Vec<V3> = Vec::with_capacity(frags.len());
    for f in frags {
        let mut c = [0.0f64; 3];
        for p in &f.poly {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
                c[k] += p[k] / f.poly.len() as f64;
            }
        }
        centroids.push(c);
    }

    let axis = (0..3).max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b])))?;
    if hi[axis] - lo[axis] <= CHOP_EPS_MIN {
        return None;
    }

    let mut along: Vec<f64> = centroids.iter().map(|c| c[axis]).collect();
    along.sort_unstable_by(f64::total_cmp);

    // Between the two centroids either side of the median, not on it: a
    // cut through a centroid puts that fragment's own probe point on the
    // plane, where f32 decides the side and the answer is a coin toss.
    let m = along.len() / 2;
    let at = if m > 0 && along[m] > along[m - 1] {
        0.5 * (along[m - 1] + along[m])
    } else {
        // Every centroid on this side shares a coordinate, so the median
        // separates nothing; the middle of the bounds still might.
        0.5 * (lo[axis] + hi[axis])
    };
    if at <= lo[axis] + CHOP_EPS_MIN || at >= hi[axis] - CHOP_EPS_MIN {
        return None;
    }

    let mut normal = [0.0f64; 3];
    normal[axis] = 1.0;
    let plane = bsp.planes.len() as u32;
    bsp.planes.push(RealPlane3d {
        i: normal[0] as f32,
        j: normal[1] as f32,
        k: normal[2] as f32,
        d: at as f32,
    });
    Some(plane)
}

/// Sutherland–Hodgman against a plane. `front` selects which half.
fn clip(poly: &[V3], n: V3, d: f64, front: bool) -> Vec<V3> {
    let sign = if front { 1.0 } else { -1.0 };
    let mut out: Vec<V3> = Vec::new();
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let da = (dot3(n, a) + d) * sign;
        let db = (dot3(n, b) + d) * sign;
        if da >= 0.0 {
            out.push(a);
        }
        if (da > 0.0 && db < 0.0) || (da < 0.0 && db > 0.0) {
            let t = da / (da - db);
            out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]);
        }
    }
    out
}

/// Descend the finished tree from a point.
fn descend(bsp: &Bsp, p: V3) -> i32 {
    if bsp.nodes3d.is_empty() {
        return 0;
    }
    // In f32, over the f32 plane, with the same expression the packed
    // walk uses. Deciding a side in f64 here and in f32 at runtime puts
    // points near a plane on opposite sides, and a surface filed in one
    // leaf is then looked for in another.
    let q = [p[0] as f32, p[1] as f32, p[2] as f32];
    let mut cur = 0i32;
    loop {
        if cur < 0 {
            return ref_index(cur) as i32;
        }
        let node = bsp.nodes3d[cur as usize];
        let pd = &bsp.planes[node.plane as usize];
        let sdist = pd.i * q[0] + pd.j * q[1] + pd.k * q[2] - pd.d;
        cur = if sdist >= 0.0 { node.front } else { node.back };
    }
}

/// Put every surface in the leaf a ray striking it would reach, then
/// group each leaf's surfaces by plane into bsp2d references.
///
/// This is the step that replaces Tool's second "connection geometry"
/// list, and it is chosen because it is directly checkable: the property
/// wanted is that a surface is found from the leaf its own face leads to.
/// Every leaf a surface's own polygon reaches, by pushing it down the
/// finished tree.
///
/// The recursion already records the surfaces whose fragments were in a
/// cell, but that bookkeeping is carried through every split and a
/// fragment lost anywhere is a surface that cannot be hit in that part of
/// the level. This recomputes the same thing from the finished tree,
/// where the only inputs are the polygon and the node planes, so a
/// surface reaches whatever its geometry says it reaches.
///
/// Coplanar with a node plane, the polygon goes to the solid side — the
/// same rule `chop` uses, so that a face is registered behind itself
/// rather than in the open space in front.
fn leaves_touching(bsp: &Bsp, si: usize, poly: &[V3], out: &mut Vec<u32>) {
    fn walk(bsp: &Bsp, at: i32, si: usize, poly: Vec<V3>, depth: usize, out: &mut Vec<u32>) {
        if poly.len() < 3 || depth > MAX_DEPTH + 8 {
            return;
        }
        if at < 0 {
            let leaf = (at as u32 & 0x7FFF_FFFF) as usize;
            if leaf < bsp.leaves.len() {
                out.push(leaf as u32);
            }
            return;
        }
        let node = &bsp.nodes3d[at as usize];
        let (n, d) = plane_of(bsp, node.plane);

        // The surface's own plane: it bounds solid rather than dividing
        // it, so it belongs on one side whole.
        let sf = &bsp.surfaces[si];
        if sf.plane == node.plane {
            let child = if sf.flipped { node.front } else { node.back };
            walk(bsp, child, si, poly, depth + 1, out);
            return;
        }

        let (sn, _) = plane_of(bsp, sf.plane);
        // Deliberately wider than the tree's own classification.
        // This decides only which leaves *reference* a surface, never
        // where a plane goes, so erring wide costs a handful of
        // references while erring narrow costs collision: a ray
        // crossing the polygon a hair off a cell boundary finds
        // nothing indexed there. Half a millimetre at level scale.
        let eps = (len3(cross3(sn, n)) * CLASSIFY_EPS)
            .abs()
            .max(CHOP_EPS_MIN)
            .max(quantisation_eps(&poly));
        let (lo, hi) = range(&poly, n, d);
        if lo < -eps && hi > eps {
            walk(bsp, node.back, si, clip(&poly, n, d, false), depth + 1, out);
            walk(bsp, node.front, si, clip(&poly, n, d, true), depth + 1, out);
        } else {
            if hi <= eps {
                walk(bsp, node.back, si, poly.clone(), depth + 1, out);
            }
            if lo >= -eps {
                walk(bsp, node.front, si, poly, depth + 1, out);
            }
        }
    }
    walk(bsp, 0, si, poly.to_vec(), 0, out);
    out.sort_unstable();
    out.dedup();
}

fn assign_surfaces_to_leaves(bsp: &mut Bsp) -> R<()> {
    // The recursion recorded, for each leaf, the surfaces with a fragment
    // in its cell. A surface a plane cut reaches several leaves and is
    // referenced from all of them.
    let mut per_leaf: BTreeMap<usize, BTreeSet<u32>> = BTreeMap::new();
    for (leaf, surfaces) in bsp.leaf_surfaces.iter().enumerate() {
        if !surfaces.is_empty() {
            per_leaf.entry(leaf).or_default().extend(surfaces.iter().copied());
        }
    }

    // And in the leaf a point just behind its own face descends into,
    // which is where the runtime looks for it. Fragments alone do not
    // guarantee that one: the probe steps off the face by an epsilon and
    // can land in a neighbouring cell.
    for si in 0..bsp.surfaces.len() {
        let s = &bsp.surfaces[si];
        let poly: Vec<V3> = s.ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect();
        let mut c = [0.0f64; 3];
        for p in &poly {
            for k in 0..3 {
                c[k] += p[k] / poly.len() as f64;
            }
        }
        // Step towards the solid side, which is behind the face.
        let (n, _) = plane_of(bsp, s.plane);
        let dir = if s.flipped { 1.0 } else { -1.0 };
        let probe =
            [c[0] + n[0] * dir * 1e-4, c[1] + n[1] * dir * 1e-4, c[2] + n[2] * dir * 1e-4];
        let leaf = descend(bsp, probe) as usize;
        if leaf >= bsp.leaves.len() {
            return Err(CollisionError::Unreachable(si));
        }
        per_leaf.entry(leaf).or_default().insert(si as u32);

        // And the same probe near each corner. The cells a surface is
        // registered in are worked out here in f64, while the runtime
        // finds its cell in f32; the two can differ by a hair, and a
        // large polygon's corners are far enough from its centre to land
        // in entirely different cells. Every ray that still disagreed
        // struck near a rim rather than in the middle, which is exactly
        // the part one centre probe never speaks for.
        //
        // Pulled a fiftieth of the way in, so the probe is inside the
        // polygon rather than on its boundary.
        for v in &poly {
            let q = [
                v[0] + (c[0] - v[0]) * 0.02 + n[0] * dir * 1e-4,
                v[1] + (c[1] - v[1]) * 0.02 + n[1] * dir * 1e-4,
                v[2] + (c[2] - v[2]) * 0.02 + n[2] * dir * 1e-4,
            ];
            let l = descend(bsp, q) as usize;
            if l < bsp.leaves.len() {
                per_leaf.entry(l).or_default().insert(si as u32);
            }
        }

        // And every leaf the polygon itself reaches. A ray crossing a
        // surface far from its centre lands in a cell the probe never
        // visits, and if the recursion's fragments did not cover that
        // cell the surface is simply not hittable there.
        let mut touched: Vec<u32> = Vec::new();
        leaves_touching(bsp, si, &poly, &mut touched);
        for l in touched {
            per_leaf.entry(l as usize).or_default().insert(si as u32);
        }
    }

    bsp.assigned_pairs = per_leaf.values().map(|v| v.len()).sum();
    for (leaf, surfaces) in per_leaf {
        let surfaces: Vec<u32> = surfaces.into_iter().collect();
        // Group by plane; each group becomes one bsp2d reference.
        let mut by_plane: BTreeMap<(u32, bool), Vec<u32>> = BTreeMap::new();
        for &s in &surfaces {
            let sf = &bsp.surfaces[s as usize];
            by_plane.entry((sf.plane, sf.flipped)).or_default().push(s);
        }
        let first = bsp.bsp2d_refs.len() as i32;
        let mut count = 0u16;
        for ((plane, flipped), list) in by_plane {
            let (n, _) = plane_of(bsp, plane);
            let axis = dominant_axis(n);
            let sign = (n[axis] > 0.0) != flipped;
            // Coplanar surfaces that overlap cannot share one 2D tree —
            // a leaf of that tree names a single surface. They can have
            // one tree each, so keep emitting references for whatever
            // the last one could not hold. Every round places at least
            // one surface, so this terminates.
            let mut todo = list;
            while !todo.is_empty() {
                let mut leftover: Vec<u32> = Vec::new();
                let node = build_bsp2d(bsp, axis, sign, &todo, &mut leftover);
                bsp.bsp2d_refs.push(Ref2d { plane, flipped, node });
                count += 1;
                if leftover.len() >= todo.len() {
                    // The round placed nothing, so looping on it would
                    // not terminate. Peel one surface into a reference of
                    // its own — always possible, since a single surface
                    // is a bsp2d leaf — and carry the rest on. This is
                    // what keeps a surface from being lost when the line
                    // search cannot divide the group at all.
                    let mut rest = leftover;
                    rest.sort_unstable();
                    rest.dedup();
                    let one = rest.remove(0);
                    bsp.bsp2d_refs.push(Ref2d {
                        plane,
                        flipped,
                        node: flag_ref(one as usize),
                    });
                    count += 1;
                    todo = rest;
                    continue;
                }
                todo = leftover;
            }
        }
        bsp.leaves[leaf].first_ref = if count > 0 { first } else { -1 };
        bsp.leaves[leaf].ref_count = count;
    }
    for l in &mut bsp.leaves {
        if l.ref_count == 0 {
            l.first_ref = -1;
        }
    }
    Ok(())
}

fn dominant_axis(n: V3) -> usize {
    let a = [n[0].abs(), n[1].abs(), n[2].abs()];
    if a[0] >= a[1] && a[0] >= a[2] {
        0
    } else if a[1] >= a[2] {
        1
    } else {
        2
    }
}

/// The projection table: `(u, v, discarded)` per (axis, sign). The swap
/// between the two sign rows keeps the 2D winding consistent when the
/// plane faces the negative direction.
const PROJECTION: [[usize; 3]; 6] = [
    [2, 1, 0], // axis 0, sign 0
    [1, 2, 0], // axis 0, sign 1
    [0, 2, 1], // axis 1, sign 0
    [2, 0, 1], // axis 1, sign 1
    [1, 0, 2], // axis 2, sign 0
    [0, 1, 2], // axis 2, sign 1
];

/// Twice the signed area of a 2D polygon, by the shoelace formula. Used
/// only to pick the least damaging surface to drop.
fn area2(poly: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..poly.len() {
        let p = poly[i];
        let q = poly[(i + 1) % poly.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a.abs()
}

fn project(p: V3, axis: usize, sign: bool) -> [f64; 2] {
    let t = PROJECTION[2 * axis + usize::from(sign)];
    [p[t[0]], p[t[1]]]
}

/// A 2D BSP over the surfaces coplanar with one plane in one leaf.
/// Returns an encoded child: `>= 0` node index, `< 0` surface as
/// `!surface`.

/// Which side of a 2D line a surface goes.
///
/// Per side, not exclusive: a surface with area on the negative side goes
/// left, one with area on the positive side goes right, and a straddling
/// surface goes to **both** — surfaces are never clipped in 2D. Testing
/// `hi <= eps` and `lo >= -eps` instead sends a straddler to neither and
/// quietly loses it.
///
/// A surface lying entirely inside the band satisfies neither test. The
/// band is 2^-13 world units, so that is a sliver off a t-junction, and
/// it goes to the side its centre falls on: one child, nothing lost.
#[allow(clippy::too_many_arguments)]
fn classify2d(
    bsp: &Bsp,
    t: u32,
    axis: usize,
    sign: bool,
    n: [f64; 2],
    d: f64,
    left: &mut Vec<u32>,
    right: &mut Vec<u32>,
) {
    const EPS: f64 = EPS_2D;
    let ring = &bsp.surfaces[t as usize].ring;
    let (mut lo, mut hi, mut sum) = (f64::MAX, f64::MIN, 0.0f64);
    for &v in ring {
        let q = project(pt(&bsp.vertices[v as usize]), axis, sign);
        let x = n[0] * q[0] + n[1] * q[1] + d;
        lo = lo.min(x);
        hi = hi.max(x);
        sum += x;
    }
    let mut placed = false;
    if lo < -EPS {
        left.push(t);
        placed = true;
    }
    if hi > EPS {
        right.push(t);
        placed = true;
    }
    if !placed {
        if sum / ring.len() as f64 <= 0.0 {
            left.push(t);
        } else {
            right.push(t);
        }
    }
}

fn build_bsp2d(
    bsp: &mut Bsp,
    axis: usize,
    sign: bool,
    surfaces: &[u32],
    leftover: &mut Vec<u32>,
) -> i32 {
    if surfaces.len() <= 1 {
        return flag_ref(surfaces.first().copied().unwrap_or(0) as usize);
    }
    // Candidate lines are the edges of the surfaces present.
    let poly2 = |bsp: &Bsp, s: u32| -> Vec<[f64; 2]> {
        bsp.surfaces[s as usize]
            .ring
            .iter()
            .map(|&v| project(pt(&bsp.vertices[v as usize]), axis, sign))
            .collect()
    };

    let mut chosen: Option<([f64; 3], Vec<u32>, Vec<u32>)> = None;
    'outer: for &s in surfaces {
        let p = poly2(bsp, s);
        for k in 0..p.len() {
            let a = p[k];
            let b = p[(k + 1) % p.len()];
            let e = [b[0] - a[0], b[1] - a[1]];
            let l = (e[0] * e[0] + e[1] * e[1]).sqrt();
            if l < 1e-12 {
                continue;
            }
            // Outward normal of the edge, and the line through it.
            let n = [e[1] / l, -e[0] / l];
            let d = -(n[0] * a[0] + n[1] * a[1]);
            let (mut left, mut right) = (Vec::new(), Vec::new());
            for &t in surfaces {
                classify2d(bsp, t, axis, sign, [n[0], n[1]], d, &mut left, &mut right);
            }
            // A line that separates nothing is useless, and one that puts
            // everything on both sides would not terminate.
            if !left.is_empty()
                && !right.is_empty()
                && left.len() < surfaces.len()
                && right.len() < surfaces.len()
            {
                chosen = Some(([n[0], n[1], d], left, right));
                break 'outer;
            }
        }
    }

    // No edge line worked. Before losing a surface, try a line of our
    // own: across the wider axis, through the median of the surface
    // centres. Their centres then sit either side of it by construction,
    // so it separates anything that is not actually coincident.
    if chosen.is_none() {
        let centres: Vec<[f64; 2]> = surfaces
            .iter()
            .map(|&t| {
                let q = poly2(bsp, t);
                let mut c = [0.0f64; 2];
                for v in &q {
                    c[0] += v[0] / q.len() as f64;
                    c[1] += v[1] / q.len() as f64;
                }
                c
            })
            .collect();
        let spread = |k: usize| -> f64 {
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for c in &centres {
                lo = lo.min(c[k]);
                hi = hi.max(c[k]);
            }
            hi - lo
        };
        let wider = if spread(0) >= spread(1) { 0 } else { 1 };
        for k in [wider, 1 - wider] {
            let mut along: Vec<f64> = centres.iter().map(|c| c[k]).collect();
            along.sort_unstable_by(f64::total_cmp);
            let m = along.len() / 2;
            if m == 0 || along[m] <= along[m - 1] {
                continue;
            }
            let at = 0.5 * (along[m - 1] + along[m]);
            let mut nrm = [0.0f64; 2];
            nrm[k] = 1.0;
            let d = -at;
            let (mut left, mut right) = (Vec::new(), Vec::new());
            for &t in surfaces {
                classify2d(bsp, t, axis, sign, nrm, d, &mut left, &mut right);
            }
            if !left.is_empty()
                && !right.is_empty()
                && left.len() < surfaces.len()
                && right.len() < surfaces.len()
            {
                chosen = Some(([nrm[0], nrm[1], d], left, right));
                break;
            }
        }
    }

    // Still nothing. A node line has to leave every surface whole and
    // on one side — with two surfaces in a group, a line that cuts
    // either one puts it on both sides, the group never shrinks, and the
    // recursion would not terminate. So the search has to find a true
    // separating line or lose a surface.
    //
    // Edge-supporting lines find one whenever the surfaces are convex,
    // which is what the separating axis theorem promises. The coplanar
    // merge produces concave rings, and for those the separating
    // direction need not be normal to any edge — which is how 36
    // separable pairs were being dropped as if they were coincident.
    //
    // So project every surface onto a direction and look for a gap. Edge
    // normals first, since they are exact when they work, then a sweep
    // of directions for the concave cases.
    if chosen.is_none() {
        let polys: Vec<Vec<[f64; 2]>> = surfaces.iter().map(|&t| poly2(bsp, t)).collect();
        let mut dirs: Vec<[f64; 2]> = Vec::new();
        for q in &polys {
            for k in 0..q.len() {
                let (a, b) = (q[k], q[(k + 1) % q.len()]);
                let e = [b[0] - a[0], b[1] - a[1]];
                let l = (e[0] * e[0] + e[1] * e[1]).sqrt();
                if l >= 1e-12 {
                    dirs.push([e[1] / l, -e[0] / l]);
                }
            }
        }
        // A half turn covers every line orientation; the other half is
        // the same lines with the sign flipped.
        const SWEEP: usize = 180;
        for i in 0..SWEEP {
            let a = std::f64::consts::PI * i as f64 / SWEEP as f64;
            dirs.push([a.cos(), a.sin()]);
        }

        'sweep: for nrm in dirs {
            // Each surface's extent along this direction.
            let mut spans: Vec<(f64, f64, u32)> = Vec::with_capacity(polys.len());
            for (i, q) in polys.iter().enumerate() {
                let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                for v in q {
                    let d = nrm[0] * v[0] + nrm[1] * v[1];
                    lo = lo.min(d);
                    hi = hi.max(d);
                }
                spans.push((lo, hi, surfaces[i]));
            }
            spans.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));

            // A gap between consecutive spans, once every earlier surface
            // ends before the next one starts, splits the group cleanly.
            let mut reach = spans[0].1;
            for k in 1..spans.len() {
                if reach < spans[k].0 - 2.0 * EPS_2D {
                    let at = 0.5 * (reach + spans[k].0);
                    let (l, r): (Vec<u32>, Vec<u32>) = (
                        spans[..k].iter().map(|s| s.2).collect(),
                        spans[k..].iter().map(|s| s.2).collect(),
                    );
                    chosen = Some(([nrm[0], nrm[1], -at], l, r));
                    break 'sweep;
                }
                reach = reach.max(spans[k].1);
            }
        }
    }

    let Some((plane, left, right)) = chosen else {
        // Genuinely coincident: no line, ours or theirs, separates them.
        // Tool warns, drops the **smallest-area** surface and retries,
        // rather than giving up on the whole group; dropping all but one
        // loses far more collision than it needs to.
        let mut remaining = surfaces.to_vec();
        let victim = remaining
            .iter()
            .enumerate()
            .min_by(|a, b| {
                area2(&poly2(bsp, *a.1)).total_cmp(&area2(&poly2(bsp, *b.1)))
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        // Not dropped: handed back. A leaf may hold more than one bsp2d
        // reference, so a surface that cannot share this tree gets a
        // tree of its own in the same leaf and stays collidable.
        leftover.push(remaining[victim]);
        remaining.remove(victim);
        return build_bsp2d(bsp, axis, sign, &remaining, leftover);
    };

    // Nothing should fall out of the classification any more — a sliver
    // goes to the side its centre is on. Kept as a net: if one ever does,
    // it is counted rather than lost silently.
    for &t in surfaces {
        if !left.contains(&t) && !right.contains(&t) {
            leftover.push(t);
        }
    }

    let index = bsp.bsp2d_nodes.len();
    bsp.bsp2d_nodes.push(Node2d { plane: [0.0; 3], left: 0, right: 0 });
    let l = build_bsp2d(bsp, axis, sign, &left, leftover);
    let r = build_bsp2d(bsp, axis, sign, &right, leftover);
    bsp.bsp2d_nodes[index] = Node2d {
        plane: [plane[0] as f32, plane[1] as f32, plane[2] as f32],
        left: l,
        right: r,
    };
    index as i32
}

fn depth_of(bsp: &Bsp, node: i32) -> usize {
    if node < 0 || bsp.nodes3d.is_empty() {
        return 0;
    }
    let n = bsp.nodes3d[node as usize];
    1 + depth_of(bsp, n.back).max(depth_of(bsp, n.front))
}

// -------------------------------------------------------------- packing

/// The shared s15 + bit15 transport. `-1` is "none"; bit 15 carries the
/// per-field flag and maps to bit 31 on the way back out.
fn pack_s15(v: i32) -> i16 {
    if v == -1 {
        return -1i16; // 0xFFFF
    }
    let u = (v as u32) & 0x7FFF;
    let flag = if v < 0 { 0x8000u32 } else { 0 };
    (u | flag) as u16 as i16
}

/// A 24-bit child inside the packed `bsp3d_node`. `-1` is the "no child"
/// sentinel `0xFFFFFF`; a leaf sets bit 23.
fn pack_child24(c: i32) -> u64 {
    // `-1` is the format's "no child", and it has to be tested before the
    // flag bit — `flag_ref(0)` is `0x80000000`, which is a perfectly
    // ordinary flagged leaf and must not be mistaken for absence.
    if c == -1 {
        return 0xFF_FFFF;
    }
    if c < 0 {
        // A flagged leaf: keep the index, move the flag from bit 31 to
        // bit 23 where this field carries it.
        let leaf = (c as u32) & 0x7FFF_FFFF;
        return ((leaf & 0x7F_FFFF) | 0x80_0000) as u64;
    }
    (c as u32 & 0xFF_FFFF) as u64
}

/// Plane in bits 0..15, back child in 16..39, front child in 40..63.
fn pack_node3d(n: &Node3d) -> i64 {
    let plane = (n.plane as u64) & 0xFFFF;
    let back = pack_child24(n.back) << 16;
    let front = pack_child24(n.front) << 40;
    (plane | back | front) as i64
}

// -------------------------------------------------------------- writing

fn with_block<T>(
    root: &mut crate::TagStructMut<'_>,
    path: &str,
    f: impl FnOnce(&mut crate::TagBlockMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = root
        .field_path_mut(path)
        .ok_or_else(|| CollisionError::MissingField(path.into()))?;
    let mut blk = fld
        .as_block_mut()
        .ok_or_else(|| CollisionError::MissingField(format!("{path} (not a block)")))?;
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
            try_set(&mut el, "next sibling node", TagFieldData::ShortBlockIndex(sibling[i]));
            try_set(&mut el, "first child node", TagFieldData::ShortBlockIndex(first_child[i]));
        }
        Ok(())
    })
}

fn write_materials(tag: &mut TagFile, jms: &JmsFile) -> R<usize> {
    let n = jms.materials.len();
    let mut root = tag.root_mut();
    with_block(&mut root, "materials", |block| {
        for m in &jms.materials {
            let idx = block.add_element();
            let mut el = block.element_mut(idx).expect("just added");
            try_set(&mut el, "name", string_id(&m.name.to_ascii_lowercase()));
        }
        Ok(n)
    })
}

fn write_regions(
    tag: &mut TagFile,
    built: &[((String, String), Bsp)],
    report: &mut CollisionReport,
) -> R<()> {
    // region -> permutation -> the BSPs under it
    let mut regions: Vec<(String, Vec<(String, Vec<usize>)>)> = Vec::new();
    for (i, ((region, perm), _)) in built.iter().enumerate() {
        let r = match regions.iter_mut().find(|(n, _)| n == region) {
            Some(r) => r,
            None => {
                regions.push((region.clone(), Vec::new()));
                regions.last_mut().expect("just pushed")
            }
        };
        match r.1.iter_mut().find(|(n, _)| n == perm) {
            Some((_, list)) => list.push(i),
            None => r.1.push((perm.clone(), vec![i])),
        }
    }
    if regions.len() > 16 {
        return Err(CollisionError::TooManyRegions(regions.len()));
    }
    report.regions = regions.len();
    report.permutations = regions.iter().map(|(_, p)| p.len()).sum();

    let mut root = tag.root_mut();
    with_block(&mut root, "regions", |rb| {
        for (name, perms) in &regions {
            let ri = rb.add_element();
            let mut r = rb.element_mut(ri).expect("just added");
            try_set(&mut r, "name", string_id(name));
            let mut fld = r
                .field_mut("permutations")
                .ok_or_else(|| CollisionError::MissingField("permutations".into()))?;
            let mut pb = fld
                .as_block_mut()
                .ok_or_else(|| CollisionError::MissingField("permutations".into()))?;
            for (pname, indices) in perms {
                if indices.len() > MAX_BSPS_PER_PERMUTATION {
                    return Err(CollisionError::TooLarge {
                        what: "BSPs in one permutation",
                        count: indices.len(),
                        max: MAX_BSPS_PER_PERMUTATION,
                    });
                }
                let pi = pb.add_element();
                let mut p = pb.element_mut(pi).expect("just added");
                try_set(&mut p, "name", string_id(pname));
                let mut fld = p
                    .field_mut("bsps")
                    .ok_or_else(|| CollisionError::MissingField("bsps".into()))?;
                let mut bb = fld
                    .as_block_mut()
                    .ok_or_else(|| CollisionError::MissingField("bsps".into()))?;
                for &bi in indices {
                    let bsp = &built[bi].1;
                    let ei = bb.add_element();
                    let mut e = bb.element_mut(ei).expect("just added");
                    try_set(&mut e, "node index", TagFieldData::ShortInteger(bsp.node_index));
                    write_bsp(&mut e, bsp)?;
                }
            }
        }
        Ok(())
    })
}

/// A built collision BSP, ready to write.
///
/// Opaque on purpose: the packing of a bsp3d node and the s15 transport
/// of a bsp2d child are this module's business, and a caller that
/// reached into them would be reimplementing the half of this that is
/// easy to get subtly wrong.
pub struct CollisionBsp(Bsp);

/// Build a collision BSP over a plain triangle list.
///
/// The same structure a `collision_model` carries, which is also what a
/// `scenario_structure_bsp` keeps at `resource interface/
/// raw_resources[0]/raw_items/collision bsp` — so a structure importer
/// gets the tree builder, the coplanar merge and the ray check in
/// [`crate::collision_verify`] without duplicating any of it.
pub fn collision_bsp_from_triangles(
    positions: &[RealPoint3d],
    triangles: &[([u32; 3], i16)],
) -> R<CollisionBsp> {
    collision_bsp_from_triangles_with(positions, triangles, true)
}

/// Build a collision BSP whose cells never straddle one of these
/// planes.
///
/// The planes are portals. They carry no collision of their own, so
/// nothing in the geometry would make the tree divide there, and the two
/// sides of a portal end up in the same cell — which makes it impossible
/// to file the leaves either side of it under different clusters. Each
/// plane is added to the table if it is not already there, and forced on
/// the cells it crosses.
pub fn collision_bsp_from_triangles_with_portals(
    positions: &[RealPoint3d],
    triangles: &[([u32; 3], i16)],
    portals: &[([f32; 3], f32)],
    world: [[f32; 2]; 3],
) -> R<CollisionBsp> {
    let mut bsp = build_geometry_from(positions, triangles, 0)?;

    let mut forced: Vec<u32> = Vec::new();
    for (n, d) in portals {
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if !l.is_finite() || l <= 1e-6 {
            continue;
        }
        let (nn, dd) = ([n[0] / l, n[1] / l, n[2] / l], d / l);
        // Reuse an existing plane when one already describes this, in
        // either orientation: a portal often sits flush with a wall.
        let found = bsp.planes.iter().position(|p| {
            let dot = p.i * nn[0] + p.j * nn[1] + p.k * nn[2];
            (dot > 0.9999 && (p.d - dd).abs() < 1e-4)
                || (dot < -0.9999 && (p.d + dd).abs() < 1e-4)
        });
        let idx = match found {
            Some(i) => i,
            None => {
                bsp.planes.push(RealPlane3d { i: nn[0], j: nn[1], k: nn[2], d: dd });
                bsp.planes.len() - 1
            }
        };
        if !forced.contains(&(idx as u32)) {
            forced.push(idx as u32);
        }
    }

    let world64 = [
        [world[0][0] as f64, world[0][1] as f64],
        [world[1][0] as f64, world[1][1] as f64],
        [world[2][0] as f64, world[2][1] as f64],
    ];
    build_tree_with(&mut bsp, MAX_DEPTH, &forced, Some(world64))?;
    Ok(CollisionBsp(bsp))
}

/// The same, choosing whether coplanar neighbours are merged.
///
/// Merging is on everywhere. It was briefly switched off for a sealed
/// world taken from render meshes, on the strength of a check that said
/// 54 of `anchor_point`'s rings sat up to 4.4 mm off their own plane —
/// which turned out to be the check misreading a flipped surface's
/// plane index, not the merge misplacing anything. The builder had those
/// rings within 6 microns the whole time.
pub fn collision_bsp_from_triangles_with(
    positions: &[RealPoint3d],
    triangles: &[([u32; 3], i16)],
    merge: bool,
) -> R<CollisionBsp> {
    let mut bsp = build_geometry_from_with(positions, triangles, 0, merge)?;
    build_tree(&mut bsp, MAX_DEPTH)?;
    Ok(CollisionBsp(bsp))
}

impl CollisionBsp {
    pub fn surfaces(&self) -> usize {
        self.0.surfaces.len()
    }
    pub fn nodes(&self) -> usize {
        self.0.nodes3d.len()
    }
    /// Surfaces the 2D builder had to discard — collision that will not
    /// be there.
    pub fn dropped(&self) -> usize {
        self.0.dropped.len()
    }
    /// (leaf, surface) pairs the assignment produced.
    pub fn assigned_pairs(&self) -> usize {
        self.0.assigned_pairs
    }
    /// How many winged edges the tag has to carry.
    pub fn edges(&self) -> usize {
        self.0.edges.len()
    }
    /// How many leaves the tree has.
    ///
    /// A structure BSP's `leaves` block runs parallel to this one —
    /// measured on shipped levels, where guardian has 11,305 of each and
    /// isolation 19,594 — so this is the length the partition has to be.
    pub fn leaf_count(&self) -> usize {
        self.0.leaves.len()
    }

    /// Which leaf a point falls in, by the same descent the runtime uses.
    pub fn leaf_at(&self, p: [f32; 3]) -> usize {
        descend(&self.0, [p[0] as f64, p[1] as f64, p[2] as f64]) as usize
    }

    /// A point inside each leaf.
    ///
    /// Descends from the root carrying a box and halves it at every
    /// plane, taking the centre of whatever box reaches a leaf. That
    /// point is inside the cell by construction, unlike a centroid of
    /// the leaf's surfaces — a cell is bounded by planes rather than by
    /// the surfaces it happens to index, and many index none at all.
    pub fn leaf_centres(&self, bounds: [[f32; 2]; 3]) -> Vec<Option<[f32; 3]>> {
        self.leaf_cells(bounds)
            .into_iter()
            .map(|c| {
                c.map(|b| {
                    [
                        0.5 * (b[0][0] + b[0][1]),
                        0.5 * (b[1][0] + b[1][1]),
                        0.5 * (b[2][0] + b[2][1]),
                    ]
                })
            })
            .collect()
    }

    /// The box each leaf arrives with, which is what a centre comes
    /// from. Kept separately because the faces of that box are how
    /// one leaf finds the leaves next to it.
    pub fn leaf_cells(&self, bounds: [[f32; 2]; 3]) -> Vec<Option<[[f32; 2]; 3]>> {
        let mut out = vec![None; self.0.leaves.len()];
        let mut stack = vec![(0i32, bounds, 0usize)];
        while let Some((at, box_, depth)) = stack.pop() {
            if depth > MAX_DEPTH + 16 {
                continue;
            }
            if at < 0 {
                let leaf = ref_index(at);
                if let Some(slot) = out.get_mut(leaf) {
                    if slot.is_none() {
                        *slot = Some(box_);
                    }
                }
                continue;
            }
            let Some(node) = self.0.nodes3d.get(at as usize) else { continue };
            let (n, d) = plane_of(&self.0, node.plane);
            let axis = (0..3).max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs())).unwrap_or(0);
            if n[axis].abs() <= 0.0 {
                continue;
            }
            // Where the plane crosses that axis through the box centre.
            // An oblique plane does not cut a box into boxes; this keeps
            // the tightest axis-aligned halves that still contain the
            // true cells, which is all the centre needs.
            let mut at_axis = -d;
            for k in 0..3 {
                if k != axis {
                    at_axis -= n[k] * 0.5 * (box_[k][0] + box_[k][1]) as f64;
                }
            }
            let cut = (at_axis / n[axis]) as f32;
            let cut = cut.clamp(box_[axis][0], box_[axis][1]);
            let (mut lo_box, mut hi_box) = (box_, box_);
            lo_box[axis][1] = cut;
            hi_box[axis][0] = cut;
            let (front_box, back_box) =
                if n[axis] > 0.0 { (hi_box, lo_box) } else { (lo_box, hi_box) };
            stack.push((node.front, front_box, depth + 1));
            stack.push((node.back, back_box, depth + 1));
        }
        out
    }

    /// The worst distance any surface vertex sits from the plane its
    /// own surface names, measured on the builder side.
    ///
    /// The tag side measures the same thing. If the two disagree the
    /// fault is in what gets written or decoded; if they agree, the
    /// guard that was meant to prevent it never ran.
    pub fn worst_plane_offset(&self) -> f32 {
        let mut worst = 0.0f64;
        for s in &self.0.surfaces {
            let (n, d) = plane_of(&self.0, s.plane);
            let len = len3(n);
            if len <= 0.0 {
                continue;
            }
            for &v in &s.ring {
                let p = pt(&self.0.vertices[v as usize]);
                worst = worst.max(((dot3(n, p) + d) / len).abs());
            }
        }
        worst as f32
    }

    /// Each surface's ring, as world positions, exactly as built.
    ///
    /// The checker walks its polygons out of the written edge rings. If
    /// that walk rebuilds a different polygon than this placed, the
    /// checker is firing at geometry the tree was never told about.
    pub fn rings(&self) -> Vec<Vec<[f32; 3]>> {
        self.0
            .surfaces
            .iter()
            .map(|s| {
                s.ring
                    .iter()
                    .map(|&v| {
                        let p = &self.0.vertices[v as usize];
                        [p.x, p.y, p.z]
                    })
                    .collect()
            })
            .collect()
    }
    /// Which leaves touch which, and across what.
    ///
    /// Every pair of neighbouring cells meets on the plane of the node
    /// that separates them, so this builds that meeting place directly
    /// rather than sampling for it. For each node: start with a large
    /// polygon lying on its plane, clip it by every ancestor half-space
    /// so it covers exactly that node's own region, then push it down
    /// the back subtree and each surviving piece down the front. A piece
    /// that reaches a leaf on both sides lies between those two cells
    /// and nowhere else.
    ///
    /// This is the piece three approximations could not replace. Probing
    /// the axes from a cell centre found 3,000 neighbours across 86,000
    /// leaves; marching until the leaf changed found fewer; sampling a
    /// node's plane inside its bounding box left 71,081 regions for
    /// 93,580 leaves. A cell here is a convex solid cut by oblique
    /// planes and its box is a poor stand-in for it — clipping is what
    /// the shape actually needs.
    pub fn leaf_adjacency(&self, world: [[f32; 2]; 3]) -> Vec<(u32, u32, Vec<V3>, u32)> {
        let world64 = [
            [world[0][0] as f64, world[0][1] as f64],
            [world[1][0] as f64, world[1][1] as f64],
            [world[2][0] as f64, world[2][1] as f64],
        ];
        let span = (0..3)
            .fold(0.0f64, |m, k| m.max(world64[k][1] - world64[k][0]))
            .max(1.0);
        let centre = [
            0.5 * (world64[0][0] + world64[0][1]),
            0.5 * (world64[1][0] + world64[1][1]),
            0.5 * (world64[2][0] + world64[2][1]),
        ];

        let mut out: Vec<(u32, u32, Vec<V3>, u32)> = Vec::new();
        // (node, the half-spaces that bound its region)
        let mut stack: Vec<(i32, Vec<(V3, f64)>)> = vec![(0, Vec::new())];
        while let Some((at, bounds)) = stack.pop() {
            if at < 0 {
                continue;
            }
            let Some(node) = self.0.nodes3d.get(at as usize).copied() else { continue };
            let (n, d) = plane_of(&self.0, node.plane);

            // The plane, as a polygon big enough to span the level, then
            // trimmed to this node's region.
            let mut poly = big_polygon(n, d, centre, span * 4.0);
            for (bn, bd) in &bounds {
                if poly.len() < 3 {
                    break;
                }
                poly = clip(&poly, *bn, *bd, true);
            }
            if poly.len() >= 3 {
                // Down the back, then each piece down the front.
                let mut back_pieces: Vec<(Vec<V3>, usize)> = Vec::new();
                self.push_down(node.back, poly, 0, &mut back_pieces);
                for (piece, leaf_back) in back_pieces {
                    let mut front_pieces: Vec<(Vec<V3>, usize)> = Vec::new();
                    self.push_down(node.front, piece, 0, &mut front_pieces);
                    for (frag, leaf_front) in front_pieces {
                        if leaf_back != leaf_front && frag.len() >= 3 {
                            out.push((leaf_back as u32, leaf_front as u32, frag, node.plane));
                        }
                    }
                }
            }

            // The children inherit this plane as a bound: back is the
            // half-space behind it, front the one in front.
            let mut back_bounds = bounds.clone();
            back_bounds.push(([-n[0], -n[1], -n[2]], -d));
            let mut front_bounds = bounds;
            front_bounds.push((n, d));
            stack.push((node.back, back_bounds));
            stack.push((node.front, front_bounds));
        }
        out
    }

    /// Split a polygon down a subtree, collecting the piece that lands
    /// in each leaf.
    fn push_down(&self, at: i32, poly: Vec<V3>, depth: usize, out: &mut Vec<(Vec<V3>, usize)>) {
        if poly.len() < 3 || depth > MAX_DEPTH + 16 {
            return;
        }
        if at < 0 {
            out.push((poly, ref_index(at)));
            return;
        }
        let Some(node) = self.0.nodes3d.get(at as usize).copied() else { return };
        let (n, d) = plane_of(&self.0, node.plane);
        let (lo, hi) = range(&poly, n, d);
        if lo >= -CLASSIFY_EPS {
            self.push_down(node.front, poly, depth + 1, out);
        } else if hi <= CLASSIFY_EPS {
            self.push_down(node.back, poly, depth + 1, out);
        } else {
            let back = clip(&poly, n, d, false);
            let front = clip(&poly, n, d, true);
            self.push_down(node.back, back, depth + 1, out);
            self.push_down(node.front, front, depth + 1, out);
        }
    }

    /// Is this point covered by a surface lying on the same plane?
    pub fn covered_by_surface(&self, plane: u32, p: V3) -> bool {
        let (n, _) = plane_of(&self.0, plane);
        let axis = (0..3).max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs())).unwrap_or(0);
        let (u, v) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        for s in self.0.surfaces.iter().filter(|s| s.plane == plane) {
            let ring: Vec<[f64; 2]> = s
                .ring
                .iter()
                .map(|&i| {
                    let q = pt(&self.0.vertices[i as usize]);
                    [q[u], q[v]]
                })
                .collect();
            if ring.len() < 3 {
                continue;
            }
            let mut inside = false;
            let mut j = ring.len() - 1;
            for i in 0..ring.len() {
                let (c, e) = (ring[i], ring[j]);
                if (c[1] > p[v]) != (e[1] > p[v]) {
                    let x = c[0] + (p[v] - c[1]) / (e[1] - c[1]) * (e[0] - c[0]);
                    if p[u] < x {
                        inside = !inside;
                    }
                }
                j = i;
            }
            if inside {
                return true;
            }
        }
        false
    }

}

/// A square lying on a plane, centred near `about` and `size` across.
fn big_polygon(n: V3, d: f64, about: V3, size: f64) -> Vec<V3> {
    // A tangent that is not parallel to the normal.
    let up = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let t1 = {
        let c = cross3(up, n);
        let l = len3(c);
        if l <= 0.0 {
            return Vec::new();
        }
        [c[0] / l, c[1] / l, c[2] / l]
    };
    let t2 = cross3(n, t1);
    // The point on the plane closest to `about`.
    let off = dot3(n, about) + d;
    let base = [about[0] - n[0] * off, about[1] - n[1] * off, about[2] - n[2] * off];
    let h = size * 0.5;
    vec![
        [
            base[0] - t1[0] * h - t2[0] * h,
            base[1] - t1[1] * h - t2[1] * h,
            base[2] - t1[2] * h - t2[2] * h,
        ],
        [
            base[0] + t1[0] * h - t2[0] * h,
            base[1] + t1[1] * h - t2[1] * h,
            base[2] + t1[2] * h - t2[2] * h,
        ],
        [
            base[0] + t1[0] * h + t2[0] * h,
            base[1] + t1[1] * h + t2[1] * h,
            base[2] + t1[2] * h + t2[2] * h,
        ],
        [
            base[0] - t1[0] * h + t2[0] * h,
            base[1] - t1[1] * h + t2[1] * h,
            base[2] - t1[2] * h + t2[2] * h,
        ],
    ]
}




/// Write a built BSP into a struct that holds the blocks directly.
///
/// A `collision_model` wraps them in a `bsp` struct; a structure BSP's
/// `collision bsp` element does not.
pub fn write_collision_bsp(
    st: &mut crate::TagStructMut<'_>,
    bsp: &CollisionBsp,
) -> R<()> {
    write_bsp_fields(st, &bsp.0)
}

fn write_bsp(el: &mut crate::TagStructMut<'_>, bsp: &Bsp) -> R<()> {
    let mut fld = el
        .field_mut("bsp")
        .ok_or_else(|| CollisionError::MissingField("bsp".into()))?;
    let mut st = fld
        .as_struct_mut()
        .ok_or_else(|| CollisionError::MissingField("bsp (not a struct)".into()))?;
    write_bsp_fields(&mut st, bsp)
}

fn write_bsp_fields(st: &mut crate::TagStructMut<'_>, bsp: &Bsp) -> R<()> {

    sub_block(st, "bsp3d nodes", |b| {
        for n in &bsp.nodes3d {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(&mut e, "node data designator", TagFieldData::Int64Integer(pack_node3d(n)));
        }
        Ok(())
    })?;
    sub_block(st, "planes", |b| {
        for p in &bsp.planes {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(&mut e, "plane", TagFieldData::RealPlane3d(*p));
        }
        Ok(())
    })?;
    sub_block(st, "leaves", |b| {
        for l in &bsp.leaves {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(&mut e, "flags", TagFieldData::ByteFlags { value: l.flags, names: Vec::new() });
            try_set(&mut e, "bsp2d reference count", TagFieldData::ShortInteger(l.ref_count as i16));
            try_set(&mut e, "first bsp2d reference", TagFieldData::LongInteger(l.first_ref));
        }
        Ok(())
    })?;
    sub_block(st, "bsp2d references", |b| {
        for r in &bsp.bsp2d_refs {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            // `flipped` is a flag bit, not a complement.
            let plane =
                if r.flipped { flag_ref(r.plane as usize) } else { r.plane as i32 };
            try_set(&mut e, "plane", TagFieldData::ShortInteger(pack_s15(plane)));
            try_set(&mut e, "bsp2d node", TagFieldData::ShortInteger(pack_s15(r.node)));
        }
        Ok(())
    })?;
    sub_block(st, "bsp2d nodes", |b| {
        for n in &bsp.bsp2d_nodes {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(
                &mut e,
                "plane",
                TagFieldData::RealPlane2d(crate::math::RealPlane2d {
                    i: n.plane[0],
                    j: n.plane[1],
                    d: n.plane[2],
                }),
            );
            try_set(&mut e, "left child", TagFieldData::ShortInteger(pack_s15(n.left)));
            try_set(&mut e, "right child", TagFieldData::ShortInteger(pack_s15(n.right)));
        }
        Ok(())
    })?;
    sub_block(st, "surfaces", |b| {
        for s in &bsp.surfaces {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            let plane =
                if s.flipped { flag_ref(s.plane as usize) } else { s.plane as i32 };
            try_set(&mut e, "plane", TagFieldData::ShortInteger(pack_s15(plane)));
            try_set(&mut e, "first edge", TagFieldData::ShortInteger(s.first_edge as i16));
            try_set(&mut e, "material", TagFieldData::ShortInteger(s.material));
            try_set(&mut e, "breakable surface set", TagFieldData::ShortInteger(-1));
            try_set(&mut e, "breakable surface", TagFieldData::ShortInteger(-1));
        }
        Ok(())
    })?;
    sub_block(st, "edges", |b| {
        for ed in &bsp.edges {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(&mut e, "start vertex", TagFieldData::ShortInteger(ed.start as i16));
            try_set(&mut e, "end vertex", TagFieldData::ShortInteger(ed.end as i16));
            try_set(&mut e, "forward edge", TagFieldData::ShortInteger(ed.forward as i16));
            try_set(&mut e, "reverse edge", TagFieldData::ShortInteger(ed.reverse as i16));
            try_set(&mut e, "left surface", TagFieldData::ShortInteger(ed.left as i16));
            try_set(&mut e, "right surface", TagFieldData::ShortInteger(ed.right as i16));
        }
        Ok(())
    })?;
    sub_block(st, "vertices", |b| {
        for (vi, v) in bsp.vertices.iter().enumerate() {
            let i = b.add_element();
            let mut e = b.element_mut(i).expect("just added");
            try_set(&mut e, "point", TagFieldData::RealPoint3d(*v));
            let first = bsp
                .edges
                .iter()
                .position(|ed| ed.start == vi as u32 || ed.end == vi as u32)
                .unwrap_or(0);
            try_set(&mut e, "first edge", TagFieldData::ShortInteger(first as i16));
        }
        Ok(())
    })?;
    Ok(())
}

fn sub_block<T>(
    st: &mut crate::TagStructMut<'_>,
    name: &str,
    f: impl FnOnce(&mut crate::TagBlockMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = st
        .field_mut(name)
        .ok_or_else(|| CollisionError::MissingField(name.into()))?;
    let mut blk = fld
        .as_block_mut()
        .ok_or_else(|| CollisionError::MissingField(format!("{name} (not a block)")))?;
    f(&mut blk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s15_round_trips_including_the_flag_and_the_sentinel() {
        let unpack = |s: i16| -> i32 {
            let u = s as u16;
            if u == 0xFFFF {
                return -1;
            }
            let idx = (u & 0x7FFF) as i32;
            if u & 0x8000 != 0 { (idx as u32 | 0x8000_0000) as i32 } else { idx }
        };
        assert_eq!(unpack(pack_s15(-1)), -1, "the none sentinel");
        for v in [0i32, 1, 100, 0x7FFE] {
            assert_eq!(unpack(pack_s15(v)), v, "plain index {v}");
        }
        // A flagged value: bit 31 set on the way in, bit 15 on the wire.
        for idx in [0u32, 5, 0x7FFE] {
            let flagged = (idx | 0x8000_0000) as i32;
            let packed = pack_s15(flagged);
            assert!(packed as u16 & 0x8000 != 0, "flag must survive");
            assert_eq!(unpack(packed), flagged);
        }
    }

    #[test]
    fn a_bsp3d_node_packs_plane_and_two_children() {
        let n = Node3d { plane: 0x1234, back: 7, front: flag_ref(3) };
        let w = pack_node3d(&n) as u64;
        assert_eq!(w & 0xFFFF, 0x1234, "plane in bits 0..15");
        assert_eq!((w >> 16) & 0xFF_FFFF, 7, "back child in 16..39");
        // A leaf sets bit 23.
        let front = (w >> 40) & 0xFF_FFFF;
        assert_eq!(front & 0x80_0000, 0x80_0000, "leaf flag on bit 23");
        assert_eq!(front & 0x7F_FFFF, 3, "leaf index below it");
    }

    #[test]
    fn the_no_child_sentinel_is_all_ones_and_leaf_zero_is_not() {
        assert_eq!(pack_child24(-1), 0xFF_FFFF, "-1 is the absence sentinel");
        // `flag_ref(0)` is `0x80000000`, which is a real flagged leaf.
        // Testing absence with a sign check instead of `== -1` would
        // silently turn leaf 0 into "no child".
        assert_eq!(pack_child24(flag_ref(0)), 0x80_0000, "leaf 0 is a leaf, not absence");
        assert_eq!(pack_child24(flag_ref(5)), 0x80_0005);
    }

    #[test]
    fn clipping_a_square_by_a_plane_through_it_halves_it() {
        let sq = vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        // The plane x = 1, normal +X.
        let front = clip(&sq, [1.0, 0.0, 0.0], -1.0, true);
        let back = clip(&sq, [1.0, 0.0, 0.0], -1.0, false);
        assert_eq!(front.len(), 4, "half a square is still a quad: {front:?}");
        assert_eq!(back.len(), 4);
        for p in &front {
            assert!(p[0] >= 1.0 - 1e-9, "front half must be x >= 1: {p:?}");
        }
        for p in &back {
            assert!(p[0] <= 1.0 + 1e-9, "back half must be x <= 1: {p:?}");
        }
    }

    #[test]
    fn the_projection_table_discards_the_dominant_axis() {
        for axis in 0..3 {
            for sign in [false, true] {
                let t = PROJECTION[2 * axis + usize::from(sign)];
                assert_eq!(t[2], axis, "the discarded axis is the dominant one");
                let mut seen = [t[0], t[1], t[2]];
                seen.sort_unstable();
                assert_eq!(seen, [0, 1, 2], "a permutation of the three axes");
            }
        }
    }

    #[test]
    fn the_dominant_axis_is_the_largest_component() {
        assert_eq!(dominant_axis([1.0, 0.0, 0.0]), 0);
        assert_eq!(dominant_axis([0.0, -1.0, 0.0]), 1);
        assert_eq!(dominant_axis([0.1, 0.2, -0.9]), 2);
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    /// A closed box as 12 triangles, built directly into a `Bsp` so the
    /// tree can be exercised without going through JMS.
    pub(super) fn box_bsp() -> Bsp {
        let c = [
            [0.0f32, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 2.0, 0.0], [0.0, 2.0, 0.0],
            [0.0, 0.0, 2.0], [2.0, 0.0, 2.0], [2.0, 2.0, 2.0], [0.0, 2.0, 2.0],
        ];
        let quads: [[u32; 4]; 6] = [
            [0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4],
            [2, 3, 7, 6], [1, 2, 6, 5], [3, 0, 4, 7],
        ];
        let mut bsp = Bsp::default();
        for p in c {
            bsp.vertices.push(RealPoint3d { x: p[0], y: p[1], z: p[2] });
        }
        let mut rings: Vec<[u32; 3]> = Vec::new();
        for q in quads {
            rings.push([q[0], q[1], q[2]]);
            rings.push([q[0], q[2], q[3]]);
        }
        let mut keys: Vec<(V3, f64)> = Vec::new();
        for r in &rings {
            let p: Vec<V3> = r.iter().map(|&i| pt(&bsp.vertices[i as usize])).collect();
            let n = cross3(sub3(p[1], p[0]), sub3(p[2], p[0]));
            let l = len3(n);
            let n = [n[0] / l, n[1] / l, n[2] / l];
            let d = -dot3(n, p[0]);
            let mut found = None;
            for (pi, (pn, pd)) in keys.iter().enumerate() {
                if dot3(*pn, n) > 0.9999 && (pd - d).abs() < 1e-5 {
                    found = Some((pi, false));
                    break;
                }
                if dot3(*pn, n) < -0.9999 && (pd + d).abs() < 1e-5 {
                    found = Some((pi, true));
                    break;
                }
            }
            let (plane, flipped) = found.unwrap_or_else(|| {
                keys.push((n, d));
                bsp.planes.push(RealPlane3d {
                    i: n[0] as f32, j: n[1] as f32, k: n[2] as f32, d: -d as f32,
                });
                (keys.len() - 1, false)
            });
            bsp.surfaces.push(Surface {
                ring: r.to_vec(),
                plane: plane as u32,
                flipped,
                material: 0,
                first_edge: 0,
            });
        }
        build_edges(&mut bsp).expect("edges");
        bsp
    }

    #[test]
    fn a_box_builds_a_tree_and_every_surface_is_reachable() {
        let mut bsp = box_bsp();
        assert_eq!(bsp.surfaces.len(), 12);
        assert_eq!(bsp.planes.len(), 6, "a box has six planes");
        build_tree(&mut bsp, MAX_DEPTH).expect("tree");
        assert!(!bsp.nodes3d.is_empty(), "a box must produce a real tree");

        // Every surface must be found from the leaf its own face leads to.
        for si in 0..bsp.surfaces.len() {
            let s = &bsp.surfaces[si];
            let poly: Vec<V3> =
                s.ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect();
            let mut c = [0.0f64; 3];
            for p in &poly {
                for k in 0..3 {
                    c[k] += p[k] / poly.len() as f64;
                }
            }
            let (n, _) = plane_of(&bsp, s.plane);
            let dir = if s.flipped { 1.0 } else { -1.0 };
            let probe = [
                c[0] + n[0] * dir * 1e-4,
                c[1] + n[1] * dir * 1e-4,
                c[2] + n[2] * dir * 1e-4,
            ];
            let leaf = descend(&bsp, probe) as usize;
            assert!(leaf < bsp.leaves.len(), "surface {si} descended out of range");
            let l = &bsp.leaves[leaf];
            let mut found = false;
            for r in 0..l.ref_count as usize {
                let rf = bsp.bsp2d_refs[l.first_ref as usize + r];
                let mut list = Vec::new();
                collect_surfaces(&bsp, rf.node, &mut list);
                if list.contains(&(si as u32)) {
                    found = true;
                    break;
                }
            }
            assert!(found, "surface {si} is not referenced by leaf {leaf} it descends to");
        }
    }

    fn collect_surfaces(bsp: &Bsp, node: i32, out: &mut Vec<u32>) {
        if node < 0 {
            out.push(ref_index(node) as u32);
            return;
        }
        if node as usize >= bsp.bsp2d_nodes.len() {
            return;
        }
        let n = bsp.bsp2d_nodes[node as usize];
        collect_surfaces(bsp, n.left, out);
        collect_surfaces(bsp, n.right, out);
    }
}

#[cfg(test)]
mod pack_tests {
    use super::*;

    fn unpack_child(v: u32) -> i32 {
        if v == 0xFF_FFFF {
            return -1;
        }
        if v & 0x80_0000 != 0 {
            return ((v & 0x7F_FFFF) | 0x8000_0000) as i32;
        }
        v as i32
    }

    /// Pack every node of a real tree and read it back. A descent over
    /// the unpacked tree must reach the same leaf as one over the
    /// in-memory tree, for every surface — that is the property the
    /// corpus test relies on.
    #[test]
    fn the_packed_tree_descends_identically_to_the_built_one() {
        let mut bsp = super::build_tests::box_bsp();
        build_tree(&mut bsp, MAX_DEPTH).expect("tree");

        let packed: Vec<(i32, i32, i32)> = bsp
            .nodes3d
            .iter()
            .map(|n| {
                let w = pack_node3d(n) as u64;
                (
                    (w & 0xFFFF) as i32,
                    unpack_child(((w >> 16) & 0xFF_FFFF) as u32),
                    unpack_child(((w >> 40) & 0xFF_FFFF) as u32),
                )
            })
            .collect();

        for (i, n) in bsp.nodes3d.iter().enumerate() {
            assert_eq!(packed[i].0, n.plane as i32, "node {i} plane");
            assert_eq!(packed[i].1, n.back, "node {i} back child");
            assert_eq!(packed[i].2, n.front, "node {i} front child");
        }

        // And the descents agree.
        for si in 0..bsp.surfaces.len() {
            let s = &bsp.surfaces[si];
            let poly: Vec<V3> = s.ring.iter().map(|&v| pt(&bsp.vertices[v as usize])).collect();
            let mut c = [0.0f64; 3];
            for p in &poly {
                for k in 0..3 {
                    c[k] += p[k] / poly.len() as f64;
                }
            }
            let (n, _) = plane_of(&bsp, s.plane);
            let dir = if s.flipped { 1.0 } else { -1.0 };
            let probe =
                [c[0] + n[0] * dir * 1e-4, c[1] + n[1] * dir * 1e-4, c[2] + n[2] * dir * 1e-4];

            let want = descend(&bsp, probe);
            // The same walk, over the packed form and in f32, exactly as
            // the runtime and the corpus test do it.
            let p32 = [probe[0] as f32, probe[1] as f32, probe[2] as f32];
            let mut cur = 0i32;
            let mut got = -1i32;
            for _ in 0..256 {
                if cur < 0 {
                    got = ref_index(cur) as i32;
                    break;
                }
                let (pl, b, f) = packed[cur as usize];
                let pd = &bsp.planes[pl as usize];
                let sdist = pd.i * p32[0] + pd.j * p32[1] + pd.k * p32[2] - pd.d;
                cur = if sdist >= 0.0 { f } else { b };
            }
            assert_eq!(got, want, "surface {si}: packed descent went elsewhere");
        }
    }
}

#[cfg(test)]
mod real_geometry_tests {
    use super::*;

    fn h3ek() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("BLAM_TEST_H3EK") {
            let p = std::path::PathBuf::from(p);
            return p.is_dir().then_some(p);
        }
        ["D:/SteamLibrary/steamapps/common", "C:/Program Files (x86)/Steam/steamapps/common"]
            .iter()
            .map(|r| std::path::PathBuf::from(r).join("H3EK"))
            .find(|p| p.join("data").is_dir())
    }

    /// Find a collision JMS under the kit's `data` tree.
    fn a_collision_jms(kit: &std::path::Path) -> Option<std::path::PathBuf> {
        let mut stack = vec![kit.join("data")];
        while let Some(dir) = stack.pop() {
            let rd = std::fs::read_dir(&dir).ok()?;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.parent().is_some_and(|d| d.ends_with("collision"))
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.to_ascii_lowercase().ends_with(".jms"))
                {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Weld a JMS the way the importer does, then group its triangles
    /// into BSPs on (region, permutation, node).
    ///
    /// The shipped guardian confirms that key: its two permutations hold
    /// 38 BSPs and 1, and it has 38 nodes.
    fn welded_groups(
        jms: &JmsFile,
        opts: &CollisionOptions,
    ) -> (crate::weld::Welded, BTreeMap<(String, String, i16), Vec<usize>>) {
        let s = opts.scale;
        let source: Vec<WeldVertex> = jms
            .vertices
            .iter()
            .map(|v| WeldVertex {
                position: RealPoint3d {
                    x: v.position.x * s,
                    y: v.position.y * s,
                    z: v.position.z * s,
                },
                normal: v.normal,
                texcoords: Vec::new(),
                influences: v.node_sets.clone(),
                color: None,
            })
            .collect();
        let welded = weld(&source, &opts.weld);

        let labels: Vec<MaterialLabel> =
            jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
        let mut groups: BTreeMap<(String, String, i16), Vec<usize>> = BTreeMap::new();
        for (i, tri) in jms.triangles.iter().enumerate() {
            let Some(label) = labels.get(usize::try_from(tri.material).unwrap_or(usize::MAX))
            else {
                continue;
            };
            let node = jms
                .vertices
                .get(tri.v[0] as usize)
                .and_then(|v| v.node_sets.first().map(|(n, _)| *n))
                .unwrap_or(0);
            groups
                .entry((label.region.clone(), label.permutation.clone(), node))
                .or_default()
                .push(i);
        }
        (welded, groups)
    }

    /// The same reachability property as the box test, but on a real
    /// shipped collision JMS and against the writer's own structures.
    ///
    /// This separates "the build is wrong on real geometry" from "the
    /// packing or the decoder disagrees" — the box passes either way.
    ///
    /// `build_tree` is the assertion: it places every surface in a leaf
    /// and returns [`CollisionError::Unreachable`] for any it cannot,
    /// so a plain `Ok` here means no collision went missing.
    #[test]
    fn a_real_collision_jms_builds_a_reachable_tree() {
        let Some(kit) = h3ek() else {
            eprintln!("skipping: no H3EK install");
            return;
        };
        let Some(path) = a_collision_jms(&kit) else {
            eprintln!("skipping: no collision JMS");
            return;
        };
        let text = std::fs::read_to_string(&path).expect("read");
        let (jms, _) = crate::jms::JmsFile::parse(&text).expect("parse");

        let opts = CollisionOptions::default();
        let (welded, groups) = welded_groups(&jms, &opts);
        assert!(!groups.is_empty(), "{}: no BSP groups", path.display());

        let (mut surfaces, mut dropped) = (0usize, 0usize);
        for (key, tris) in &groups {
            let mut bsp = build_geometry(&jms, &welded, tris, key.2)
                .unwrap_or_else(|e| panic!("{}: geometry: {e}", path.display()));
            surfaces += bsp.surfaces.len();
            build_tree(&mut bsp, MAX_DEPTH)
                .unwrap_or_else(|e| panic!("{}: tree: {e}", path.display()));
            dropped += bsp.dropped.len();
            assert!(
                depth_of(&bsp, 0) <= MAX_DEPTH,
                "{}: tree is {} deep",
                path.display(),
                depth_of(&bsp, 0)
            );
        }

        assert!(surfaces > 0, "{}: built no surfaces", path.display());
        // Coplanar faces that no 2D line separates cannot all be kept, so
        // some loss is inherent; a large share of it is not.
        assert!(
            dropped * 100 <= surfaces,
            "{}: dropped {dropped} of {surfaces} surfaces",
            path.display()
        );
    }

    /// Diagnostic: how deep does each of guardian's BSPs really go?
    ///
    /// The depth limit turns a runaway tree into `MAX_DEPTH + 1` whatever
    /// it really was, which makes every change to the chooser look like
    /// it did nothing. This lifts the cap and prints the real numbers per
    /// group. Tool's own guardian is 65 deep over 1960 planes and 2193
    /// surfaces, and no shipped collision_model passes 128.
    #[test]
    #[ignore = "diagnostic; needs an H3EK install"]
    fn how_deep_is_each_guardian_bsp() {
        let Some(kit) = h3ek() else {
            eprintln!("skipping: no H3EK install");
            return;
        };
        let path = kit
            .join("data/objects/characters/guardian/collision/Guardian2_collision.JMS");
        let Ok(text) = std::fs::read_to_string(&path) else {
            eprintln!("skipping: no guardian JMS");
            return;
        };
        let (jms, _) = crate::jms::JmsFile::parse(&text).expect("parse");

        let opts = CollisionOptions::default();
        let (welded, groups) = welded_groups(&jms, &opts);
        println!("{} triangles in {} groups", jms.triangles.len(), groups.len());

        let mut rows: Vec<(usize, String)> = Vec::new();
        for (key, tris) in &groups {
            let mut bsp = build_geometry(&jms, &welded, tris, key.2).expect("geometry");
            let (surfaces, planes) = (bsp.surfaces.len(), bsp.planes.len());
            build_tree(&mut bsp, 100_000).expect("tree");
            let d = depth_of(&bsp, 0);
            rows.push((
                d,
                format!(
                    "{:<24} node {:>3}  tris {:>5}  surfaces {:>5}  planes {:>5}                       nodes {:>6}  depth {d}",
                    format!("{}/{}", key.0, key.1),
                    key.2,
                    tris.len(),
                    surfaces,
                    planes,
                    bsp.nodes3d.len(),
                ),
            ));
        }
        rows.sort_unstable_by(|a, b| b.0.cmp(&a.0));
        println!("deepest groups:");
        for (_, line) in rows.iter().take(8) {
            println!("  {line}");
        }
    }
}
