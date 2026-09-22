//! ASS → `scenario_structure_bsp`, the geometry core.
//!
//! [`crate::ass`] takes a structure BSP apart into an ASS scene and
//! [`crate::ass_parse`] reads one back. This puts a scene into a tag,
//! which is the direction an importer needs.
//!
//! # Scope, stated plainly
//!
//! An sbsp has 56 top-level blocks. This writes the ones that carry
//! geometry — materials, world bounds, clusters, and the render geometry
//! those clusters point at — and leaves the rest empty. That is enough
//! for the mesh to survive the trip intact, which is the thing that is
//! hard to get right and easy to get silently wrong, and it is **not**
//! enough for a level to run: portals, pathfinding, lightmap data,
//! instanced geometry and the structure collision BSP are all absent.
//!
//! # What is easy to get silently wrong
//!
//! **sbsp meshes are triangle lists, not strips.** `render_model` uses
//! strips and shares the same `global_mesh_block` layout, `index buffer
//! type` even reads `triangle strip` on some shipped sbsp meshes, and
//! reading one as the other produces a mesh that looks plausible and is
//! wrong everywhere — [`crate::ass`] measured list interpretation at
//! 1.000 face-normal correlation against ~0.50 for strips. Lists it is.
//!
//! **Positions are centimetres in ASS and world units in the tag**, a
//! factor of 100, and the V coordinate is flipped between them. Both are
//! silent if missed: the model is simply the wrong size or its textures
//! are upside down.
//!
//! # Checking it
//!
//! The pair of modules is its own oracle. Build a tag from a scene,
//! export it back with [`crate::ass::AssFile::from_scenario_structure_bsp`],
//! and the geometry must come back unchanged — same vertex count, same
//! positions, same triangles with the same materials and winding. The
//! test does that against scenes taken from shipped BSPs, so the inputs
//! are real rather than hand-made.

use std::path::Path;

use crate::ass::{AssFile, AssObjectPayload};
use crate::math::{RealPoint2d, RealPoint3d, RealVector3d};
use crate::{TagFieldData, TagFile};

/// ASS centimetres to world units.
const ASS_TO_WORLD: f32 = 0.01;

/// Triangles per part.
///
/// `index count` is a `short_integer`, so a part's run must stay under
/// 32,768 indices; 10,922 triangles is 32,766 of them and a whole number
/// of triangles. Going over writes a negative count, and a reader that
/// treats that as an empty range drops the geometry without complaining.
const MAX_TRIANGLES_PER_PART: usize = 10_922;

/// Why a scene could not be written.
#[derive(Debug, Clone, PartialEq)]
pub enum SbspError {
    Schema(String),
    MissingField(String),
    /// No mesh in the scene had any geometry.
    Empty,
    /// A mesh needs more indices than the block can address.
    TooManyIndices { object: usize, indices: usize },
    /// A mesh has more vertices than a signed-word index can name.
    TooManyVertices { object: usize, vertices: usize },
}

impl std::fmt::Display for SbspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Schema(m) => write!(f, "{m}"),
            Self::MissingField(m) => write!(f, "the schema has no field {m:?}"),
            Self::Empty => write!(f, "the scene has no geometry to import"),
            Self::TooManyIndices { object, indices } => write!(
                f,
                "object {object} needs {indices} indices, over the 65,535 a mesh can address"
            ),
            Self::TooManyVertices { object, vertices } => write!(
                f,
                "object {object} has {vertices} vertices, over the 32,767 an index can name                  without going negative in a signed word"
            ),
        }
    }
}

impl std::error::Error for SbspError {}

type R<T> = Result<T, SbspError>;

/// What a mesh in a structure scene is for.
///
/// Decided by the material its triangles use. `ass.rs` writes these
/// markers when it takes a BSP apart, and tool recognises the same names
/// when re-importing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRole {
    /// Ordinary render geometry: becomes a cluster.
    Render,
    /// A cluster portal, one convex polygon per mesh.
    Portal,
    /// A weather polyhedron.
    Weather,
    /// Collision-only geometry, which never reaches the render path.
    Collision,
}

/// Which role a material name marks.
///
/// `+portal` and `+weather` are prefixes; the collision markers are
/// whole names, and both spellings appear.
pub fn role_of_material(name: &str) -> MeshRole {
    if name.starts_with("+portal") {
        MeshRole::Portal
    } else if name.starts_with("+weather") {
        MeshRole::Weather
    } else if name.eq_ignore_ascii_case("@collision_only")
        || name.eq_ignore_ascii_case("@CollideOnly")
        || name.starts_with('@')
    {
        MeshRole::Collision
    } else {
        MeshRole::Render
    }
}

/// What was written.
#[derive(Debug, Clone, Default)]
pub struct SbspReport {
    pub materials: usize,
    pub clusters: usize,
    pub vertices: usize,
    pub triangles: usize,
    /// Objects that carried no geometry, or none this writes.
    pub skipped_objects: usize,
    /// Meshes recognised as portals, weather or collision. Their target
    /// blocks are not written yet, so these are carried through the
    /// report rather than dropped without a word.
    pub portals: usize,
    pub weather: usize,
    pub collision_meshes: usize,
    /// Surfaces in the structure collision BSP, and how many the 2D
    /// builder had to discard.
    pub collision_surfaces: usize,
    pub collision_dropped: usize,
    /// (leaf, surface) pairs the collision builder assigned.
    pub collision_pairs: usize,
    /// Winged edges the collision needs.
    pub collision_edges: usize,
    /// Worst distance a surface vertex sits from its own plane.
    pub worst_plane_offset: f32,
    /// Cluster portals written, and their vertices.
    pub portals_written: usize,
    pub portal_vertices: usize,
    /// Objects too big for one mesh, and the meshes they became.
    pub split_objects: usize,
    pub meshes_from_splits: usize,
    /// Instanced geometry: distinct definitions, and placements of them.
    pub instance_definitions: usize,
    pub instance_placements: usize,
    /// Leaves given a cluster, and cluster-to-portal links written.
    pub leaves_written: usize,
    pub cluster_portal_links: usize,
    /// Portals with the same cluster on both sides.
    pub portals_dividing_nothing: usize,
    /// The sealed world was built from the render geometry, because the
    /// scene marked no collision meshes.
    pub collision_from_render: bool,
    /// Leaves no cluster could reach, filed by nearest geometry instead.
    /// Leaves whose nearest cluster was behind a portal, so a
    /// further one took them.
    pub leaves_behind_a_portal: usize,
    pub adjacency_fragments: usize,
    pub adjacency_joins: usize,
    pub regions_found: usize,
    pub largest_region: usize,
    pub solid_leaves: usize,
    pub portal_same_leaf: usize,
    pub portal_same_region: usize,
    pub portal_same_cluster_diff_region: usize,
    pub portal_notes: Vec<String>,
    /// Each collision surface's ring, as built.
    pub collision_rings: Vec<Vec<[f32; 3]>>,

    /// Blocks a running level needs that this does not fill.
    pub not_written: Vec<String>,
}

/// What to write beyond the geometry every scene has.
#[derive(Debug, Clone, Copy)]
pub struct SbspOptions {
    /// Write instanced geometry definitions and their placements.
    ///
    /// On. A prop placed a hundred times is one definition and a hundred
    /// placements, which is what instancing is for and what the tag
    /// expects; turning this off treats every authored mesh as its own
    /// cluster, which is still a correct scene but a much larger one.
    ///
    /// Verified by placing the scene rather than by matching objects one
    /// to one. Those are not the same claim: the trip through world
    /// units quantises, so two near-identical copies of a prop land on
    /// the same values and merge into one definition placed twice. The
    /// object count moves and the scene does not.
    pub instanced_geometry: bool,
}

impl Default for SbspOptions {
    fn default() -> Self {
        Self { instanced_geometry: true }
    }
}

/// Build a `scenario_structure_bsp` from an ASS scene.
pub fn structure_bsp_from_ass(ass: &AssFile, schema: &Path) -> R<(TagFile, SbspReport)> {
    structure_bsp_from_ass_with(ass, schema, SbspOptions::default())
}

/// Build a `scenario_structure_bsp`, choosing what else to write.
pub fn structure_bsp_from_ass_with(
    ass: &AssFile,
    schema: &Path,
    opts: SbspOptions,
) -> R<(TagFile, SbspReport)> {
    let mut report = SbspReport::default();
    let mut tag = TagFile::new(schema).map_err(|e| {
        SbspError::Schema(format!(
            "cannot create a scenario_structure_bsp from {}: {e}",
            schema.display()
        ))
    })?;

    // Every MESH that carries geometry becomes one cluster. A real
    // importer partitions space into clusters by visibility; without
    // portals to partition against, one cluster per authored mesh keeps
    // the geometry intact and the mapping obvious.
    let mut meshes: Vec<(usize, &Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)> =
        Vec::new();
    let mut with_geometry = 0usize;
    let mut collision: Vec<(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)> =
        Vec::new();
    let mut portals: Vec<(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)> =
        Vec::new();
    for (i, o) in ass.objects.iter().enumerate() {
        let AssObjectPayload::Mesh { vertices, triangles } = &o.payload else { continue };
        if vertices.is_empty() || triangles.is_empty() {
            continue;
        }
        with_geometry += 1;
        // A mesh's role is its material's, and a marker material means
        // the geometry belongs somewhere other than the render path.
        let role = triangles
            .first()
            .and_then(|t| ass.materials.get(t.material.max(0) as usize))
            .map(|m| role_of_material(&m.name))
            .unwrap_or(MeshRole::Render);
        match role {
            MeshRole::Render => meshes.push((i, vertices, triangles)),
            MeshRole::Portal => {
                report.portals += 1;
                portals.push((vertices, triangles));
            }
            MeshRole::Weather => report.weather += 1,
            MeshRole::Collision => {
                report.collision_meshes += 1;
                collision.push((vertices, triangles));
            }
        }
    }
    report.skipped_objects = ass.objects.len() - with_geometry;
    if meshes.is_empty() {
        return Err(SbspError::Empty);
    }

    // Cut any object too big for one mesh into several, in place, so the
    // authored order is kept. Computed first and referenced after, since
    // everything downstream borrows.
    //
    // A cluster also carries its placement in its vertices. A cluster is
    // placed by one INSTANCE and the tag has nowhere to put that
    // transform, so it has to be baked in: an artist places the whole
    // structure wherever the scene sits, and a definition placed only
    // once is written as a cluster too. Ignoring it left 209 of armory's
    // placements at the origin instead of where they belong.
    let placements = instance_placements(ass);
    let instanced_pre = if opts.instanced_geometry {
        instanced_objects(ass, &meshes, &placements)
    } else {
        Default::default()
    };
    let bake: Vec<Option<Placement>> = meshes
        .iter()
        .map(|(object, _, _)| {
            let list = placements.get(object)?;
            if opts.instanced_geometry && instanced_pre.contains(object) {
                return None; // the placement block carries it instead
            }
            list.first().and_then(|&i| ass.instances.get(i)).and_then(Placement::of)
        })
        .collect();
    let split: Vec<Vec<(Vec<crate::ass::AssVertex>, Vec<crate::ass::AssTriangle>)>> = meshes
        .iter()
        .zip(&bake)
        .map(|((_, v, t), place)| {
            let big = t.len() * 3 > MAX_INDICES_PER_MESH || v.len() > MAX_VERTICES_PER_MESH;
            match (big, place) {
                (false, None) => Vec::new(),
                (true, None) => split_object(v, t),
                (big, Some(x)) => {
                    let moved: Vec<crate::ass::AssVertex> =
                        v.iter().map(|vert| x.apply(vert)).collect();
                    if big {
                        split_object(&moved, t)
                    } else {
                        vec![(moved, t.to_vec())]
                    }
                }
            }
        })
        .collect();
    let meshes: Vec<MeshRef<'_>> = meshes
        .iter()
        .zip(&split)
        .flat_map(|((object, v, t), pieces)| {
            if pieces.is_empty() {
                vec![(*object, *v, *t)]
            } else {
                pieces.iter().map(|(pv, pt)| (*object, pv, pt)).collect()
            }
        })
        .collect();
    report.split_objects = split.iter().filter(|p| !p.is_empty()).count();
    report.meshes_from_splits = split.iter().map(|p| p.len()).sum();

    // Split the render meshes into clusters and instanced geometry.
    //
    // The exporter's own convention decides it: a cluster is placed by a
    // single identity INSTANCE, while instanced geometry carries a
    // per-placement transform, and anything placed more than once must
    // be instanced by definition. An object placed exactly once is a
    // cluster — an instanced definition placed once is indistinguishable
    // from one, which is an ambiguity in the format rather than a choice
    // made here.
    let instanced = if opts.instanced_geometry {
        instanced_objects(ass, &meshes, &placements)
    } else {
        Default::default()
    };
    let (clusters, definitions): (Vec<_>, Vec<_>) =
        meshes.iter().cloned().partition(|(i, _, _)| !instanced.contains(i));
    if clusters.is_empty() {
        return Err(SbspError::Empty);
    }

    // Definitions keep their place in the mesh list rather than being
    // moved to the end. A definition names its geometry by index, so any
    // order works for the tag — but the exporter emits one OBJECT per
    // mesh in list order, and reordering here would reorder the scene
    // that comes back out. On a symmetric level that reads as geometry
    // mirrored through the origin, which is a far more alarming symptom
    // than the cause deserves.
    let mesh_index_of: std::collections::BTreeMap<usize, usize> =
        meshes.iter().enumerate().map(|(k, (i, _, _))| (*i, k)).collect();

    // Two definitions with identical geometry are one definition placed
    // twice — that is what instancing is for, and the exporter collapses
    // them on the way out, so leaving them separate here makes a scene
    // that cannot round-trip. The survivor keeps the first object's
    // mesh, and the duplicate's placements point at it.
    let mut seen: std::collections::BTreeMap<Vec<u8>, usize> = Default::default();
    let mut definitions: Vec<MeshRef<'_>> = definitions;
    let mut alias: std::collections::BTreeMap<usize, usize> = Default::default();
    definitions.retain(|(object, verts, tris)| {
        let mut key: Vec<u8> = Vec::with_capacity(verts.len() * 12 + tris.len() * 16);
        for v in verts.iter() {
            for c in [v.position.x, v.position.y, v.position.z] {
                key.extend_from_slice(&c.to_bits().to_le_bytes());
            }
        }
        for t in tris.iter() {
            for c in t.v {
                key.extend_from_slice(&c.to_le_bytes());
            }
            key.extend_from_slice(&t.material.to_le_bytes());
        }
        match seen.get(&key) {
            Some(&first) => {
                alias.insert(*object, first);
                false
            }
            None => {
                seen.insert(key, *object);
                true
            }
        }
    });
    // Fold the duplicates' placements onto the survivor.
    let mut placements = placements;
    for (dup, first) in &alias {
        if let Some(extra) = placements.remove(dup) {
            placements.entry(*first).or_default().extend(extra);
        }
    }

    write_materials(&mut tag, ass, &mut report)?;
    write_world_bounds(&mut tag, &clusters)?;
    write_render_geometry(&mut tag, &meshes, &mut report)?;
    write_clusters(&mut tag, &clusters, &mesh_index_of, &mut report)?;
    // A sealed world, from whatever the scene offers.
    //
    // A scene exported from a tag marks its collision with
    // `@collision_only`, because that is how the exporter round-trips
    // what the tag already held. An artist scene does not: not one of
    // the kit's fourteen source levels carries a collision-marked mesh,
    // and they are what a level is actually built from. For those the
    // sealed world is the level's own surfaces, which is what tool
    // derives it from too — without this they imported with no collision
    // at all, and so no partition either.
    let from_render: Vec<(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)> =
        clusters.iter().map(|(_, v, t)| (*v, *t)).collect();
    let collision_source = if collision.is_empty() { &from_render } else { &collision };
    if collision.is_empty() {
        report.collision_from_render = true;
    }
    // The portals come first: their planes are forced into the sealed
    // world so that no cell straddles one, which is what lets the two
    // sides of a portal be different clusters at all.
    let portal_rings = portal_rings(&portals, &mut report);
    let world = world_box(&clusters, &portal_rings);
    let sealed =
        write_structure_collision(&mut tag, collision_source, &portal_rings, world, &mut report)?;
    if !portal_rings.is_empty() {
        write_portal_block(&mut tag, &portal_rings, &mut report)?;
    }
    write_instanced_geometry(&mut tag, ass, &definitions, &mesh_index_of, &placements, &mut report)?;

    // The partition needs both: the tree supplies the leaves, and the
    // portals are what divides the clusters they are filed under.
    if let Some(sealed) = &sealed {
        write_partition(&mut tag, sealed, &clusters, &portal_rings, world, &mut report)?;
    } else {
        report.not_written.push("the leaf partition (no sealed world to divide)".into());
    }

    // What a level still needs that this does not author. Kept honest
    // as each piece lands: portals are written now, so only the
    // partition they divide is missing.
    report.not_written = [
        "a leaf partition that follows the portals (leaves are filed by
\n         nearest cluster geometry instead, which agrees with tool on
\n         91-98% of leaves and leaves about half the portals dividing
\n         one cluster from itself)",
        "pathfinding data",
        "lightmap-facing data (per-vertex lighting, PRT, lightmap texcoords)",
        "weather, atmosphere, sound and camera FX palettes",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    if !opts.instanced_geometry {
        report.not_written.push("instanced geometry (turned off for this build)".into());
    }

    Ok((tag, report))
}

/// Build and write the sealed world from the collision-marked meshes.
///
/// A structure BSP keeps its collision in the same shape a
/// `collision_model` does, so this is the same builder and the same ray
/// check — no second implementation to keep honest.
fn write_structure_collision(
    tag: &mut TagFile,
    meshes: &[(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)],
    portals: &[Vec<RealPoint3d>],
    world: [[f32; 2]; 3],
    report: &mut SbspReport,
) -> R<Option<crate::collision_import::CollisionBsp>> {
    if meshes.is_empty() {
        report.not_written.push("the structure collision BSP (no collision-marked meshes)".into());
        return Ok(None);
    }

    // Merge every collision mesh into one soup: the sealed world is a
    // single BSP, not one per authored object.
    let mut positions: Vec<RealPoint3d> = Vec::new();
    let mut tris: Vec<([u32; 3], i16)> = Vec::new();
    for (verts, triangles) in meshes {
        let base = positions.len() as u32;
        for v in verts.iter() {
            positions.push(RealPoint3d {
                x: v.position.x * ASS_TO_WORLD,
                y: v.position.y * ASS_TO_WORLD,
                z: v.position.z * ASS_TO_WORLD,
            });
        }
        for t in triangles.iter() {
            tris.push((
                [t.v[0] + base, t.v[1] + base, t.v[2] + base],
                t.material.clamp(0, i16::MAX as i32) as i16,
            ));
        }
    }

    let planes: Vec<([f32; 3], f32)> = portals
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| {
            let n = ring_normal(r);
            (n, n[0] * r[0].x + n[1] * r[0].y + n[2] * r[0].z)
        })
        .collect();
    let bsp = match crate::collision_import::collision_bsp_from_triangles_with_portals(
        &positions,
        &tris,
        &planes,
        world,
    ) {
        Ok(b) => b,
        Err(e) => {
            // A refusal here is worth saying out loud rather than
            // leaving as an empty block that reads as "no collision".
            report
                .not_written
                .push(format!("the structure collision BSP — the builder refused it: {e}"));
            return Ok(None);
        }
    };
    report.collision_surfaces = bsp.surfaces();
    report.collision_dropped = bsp.dropped();
    report.collision_pairs = bsp.assigned_pairs();
    report.collision_rings = bsp.rings();
    report.collision_edges = bsp.edges();
    report.worst_plane_offset = bsp.worst_plane_offset();


    let mut root = tag.root_mut();
    with_block(&mut root, "resource interface/raw_resources", |raw| {
        let ri = if raw.len() > 0 { 0 } else { raw.add_element() };
        let mut res = raw.element_mut(ri).ok_or_else(|| {
            SbspError::MissingField("resource interface/raw_resources element".into())
        })?;
        // `raw_items` is a struct, not a block — the resource holds one
        // set of items, not a list of them.
        let mut items_f = res
            .field_mut("raw_items")
            .ok_or_else(|| SbspError::MissingField("raw_items".into()))?;
        let mut item = items_f
            .as_struct_mut()
            .ok_or_else(|| SbspError::MissingField("raw_items (not a struct)".into()))?;
        let mut cf = item
            .field_mut("collision bsp")
            .ok_or_else(|| SbspError::MissingField("collision bsp".into()))?;
        let mut cb = cf
            .as_block_mut()
            .ok_or_else(|| SbspError::MissingField("collision bsp (not a block)".into()))?;
        let bi = cb.add_element();
        let mut el = cb
            .element_mut(bi)
            .ok_or_else(|| SbspError::MissingField("collision bsp element".into()))?;
        crate::collision_import::write_collision_bsp(&mut el, &bsp)
            .map_err(|e| SbspError::Schema(format!("cannot write the collision bsp: {e}")))
    })?;

    Ok(Some(bsp))
}

/// The largest ring a cluster portal may have, and the most portals a
/// structure may carry. Both are the schema's own limits.
const MAX_PORTAL_VERTICES: usize = 128;
const MAX_PORTALS: usize = 1024;

/// Write the `+portal` meshes back into `cluster portals`.
///
/// A portal is one convex polygon, and the exporter fan-triangulates it
/// from a single vertex — so taking the mesh's vertices in first-use
/// order recovers the ring it was written from, in order, with no
/// boundary walk needed.
///
/// Three fields are left as NONE rather than guessed. `back cluster` and
/// `front cluster` say which cluster lies on each side, and `plane
/// index` names a plane in a table this does not build; both need the
/// spatial partition that turns authored meshes into real clusters, and
/// that is not written yet. Inventing them would produce a tag that
/// looks complete and routes visibility wrongly. The geometry, which was
/// being dropped on the floor entirely, now survives.
fn write_cluster_portals(
    tag: &mut TagFile,
    portals: &[(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)],
    report: &mut SbspReport,
) -> R<Vec<Vec<RealPoint3d>>> {
    if portals.is_empty() {
        return Ok(Vec::new());
    }
    if portals.len() > MAX_PORTALS {
        report.not_written.push(format!(
            "cluster portals: {} of them, over the {MAX_PORTALS} a structure may carry",
            portals.len()
        ));
        return Ok(Vec::new());
    }

    let rings = portal_rings(portals, report);
    if rings.is_empty() {
        return Ok(Vec::new());
    }
    write_portal_block(tag, &rings, report)?;
    Ok(rings)
}

/// The ring of each `+portal` mesh, in world units.
fn portal_rings(
    portals: &[(&Vec<crate::ass::AssVertex>, &Vec<crate::ass::AssTriangle>)],
    report: &mut SbspReport,
) -> Vec<Vec<RealPoint3d>> {
    let mut rings: Vec<Vec<RealPoint3d>> = Vec::new();
    for (verts, triangles) in portals {
        let mut order: Vec<u32> = Vec::new();
        for t in triangles.iter() {
            for &c in &t.v {
                if !order.contains(&c) {
                    order.push(c);
                }
            }
        }
        if order.len() < 3 || order.len() > MAX_PORTAL_VERTICES {
            report.not_written.push(format!(
                "a cluster portal with {} vertices, which is outside 3..={MAX_PORTAL_VERTICES}",
                order.len()
            ));
            continue;
        }
        let ring: Vec<RealPoint3d> = order
            .iter()
            .filter_map(|&i| verts.get(i as usize))
            .map(|v| RealPoint3d {
                x: v.position.x * ASS_TO_WORLD,
                y: v.position.y * ASS_TO_WORLD,
                z: v.position.z * ASS_TO_WORLD,
            })
            .collect();
        if ring.len() == order.len() {
            rings.push(ring);
        }
    }
    rings
}

/// Write the rings into `cluster portals`.
fn write_portal_block(
    tag: &mut TagFile,
    rings: &[Vec<RealPoint3d>],
    report: &mut SbspReport,
) -> R<()> {
    let mut root = tag.root_mut();
    let (written, points) = with_block(&mut root, "cluster portals", |block| {
        let (mut written, mut points) = (0usize, 0usize);
        for ring in rings {
            let i = block.add_element();
            let mut el = block
                .element_mut(i)
                .ok_or_else(|| SbspError::MissingField("cluster portal element".into()))?;

            let inv = 1.0 / ring.len() as f32;
            let c = ring.iter().fold([0.0f32; 3], |a, p| {
                [a[0] + p.x * inv, a[1] + p.y * inv, a[2] + p.z * inv]
            });
            let radius = ring
                .iter()
                .map(|p| {
                    let (dx, dy, dz) = (p.x - c[0], p.y - c[1], p.z - c[2]);
                    (dx * dx + dy * dy + dz * dz).sqrt()
                })
                .fold(0.0f32, f32::max);

            try_set(&mut el, "back cluster", TagFieldData::ShortInteger(-1));
            try_set(&mut el, "front cluster", TagFieldData::ShortInteger(-1));
            try_set(&mut el, "plane index", TagFieldData::LongInteger(-1));
            try_set(
                &mut el,
                "centroid",
                TagFieldData::RealPoint3d(RealPoint3d { x: c[0], y: c[1], z: c[2] }),
            );
            try_set(&mut el, "bounding radius", TagFieldData::Real(radius));
            try_set(&mut el, "flags", TagFieldData::LongFlags { value: 0, names: Vec::new() });

            with_block(&mut el, "vertices", |vb| {
                for p in ring {
                    let vi = vb.add_element();
                    let mut ve = vb.element_mut(vi).ok_or_else(|| {
                        SbspError::MissingField("cluster portal vertex".into())
                    })?;
                    try_set(&mut ve, "point", TagFieldData::RealPoint3d(*p));
                    points += 1;
                }
                Ok(())
            })?;
            written += 1;
        }
        Ok((written, points))
    })?;

    report.portals_written = written;
    report.portal_vertices = points;
    Ok(())
}

/// An instance transform, ready to apply to a vertex.
struct Placement {
    rotation: [[f32; 3]; 3],
    translation: [f32; 3],
    scale: f32,
}

impl Placement {
    /// `None` when the instance does not move anything, so the common
    /// case copies no vertices.
    fn of(inst: &crate::ass::AssInstance) -> Option<Self> {
        let q = &inst.local_rotation;
        let t = &inst.local_translation;
        let still = (inst.local_scale - 1.0).abs() <= 1e-6
            && t.x.abs() <= 1e-6
            && t.y.abs() <= 1e-6
            && t.z.abs() <= 1e-6
            && q.i.abs() <= 1e-6
            && q.j.abs() <= 1e-6
            && q.k.abs() <= 1e-6
            && (q.w.abs() - 1.0).abs() <= 1e-6;
        if still {
            return None;
        }
        let (x, y, z, w) = (q.i, q.j, q.k, q.w);
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        Some(Self {
            rotation: [
                [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
                [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
                [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
            ],
            translation: [t.x, t.y, t.z],
            scale: inst.local_scale,
        })
    }

    fn apply(&self, v: &crate::ass::AssVertex) -> crate::ass::AssVertex {
        let r = &self.rotation;
        let turn = |p: [f32; 3]| {
            [
                r[0][0] * p[0] + r[0][1] * p[1] + r[0][2] * p[2],
                r[1][0] * p[0] + r[1][1] * p[1] + r[1][2] * p[2],
                r[2][0] * p[0] + r[2][1] * p[1] + r[2][2] * p[2],
            ]
        };
        let p = turn([
            v.position.x * self.scale,
            v.position.y * self.scale,
            v.position.z * self.scale,
        ]);
        // The normal turns but neither moves nor scales; a negative
        // scale is a mirror, and the sign belongs on it too.
        let sign = self.scale.signum();
        let nrm = turn([v.normal.i * sign, v.normal.j * sign, v.normal.k * sign]);
        let mut out = v.clone();
        out.position = RealPoint3d {
            x: p[0] + self.translation[0],
            y: p[1] + self.translation[1],
            z: p[2] + self.translation[2],
        };
        out.normal = RealVector3d { i: nrm[0], j: nrm[1], k: nrm[2] };
        out
    }
}

/// Which ASS instances place each object.
fn instance_placements(ass: &AssFile) -> std::collections::BTreeMap<usize, Vec<usize>> {
    let mut out: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for (i, inst) in ass.instances.iter().enumerate() {
        if inst.object_index >= 0 {
            out.entry(inst.object_index as usize).or_default().push(i);
        }
    }
    out
}

/// Does this placement move its object?
fn moves_it(ass: &AssFile, list: &[usize]) -> bool {
    list.iter().any(|&i| {
        ass.instances.get(i).is_some_and(|inst| {
            let q = &inst.local_rotation;
            let t = &inst.local_translation;
            (inst.local_scale - 1.0).abs() > 1e-5
                || t.x.abs() > 1e-5
                || t.y.abs() > 1e-5
                || t.z.abs() > 1e-5
                || q.i.abs() > 1e-5
                || q.j.abs() > 1e-5
                || q.k.abs() > 1e-5
                || (q.w.abs() - 1.0).abs() > 1e-5
        })
    })
}

/// Which objects are instanced geometry rather than clusters.
///
/// Placed more than once always counts: that is what instancing means.
/// A single placement counts too when it moves the object, because a
/// scene exported from a tag writes its clusters at identity and its
/// instances with a real transform — and that reading matters. Without
/// it armory keeps 247 clusters where tool has 38, and a leaf names its
/// cluster in a signed byte, so everything past 127 wraps negative.
///
/// But that is a habit of the exporter, not a rule of the format. An
/// artist scene places its structure wherever it sits, so the same test
/// calls every object instanced and leaves no clusters at all — it
/// refused all fourteen source levels in the kit. So it is used only
/// when it leaves some clusters behind, and otherwise the plain rule
/// stands.
fn instanced_objects(
    ass: &AssFile,
    meshes: &[MeshRef<'_>],
    placements: &std::collections::BTreeMap<usize, Vec<usize>>,
) -> std::collections::BTreeSet<usize> {
    let pick = |use_transform: bool| -> std::collections::BTreeSet<usize> {
        meshes
            .iter()
            .filter_map(|(object, _, _)| {
                let list = placements.get(object)?;
                let yes = list.len() > 1 || (use_transform && moves_it(ass, list));
                yes.then_some(*object)
            })
            .collect()
    };
    let distinct: std::collections::BTreeSet<usize> =
        meshes.iter().map(|(o, _, _)| *o).collect();
    let with_transform = pick(true);
    if with_transform.len() < distinct.len() {
        with_transform
    } else {
        pick(false)
    }
}

/// The most instanced geometry a structure may carry, per the schema.
const MAX_INSTANCE_DEFINITIONS: usize = 1024;
const MAX_INSTANCE_PLACEMENTS: usize = 4096;

/// Write instanced geometry definitions and their placements.
///
/// A definition names its geometry by an index into the mesh list, which
/// `mesh_index_of` supplies — the meshes keep their authored order, so a
/// definition's index is simply where its object already sits. The
/// placement carries the transform as three basis vectors and a
/// position, which is what the exporter reads back.
///
/// A definition's own collision BSP is not built here. That is the same
/// work the structure collision does, once per definition, and it is
/// worth doing; leaving it empty keeps the render geometry and the
/// placements, which were dropped whole before.
fn write_instanced_geometry(
    tag: &mut TagFile,
    ass: &AssFile,
    definitions: &[MeshRef<'_>],
    mesh_index_of: &std::collections::BTreeMap<usize, usize>,
    placements: &std::collections::BTreeMap<usize, Vec<usize>>,
    report: &mut SbspReport,
) -> R<()> {
    if definitions.is_empty() {
        return Ok(());
    }
    if definitions.len() > MAX_INSTANCE_DEFINITIONS {
        report.not_written.push(format!(
            "instanced geometry: {} definitions, over the {MAX_INSTANCE_DEFINITIONS} allowed",
            definitions.len()
        ));
        return Ok(());
    }

    let mut root = tag.root_mut();
    with_block(&mut root, "resource interface/raw_resources", |raw| {
        let ri = if raw.len() > 0 { 0 } else { raw.add_element() };
        let mut res = raw.element_mut(ri).ok_or_else(|| {
            SbspError::MissingField("resource interface/raw_resources element".into())
        })?;
        let mut items_f = res
            .field_mut("raw_items")
            .ok_or_else(|| SbspError::MissingField("raw_items".into()))?;
        // A struct, not a block: the resource holds one set of items.
        let mut item = items_f
            .as_struct_mut()
            .ok_or_else(|| SbspError::MissingField("raw_items (not a struct)".into()))?;
        with_block(&mut item, "instanced geometries definitions", |defs| {
            for (object, verts, _) in definitions.iter() {
                let di = defs.add_element();
                let mut el = defs
                    .element_mut(di)
                    .ok_or_else(|| SbspError::MissingField("definition element".into()))?;
                let (c, radius) = bounding_sphere(verts);
                try_set(&mut el, "bounding sphere center", TagFieldData::RealPoint3d(c));
                try_set(&mut el, "bounding sphere radius", TagFieldData::Real(radius));
                let mi = mesh_index_of.get(object).copied().unwrap_or(0);
                try_set(&mut el, "mesh index", TagFieldData::ShortInteger(mi as i16));
                try_set(&mut el, "compression index", TagFieldData::ShortInteger(0));
            }
            Ok(())
        })
    })?;
    report.instance_definitions = definitions.len();

    let mut root = tag.root_mut();
    let written = with_block(&mut root, "instanced geometry instances", |block| {
        let mut written = 0usize;
        for (k, (object, _, _)) in definitions.iter().enumerate() {
            let Some(list) = placements.get(object) else { continue };
            for &pi in list {
                if written >= MAX_INSTANCE_PLACEMENTS {
                    break;
                }
                let Some(inst) = ass.instances.get(pi) else { continue };
                let i = block.add_element();
                let mut el = block
                    .element_mut(i)
                    .ok_or_else(|| SbspError::MissingField("instance element".into()))?;
                let (f, l, u) = basis_of(&inst.local_rotation);
                try_set(&mut el, "scale", TagFieldData::Real(inst.local_scale));
                try_set(&mut el, "forward", TagFieldData::RealVector3d(f));
                try_set(&mut el, "left", TagFieldData::RealVector3d(l));
                try_set(&mut el, "up", TagFieldData::RealVector3d(u));
                try_set(
                    &mut el,
                    "position",
                    TagFieldData::RealPoint3d(RealPoint3d {
                        x: inst.local_translation.x * ASS_TO_WORLD,
                        y: inst.local_translation.y * ASS_TO_WORLD,
                        z: inst.local_translation.z * ASS_TO_WORLD,
                    }),
                );
                try_set(&mut el, "instance definition", TagFieldData::ShortBlockIndex(k as i16));
                written += 1;
            }
        }
        Ok(written)
    })?;
    report.instance_placements = written;
    Ok(())
}

/// A mesh's bounding sphere, in world units.
fn bounding_sphere(verts: &[crate::ass::AssVertex]) -> (RealPoint3d, f32) {
    let inv = 1.0 / verts.len().max(1) as f32;
    let c = verts.iter().fold([0.0f32; 3], |a, v| {
        [
            a[0] + v.position.x * ASS_TO_WORLD * inv,
            a[1] + v.position.y * ASS_TO_WORLD * inv,
            a[2] + v.position.z * ASS_TO_WORLD * inv,
        ]
    });
    let r = verts
        .iter()
        .map(|v| {
            let dx = v.position.x * ASS_TO_WORLD - c[0];
            let dy = v.position.y * ASS_TO_WORLD - c[1];
            let dz = v.position.z * ASS_TO_WORLD - c[2];
            (dx * dx + dy * dy + dz * dz).sqrt()
        })
        .fold(0.0f32, f32::max);
    (RealPoint3d { x: c[0], y: c[1], z: c[2] }, r)
}

/// A rotation as the three basis vectors the tag stores.
fn basis_of(q: &crate::math::RealQuaternion) -> (RealVector3d, RealVector3d, RealVector3d) {
    let (x, y, z, w) = (q.i, q.j, q.k, q.w);
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    (
        RealVector3d { i: 1.0 - 2.0 * (yy + zz), j: 2.0 * (xy + wz), k: 2.0 * (xz - wy) },
        RealVector3d { i: 2.0 * (xy - wz), j: 1.0 - 2.0 * (xx + zz), k: 2.0 * (yz + wx) },
        RealVector3d { i: 2.0 * (xz + wy), j: 2.0 * (yz - wx), k: 1.0 - 2.0 * (xx + yy) },
    )
}

/// The most clusters a structure may hold, per the schema. A leaf names
/// its cluster in a single `char_integer`, so the index also has to stay
/// inside a byte.
const MAX_CLUSTERS: usize = 255;

/// Assign every leaf of the sealed world to a cluster, and link the
/// portals to the clusters they divide.
///
/// The `leaves` block runs parallel to the collision BSP's leaves — one
/// entry each, measured on shipped levels, where guardian has 11,305 of
/// both and isolation 19,594 — and every entry names a real cluster:
/// across five levels and 114,162 leaves, tool never writes a negative
/// one.
///
/// Which cluster is a question about a point in space, and the rule here
/// is the nearest cluster geometry. Scored against tool's own answers on
/// shipped levels it agrees on about 91% of armory's leaves and 98% of
/// bunkerworld's. It is not what tool does: tool's clusters are the
/// regions its portals cut space into, and reproducing that needs a
/// flood fill across leaf adjacency this does not compute. The
/// difference shows up as a leaf near a cluster boundary filed on the
/// far side of it.
///
/// What is written is at least self-consistent: a portal's two clusters
/// come from the same assignment the leaves got, and a cluster's portal
/// list is exactly the portals naming it.
fn write_partition(
    tag: &mut TagFile,
    bsp: &crate::collision_import::CollisionBsp,
    clusters: &[MeshRef<'_>],
    portals: &[Vec<RealPoint3d>],
    world: [[f32; 2]; 3],
    report: &mut SbspReport,
) -> R<()> {
    if clusters.is_empty() || bsp.leaf_count() == 0 {
        return Ok(());
    }
    if clusters.len() > MAX_CLUSTERS {
        report.not_written.push(format!(
            "the leaf partition: {} clusters, over the {MAX_CLUSTERS} a leaf can name",
            clusters.len()
        ));
        return Ok(());
    }

    let points: Vec<Vec<[f32; 3]>> = clusters
        .iter()
        .map(|(_, verts, _)| {
            verts
                .iter()
                .map(|v| {
                    [
                        v.position.x * ASS_TO_WORLD,
                        v.position.y * ASS_TO_WORLD,
                        v.position.z * ASS_TO_WORLD,
                    ]
                })
                .collect()
        })
        .collect();

    // A uniform grid over that geometry. Without one this is every leaf
    // against every vertex, which on a real level is billions of
    // distance tests.
    let grid = Grid::build(&points, world);
    // A leaf the descent never reached keeps cluster 0 rather than a
    // negative: tool writes no negative leaf, and an index the runtime
    // cannot follow is worse than a merely wrong one.
    let cells = bsp.leaf_cells(world);

    // Regions, then a cluster for each region.
    let rings: Vec<(Vec<[f32; 3]>, [f32; 3])> = portals
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| (r.iter().map(|p| [p.x, p.y, p.z]).collect(), ring_normal(r)))
        .collect();
    let solidity = Solidity::build(bsp.rings(), world);
    let solid: Vec<bool> = cells
        .iter()
        .map(|c| {
            c.map(|b| {
                solidity.is_solid([
                    0.5 * (b[0][0] + b[0][1]),
                    0.5 * (b[1][0] + b[1][1]),
                    0.5 * (b[2][0] + b[2][1]),
                ])
            })
            .unwrap_or(false)
        })
        .collect();
    report.solid_leaves = solid.iter().filter(|s| **s).count();
    let centres: Vec<Option<[f32; 3]>> = cells
        .iter()
        .map(|c| {
            c.map(|b| {
                [
                    0.5 * (b[0][0] + b[0][1]),
                    0.5 * (b[1][0] + b[1][1]),
                    0.5 * (b[2][0] + b[2][1]),
                ]
            })
        })
        .collect();
    let (region, fragments, joins) = leaf_regions(bsp, world, &rings, &solid, &centres);
    report.adjacency_fragments = fragments;
    report.adjacency_joins = joins;

    // A region belongs to whichever cluster seeded most of its cells. A
    // seed steps off a cluster triangle along its normal, because a
    // vertex sits on the surface itself and descends to whichever side
    // rounding picks.
    let mut votes: std::collections::HashMap<(u32, i16), usize> = Default::default();
    for (ci, (_, verts, tris)) in clusters.iter().enumerate() {
        for t in tris.iter() {
            let p: Vec<[f32; 3]> = t
                .v
                .iter()
                .filter_map(|&i| verts.get(i as usize))
                .map(|v| {
                    [
                        v.position.x * ASS_TO_WORLD,
                        v.position.y * ASS_TO_WORLD,
                        v.position.z * ASS_TO_WORLD,
                    ]
                })
                .collect();
            if p.len() != 3 {
                continue;
            }
            let e1 = [p[1][0] - p[0][0], p[1][1] - p[0][1], p[1][2] - p[0][2]];
            let e2 = [p[2][0] - p[0][0], p[2][1] - p[0][1], p[2][2] - p[0][2]];
            let nv = [
                e1[1] * e2[2] - e1[2] * e2[1],
                e1[2] * e2[0] - e1[0] * e2[2],
                e1[0] * e2[1] - e1[1] * e2[0],
            ];
            let l = (nv[0] * nv[0] + nv[1] * nv[1] + nv[2] * nv[2]).sqrt();
            if l <= 1e-12 {
                continue;
            }
            let c = [
                (p[0][0] + p[1][0] + p[2][0]) / 3.0,
                (p[0][1] + p[1][1] + p[2][1]) / 3.0,
                (p[0][2] + p[1][2] + p[2][2]) / 3.0,
            ];
            for dir in [1.0f32, -1.0] {
                let q = [
                    c[0] + nv[0] / l * dir * 0.02,
                    c[1] + nv[1] / l * dir * 0.02,
                    c[2] + nv[2] / l * dir * 0.02,
                ];
                if let Some(&r) = region.get(bsp.leaf_at(q)) {
                    *votes.entry((r, ci as i16)).or_default() += 1;
                }
            }
        }
    }
    let mut owner: std::collections::HashMap<u32, (i16, usize)> = Default::default();
    for ((r, ci), count) in &votes {
        let e = owner.entry(*r).or_insert((*ci, 0));
        if *count > e.1 {
            *e = (*ci, *count);
        }
    }

    // A region nothing seeded — sealed in a wall, or outside the level —
    // takes the cluster nearest one of its cells. Tool writes no
    // negative leaf, so every cell names something.
    let mut assigned = vec![0i16; cells.len()];
    let mut unseeded = 0usize;
    for (i, cell) in cells.iter().enumerate() {
        let r = region.get(i).copied().unwrap_or(0);
        assigned[i] = match owner.get(&r) {
            Some((ci, _)) => *ci,
            None => {
                unseeded += 1;
                cell.map(|b| {
                    grid.nearest(
                        &points,
                        [
                            0.5 * (b[0][0] + b[0][1]),
                            0.5 * (b[1][0] + b[1][1]),
                            0.5 * (b[2][0] + b[2][1]),
                        ],
                    )
                })
                .unwrap_or(0)
            }
        };
    }
    report.leaves_behind_a_portal = unseeded;
    {
        let mut sizes: std::collections::HashMap<u32, usize> = Default::default();
        for r in &region {
            *sizes.entry(*r).or_default() += 1;
        }
        report.regions_found = sizes.len();
        report.largest_region = sizes.values().copied().max().unwrap_or(0);
    }

    let mut root = tag.root_mut();
    with_block(&mut root, "leaves", |block| {
        for c in &assigned {
            let i = block.add_element();
            let mut el = block
                .element_mut(i)
                .ok_or_else(|| SbspError::MissingField("leaf element".into()))?;
            try_set(&mut el, "cluster", TagFieldData::CharInteger(*c as i8));
        }
        Ok(())
    })?;
    report.leaves_written = assigned.len();

    // A portal divides two clusters: step off its face either way and
    // see which leaf, and so which cluster, each side lands in.
    let mut sides: Vec<(i16, i16)> = Vec::with_capacity(portals.len());
    for ring in portals {
        let inv = 1.0 / ring.len().max(1) as f32;
        let c = ring
            .iter()
            .fold([0.0f32; 3], |a, p| [a[0] + p.x * inv, a[1] + p.y * inv, a[2] + p.z * inv]);
        let n = ring_normal(ring);
        let side = |dir: f32| -> i16 {
            let q = [
                c[0] + n[0] * dir * 0.05,
                c[1] + n[1] * dir * 0.05,
                c[2] + n[2] * dir * 0.05,
            ];
            assigned.get(bsp.leaf_at(q)).copied().unwrap_or(-1)
        };
        let leaf_of = |dir: f32| -> usize {
            bsp.leaf_at([
                c[0] + n[0] * dir * 0.05,
                c[1] + n[1] * dir * 0.05,
                c[2] + n[2] * dir * 0.05,
            ])
        };
        let (b, f) = (side(-1.0), side(1.0));
        // A portal that finds the same cluster on both sides is
        // dividing nothing the partition knows about — worth
        // counting, because a level full of them has a partition
        // that is not tracking its own portals.
        if b == f {
            report.portals_dividing_nothing += 1;
            let (la, lb) = (leaf_of(-1.0), leaf_of(1.0));
            let (ra, rb) = (
                region.get(la).copied().unwrap_or(u32::MAX),
                region.get(lb).copied().unwrap_or(u32::MAX),
            );
            if la == lb {
                report.portal_same_leaf += 1;
            } else if ra == rb {
                report.portal_same_region += 1;
            } else {
                report.portal_same_cluster_diff_region += 1;
            }
            if report.portal_notes.len() < 4 {
                let lb = bsp.leaf_at([
                    c[0] - n[0] * 0.05,
                    c[1] - n[1] * 0.05,
                    c[2] - n[2] * 0.05,
                ]);
                let lf = bsp.leaf_at([
                    c[0] + n[0] * 0.05,
                    c[1] + n[1] * 0.05,
                    c[2] + n[2] * 0.05,
                ]);
                report.portal_notes.push(format!(
                    "portal at ({:.1},{:.1},{:.1}) n=({:.2},{:.2},{:.2}): leaves {lb} and {lf}, both cluster {b}",
                    c[0], c[1], c[2], n[0], n[1], n[2]
                ));
            }
        }
        sides.push((b, f));
    }

    let mut root = tag.root_mut();
    with_block(&mut root, "cluster portals", |block| {
        for (i, (back, front)) in sides.iter().enumerate() {
            let Some(mut el) = block.element_mut(i) else { continue };
            try_set(&mut el, "back cluster", TagFieldData::ShortInteger(*back));
            try_set(&mut el, "front cluster", TagFieldData::ShortInteger(*front));
        }
        Ok(())
    })?;

    let mut root = tag.root_mut();
    with_block(&mut root, "clusters", |block| {
        for ci in 0..clusters.len() {
            let Some(mut el) = block.element_mut(ci) else { continue };
            let mine: Vec<usize> = sides
                .iter()
                .enumerate()
                .filter(|(_, (b, f))| *b as usize == ci || *f as usize == ci)
                .map(|(i, _)| i)
                .collect();
            with_block(&mut el, "portals", |pb| {
                for pi in &mine {
                    let k = pb.add_element();
                    let mut pe = pb
                        .element_mut(k)
                        .ok_or_else(|| SbspError::MissingField("cluster portal index".into()))?;
                    try_set(&mut pe, "portal index", TagFieldData::ShortInteger(*pi as i16));
                }
                Ok(())
            })?;
            report.cluster_portal_links += mine.len();
        }
        Ok(())
    })?;
    Ok(())
}

/// Does the segment `a`..`b` pass through this polygon?
///
/// A portal is a convex polygon, so once the segment crosses its plane
/// the only question left is whether the crossing point is inside the
/// outline.
fn segment_crosses(a: [f32; 3], b: [f32; 3], ring: &[RealPoint3d], n: [f32; 3]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let d = n[0] * ring[0].x + n[1] * ring[0].y + n[2] * ring[0].z;
    let da = n[0] * a[0] + n[1] * a[1] + n[2] * a[2] - d;
    let db = n[0] * b[0] + n[1] * b[1] + n[2] * b[2] - d;
    if (da > 0.0) == (db > 0.0) {
        return false;
    }
    let t = da / (da - db);
    let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];

    // Inside the outline, tested on the two axes the plane faces least.
    let axis = (0..3).max_by(|&x, &y| n[x].abs().total_cmp(&n[y].abs())).unwrap_or(0);
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let at = |q: &RealPoint3d| [[q.x, q.y, q.z][u], [q.x, q.y, q.z][v]];
    let pt2 = [p[u], p[v]];
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (c, e) = (at(&ring[i]), at(&ring[j]));
        if (c[1] > pt2[1]) != (e[1] > pt2[1]) {
            let x = c[0] + (pt2[1] - c[1]) / (e[1] - c[1]) * (e[0] - c[0]);
            if pt2[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// File each leaf under the nearest cluster it can see.
///
/// Nearest geometry alone ignores the portals, and it showed: 650 of
/// 1,192 portals came back with the same cluster on both sides, which is
/// a portal dividing nothing. A cluster is a region and a portal is
/// where one region stops, so the line from a leaf to the geometry it is
/// filed under must not pass through one.
///
/// Two other shapes were tried first and are worth not repeating. A
/// breadth-first flood from seeded leaves needs leaf adjacency, and a
/// cell here is a convex solid bounded by oblique planes: probing along
/// the six axes from its centre found 3,000 neighbours across 86,000
/// leaves, because the probes kept landing back in the cell they left.
/// Marching until the leaf index changed found no more.
///
/// This asks the question directly instead. Take the nearest few
/// clusters, and file the leaf under the first whose closest point it
/// can reach in a straight line without crossing a portal. If every
/// candidate is blocked, the nearest one stands — a leaf sealed inside a
/// wall still has to name a cluster, and tool never writes a negative.
fn portal_aware_clusters(
    cells: &[Option<[[f32; 2]; 3]>],
    grid: &Grid,
    points: &[Vec<[f32; 3]>],
    portals: &[Vec<RealPoint3d>],
) -> (Vec<i16>, usize) {
    let normals: Vec<[f32; 3]> = portals.iter().map(|r| ring_normal(r)).collect();
    let mut out = vec![0i16; cells.len()];
    let mut blocked_off = 0usize;
    for (i, cell) in cells.iter().enumerate() {
        let Some(b) = cell else { continue };
        let p = [
            0.5 * (b[0][0] + b[0][1]),
            0.5 * (b[1][0] + b[1][1]),
            0.5 * (b[2][0] + b[2][1]),
        ];
        let cands = grid.nearest_few(points, p, 6);
        if cands.is_empty() {
            continue;
        }
        let clear = cands.iter().find(|(_, q)| {
            !portals
                .iter()
                .zip(&normals)
                .any(|(ring, n)| segment_crosses(p, *q, ring, *n))
        });
        match clear {
            Some((ci, _)) => {
                if *ci != cands[0].0 {
                    blocked_off += 1;
                }
                out[i] = *ci;
            }
            None => out[i] = cands[0].0,
        }
    }
    (out, blocked_off)
}

/// Which cells sit inside solid, worked out by counting crossings.
///
/// A region has to stop at a wall, and the fragments the adjacency finds
/// do not say so on their own: a wall is a volume, the tree splits its
/// inside with planes no surface lies on, and a fragment there is
/// "uncovered" and joins the two cells straight through the wall. That
/// leak put 46,355 of 53,079 leaves in one region.
///
/// So each cell is asked whether it is inside the sealed world instead.
/// Fire a ray along +X and count the surfaces it crosses: an odd count
/// means the point started inside solid. Cells inside solid are left out
/// of every region, and two rooms with a wall between them then have no
/// path at all, because every path would have to pass through one.
///
/// Surfaces are bucketed by the Y and Z they span so a query only tests
/// the few that could possibly be in the way.
struct Solidity {
    origin: [f32; 2],
    cell: f32,
    dims: [usize; 2],
    buckets: Vec<Vec<u32>>,
    polys: Vec<Vec<[f32; 3]>>,
}

impl Solidity {
    fn build(polys: Vec<Vec<[f32; 3]>>, world: [[f32; 2]; 3]) -> Self {
        let span = (world[1][1] - world[1][0]).max(world[2][1] - world[2][0]).max(1e-3);
        let n = 96usize;
        let cell = span / n as f32;
        let dims = [n + 1, n + 1];
        let origin = [world[1][0], world[2][0]];
        let mut buckets = vec![Vec::new(); dims[0] * dims[1]];
        for (i, poly) in polys.iter().enumerate() {
            if poly.len() < 3 {
                continue;
            }
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for p in poly {
                for (k, c) in [p[1], p[2]].into_iter().enumerate() {
                    lo[k] = lo[k].min(c);
                    hi[k] = hi[k].max(c);
                }
            }
            let idx = |c: f32, k: usize| {
                (((c - origin[k]) / cell).floor().max(0.0) as usize).min(dims[k] - 1)
            };
            for y in idx(lo[0], 0)..=idx(hi[0], 0) {
                for z in idx(lo[1], 1)..=idx(hi[1], 1) {
                    buckets[z * dims[0] + y].push(i as u32);
                }
            }
        }
        Self { origin, cell, dims, buckets, polys }
    }

    fn is_solid(&self, p: [f32; 3]) -> bool {
        let idx = |c: f32, k: usize| {
            (((c - self.origin[k]) / self.cell).floor().max(0.0) as usize).min(self.dims[k] - 1)
        };
        let b = &self.buckets[idx(p[2], 1) * self.dims[0] + idx(p[1], 0)];
        let mut crossings = 0usize;
        for &si in b {
            let poly = &self.polys[si as usize];
            if poly.len() < 3 {
                continue;
            }
            // The plane, from the first corner that gives a normal.
            let mut n = [0.0f32; 3];
            for k in 1..poly.len() - 1 {
                let a = [
                    poly[k][0] - poly[0][0],
                    poly[k][1] - poly[0][1],
                    poly[k][2] - poly[0][2],
                ];
                let c = [
                    poly[k + 1][0] - poly[0][0],
                    poly[k + 1][1] - poly[0][1],
                    poly[k + 1][2] - poly[0][2],
                ];
                let m = [
                    a[1] * c[2] - a[2] * c[1],
                    a[2] * c[0] - a[0] * c[2],
                    a[0] * c[1] - a[1] * c[0],
                ];
                if m[0] * m[0] + m[1] * m[1] + m[2] * m[2] > 1e-20 {
                    n = m;
                    break;
                }
            }
            if n[0].abs() <= 1e-12 {
                continue; // parallel to the ray, cannot be crossed
            }
            let d = n[0] * poly[0][0] + n[1] * poly[0][1] + n[2] * poly[0][2];
            let t = (d - n[1] * p[1] - n[2] * p[2]) / n[0] - p[0];
            if t <= 1e-4 {
                continue;
            }
            // Inside the outline, in the Y/Z plane the ray travels through.
            let hit = [p[1], p[2]];
            let mut inside = false;
            let mut j = poly.len() - 1;
            for i in 0..poly.len() {
                let (c, e) = ([poly[i][1], poly[i][2]], [poly[j][1], poly[j][2]]);
                if (c[1] > hit[1]) != (e[1] > hit[1]) {
                    let x = c[0] + (hit[1] - c[1]) / (e[1] - c[1]) * (e[0] - c[0]);
                    if hit[0] < x {
                        inside = !inside;
                    }
                }
                j = i;
            }
            if inside {
                crossings += 1;
            }
        }
        crossings % 2 == 1
    }
}

/// Group the leaves into regions: the cells you can move between
/// without passing through a surface or a portal.
///
/// That is what a cluster is, and it is why a portal always divides two
/// of them. Filing leaves by whichever cluster geometry is nearest does
/// not: 357 portals came back with the same cluster on both sides, where
/// tool has none at all across 252.
///
/// The adjacency comes from [`CollisionBsp::leaf_adjacency`], which
/// clips a polygon on each node's plane down both subtrees, so every
/// fragment lies between exactly two cells. A fragment is passable where
/// it is neither covered by a surface nor covered by a portal — open
/// space, in other words — and cells joined by a passable fragment are
/// the same region.
fn leaf_regions(
    bsp: &crate::collision_import::CollisionBsp,
    world: [[f32; 2]; 3],
    portals: &[(Vec<[f32; 3]>, [f32; 3])],
    solid: &[bool],
    centres: &[Option<[f32; 3]>],
) -> (Vec<u32>, usize, usize) {
    let n_leaves = bsp.leaf_count();
    let mut parent: Vec<u32> = (0..n_leaves as u32).collect();
    fn find(parent: &mut [u32], mut x: u32) -> u32 {
        while parent[x as usize] != x {
            parent[x as usize] = parent[parent[x as usize] as usize];
            x = parent[x as usize];
        }
        x
    }

    let adj = bsp.leaf_adjacency(world);
    let total = adj.len();
    let mut joined = 0usize;
    for (a, b, frag, plane) in &adj {
        if *a as usize >= n_leaves || *b as usize >= n_leaves {
            continue;
        }
        // Neither cell may be inside a wall: a region stops at solid.
        if solid.get(*a as usize).copied().unwrap_or(false)
            || solid.get(*b as usize).copied().unwrap_or(false)
        {
            continue;
        }
        // Samples across the fragment: its centre, and a point part way
        // from the centre to each corner. A doorway is a fragment part
        // covered by wall and part open, and it only takes one open
        // sample for the two cells to be connected.
        let inv = 1.0 / frag.len() as f64;
        let c = frag.iter().fold([0.0f64; 3], |acc, p| {
            [acc[0] + p[0] * inv, acc[1] + p[1] * inv, acc[2] + p[2] * inv]
        });
        // A portal blocks the *path* between the two cells, not just
        // the plane the fragment sits on. A doorway is an opening with
        // volume: the portal spans it, but the cells either side also
        // meet on fragments beside that plane, and testing only the
        // fragment let the flood walk round the portal through its own
        // doorway. That left one region holding 23,402 of 53,079 cells.
        if let (Some(ca), Some(cb)) =
            (centres.get(*a as usize).copied().flatten(), centres.get(*b as usize).copied().flatten())
        {
            if portals.iter().any(|(ring, pn)| segment_crosses_ring(ca, cb, ring, *pn)) {
                continue;
            }
        }
        let mut open = false;
        let mut probe = vec![c];
        for q in frag.iter() {
            probe.push([
                c[0] + (q[0] - c[0]) * 0.6,
                c[1] + (q[1] - c[1]) * 0.6,
                c[2] + (q[2] - c[2]) * 0.6,
            ]);
        }
        for p in &probe {
            if !bsp.covered_by_surface(*plane, *p) {
                open = true;
                break;
            }
        }
        if !open {
            continue;
        }
        let (ra, rb) = (find(&mut parent, *a), find(&mut parent, *b));
        if ra != rb {
            parent[rb as usize] = ra;
            joined += 1;
        }
    }

    let regions: Vec<u32> = (0..n_leaves as u32).map(|i| find(&mut parent, i)).collect();
    (regions, total, joined)
}

/// Does the segment `a`..`b` pass through this ring?
fn segment_crosses_ring(a: [f32; 3], b: [f32; 3], ring: &[[f32; 3]], n: [f32; 3]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let d = n[0] * ring[0][0] + n[1] * ring[0][1] + n[2] * ring[0][2];
    let da = n[0] * a[0] + n[1] * a[1] + n[2] * a[2] - d;
    let db = n[0] * b[0] + n[1] * b[1] + n[2] * b[2] - d;
    if (da > 0.0) == (db > 0.0) {
        return false;
    }
    let t = da / (da - db);
    let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    point_in_ring(p, ring, n)
}

/// Is this point inside a ring lying in its own plane?
fn point_in_ring(p: [f32; 3], ring: &[[f32; 3]], n: [f32; 3]) -> bool {
    if ring.len() < 3 {
        return false;
    }
    // The point has to be on the ring's plane before being inside it.
    let d = n[0] * ring[0][0] + n[1] * ring[0][1] + n[2] * ring[0][2];
    if (n[0] * p[0] + n[1] * p[1] + n[2] * p[2] - d).abs() > 0.05 {
        return false;
    }
    let axis = (0..3).max_by(|&a, &b| n[a].abs().total_cmp(&n[b].abs())).unwrap_or(0);
    let (u, v) = match axis {
        0 => (1, 2),
        1 => (0, 2),
        _ => (0, 1),
    };
    let pt2 = [p[u], p[v]];
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (c, e) = ([ring[i][u], ring[i][v]], [ring[j][u], ring[j][v]]);
        if (c[1] > pt2[1]) != (e[1] > pt2[1]) {
            let x = c[0] + (pt2[1] - c[1]) / (e[1] - c[1]) * (e[0] - c[0]);
            if pt2[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// A polygon's unit normal, from the first corner that gives one.
fn ring_normal(ring: &[RealPoint3d]) -> [f32; 3] {
    for k in 1..ring.len().saturating_sub(1) {
        let a = [ring[k].x - ring[0].x, ring[k].y - ring[0].y, ring[k].z - ring[0].z];
        let b = [
            ring[k + 1].x - ring[0].x,
            ring[k + 1].y - ring[0].y,
            ring[k + 1].z - ring[0].z,
        ];
        let n = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if l > 1e-12 {
            return [n[0] / l, n[1] / l, n[2] / l];
        }
    }
    [0.0, 0.0, 1.0]
}

/// A uniform grid over the cluster geometry, for nearest-cluster queries.
struct Grid {
    origin: [f32; 3],
    cell: f32,
    dims: [usize; 3],
    cells: Vec<Vec<i16>>,
}

impl Grid {
    fn build(points: &[Vec<[f32; 3]>], world: [[f32; 2]; 3]) -> Self {
        let span = (0..3).fold(0.0f32, |m, k| m.max(world[k][1] - world[k][0])).max(1e-3);
        let n = 64usize;
        let cell = span / n as f32;
        let dims = [n + 1, n + 1, n + 1];
        let mut cells = vec![Vec::new(); dims[0] * dims[1] * dims[2]];
        let origin = [world[0][0], world[1][0], world[2][0]];
        for (ci, pts) in points.iter().enumerate() {
            for p in pts {
                let slot = &mut cells[Self::cell_of(origin, cell, dims, *p)];
                if !slot.contains(&(ci as i16)) {
                    slot.push(ci as i16);
                }
            }
        }
        Self { origin, cell, dims, cells }
    }

    fn cell_of(origin: [f32; 3], cell: f32, dims: [usize; 3], p: [f32; 3]) -> usize {
        let c =
            |k: usize| (((p[k] - origin[k]) / cell).floor().max(0.0) as usize).min(dims[k] - 1);
        (c(2) * dims[1] + c(1)) * dims[0] + c(0)
    }

    /// The nearest cluster, searching outward one shell of cells at a
    /// time so a distant cluster is never measured against.
    fn nearest(&self, points: &[Vec<[f32; 3]>], p: [f32; 3]) -> i16 {
        let base = [
            (((p[0] - self.origin[0]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[0] - 1),
            (((p[1] - self.origin[1]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[1] - 1),
            (((p[2] - self.origin[2]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[2] - 1),
        ];
        let mut seen: Vec<i16> = Vec::new();
        let mut hit_at: Option<usize> = None;
        for r in 0..self.dims[0] {
            let rr = r as isize;
            for dz in -rr..=rr {
                for dy in -rr..=rr {
                    for dx in -rr..=rr {
                        if dx.abs() != rr && dy.abs() != rr && dz.abs() != rr {
                            continue;
                        }
                        let (x, y, z) =
                            (base[0] as isize + dx, base[1] as isize + dy, base[2] as isize + dz);
                        if x < 0 || y < 0 || z < 0 {
                            continue;
                        }
                        let (x, y, z) = (x as usize, y as usize, z as usize);
                        if x >= self.dims[0] || y >= self.dims[1] || z >= self.dims[2] {
                            continue;
                        }
                        for &ci in &self.cells[(z * self.dims[1] + y) * self.dims[0] + x] {
                            if !seen.contains(&ci) {
                                seen.push(ci);
                            }
                        }
                    }
                }
            }
            // One shell past the first hit, so a cluster just beyond it
            // that is actually nearer still gets measured.
            match hit_at {
                Some(first) if r > first => break,
                None if !seen.is_empty() => hit_at = Some(r),
                _ => {}
            }
        }
        if seen.is_empty() {
            return 0;
        }
        let mut best = (f32::MAX, 0i16);
        for ci in seen {
            for q in &points[ci as usize] {
                let d = (0..3).map(|k| (p[k] - q[k]) * (p[k] - q[k])).sum::<f32>();
                if d < best.0 {
                    best = (d, ci);
                }
            }
        }
        best.1
    }

    /// The nearest few clusters, each with the point of its own geometry
    /// that comes closest, in increasing distance.
    fn nearest_few(&self, points: &[Vec<[f32; 3]>], p: [f32; 3], want: usize) -> Vec<(i16, [f32; 3])> {
        let base = [
            (((p[0] - self.origin[0]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[0] - 1),
            (((p[1] - self.origin[1]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[1] - 1),
            (((p[2] - self.origin[2]) / self.cell).floor().max(0.0) as usize)
                .min(self.dims[2] - 1),
        ];
        let mut seen: Vec<i16> = Vec::new();
        let mut hit_at: Option<usize> = None;
        for r in 0..self.dims[0] {
            let rr = r as isize;
            for dz in -rr..=rr {
                for dy in -rr..=rr {
                    for dx in -rr..=rr {
                        if dx.abs() != rr && dy.abs() != rr && dz.abs() != rr {
                            continue;
                        }
                        let (x, y, z) =
                            (base[0] as isize + dx, base[1] as isize + dy, base[2] as isize + dz);
                        if x < 0 || y < 0 || z < 0 {
                            continue;
                        }
                        let (x, y, z) = (x as usize, y as usize, z as usize);
                        if x >= self.dims[0] || y >= self.dims[1] || z >= self.dims[2] {
                            continue;
                        }
                        for &ci in &self.cells[(z * self.dims[1] + y) * self.dims[0] + x] {
                            if !seen.contains(&ci) {
                                seen.push(ci);
                            }
                        }
                    }
                }
            }
            // A couple of shells past the first hit, so the candidate
            // list holds more than whichever cluster happened to be
            // closest — the point of it is to have somewhere to fall
            // back to when a portal is in the way.
            match hit_at {
                Some(first) if r > first + 1 => break,
                None if !seen.is_empty() => hit_at = Some(r),
                _ => {}
            }
        }
        let mut out: Vec<(f32, i16, [f32; 3])> = Vec::new();
        for ci in seen {
            let mut best = (f32::MAX, [0.0f32; 3]);
            for q in &points[ci as usize] {
                let d = (0..3).map(|k| (p[k] - q[k]) * (p[k] - q[k])).sum::<f32>();
                if d < best.0 {
                    best = (d, *q);
                }
            }
            if best.0 < f32::MAX {
                out.push((best.0, ci, best.1));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.truncate(want);
        out.into_iter().map(|(_, ci, q)| (ci, q)).collect()
    }
}


fn with_block<T>(
    root: &mut crate::TagStructMut<'_>,
    path: &str,
    f: impl FnOnce(&mut crate::TagBlockMut<'_>) -> R<T>,
) -> R<T> {
    let mut fld = root
        .field_path_mut(path)
        .ok_or_else(|| SbspError::MissingField(path.into()))?;
    let mut blk = fld
        .as_block_mut()
        .ok_or_else(|| SbspError::MissingField(format!("{path} (not a block)")))?;
    f(&mut blk)
}

fn try_set(el: &mut crate::TagStructMut<'_>, field: &str, v: TagFieldData) -> bool {
    match el.field_mut(field) {
        Some(mut f) => f.set(v).is_ok(),
        None => false,
    }
}

fn write_materials(tag: &mut TagFile, ass: &AssFile, report: &mut SbspReport) -> R<()> {
    let mut root = tag.root_mut();
    with_block(&mut root, "materials", |block| {
        for m in &ass.materials {
            let i = block.add_element();
            let mut e = block.element_mut(i).expect("just added");
            // The shader path. An ASS carries the material's *name*, not
            // the tag path it resolves to, so this is the name as given;
            // resolving it against the shader tree is the caller's job.
            if let Some(mut f) = e.field_mut("render method") {
                let _ = f.set(TagFieldData::TagReference(crate::fields::TagReferenceData {
                    group_tag_and_name: Some((
                        u32::from_be_bytes(*b"rm  "),
                        m.name.clone(),
                    )),
                }));
            }
            try_set(&mut e, "imported material index", TagFieldData::LongInteger(-1));
            try_set(&mut e, "breakable surface index", TagFieldData::CharInteger(-1));
        }
        Ok(())
    })?;
    report.materials = ass.materials.len();
    Ok(())
}

type MeshRef<'a> = (usize, &'a Vec<crate::ass::AssVertex>, &'a Vec<crate::ass::AssTriangle>);

/// The most a single mesh can address.
///
/// Indices are unsigned words and a vertex index is a signed one, so a
/// mesh holds at most 65,535 indices — 21,845 triangles — over at most
/// 32,767 vertices.
const MAX_INDICES_PER_MESH: usize = 65_535;
const MAX_TRIANGLES_PER_MESH: usize = MAX_INDICES_PER_MESH / 3;
const MAX_VERTICES_PER_MESH: usize = 32_767;

/// Cut an authored object into as many meshes as it needs.
///
/// An artist's object is not bound by the tag's per-mesh limits, and one
/// on `070_bsp_010` wants 66,255 indices. Tool answers that by splitting
/// the geometry across meshes, which is why a shipped level has far more
/// meshes than the artist authored objects. Refusing the whole level
/// instead — which is what happened before — loses everything for the
/// sake of one object.
///
/// Each piece carries only the vertices its own triangles use, with the
/// corners renumbered onto them, so a piece is under both limits by
/// construction rather than by hope.
fn split_object(
    verts: &[crate::ass::AssVertex],
    tris: &[crate::ass::AssTriangle],
) -> Vec<(Vec<crate::ass::AssVertex>, Vec<crate::ass::AssTriangle>)> {
    let mut out = Vec::new();
    let mut local: std::collections::HashMap<u32, u32> = Default::default();
    let (mut pv, mut pt) = (Vec::new(), Vec::new());

    for t in tris {
        // Would this triangle push the piece past either limit? A
        // triangle adds three indices and up to three new vertices.
        let fresh = t.v.iter().filter(|c| !local.contains_key(c)).count();
        if !pt.is_empty()
            && (pt.len() + 1 > MAX_TRIANGLES_PER_MESH
                || pv.len() + fresh > MAX_VERTICES_PER_MESH)
        {
            out.push((std::mem::take(&mut pv), std::mem::take(&mut pt)));
            local.clear();
        }
        let mut v = [0u32; 3];
        for (k, &c) in t.v.iter().enumerate() {
            let next = pv.len() as u32;
            v[k] = *local.entry(c).or_insert_with(|| {
                pv.push(verts[c as usize].clone());
                next
            });
        }
        pt.push(crate::ass::AssTriangle { v, ..t.clone() });
    }
    if !pt.is_empty() {
        out.push((pv, pt));
    }
    out
}

/// The world box the clusters occupy, in world units.
///
/// Padded a little: the partition descends a box down the tree and
/// a leaf whose cell reaches the very edge would otherwise get a
/// centre sitting exactly on it.
fn world_box(meshes: &[MeshRef<'_>], portals: &[Vec<RealPoint3d>]) -> [[f32; 2]; 3] {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for (_, verts, _) in meshes {
        for v in verts.iter() {
            for (a, c) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                lo[a] = lo[a].min(c * ASS_TO_WORLD);
                hi[a] = hi[a].max(c * ASS_TO_WORLD);
            }
        }
    }
    // The portals count too. A portal outside the cluster geometry
    // would otherwise sit outside the box, its plane would never be
    // forced into the tree, and both its sides would land in the same
    // cell — 82 of them did.
    for ring in portals {
        for p in ring {
            for (a, c) in [p.x, p.y, p.z].into_iter().enumerate() {
                lo[a] = lo[a].min(c);
                hi[a] = hi[a].max(c);
            }
        }
    }
    let mut out = [[0.0f32; 2]; 3];
    for a in 0..3 {
        if lo[a] > hi[a] {
            return [[-1.0, 1.0]; 3];
        }
        let pad = (hi[a] - lo[a]).max(1.0) * 0.05;
        out[a] = [lo[a] - pad, hi[a] + pad];
    }
    out
}

fn write_world_bounds(tag: &mut TagFile, meshes: &[MeshRef<'_>]) -> R<()> {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for (_, verts, _) in meshes {
        for v in verts.iter() {
            let p = [v.position.x, v.position.y, v.position.z];
            for a in 0..3 {
                lo[a] = lo[a].min(p[a] * ASS_TO_WORLD);
                hi[a] = hi[a].max(p[a] * ASS_TO_WORLD);
            }
        }
    }
    let mut root = tag.root_mut();
    for (a, name) in ["world bounds x", "world bounds y", "world bounds z"].iter().enumerate() {
        if let Some(mut f) = root.field_mut(name) {
            let _ = f.set(TagFieldData::RealBounds(crate::math::RealBounds { lower: lo[a], upper: hi[a] }));
        }
    }
    Ok(())
}

fn write_render_geometry(
    tag: &mut TagFile,
    meshes: &[MeshRef<'_>],
    report: &mut SbspReport,
) -> R<()> {
    for (object, verts, tris) in meshes {
        // Triangles are grouped by material into parts, in first-use
        // order, so each part owns one contiguous index run.
        let mut order: Vec<i32> = Vec::new();
        let mut by_material: std::collections::BTreeMap<i32, Vec<[u32; 3]>> = Default::default();
        for t in tris.iter() {
            if !by_material.contains_key(&t.material) {
                order.push(t.material);
            }
            by_material.entry(t.material).or_default().push(t.v);
        }

        let mut indices: Vec<u32> = Vec::new();
        let mut ranges: Vec<(i32, usize, usize)> = Vec::new();
        for m in &order {
            // `index count` is a signed word, so a run has to stay under
            // 32,768. 10,922 triangles is the largest chunk whose index
            // count does, and being a multiple of three keeps every
            // triangle whole.
            for chunk in by_material[m].chunks(MAX_TRIANGLES_PER_PART) {
                let start = indices.len();
                for t in chunk {
                    indices.extend_from_slice(t);
                }
                ranges.push((*m, start, indices.len() - start));
            }
        }
        // `raw indices` is a `short_integer`. The bit pattern of a
        // vertex index above 32,767 is correct as a `u16`, but any reader
        // that sign-extends it — including this crate's own exporter —
        // gets a nonsense index and silently reassembles the mesh out of
        // the wrong vertices. Refuse rather than emit something whose
        // correctness depends on how the reader widens a word.
        if verts.len() > 32_767 {
            return Err(SbspError::TooManyVertices { object: *object, vertices: verts.len() });
        }
        if indices.len() > 65_535 {
            return Err(SbspError::TooManyIndices { object: *object, indices: indices.len() });
        }

        let mut root = tag.root_mut();
        // The mesh header: parts and subparts.
        with_block(&mut root, "render geometry/meshes", |block| {
            let i = block.add_element();
            let mut mesh = block.element_mut(i).expect("just added");
            try_set(&mut mesh, "rigid node index", TagFieldData::CharInteger(-1));
            try_set(&mut mesh, "index buffer index", TagFieldData::ShortInteger(0));
            // A list, not a strip. `render_model` writes 5 here and the
            // enum offers it, but sbsp geometry is read as a list
            // whatever this says — writing 5 would be a lie that happens
            // to work and would mislead the next reader.
            try_set(&mut mesh, "index buffer type", TagFieldData::CharEnum { value: 0, name: None });

            for (k, (material, start, count)) in ranges.iter().enumerate() {
                if let Some(mut pf) = mesh.field_mut("parts") {
                    if let Some(mut parts) = pf.as_block_mut() {
                        let pi = parts.add_element();
                        let mut part = parts.element_mut(pi).expect("just added");
                        try_set(
                            &mut part,
                            "render method index",
                            TagFieldData::ShortBlockIndex(*material as i16),
                        );
                        try_set(&mut part, "index start", TagFieldData::ShortInteger(*start as i16));
                        try_set(&mut part, "index count", TagFieldData::ShortInteger(*count as i16));
                        try_set(&mut part, "subpart start", TagFieldData::ShortInteger(k as i16));
                        try_set(&mut part, "subpart count", TagFieldData::ShortInteger(1));
                    }
                }
                if let Some(mut sf) = mesh.field_mut("subparts") {
                    if let Some(mut subs) = sf.as_block_mut() {
                        let si = subs.add_element();
                        let mut sub = subs.element_mut(si).expect("just added");
                        try_set(&mut sub, "index start", TagFieldData::ShortInteger(*start as i16));
                        try_set(&mut sub, "index count", TagFieldData::ShortInteger(*count as i16));
                        try_set(&mut sub, "part index", TagFieldData::ShortBlockIndex(k as i16));
                    }
                }
            }
            Ok(())
        })?;

        // The geometry itself.
        with_block(&mut root, "render geometry/per mesh temporary", |block| {
            let i = block.add_element();
            let mut pmt = block.element_mut(i).expect("just added");
            if let Some(mut vf) = pmt.field_mut("raw vertices") {
                if let Some(mut vb) = vf.as_block_mut() {
                    for v in verts.iter() {
                        let k = vb.add_element();
                        let mut e = vb.element_mut(k).expect("just added");
                        try_set(
                            &mut e,
                            "position",
                            TagFieldData::RealPoint3d(RealPoint3d {
                                x: v.position.x * ASS_TO_WORLD,
                                y: v.position.y * ASS_TO_WORLD,
                                z: v.position.z * ASS_TO_WORLD,
                            }),
                        );
                        try_set(
                            &mut e,
                            "normal",
                            TagFieldData::RealPoint3d(RealPoint3d {
                                x: v.normal.i,
                                y: v.normal.j,
                                z: v.normal.k,
                            }),
                        );
                        // V is flipped between ASS and the tag.
                        let uv = v.uvs.first().copied().unwrap_or(RealPoint3d {
                            x: 0.0,
                            y: 0.0,
                            z: 0.0,
                        });
                        try_set(
                            &mut e,
                            "texcoord",
                            TagFieldData::RealPoint2d(RealPoint2d { x: uv.x, y: 1.0 - uv.y }),
                        );
                    }
                }
            }
            if let Some(mut inf) = pmt.field_mut("raw indices") {
                if let Some(mut ib) = inf.as_block_mut() {
                    for idx in &indices {
                        let k = ib.add_element();
                        let mut e = ib.element_mut(k).expect("just added");
                        try_set(&mut e, "word", TagFieldData::ShortInteger(*idx as i16));
                    }
                }
            }
            Ok(())
        })?;

        report.vertices += verts.len();
        report.triangles += tris.len();
    }
    Ok(())
}

/// One cluster per authored mesh, each naming its geometry.
///
/// The mesh index has to be the position in the *whole* mesh list, not
/// in the clusters. With instanced geometry on, the definitions sit in
/// that list too — interleaved, since the authored order is kept — so
/// counting along the clusters alone points every cluster after the
/// first definition at the wrong geometry. On `construct` that moved
/// three of them, and nowhere else, because nowhere else does a cluster
/// follow a definition.
fn write_clusters(
    tag: &mut TagFile,
    meshes: &[MeshRef<'_>],
    mesh_index_of: &std::collections::BTreeMap<usize, usize>,
    report: &mut SbspReport,
) -> R<()> {
    let mut root = tag.root_mut();
    with_block(&mut root, "clusters", |block| {
        for (object, verts, _) in meshes.iter() {
            let k = mesh_index_of.get(object).copied().unwrap_or(0);
            let i = block.add_element();
            let mut c = block.element_mut(i).expect("just added");
            try_set(&mut c, "mesh index", TagFieldData::ShortBlockIndex(k as i16));
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in verts.iter() {
                for (a, x) in [v.position.x, v.position.y, v.position.z].into_iter().enumerate() {
                    lo[a] = lo[a].min(x * ASS_TO_WORLD);
                    hi[a] = hi[a].max(x * ASS_TO_WORLD);
                }
            }
            for (a, name) in ["bounds x", "bounds y", "bounds z"].iter().enumerate() {
                try_set(&mut c, name, TagFieldData::RealBounds(crate::math::RealBounds { lower: lo[a], upper: hi[a] }));
            }
            // No sky, atmosphere or fx assigned: those are scenario-side
            // choices an ASS does not carry.
            for name in ["scenario sky index", "atmosphere index", "camera fx index"] {
                try_set(&mut c, name, TagFieldData::CharInteger(-1));
            }
        }
        Ok(())
    })?;
    report.clusters = meshes.len();
    Ok(())
}
