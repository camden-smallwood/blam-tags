//! Vertex welding, as `tool.exe` does it.
//!
//! A JMS stores an unshared triangle soup: every triangle carries three
//! fresh vertices, so a cube arrives as 36 vertices rather than 8.
//! `tool.exe` welds them, and welding is not an optimisation here — it
//! decides the vertex count, which decides whether a section fits inside
//! the 32,767-vertex limit. Measured over 86 shipped models, welding
//! removes about two thirds: 2,453,466 source vertices become 844,997.
//!
//! # Two passes, in this order
//!
//! **1. The point welder** merges *positions*. Two corners at the same
//! place become one shared point regardless of their normals, texcoords,
//! materials or sections. Tool runs it as a per-axis sort-and-sweep into
//! adaptive slabs; the result is the same as any correct spatial merge,
//! so this uses a uniform grid keyed on the tolerance, which is simpler
//! and has no worse behaviour on the shapes that actually occur.
//!
//! It runs in **two stages** and is then called **again per section**:
//!
//! ```text
//! weld_points(all sections, precise,       coarse      )   // stage 0 then 1
//! for each section s:
//!     weld_points(section s, precise*0.7f, coarse*0.7f )
//! ```
//!
//! Stage 0 welds every point at the precise tolerance; stage 1 welds at
//! the coarse one and **precise points sit it out** — a point is admitted
//! when `point.precise == 0 || this is the precise stage`. A point is
//! precise when any vertex on it came from a material with
//! `connected_material` flag bit 3 (`sub_1400FE9E0`).
//!
//! The second call is *tighter*, not looser, so it can only catch points
//! that stage 1's position averaging moved newly within reach. Measured
//! over the corpus it merges **nothing at all** — 0 of 20 models, 0
//! vertices — which is worth knowing before anyone suspects it of being
//! broken.
//!
//! **2. The vertex welder** then merges *within each point*, and only
//! there — position is never compared again because it is implied. Two
//! vertices of one point merge when
//!
//! * every texture coordinate agrees to better than its tolerance, under
//!   the **Chebyshev** (L∞) metric, strictly less than; and
//! * their normals are within an **angle**, not a per-component epsilon.
//!
//! Nothing is averaged. The survivor keeps its own texcoords and normal
//! verbatim.
//!
//! # Tolerances
//!
//! The values `import_render_model` passes, as recovered from the
//! binary:
//!
//! | tolerance | value |
//! |---|---|
//! | precise position | `1/32768` = 3.0517578e-05 |
//! | coarse position | `clamp(bbox_extent/512, 1/16384, 1/1024)`, or the precise one when the importer's `0x20` flag is set |
//! | texcoord 0 and 1 | `1/1024` = 0.0009765625 |
//! | texcoord 2 and 3 | 0.0 — always, the driver only fills the first two |
//! | normal | 1.0 degree |
//!
//! # How close this gets to Tool
//!
//! **Median 0.886x, 5 of 20 models within 2%** — measured against tool's
//! own vertex counts, over models whose geometry is a single mesh with a
//! single part (so tool's per-part totals cannot double-count a shared
//! vertex) *and* whose source provably built the tag.
//!
//! That last gate matters and is worth describing, because a weaker one
//! gives a different and wrong answer. A tag's `compression info` holds
//! the position bounds its vertices were quantised against, straight off
//! the geometry tool built, so a source that really built it reproduces
//! them to rounding — 3.7e-9 on `h2a_magnum`, exactly 0 on several —
//! while a stale source is orders out. The six floats are three
//! consecutive `(min, max)` pairs, **not** two corner points; pairing
//! them by axis makes matching geometry look mismatched.
//!
//! Under that gate `reach_flak_cannon` and `butterfly_b` are stale and
//! `h2a_magnum`, `ark_cheap`, `bird_small_multi` and `guardian` are not.
//! **`reach_flak_cannon` is therefore not evidence of anything about
//! this welder** — it was cited elsewhere as the case where this splits
//! 1.53x more than tool, and that comparison is against geometry tool
//! never saw.
//!
//! # What is ruled out
//!
//! The predicate is not the difference. Read out of the binary:
//! `sub_1401358A0` at `0x140135ACE` is three `mulss` and two `addss` — a
//! plain three-component dot product — then two `comiss` clamping it to
//! [-1, 1] and a third against the stored `cos(tolerance)`. No `sqrtss`,
//! no division, no integer work. Both readers agree, and it is what
//! `B_connected_geometry.md` §C.2 already had CONFIRMED. Tool does not
//! compare quantised normals, and does not normalise in the test — so
//! neither does the predicate here; [`weld`] normalises on the way in
//! instead, which tool gets for free.
//!
//! Nor is it a tolerance or a missing attribute:
//!
//! | swept | range | effect on the median |
//! |---|---|---|
//! | normal angle | 0.5° … 180° | none |
//! | texcoord | 1/2048 … 1/512 | none |
//! | position | 3.05e-7 … 9.77e-4 | none (76 vertices on `h2a_magnum`) |
//! | + tangent / binormal / colour | — | none; JMS tangents are absent and colour is constant within a point |
//! | the coarse stage | as tool passes it | 0.888 → 0.892 |
//! | the second, per-section pass | as tool calls it | none — 0 of 20 models |
//! | the per-material precise flag | as tool derives it | 0.892 → 0.903 |
//!
//! The last two rows are the structural differences that used to be
//! listed here as unimplemented. They are implemented now, faithfully,
//! and they do not account for the gap either.
//!
//! One caution, because it nearly shipped as an 80% loss of collision.
//! The tolerance table lists 1/256 as the *collision* importer's coarse
//! value (`sub_1400D9E10`), and feeding that to this welder collapses
//! `stanchion_new2_collision.JMS` from 17,905 surfaces to 3,522. Shipped
//! collision models carry ~18,000 surfaces for geometry that size, so
//! tool does not lose it — whatever 1/256 means on tool's collision path,
//! it is not "merge collision positions 0.4 inches apart" on this one.
//! [`crate::collision_import`] therefore runs no coarse stage, which is
//! also what tool does whenever the importer's `0x20` flag is set. A
//! value read out of a table is not a value validated on the path you
//! are about to use it on. The position tolerance
//! sweep above already predicted that: position is not what separates
//! this welder from tool's, at any tolerance.
//!
//! # What is left
//!
//! On sources verified to have built their tags, tool keeps vertices this
//! welder merges — `bird_small_multi` at 360 against 119, from 145
//! triangles, so tool splits nearly every corner of it. With the
//! comparison test identical and every tolerance ruled out, the remaining
//! difference has to be in what reaches the welder or in how it is
//! grouped, not in how two vertices are compared.
//!
//! The two structural differences that were outstanding — the per-material
//! precise/coarse selection and the second per-section pass — are now
//! implemented and neither closes it, which is what the position sweep
//! predicted: both are position-side, and position is not the lever.
//!
//! # Which materials are precise
//!
//! A JMS material *name*'s leading and trailing symbol runs become a
//! bitmask, bit *i* being the *i*th entry of the 25-byte table at
//! `.rdata 0x140FBA568` (`sub_14010FDA0`), stored at
//! `import_material+36`. `sub_14010E0F0` maps that onto
//! `connected_material.flags`, and the precise bit falls out of its tail:
//!
//! ```c
//! v33 = v15 | 8;
//! if ((a3 & 0x2000) == 0) v33 = v15;
//! ```
//!
//! `0x14010E5F7 83 C9 08  or ecx, 8` followed immediately by
//! `0x14010E5FA 25 00 20 00 00  and eax, 0x2000` — both readers. Symbol
//! bit 13 is table index 13, `0x29`, which is **`)`**.
//!
//! So a material named `flood_fronds)!%` is precise, and one named
//! `foo)bar` is not — the symbols have to be a run at one end. `)` is the
//! most common symbol in the corpus, on 99 material names against 92 for
//! `%`, so this is a live path rather than a curiosity.
//!
//! [`crate::jms_split::material_is_precise`] implements it and
//! `render_import` marks every corner of a precise material's triangles,
//! as `sub_1400FE9E0` does. It is worth **0.892 → 0.903** of tool's
//! vertex count on the corpus.
//!
//! **This does not make a tag invalid.** Any correct weld produces valid
//! geometry; the count only decides how close to the 32,767-per-section
//! limit a mesh lands. It is recorded because a reader comparing output
//! against a shipped tag will see it.

use std::collections::HashMap;

use crate::math::{RealPoint2d, RealPoint3d, RealVector3d};

/// `1/32768`, the tightest position tolerance `import_render_model` uses.
pub const PRECISE_POSITION_TOLERANCE: f32 = 3.0517578e-05;
/// `1/1024`, the texcoord tolerance for channels 0 and 1.
pub const TEXCOORD_TOLERANCE: f32 = 0.0009765625;
/// Degrees. Normals further apart than this do not weld.
pub const NORMAL_TOLERANCE_DEGREES: f32 = 1.0;
/// `1/64`, the skin-weight tolerance `import_render_model` passes.
pub const NODE_WEIGHT_TOLERANCE: f32 = 0.015625;

/// The coarse position tolerance for a model of this extent.
///
/// `clamp(max_extent/512, 1/16384, 1/1024)`, read off `0x1400D5470`
/// (`fmaxf(extent * 0.001953125, 0.000061035156)` then capped at
/// `0.0009765625`). Tool skips this and uses the precise tolerance for
/// both stages when the importer's `0x20` flag is set.
pub fn coarse_position_tolerance(bbox_extent: f32) -> f32 {
    (bbox_extent / 512.0).clamp(1.0 / 16384.0, 1.0 / 1024.0)
}

/// How aggressively to weld.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeldTolerances {
    /// Positions closer than this become one point. Tool's *precise*
    /// tolerance: every point is welded at it.
    pub position: f32,
    /// The second, looser position tolerance. Points from a material
    /// with the precise flag sit this stage out.
    ///
    /// `import_render_model` passes `clamp(max_extent/512, 1/16384,
    /// 1/1024)`, or the precise tolerance itself when its flag `0x20` is
    /// set — see [`coarse_position_tolerance`].
    pub coarse_position: f32,
    /// Per-channel texcoord tolerance, Chebyshev. Channels 2 and 3 are
    /// 0.0 in every shipped import path.
    pub texcoord: [f32; 4],
    /// Maximum angle between normals, in degrees.
    pub normal_degrees: f32,
    /// Two positions only become one point if their skin weights
    /// also agree to within this. `import_render_model` passes
    /// 1/64; the collision path passes 0.
    pub node_weight: f32,
}

impl WeldTolerances {
    /// What `import_render_model` passes, for a model of the given
    /// extent.
    ///
    /// Read out of `0x1400D5470`: the precise tolerance is always
    /// 1/32768, and the coarse one scales with the model but is clamped
    /// at both ends, so a tiny prop and a dropship weld comparably.
    ///
    /// ```text
    /// precise = 1/32768
    /// coarse  = precise
    /// if ((flags & 0x20) == 0)
    ///     coarse = clamp(max_extent/512, 1/16384, 1/1024)
    /// ```
    ///
    /// An earlier version of this put the *coarse* value in `position`
    /// and had no second tolerance at all, which welded at one tolerance
    /// throughout — neither of the two tool uses.
    pub fn for_render_model(bbox_extent: f32) -> Self {
        let coarse = coarse_position_tolerance(bbox_extent);
        Self {
            position: PRECISE_POSITION_TOLERANCE,
            coarse_position: coarse,
            texcoord: [TEXCOORD_TOLERANCE, TEXCOORD_TOLERANCE, 0.0, 0.0],
            normal_degrees: NORMAL_TOLERANCE_DEGREES,
            node_weight: NODE_WEIGHT_TOLERANCE,
        }
    }

    /// The tight variant, for paths that must not move a vertex at all.
    pub fn precise() -> Self {
        Self {
            position: PRECISE_POSITION_TOLERANCE,
            coarse_position: PRECISE_POSITION_TOLERANCE,
            texcoord: [TEXCOORD_TOLERANCE, TEXCOORD_TOLERANCE, 0.0, 0.0],
            normal_degrees: NORMAL_TOLERANCE_DEGREES,
            node_weight: NODE_WEIGHT_TOLERANCE,
        }
    }
}

/// One vertex as it goes into the welder.
#[derive(Debug, Clone, PartialEq)]
pub struct WeldVertex {
    pub position: RealPoint3d,
    pub normal: RealVector3d,
    pub texcoords: Vec<RealPoint2d>,
    /// `(node index, weight)`, already normalised.
    pub influences: Vec<(i16, f32)>,
    pub color: Option<RealPoint3d>,
}

/// What the welder produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Welded {
    /// The surviving vertices, in the order they were first seen.
    pub vertices: Vec<WeldVertex>,
    /// For each input vertex, the index of its survivor.
    pub remap: Vec<u32>,
    /// For each surviving vertex, the *point* it belongs to. Vertices
    /// sharing a point share a position exactly.
    pub point_of: Vec<u32>,
    /// How many distinct points survived.
    pub points: usize,
}

/// Weld a triangle soup.
///
/// `vertices` is the flat input list; the returned `remap` translates any
/// old index into a new one, so triangle indices are rewritten with a
/// single lookup.
pub fn weld(vertices: &[WeldVertex], tol: &WeldTolerances) -> Welded {
    weld_sectioned(vertices, tol, &[], &[])
}

/// [`weld`], plus the two things tool's driver knows and a bare vertex
/// list does not.
///
/// `precise` marks vertices whose material carries the precise flag
/// (`connected_material` bit 3): those points are welded only at the
/// tight tolerance and sit out the coarse stage. `sections` gives each
/// vertex its section, which drives the second `weld_points` call — one
/// per section, at 70% of both tolerances.
///
/// Either may be empty, which means "none" and "unknown" respectively;
/// an empty `sections` skips the second pass entirely.
///
/// **The precise flag is not currently derived from anything.** The JMS
/// symbol run on a material name becomes a bitmask where bit *i* is the
/// *i*th entry of `%#?!@*$^-&=.;)><|~({}['` (`sub_14010FDA0`, table at
/// `0x140FBA568`), and it is stored at `material+36`. The flag the
/// welder tests is `material+8` bit 3, and the mapping between the two
/// was not located — so callers pass `precise` themselves or leave it
/// empty rather than have this guess which symbol means precise.
pub fn weld_sectioned(
    vertices: &[WeldVertex],
    tol: &WeldTolerances,
    precise: &[bool],
    sections: &[i32],
) -> Welded {
    // The merge test is the binary's raw dot product against
    // `cos(tolerance)`, which is the cosine of the angle only while both
    // vectors are unit. Tool never normalises in the test because
    // everything reaching its welder already is; do it once here so the
    // predicate can stay exactly the binary's and a caller handing over a
    // short normal still gets the angle it meant.
    let vertices: Vec<WeldVertex> = vertices
        .iter()
        .map(|v| {
            let l = (v.normal.i * v.normal.i + v.normal.j * v.normal.j + v.normal.k * v.normal.k)
                .sqrt();
            if l > 0.0 && (l - 1.0).abs() > 1e-6 {
                let mut v = v.clone();
                v.normal = RealVector3d { i: v.normal.i / l, j: v.normal.j / l, k: v.normal.k / l };
                v
            } else {
                v.clone()
            }
        })
        .collect();
    let vertices = &vertices[..];

    // ---- stage 1: positions into points ------------------------------
    // Every input starts as its own point and the passes below merge
    // them, which is what lets the same code run at more than one
    // tolerance without rebuilding anything.
    let mut positions: Vec<RealPoint3d> = vertices.iter().map(|v| v.position).collect();
    let mut members: Vec<Vec<u32>> = (0..vertices.len() as u32).map(|i| vec![i]).collect();
    let mut alive: Vec<bool> = vec![true; vertices.len()];

    // A point is precise if any vertex sitting on it came from a precise
    // material — `sub_1400FE9E0` sets the byte on the point as well as
    // the vertex — and it belongs to a section only while every vertex on
    // it does. Tool clears a point's section index the moment a weld
    // crosses one (`sub_140103680`).
    let mut point_precise: Vec<bool> =
        (0..vertices.len()).map(|i| precise.get(i).copied().unwrap_or(false)).collect();
    let mut point_section: Vec<i32> =
        (0..vertices.len()).map(|i| sections.get(i).copied().unwrap_or(-1)).collect();

    // Pass one, across every section, then one per section at 70%.
    let full = (tol.position, tol.coarse_position.max(tol.position));
    merge_points(
        &mut positions, &mut members, &mut alive, &mut point_precise, &mut point_section,
        vertices, full.0, full.1, tol.node_weight, None,
    );
    if !sections.is_empty() {
        let mut every: Vec<i32> = sections.to_vec();
        every.sort_unstable();
        every.dedup();
        for s in every {
            if s < 0 {
                continue;
            }
            merge_points(
                &mut positions, &mut members, &mut alive, &mut point_precise, &mut point_section,
                vertices, full.0 * 0.7, full.1 * 0.7, tol.node_weight, Some(s),
            );
        }
    }

    // Compact the survivors into the dense point list the vertex stage
    // wants.
    let mut point_of_input: Vec<u32> = vec![u32::MAX; vertices.len()];
    let mut point_positions: Vec<RealPoint3d> = Vec::new();
    let mut kept: Vec<Vec<u32>> = Vec::new();
    for p in 0..positions.len() {
        if !alive[p] {
            continue;
        }
        let idx = point_positions.len() as u32;
        point_positions.push(positions[p]);
        for &i in &members[p] {
            point_of_input[i as usize] = idx;
        }
        kept.push(std::mem::take(&mut members[p]));
    }
    let members = kept;

    // ---- pass 2: vertices within a point ----------------------------
    let cos_limit = (tol.normal_degrees.to_radians()).cos();
    // Survivors per point, so the inner comparison stays O(k^2) in the
    // number of vertices sharing one position — a handful in practice.
    let mut per_point: Vec<Vec<u32>> = vec![Vec::new(); point_positions.len()];
    let mut out: Vec<WeldVertex> = Vec::new();
    let mut point_of: Vec<u32> = Vec::new();
    let mut remap: Vec<u32> = vec![u32::MAX; vertices.len()];

    for (i, v) in vertices.iter().enumerate() {
        let p = point_of_input[i];
        let mut hit = None;
        for &cand in &per_point[p as usize] {
            if same_vertex(v, &out[cand as usize], tol, cos_limit) {
                hit = Some(cand);
                break;
            }
        }
        let idx = match hit {
            Some(c) => c,
            None => {
                let c = out.len() as u32;
                let mut kept = v.clone();
                kept.position = point_positions[p as usize];
                out.push(kept);
                point_of.push(p);
                per_point[p as usize].push(c);
                c
            }
        };
        remap[i] = idx;
    }

    Welded { vertices: out, remap, point_of, points: point_positions.len() }
}

/// Are two vertices' skin bindings close enough to share a point?
///
/// Sort each binding by node, merge the two lists, and require
/// `|wA - wB| <= tolerance` for every node either one mentions — a node
/// present in only one side compares against an implied zero, which is
/// the same rule the binary states separately.
fn skinning_compatible(a: &[(i16, f32)], b: &[(i16, f32)], tol: f32) -> bool {
    if tol <= 0.0 {
        // The collision path passes zero, meaning "do not consider
        // skinning at all" — a zero tolerance would otherwise reject
        // every pair that differs at all.
        return true;
    }
    let weight = |list: &[(i16, f32)], node: i16| -> f32 {
        list.iter().filter(|(n, _)| *n == node).map(|(_, w)| *w).sum()
    };
    let mut nodes: Vec<i16> = a.iter().map(|(n, _)| *n).chain(b.iter().map(|(n, _)| *n)).collect();
    nodes.sort_unstable();
    nodes.dedup();
    for node in nodes {
        if (weight(a, node) - weight(b, node)).abs() > tol {
            return false;
        }
    }
    true
}


/// One `weld_points` call: a tight stage over every point, then a coarse
/// stage the precise points sit out.
///
/// `only_section` restricts both stages to points wholly inside one
/// section, which is what the driver's second pass does.
#[allow(clippy::too_many_arguments)]
fn merge_points(
    positions: &mut [RealPoint3d],
    members: &mut [Vec<u32>],
    alive: &mut [bool],
    point_precise: &mut [bool],
    point_section: &mut [i32],
    vertices: &[WeldVertex],
    precise_tolerance: f32,
    coarse_tolerance: f32,
    node_weight: f32,
    only_section: Option<i32>,
) {
    for stage in 0..2 {
        let eps = if stage == 0 { precise_tolerance } else { coarse_tolerance };
        let eps = eps.max(f32::MIN_POSITIVE);
        // A point is admitted when `point.precise == 0` or this is the
        // precise stage, so a precise point never sees the coarse one.
        let admits = |p: usize, point_precise: &[bool], point_section: &[i32]| -> bool {
            if stage != 0 && point_precise[p] {
                return false;
            }
            match only_section {
                Some(s) => point_section[p] == s,
                None => true,
            }
        };

        let cell = eps as f64;
        let key = |q: &RealPoint3d| -> (i64, i64, i64) {
            (
                (q.x as f64 / cell).floor() as i64,
                (q.y as f64 / cell).floor() as i64,
                (q.z as f64 / cell).floor() as i64,
            )
        };
        let eps2 = (eps as f64) * (eps as f64);
        let mut grid: HashMap<(i64, i64, i64), Vec<u32>> = HashMap::new();

        for p in 0..positions.len() {
            if !alive[p] || !admits(p, point_precise, point_section) {
                continue;
            }
            let (kx, ky, kz) = key(&positions[p]);
            let mut found = None;
            'search: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        let Some(bucket) = grid.get(&(kx + dx, ky + dy, kz + dz)) else { continue };
                        for &q in bucket {
                            if !alive[q as usize] {
                                continue;
                            }
                            let a = positions[q as usize];
                            let b = positions[p];
                            let d = (a.x as f64 - b.x as f64).powi(2)
                                + (a.y as f64 - b.y as f64).powi(2)
                                + (a.z as f64 - b.z as f64).powi(2);
                            // Strict: the binary rejects on
                            // `position_epsilon_squared <= d2`.
                            if d >= eps2 {
                                continue;
                            }
                            let ra = &vertices[members[q as usize][0] as usize];
                            let rb = &vertices[members[p][0] as usize];
                            if !skinning_compatible(&ra.influences, &rb.influences, node_weight) {
                                continue;
                            }
                            found = Some(q as usize);
                            break 'search;
                        }
                    }
                }
            }
            match found {
                Some(q) => {
                    let moved = std::mem::take(&mut members[p]);
                    members[q].extend_from_slice(&moved);
                    alive[p] = false;
                    point_precise[q] |= point_precise[p];
                    if point_section[q] != point_section[p] {
                        // A weld across sections leaves the survivor in
                        // none of them.
                        point_section[q] = -1;
                    }
                    // The survivor takes the mean of everything on it,
                    // which is tool's "welding point positions" stage.
                    let list = &members[q];
                    let inv = 1.0 / list.len() as f64;
                    let (mut x, mut y, mut z) = (0.0f64, 0.0, 0.0);
                    for &i in list {
                        x += vertices[i as usize].position.x as f64 * inv;
                        y += vertices[i as usize].position.y as f64 * inv;
                        z += vertices[i as usize].position.z as f64 * inv;
                    }
                    positions[q] = RealPoint3d { x: x as f32, y: y as f32, z: z as f32 };
                }
                None => grid.entry((kx, ky, kz)).or_default().push(p as u32),
            }
        }
    }
}

/// The equality test, exactly as recovered: Chebyshev on each texcoord
/// channel, an angle on the normal, and nothing else. Position is not
/// compared — it is implied by the shared point.
fn same_vertex(a: &WeldVertex, b: &WeldVertex, tol: &WeldTolerances, cos_limit: f32) -> bool {
    let channels = a.texcoords.len().max(b.texcoords.len());
    for k in 0..channels {
        let t = *tol.texcoord.get(k).unwrap_or(&0.0);
        let zero = RealPoint2d { x: 0.0, y: 0.0 };
        let ua = a.texcoords.get(k).copied().unwrap_or(zero);
        let ub = b.texcoords.get(k).copied().unwrap_or(zero);
        let du = (ua.x - ub.x).abs();
        let dv = (ua.y - ub.y).abs();
        // A zero tolerance means exact equality, which is what channels
        // 2 and 3 always get.
        if du.max(dv) >= t.max(f32::MIN_POSITIVE) {
            return false;
        }
    }
    // Exactly the binary's test: a raw three-component dot product,
    // clamped, against the stored cosine. `0x140135ACE` is three `mulss`
    // and two `addss` with no `sqrtss` and no division, so tool does not
    // normalise here either — it relies on the vectors already being
    // unit, which [`weld`] guarantees on the way in.
    let d = a.normal.i * b.normal.i + a.normal.j * b.normal.j + a.normal.k * b.normal.k;
    d.clamp(-1.0, 1.0) > cos_limit
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(p: [f32; 3], n: [f32; 3], uv: [f32; 2]) -> WeldVertex {
        WeldVertex {
            position: RealPoint3d { x: p[0], y: p[1], z: p[2] },
            normal: RealVector3d { i: n[0], j: n[1], k: n[2] },
            texcoords: vec![RealPoint2d { x: uv[0], y: uv[1] }],
            influences: vec![(0, 1.0)],
            color: None,
        }
    }

    #[test]
    fn identical_vertices_weld_to_one() {
        let a = v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let input = vec![a.clone(), a.clone(), a];
        let w = weld(&input, &WeldTolerances::precise());
        assert_eq!(w.vertices.len(), 1);
        assert_eq!(w.remap, vec![0, 0, 0]);
        assert_eq!(w.points, 1);
    }

    #[test]
    fn a_uv_seam_shares_a_point_but_not_a_vertex() {
        // Same place, same normal, different texcoords — this is what a
        // UV seam is, and it must NOT weld, or the texture tears.
        let a = v([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [0.0, 0.0]);
        let b = v([1.0, 2.0, 3.0], [0.0, 0.0, 1.0], [1.0, 0.0]);
        let w = weld(&[a, b], &WeldTolerances::precise());
        assert_eq!(w.points, 1, "one position, so one point");
        assert_eq!(w.vertices.len(), 2, "two texcoords, so two vertices");
        assert_eq!(w.point_of, vec![0, 0]);
    }

    #[test]
    fn a_hard_edge_shares_a_point_but_not_a_vertex() {
        // Same place, same uv, normals 90 degrees apart.
        let a = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let b = v([0.0; 3], [1.0, 0.0, 0.0], [0.5, 0.5]);
        let w = weld(&[a, b], &WeldTolerances::precise());
        assert_eq!(w.points, 1);
        assert_eq!(w.vertices.len(), 2);
    }

    #[test]
    fn normals_inside_one_degree_weld_and_outside_do_not() {
        let base = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        for (deg, expect) in [(0.5f32, 1usize), (0.9, 1), (1.5, 2), (5.0, 2)] {
            let r = deg.to_radians();
            let tilted = v([0.0; 3], [r.sin(), 0.0, r.cos()], [0.5, 0.5]);
            let w = weld(&[base.clone(), tilted], &WeldTolerances::precise());
            assert_eq!(w.vertices.len(), expect, "{deg} degrees");
        }
    }

    #[test]
    fn texcoords_use_chebyshev_not_euclidean() {
        // Both components just under the tolerance: Chebyshev accepts,
        // Euclidean distance (which would be tol*sqrt(2)) would not.
        let t = TEXCOORD_TOLERANCE * 0.9;
        let a = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let b = v([0.0; 3], [0.0, 0.0, 1.0], [0.5 + t, 0.5 + t]);
        let w = weld(&[a, b], &WeldTolerances::precise());
        assert_eq!(w.vertices.len(), 1, "L-infinity, so both under tol is a match");
    }

    #[test]
    fn positions_within_tolerance_share_a_point() {
        let tol = WeldTolerances::precise();
        let d = tol.position * 0.5;
        let a = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let b = v([d, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let w = weld(&[a, b], &tol);
        assert_eq!(w.points, 1);
        assert_eq!(w.vertices.len(), 1);
    }

    #[test]
    fn positions_outside_tolerance_do_not() {
        let tol = WeldTolerances::precise();
        let d = tol.position * 4.0;
        let a = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let b = v([d, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let w = weld(&[a, b], &tol);
        assert_eq!(w.points, 2);
        assert_eq!(w.vertices.len(), 2);
    }

    #[test]
    fn nothing_is_averaged() {
        // The survivor keeps its own values verbatim; a merged-in vertex
        // must not shift it.
        let a = v([0.0; 3], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let t = TEXCOORD_TOLERANCE * 0.5;
        let b = v([0.0; 3], [0.0, 0.0, 1.0], [0.5 + t, 0.5]);
        let w = weld(&[a.clone(), b], &WeldTolerances::precise());
        assert_eq!(w.vertices.len(), 1);
        assert_eq!(w.vertices[0], a, "the first vertex survives unchanged");
    }

    #[test]
    fn a_cube_soup_welds_to_twenty_four_vertices_and_eight_points() {
        // Six faces, two triangles each, unshared: 36 input vertices.
        // Each corner is shared by three faces with different normals,
        // so 8 points and 24 vertices — the textbook answer, and the
        // one that proves points and vertices are different things.
        let mut input = Vec::new();
        let corners = [
            [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0],
        ];
        let faces: [([usize; 4], [f32; 3]); 6] = [
            ([0, 3, 2, 1], [0.0, 0.0, -1.0]),
            ([4, 5, 6, 7], [0.0, 0.0, 1.0]),
            ([0, 1, 5, 4], [0.0, -1.0, 0.0]),
            ([2, 3, 7, 6], [0.0, 1.0, 0.0]),
            ([1, 2, 6, 5], [1.0, 0.0, 0.0]),
            ([3, 0, 4, 7], [-1.0, 0.0, 0.0]),
        ];
        for (quad, normal) in faces {
            for tri in [[0usize, 1, 2], [0, 2, 3]] {
                for k in tri {
                    input.push(v(corners[quad[k]], normal, [0.5, 0.5]));
                }
            }
        }
        assert_eq!(input.len(), 36);
        let w = weld(&input, &WeldTolerances::precise());
        assert_eq!(w.points, 8, "eight distinct positions");
        assert_eq!(w.vertices.len(), 24, "three normals at each corner");
        // And the remap must cover every input.
        assert!(w.remap.iter().all(|r| (*r as usize) < w.vertices.len()));
    }

    #[test]
    fn the_render_model_tolerance_scales_with_the_model_but_is_clamped() {
        // The scaling one is the *coarse* tolerance; the precise one is
        // the same 1/32768 whatever the model.
        assert_eq!(
            WeldTolerances::for_render_model(1.0).position,
            PRECISE_POSITION_TOLERANCE,
            "the precise tolerance does not scale"
        );

        // Tiny model: clamped up to 1/16384.
        assert_eq!(WeldTolerances::for_render_model(0.001).coarse_position, 1.0 / 16384.0);
        // Huge model: clamped down to 1/1024.
        assert_eq!(WeldTolerances::for_render_model(10_000.0).coarse_position, 1.0 / 1024.0);
        // The unclamped band is narrow: extent/512 only lands between
        // the clamps for extents in [0.03125, 0.5] world units. Anything
        // bigger than half a world unit — which is nearly every model —
        // sits on the 1/1024 ceiling.
        let mid = WeldTolerances::for_render_model(0.25).coarse_position;
        assert!((mid - 0.25 / 512.0).abs() < 1e-9, "got {mid}");
        assert_eq!(
            WeldTolerances::for_render_model(1.0).coarse_position,
            1.0 / 1024.0,
            "a 1-world-unit model is already on the ceiling"
        );
    }

    /// Normals that are not unit length still weld by their angle.
    ///
    /// A raw dot product scales by the lengths, so two identical
    /// directions written slightly short used to read as far apart and
    /// refuse to weld — silently costing vertices, and with them strip
    /// length. Shipped content happens to be unit to within 7.6e-5, so
    /// nothing caught this.
    #[test]
    fn short_normals_weld_by_angle_not_by_length() {
        let tol = WeldTolerances::precise();
        // The same direction, one of them 0.1% short: at a 1 degree
        // tolerance the raw dot product is 0.999 against a limit of
        // 0.999848, so this used to split.
        let a = v([0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.5, 0.5]);
        let b = v([0.0, 0.0, 0.0], [0.0, 0.0, 0.999], [0.5, 0.5]);
        assert_eq!(weld(&[a, b], &tol).vertices.len(), 1, "same direction, different length");

        // And a real angle still splits, whatever the lengths.
        let c = v([0.0, 0.0, 0.0], [0.0, 0.0, 2.0], [0.5, 0.5]);
        let d = v([0.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.5, 0.5]);
        assert_eq!(weld(&[c, d], &tol).vertices.len(), 2, "90 degrees apart");
    }
}
