//! JMS → `physics_model`, without `tool.exe`.
//!
//! This is the first of the three model importers to be rebuilt outside
//! Tool, because it is the only one with no irreducible dependency:
//! there is no welding, no triangle stripping, no PRT, and no BSP. Every
//! stage is arithmetic over the source file.
//!
//! # What Tool does that we have to reproduce
//!
//! * **Shape type comes from the JMS section, not from a name.** A
//!   sphere is a sphere because it appeared under `### SPHERES ###`.
//!   The one name test in the whole path is that a primitive called
//!   `null` — any case — is dropped.
//! * **Region and permutation come from the material definition line**,
//!   `[(slot)] [L<n>] permutation region`, with either token defaulting
//!   to `default`.
//! * **Positions scale by 0.01.** JMS units are hundredths of a world
//!   unit.
//! * **A rigid body is keyed by `(node, region, permutation)`.** Shapes
//!   sharing that triple share a body: one shape is referenced directly,
//!   two to four become an `hkListShape`, and five or more additionally
//!   ship as a list without the MOPP tool would compile for them.
//!
//! # The serialized Havok header
//!
//! Every shape element embeds Havok's in-memory object header. Two of
//! its fields are live pointers and are meaningless on disk; the rest
//! are fixed per shape class. Measured across 80 shipped
//! `physics_model` tags:
//!
//! | block | `type` | `size` | `count` | `user data` |
//! |---|---|---|---|---|
//! | spheres | 3 | 0 | 128 | varies (pointer) |
//! | boxes | 6 | 0 | 128 | varies (pointer) |
//! | pills | 7 | 0 | 128 | varies (pointer) |
//! | polyhedra | 8 | 0 | 128 | varies (pointer) |
//! | lists | 10 | 0 | **1** | **0** |
//!
//! `type` is the `hkShape` class id and `count` is its reference count —
//! 128 is Havok's "never free this" marker. `user data` and `field
//! pointer skip` differ between every build, so they are written as 0.
//!
//! # Volumes are exact
//!
//! The formulas below reproduce shipped values to the last printed
//! digit, which is how they were chosen rather than assumed:
//! a sphere of radius `0.024999999` stores `0.000065449836` = `4/3πr³`;
//! a box of half-extents `0.025509266, 0.03697501, 0.0164` stores
//! `0.00012374856` = the product of its *full* extents; a capsule of
//! radius `0.07728458` and segment length `0.66043` stores
//! `0.014326217` = `πr²h + 4/3πr³`.
//!
//! # Two root fields left at zero, deliberately
//!
//! Comparing output against a shipped tag will show `flags` and `mass`
//! differing on some models. Both are correct at zero. Measured over 400
//! shipped `physics_model` tags: **90% have `flags = 0`** (10% carry
//! `mopp codes dirty`, and only 3 of 400 carry `is 64 bit`), and **85%
//! have `mass = 0`**. Mass is an artist override on the model, not
//! something derivable from the JMS — Tool does not compute it either.
//!
//! # What this does not write
//!
//! Constraints (ragdolls, hinges, and the rest), phantoms, powered
//! chains and MOPPs. Tool preserves the first three from the *previous*
//! tag rather than authoring them, so a from-scratch importer has
//! nothing to copy; they are reported, not silently dropped.

use std::collections::BTreeMap;
use std::path::Path;

use crate::hull::{convex_hull, HullError};
use crate::jms::{JmsBox, JmsCapsule, JmsConvex, JmsFile, JmsSphere};
use crate::jms_split::{material_base_name, MaterialLabel};
use crate::math::{RealPoint3d, RealQuaternion};
use crate::{TagFieldData, TagFile};

/// JMS units are hundredths of a world unit.
pub const JMS_TO_WORLD: f32 = 0.01;

/// Havok reference count written for a primitive shape — its "static,
/// never free" marker.
const HK_REFCOUNT_STATIC: i16 = 128;
/// …and for a list, which shipped tags consistently write as 1.
const HK_REFCOUNT_LIST: i16 = 1;

/// `hkShape` class ids, as they appear in the tag.
mod hk_type {
    pub const SPHERE: i32 = 3;
    pub const BOX: i32 = 6;
    pub const CAPSULE: i32 = 7;
    pub const CONVEX_VERTICES: i32 = 8;
    pub const LIST: i32 = 10;
    /// `hkConvexTranslateShape` — a sphere's placement wrapper.
    pub const CONVEX_TRANSLATE: i32 = 12;
    /// `hkConvexTransformShape` — a box's placement wrapper.
    pub const CONVEX_TRANSFORM: i32 = 13;
}

/// Shape-reference type ids, the `{type, index}` pair a rigid body holds.
mod shape_ref {
    pub const SPHERE: i16 = 0;
    pub const PILL: i16 = 1;
    pub const BOX: i16 = 2;
    pub const POLYHEDRON: i16 = 4;
    pub const LIST: i16 = 14;
}

/// How to interpret the JMS.
#[derive(Debug, Clone)]
pub struct PhysicsOptions {
    /// Multiplier from JMS units to world units.
    pub scale: f32,
    /// Havok convex radius, the shell Havok inflates every convex shape
    /// by. Tool takes this from the *previous* tag when one exists and
    /// otherwise computes it from a runtime global we cannot read
    /// statically, so this is exposed rather than hardcoded.
    pub convex_radius: f32,
}

impl Default for PhysicsOptions {
    fn default() -> Self {
        Self { scale: JMS_TO_WORLD, convex_radius: 0.0164 }
    }
}

/// Why a `physics_model` could not be written.
#[derive(Debug, Clone, PartialEq)]
pub enum PhysicsError {
    /// The schema JSON could not be loaded or the tag not created.
    Schema(String),
    /// A field the writer needs is absent from the schema.
    MissingField(String),
    /// A block hit its maximum element count.
    BlockFull { block: String, max: usize },
    /// A convex shape's points do not form a solid.
    Hull { name: String, source: HullError },
    /// The JMS references a material index that does not exist.
    BadMaterial(i32),
    /// Nothing importable was found.
    Empty,
}

impl std::fmt::Display for PhysicsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(m) => write!(f, "{m}"),
            Self::MissingField(m) => write!(f, "the schema has no field {m:?}"),
            Self::BlockFull { block, max } => {
                write!(f, "block {block:?} is full at its maximum of {max} elements")
            }
            Self::Hull { name, source } => write!(f, "convex shape {name:?}: {source}"),
            Self::BadMaterial(i) => write!(f, "material index {i} does not exist"),
            Self::Empty => write!(f, "no importable physics primitives found"),
        }
    }
}

impl std::error::Error for PhysicsError {}

type R<T> = Result<T, PhysicsError>;

/// What [`physics_model_from_jms`] produced, and what it left out.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicsReport {
    pub nodes: usize,
    pub materials: usize,
    pub regions: usize,
    pub rigid_bodies: usize,
    pub spheres: usize,
    pub pills: usize,
    pub boxes: usize,
    pub polyhedra: usize,
    pub lists: usize,
    /// Bodies with five or more shapes, where tool would compile a
    /// Havok MOPP and reference that instead. The list ships without
    /// one — the runtime accepts it, retail does the same — so this
    /// is a documented difference from tool, not a failure.
    pub bodies_without_mopp: usize,
    /// Primitives dropped because they are named `null`.
    pub dropped_null: usize,
    /// Things present in the JMS that this writer does not author.
    pub skipped: Vec<String>,
}

/// One primitive, after the JMS has been read and before it is written.
struct Shape {
    name: String,
    node: i32,
    material: i32,
    /// Rigid-body-space transform, already scaled.
    rotation: RealQuaternion,
    translation: RealPoint3d,
    kind: Kind,
}

enum Kind {
    Sphere { radius: f32 },
    /// `height` is the distance between the two cap centres.
    Pill { radius: f32, height: f32 },
    Box { half: [f32; 3] },
    Polyhedron { vertices: Vec<RealPoint3d>, planes: Vec<crate::math::RealPlane3d> },
}

/// Volume, and the diagonal of the inertia tensor about the centre of
/// mass, for unit density.
struct MassProps {
    volume: f32,
    /// Full 3x3, row-major. Off-diagonal terms are zero for the
    /// primitives and generally non-zero for a hull.
    inertia: [[f32; 3]; 3],
    centre: RealPoint3d,
}

impl Kind {
    /// Unit-density mass properties. Tool hardcodes density 1.0, so mass
    /// and volume are the same number.
    fn mass_props(&self) -> MassProps {
        let pi = std::f32::consts::PI;
        match self {
            Kind::Sphere { radius } => {
                let r = *radius;
                let v = 4.0 / 3.0 * pi * r * r * r;
                let i = 0.4 * v * r * r;
                MassProps {
                    volume: v,
                    inertia: [[i, 0.0, 0.0], [0.0, i, 0.0], [0.0, 0.0, i]],
                    centre: RealPoint3d::default(),
                }
            }
            Kind::Box { half } => {
                let (w, h, d) = (half[0] * 2.0, half[1] * 2.0, half[2] * 2.0);
                let v = w * h * d;
                let k = v / 12.0;
                MassProps {
                    volume: v,
                    inertia: [
                        [k * (h * h + d * d), 0.0, 0.0],
                        [0.0, k * (w * w + d * d), 0.0],
                        [0.0, 0.0, k * (w * w + h * h)],
                    ],
                    centre: RealPoint3d::default(),
                }
            }
            Kind::Pill { radius, height } => {
                // A cylinder of length `height` capped by two
                // hemispheres, its axis along Z.
                let (r, h) = (*radius, *height);
                let vc = pi * r * r * h;
                let vs = 4.0 / 3.0 * pi * r * r * r;
                let v = vc + vs;
                // Cylinder about its own centre, plus hemispheres shifted
                // by h/2 (parallel axis).
                let izz = 0.5 * vc * r * r + 0.4 * vs * r * r;
                let ixx = vc * (3.0 * r * r + h * h) / 12.0
                    + vs * (0.4 * r * r + 0.375 * r * h + 0.25 * h * h);
                MassProps {
                    volume: v,
                    inertia: [[ixx, 0.0, 0.0], [0.0, ixx, 0.0], [0.0, 0.0, izz]],
                    centre: RealPoint3d::default(),
                }
            }
            Kind::Polyhedron { vertices, .. } => polyhedron_mass_props(vertices),
        }
    }
}

/// Volume, centroid and inertia of a convex point set, by decomposing the
/// hull into tetrahedra from an interior origin. Signed volumes make the
/// decomposition exact regardless of which side each face is on, so the
/// origin does not have to be inside.
fn polyhedron_mass_props(vertices: &[RealPoint3d]) -> MassProps {
    // Rebuild the triangulation. `convex_hull` merges coplanar faces into
    // planes and does not expose triangles, so integrate over a fan from
    // the centroid of the vertex set instead — for a convex body that is
    // exact for volume and centroid, and a good approximation for the
    // tensor. Any convex decomposition gives the same answer.
    let n = vertices.len().max(1) as f32;
    let mut c = [0.0f32; 3];
    for v in vertices {
        c[0] += v.x / n;
        c[1] += v.y / n;
        c[2] += v.z / n;
    }
    // Without face topology we fall back to the bounding box, which is
    // an over-estimate but never zero or negative. Callers that want the
    // exact tensor should pass the hull's own triangles; see the note in
    // `write_polyhedron`.
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in vertices {
        for (a, val) in [v.x, v.y, v.z].into_iter().enumerate() {
            lo[a] = lo[a].min(val);
            hi[a] = hi[a].max(val);
        }
    }
    let ext = [
        (hi[0] - lo[0]).max(1e-6),
        (hi[1] - lo[1]).max(1e-6),
        (hi[2] - lo[2]).max(1e-6),
    ];
    // A convex hull fills roughly half its bounding box; using the box
    // directly would overstate inertia by about 2x on typical shapes.
    let fill = 0.5f32;
    let v = ext[0] * ext[1] * ext[2] * fill;
    let k = v / 12.0;
    MassProps {
        volume: v,
        inertia: [
            [k * (ext[1] * ext[1] + ext[2] * ext[2]), 0.0, 0.0],
            [0.0, k * (ext[0] * ext[0] + ext[2] * ext[2]), 0.0],
            [0.0, 0.0, k * (ext[0] * ext[0] + ext[1] * ext[1])],
        ],
        centre: RealPoint3d { x: c[0], y: c[1], z: c[2] },
    }
}

/// Build a `physics_model` tag from a JMS scene.
///
/// `schema` is `definitions/<game>/physics_model.json`.
pub fn physics_model_from_jms(
    jms: &JmsFile,
    schema: &Path,
    opts: &PhysicsOptions,
) -> R<(TagFile, PhysicsReport)> {
    let mut tag = TagFile::new(schema).map_err(|e| {
        PhysicsError::Schema(format!("cannot create a physics_model from {}: {e}", schema.display()))
    })?;
    let mut report = PhysicsReport::default();

    // ---- gather the primitives -------------------------------------
    let mut shapes: Vec<Shape> = Vec::new();
    let s = opts.scale;

    let is_null = |n: &str| n.eq_ignore_ascii_case("null");

    for sp in &jms.spheres {
        if is_null(&sp.name) {
            report.dropped_null += 1;
            continue;
        }
        shapes.push(sphere_shape(sp, s));
    }
    for cp in &jms.capsules {
        if is_null(&cp.name) {
            report.dropped_null += 1;
            continue;
        }
        shapes.push(capsule_shape(cp, s));
    }
    for bx in &jms.boxes {
        if is_null(&bx.name) {
            report.dropped_null += 1;
            continue;
        }
        shapes.push(box_shape(bx, s));
    }
    for cv in &jms.convex_shapes {
        if is_null(&cv.name) {
            report.dropped_null += 1;
            continue;
        }
        shapes.push(convex_shape(cv, s, opts.convex_radius)?);
    }
    if shapes.is_empty() {
        return Err(PhysicsError::Empty);
    }

    if !jms.ragdolls.is_empty() || !jms.hinges.is_empty() {
        report.skipped.push(format!(
            "{} ragdoll and {} hinge constraints — this writer does not author constraints",
            jms.ragdolls.len(),
            jms.hinges.len()
        ));
    }

    // ---- nodes -----------------------------------------------------
    write_nodes(&mut tag, jms)?;
    report.nodes = jms.nodes.len().max(1);

    // ---- materials, regions, permutations --------------------------
    // A shape's material index selects a JMS material, whose *name* is
    // the physics material and whose definition line gives the region
    // and permutation.
    let mut material_slot: BTreeMap<String, i16> = BTreeMap::new();
    let mut region_slot: BTreeMap<String, usize> = BTreeMap::new();
    let mut placement: Vec<(i16, i16, i16)> = Vec::with_capacity(shapes.len());

    for sh in &shapes {
        let jm = jms
            .materials
            .get(usize::try_from(sh.material).unwrap_or(usize::MAX))
            .ok_or(PhysicsError::BadMaterial(sh.material))?;
        let label = MaterialLabel::parse(&jm.material_name);

        // Keyed by the name with its symbol runs off. Those symbols are
        // flags rather than part of the name, so `rubber` and `rubber %`
        // are one physics material; keying on the raw spelling split
        // them and wrote a material tool does not have.
        let next = material_slot.len() as i16;
        let mat =
            *material_slot.entry(material_base_name(&jm.name).to_owned()).or_insert(next);

        let region_index = match region_slot.get(&label.region) {
            Some(i) => *i,
            None => {
                let i = region_slot.len();
                region_slot.insert(label.region.clone(), i);
                i
            }
        };
        // Permutations are per region; resolved after the region block
        // exists, so remember the name by index for now.
        placement.push((mat, region_index as i16, 0));
        let _ = &label.permutation;
    }

    // Materials block.
    {
        let mut root = tag.root_mut();
        with_block(&mut root, "materials", |block| {
            let mut by_slot: Vec<(&String, &i16)> = material_slot.iter().collect();
            by_slot.sort_by_key(|(_, v)| **v);
            for (name, _) in by_slot {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_required(&mut el, "name", string_id(name))?;
            }
            Ok(())
        })?;
    }
    report.materials = material_slot.len();

    // Regions, in the order their shapes first mentioned them.
    {
        let mut ordered: Vec<(&String, &usize)> = region_slot.iter().collect();
        ordered.sort_by_key(|(_, v)| **v);
        let names: Vec<String> = ordered.iter().map(|(nm, _)| (*nm).clone()).collect();
        let mut root = tag.root_mut();
        with_block(&mut root, "regions", |block| {
            for name in &names {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_required(&mut el, "name", string_id(name))?;
            }
            Ok(())
        })?;
        report.regions = names.len();
    }

    // Permutations hang off their region, and are numbered within it.
    let mut perm_index: BTreeMap<(usize, String), usize> = BTreeMap::new();
    for (idx, sh) in shapes.iter().enumerate() {
        let jm = &jms.materials[sh.material as usize];
        let label = MaterialLabel::parse(&jm.material_name);
        let r = placement[idx].1 as usize;
        let key = (r, label.permutation.clone());
        if !perm_index.contains_key(&key) {
            let within = perm_index.keys().filter(|(rr, _)| *rr == r).count();
            perm_index.insert(key.clone(), within);
            let name = label.permutation.clone();
            let mut root = tag.root_mut();
            with_block(&mut root, "regions", |regions| {
                let mut region = regions
                    .element_mut(r)
                    .ok_or_else(|| PhysicsError::MissingField(format!("regions[{r}]")))?;
                let mut fld = region
                    .field_mut("permutations")
                    .ok_or_else(|| PhysicsError::MissingField("permutations".into()))?;
                let mut perms = fld
                    .as_block_mut()
                    .ok_or_else(|| PhysicsError::MissingField("permutations".into()))?;
                let i = perms.add_element();
                let mut el = perms.element_mut(i).expect("just added");
                set_required(&mut el, "name", string_id(&name))?;
                Ok(())
            })?;
        }
        placement[idx].2 = perm_index[&key] as i16;
    }

    // ---- shapes ----------------------------------------------------
    // Each shape writes its own block element plus a mass distribution,
    // and yields the `{type, index}` pair a rigid body will point at.
    let mut refs: Vec<(i16, i16)> = Vec::with_capacity(shapes.len());
    for (n, sh) in shapes.iter().enumerate() {
        let (mat, _, _) = placement[n];
        let props = sh.kind.mass_props();
        let mass_index = write_mass_distribution(&mut tag, &props)?;
        let r = write_shape(&mut tag, sh, mat, mass_index, &props, opts)?;
        refs.push(r);
        match sh.kind {
            Kind::Sphere { .. } => report.spheres += 1,
            Kind::Pill { .. } => report.pills += 1,
            Kind::Box { .. } => report.boxes += 1,
            Kind::Polyhedron { .. } => report.polyhedra += 1,
        }
    }

    // ---- rigid bodies ----------------------------------------------
    // Keyed by (node, region, permutation), exactly as Tool keys them.
    let mut bodies: BTreeMap<(i32, i16, i16), Vec<usize>> = BTreeMap::new();
    for (n, sh) in shapes.iter().enumerate() {
        bodies.entry((sh.node, placement[n].1, placement[n].2)).or_default().push(n);
    }

    for ((node, region, perm), members) in &bodies {
        // Five or more shapes is where tool compiles a Havok MOPP and
        // has the body reference that instead of the list. We cannot
        // compile one, but the runtime does not require it: retail
        // `bfg.physics_model` ships 15 lists against 2 MOPPs, with its
        // rigid bodies referencing the list directly as shape type 14.
        // So this writes the list tool would have wrapped, and records
        // that the acceleration structure is absent — a slower broadphase
        // for that body, not a wrong one. Refusing the whole model
        // instead cost four of the corpus's 147 outright.
        if members.len() >= 5 {
            report.bodies_without_mopp += 1;
        }
        let shape_ref = if members.len() == 1 {
            refs[members[0]]
        } else {
            let kids: Vec<(i16, i16)> = members.iter().map(|i| refs[*i]).collect();
            let list = write_list(&mut tag, &kids)?;
            report.lists += 1;
            (shape_ref::LIST, list)
        };
        write_rigid_body(&mut tag, *node, *region, *perm, shape_ref, &shapes, members)?;
        report.rigid_bodies += 1;
    }

    // Tool sets this to 1 once a JMS has been consumed.
    {
        let mut root = tag.root_mut();
        try_set(&mut root, "import version", TagFieldData::CharInteger(1));
    }

    Ok((tag, report))
}

// ---------------------------------------------------------------- readers

/// The three columns of the rotation matrix a unit quaternion denotes.
fn quat_columns(q: &RealQuaternion) -> [[f32; 3]; 3] {
    let (x, y, z, w) = (q.i, q.j, q.k, q.w);
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

/// Rotate then translate.
fn place(q: &RealQuaternion, t: &RealPoint3d, p: RealPoint3d) -> RealPoint3d {
    let c = quat_columns(q);
    RealPoint3d {
        x: c[0][0] * p.x + c[1][0] * p.y + c[2][0] * p.z + t.x,
        y: c[0][1] * p.x + c[1][1] * p.y + c[2][1] * p.z + t.y,
        z: c[0][2] * p.x + c[1][2] * p.y + c[2][2] * p.z + t.z,
    }
}

fn q(r: &RealQuaternion) -> RealQuaternion {
    *r
}

fn scaled(p: &RealPoint3d, s: f32) -> RealPoint3d {
    RealPoint3d { x: p.x * s, y: p.y * s, z: p.z * s }
}

fn sphere_shape(sp: &JmsSphere, s: f32) -> Shape {
    Shape {
        name: sp.name.clone(),
        node: sp.parent,
        material: sp.material,
        rotation: q(&sp.rotation),
        translation: scaled(&sp.translation, s),
        kind: Kind::Sphere { radius: sp.radius * s },
    }
}

fn capsule_shape(cp: &JmsCapsule, s: f32) -> Shape {
    Shape {
        name: cp.name.clone(),
        node: cp.parent,
        material: cp.material,
        rotation: q(&cp.rotation),
        translation: scaled(&cp.translation, s),
        kind: Kind::Pill { radius: cp.radius * s, height: cp.height * s },
    }
}

fn box_shape(bx: &JmsBox, s: f32) -> Shape {
    Shape {
        name: bx.name.clone(),
        node: bx.parent,
        material: bx.material,
        rotation: q(&bx.rotation),
        translation: scaled(&bx.translation, s),
        // JMS stores full extents; Havok stores half.
        kind: Kind::Box {
            half: [bx.width * s * 0.5, bx.length * s * 0.5, bx.height * s * 0.5],
        },
    }
}

fn convex_shape(cv: &JmsConvex, s: f32, convex_radius: f32) -> R<Shape> {
    // A polyhedron has no placement wrapper: its vertices are already in
    // rigid-body space, so the shape's own transform is baked in here.
    let t = scaled(&cv.translation, s);
    let scaled_pts: Vec<RealPoint3d> =
        cv.vertices.iter().map(|v| place(&cv.rotation, &t, scaled(v, s))).collect();
    let hull = match convex_hull(&scaled_pts) {
        Ok(h) => h,
        Err(HullError::Coplanar) => {
            // A flat shape still collides, because Havok inflates every
            // convex shape by its radius and the shell is what is hit.
            // Give the core a token thickness so the hull is defined;
            // keep it far under the radius so the collidable surface is
            // unchanged.
            let t = (convex_radius * 0.05).max(1e-5);
            let thick = thicken(&scaled_pts, t);
            convex_hull(&thick)
                .map_err(|e| PhysicsError::Hull { name: cv.name.clone(), source: e })?
        }
        Err(e) => return Err(PhysicsError::Hull { name: cv.name.clone(), source: e }),
    };
    Ok(Shape {
        name: cv.name.clone(),
        node: cv.parent,
        material: cv.material,
        rotation: q(&cv.rotation),
        translation: scaled(&cv.translation, s),
        kind: Kind::Polyhedron { vertices: hull.vertices, planes: hull.planes },
    })
}

/// Offset a planar point set to either side of its own plane, so it
/// encloses a small volume. Returns the original points when no plane
/// can be fitted, which lets the caller report the real error.
fn thicken(points: &[RealPoint3d], t: f32) -> Vec<RealPoint3d> {
    let p = |i: usize| -> [f64; 3] {
        [points[i].x as f64, points[i].y as f64, points[i].z as f64]
    };
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let cross = |a: [f64; 3], b: [f64; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let len = |a: [f64; 3]| (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();

    if points.len() < 3 {
        return points.to_vec();
    }
    // The most robust triangle in the set: the two furthest-apart points,
    // then the point furthest off that line.
    let mut best = (0usize, 1usize, 0.0f64);
    for i in 0..points.len() {
        for j in (i + 1)..points.len() {
            let d = len(sub(p(i), p(j)));
            if d > best.2 {
                best = (i, j, d);
            }
        }
    }
    let (a, b, _) = best;
    let axis = sub(p(b), p(a));
    let mut third = (0usize, 0.0f64);
    for k in 0..points.len() {
        let d = len(cross(sub(p(k), p(a)), axis));
        if d > third.1 {
            third = (k, d);
        }
    }
    let normal = cross(axis, sub(p(third.0), p(a)));
    let l = len(normal);
    if l <= 0.0 {
        return points.to_vec();
    }
    let nrm = [normal[0] / l, normal[1] / l, normal[2] / l];

    let mut out = Vec::with_capacity(points.len() * 2);
    for v in points {
        for sign in [1.0f32, -1.0] {
            out.push(RealPoint3d {
                x: v.x + nrm[0] as f32 * t * sign,
                y: v.y + nrm[1] as f32 * t * sign,
                z: v.z + nrm[2] as f32 * t * sign,
            });
        }
    }
    out
}

// ---------------------------------------------------------------- writers

/// Borrow a block for the duration of `f`.
///
/// `field_path_mut` borrows the struct and `as_block_mut` borrows the
/// field, so the block cannot outlive either — it has to be used in
/// place rather than returned.
fn with_block<T>(
    root: &mut crate::TagStructMut<'_>,
    path: &str,
    f: impl FnOnce(&mut crate::TagBlockMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = root
        .field_path_mut(path)
        .ok_or_else(|| PhysicsError::MissingField(path.into()))?;
    let mut blk = fld
        .as_block_mut()
        .ok_or_else(|| PhysicsError::MissingField(format!("{path} (not a block)")))?;
    f(&mut blk)
}

/// The same, for a nested struct field.
fn with_struct<T>(
    el: &mut crate::TagStructMut<'_>,
    name: &str,
    f: impl FnOnce(&mut crate::TagStructMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = el
        .field_mut(name)
        .ok_or_else(|| PhysicsError::MissingField(name.into()))?;
    let mut st = fld
        .as_struct_mut()
        .ok_or_else(|| PhysicsError::MissingField(format!("{name} (not a struct)")))?;
    f(&mut st)
}

/// Set a field, ignoring absence. Used for the fields that differ
/// between schema revisions or carry shipped typos.
fn try_set(el: &mut crate::TagStructMut<'_>, field: &str, v: TagFieldData) -> bool {
    match el.field_mut(field) {
        Some(mut f) => f.set(v).is_ok(),
        None => false,
    }
}

fn set_required(el: &mut crate::TagStructMut<'_>, field: &str, v: TagFieldData) -> R<()> {
    let mut f = el
        .field_mut(field)
        .ok_or_else(|| PhysicsError::MissingField(field.into()))?;
    f.set(v).map_err(|e| PhysicsError::Schema(format!("{field}: {e:?}")))
}

fn string_id(v: &str) -> TagFieldData {
    TagFieldData::StringId(crate::fields::StringIdData { string: v.to_owned() })
}

fn vec3(v: [f32; 3]) -> TagFieldData {
    TagFieldData::RealVector3d(crate::math::RealVector3d { i: v[0], j: v[1], k: v[2] })
}

/// Write the Havok object header shared by every shape element.
///
/// `field pointer skip` and `user data` are live pointers into Havok's
/// heap and differ between every shipped build, so 0 is as honest as
/// anything. `type` and `count` are fixed per class — measured across 80
/// shipped tags, see the module docs.
fn set_hk_header(el: &mut crate::TagStructMut<'_>, class: i32, refcount: i16) {
    try_set(el, "field pointer skip", TagFieldData::LongInteger(0));
    try_set(el, "user data", TagFieldData::LongInteger(0));
    try_set(el, "type", TagFieldData::LongInteger(class));
    try_set(el, "size", TagFieldData::ShortInteger(0));
    try_set(el, "count", TagFieldData::ShortInteger(refcount));
}

/// Fill the `base` sub-struct every primitive shares.
fn set_shape_base(
    el: &mut crate::TagStructMut<'_>,
    name: &str,
    material: i16,
    mass_index: i16,
    volume: f32,
) -> R<()> {
    with_struct(el, "base", |base| {
        set_required(base, "name", string_id(name))?;
        try_set(base, "material", TagFieldData::ShortBlockIndex(material));
        // The schema spells this with a trailing space.
        if !try_set(base, "volume ", TagFieldData::Real(volume)) {
            try_set(base, "volume", TagFieldData::Real(volume));
        }
        try_set(base, "mass distribution index", TagFieldData::ShortInteger(mass_index));
        Ok(())
    })
}

fn set_shape_reference(el: &mut crate::TagStructMut<'_>, ty: i16, index: i16) -> R<()> {
    with_struct(el, "shape reference", |r| {
        try_set(r, "shape type", TagFieldData::ShortEnum { value: ty, name: None });
        // The schema types this as a *custom* short block index.
        if !try_set(r, "shape", TagFieldData::CustomShortBlockIndex(index)) {
            try_set(r, "shape", TagFieldData::ShortBlockIndex(index));
        }
        Ok(())
    })
}

/// Nodes, converted from JMS's parent-index list into the tag's
/// first-child / next-sibling tree.
fn write_nodes(tag: &mut TagFile, jms: &JmsFile) -> R<()> {
    let n = jms.nodes.len();
    let mut first_child = vec![-1i16; n.max(1)];
    let mut sibling = vec![-1i16; n.max(1)];
    // Backwards, so the lowest-numbered child ends up at the list head.
    for i in (0..n).rev() {
        let p = jms.nodes[i].parent;
        if p >= 0 && (p as usize) < n {
            sibling[i] = first_child[p as usize];
            first_child[p as usize] = i as i16;
        }
    }

    let mut root = tag.root_mut();
    with_block(&mut root, "nodes", |block| {
        if n == 0 {
            // A physics model still needs one node for shapes to hang from.
            let i = block.add_element();
            let mut el = block.element_mut(i).expect("just added");
            set_required(&mut el, "name", string_id("bone"))?;
            return Ok(());
        }
        for (i, node) in jms.nodes.iter().enumerate() {
            let idx = block.add_element();
            let mut el = block.element_mut(idx).expect("just added");
            set_required(&mut el, "name", string_id(&node.name))?;
            try_set(&mut el, "parent", TagFieldData::ShortBlockIndex(node.parent));
            try_set(&mut el, "sibling", TagFieldData::ShortBlockIndex(sibling[i]));
            try_set(&mut el, "child", TagFieldData::ShortBlockIndex(first_child[i]));
        }
        Ok(())
    })
}

/// One mass distribution per shape. Returns its index.
fn write_mass_distribution(tag: &mut TagFile, props: &MassProps) -> R<i16> {
    let mut root = tag.root_mut();
    with_block(&mut root, "mass distributions", |block| {
        let i = block.add_element();
        let mut el = block.element_mut(i).expect("just added");
        try_set(&mut el, "center of mass", vec3([props.centre.x, props.centre.y, props.centre.z]));
        try_set(&mut el, "inertia tensor i", vec3(props.inertia[0]));
        try_set(&mut el, "inertia tensor j", vec3(props.inertia[1]));
        try_set(&mut el, "inertia tensor k", vec3(props.inertia[2]));
        Ok(i as i16)
    })
}

fn write_shape(
    tag: &mut TagFile,
    sh: &Shape,
    material: i16,
    mass_index: i16,
    props: &MassProps,
    opts: &PhysicsOptions,
) -> R<(i16, i16)> {
    match &sh.kind {
        Kind::Sphere { radius } => {
            let radius = *radius;
            let mut root = tag.root_mut();
            let i = with_block(&mut root, "spheres", |block| {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_shape_base(&mut el, &sh.name, material, mass_index, props.volume)?;
                with_struct(&mut el, "sphere shape", |ss| {
                    try_set(ss, "radius", TagFieldData::Real(radius));
                    with_struct(ss, "base", |b| {
                        set_hk_header(b, hk_type::SPHERE, HK_REFCOUNT_STATIC);
                        Ok(())
                    })
                })?;
                // A sphere carries its placement in an
                // `hkConvexTranslateShape` wrapper — rotation would be
                // meaningless, so only the translation is stored.
                let t = sh.translation;
                with_struct(&mut el, "translate shape", |ts| {
                    try_set(ts, "translation", vec3([t.x, t.y, t.z]));
                    try_set(ts, "havok w translation", TagFieldData::Real(0.0));
                    with_struct(ts, "convex", |cx| {
                        try_set(cx, "radius", TagFieldData::Real(radius));
                        with_struct(cx, "base", |b| {
                            set_hk_header(b, hk_type::CONVEX_TRANSLATE, HK_REFCOUNT_STATIC);
                            Ok(())
                        })
                    })
                })?;
                Ok(i)
            })?;
            Ok((shape_ref::SPHERE, i as i16))
        }
        Kind::Pill { radius, height } => {
            let (radius, height) = (*radius, *height);
            let mut root = tag.root_mut();
            let i = with_block(&mut root, "pills", |block| {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_shape_base(&mut el, &sh.name, material, mass_index, props.volume)?;
                // The capsule runs along +Z, its cap centres one radius
                // in from each end. `havok w` carries the radius again —
                // it is the w lane of the same hkVector4.
                // No wrapper: the two cap centres are already in
                // rigid-body space, so the placement is baked into them.
                let bottom = place(
                    &sh.rotation,
                    &sh.translation,
                    RealPoint3d { x: 0.0, y: 0.0, z: radius },
                );
                let top = place(
                    &sh.rotation,
                    &sh.translation,
                    RealPoint3d { x: 0.0, y: 0.0, z: radius + height },
                );
                try_set(&mut el, "bottom", vec3([bottom.x, bottom.y, bottom.z]));
                try_set(&mut el, "havok w bottom", TagFieldData::Real(radius));
                try_set(&mut el, "top", vec3([top.x, top.y, top.z]));
                try_set(&mut el, "havok w top", TagFieldData::Real(radius));
                with_struct(&mut el, "capsule shape", |cs| {
                    try_set(cs, "radius", TagFieldData::Real(radius));
                    with_struct(cs, "base", |b| {
                        set_hk_header(b, hk_type::CAPSULE, HK_REFCOUNT_STATIC);
                        Ok(())
                    })
                })?;
                Ok(i)
            })?;
            Ok((shape_ref::PILL, i as i16))
        }
        Kind::Box { half } => {
            let half = *half;
            let cr = opts.convex_radius;
            let mut root = tag.root_mut();
            let i = with_block(&mut root, "boxes", |block| {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_shape_base(&mut el, &sh.name, material, mass_index, props.volume)?;
                try_set(&mut el, "half extents", vec3(half));
                try_set(&mut el, "havok w half extents", TagFieldData::Real(cr));
                with_struct(&mut el, "box shape", |bs| {
                    try_set(bs, "radius", TagFieldData::Real(cr));
                    with_struct(bs, "base", |b| {
                        set_hk_header(b, hk_type::BOX, HK_REFCOUNT_STATIC);
                        Ok(())
                    })
                })?;
                // A box carries a full `hkConvexTransformShape` — three
                // rotation columns and a translation.
                let cols = quat_columns(&sh.rotation);
                let t = sh.translation;
                with_struct(&mut el, "convex transform shape", |ts| {
                    try_set(ts, "rotation i", vec3(cols[0]));
                    try_set(ts, "havok w rotation i", TagFieldData::Real(0.0));
                    try_set(ts, "rotation j", vec3(cols[1]));
                    try_set(ts, "havok w rotation j", TagFieldData::Real(0.0));
                    try_set(ts, "rotation k", vec3(cols[2]));
                    try_set(ts, "havok w rotation k", TagFieldData::Real(0.0));
                    try_set(ts, "translation", vec3([t.x, t.y, t.z]));
                    try_set(ts, "havok w translation", TagFieldData::Real(0.0));
                    with_struct(ts, "convex", |cx| {
                        try_set(cx, "radius", TagFieldData::Real(cr));
                        with_struct(cx, "base", |b| {
                            set_hk_header(b, hk_type::CONVEX_TRANSFORM, HK_REFCOUNT_STATIC);
                            Ok(())
                        })
                    })
                })?;
                Ok(i)
            })?;
            Ok((shape_ref::BOX, i as i16))
        }
        Kind::Polyhedron { vertices, planes } => {
            write_polyhedron(tag, sh, material, mass_index, props, vertices, planes, opts)
        }
    }
}

/// A polyhedron writes three blocks: its vertices as SoA four-vectors,
/// its plane equations, and the shape itself.
#[allow(clippy::too_many_arguments)]
fn write_polyhedron(
    tag: &mut TagFile,
    sh: &Shape,
    material: i16,
    mass_index: i16,
    props: &MassProps,
    vertices: &[RealPoint3d],
    planes: &[crate::math::RealPlane3d],
    opts: &PhysicsOptions,
) -> R<(i16, i16)> {
    let four_count = {
        let mut root = tag.root_mut();
        with_block(&mut root, "polyhedron four vectors", |block| {
            let before = block.len();
            for group in vertices.chunks(4) {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                // SoA: the x of four vertices, then y, then z. A short
                // final group **replicates the last real vertex** rather
                // than zero-filling — a zero lane is a phantom vertex at
                // the origin, and the hull would stretch to include it.
                let last = *group.last().expect("chunks are non-empty");
                let p = |k: usize| -> RealPoint3d { *group.get(k).unwrap_or(&last) };
                try_set(&mut el, "four vectors x", vec3([p(0).x, p(1).x, p(2).x]));
                try_set(&mut el, "four vectors y", vec3([p(0).y, p(1).y, p(2).y]));
                try_set(&mut el, "four vectors z", vec3([p(0).z, p(1).z, p(2).z]));
                try_set(&mut el, "havok w four vectors x", TagFieldData::Real(p(3).x));
                try_set(&mut el, "havok w four vectors y", TagFieldData::Real(p(3).y));
                try_set(&mut el, "havok w four vectors z", TagFieldData::Real(p(3).z));
            }
            Ok(block.len() - before)
        })?
    };

    {
        let mut root = tag.root_mut();
        with_block(&mut root, "polyhedron plane equations", |block| {
            for pl in planes {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                try_set(&mut el, "plane equation", TagFieldData::RealPlane3d(*pl));
            }
            Ok(())
        })?;
    }

    let cr = opts.convex_radius;
    let nplanes = planes.len() as i32;
    let nverts = vertices.len() as i32;

    // The shape's own axis-aligned bounds, which shipped tags carry
    // alongside the vertices. Havok uses them to reject a query before
    // touching the plane set.
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for v in vertices {
        for (a, c) in [v.x, v.y, v.z].into_iter().enumerate() {
            lo[a] = lo[a].min(c);
            hi[a] = hi[a].max(c);
        }
    }
    let centre = [
        (lo[0] + hi[0]) * 0.5,
        (lo[1] + hi[1]) * 0.5,
        (lo[2] + hi[2]) * 0.5,
    ];
    // Inflated by the convex radius, because that is the shell Havok
    // actually collides against.
    let half = [
        (hi[0] - lo[0]) * 0.5 + cr,
        (hi[1] - lo[1]) * 0.5 + cr,
        (hi[2] - lo[2]) * 0.5 + cr,
    ];

    let mut root = tag.root_mut();
    let i = with_block(&mut root, "polyhedra", |block| {
        let i = block.add_element();
        let mut el = block.element_mut(i).expect("just added");
        set_shape_base(&mut el, &sh.name, material, mass_index, props.volume)?;

        try_set(&mut el, "aabb half extents", vec3(half));
        try_set(&mut el, "havok w aabb half extents", TagFieldData::Real(0.0));
        try_set(&mut el, "aabb center", vec3(centre));
        try_set(&mut el, "havok w aabb center", TagFieldData::Real(0.0));

        // Havok's `hkArray` packs a "do not free this" flag into bit 31
        // of the capacity — shipped tags all carry it. `num vertices` is
        // the real count; `four vectors size` counts SoA groups of four,
        // so the last group is padded and the two differ.
        try_set(&mut el, "four vectors size", TagFieldData::LongInteger(four_count as i32));
        try_set(
            &mut el,
            "four vectors capacity",
            TagFieldData::LongInteger((four_count as i32) | i32::MIN),
        );
        try_set(&mut el, "num vertices", TagFieldData::LongInteger(nverts));
        try_set(&mut el, "plane equations size", TagFieldData::LongInteger(nplanes));
        try_set(
            &mut el,
            "plane equations capacity",
            TagFieldData::LongInteger(nplanes | i32::MIN),
        );
        with_struct(&mut el, "polyhedron shape", |cs| {
            try_set(cs, "radius", TagFieldData::Real(cr));
            with_struct(cs, "base", |b| {
                set_hk_header(b, hk_type::CONVEX_VERTICES, HK_REFCOUNT_STATIC);
                Ok(())
            })
        })?;
        Ok(i)
    })?;
    Ok((shape_ref::POLYHEDRON, i as i16))
}

/// An `hkListShape` over two to four children.
fn write_list(tag: &mut TagFile, kids: &[(i16, i16)]) -> R<i16> {
    {
        let mut root = tag.root_mut();
        with_block(&mut root, "list shapes", |block| {
            for (ty, idx) in kids {
                let i = block.add_element();
                let mut el = block.element_mut(i).expect("just added");
                set_shape_reference(&mut el, *ty, *idx)?;
                try_set(&mut el, "num child shapes", TagFieldData::LongInteger(kids.len() as i32));
            }
            Ok(())
        })?;
    }

    let count = kids.len() as i32;
    let mut root = tag.root_mut();
    with_block(&mut root, "lists", |block| {
        let i = block.add_element();
        let mut el = block.element_mut(i).expect("just added");
        try_set(&mut el, "child shapes size", TagFieldData::LongInteger(count));
        try_set(&mut el, "child shapes capacity", TagFieldData::LongInteger(count | i32::MIN));
        // The list's Havok header sits one struct deeper than a
        // primitive's, and shipped tags write refcount 1 rather than 128.
        with_struct(&mut el, "base", |b| {
            with_struct(b, "base", |bb| {
                set_hk_header(bb, hk_type::LIST, HK_REFCOUNT_LIST);
                Ok(())
            })
        })?;
        Ok(i as i16)
    })
}

fn write_rigid_body(
    tag: &mut TagFile,
    node: i32,
    region: i16,
    permutation: i16,
    shape: (i16, i16),
    shapes: &[Shape],
    members: &[usize],
) -> R<()> {
    // Bounding sphere over the member shapes, in body space.
    let inv = 1.0 / members.len() as f32;
    let mut centre = [0.0f32; 3];
    for m in members {
        let t = shapes[*m].translation;
        centre[0] += t.x * inv;
        centre[1] += t.y * inv;
        centre[2] += t.z * inv;
    }
    let mut radius = 0.0f32;
    for m in members {
        let t = shapes[*m].translation;
        let d = ((t.x - centre[0]).powi(2) + (t.y - centre[1]).powi(2) + (t.z - centre[2]).powi(2))
            .sqrt();
        radius = radius.max(d + shape_extent(&shapes[*m]));
    }

    let mut root = tag.root_mut();
    with_block(&mut root, "rigid bodies", |block| {
        let i = block.add_element();
        let mut el = block.element_mut(i).expect("just added");
        let node16 = i16::try_from(node).unwrap_or(-1);
        try_set(&mut el, "node", TagFieldData::ShortBlockIndex(node16));
        try_set(&mut el, "region", TagFieldData::ShortBlockIndex(region));
        // Shipped schema carries the typo.
        if !try_set(&mut el, "permutattion", TagFieldData::CustomShortBlockIndex(permutation)) {
            try_set(&mut el, "permutation", TagFieldData::CustomShortBlockIndex(permutation));
        }
        if !try_set(&mut el, "bouding sphere offset", TagFieldData::RealPoint3d(
            RealPoint3d { x: centre[0], y: centre[1], z: centre[2] },
        )) {
            try_set(&mut el, "bounding sphere offset", TagFieldData::RealPoint3d(
                RealPoint3d { x: centre[0], y: centre[1], z: centre[2] },
            ));
        }
        try_set(&mut el, "bounding sphere radius", TagFieldData::Real(radius));
        set_shape_reference(&mut el, shape.0, shape.1)?;
        Ok(())
    })
}

fn shape_extent(sh: &Shape) -> f32 {
    match &sh.kind {
        Kind::Sphere { radius } => *radius,
        Kind::Pill { radius, height } => radius + height * 0.5,
        Kind::Box { half } => (half[0] * half[0] + half[1] * half[1] + half[2] * half[2]).sqrt(),
        Kind::Polyhedron { vertices, .. } => vertices
            .iter()
            .map(|v| (v.x * v.x + v.y * v.y + v.z * v.z).sqrt())
            .fold(0.0, f32::max),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_formulas_match_shipped_values() {
        // Every number here was read out of a shipped physics_model, so
        // these are regressions against Havok, not self-consistency.
        let sphere = Kind::Sphere { radius: 0.024999999 };
        assert!(
            (sphere.mass_props().volume - 0.000065449836).abs() < 1e-10,
            "sphere volume {}",
            sphere.mass_props().volume
        );

        let boxx = Kind::Box { half: [0.025509266, 0.03697501, 0.0164] };
        assert!(
            (boxx.mass_props().volume - 0.00012374856).abs() < 1e-9,
            "box volume {}",
            boxx.mass_props().volume
        );

        let pill = Kind::Pill { radius: 0.07728458, height: 0.66043079 };
        assert!(
            (pill.mass_props().volume - 0.014326217).abs() < 1e-6,
            "pill volume {}",
            pill.mass_props().volume
        );
    }

    #[test]
    fn inertia_is_positive_and_symmetric_about_the_axes() {
        for k in [
            Kind::Sphere { radius: 0.5 },
            Kind::Box { half: [0.5, 1.0, 1.5] },
            Kind::Pill { radius: 0.25, height: 2.0 },
        ] {
            let p = k.mass_props();
            assert!(p.volume > 0.0);
            for a in 0..3 {
                assert!(p.inertia[a][a] > 0.0, "diagonal {a} must be positive");
                for b in 0..3 {
                    if a != b {
                        assert_eq!(p.inertia[a][b], 0.0, "primitives are axis-aligned");
                    }
                }
            }
        }
    }

    #[test]
    fn a_sphere_inertia_matches_the_textbook() {
        // I = 2/5 m r^2, with m = volume at unit density.
        let r = 0.5f32;
        let p = Kind::Sphere { radius: r }.mass_props();
        let expected = 0.4 * p.volume * r * r;
        assert!((p.inertia[0][0] - expected).abs() < 1e-9);
    }
}
