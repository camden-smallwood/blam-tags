//! Precomputed radiance transfer — the per-vertex term `tool render`
//! solves and this importer used to leave at zero.
//!
//! # What the format holds
//!
//! Measured over 5,099 shipped meshes, `mesh pca data` is exactly:
//!
//! | `PRT vertex type` | bytes/vertex | floats | meshes |
//! |---|---|---|---|
//! | `No PRT` | — | — | 1,072 (21.0%) |
//! | `PRT Ambient` | **12** | 3 | 2,443 (47.9%) |
//! | `PRT Linear` | **48** | 12 | 423 (8.3%) |
//! | `PRT Quadratic` | **108** | 27 | 1,161 (22.8%) |
//!
//! Min, median and max are identical for every type, with no exceptions
//! — so it is three colour channels times the spherical-harmonic
//! coefficients of order 0, 1 and 2 (1, 4 and 9 of them).
//!
//! # Ambient is a scalar, and its scale is a known constant
//!
//! In 22,407 of 22,407 shipped ambient vertices the three channels are
//! **equal**, and the values run `0.0 ..= 0.282094`. That ceiling is
//! `1/(2*sqrt(pi))` = `Y00`, the order-0 SH basis constant, and plenty of
//! vertices sit exactly on it.
//!
//! So an ambient vertex is `Y00 * V`, where `V` is the fraction of the
//! hemisphere above the vertex that is not blocked by the model's own
//! geometry, cosine weighted, and `V = 1` in the open. That is an
//! occlusion solve, which is computable from the geometry alone — no
//! lighting, no materials, nothing this importer does not already have.
//!
//! Linear and quadratic are the same visibility projected onto more
//! basis functions — [`sh_transfer`] does all three, and
//! [`ambient_transfer`] is its order-0 case.
//!
//! # The frame, and the layout
//!
//! Two things had to be settled from the data before the higher orders
//! could be written at all.
//!
//! **Object space, not the vertex's tangent frame.** Over 212,245
//! shipped linear vertices the three l=1 slots are symmetric about zero
//! — medians 0.00002, 0.0006 and -0.007 — which is what an object frame
//! gives when normals point every way across a mesh. A tangent frame
//! would pin one slot at `0.4886 * 2/3 = 0.3257` for every open vertex;
//! the observed maxima do reach 0.33, but as the extreme of a
//! zero-centred spread rather than the norm.
//!
//! **Channel major.** In a linear record the second group of four floats
//! repeats the first to within a fraction of a percent — three
//! nearly-equal colour channels of four coefficients each, not four
//! coefficients of three channels.
//!
//! Checked against tool slot by slot on matched sources:
//!
//! ```text
//! reach_moa_statue, l=1 slot 1
//!   tool -0.325 -0.263 -0.195 -0.112 -0.047 0.000 +0.048 +0.115 +0.197 +0.263 +0.326
//!   ours -0.326 -0.269 -0.208 -0.132 -0.042 0.000 +0.043 +0.132 +0.210 +0.269 +0.325
//! ```
//!
//! **Median per-slot decile gap 0.026, worst 0.078.**
//!
//! # How close this gets, and where it does not
//!
//! Against tool's own values on 127 single-mesh ambient models whose
//! source provably built the tag, comparing deciles of the visibility
//! distribution — per-vertex comparison is impossible because a shipped
//! tag keeps no vertex positions, so there is nothing to line the two
//! vertex sets up by:
//!
//! ```text
//! halo_reveal_clouds   tool 0.653 0.708 0.735 0.761 0.793 0.830 0.865
//!                      ours 0.656 0.688 0.734 0.758 0.789 0.820 0.859
//! ```
//!
//! **Median worst-decile gap 0.079** across the set, with several models
//! tracking to within a few thousandths.
//!
//! The tail is one specific shape and worth naming. On flat debris
//! (`low_debris_c/d/e`) tool's values are **bimodal**: about half the
//! vertices are exactly 0.0 and the rest near 1.0, while this reports
//! everything open. Those meshes carry more vertices than their source
//! has corners — 80 against 72 on `low_debris_e` — so tool is emitting a
//! second, back-facing copy of a two-sided sheet and giving it zero
//! transfer. This solver has no notion of a back face: it fires up each
//! vertex's own normal and a lone sheet is open from both sides.
//! Reproducing that needs the two-sided material flag carried through to
//! here, which is not wired yet.

use crate::math::{RealPoint3d, RealVector3d};

/// `1 / (2 * sqrt(pi))`, the order-0 SH basis constant — and exactly the
/// largest value in shipped ambient PRT data.
pub const Y00: f32 = 0.282_094_79;

/// How the transfer is sampled.
#[derive(Debug, Clone)]
pub struct PrtOptions {
    /// Rays per vertex over the hemisphere.
    pub samples: usize,
    /// A ray starts this far off the surface, as a fraction of the
    /// model's size, so a vertex does not shadow itself on the triangles
    /// it belongs to.
    pub bias: f32,
}

impl Default for PrtOptions {
    fn default() -> Self {
        Self { samples: 128, bias: 1e-4 }
    }
}

// ------------------------------------------------------------------ bvh

/// A bounding-volume hierarchy over the triangles, so a vertex does not
/// have to test all of them.
///
/// Brute force is not an option at this size: guardian is 3,981 vertices
/// against 3,410 triangles, and at 128 rays each that is 1.7 billion
/// ray-triangle tests.
struct Bvh {
    nodes: Vec<BvhNode>,
    /// Triangle indices, ordered so each leaf owns a contiguous run.
    order: Vec<u32>,
}

struct BvhNode {
    lo: [f32; 3],
    hi: [f32; 3],
    /// Leaf: `first` and `count` into `order`. Interior: `first` is the
    /// right child and `count` is 0, the left child being `self + 1`.
    first: u32,
    count: u32,
}

fn tri_bounds(p: &[[f32; 3]; 3]) -> ([f32; 3], [f32; 3]) {
    let mut lo = p[0];
    let mut hi = p[0];
    for q in &p[1..] {
        for a in 0..3 {
            lo[a] = lo[a].min(q[a]);
            hi[a] = hi[a].max(q[a]);
        }
    }
    (lo, hi)
}

impl Bvh {
    fn build(tris: &[[[f32; 3]; 3]]) -> Self {
        let mut order: Vec<u32> = (0..tris.len() as u32).collect();
        let mut nodes: Vec<BvhNode> = Vec::new();
        if tris.is_empty() {
            return Self { nodes, order };
        }
        // An explicit stack: a mesh can be deep enough to overflow the
        // real one, and this runs on whatever the caller hands it.
        let mut stack = vec![(0usize, tris.len(), usize::MAX, false)];
        while let Some((begin, end, parent, is_right)) = stack.pop() {
            let me = nodes.len();
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for &t in &order[begin..end] {
                let (l, h) = tri_bounds(&tris[t as usize]);
                for a in 0..3 {
                    lo[a] = lo[a].min(l[a]);
                    hi[a] = hi[a].max(h[a]);
                }
            }
            nodes.push(BvhNode { lo, hi, first: begin as u32, count: (end - begin) as u32 });
            if parent != usize::MAX && is_right {
                nodes[parent].first = me as u32;
            }

            if end - begin <= 4 {
                continue;
            }
            // Split at the median along the widest axis. Not the best
            // heuristic, but this is a build-once structure and the sort
            // dominates either way.
            let axis = (0..3)
                .max_by(|&a, &b| (hi[a] - lo[a]).total_cmp(&(hi[b] - lo[b])))
                .unwrap_or(0);
            let mid = (begin + end) / 2;
            order[begin..end].select_nth_unstable_by(mid - begin, |&x, &y| {
                let c = |t: u32| {
                    let p = &tris[t as usize];
                    p[0][axis] + p[1][axis] + p[2][axis]
                };
                c(x).total_cmp(&c(y))
            });
            nodes[me].count = 0;
            // Right first, so the left child lands at `me + 1`.
            stack.push((mid, end, me, true));
            stack.push((begin, mid, me, false));
        }
        Self { nodes, order }
    }

    /// Is anything in the way between `o` and `o + dir * max_t`?
    ///
    /// Occlusion only — the nearest hit is never needed, so this stops at
    /// the first blocker.
    fn occluded(&self, tris: &[[[f32; 3]; 3]], o: [f32; 3], dir: [f32; 3], max_t: f32) -> bool {
        if self.nodes.is_empty() {
            return false;
        }
        let inv = [1.0 / dir[0], 1.0 / dir[1], 1.0 / dir[2]];
        let mut stack = vec![0usize];
        while let Some(at) = stack.pop() {
            let node = &self.nodes[at];
            // Slab test.
            let (mut tmin, mut tmax) = (0.0f32, max_t);
            for a in 0..3 {
                let t0 = (node.lo[a] - o[a]) * inv[a];
                let t1 = (node.hi[a] - o[a]) * inv[a];
                let (t0, t1) = if inv[a] < 0.0 { (t1, t0) } else { (t0, t1) };
                tmin = tmin.max(t0);
                tmax = tmax.min(t1);
            }
            if tmin > tmax {
                continue;
            }
            if node.count == 0 {
                stack.push(at + 1);
                stack.push(node.first as usize);
                continue;
            }
            let begin = node.first as usize;
            for &t in &self.order[begin..begin + node.count as usize] {
                if moller_trumbore(&tris[t as usize], o, dir).is_some_and(|h| h > 0.0 && h < max_t) {
                    return true;
                }
            }
        }
        false
    }
}

/// Ray-triangle, both faces. Occlusion does not care which way a
/// triangle points, and a model's own back faces block light just the
/// same.
fn moller_trumbore(p: &[[f32; 3]; 3], o: [f32; 3], d: [f32; 3]) -> Option<f32> {
    let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
    let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
    let h = [d[1] * e2[2] - d[2] * e2[1], d[2] * e2[0] - d[0] * e2[2], d[0] * e2[1] - d[1] * e2[0]];
    let a = e1[0] * h[0] + e1[1] * h[1] + e1[2] * h[2];
    if a.abs() < 1e-12 {
        return None;
    }
    let f = 1.0 / a;
    let s = [o[0] - p[0][0], o[1] - p[0][1], o[2] - p[0][2]];
    let u = f * (s[0] * h[0] + s[1] * h[1] + s[2] * h[2]);
    if !(0.0..=1.0).contains(&u) {
        return None;
    }
    let q = [s[1] * e1[2] - s[2] * e1[1], s[2] * e1[0] - s[0] * e1[2], s[0] * e1[1] - s[1] * e1[0]];
    let v = f * (d[0] * q[0] + d[1] * q[1] + d[2] * q[2]);
    if v < 0.0 || u + v > 1.0 {
        return None;
    }
    Some(f * (e2[0] * q[0] + e2[1] * q[1] + e2[2] * q[2]))
}

// ------------------------------------------------------------- the solve

/// Cosine-weighted visibility per vertex, in `0.0 ..= 1.0`.
///
/// `1.0` is a vertex with nothing above it. Multiply by [`Y00`] for the
/// value the tag stores.
pub fn ambient_transfer(
    positions: &[RealPoint3d],
    normals: &[RealVector3d],
    triangles: &[[u32; 3]],
    opts: &PrtOptions,
) -> Vec<f32> {
    let tris: Vec<[[f32; 3]; 3]> = triangles
        .iter()
        .filter_map(|t| {
            let g = |i: u32| positions.get(i as usize).map(|p| [p.x, p.y, p.z]);
            Some([g(t[0])?, g(t[1])?, g(t[2])?])
        })
        .collect();
    let bvh = Bvh::build(&tris);

    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in positions {
        for (a, c) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[a] = lo[a].min(c);
            hi[a] = hi[a].max(c);
        }
    }
    let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a])).max(1e-6);
    let bias = extent * opts.bias;
    let reach = extent * 2.0;

    // A fixed low-discrepancy set, so the same mesh always gets the same
    // answer: a stochastic solve that moves between runs cannot be
    // compared against anything, including itself.
    let dirs = cosine_hemisphere(opts.samples);

    positions
        .iter()
        .zip(normals)
        .map(|(p, n)| {
            let nl = (n.i * n.i + n.j * n.j + n.k * n.k).sqrt();
            if nl <= 0.0 {
                return 1.0;
            }
            let up = [n.i / nl, n.j / nl, n.k / nl];
            let (t, b) = basis_from(up);
            let o = [p.x + up[0] * bias, p.y + up[1] * bias, p.z + up[2] * bias];

            let mut open = 0usize;
            for d in &dirs {
                let dir = [
                    t[0] * d[0] + b[0] * d[1] + up[0] * d[2],
                    t[1] * d[0] + b[1] * d[1] + up[1] * d[2],
                    t[2] * d[0] + b[2] * d[1] + up[2] * d[2],
                ];
                if !bvh.occluded(&tris, o, dir, reach) {
                    open += 1;
                }
            }
            open as f32 / dirs.len().max(1) as f32
        })
        .collect()
}


/// Real spherical harmonics up to order 2, in the standard order
/// `[Y00, Y1-1, Y10, Y11, Y2-2, Y2-1, Y20, Y21, Y22]`.
fn sh_basis(d: [f32; 3], out: &mut [f32]) {
    let (x, y, z) = (d[0], d[1], d[2]);
    if !out.is_empty() {
        out[0] = 0.282_094_79;
    }
    if out.len() > 3 {
        out[1] = 0.488_602_5 * y;
        out[2] = 0.488_602_5 * z;
        out[3] = 0.488_602_5 * x;
    }
    if out.len() > 8 {
        out[4] = 1.092_548_4 * x * y;
        out[5] = 1.092_548_4 * y * z;
        out[6] = 0.315_391_57 * (3.0 * z * z - 1.0);
        out[7] = 1.092_548_4 * x * z;
        out[8] = 0.546_274_2 * (x * x - y * y);
    }
}

/// How many coefficients an order carries: 1, 4 or 9.
pub fn coefficients_for(order: u32) -> usize {
    match order {
        0 => 1,
        1 => 4,
        _ => 9,
    }
}

/// Per-vertex transfer, projected onto spherical harmonics of `order`.
///
/// The coefficients are in **object space**, not the vertex's tangent
/// frame. Shipped linear data settles it: all three l=1 slots are
/// symmetric about zero with medians of 0.00002, 0.0006 and -0.007 over
/// 212,245 vertices, which is what an object frame gives when normals
/// point every way across a mesh. A tangent frame would pin one slot at
/// `0.4886 * 2/3 = 0.3257` for every open vertex; the observed maxima do
/// reach ~0.33, but as the extreme of a zero-centred spread rather than
/// as the norm.
///
/// Sampling is cosine weighted, so the estimator is the plain mean of
/// `Y_lm` over the directions that were not blocked. For order 0 that
/// reduces to `Y00 * V`, which is what [`ambient_transfer`] returns.
pub fn sh_transfer(
    positions: &[RealPoint3d],
    normals: &[RealVector3d],
    triangles: &[[u32; 3]],
    order: u32,
    opts: &PrtOptions,
) -> Vec<Vec<f32>> {
    let coeffs = coefficients_for(order);
    let tris: Vec<[[f32; 3]; 3]> = triangles
        .iter()
        .filter_map(|t| {
            let g = |i: u32| positions.get(i as usize).map(|p| [p.x, p.y, p.z]);
            Some([g(t[0])?, g(t[1])?, g(t[2])?])
        })
        .collect();
    let bvh = Bvh::build(&tris);

    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for p in positions {
        for (a, c) in [p.x, p.y, p.z].into_iter().enumerate() {
            lo[a] = lo[a].min(c);
            hi[a] = hi[a].max(c);
        }
    }
    let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a])).max(1e-6);
    let bias = extent * opts.bias;
    let reach = extent * 2.0;
    let dirs = cosine_hemisphere(opts.samples);

    positions
        .iter()
        .zip(normals)
        .map(|(p, nv)| {
            let mut acc = vec![0.0f32; coeffs];
            let nl = (nv.i * nv.i + nv.j * nv.j + nv.k * nv.k).sqrt();
            if nl <= 0.0 {
                acc[0] = Y00;
                return acc;
            }
            let up = [nv.i / nl, nv.j / nl, nv.k / nl];
            let (t, b) = basis_from(up);
            let o = [p.x + up[0] * bias, p.y + up[1] * bias, p.z + up[2] * bias];

            let mut basis = vec![0.0f32; coeffs];
            for d in &dirs {
                // Into object space: the samples are built around +Z and
                // the coefficients are not tangent-frame.
                let dir = [
                    t[0] * d[0] + b[0] * d[1] + up[0] * d[2],
                    t[1] * d[0] + b[1] * d[1] + up[1] * d[2],
                    t[2] * d[0] + b[2] * d[1] + up[2] * d[2],
                ];
                if bvh.occluded(&tris, o, dir, reach) {
                    continue;
                }
                sh_basis(dir, &mut basis);
                for (a, v) in acc.iter_mut().zip(&basis) {
                    *a += *v;
                }
            }
            let inv = 1.0 / dirs.len().max(1) as f32;
            for a in acc.iter_mut() {
                *a *= inv;
            }
            acc
        })
        .collect()
}

/// The bytes a mesh of this order stores.
///
/// Channel major: every coefficient of the red channel, then green, then
/// blue. The shipped linear records show it — the second group of four
/// floats repeats the first to within a fraction of a percent, which is
/// three nearly-equal colour channels rather than four coefficients of
/// one.
pub fn sh_pca_data(per_vertex: &[Vec<f32>]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in per_vertex {
        for _ in 0..3 {
            for c in v {
                out.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    out
}

/// Cosine-weighted directions on the +Z hemisphere.
///
/// Cosine weighted because the transfer integral carries a `cos(theta)`:
/// sampling proportionally to it means every sample counts the same, and
/// the estimate is the plain unoccluded fraction.
fn cosine_hemisphere(n: usize) -> Vec<[f32; 3]> {
    (0..n.max(1))
        .map(|i| {
            // A golden-ratio spiral: even coverage without randomness, so
            // the result is reproducible.
            let u = (i as f32 + 0.5) / n.max(1) as f32;
            let phi = i as f32 * std::f32::consts::PI * (3.0 - 5.0f32.sqrt());
            let r = u.sqrt();
            [r * phi.cos(), r * phi.sin(), (1.0 - u).max(0.0).sqrt()]
        })
        .collect()
}

/// Any two axes perpendicular to `n`.
fn basis_from(n: [f32; 3]) -> ([f32; 3], [f32; 3]) {
    // Picking the smaller component keeps the cross product well
    // conditioned whichever way the normal points.
    let a = if n[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let t = [
        a[1] * n[2] - a[2] * n[1],
        a[2] * n[0] - a[0] * n[2],
        a[0] * n[1] - a[1] * n[0],
    ];
    let l = (t[0] * t[0] + t[1] * t[1] + t[2] * t[2]).sqrt().max(1e-20);
    let t = [t[0] / l, t[1] / l, t[2] / l];
    let b = [
        n[1] * t[2] - n[2] * t[1],
        n[2] * t[0] - n[0] * t[2],
        n[0] * t[1] - n[1] * t[0],
    ];
    (t, b)
}

/// The bytes a `PRT Ambient` mesh stores: `Y00 * V`, three times per
/// vertex.
pub fn ambient_pca_data(visibility: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(visibility.len() * 12);
    for v in visibility {
        let c = (Y00 * v.clamp(0.0, 1.0)).to_le_bytes();
        for _ in 0..3 {
            out.extend_from_slice(&c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32, z: f32) -> RealPoint3d {
        RealPoint3d { x, y, z }
    }
    fn v(i: f32, j: f32, k: f32) -> RealVector3d {
        RealVector3d { i, j, k }
    }

    /// A lone triangle has nothing to shadow it.
    #[test]
    fn an_unoccluded_surface_is_fully_open() {
        let pos = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(0.0, 1.0, 0.0)];
        let nrm = vec![v(0.0, 0.0, 1.0); 3];
        let out = ambient_transfer(&pos, &nrm, &[[0, 1, 2]], &PrtOptions::default());
        for x in &out {
            assert!(*x > 0.99, "an open surface should see the whole sky, got {x}");
        }
        // And the stored value is the SH constant itself.
        let bytes = ambient_pca_data(&out);
        assert_eq!(bytes.len(), 3 * 12);
        let first = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert!((first - Y00).abs() < 1e-4, "expected Y00, got {first}");
    }

    /// A lid directly above blocks most of the hemisphere.
    #[test]
    fn a_surface_under_a_lid_is_mostly_closed() {
        let mut pos = vec![p(0.0, 0.0, 0.0), p(4.0, 0.0, 0.0), p(0.0, 4.0, 0.0)];
        let mut nrm = vec![v(0.0, 0.0, 1.0); 3];
        // A big quad a short way above, as two triangles.
        for q in [
            p(-8.0, -8.0, 0.25),
            p(8.0, -8.0, 0.25),
            p(8.0, 8.0, 0.25),
            p(-8.0, 8.0, 0.25),
        ] {
            pos.push(q);
            nrm.push(v(0.0, 0.0, -1.0));
        }
        let tris = [[0, 1, 2], [3, 4, 5], [3, 5, 6]];
        let out = ambient_transfer(&pos, &nrm, &tris, &PrtOptions::default());
        assert!(out[0] < 0.15, "a covered vertex should be mostly blocked, got {}", out[0]);
    }

    /// The sampling is fixed, so two runs of the same mesh agree.
    #[test]
    fn the_solve_is_reproducible() {
        let pos = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(0.0, 1.0, 0.0)];
        let nrm = vec![v(0.0, 0.0, 1.0); 3];
        let a = ambient_transfer(&pos, &nrm, &[[0, 1, 2]], &PrtOptions::default());
        let b = ambient_transfer(&pos, &nrm, &[[0, 1, 2]], &PrtOptions::default());
        assert_eq!(a, b);
    }
}
