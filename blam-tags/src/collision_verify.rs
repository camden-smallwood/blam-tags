//! `collision_bsp_test` — does the tree agree with the surfaces it indexes?
//!
//! A wrong collision BSP is the one defect in this whole importer that
//! cannot be seen by looking at the output. The tag loads, the model
//! appears, and shots pass through a wall or stop in mid-air. Everything
//! else here is checkable by eye or by a count; this is not, so it needs
//! an instrument.
//!
//! # What it compares
//!
//! A `collision_model` carries two independent descriptions of the same
//! geometry:
//!
//! * the **surfaces** — polygons, reachable by walking each surface's
//!   edge ring through the winged-edge structure;
//! * the **tree** — bsp3d nodes over planes, ending in leaves that index
//!   those surfaces through bsp2d references.
//!
//! The surfaces are the ground truth: they are the collision geometry,
//! and they are read without consulting the tree at all. The tree is what
//! is under test. Casting a ray at the polygons directly and casting the
//! same ray through the tree must produce the same hit — if the tree
//! routes a ray to the wrong leaf, or to a leaf that does not index the
//! surface the ray actually strikes, the two answers diverge.
//!
//! That is a real independence: a builder bug cannot hide, because
//! nothing in the truth side comes from the tree.
//!
//! # It runs on a tag, which is the point
//!
//! Taking a `TagFile` rather than the builder's own structures means it
//! can be pointed at **tool's** shipped collision models. Those must
//! pass. A verifier that fails them is a broken verifier, not a
//! discovery, and having that control available is worth more than the
//! convenience of checking an in-memory tree.

use crate::TagFile;

/// How many surfaces to aim a ray at, per BSP.
///
/// The truth side scans every surface, so this bounds the check at
/// `SURFACE_SAMPLE * surfaces` polygon tests rather than `surfaces`
/// squared.
/// How near a polygon's rim counts as clipping it, in world units.
///
/// A millimetre. Vertices are f32 and level coordinates reach a few
/// hundred, where the spacing between representable values is around
/// 3e-5 and a plane test accumulates a few times that; inside a
/// millimetre the side a ray falls on is decided by rounding, not by the
/// tree. Outside it, a disagreement is a real one.
const GRAZE: f32 = 1e-3;

const SURFACE_SAMPLE: usize = 250;

/// A surface, ready to intersect.
#[derive(Clone, Copy)]
struct Poly {
    first: u32,
    count: u32,
    normal: [f32; 3],
    d: f32,
    lo: [f32; 3],
    hi: [f32; 3],
}

/// What a ray found.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hit {
    t: f32,
    surface: u32,
}

/// One BSP's worth of decoded collision.
struct Decoded {
    vertices: Vec<[f32; 3]>,
    /// `(start, end, forward, reverse, left surface, right surface)`
    edges: Vec<(i32, i32, i32, i32, i32, i32)>,
    /// Each surface's vertex ring, in order.
    rings: Vec<Vec<u32>>,
    /// The rings' positions, flattened, so a polygon test never
    /// allocates: surface `i` owns `flat[polys[i].first ..][.. count]`.
    flat: Vec<[f32; 3]>,
    /// Per surface: `(first, count, normal, d, lo, hi)`.
    polys: Vec<Poly>,
    planes: Vec<[f32; 4]>,
    /// Packed `bsp3d_node` values.
    nodes: Vec<i64>,
    /// Surfaces each leaf indexes, through its bsp2d references.
    leaf_surfaces: Vec<Vec<u32>>,
    /// Deepest root-to-leaf path, which sets the march resolution.
    max_depth: usize,
}

/// Why a model could not be tested.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyError {
    /// The tag has no collision geometry to check.
    Empty,
    /// A block the check needs is missing or unreadable.
    Malformed(String),
    /// The tag has surfaces and a node array, but the tree in it does not
    /// reach a single leaf — so nothing can collide with any of it.
    ///
    /// This is a finding about the *tag*, not a limit of the check.
    /// `baboon_converted/halo2_mcc/bird_quadwing` is one: six surfaces,
    /// six leaves, and a `bsp3d nodes` array that is entirely zero, so
    /// every node claims plane 0 with both children pointing at node 0.
    NoUsableTree { nodes: usize, surfaces: usize },
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "no collision geometry in this tag"),
            Self::Malformed(m) => write!(f, "cannot read the collision structures: {m}"),
            Self::NoUsableTree { nodes, surfaces } => write!(
                f,
                "the bsp3d tree reaches no leaf: {nodes} nodes over {surfaces} surfaces,                  so none of this collision can be hit"
            ),
        }
    }
}

impl std::error::Error for VerifyError {}

/// What the check found.
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    pub bsps: usize,
    pub surfaces: usize,
    /// Rays cast in total.
    pub rays: usize,
    /// Rays where the tree and the surfaces agreed.
    pub agreed: usize,
    /// The tree found nothing where the surfaces were hit.
    pub missed: usize,
    /// The tree found a different surface than the nearest one.
    pub wrong_surface: usize,
    /// The tree reported a hit where the surfaces had none.
    pub phantom: usize,
    /// Surfaces a ray at their own centre could not reach through the
    /// tree — collision that is present in the list but unreachable.
    pub unreachable_surfaces: usize,
    /// Deepest root-to-leaf path.
    pub max_depth: usize,
    /// Leaves, and how many surface references they hold between them —
    /// a tree whose leaves index nothing cannot find anything.
    pub leaves: usize,
    pub leaf_refs: usize,
    /// Rings that did not close, so their polygon is not the surface.
    pub open_rings: usize,
    /// Distinct planes the tree carries.
    pub planes: usize,
    /// Of the missed rays, how many struck within a thousandth of the
    /// model's size of a polygon edge — grazing, where the tree may
    /// legitimately have filed that sliver with the neighbour.
    pub missed_grazing: usize,
    /// Disagreements where the nearer surface is clipped at its very
    /// rim. Counted apart because at that margin the answer is decided
    /// by f32 noise, not by the tree.
    pub wrong_surface_grazing: usize,
    /// The largest margin among disagreements that were *not* grazes —
    /// the size of any real defect left.
    pub worst_solid_margin: f32,
    /// The first few disagreements, for a human.
    pub examples: Vec<String>,
}

impl VerifyReport {
    /// Did every ray agree?
    pub fn clean(&self) -> bool {
        self.missed == 0
            && self.wrong_surface == 0
            && self.phantom == 0
            && self.unreachable_surfaces == 0
    }
}

/// Check every BSP in a `collision_model`.
///
/// `rays_per_bsp` random rays are cast in addition to one per surface;
/// pass 0 for the per-surface check alone.
pub fn test_collision_model(
    tag: &TagFile,
    rays_per_bsp: usize,
) -> Result<VerifyReport, VerifyError> {
    let mut report = VerifyReport::default();
    let root = tag.root();
    let regions = root
        .field_path("regions")
        .and_then(|f| f.as_block())
        .ok_or_else(|| VerifyError::Malformed("regions".into()))?;

    for region in regions.iter() {
        let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
            continue;
        };
        for perm in perms.iter() {
            let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
            for bsp_el in bsps.iter() {
                let Some(bsp) = bsp_el.descend("bsp") else { continue };
                check_one_bsp(&bsp, rays_per_bsp, &mut report)?;
            }
        }
    }

    if report.bsps == 0 {
        return Err(VerifyError::Empty);
    }
    Ok(report)
}


/// Check one BSP-shaped struct.
///
/// A `collision_model` wraps each in a `bsp` struct under
/// `regions/permutations/bsps`; a `scenario_structure_bsp` keeps the
/// sealed world at `resource interface/raw_resources/raw_items/collision
/// bsp` with the same blocks directly on the element. Same structure,
/// same check.
/// How far each surface's written ring strays from its own plane.
///
/// The checker tests the polygon it walks out of the edge rings. If that
/// walk reconstructs a different polygon than the builder placed — a
/// mis-linked edge on a merged ring, say — the checker would be firing
/// at geometry the tree was never told about, and the disagreement would
/// look like a tree defect. Returns the worst deviation and how many
/// surfaces are off by more than a millimetre.
pub fn ring_planarity(bsp: &crate::TagStruct<'_>) -> Option<(f32, usize, usize)> {
    let d = decode(bsp)?;
    let mut worst = 0.0f32;
    let mut bad = 0usize;
    let mut worst_ring = 0usize;
    for si in 0..d.polys.len() {
        let poly = d.polys[si];
        if poly.count < 3 {
            continue;
        }
        let nrm = poly.normal;
        if dot(nrm, nrm) <= 0.0 {
            continue;
        }
        let base = poly.first as usize;
        let first = d.flat[base];
        let plane_d = dot(nrm, first);
        let mut off = 0.0f32;
        for k in 1..poly.count as usize {
            off = off.max((dot(nrm, d.flat[base + k]) - plane_d).abs());
        }
        worst = worst.max(off);
        if off > 1e-3 {
            bad += 1;
            worst_ring = worst_ring.max(poly.count as usize);
        }
    }
    Some((worst, bad, worst_ring))
}

/// Each surface's polygon as the checker walks it out of the tag.
pub fn decoded_rings(bsp: &crate::TagStruct<'_>) -> Option<Vec<Vec<[f32; 3]>>> {
    let d = decode(bsp)?;
    Some(
        (0..d.polys.len())
            .map(|si| {
                let p = d.polys[si];
                (0..p.count as usize).map(|k| d.flat[p.first as usize + k]).collect()
            })
            .collect(),
    )
}

/// The surfaces each leaf of a collision BSP indexes.
pub fn leaf_surface_lists(bsp: &crate::TagStruct<'_>) -> Option<Vec<Vec<u32>>> {
    Some(decode(bsp)?.leaf_surfaces)
}

/// A point inside each leaf of a collision BSP.
///
/// Descends from the root carrying a box, halving it at every plane, and
/// takes the centre of whatever box arrives at a leaf. That point is
/// inside the cell by construction, which a centroid of the leaf's
/// surfaces would not be — a cell is bounded by planes, not by the
/// surfaces it happens to index, and plenty of cells index none at all.
///
/// The box starts at `bounds`, which has to contain the level. Leaves
/// the descent never reaches keep `None`.
pub fn leaf_centres(
    bsp: &crate::TagStruct<'_>,
    bounds: [[f32; 2]; 3],
) -> Option<Vec<Option<[f32; 3]>>> {
    let d = decode(bsp)?;
    let mut out = vec![None; d.leaf_surfaces.len()];

    // Iterative, with an explicit stack: a structure BSP is deep enough
    // that recursion here is a stack overflow waiting for a big level.
    let mut stack = vec![(0i32, bounds, 0usize)];
    while let Some((at, box_, depth)) = stack.pop() {
        if depth > 512 {
            continue;
        }
        if at < 0 {
            if at != i32::MIN {
                let leaf = !at as usize;
                if let Some(slot) = out.get_mut(leaf) {
                    if slot.is_none() {
                        *slot = Some([
                            0.5 * (box_[0][0] + box_[0][1]),
                            0.5 * (box_[1][0] + box_[1][1]),
                            0.5 * (box_[2][0] + box_[2][1]),
                        ]);
                    }
                }
            }
            continue;
        }
        let Some(&packed) = d.nodes.get(at as usize) else { continue };
        let (pi, back, front) = children(packed);
        let Some(pl) = d.planes.get(pi as usize) else { continue };

        // Split the box on the axis the plane most nearly faces. An
        // oblique plane does not cut the box into boxes, so this keeps
        // the halves as the tightest axis-aligned boxes that still
        // contain the true cells — the centre stays inside.
        let nrm = [pl[0], pl[1], pl[2]];
        let axis = (0..3)
            .max_by(|&a, &b| nrm[a].abs().total_cmp(&nrm[b].abs()))
            .unwrap_or(0);
        if nrm[axis].abs() <= 0.0 {
            continue;
        }
        // Where the plane crosses that axis through the box centre.
        let mut at_axis = pl[3];
        for k in 0..3 {
            if k != axis {
                at_axis -= nrm[k] * 0.5 * (box_[k][0] + box_[k][1]);
            }
        }
        at_axis /= nrm[axis];
        let cut = at_axis.clamp(box_[axis][0], box_[axis][1]);

        let (mut lo_box, mut hi_box) = (box_, box_);
        lo_box[axis][1] = cut;
        hi_box[axis][0] = cut;
        // Positive side of the plane is the front child.
        let (front_box, back_box) =
            if nrm[axis] > 0.0 { (hi_box, lo_box) } else { (lo_box, hi_box) };
        stack.push((front, front_box, depth + 1));
        stack.push((back, back_box, depth + 1));
    }
    Some(out)
}

/// Leaf references in the bsp3d tree that point past the end of the leaf
/// block.
///
/// A dangling reference is not a near miss: every ray routed into one
/// finds no surfaces at all, so the collision is simply absent there
/// while the tag still looks well formed. Counting them separates a
/// builder that put a surface in the wrong cell from one that wrote a
/// cell that does not exist.
pub fn dangling_leaf_refs(bsp: &crate::TagStruct<'_>) -> Option<(usize, usize, usize)> {
    let d = decode(bsp)?;
    let mut dangling = 0usize;
    let mut worst = 0usize;
    for &packed in &d.nodes {
        let (_, back, front) = children(packed);
        for c in [back, front] {
            if c < 0 && c != i32::MIN {
                let li = !c as usize;
                if li >= d.leaf_surfaces.len() {
                    dangling += 1;
                    worst = worst.max(li);
                }
            }
        }
    }
    Some((dangling, worst, d.leaf_surfaces.len()))
}

pub fn test_collision_bsp(
    bsp: &crate::TagStruct<'_>,
    rays: usize,
) -> Result<VerifyReport, VerifyError> {
    let mut report = VerifyReport::default();
    check_one_bsp(bsp, rays, &mut report)?;
    if report.bsps == 0 {
        return Err(VerifyError::Empty);
    }
    Ok(report)
}

fn check_one_bsp(
    bsp: &crate::TagStruct<'_>,
    rays: usize,
    report: &mut VerifyReport,
) -> Result<(), VerifyError> {
    let Some(d) = decode(bsp) else {
        return Err(VerifyError::Malformed("a bsp block".into()));
    };
    if d.rings.is_empty() || d.nodes.is_empty() {
        return Ok(());
    }
    // A tree that reaches no leaf cannot be tested, and is not the
    // check's failure — say so rather than count every ray as a
    // disagreement.
    if !reaches_a_leaf(&d) {
        return Err(VerifyError::NoUsableTree {
            nodes: d.nodes.len(),
            surfaces: d.rings.len(),
        });
    }
    report.bsps += 1;
    report.surfaces += d.rings.len();
    report.max_depth = report.max_depth.max(d.max_depth);
    report.leaves += d.leaf_surfaces.len();
    report.leaf_refs += d.leaf_surfaces.iter().map(|l| l.len()).sum::<usize>();
    report.open_rings += d.rings.iter().filter(|r| r.len() < 3).count();
    report.planes += d.planes.len();
    check(&d, rays, report);
    Ok(())
}

// ------------------------------------------------------------- decoding

fn decode(bsp: &crate::TagStruct<'_>) -> Option<Decoded> {
    let blk = |name: &str| bsp.field(name).and_then(|f| f.as_block());

    let mut vertices = Vec::new();
    for e in blk("vertices")?.iter() {
        let p = e.read_point3d("point");
        vertices.push([p.x, p.y, p.z]);
    }

    let mut edges = Vec::new();
    for e in blk("edges")?.iter() {
        // These are `short_integer`, and a structure BSP needs more than
        // 32,767 of them — armory alone has 46,707 edges. The index is
        // the low sixteen bits, so it has to be read back unsigned; read
        // as signed, every reference past 32,767 comes out negative and
        // the ring walk stops on its first edge. That is the same
        // convention `index start` uses on the render side. NONE is
        // 0xFFFF, which stays -1.
        let g = |n: &str| {
            let raw = e.read_int_any(n).unwrap_or(-1) as i32;
            let bits = raw as u16;
            if bits == 0xFFFF { -1 } else { bits as i32 }
        };
        edges.push((
            g("start vertex"),
            g("end vertex"),
            g("forward edge"),
            g("reverse edge"),
            g("left surface"),
            g("right surface"),
        ));
    }

    let mut planes = Vec::new();
    for e in blk("planes")?.iter() {
        let p = e.read_plane3d("plane");
        planes.push([p.i, p.j, p.k, p.d]);
    }

    let mut nodes = Vec::new();
    for e in blk("bsp3d nodes")?.iter() {
        nodes.push(e.read_int_any("node data designator").unwrap_or(0) as i64);
    }

    // The rings, walked through the winged edge: an edge's `forward`
    // continues the surface on its left, its `reverse` the one on its
    // right.
    let surfaces = blk("surfaces")?;
    let mut rings: Vec<Vec<u32>> = Vec::new();
    // A surface names its plane with a flag bit saying it faces the
    // plane's back. That, not the ring's winding, is which way the
    // surface looks — and collision is one sided, so it decides whether
    // a ray can hit it at all.
    let mut facing: Vec<(u32, bool)> = Vec::new();
    for (si, sf) in surfaces.iter().enumerate() {
        // A surface names its plane in a signed *word*: fifteen bits of
        // index, bit 15 saying the surface faces the plane's back. The
        // same s15 transport a bsp2d child uses.
        //
        // Masking 31 bits instead read a flipped surface's index as
        // 0x7FFF-something, `planes` had no such entry, and the code
        // below quietly fell back to deriving the normal from the ring's
        // first three corners. For a triangle that is the same plane, so
        // nothing showed. For a merged ring whose leading corners are
        // nearly collinear it is not: it reported 54 of `anchor_point`'s
        // rings sitting up to 4.4 mm off their own plane, when the
        // builder had them within 6 microns.
        let raw = sf.read_int_any("plane").unwrap_or(0) as i64 as u16;
        facing.push(((raw & 0x7FFF) as u32, raw & 0x8000 != 0));
        // Unsigned, for the same reason the edge links are: a
        // structure BSP has more than 32,767 edges, and read as
        // signed this walk stops before it starts.
        let first = {
            let bits = sf.read_int_any("first edge").unwrap_or(-1) as i32 as u16;
            if bits == 0xFFFF { -1 } else { bits as i32 }
        };
        let mut ring = Vec::new();
        let mut at = first;
        for _ in 0..64 {
            if at < 0 || at as usize >= edges.len() {
                break;
            }
            let e = edges[at as usize];
            // The ring runs start->end for the edge's left surface and
            // end->start for its right.
            let (v, next) = if e.4 == si as i32 { (e.0, e.2) } else { (e.1, e.3) };
            if v < 0 || v as usize >= vertices.len() {
                break;
            }
            ring.push(v as u32);
            at = next;
            if at == first {
                break;
            }
        }
        rings.push(ring);
    }

    // Which surfaces each leaf indexes. A bsp2d node's children are
    // either another node or a surface, flagged in the top bit.
    // A bsp2d child is a signed *word*, not a long: `0xFFFF` is none,
    // bit 15 says the low 15 bits are a surface rather than a node.
    // Reading it as a 32-bit value sign-extends the flag and the index
    // comes out as a nonsense 0x7FFF-something.
    let s15 = |v: i128| -> i32 {
        let u = v as i64 as u16;
        if u == 0xFFFF {
            return i32::MIN;
        }
        let idx = (u & 0x7FFF) as i32;
        if u & 0x8000 != 0 {
            !idx
        } else {
            idx
        }
    };
    let mut bsp2d: Vec<(i32, i32)> = Vec::new();
    if let Some(b) = blk("bsp2d nodes") {
        for e in b.iter() {
            bsp2d.push((
                e.read_int_any("left child").map(s15).unwrap_or(i32::MIN),
                e.read_int_any("right child").map(s15).unwrap_or(i32::MIN),
            ));
        }
    }
    let mut refs: Vec<i32> = Vec::new();
    if let Some(b) = blk("bsp2d references") {
        for e in b.iter() {
            refs.push(e.read_int_any("bsp2d node").map(s15).unwrap_or(i32::MIN));
        }
    }

    let mut leaf_surfaces = Vec::new();
    if let Some(b) = blk("leaves") {
        for e in b.iter() {
            let first = e.read_int_any("first bsp2d reference").unwrap_or(-1) as i32;
            let count = e.read_int_any("bsp2d reference count").unwrap_or(0) as i32;
            let mut list = Vec::new();
            for k in 0..count.max(0) {
                let Some(&node) = refs.get((first + k) as usize) else { continue };
                let mut budget = 4096u32;
                collect_2d(&bsp2d, node, &mut list, &mut budget);
            }
            list.sort_unstable();
            list.dedup();
            leaf_surfaces.push(list);
        }
    }

    // Flatten the rings once and precompute each surface's plane and
    // bounds.
    let mut flat: Vec<[f32; 3]> = Vec::new();
    let mut polys: Vec<Poly> = Vec::with_capacity(rings.len());
    for ring in &rings {
        let first = flat.len() as u32;
        for &v in ring {
            flat.push(vertices[v as usize]);
        }
        let count = ring.len() as u32;
        let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
        for k in 0..count as usize {
            let p = flat[first as usize + k];
            for a in 0..3 {
                lo[a] = lo[a].min(p[a]);
                hi[a] = hi[a].max(p[a]);
            }
        }
        let (normal, d) = if count >= 3 {
            let p0 = flat[first as usize];
            // The surface's own plane, flipped when it faces the back.
            let (pi, flipped) = facing[polys.len()];
            match planes.get(pi as usize) {
                Some(pl) => {
                    let s = if flipped { -1.0 } else { 1.0 };
                    let u = [pl[0] * s, pl[1] * s, pl[2] * s];
                    (u, pl[3] * s)
                }
                None => {
                    let p1 = flat[first as usize + 1];
                    let p2 = flat[first as usize + 2];
                    let nn = cross(sub(p1, p0), sub(p2, p0));
                    let l = dot(nn, nn).sqrt();
                    if l > 0.0 {
                        let u = [nn[0] / l, nn[1] / l, nn[2] / l];
                        (u, dot(u, p0))
                    } else {
                        ([0.0; 3], 0.0)
                    }
                }
            }
        } else {
            ([0.0; 3], 0.0)
        };
        polys.push(Poly { first, count, normal, d, lo, hi });
    }

    let mut d =
        Decoded { vertices, edges, rings, flat, polys, planes, nodes, leaf_surfaces, max_depth: 0 };
    d.max_depth = depth_of(&d);
    Some(d)
}

/// Every surface a bsp2d subtree resolves to.
///
/// Bounded by node visits, not depth. This descends both children, so a
/// depth guard alone leaves a mis-decoded cycle costing 2^depth rather
/// than looping — which reads as a hang on a model small enough that
/// nothing else could be slow.
fn collect_2d(nodes: &[(i32, i32)], at: i32, out: &mut Vec<u32>, budget: &mut u32) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    if at == i32::MIN {
        return;
    }
    if at < 0 {
        // A surface: the s15 unpack above turned the flag into `!index`.
        out.push(!at as u32);
        return;
    }
    let Some(&(l, r)) = nodes.get(at as usize) else { return };
    collect_2d(nodes, l, out, budget);
    collect_2d(nodes, r, out, budget);
}

/// A packed `bsp3d_node`'s children: `>= 0` a node, `< 0` a leaf as
/// `!leaf`, or `i32::MIN` for none.
fn children(packed: i64) -> (u32, i32, i32) {
    let v = packed as u64;
    let plane = (v & 0xFFFF) as u32;
    let un = |c: u64| -> i32 {
        if c == 0xFF_FFFF {
            i32::MIN
        } else if c & 0x80_0000 != 0 {
            !((c & 0x7F_FFFF) as i32)
        } else {
            c as i32
        }
    };
    (plane, un((v >> 16) & 0xFF_FFFF), un((v >> 40) & 0xFF_FFFF))
}

/// Can the tree get from its root to any leaf at all?
///
/// A node whose children point back at itself — which is what an
/// all-zero node array decodes to — leaves every descent going nowhere.
fn reaches_a_leaf(d: &Decoded) -> bool {
    let mut seen = vec![false; d.nodes.len()];
    let mut stack = vec![0i32];
    while let Some(at) = stack.pop() {
        if at == i32::MIN {
            continue;
        }
        if at < 0 {
            return true;
        }
        let idx = at as usize;
        if idx >= d.nodes.len() || seen[idx] {
            continue;
        }
        seen[idx] = true;
        let (_, back, front) = children(d.nodes[idx]);
        stack.push(back);
        stack.push(front);
    }
    false
}

/// Deepest root-to-leaf path.
///
/// Memoised, because this descends both children: a depth guard alone
/// makes a mis-decoded cycle cost 2^depth, which is a hang rather than a
/// wrong answer. Caching each node's depth makes it linear whatever the
/// bytes say.
fn depth_of(d: &Decoded) -> usize {
    let mut memo: Vec<u32> = vec![u32::MAX; d.nodes.len()];
    let mut stack: Vec<(usize, bool)> = vec![(0, false)];
    while let Some((at, expanded)) = stack.pop() {
        if at >= d.nodes.len() {
            continue;
        }
        let (_, back, front) = children(d.nodes[at]);
        if !expanded {
            if memo[at] != u32::MAX {
                continue;
            }
            memo[at] = 0; // marks in-progress, so a cycle cannot loop
            stack.push((at, true));
            for c in [back, front] {
                if c >= 0 && (c as usize) < d.nodes.len() && memo[c as usize] == u32::MAX {
                    stack.push((c as usize, false));
                }
            }
        } else {
            let at_depth = |c: i32| -> u32 {
                if c >= 0 && (c as usize) < d.nodes.len() {
                    memo[c as usize]
                } else {
                    0
                }
            };
            memo[at] = 1 + at_depth(back).max(at_depth(front));
        }
    }
    memo.first().copied().unwrap_or(0) as usize
}

// -------------------------------------------------------------- the test

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Where a ray meets a polygon, if it does.
///
/// The polygon is planar by construction, so this is a plane hit
/// followed by an inside test against every edge — no triangulation,
/// which would need a winding this does not want to assume.
fn ray_polygon(d: &Decoded, si: usize, o: [f32; 3], dir: [f32; 3]) -> Option<f32> {
    let poly = *d.polys.get(si)?;
    if poly.count < 3 {
        return None;
    }
    let n = poly.normal;
    let denom = dot(n, dir);
    // One sided. A ray arriving at the back of a surface passes through
    // it — which is also why the tree need not route to it, so counting
    // such a hit as truth would make a correct tree look wrong.
    if denom > -1e-12 {
        return None;
    }
    let t = (poly.d - dot(n, o)) / denom;
    if t < 1e-5 {
        return None;
    }
    let x = [o[0] + dir[0] * t, o[1] + dir[1] * t, o[2] + dir[2] * t];
    // Cheap reject before the edge walk. The slack absorbs the float
    // error in landing exactly on a boundary vertex.
    for a in 0..3 {
        let slack = (poly.hi[a] - poly.lo[a]).abs() * 1e-4 + 1e-5;
        if x[a] < poly.lo[a] - slack || x[a] > poly.hi[a] + slack {
            return None;
        }
    }
    // Inside if it stays on the same side of every edge.
    let base = poly.first as usize;
    let count = poly.count as usize;
    let mut sign = 0.0f32;
    for k in 0..count {
        let a = d.flat[base + k];
        let b = d.flat[base + (k + 1) % count];
        let s = dot(cross(sub(b, a), sub(x, a)), n);
        if s.abs() < 1e-9 {
            continue;
        }
        if sign == 0.0 {
            sign = s.signum();
        } else if s.signum() != sign {
            return None;
        }
    }
    Some(t)
}

/// The nearest surface the ray meets, over every surface — no tree.
fn brute_force(d: &Decoded, o: [f32; 3], dir: [f32; 3]) -> Option<Hit> {
    let mut best: Option<Hit> = None;
    for si in 0..d.rings.len() {
        if let Some(t) = ray_polygon(d, si, o, dir) {
            if best.is_none_or(|h| t < h.t) {
                best = Some(Hit { t, surface: si as u32 });
            }
        }
    }
    best
}

thread_local! {

    /// The candidate set from the last `through_tree`, so a
    /// disagreement can report whether the tree offered the surface at
    /// all. Diagnostic only.
    static LAST_CANDIDATES: std::cell::RefCell<Vec<u32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The nearest surface the ray meets, out of the candidates the tree
/// gives it.
///
/// The candidate set is gathered by **marching**: step along the ray,
/// descend each point to its cell, and union what those cells index. A
/// cell the ray passes through cannot be missed, because the question is
/// asked of the points themselves.
///
/// This replaced an interval-splitting walk that had a hole in it — for a
/// ray that struck a surface, the walk reported three leaves and no empty
/// cells while a point descent of the hit itself landed in an empty one.
/// Splitting is the faster shape and worth returning to, but only against
/// this as a reference.
///
/// The candidate set is the thing under test. If the tree routes a ray
/// past the surface it is about to hit, that surface never enters the set
/// and the answer differs from a scan of every surface.
fn through_tree(d: &Decoded, o: [f32; 3], dir: [f32; 3], reach: f32) -> Option<Hit> {
    // Resolution has to beat the thinnest cell, and cells get thinner
    // with depth: every extra level can halve one. A fixed 1024 stepped
    // straight over cells in the 51-deep material-sample models and lost
    // 34 rays in 188; scaling with depth is what makes the check
    // resolution-independent rather than tuned to one corpus.
    let mut candidates: Vec<u32> = Vec::new();
    visit_cells(d, 0, o, dir, 0.0, reach, &mut |leaf| {
        if let Some(list) = d.leaf_surfaces.get(leaf) {
            candidates.extend_from_slice(list);
        }
    });
    candidates.sort_unstable();
    candidates.dedup();

    let mut best: Option<Hit> = None;
    for si in candidates {
        if let Some(t) = ray_polygon(d, si as usize, o, dir) {
            if t <= reach && best.is_none_or(|h| t < h.t) {
                best = Some(Hit { t, surface: si });
            }
        }
    }
    best
}

/// The leaf a point lands in, walking the same tree the ray does.
///
/// Used only to explain a disagreement: it separates "the descent went
/// to the wrong leaf" from "the descent was right but that leaf does not
/// index the surface".
/// Every cell the ray segment truly passes through.
///
/// Sampling the ray at a stride cannot do this. The builder registers a
/// surface in the cell a point 1e-4 behind its face descends into, and at
/// structure scale the tree is subdivided finely enough that such a cell
/// is thinner than any practical step — a march over `armory` steps
/// 0.0028 at a time and jumps clean over the cell that holds the answer.
/// Raising the step count 4x moved the agreement 1.6 points and would
/// have needed 74 million samples to close, which is the shape of a
/// method that is wrong rather than merely slow.
///
/// So this splits the segment at the exact parameter where it crosses
/// each node's plane and recurses into the near half then the far half,
/// which visits every cell however thin, and is what the runtime does.
fn visit_cells(
    d: &Decoded,
    at: i32,
    o: [f32; 3],
    dir: [f32; 3],
    tmin: f32,
    tmax: f32,
    out: &mut impl FnMut(usize),
) {
    if tmin > tmax {
        return;
    }
    if at < 0 {
        if at != i32::MIN {
            out(!at as usize);
        }
        return;
    }
    let Some(&packed) = d.nodes.get(at as usize) else { return };
    let (pi, back, front) = children(packed);
    let Some(pl) = d.planes.get(pi as usize) else { return };

    let nrm = [pl[0], pl[1], pl[2]];
    let denom = dot(nrm, dir);
    let dist = dot(nrm, o) - pl[3];
    // Which side the segment starts on decides which child is near.
    // Which side the segment starts on. When it starts *exactly* on the
    // plane — which is every far half of a split, by construction — the
    // position says nothing and the direction decides: a ray with denom
    // > 0 is heading into the front. Reading the degenerate zero as
    // "front" instead sends every far half back into the front subtree,
    // and the whole back side of the tree is never visited.
    let at_min = dist + denom * tmin;
    let side = if at_min != 0.0 { at_min > 0.0 } else { denom > 0.0 };
    let (near, far) = if side { (front, back) } else { (back, front) };

    if denom == 0.0 {
        visit_cells(d, near, o, dir, tmin, tmax, out);
        return;
    }
    let t = -dist / denom;
    if t <= tmin || t >= tmax {
        // The crossing is outside the segment: one side only.
        visit_cells(d, near, o, dir, tmin, tmax, out);
    } else {
        visit_cells(d, near, o, dir, tmin, t, out);
        visit_cells(d, far, o, dir, t, tmax, out);
    }
}

fn descend_point(d: &Decoded, x: [f32; 3]) -> i32 {
    let mut at = 0i32;
    for _ in 0..512 {
        if at < 0 {
            return at;
        }
        let Some(&packed) = d.nodes.get(at as usize) else { return i32::MIN };
        let (pi, back, front) = children(packed);
        let Some(pl) = d.planes.get(pi as usize) else { return i32::MIN };
        let dist = dot([pl[0], pl[1], pl[2]], x) - pl[3];
        at = if dist >= 0.0 { front } else { back };
    }
    i32::MIN
}


/// How far the point is inside its polygon, as a fraction of the model.
///
/// A ray that clips a surface's very edge is ambiguous: the polygon test
/// says hit, and the tree may legitimately have filed that sliver of
/// space with the neighbour. Reporting the margin is what distinguishes
/// "the tree is wrong" from "this ray grazed".
fn edge_margin(d: &Decoded, si: usize, x: [f32; 3], extent: f32) -> f32 {
    let Some(poly) = d.polys.get(si) else { return 0.0 };
    let (base, count) = (poly.first as usize, poly.count as usize);
    if count < 3 {
        return 0.0;
    }
    let mut nearest = f32::MAX;
    for k in 0..count {
        let a = d.flat[base + k];
        let b = d.flat[base + (k + 1) % count];
        let ab = sub(b, a);
        let len = dot(ab, ab).sqrt();
        if len <= 0.0 {
            continue;
        }
        let t = (dot(sub(x, a), ab) / (len * len)).clamp(0.0, 1.0);
        let proj = [a[0] + ab[0] * t, a[1] + ab[1] * t, a[2] + ab[2] * t];
        let dv = sub(x, proj);
        nearest = nearest.min(dot(dv, dv).sqrt());
    }
    let _ = extent;
    // Absolute, in world units. As a fraction of the model this was
    // meaningless across scales: a thousandth of a two-unit weapon is two
    // millimetres and genuinely ambiguous, while a thousandth of a
    // 150-unit level is fifteen centimetres and nowhere near an edge.
    // The question being asked is whether f32 could have decided the
    // side, and that is an absolute distance.
    if nearest == f32::MAX { 0.0 } else { nearest }
}

fn check(d: &Decoded, rays: usize, report: &mut VerifyReport) {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in &d.vertices {
        for a in 0..3 {
            lo[a] = lo[a].min(v[a]);
            hi[a] = hi[a].max(v[a]);
        }
    }
    let extent = (0..3).fold(0.0f32, |m, a| m.max(hi[a] - lo[a])).max(1e-4);
    let centre = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    // How far a ray is followed: every ray here starts outside and is
    // aimed across the model.
    let reach = extent * 5.0;

    // One ray per surface, fired at its own centre from outside along
    // its normal. If the tree cannot deliver a ray to the surface it is
    // aimed at, that collision is not there.
    //
    // Sampled with a stride on big models: the ground-truth side is a
    // scan of every surface, so testing all of them is quadratic and
    // `stanchion` alone would be 289 million polygon tests. A stride
    // covers the model evenly and is deterministic, so a failure is
    // reproducible.
    let stride = (d.rings.len() / SURFACE_SAMPLE).max(1);
    for si in (0..d.rings.len()).step_by(stride) {
        let poly = d.polys[si];
        if poly.count < 3 {
            continue;
        }
        let base = poly.first as usize;
        let count = poly.count as usize;
        let mut c = [0.0f32; 3];
        for k in 0..count {
            let q = d.flat[base + k];
            for a in 0..3 {
                c[a] += q[a] / count as f32;
            }
        }
        let n = poly.normal;
        if dot(n, n) <= 0.0 {
            continue;
        }
        let back = extent * 2.0;
        let o = [c[0] + n[0] * back, c[1] + n[1] * back, c[2] + n[2] * back];
        let dir = [-n[0], -n[1], -n[2]];

        report.rays += 1;
        let truth = brute_force(d, o, dir);
        let tree = through_tree(d, o, dir, reach);
        match (truth, tree) {
            (Some(a), Some(b)) => {
                if (a.t - b.t).abs() <= 1e-3 * extent.max(1.0) {
                    report.agreed += 1;
                } else {
                    report.wrong_surface += 1;
                    let mut graze = false;
                    {
                        let m = edge_margin(
                            d,
                            a.surface as usize,
                            [o[0] + dir[0] * a.t, o[1] + dir[1] * a.t, o[2] + dir[2] * a.t],
                            extent,
                        );
                        if m < GRAZE {
                            report.wrong_surface_grazing += 1;
                        } else {
                            report.worst_solid_margin = report.worst_solid_margin.max(m);
                        }
                        graze = m < GRAZE;
                    }
                    // Only the non-grazing ones are worth an example: a
                    // rim clip is ambiguous by nature and tool's own
                    // trees show it too, so it would crowd out the real
                    // defects.
                    if !graze && report.examples.len() < 8 {
                        // The tree finds the surface aimed at but misses
                        // a nearer one. Same discriminator as a miss: is
                        // the surface the tree skipped in any leaf at
                        // all, was it offered, and does the cell just
                        // past where the ray crosses it know about it?
                        let anywhere = d
                            .leaf_surfaces
                            .iter()
                            .filter(|l| l.contains(&a.surface))
                            .count();
                        let offered =
                            LAST_CANDIDATES.with(|c| c.borrow().contains(&a.surface));
                        let step = 1e-4f32;
                        let probe = |m: f32| {
                            let q = [
                                o[0] + dir[0] * (a.t + m),
                                o[1] + dir[1] * (a.t + m),
                                o[2] + dir[2] * (a.t + m),
                            ];
                            let c = descend_point(d, q);
                            if c < 0 && c != i32::MIN {
                                let li = !c as usize;
                                (
                                    li as i64,
                                    d.leaf_surfaces
                                        .get(li)
                                        .map(|l| l.contains(&a.surface))
                                        .unwrap_or(false),
                                )
                            } else {
                                (-1, false)
                            }
                        };
                        let (lb, kb) = probe(-step);
                        let (la, ka) = probe(step);
                        let margin = edge_margin(
                            d,
                            a.surface as usize,
                            [
                                o[0] + dir[0] * a.t,
                                o[1] + dir[1] * a.t,
                                o[2] + dir[2] * a.t,
                            ],
                            extent,
                        );
                        report.examples.push(format!(
                            "surface {si}: surfaces say {} at {:.5}, tree says {} at {:.5};                              missed one is in {anywhere} leaves, offered={offered},                              margin={margin:.6}, cell before={lb} knows={kb},                              cell after={la} knows={ka}",
                            a.surface, a.t, b.surface, b.t
                        ));
                    }
                }
            }
            (Some(a), None) => {
                report.missed += 1;
                report.unreachable_surfaces += 1;
                let x = [o[0] + dir[0] * a.t, o[1] + dir[1] * a.t, o[2] + dir[2] * a.t];
                if edge_margin(d, a.surface as usize, x, extent) < 1e-3 {
                    report.missed_grazing += 1;
                }
                if report.examples.len() < 8 {
                    // Where does the hit point itself land, and does that
                    // leaf know about the surface?
                    let x = [
                        o[0] + dir[0] * a.t,
                        o[1] + dir[1] * a.t,
                        o[2] + dir[2] * a.t,
                    ];
                    let leaf = descend_point(d, x);
                    let knows = if leaf < 0 && leaf != i32::MIN {
                        d.leaf_surfaces
                            .get(!leaf as usize)
                            .map(|l| l.contains(&a.surface))
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    let offered = LAST_CANDIDATES
                        .with(|c| c.borrow().contains(&a.surface));
                    let n_cand = LAST_CANDIDATES.with(|c| c.borrow().len());

                    // The discriminator: is the surface in *any* leaf?
                    // "no leaf has it" is the builder losing it; "some
                    // leaf has it but not this one" is the tree routing
                    // the ray past it. They need different fixes.
                    let anywhere = d
                        .leaf_surfaces
                        .iter()
                        .filter(|l| l.contains(&a.surface))
                        .count();
                    report.examples.push(format!(
                        "surface {si}: truth {} at {:.5}; indexed by {anywhere} leaves;                          the tree offered {n_cand}                          candidates {} include it; the hit point lands in leaf {} which                          {} index it",
                        a.surface,
                        a.t,
                        if offered { "which DO" } else { "which do NOT" },
                        if leaf == i32::MIN { -1 } else { !leaf },
                        if knows { "does" } else { "does NOT" }
                    ));
                }
            }
            (None, Some(b)) => {
                report.phantom += 1;
                if report.examples.len() < 8 {
                    report.examples
                        .push(format!("surface {si}: tree invented {} at {:.5}", b.surface, b.t));
                }
            }
            (None, None) => report.agreed += 1,
        }
    }

    // And rays from every direction, to catch a tree that happens to work
    // for face-on shots. Deterministic, so a failure can be reproduced.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f32 / (1u64 << 53) as f32
    };
    for _ in 0..rays {
        let dir = {
            let z = next() * 2.0 - 1.0;
            let a = next() * std::f32::consts::TAU;
            let r = (1.0 - z * z).max(0.0).sqrt();
            [r * a.cos(), r * a.sin(), z]
        };
        let o = [
            centre[0] - dir[0] * extent * 2.0 + (next() - 0.5) * extent,
            centre[1] - dir[1] * extent * 2.0 + (next() - 0.5) * extent,
            centre[2] - dir[2] * extent * 2.0 + (next() - 0.5) * extent,
        ];
        report.rays += 1;
        let truth = brute_force(d, o, dir);
        let tree = through_tree(d, o, dir, reach);
        match (truth, tree) {
            (Some(a), Some(b)) => {
                if (a.t - b.t).abs() <= 1e-3 * extent.max(1.0) {
                    report.agreed += 1;
                } else {
                    report.wrong_surface += 1;
                    {
                        let m = edge_margin(
                            d,
                            a.surface as usize,
                            [o[0] + dir[0] * a.t, o[1] + dir[1] * a.t, o[2] + dir[2] * a.t],
                            extent,
                        );
                        if m < GRAZE {
                            report.wrong_surface_grazing += 1;
                        } else {
                            report.worst_solid_margin = report.worst_solid_margin.max(m);
                        }
                    }
                    if report.examples.len() < 8 {
                        report.examples.push(format!(
                            "ray: surfaces say {} at {:.5}, tree says {} at {:.5}",
                            a.surface, a.t, b.surface, b.t
                        ));
                    }
                }
            }
            (Some(a), None) => {
                report.missed += 1;
                let x = [o[0] + dir[0] * a.t, o[1] + dir[1] * a.t, o[2] + dir[2] * a.t];
                let margin = edge_margin(d, a.surface as usize, x, extent);
                if margin < 1e-3 {
                    report.missed_grazing += 1;
                }
                if report.examples.len() < 8 {
                    report.examples.push(format!(
                        "ray: surfaces say {} at {:.5}, tree found nothing (edge margin {margin:.2e})",
                        a.surface, a.t
                    ));
                }
            }
            (None, Some(_)) => report.phantom += 1,
            (None, None) => report.agreed += 1,
        }
    }
    let _ = &d.edges;
}
