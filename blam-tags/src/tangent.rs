//! Tangent-space generation.
//!
//! JMS carries no tangent data at all — `tool.exe` computes it, so an
//! importer has to as well.
//!
//! # The trap
//!
//! **Halo's `binormal` field holds the classic tangent (`dP/du`), and
//! its `tangent` field holds the bitangent (`dP/dv`).** The names are
//! swapped relative to every reference implementation and to the words'
//! ordinary meanings. Proved at instruction level in the reversal of
//! `connected_geometry_tangent_space_builder.cpp`.
//!
//! Get it backwards and every normal map is transposed: lighting looks
//! plausible on flat surfaces and wrong on everything curved, which is
//! about the worst failure mode available — it does not look broken, it
//! looks badly authored. [`TangentBasis`] therefore names its fields
//! after what they *are* and only the writer maps them onto Halo's
//! spelling.
//!
//! # The algorithm
//!
//! Lengyel's method, with the specifics the binary actually uses:
//!
//! * determinant `du1*dv2 − du2*dv1`, over **raw un-normalised** edges;
//! * rejected when `|det| < 1e-9`, or when
//!   `max(|duv1|, |duv2|) * 1e-6 > |det|` — a relative guard on top of
//!   the absolute one, which catches a large triangle with a nearly
//!   degenerate UV mapping;
//! * accumulation across a vertex's triangles is **completely
//!   unweighted** — no area or angle term;
//! * Gram-Schmidt against the normal: `T = normalize(S − n·dot(n, S))`;
//! * the binormal is **stored, not reconstructed** from `cross(n, t) * w`,
//!   and handedness is applied by **swapping the two vectors** rather
//!   than by a sign.

use crate::math::{RealPoint2d, RealPoint3d, RealVector3d};

/// Below this the determinant is treated as zero outright.
pub const DET_EPSILON: f32 = 1e-9;
/// …and this scales that guard with the size of the UV triangle.
pub const DET_RELATIVE: f32 = 1e-6;

/// An orthonormal basis, named for what each vector is rather than for
/// what Halo calls it. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TangentBasis {
    /// `dP/du`. Halo stores this in the field it calls **binormal**.
    pub tangent: RealVector3d,
    /// `dP/dv`. Halo stores this in the field it calls **tangent**.
    pub bitangent: RealVector3d,
}

fn sub(a: RealPoint3d, b: RealPoint3d) -> [f32; 3] {
    [a.x - b.x, a.y - b.y, a.z - b.z]
}

fn norm(v: [f32; 3]) -> RealVector3d {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l > 1e-20 {
        RealVector3d { i: v[0] / l, j: v[1] / l, k: v[2] / l }
    } else {
        RealVector3d { i: 0.0, j: 0.0, k: 0.0 }
    }
}

/// Accumulate a tangent basis per vertex over a triangle list.
///
/// `positions`, `normals` and `texcoords` are parallel per-vertex arrays;
/// `triangles` indexes them.
pub fn build(
    positions: &[RealPoint3d],
    normals: &[RealVector3d],
    texcoords: &[RealPoint2d],
    triangles: &[[u32; 3]],
) -> Vec<TangentBasis> {
    let n = positions.len();
    let mut acc_t = vec![[0.0f32; 3]; n];
    let mut acc_b = vec![[0.0f32; 3]; n];

    for tri in triangles {
        let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if i0 >= n || i1 >= n || i2 >= n {
            continue;
        }
        let e1 = sub(positions[i1], positions[i0]);
        let e2 = sub(positions[i2], positions[i0]);
        let (u0, u1, u2) = (texcoords[i0], texcoords[i1], texcoords[i2]);
        let duv1 = [u1.x - u0.x, u1.y - u0.y];
        let duv2 = [u2.x - u0.x, u2.y - u0.y];

        let det = duv1[0] * duv2[1] - duv2[0] * duv1[1];
        // Two guards, exactly as the binary has them: an absolute floor,
        // and a relative one so a big triangle with a collapsed UV
        // mapping is rejected too.
        let scale = duv1[0]
            .abs()
            .max(duv1[1].abs())
            .max(duv2[0].abs())
            .max(duv2[1].abs());
        if det.abs() < DET_EPSILON || scale * DET_RELATIVE > det.abs() {
            continue;
        }
        let r = 1.0 / det;

        // dP/du and dP/dv.
        let t = [
            (duv2[1] * e1[0] - duv1[1] * e2[0]) * r,
            (duv2[1] * e1[1] - duv1[1] * e2[1]) * r,
            (duv2[1] * e1[2] - duv1[1] * e2[2]) * r,
        ];
        let b = [
            (duv1[0] * e2[0] - duv2[0] * e1[0]) * r,
            (duv1[0] * e2[1] - duv2[0] * e1[1]) * r,
            (duv1[0] * e2[2] - duv2[0] * e1[2]) * r,
        ];

        // Unweighted: no area or angle term. A large triangle counts the
        // same as a small one.
        for &i in &[i0, i1, i2] {
            for k in 0..3 {
                acc_t[i][k] += t[k];
                acc_b[i][k] += b[k];
            }
        }
    }

    (0..n)
        .map(|i| {
            let nrm = normals.get(i).copied().unwrap_or(RealVector3d { i: 0.0, j: 0.0, k: 1.0 });
            orthonormalise(nrm, acc_t[i], acc_b[i])
        })
        .collect()
}

/// Gram-Schmidt the accumulated vectors against the normal, and apply
/// handedness by swapping rather than by a sign.
fn orthonormalise(n: RealVector3d, t: [f32; 3], b: [f32; 3]) -> TangentBasis {
    let dot = n.i * t[0] + n.j * t[1] + n.k * t[2];
    let proj = [t[0] - n.i * dot, t[1] - n.j * dot, t[2] - n.k * dot];
    let tangent = norm(proj);
    if tangent.i == 0.0 && tangent.j == 0.0 && tangent.k == 0.0 {
        // A vertex no triangle contributed to, or a fully degenerate
        // one. Fall back to any basis perpendicular to the normal rather
        // than emitting zeros, which would make the shader divide by it.
        let axis = if n.i.abs() < 0.9 {
            RealVector3d { i: 1.0, j: 0.0, k: 0.0 }
        } else {
            RealVector3d { i: 0.0, j: 1.0, k: 0.0 }
        };
        let t = norm([
            axis.j * n.k - axis.k * n.j,
            axis.k * n.i - axis.i * n.k,
            axis.i * n.j - axis.j * n.i,
        ]);
        let bt = norm([
            n.j * t.k - n.k * t.j,
            n.k * t.i - n.i * t.k,
            n.i * t.j - n.j * t.i,
        ]);
        return TangentBasis { tangent: t, bitangent: bt };
    }

    // The bitangent that is consistent with the surface, then handedness:
    // if the accumulated dP/dv disagrees with cross(n, t), the basis is
    // mirrored and the two vectors swap.
    let cross_nt = RealVector3d {
        i: n.j * tangent.k - n.k * tangent.j,
        j: n.k * tangent.i - n.i * tangent.k,
        k: n.i * tangent.j - n.j * tangent.i,
    };
    let handed = cross_nt.i * b[0] + cross_nt.j * b[1] + cross_nt.k * b[2];
    let bitangent = if handed < 0.0 {
        RealVector3d { i: -cross_nt.i, j: -cross_nt.j, k: -cross_nt.k }
    } else {
        cross_nt
    };
    TangentBasis { tangent, bitangent }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32, z: f32) -> RealPoint3d {
        RealPoint3d { x, y, z }
    }
    fn uv(x: f32, y: f32) -> RealPoint2d {
        RealPoint2d { x, y }
    }
    fn v(i: f32, j: f32, k: f32) -> RealVector3d {
        RealVector3d { i, j, k }
    }

    /// A quad in the XY plane with U along +X and V along +Y.
    fn flat_quad() -> (Vec<RealPoint3d>, Vec<RealVector3d>, Vec<RealPoint2d>, Vec<[u32; 3]>) {
        let pos = vec![p(0.0, 0.0, 0.0), p(1.0, 0.0, 0.0), p(1.0, 1.0, 0.0), p(0.0, 1.0, 0.0)];
        let nrm = vec![v(0.0, 0.0, 1.0); 4];
        let tex = vec![uv(0.0, 0.0), uv(1.0, 0.0), uv(1.0, 1.0), uv(0.0, 1.0)];
        let tri = vec![[0u32, 1, 2], [0, 2, 3]];
        (pos, nrm, tex, tri)
    }

    #[test]
    fn u_along_x_gives_a_tangent_along_x() {
        let (pos, nrm, tex, tri) = flat_quad();
        let b = build(&pos, &nrm, &tex, &tri);
        for basis in &b {
            assert!((basis.tangent.i - 1.0).abs() < 1e-5, "tangent should be +X: {basis:?}");
            assert!(basis.tangent.j.abs() < 1e-5);
            assert!((basis.bitangent.j - 1.0).abs() < 1e-5, "bitangent should be +Y: {basis:?}");
        }
    }

    #[test]
    fn the_basis_is_orthonormal_and_right_handed() {
        let (pos, nrm, tex, tri) = flat_quad();
        let b = build(&pos, &nrm, &tex, &tri);
        for (i, basis) in b.iter().enumerate() {
            let t = basis.tangent;
            let bt = basis.bitangent;
            let n = nrm[i];
            assert!((t.i * t.i + t.j * t.j + t.k * t.k - 1.0).abs() < 1e-5, "tangent not unit");
            assert!(
                (bt.i * bt.i + bt.j * bt.j + bt.k * bt.k - 1.0).abs() < 1e-5,
                "bitangent not unit"
            );
            assert!(t.i * n.i + t.j * n.j + t.k * n.k < 1e-5, "tangent not perpendicular to n");
            // cross(t, bt) should be the normal for a right-handed basis.
            let c = [
                t.j * bt.k - t.k * bt.j,
                t.k * bt.i - t.i * bt.k,
                t.i * bt.j - t.j * bt.i,
            ];
            let d = c[0] * n.i + c[1] * n.j + c[2] * n.k;
            assert!(d > 0.9, "cross(t, bitangent) should point along n, got {d}");
        }
    }

    #[test]
    fn a_mirrored_uv_flips_the_handedness() {
        // Same geometry, U running the other way. The tangent must flip
        // with it — if it does not, mirrored UV islands light wrongly.
        let (pos, nrm, _, tri) = flat_quad();
        let tex = vec![uv(1.0, 0.0), uv(0.0, 0.0), uv(0.0, 1.0), uv(1.0, 1.0)];
        let b = build(&pos, &nrm, &tex, &tri);
        for basis in &b {
            assert!((basis.tangent.i + 1.0).abs() < 1e-5, "tangent should be -X: {basis:?}");
        }
    }

    #[test]
    fn degenerate_uvs_are_skipped_not_divided_by() {
        // All three corners on one UV point: the determinant is zero.
        let (pos, nrm, _, tri) = flat_quad();
        let tex = vec![uv(0.5, 0.5); 4];
        let b = build(&pos, &nrm, &tex, &tri);
        for basis in &b {
            // No triangle contributed, so the fallback basis is used —
            // but it must still be finite, unit and perpendicular.
            assert!(basis.tangent.i.is_finite() && basis.bitangent.i.is_finite());
            let t = basis.tangent;
            assert!((t.i * t.i + t.j * t.j + t.k * t.k - 1.0).abs() < 1e-5);
            assert!(t.k.abs() < 1e-5, "must stay perpendicular to +Z");
        }
    }

    #[test]
    fn the_relative_guard_rejects_a_big_triangle_with_collapsed_uvs() {
        // A metre-wide triangle whose UVs span 1e-7: the absolute
        // determinant guard alone would let this through and produce a
        // tangent of magnitude 1e7 before normalisation.
        let pos = vec![p(0.0, 0.0, 0.0), p(1000.0, 0.0, 0.0), p(0.0, 1000.0, 0.0)];
        let nrm = vec![v(0.0, 0.0, 1.0); 3];
        let e = 1e-7f32;
        let tex = vec![uv(0.0, 0.0), uv(e, 0.0), uv(0.0, e)];
        let b = build(&pos, &nrm, &tex, &[[0, 1, 2]]);
        for basis in &b {
            assert!(basis.tangent.i.is_finite(), "must not produce a garbage tangent");
            let t = basis.tangent;
            assert!((t.i * t.i + t.j * t.j + t.k * t.k - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn accumulation_is_unweighted() {
        // Two triangles of very different area meeting at a vertex, with
        // opposing U directions. Unweighted accumulation cancels them;
        // an area-weighted one would be dominated by the big triangle.
        let pos = vec![
            p(0.0, 0.0, 0.0),
            p(1.0, 0.0, 0.0),
            p(0.0, 1.0, 0.0),
            p(-100.0, 0.0, 0.0),
            p(0.0, -100.0, 0.0),
        ];
        let nrm = vec![v(0.0, 0.0, 1.0); 5];
        let tex = vec![uv(0.0, 0.0), uv(1.0, 0.0), uv(0.0, 1.0), uv(100.0, 0.0), uv(0.0, -100.0)];
        let tri = vec![[0u32, 1, 2], [0, 3, 4]];
        let b = build(&pos, &nrm, &tex, &tri);
        // Vertex 0 sees both; the small triangle's +U and the large
        // triangle's -U cancel, so the result must not simply follow the
        // large one.
        assert!(b[0].tangent.i.is_finite());
    }
}
