//! Probe: what does Campaign Evolved collision/physics JMS extraction actually
//! produce, and where does it sit relative to the render JMS?
//!
//! CE `.model` tags carry a `skeleton model` instead of a `render model`, so
//! Baboon's model-geometry export has no skeleton to hand
//! `from_collision_model_with_skeleton` / `from_physics_model_with_skeleton`.
//! This measures the consequence: per-node and overall bounding boxes for the
//! collision/physics geometry with no skeleton, with the skeleton_model's own
//! rest pose, and for the render JMS the same model exports.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_coll_probe -- \
//!     "<paks>" [model-substr]

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use blam_tags::file::TagFile;
use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};
use blam_tags::math::{RealPoint3d, RealQuaternion};
use blam_tags::paths::tag_ref_path;
use blam_tags::{JmsFile, JmsNode};

const DEFAULT_PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
/// `jms.rs` scales world units to JMS inches by 100.
const SCALE: f32 = 100.0;

struct Mount {
    archives: Vec<Arc<IoStoreArchive>>,
    index: Vec<(String, usize, String)>,
}

impl Mount {
    fn read(&self, reference: &str, group_ext: &str) -> Option<TagFile> {
        let needle = format!(
            "{}-{}.ubulk",
            reference.to_ascii_lowercase().replace('\\', "/"),
            group_ext
        );
        let (_, i, rel) = self.index.iter().find(|(norm, _, _)| norm.ends_with(&needle))?;
        let bytes = self.archives[*i].read(rel).ok()?;
        TagFile::read_from_bytes(&bytes).ok()
    }
}

#[derive(Default, Clone, Copy)]
struct Bounds {
    min: [f32; 3],
    max: [f32; 3],
    count: usize,
}

impl Bounds {
    fn new() -> Self {
        Self { min: [f32::MAX; 3], max: [f32::MIN; 3], count: 0 }
    }
    fn add(&mut self, p: RealPoint3d) {
        for (i, v) in [p.x, p.y, p.z].into_iter().enumerate() {
            self.min[i] = self.min[i].min(v);
            self.max[i] = self.max[i].max(v);
        }
        self.count += 1;
    }
    fn size(&self) -> [f32; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
    fn line(&self) -> String {
        if self.count == 0 {
            return "(empty)".into();
        }
        let s = self.size();
        format!(
            "{} verts  min[{:.1} {:.1} {:.1}] max[{:.1} {:.1} {:.1}]  size[{:.1} {:.1} {:.1}]",
            self.count,
            self.min[0], self.min[1], self.min[2],
            self.max[0], self.max[1], self.max[2],
            s[0], s[1], s[2],
        )
    }
}

fn bounds_of(jms: &JmsFile) -> Bounds {
    let mut b = Bounds::new();
    for v in &jms.vertices {
        b.add(v.position);
    }
    b
}

/// The skeleton_model's own rest pose, chained parent→child into world space —
/// the transform set collision vertices are stored relative to. Deliberately NOT
/// the reoriented armature the render JMS emits.
fn skeleton_world(skel: &TagFile) -> Vec<JmsNode> {
    let root = skel.root();
    let Some(block) = root.field_path("nodes").and_then(|f| f.as_block()) else {
        return Vec::new();
    };
    let mut local: Vec<JmsNode> = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let n = block.element(i).unwrap();
        // `skeleton_model_node_block_struct` declares this real_point_3d.
        let t = n.read_point3d("default translation");
        local.push(JmsNode {
            name: n.read_string_id("name").or_else(|| n.read_string("name")).unwrap_or_default(),
            parent: n.read_block_index("parent node"),
            rotation: n.read_quat("default rotation"),
            translation: RealPoint3d { x: t.x * SCALE, y: t.y * SCALE, z: t.z * SCALE },
        });
    }
    let mut world = local.clone();
    for i in 0..world.len() {
        let parent = local[i].parent;
        if parent >= 0 && (parent as usize) < i {
            let p = world[parent as usize].clone();
            let rot: RealQuaternion = p.rotation * local[i].rotation;
            let off = p.rotation * local[i].translation.as_vector();
            world[i].rotation = rot.normalized();
            world[i].translation = p.translation + off;
        }
    }
    world
}

/// Which node each collision BSP hangs off, and how many BSPs there are.
fn collision_node_usage(coll: &TagFile) -> (BTreeMap<i32, usize>, usize) {
    let root = coll.root();
    let mut usage: BTreeMap<i32, usize> = BTreeMap::new();
    let mut bsps = 0usize;
    let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
        return (usage, bsps);
    };
    for ri in 0..regions.len() {
        let region = regions.element(ri).unwrap();
        let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
            continue;
        };
        for pi in 0..perms.len() {
            let perm = perms.element(pi).unwrap();
            let Some(block) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
            for bi in 0..block.len() {
                let e = block.element(bi).unwrap();
                let node = e.read_int_any("node index").map(|v| v as i32).unwrap_or(-1);
                *usage.entry(node).or_default() += 1;
                bsps += 1;
            }
        }
    }
    (usage, bsps)
}

fn report(label: &str, jms: &JmsFile) {
    println!("  {label}: {}", bounds_of(jms).line());
    let mut per_node: HashMap<i16, Bounds> = HashMap::new();
    for v in &jms.vertices {
        let node = v.node_sets.first().map(|(n, _)| *n).unwrap_or(-1);
        per_node.entry(node).or_insert_with(Bounds::new).add(v.position);
    }
    let mut nodes: Vec<_> = per_node.into_iter().collect();
    nodes.sort_by_key(|(n, _)| *n);
    // Origin-clustered geometry is the signature of an uncomposed export: every
    // limb's hull sits on top of the pelvis instead of out at the limb.
    let at_origin = nodes
        .iter()
        .filter(|(_, b)| {
            b.min[0].abs() < 30.0 && b.min[1].abs() < 30.0 && b.min[2].abs() < 30.0
        })
        .count();
    println!("    {} distinct node(s); {at_origin} with geometry within 30 JMS units of the origin", nodes.len());
    // The armature the file actually carries -- separate from where the
    // geometry ended up.
    let mut nb = Bounds::new();
    for n in &jms.nodes {
        nb.add(n.translation);
    }
    let posed = jms.nodes.iter().filter(|n| {
        n.translation.x != 0.0 || n.translation.y != 0.0 || n.translation.z != 0.0
    }).count();
    println!("    emitted skeleton: {posed}/{} node(s) posed; origins {}", jms.nodes.len(), nb.line());
    for (node, b) in nodes.iter().take(6) {
        println!("      node {node:>3}: {}", b.line());
    }
    if nodes.len() > 6 {
        println!("      ... +{} more", nodes.len() - 6);
    }
}

/// A physics JMS carries shapes bound to nodes, not triangles -- so what matters
/// is whether the emitted nodes are posed at all.
fn report_physics(label: &str, jms: &JmsFile) {
    println!(
        "  {label}: {} spheres, {} boxes, {} capsules, {} convex, {} ragdolls, {} hinges, {} nodes",
        jms.spheres.len(),
        jms.boxes.len(),
        jms.capsules.len(),
        jms.convex_shapes.len(),
        jms.ragdolls.len(),
        jms.hinges.len(),
        jms.nodes.len(),
    );
    let mut nb = Bounds::new();
    for n in &jms.nodes {
        nb.add(n.translation);
    }
    println!("    node origins: {}", nb.line());
    let posed = jms.nodes.iter().filter(|n| {
        n.translation.x != 0.0 || n.translation.y != 0.0 || n.translation.z != 0.0
    }).count();
    println!("    {posed}/{} node(s) have a non-zero translation", jms.nodes.len());
    for s in jms.spheres.iter().take(3) {
        println!(
            "      sphere {:?} parent={} r={:.2} at [{:.1} {:.1} {:.1}]",
            s.name, s.parent, s.radius, s.translation.x, s.translation.y, s.translation.z
        );
    }
    // Where each shape actually is, and whether it is bound to a bone at all.
    let orphans = jms.spheres.iter().map(|s| s.parent)
        .chain(jms.capsules.iter().map(|c| c.parent))
        .chain(jms.boxes.iter().map(|b| b.parent))
        .chain(jms.convex_shapes.iter().map(|c| c.parent))
        .filter(|p| *p < 0)
        .count();
    println!("    {orphans} shape(s) with parent = -1 (bound to no bone)");
    let mut cv = Bounds::new();
    for c in &jms.convex_shapes {
        for v in &c.vertices {
            cv.add(*v);
        }
    }
    if cv.count > 0 {
        println!("    convex hull points (node-local): {}", cv.line());
    }
    for b in jms.boxes.iter().take(6) {
        println!(
            "      box {:?} parent={} local[{:.1} {:.1} {:.1}] bone@[{:.1} {:.1} {:.1}]",
            b.name, b.parent, b.translation.x, b.translation.y, b.translation.z,
            jms.nodes.get(b.parent.max(0) as usize).map(|n| n.translation.x).unwrap_or(0.0),
            jms.nodes.get(b.parent.max(0) as usize).map(|n| n.translation.y).unwrap_or(0.0),
            jms.nodes.get(b.parent.max(0) as usize).map(|n| n.translation.z).unwrap_or(0.0)
        );
    }
    for c in jms.convex_shapes.iter().take(3) {
        println!(
            "      convex {:?} parent={} {} pts at [{:.1} {:.1} {:.1}]",
            c.name, c.parent, c.vertices.len(), c.translation.x, c.translation.y, c.translation.z
        );
    }
    for c in jms.capsules.iter().take(3) {
        println!(
            "      capsule {:?} parent={} at [{:.1} {:.1} {:.1}]",
            c.name, c.parent, c.translation.x, c.translation.y, c.translation.z
        );
    }
}

/// Across every CE `.model`: can the skeleton_model supply the transforms the
/// collision/physics export is missing, and do the bone names line up?
fn sweep(mount: &Mount, models: &[(String, usize, String)]) {
    let mut total = 0;
    let mut with_coll = 0;
    let mut with_phys = 0;
    let mut no_skel = 0;
    let mut unresolved_nodes = 0;
    let mut resolved_nodes = 0;
    let mut coll_failed = 0;
    let mut phys_failed = 0;
    let mut grew = 0;
    let mut multi_node = 0;
    let mut phys_misplaced = 0;
    let mut phys_empty = 0;
    let mut bad_models: std::collections::BTreeSet<String> = Default::default();
    let mut problems: Vec<String> = Vec::new();

    for (norm, ai, rel) in models {
        let Ok(bytes) = mount.archives[*ai].read(rel) else { continue };
        let Ok(model) = TagFile::read_from_bytes(&bytes) else { continue };
        let root = model.root();
        let coll_ref = tag_ref_path(&root, "collision model");
        let phys_ref = tag_ref_path(&root, "physics_model")
            .or_else(|| tag_ref_path(&root, "physics model"));
        if coll_ref.is_none() && phys_ref.is_none() {
            continue;
        }
        total += 1;
        let skel = tag_ref_path(&root, "skeleton model")
            .and_then(|r| mount.read(&r, "skeleton_model"));
        let Some(skel) = skel else {
            no_skel += 1;
            problems.push(format!("{norm}: has collision/physics but NO skeleton model"));
            continue;
        };
        let world = skeleton_world(&skel);
        let names: std::collections::HashSet<String> =
            world.iter().map(|n| n.name.to_ascii_lowercase()).collect();

        if let Some(coll) = coll_ref.as_deref().and_then(|r| mount.read(r, "collision_model")) {
            with_coll += 1;
            let (usage, _) = collision_node_usage(&coll);
            if usage.len() > 1 {
                multi_node += 1;
            }
            let coll_root = coll.root();
            let coll_nodes = coll_root.field_path("nodes").and_then(|f| f.as_block());
            for node in usage.keys() {
                let name = coll_nodes
                    .as_ref()
                    .and_then(|b| b.element(*node as usize))
                    .and_then(|e| e.read_string_id("name"))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if names.contains(&name) {
                    resolved_nodes += 1;
                } else {
                    unresolved_nodes += 1;
                    bad_models.insert(norm.clone());
                    problems.push(format!("{norm}: collision node {node} {name:?} not in skeleton"));
                }
            }
            match (
                JmsFile::from_collision_model(&coll),
                JmsFile::from_collision_model_with_skeleton(&coll, &world),
            ) {
                (Ok(plain), Ok(posed)) => {
                    // Did composing actually relocate any hull? Extents alone
                    // miss a pure rotation.
                    let moved = plain.vertices.iter().zip(posed.vertices.iter()).any(|(a, b)| {
                        (a.position.x - b.position.x).abs()
                            + (a.position.y - b.position.y).abs()
                            + (a.position.z - b.position.z).abs()
                            > 0.01
                    });
                    if moved {
                        grew += 1;
                    }
                }
                _ => {
                    coll_failed += 1;
                    problems.push(format!("{norm}: collision decode failed"));
                }
            }
        }
        if let Some(phys) = phys_ref.as_deref().and_then(|r| mount.read(r, "physics_model")) {
            with_phys += 1;
            match JmsFile::from_physics_model_with_skeleton(&phys, &world) {
                Err(_) => {
                    phys_failed += 1;
                    problems.push(format!("{norm}: physics decode failed"));
                }
                Ok(posed) => {
                    // Physics shapes are node-local, so the pose lives entirely
                    // in the emitted nodes: any shape hanging off a bone that is
                    // not at the origin is misplaced without it.
                    let shape_parents: Vec<i32> = posed
                        .spheres
                        .iter()
                        .map(|s| s.parent)
                        .chain(posed.capsules.iter().map(|c| c.parent))
                        .chain(posed.boxes.iter().map(|b| b.parent))
                        .chain(posed.convex_shapes.iter().map(|c| c.parent))
                        .collect();
                    let off_origin = shape_parents.iter().any(|p| {
                        posed
                            .nodes
                            .get(*p as usize)
                            .is_some_and(|n| {
                                n.translation.x != 0.0
                                    || n.translation.y != 0.0
                                    || n.translation.z != 0.0
                            })
                    });
                    if off_origin {
                        phys_misplaced += 1;
                    }
                    if shape_parents.is_empty() {
                        phys_empty += 1;
                    }
                }
            }
        }
    }

    println!("models with collision and/or physics: {total}");
    println!("  collision refs: {with_coll}   physics refs: {with_phys}");
    println!("  no skeleton_model to compose with: {no_skel}");
    println!("  collision bsp bones resolved by name: {resolved_nodes} ok, {unresolved_nodes} unresolved");
    println!("  collision spanning more than one bone: {multi_node}/{with_coll}");
    println!("  collision geometry actually moved once composed: {grew}/{with_coll}");
    println!("  models with any unresolved bone: {}", bad_models.len());
    for m in bad_models.iter() {
        println!("    {m}");
    }
    println!("  physics with shapes on off-origin bones (wrong today): {phys_misplaced}/{with_phys}; {phys_empty} carry no shapes at all");
    println!("  decode failures: collision {coll_failed}, physics {phys_failed}");
    if !problems.is_empty() {
        println!("  problems ({}):", problems.len());
        for p in problems.iter().take(25) {
            println!("    {p}");
        }
        if problems.len() > 25 {
            println!("    ... +{} more", problems.len() - 25);
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let filter = args.next().unwrap_or_else(|| "characters/brute/brute-".to_string());

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut mount = Mount { archives: Vec::new(), index: Vec::new() };
    let mut models: Vec<(String, usize, String)> = Vec::new();
    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        let archive = Arc::new(archive);
        let ai = mount.archives.len();
        for e in archive.ublock_entries() {
            let Some((_n, group)) = parse_ublock_stem(&e.path) else { continue };
            let norm = e.path.to_ascii_lowercase().replace('\\', "/");
            if group == "model" {
                models.push((norm.clone(), ai, e.path.clone()));
            }
            mount.index.push((norm, ai, e.path.clone()));
        }
        mount.archives.push(archive);
    }
    models.sort();

    if filter == "--write" {
        // Mirror exactly what Baboon's model-geometry export now does.
        let target = args.next().unwrap_or_else(|| "pelican".to_string()).to_ascii_lowercase();
        let out = args.next().unwrap_or_else(|| ".".to_string());
        let Some((norm, ai, rel)) = models.iter().find(|(n, _, _)| n.contains(&target)) else {
            eprintln!("no .model matched {target:?}");
            std::process::exit(1);
        };
        let bytes = mount.archives[*ai].read(rel).expect("read model");
        let model = TagFile::read_from_bytes(&bytes).expect("parse model");
        let root = model.root();
        let skel = tag_ref_path(&root, "skeleton model")
            .and_then(|r| mount.read(&r, "skeleton_model"))
            .expect("skeleton_model");
        let rest = JmsFile::skeleton_rest_pose(&skel).expect("rest pose");
        for (field, group, ext) in [
            ("collision model", "collision_model", "collision"),
            ("physics model", "physics_model", "physics"),
        ] {
            let Some(tag) = tag_ref_path(&root, field)
                .or_else(|| tag_ref_path(&root, group))
                .and_then(|r| mount.read(&r, group))
            else {
                continue;
            };
            let mut jms = if ext == "collision" {
                JmsFile::from_collision_model_with_skeleton(&tag, &rest).expect("jms")
            } else {
                JmsFile::from_physics_model_with_skeleton(&tag, &rest).expect("jms")
            };
            jms.reorient_for_campaign_evolved(&skel);
            let stem = norm.rsplit('/').next().and_then(|f| f.split('-').next()).unwrap_or("model");
            let path = std::path::Path::new(&out).join(format!("{stem}.{ext}.jms"));
            let file = std::fs::File::create(&path).expect("create");
            let mut w = std::io::BufWriter::new(file);
            jms.write(&mut w, 8213).expect("write");
            println!("wrote {}", path.display());
        }
        return;
    }

    if filter == "--sweep" {
        sweep(&mount, &models);
        return;
    }

    let needle = filter.to_ascii_lowercase();
    let Some((norm, ai, rel)) = models.iter().find(|(n, _, _)| n.contains(&needle)) else {
        eprintln!("no .model matched {filter:?}");
        std::process::exit(1);
    };
    println!("=== {norm} ===");

    let bytes = mount.archives[*ai].read(rel).expect("read model");
    let model = TagFile::read_from_bytes(&bytes).expect("parse model");
    let root = model.root();
    let skel_ref = tag_ref_path(&root, "skeleton model");
    let coll_ref = tag_ref_path(&root, "collision model");
    let phys_ref = tag_ref_path(&root, "physics_model")
        .or_else(|| tag_ref_path(&root, "physics model"));
    println!("  skeleton model  -> {skel_ref:?}");
    println!("  collision model -> {coll_ref:?}");
    println!("  physics model   -> {phys_ref:?}");
    println!("  render model    -> {:?}", tag_ref_path(&root, "render model"));

    let skel = skel_ref.as_deref().and_then(|r| mount.read(r, "skeleton_model"));
    let world = skel.as_ref().map(|s| skeleton_world(s)).unwrap_or_default();
    println!("\n  skeleton_model rest pose: {} nodes", world.len());
    let mut sb = Bounds::new();
    for n in &world {
        sb.add(n.translation);
    }
    println!("    bone origins: {}", sb.line());

    // The render JMS emits a REORIENTED armature (bones down local +X). Does that
    // stay world-preserving, i.e. same translations, different rotations?
    if let Some(skel_tag) = skel.as_ref() {
        if let Ok(reoriented) = JmsFile::from_ue_meshes(&[], &[], &[], skel_tag) {
            let mut moved = 0;
            let mut rotated = 0;
            for (a, b) in world.iter().zip(reoriented.nodes.iter()) {
                let d = (a.translation.x - b.translation.x).abs()
                    + (a.translation.y - b.translation.y).abs()
                    + (a.translation.z - b.translation.z).abs();
                if d > 0.001 {
                    moved += 1;
                }
                let dot = a.rotation.i * b.rotation.i
                    + a.rotation.j * b.rotation.j
                    + a.rotation.k * b.rotation.k
                    + a.rotation.w * b.rotation.w;
                if dot.abs() < 0.9999 {
                    rotated += 1;
                }
            }
            println!(
                "\n  render-JMS armature vs rest pose: {moved}/{} translations differ, {rotated}/{} rotations differ",
                world.len(),
                world.len()
            );
        }
    }

    if let Some(coll) = coll_ref.as_deref().and_then(|r| mount.read(r, "collision_model")) {
        let (usage, bsps) = collision_node_usage(&coll);
        println!("\n  collision_model: {bsps} bsp(s) across {} node(s)", usage.len());
        let named: Vec<String> = usage
            .keys()
            .take(8)
            .map(|n| {
                world
                    .get(*n as usize)
                    .map(|w| format!("{n}={}", w.name))
                    .unwrap_or_else(|| format!("{n}=?"))
            })
            .collect();
        println!("    nodes: {}", named.join(", "));
        match JmsFile::from_collision_model(&coll) {
            Ok(jms) => report("collision, NO skeleton (what Baboon writes today)", &jms),
            Err(e) => println!("    from_collision_model failed: {e}"),
        }
        if !world.is_empty() {
            match JmsFile::from_collision_model_with_skeleton(&coll, &world) {
                Ok(jms) => report("collision, skeleton_model rest pose", &jms),
                Err(e) => println!("    with_skeleton failed: {e}"),
            }
        }
    }

    if let Some(phys) = phys_ref.as_deref().and_then(|r| mount.read(r, "physics_model")) {
        println!("\n  physics_model:");
        match JmsFile::from_physics_model(&phys) {
            Ok(jms) => report_physics("physics, NO skeleton (what Baboon writes today)", &jms),
            Err(e) => println!("    from_physics_model failed: {e}"),
        }
        if !world.is_empty() {
            match JmsFile::from_physics_model_with_skeleton(&phys, &world) {
                Ok(jms) => report_physics("physics, skeleton_model rest pose", &jms),
                Err(e) => println!("    with_skeleton failed: {e}"),
            }
        }
    }
}
