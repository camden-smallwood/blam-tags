//! Triangle stripification.
//!
//! Halo 3 render meshes are **triangle strips, exclusively** — 313 of
//! 313 meshes sampled across shipped `render_model` tags carry
//! `index buffer type = 5 (triangle strip)`, and not one is a list. So
//! this is on the required path, not an optimisation.
//!
//! It is also what decides whether a mesh fits. A mesh gets at most
//! **65,535 indices**, and the reachable vertex count is
//! `65,535 / (indices per vertex)`. Measured over 505 shipped meshes that
//! ratio is 1.52 at the tenth percentile, 1.77 median and 2.21 at the
//! ninetieth — so a good strip reaches ~37,000 vertices and a bad one
//! ~29,000. Emitting a triangle *list* as a degenerate strip costs about
//! six indices per vertex and caps a mesh near **10,900** — a two-thirds
//! loss of budget, which is why this is a real greedy stripifier rather
//! than the three-line version.
//!
//! # The algorithm
//!
//! Greedy sequential: pick the unused triangle with the fewest unused
//! neighbours, walk as far as adjacency allows alternating winding, then
//! start again. Separate strips are stitched into one index run with
//! degenerate triangles, which is how a single strip can carry a whole
//! mesh.
//!
//! Winding is preserved throughout. A strip alternates orientation by
//! construction, so joining two strips needs one or two degenerate
//! indices depending on parity — getting that wrong flips every other
//! triangle inside out, which is why there is a test that reconstructs
//! the triangle set and compares it to the input.

use std::collections::{HashMap, HashSet};

/// Turn triangles into one strip index run.
///
/// Returns indices to be drawn as a single `TRIANGLESTRIP`. Degenerate
/// triangles (two equal indices) separate the runs and draw nothing.
/// # How good is it
///
/// Against tool on **byte-identical source geometry** — the JMS out of
/// each shipped tag's own `info` stream, 157 models — this writes
/// **109.7%** of tool's indices, a median of 1.131x per model. Per source
/// triangle that is **2.018 against tool's 1.826**.
///
/// It was 3.683 indices per triangle, which is worse than emitting every
/// triangle as its own three. A strip of L triangles costs L+2 indices
/// plus two or three to bridge to the next, so six per triangle is a
/// strip of exactly one — and that is what the walk produced whenever a
/// seed's free neighbours were off an edge it never presented, or lay
/// behind a seed it only ever walked away from.
///
/// On clean topology the walk is close to optimal: **1.07** indices per
/// triangle on an open grid and **1.031** on a closed tube, against a
/// theoretical 1.0. So what is left is not the walk being naive.
///
/// # Where the remaining 10% is, and it is mostly not here
///
/// Two different things, and only one of them is this file's:
///
/// * At the median the walk is about 10% behind tool. Closing that needs
///   a global method — tunneling, or something else that can undo an
///   early choice — rather than a better greedy rule.
/// * On the tail it is [`crate::weld`]. A strip cannot cross a split
///   vertex, so vertices decide the ceiling on strip length before the
///   walk gets a say. This welder keeps a median of **0.888x** tool's
///   vertex count — welding harder — but its p90 is 1.000x and its worst
///   is **1.529x**. `reach_flak_cannon` is that worst case: tool welds it
///   to 10,231 vertices and strips it at 1.006 indices per triangle,
///   essentially one perfect strip per part; this welds the same geometry
///   to 15,405 and then no stripifier could do better than the 2.02 it
///   gets. The models where this importer is furthest behind are the
///   models where it over-splits.
///
/// Re-measure both with `cargo test -p blam-tags --release --test
/// render_import_corpus indices_against_tools_own_output -- --ignored
/// --nocapture`.
pub fn stripify(triangles: &[[u32; 3]]) -> Vec<u32> {
    let strips = build_strips(triangles);
    stitch(&strips)
}

/// The individual strips, before stitching.
fn build_strips(triangles: &[[u32; 3]]) -> Vec<Vec<u32>> {
    if triangles.is_empty() {
        return Vec::new();
    }

    // Directed edge -> triangles carrying it in that direction. Matching
    // on direction is what preserves winding; an undirected map picks up
    // neighbours wound the other way and flips them.
    let mut directed: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
    for (i, t) in triangles.iter().enumerate() {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            directed.entry((a, b)).or_default().push(i);
        }
    }

    /// The vertex of `t` that is neither `a` nor `b`, if there is exactly
    /// one. `None` for a degenerate triangle.
    fn third(t: &[u32; 3], a: u32, b: u32) -> Option<u32> {
        let mut found = None;
        for &v in t {
            if v != a && v != b {
                if found.is_some() {
                    return None; // two spare vertices: not this edge
                }
                found = Some(v);
            }
        }
        found
    }

    // Which triangles touch which, so "most constrained" can be kept
    // live as the mesh is consumed.
    let mut neighbours: Vec<Vec<usize>> = vec![Vec::new(); triangles.len()];
    {
        let mut undirected: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        for (i, t) in triangles.iter().enumerate() {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let k = if a < b { (a, b) } else { (b, a) };
                undirected.entry(k).or_default().push(i);
            }
        }
        for list in undirected.values() {
            for &i in list {
                for &j in list {
                    if i != j && !neighbours[i].contains(&j) {
                        neighbours[i].push(j);
                    }
                }
            }
        }
    }

    let mut used = vec![false; triangles.len()];
    // Unused neighbours, kept current. The static count the order used to
    // be built from says which triangles started constrained, not which
    // are about to be stranded.
    let mut free: Vec<usize> = neighbours.iter().map(|n| n.len()).collect();

    // A triangle has at most three neighbours, so bucketing by that count
    // makes "least constrained first" O(1) instead of a scan per strip.
    // Entries are left stale and skipped on the way out rather than
    // removed.
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); 4];
    for (i, &d) in free.iter().enumerate() {
        buckets[d.min(3)].push(i);
    }

    let mut strips: Vec<Vec<u32>> = Vec::new();
    let mut taken: HashSet<usize> = HashSet::new();

    // Walk from the end of `strip`, taking triangles as adjacency allows.
    let grow = |strip: &mut Vec<u32>,
                consumed: &mut Vec<usize>,
                taken: &mut HashSet<usize>,
                used: &[bool]| {
        loop {
            let l = strip.len();
            let (a, b) = (strip[l - 2], strip[l - 1]);
            // Triangle index this extension would create.
            let k = l - 2;
            let want = if k % 2 == 0 { (a, b) } else { (b, a) };
            let Some(list) = directed.get(&want) else { break };
            let mut next = None;
            for &j in list {
                if used[j] || taken.contains(&j) {
                    continue;
                }
                if let Some(c) = third(&triangles[j], a, b) {
                    next = Some((j, c));
                    break;
                }
            }
            let Some((j, c)) = next else { break };
            taken.insert(j);
            consumed.push(j);
            strip.push(c);
        }
    };

    loop {
        // The most constrained unused triangle: the one likeliest to be
        // stranded if the mesh around it is eaten first.
        let mut seed = None;
        for d in 0..4 {
            while let Some(&i) = buckets[d].last() {
                if used[i] || free[i].min(3) != d {
                    buckets[d].pop();
                    continue;
                }
                seed = Some(i);
                break;
            }
            if seed.is_some() {
                break;
            }
        }
        let Some(seed) = seed else { break };
        let t = triangles[seed];

        // A strip grows off its last edge only, so which of the seed's
        // three edges it presents decides how far it reaches. Rotations
        // preserve winding, so all three are legal starts; walk each both
        // ways and keep whichever takes the most triangles.
        let mut best: Option<(Vec<u32>, Vec<usize>)> = None;
        for rot in 0..3 {
            let mut strip = vec![t[rot], t[(rot + 1) % 3], t[(rot + 2) % 3]];
            let mut consumed = vec![seed];
            taken.clear();
            taken.insert(seed);

            grow(&mut strip, &mut consumed, &mut taken, &used);

            // Now the other way. Prepending would flip every triangle
            // after it, so reverse instead — which preserves winding
            // exactly when the index count is even, and needs one
            // duplicated index on the front when it is odd.
            //
            // That duplicate is only worth paying for if there is
            // something back there: a strip with nothing behind it would
            // otherwise turn a lone triangle into four indices.
            let reached = consumed.len();
            let forward_only = strip.clone();
            strip.reverse();
            if strip.len() % 2 != 0 {
                strip.insert(0, strip[0]);
            }
            grow(&mut strip, &mut consumed, &mut taken, &used);
            if consumed.len() == reached {
                strip = forward_only;
            }

            if best.as_ref().is_none_or(|(bs, bc)| {
                (consumed.len(), std::cmp::Reverse(strip.len()))
                    > (bc.len(), std::cmp::Reverse(bs.len()))
            }) {
                best = Some((strip, consumed));
            }
        }

        let (strip, consumed) = best.expect("a seed always has three rotations");
        for &j in &consumed {
            used[j] = true;
            for &k in &neighbours[j] {
                if !used[k] {
                    free[k] -= 1;
                    buckets[free[k].min(3)].push(k);
                }
            }
        }
        strips.push(strip);
    }
    strips
}

/// Join strips into one run using degenerate triangles.
///
/// A strip alternates winding with every index, so the next strip has to
/// land on an even triangle index or all of it is flipped. Two degenerate
/// indices bridge the gap and a third is added when the parity needs it.
fn stitch(strips: &[Vec<u32>]) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for strip in strips {
        if strip.len() < 3 {
            continue;
        }
        if out.is_empty() {
            out.extend_from_slice(strip);
            continue;
        }
        let last = *out.last().expect("non-empty");
        out.push(last);
        out.push(strip[0]);
        // After the bridge, the next strip's first real triangle sits at
        // index `out.len()`. It must be even.
        if out.len() % 2 != 0 {
            out.push(strip[0]);
        }
        out.extend_from_slice(strip);
    }
    out
}

/// Reconstruct the triangles a strip draws, skipping degenerates.
///
/// This is the inverse of [`stripify`] and exists so the round trip can
/// be asserted: a stripifier that silently drops or flips triangles is
/// the kind of defect that shows up as holes in a model, not as an error.
pub fn destripify(indices: &[u32]) -> Vec<[u32; 3]> {
    let mut out = Vec::new();
    for i in 0..indices.len().saturating_sub(2) {
        let (a, b, c) = (indices[i], indices[i + 1], indices[i + 2]);
        if a == b || b == c || a == c {
            continue; // degenerate: draws nothing
        }
        // Odd triangles in a strip have reversed winding.
        if i % 2 == 0 {
            out.push([a, b, c]);
        } else {
            out.push([a, c, b]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A triangle as an orientation-independent-but-winding-preserving
    /// key: the cyclic rotation starting at the smallest index.
    fn canon(t: [u32; 3]) -> [u32; 3] {
        let m = t.iter().copied().enumerate().min_by_key(|(_, v)| *v).map(|(i, _)| i).unwrap();
        [t[m], t[(m + 1) % 3], t[(m + 2) % 3]]
    }

    fn assert_round_trip(tris: &[[u32; 3]]) {
        let strip = stripify(tris);
        let back = destripify(&strip);
        let want: HashSet<[u32; 3]> = tris.iter().map(|t| canon(*t)).collect();
        let got: HashSet<[u32; 3]> = back.iter().map(|t| canon(*t)).collect();
        assert_eq!(
            got, want,
            "strip does not draw the same triangles\n  strip = {strip:?}\n  back  = {back:?}"
        );
        assert_eq!(back.len(), tris.len(), "triangle count changed");
    }

    #[test]
    fn one_triangle_is_three_indices() {
        let t = [[0u32, 1, 2]];
        assert_eq!(stripify(&t), vec![0, 1, 2]);
        assert_round_trip(&t);
    }

    #[test]
    fn a_quad_strips_to_four_indices() {
        // Two triangles sharing an edge is the case a strip exists for.
        let t = [[0u32, 1, 2], [2, 1, 3]];
        let s = stripify(&t);
        assert_eq!(s.len(), 4, "a quad is four indices, got {s:?}");
        assert_round_trip(&t);
    }

    #[test]
    fn winding_survives_the_round_trip() {
        // Not just the triangle *set* — the winding of each one. A strip
        // alternates, so an off-by-one in the parity flips every other
        // face and the model renders inside out in patches.
        let t = [[0u32, 1, 2], [2, 1, 3], [2, 3, 4], [4, 3, 5]];
        let strip = stripify(&t);
        let back = destripify(&strip);
        let want: HashSet<[u32; 3]> = t.iter().map(|x| canon(*x)).collect();
        for tri in &back {
            assert!(want.contains(&canon(*tri)), "wound the wrong way: {tri:?} from {strip:?}");
        }
    }

    #[test]
    fn disconnected_triangles_still_round_trip() {
        // Nothing shares an edge, so every triangle is its own strip and
        // the stitching does all the work.
        let t: Vec<[u32; 3]> = (0..8).map(|i| [i * 3, i * 3 + 1, i * 3 + 2]).collect();
        assert_round_trip(&t);
    }

    #[test]
    fn a_grid_strips_efficiently() {
        // The case that matters for the index budget: a regular grid
        // should approach one index per triangle, not three.
        let n = 32u32;
        let mut tris = Vec::new();
        for j in 0..n - 1 {
            for i in 0..n - 1 {
                let a = j * n + i;
                let (b, c, d) = (a + 1, a + n, a + n + 1);
                tris.push([a, c, b]);
                tris.push([b, c, d]);
            }
        }
        assert_round_trip(&tris);
        let strip = stripify(&tris);
        let per_tri = strip.len() as f64 / tris.len() as f64;
        // A perfect strip is 1.0 + epsilon; a degenerate-stitched list is
        // 5.0. Anything under 2 keeps us inside the shipped ratio band.
        assert!(per_tri < 2.0, "{per_tri:.2} indices per triangle — too many");
        eprintln!("grid: {} triangles, {} indices, {per_tri:.2}/tri", tris.len(), strip.len());
    }

    #[test]
    fn a_closed_cube_round_trips() {
        let quads: [[u32; 4]; 6] = [
            [0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4],
            [2, 3, 7, 6], [1, 2, 6, 5], [3, 0, 4, 7],
        ];
        let mut tris = Vec::new();
        for q in quads {
            tris.push([q[0], q[1], q[2]]);
            tris.push([q[0], q[2], q[3]]);
        }
        assert_round_trip(&tris);
    }

    #[test]
    fn an_empty_mesh_is_an_empty_strip() {
        assert!(stripify(&[]).is_empty());
        assert!(destripify(&[]).is_empty());
    }

    #[test]
    fn degenerate_input_triangles_do_not_break_it() {
        // A triangle with a repeated index draws nothing, and must not
        // derail the walk.
        let t = [[0u32, 1, 2], [3, 3, 3], [2, 1, 4]];
        let strip = stripify(&t);
        let back = destripify(&strip);
        // The degenerate one is allowed to vanish; the real two are not.
        let got: HashSet<[u32; 3]> = back.iter().map(|x| canon(*x)).collect();
        assert!(got.contains(&canon([0, 1, 2])));
        assert!(got.contains(&canon([2, 1, 4])));
    }

    /// A closed tube — a grid that wraps, which is what a lathed barrel
    /// is — should strip about as well as a flat grid.
    ///
    /// `reach_flak_cannon` is 15,656 triangles at 0.98 vertices each, so
    /// its adjacency is intact, and tool strips it at 1.006 indices per
    /// triangle. This importer manages 2.02 on it while reaching ~1.07 on
    /// an open grid, so the shape of the failure is somewhere between the
    /// two.
    #[test]
    fn a_closed_tube_strips_about_as_well_as_a_grid() {
        // rings around the axis, segments along it
        let (rings, segments) = (32usize, 64usize);
        let vid = |r: usize, s: usize| ((s * rings) + (r % rings)) as u32;
        let mut tris: Vec<[u32; 3]> = Vec::new();
        for s in 0..segments {
            for r in 0..rings {
                let (a, b) = (vid(r, s), vid(r + 1, s));
                let (c, d) = (vid(r, s + 1), vid(r + 1, s + 1));
                tris.push([a, b, d]);
                tris.push([a, d, c]);
            }
        }
        assert_round_trip(&tris);

        let indices = stripify(&tris);
        let ratio = indices.len() as f64 / tris.len() as f64;
        println!(
            "tube: {} triangles -> {} indices, {ratio:.3} per triangle",
            tris.len(),
            indices.len()
        );
        assert!(
            ratio < 1.35,
            "a tube should strip nearly perfectly, got {ratio:.3} indices per triangle"
        );
    }
}
