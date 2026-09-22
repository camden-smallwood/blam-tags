//! 3D convex hull — the vertices and face planes a `physics_model`
//! polyhedron needs.
//!
//! # Why this is here
//!
//! `tool.exe` hands a JMS `CONVEX SHAPES` vertex list to Havok, which
//! builds the hull and stores **both** the hull vertices (as SoA
//! four-vectors) and its **face plane equations**. The planes are what
//! the runtime collides against, so an importer that writes the tag
//! itself has to compute them. Havok is not callable from outside
//! `tool.exe` — it has no export table at all — so the hull has to be
//! rebuilt.
//!
//! # Sizing
//!
//! Measured over the 254 shipped H3EK JMS files, 1,007 convex shapes:
//! median **12** input vertices, p99 **128**, max **3,708**. 317 shapes
//! are exactly 8 — artists boxing something. The long tail is an artist
//! handing Havok a whole mesh and letting it reduce, so the algorithm
//! has to be output-sensitive rather than brute force: an `O(n⁴)`
//! all-triples test is fine at 12 points and hopeless at 3,708.
//!
//! This is the standard incremental (quickhull-family) construction:
//! seed a tetrahedron, then for each remaining point delete the faces it
//! can see and rebuild the cone from the horizon. Cost is `O(n·F)`.
//!
//! # Plane convention
//!
//! Halo stores planes so that **`dot(n, v) + d <= 0` for every point of
//! the shape**, with unit normals — verified against shipped tags to a
//! maximum residual of `+5e-8`. So normals point *outward* and
//! `d = -dot(n, p)` for any `p` on the face. Getting the sign backwards
//! produces a shape that collides with its own inside-out complement,
//! which is not something you notice until something falls through it.
//!
//! # Coplanar faces are merged
//!
//! A cube's hull is 12 triangles but only **6** planes, and the tag
//! stores planes, not triangles. Faces sharing a plane are therefore
//! collapsed before output. Without that a box would emit 12 plane
//! equations, doubling its share of the 4,096 plane-equation budget —
//! which shipped Forge terrain already fills to 83%.

use std::collections::HashMap;

use crate::math::{RealPlane3d, RealPoint3d};

/// Tolerances for [`convex_hull_with`].
///
/// All three are **relative to the point set's largest extent**, so a
/// 1-unit crate and a 40-unit ship behave the same.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HullOptions {
    /// A point this far outside a face is treated as outside, and
    /// becomes a hull vertex. Larger values drop more near-surface
    /// points, giving a simpler hull.
    pub point_epsilon: f64,
    /// Two faces whose normals agree to better than this are the same
    /// plane. Expressed as `1 - dot(n1, n2)`, so smaller is stricter.
    pub coplanar_angle: f64,
    /// …and whose plane offsets agree to within this fraction of the
    /// extent.
    pub coplanar_offset: f64,
}

/// Fitted against 89 shipped `physics_model` tags with a matched source
/// JMS, comparing four-vector and plane counts to Havok's own.
///
/// | `point_epsilon` | `coplanar_angle` | vec hit | plane hit | vec err | plane err |
/// |---|---|---|---|---|---|
/// | 1e-9 | any | 38/89 | 25/89 | 0.1182 | 0.1050 |
/// | 1e-7 | 1e-6..1e-3 | 41/89 | 25/89 | 0.1122 | 0.1047 |
/// | 1e-6 | 1e-6..1e-3 | 41/89 | 25/89 | 0.1122 | 0.1048 |
/// | **1e-5** | **1e-6..1e-3** | **41/89** | **25/89** | **0.1122** | **0.1048** |
/// | 1e-4 | any | 40/89 | 26/89 | 0.1205 | 0.1171 |
/// | 1e-3 | any | 41/89 | 26/89 | 0.2115 | 0.2366 |
///
/// Both knobs are flat over a wide range and the defaults sit inside it —
/// the nominal winner is better by one part in ten thousand of plane
/// error, which is noise. 1e-5 is also the safer end of the plateau: the
/// same tolerance decides which near-coincident points are merged before
/// the hull is built, and `heretic_banshee` needs points a ten-thousandth
/// apart merged or the surface stops closing.
///
/// `coplanar_offset` is **not** covered — the sweep held it at 1e-5. It
/// is still a guess.
///
/// Reproduce with `cargo test -p blam-tags --release --test hull_corpus
/// fit_hull_tolerances -- --ignored --nocapture`.
impl Default for HullOptions {
    fn default() -> Self {
        // Fitted against the shipped corpus rather than guessed: these
        // are the values that best reproduce Havok's own vertex and
        // plane counts over the 49 shipped `physics_model` tags whose
        // `info` stream carries exactly one source JMS. See
        // `tests/hull_corpus.rs`. A stricter hull is not "more correct"
        // here — it is more planes against a 4,096 budget that shipped
        // Forge terrain already fills to 83%.
        Self { point_epsilon: 1e-5, coplanar_angle: 1e-4, coplanar_offset: 1e-5 }
    }
}

/// A convex hull: its extreme points and its unique face planes.
#[derive(Debug, Clone, PartialEq)]
pub struct Hull {
    /// Hull vertices — the subset of the input that survives. Interior
    /// points are dropped, as Havok drops them.
    pub vertices: Vec<RealPoint3d>,
    /// Unique outward face planes, `dot(n,v) + d <= 0` inside.
    pub planes: Vec<RealPlane3d>,
}

/// Why a hull could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HullError {
    /// Fewer than four distinct points — no volume is possible.
    TooFewPoints(usize),
    /// The points are all on one line.
    Collinear,
    /// The points are all on one plane. A zero-thickness "hull" has no
    /// inside, so this is refused rather than returned as a degenerate
    /// shape that would collide with nothing.
    Coplanar,
    /// The surface stopped being a closed manifold and the face count ran
    /// away. Refused rather than left to exhaust memory.
    Degenerate { points: usize, faces: usize },
}

impl std::fmt::Display for HullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooFewPoints(n) => {
                write!(f, "a convex hull needs at least 4 distinct points, got {n}")
            }
            Self::Collinear => write!(f, "all points lie on one line — no hull"),
            Self::Coplanar => write!(f, "all points lie on one plane — the hull has no volume"),
            Self::Degenerate { points, faces } => write!(
                f,
                "the hull stopped being a closed surface: {faces} faces from {points} points. \
                 The shape is probably slivered or has near-coincident points that survived \
                 merging."
            ),
        }
    }
}

impl std::error::Error for HullError {}

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn norm(a: V3) -> f64 {
    dot(a, a).sqrt()
}

/// One triangular face of the working hull.
#[derive(Clone, Copy)]
struct Face {
    v: [usize; 3],
    /// Outward unit normal.
    n: V3,
    /// `dot(n, p) + d == 0` on the face.
    d: f64,
    /// Cleared when the face is deleted rather than compacting the list.
    live: bool,
}

impl Face {
    fn new(pts: &[V3], a: usize, b: usize, c: usize) -> Option<Self> {
        let n = cross(sub(pts[b], pts[a]), sub(pts[c], pts[a]));
        let len = norm(n);
        if len <= 0.0 {
            return None;
        }
        let n = [n[0] / len, n[1] / len, n[2] / len];
        Some(Self { v: [a, b, c], n, d: -dot(n, pts[a]), live: true })
    }

    fn distance(&self, p: V3) -> f64 {
        dot(self.n, p) + self.d
    }
}

/// Build the convex hull of `points`.
///
/// Interior and duplicate points are dropped. Returned vertices keep the
/// order of their first appearance in the input, which makes the output
/// stable for a given input and therefore diffable.
pub fn convex_hull(points: &[RealPoint3d]) -> Result<Hull, HullError> {
    convex_hull_with(points, &HullOptions::default())
}

/// [`convex_hull`] with explicit tolerances.
pub fn convex_hull_with(points: &[RealPoint3d], opts: &HullOptions) -> Result<Hull, HullError> {
    let (faces, pts, keep, extent) = build_faces(points, opts)?;

    let vertices: Vec<RealPoint3d> = keep
        .iter()
        .map(|i| RealPoint3d { x: pts[*i][0] as f32, y: pts[*i][1] as f32, z: pts[*i][2] as f32 })
        .collect();

    // --- merge coplanar faces into unique planes --------------------------
    // A cube is 12 triangles and 6 planes; the tag stores planes.
    let angular = opts.coplanar_angle;
    let offset = extent * opts.coplanar_offset;
    let mut planes: Vec<(V3, f64)> = Vec::new();
    for f in &faces {
        if let Some(existing) = planes
            .iter_mut()
            .find(|(n, d)| dot(*n, f.n) > 1.0 - angular && (*d - f.d).abs() <= offset)
        {
            // Average, so a face split across many triangles does not
            // inherit one triangle's rounding.
            let w = 0.5;
            existing.0 = [
                existing.0[0] * (1.0 - w) + f.n[0] * w,
                existing.0[1] * (1.0 - w) + f.n[1] * w,
                existing.0[2] * (1.0 - w) + f.n[2] * w,
            ];
            let l = norm(existing.0);
            existing.0 = [existing.0[0] / l, existing.0[1] / l, existing.0[2] / l];
            existing.1 = existing.1 * (1.0 - w) + f.d * w;
        } else {
            planes.push((f.n, f.d));
        }
    }

    // Tighten every plane so no vertex is outside it. Averaging can push
    // a plane a few ULPs inward; the runtime convention is
    // `dot(n,v)+d <= 0` for *every* vertex, so make that literally true.
    let planes: Vec<RealPlane3d> = planes
        .into_iter()
        .map(|(n, mut d)| {
            let worst = keep.iter().map(|i| dot(n, pts[*i])).fold(f64::MIN, f64::max);
            if -worst < d {
                d = -worst;
            }
            RealPlane3d { i: n[0] as f32, j: n[1] as f32, k: n[2] as f32, d: d as f32 }
        })
        .collect();

    Ok(Hull { vertices, planes })
}

/// The triangulated hull, before coplanar faces are merged into planes.
///
/// Split out from [`convex_hull`] so the topological invariants — closure,
/// and Euler's `F = 2V - 4` — can be asserted on the triangles themselves.
/// Checking them on the *merged* plane count instead would be wrong: two
/// adjacent triangles that happen to be coplanar legitimately collapse to
/// one plane, which is a property of the input, not a defect.
///
/// Returns the live faces, the deduplicated points they index, the hull
/// vertex indices in input order, and the point set's extent.
#[allow(clippy::type_complexity)]
fn build_faces(
    points: &[RealPoint3d],
    opts: &HullOptions,
) -> Result<(Vec<Face>, Vec<V3>, Vec<usize>, f64), HullError> {
    // Deduplicate exactly first. Near-duplicates are handled later by
    // the epsilon; exact ones would make the seed search degenerate.
    let mut pts: Vec<V3> = Vec::with_capacity(points.len());
    let mut origin: Vec<usize> = Vec::with_capacity(points.len());
    for (i, p) in points.iter().enumerate() {
        let v: V3 = [p.x as f64, p.y as f64, p.z as f64];
        if !v.iter().all(|c| c.is_finite()) {
            continue;
        }
        if !pts.contains(&v) {
            pts.push(v);
            origin.push(i);
        }
    }
    if pts.len() < 4 {
        return Err(HullError::TooFewPoints(pts.len()));
    }

    // Tolerance scaled to the point set, so a 1-unit crate and a
    // 40-unit ship get comparable treatment.
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for p in &pts {
        for a in 0..3 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    let extent = (0..3).fold(0.0f64, |m, a| m.max(hi[a] - lo[a]));
    if extent <= 0.0 {
        return Err(HullError::TooFewPoints(1));
    }
    let eps = extent * opts.point_epsilon;

    // Merge points closer together than the tolerance. Exact dedup above
    // is not enough: points a fraction apart seed sliver faces, the
    // surface stops being a closed manifold, and the incremental step
    // then has no single horizon to work with.
    //
    // On a grid of cell size `eps`, checking the 27 neighbouring cells,
    // so it stays linear and does not depend on the order points arrive
    // in. `heretic_banshee` drops from 1,071 points to a workable set
    // this way.
    {
        use std::collections::HashMap;
        let cell = |v: &V3| -> [i64; 3] {
            [
                (v[0] / eps).floor() as i64,
                (v[1] / eps).floor() as i64,
                (v[2] / eps).floor() as i64,
            ]
        };
        let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::new();
        let mut merged: Vec<V3> = Vec::with_capacity(pts.len());
        let mut merged_origin: Vec<usize> = Vec::with_capacity(pts.len());
        for (i, v) in pts.iter().enumerate() {
            let c = cell(v);
            let mut dup = false;
            'search: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let key = [c[0] + dx, c[1] + dy, c[2] + dz];
                        for &j in grid.get(&key).map(Vec::as_slice).unwrap_or(&[]) {
                            if norm(sub(*v, merged[j])) <= eps {
                                dup = true;
                                break 'search;
                            }
                        }
                    }
                }
            }
            if dup {
                continue;
            }
            grid.entry(c).or_default().push(merged.len());
            merged.push(*v);
            merged_origin.push(origin[i]);
        }
        pts = merged;
        origin = merged_origin;
        if pts.len() < 4 {
            return Err(HullError::TooFewPoints(pts.len()));
        }
    }

    // --- seed tetrahedron -------------------------------------------------
    // Two points furthest apart along the widest axis, then the point
    // furthest from that line, then the point furthest from that plane.
    let widest = (0..3).max_by(|a, b| (hi[*a] - lo[*a]).total_cmp(&(hi[*b] - lo[*b]))).unwrap();
    let i0 = (0..pts.len()).min_by(|a, b| pts[*a][widest].total_cmp(&pts[*b][widest])).unwrap();
    let i1 = (0..pts.len()).max_by(|a, b| pts[*a][widest].total_cmp(&pts[*b][widest])).unwrap();
    if i0 == i1 {
        return Err(HullError::Collinear);
    }

    let line = sub(pts[i1], pts[i0]);
    let line_len = norm(line);
    let i2 = (0..pts.len())
        .max_by(|a, b| {
            let da = norm(cross(sub(pts[*a], pts[i0]), line)) / line_len;
            let db = norm(cross(sub(pts[*b], pts[i0]), line)) / line_len;
            da.total_cmp(&db)
        })
        .unwrap();
    if norm(cross(sub(pts[i2], pts[i0]), line)) / line_len <= eps {
        return Err(HullError::Collinear);
    }

    let seed_n = {
        let n = cross(sub(pts[i1], pts[i0]), sub(pts[i2], pts[i0]));
        let l = norm(n);
        [n[0] / l, n[1] / l, n[2] / l]
    };
    let seed_d = -dot(seed_n, pts[i0]);
    let i3 = (0..pts.len())
        .max_by(|a, b| {
            (dot(seed_n, pts[*a]) + seed_d)
                .abs()
                .total_cmp(&(dot(seed_n, pts[*b]) + seed_d).abs())
        })
        .unwrap();
    if (dot(seed_n, pts[i3]) + seed_d).abs() <= eps {
        return Err(HullError::Coplanar);
    }

    // Orient every seed face away from the tetrahedron's centroid.
    let centroid: V3 = {
        let s = [i0, i1, i2, i3];
        let mut c = [0.0; 3];
        for i in s {
            for a in 0..3 {
                c[a] += pts[i][a] / 4.0;
            }
        }
        c
    };
    let mut faces: Vec<Face> = Vec::new();
    for (a, b, c) in [(i0, i1, i2), (i0, i1, i3), (i0, i2, i3), (i1, i2, i3)] {
        let Some(mut f) = Face::new(&pts, a, b, c) else { continue };
        if f.distance(centroid) > 0.0 {
            // Flip so the normal points outward.
            f = Face::new(&pts, a, c, b).expect("flip of a valid face is valid");
        }
        faces.push(f);
    }
    if faces.len() != 4 {
        return Err(HullError::Coplanar);
    }

    // --- incremental insertion -------------------------------------------
    // Insert the remaining points furthest-first: each one deletes more
    // faces, so the working hull stays small and later points are more
    // often trivially inside.
    let seeds = [i0, i1, i2, i3];
    let mut order: Vec<usize> = (0..pts.len()).filter(|i| !seeds.contains(i)).collect();
    order.sort_by(|a, b| {
        let da = norm(sub(pts[*a], centroid));
        let db = norm(sub(pts[*b], centroid));
        db.total_cmp(&da)
    });

    let mut owner: HashMap<(usize, usize), usize> = HashMap::new();
    let mut patch: Vec<usize> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut horizon: Vec<(usize, usize)> = Vec::new();
    for p in order {
        let point = pts[p];

        // The face this point is furthest outside of. Seeding from the
        // furthest rather than the first keeps the choice away from the
        // rounding that makes near-coplanar faces flip.
        let mut seed = None;
        let mut furthest = eps;
        for (fi, f) in faces.iter().enumerate() {
            let d = f.distance(point);
            if d > furthest {
                furthest = d;
                seed = Some(fi);
            }
        }
        let Some(seed) = seed else {
            continue; // inside the hull already
        };

        // Which face traverses each directed edge. A pair of faces
        // sharing an edge holds it in opposite directions, so this is
        // the surface's adjacency.
        owner.clear();
        for (fi, f) in faces.iter().enumerate() {
            owner.insert((f.v[0], f.v[1]), fi);
            owner.insert((f.v[1], f.v[2]), fi);
            owner.insert((f.v[2], f.v[0]), fi);
        }

        // Grow the visible patch across shared edges. Connected by
        // construction, so its boundary is one loop and the fan below
        // closes the surface again.
        let mut inside = vec![false; faces.len()];
        patch.clear();
        stack.clear();
        stack.push(seed);
        inside[seed] = true;
        while let Some(fi) = stack.pop() {
            patch.push(fi);
            let v = faces[fi].v;
            for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
                if let Some(&nb) = owner.get(&(b, a)) {
                    if !inside[nb] && faces[nb].distance(point) > eps {
                        inside[nb] = true;
                        stack.push(nb);
                    }
                }
            }
        }

        // The horizon is an edge of the patch whose other side is not in
        // it — including an edge with no other side at all, which is a
        // hole the surface already had.
        patch.sort_unstable();
        horizon.clear();
        for &fi in &patch {
            let v = faces[fi].v;
            for (a, b) in [(v[0], v[1]), (v[1], v[2]), (v[2], v[0])] {
                let open = owner.get(&(b, a)).is_none_or(|&nb| !inside[nb]);
                if open {
                    horizon.push((a, b));
                }
            }
        }

        for &fi in &patch {
            faces[fi].live = false;
        }
        // Drop them now rather than at the end: a dead face left in place
        // is one the next point's scan still walks, and that list only
        // ever grows.
        faces.retain(|f| f.live);

        for &(a, b) in &horizon {
            // (a, b, p) inherits the deleted face's winding, so the new
            // face is already outward-facing.
            if let Some(f) = Face::new(&pts, a, b, p) {
                faces.push(f);
            }
        }

        // A closed surface over n points has at most 2n-4 faces. Well
        // past that means something above has stopped holding, so stop
        // with a reason rather than growing the list until an allocation
        // fails.
        if faces.len() > 8 * pts.len() + 64 {
            return Err(HullError::Degenerate { points: pts.len(), faces: faces.len() });
        }
    }

    if faces.is_empty() {
        return Err(HullError::Coplanar);
    }

    // --- collect vertices, in first-appearance order ----------------------
    let mut used = vec![false; pts.len()];
    for f in &faces {
        for i in f.v {
            used[i] = true;
        }
    }
    let mut keep: Vec<usize> = (0..pts.len()).filter(|i| used[*i]).collect();
    keep.sort_by_key(|i| origin[*i]);

    Ok((faces, pts, keep, extent))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32, z: f32) -> RealPoint3d {
        RealPoint3d { x, y, z }
    }

    fn cube() -> Vec<RealPoint3d> {
        let mut v = Vec::new();
        for x in [0.0, 1.0] {
            for y in [0.0, 1.0] {
                for z in [0.0, 1.0] {
                    v.push(p(x, y, z));
                }
            }
        }
        v
    }

    /// Every vertex must satisfy the engine's own convention.
    fn assert_convention(h: &Hull) {
        for v in &h.vertices {
            for pl in &h.planes {
                let s = pl.i * v.x + pl.j * v.y + pl.k * v.z + pl.d;
                assert!(s <= 1e-5, "vertex {v:?} is outside plane {pl:?} by {s}");
            }
        }
        for pl in &h.planes {
            let len = (pl.i * pl.i + pl.j * pl.j + pl.k * pl.k).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "plane normal not unit: {len}");
        }
    }

    #[test]
    fn a_cube_has_eight_vertices_and_six_planes() {
        let h = convex_hull(&cube()).unwrap();
        assert_eq!(h.vertices.len(), 8);
        assert_eq!(h.planes.len(), 6, "coplanar triangles must merge: {:?}", h.planes);
        assert_convention(&h);
    }

    #[test]
    fn interior_points_are_dropped() {
        let mut v = cube();
        v.push(p(0.5, 0.5, 0.5));
        v.push(p(0.25, 0.5, 0.75));
        let h = convex_hull(&v).unwrap();
        assert_eq!(h.vertices.len(), 8);
        assert_eq!(h.planes.len(), 6);
    }

    #[test]
    fn duplicate_points_do_not_break_the_seed() {
        let mut v = cube();
        v.extend(cube());
        let h = convex_hull(&v).unwrap();
        assert_eq!(h.vertices.len(), 8);
        assert_eq!(h.planes.len(), 6);
    }

    #[test]
    fn a_tetrahedron_has_four_planes() {
        let v = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(0.0, 1.0, 0.0), p(0.0, 0.0, 1.0)];
        let h = convex_hull(&v).unwrap();
        assert_eq!(h.vertices.len(), 4);
        assert_eq!(h.planes.len(), 4);
        assert_convention(&h);
    }

    #[test]
    fn normals_point_outward_not_inward() {
        let h = convex_hull(&cube()).unwrap();
        // The centre of the cube is strictly inside every plane.
        let c = p(0.5, 0.5, 0.5);
        for pl in &h.planes {
            let s = pl.i * c.x + pl.j * c.y + pl.k * c.z + pl.d;
            assert!(s < -0.4, "centre should be well inside, got {s} for {pl:?}");
        }
    }

    #[test]
    fn degenerate_inputs_are_refused_not_fudged() {
        assert_eq!(convex_hull(&[]).unwrap_err(), HullError::TooFewPoints(0));
        assert_eq!(
            convex_hull(&[p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(2.0, 0.0, 0.0)]).unwrap_err(),
            HullError::TooFewPoints(3)
        );
        // Four collinear points: enough points, no area.
        let line = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(2.0, 0.0, 0.0), p(3.0, 0.0, 0.0)];
        assert_eq!(convex_hull(&line).unwrap_err(), HullError::Collinear);
        // A flat square: enough points, no volume.
        let flat = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(1.0, 1.0, 0.0), p(0.0, 1.0, 0.0)];
        assert_eq!(convex_hull(&flat).unwrap_err(), HullError::Coplanar);
    }

    fn fibonacci_sphere(n: usize) -> Vec<RealPoint3d> {
        let ga = std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
        (0..n)
            .map(|i| {
                let y = 1.0 - (i as f32 / (n - 1) as f32) * 2.0;
                let r = (1.0 - y * y).max(0.0).sqrt();
                let t = ga * i as f32;
                p(t.cos() * r, y, t.sin() * r)
            })
            .collect()
    }

    /// The topological invariant, asserted on the triangles rather than
    /// the merged planes.
    ///
    /// Two adjacent triangles that happen to be coplanar legitimately
    /// collapse into one plane, so plane count is *not* `2V - 4` in
    /// general — a Fibonacci sphere produces sliver triangles at its
    /// poles whose normals agree to well under a degree, and six of them
    /// merge. That is correct behaviour and an earlier version of this
    /// test wrongly flagged it. Closure and Euler hold on the triangles
    /// either way.
    #[test]
    fn the_triangulated_hull_is_closed_and_obeys_eulers_formula() {
        for n in [50usize, 200, 500] {
            let v = fibonacci_sphere(n);
            let (faces, _, keep, _) =
                build_faces(&v, &HullOptions::default()).unwrap();
            assert_eq!(keep.len(), n, "every point of a sphere is extreme (n={n})");
            assert_eq!(faces.len(), 2 * n - 4, "Euler: F = 2V - 4 (n={n})");

            // Every directed edge must have exactly one reverse partner.
            // A hull missing a face, or with a face wound the wrong way,
            // fails here and nowhere else.
            let mut edges: Vec<(usize, usize)> = Vec::new();
            for f in &faces {
                edges.push((f.v[0], f.v[1]));
                edges.push((f.v[1], f.v[2]));
                edges.push((f.v[2], f.v[0]));
            }
            for &(a, b) in &edges {
                let fwd = edges.iter().filter(|e| **e == (a, b)).count();
                let rev = edges.iter().filter(|e| **e == (b, a)).count();
                assert_eq!(fwd, 1, "edge ({a},{b}) appears {fwd} times (n={n})");
                assert_eq!(rev, 1, "edge ({a},{b}) has {rev} reverses (n={n})");
            }
        }
    }

    #[test]
    fn a_sphere_keeps_every_point() {
        let n = 200usize;
        let v = fibonacci_sphere(n);
        // Pinned to explicit tight tolerances: the *default* is fitted
        // against Havok and may move, but this test is about the
        // geometry, not the fit. At a strict merge threshold a sphere's
        // faces are all distinct planes.
        let tight = HullOptions {
            point_epsilon: 1e-9,
            coplanar_angle: 1e-9,
            coplanar_offset: 1e-9,
        };
        let h = convex_hull_with(&v, &tight).unwrap();
        assert_eq!(h.vertices.len(), n, "every point of a sphere is extreme");
        assert_eq!(h.planes.len(), 2 * n - 4, "no two faces of a sphere are coplanar");
        assert_convention(&h);
    }

    #[test]
    fn output_is_deterministic() {
        let v = cube();
        let a = convex_hull(&v).unwrap();
        let b = convex_hull(&v).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn vertices_keep_input_order() {
        let v = cube();
        let h = convex_hull(&v).unwrap();
        assert_eq!(h.vertices, v, "all eight are extreme, so order is unchanged");
    }

    #[test]
    fn a_large_hull_completes_and_stays_convex() {
        // The corpus tail is 3,708 input points. Use a noisy sphere so
        // most points are interior — the case that would blow up a
        // brute-force implementation.
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as f32 / (1u32 << 31) as f32) - 0.5
        };
        let v: Vec<RealPoint3d> = (0..4000).map(|_| p(rnd(), rnd(), rnd())).collect();
        let h = convex_hull(&v).unwrap();
        assert!(h.vertices.len() >= 8, "got {}", h.vertices.len());
        assert!(h.vertices.len() < 4000, "most random points are interior");
        assert_convention(&h);
        // And every input point must be inside the hull, not just the
        // vertices — that is what "hull" means.
        for q in &v {
            for pl in &h.planes {
                let s = pl.i * q.x + pl.j * q.y + pl.k * q.z + pl.d;
                assert!(s <= 1e-4, "input point {q:?} outside hull by {s}");
            }
        }
    }

    #[test]
    fn scale_does_not_change_the_answer() {
        // A 1-unit crate and a 40-unit ship must behave the same.
        for s in [0.001f32, 1.0, 1000.0] {
            let v: Vec<RealPoint3d> = cube().iter().map(|q| p(q.x * s, q.y * s, q.z * s)).collect();
            let h = convex_hull(&v).unwrap_or_else(|e| panic!("scale {s}: {e}"));
            assert_eq!(h.vertices.len(), 8, "scale {s}");
            assert_eq!(h.planes.len(), 6, "scale {s}");
        }
    }
}
