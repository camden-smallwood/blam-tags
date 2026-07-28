//! Probe: extract from a Halo: Campaign Evolved `.model` (hlmt) the pieces
//! that still live in the tags — animations (via `animation`→jmad) and
//! collision geometry (via `collision model`→coll) — and dump the new
//! `skeleton model`→skel (nodes/markers/regions) for the render-mesh
//! linkage investigation.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_model_extract -- \
//!     "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks" [model-substr] [out-dir]
//!
//! With no args it mounts the default Paks dir, picks the first `.model`,
//! and writes under a scratch dir. Pass a substring to target a specific
//! model (e.g. `masterchief`).

use std::collections::BTreeMap;
use std::sync::Arc;

use blam_tags::extract::animation::animations_to_dir;
use blam_tags::extract::{ExtractError, TagResolver};
use blam_tags::file::TagFile;
use blam_tags::game::Game;
use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};
use blam_tags::paths::tag_ref_path;
use blam_tags::JmsFile;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

/// Resolves tag_references against a set of mounted IoStore containers by
/// matching `<reference>-<group_ext>.ubulk` as a path suffix.
struct ContainerResolver {
    archives: Vec<Arc<IoStoreArchive>>,
    /// (normalized-lowercase-path, archive index, original rel path)
    index: Vec<(String, usize, String)>,
}

impl ContainerResolver {
    fn find(&self, reference: &str, group_ext: &str) -> Option<(usize, &str)> {
        let needle = format!(
            "{}-{}.ubulk",
            reference.to_ascii_lowercase().replace('\\', "/"),
            group_ext
        );
        self.index
            .iter()
            .find(|(norm, _, _)| norm.ends_with(&needle))
            .map(|(_, i, rel)| (*i, rel.as_str()))
    }

    fn read_tag(&self, reference: &str, group_ext: &str) -> Result<TagFile, ExtractError> {
        let (i, rel) = self
            .find(reference, group_ext)
            .ok_or_else(|| ExtractError::msg(format!("unresolved {group_ext} ref: {reference}")))?;
        let bytes = self.archives[i]
            .read(rel)
            .map_err(|e| ExtractError::msg(format!("read {rel}: {e}")))?;
        TagFile::read_from_bytes(&bytes)
            .map_err(|e| ExtractError::msg(format!("parse {rel}: {e}")))
    }
}

impl TagResolver for ContainerResolver {
    fn resolve(
        &self,
        reference: &str,
        group_ext: &str,
        _group_tag: u32,
    ) -> Result<TagFile, ExtractError> {
        self.read_tag(reference, group_ext)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let filter = args.next().unwrap_or_default();
    let out = args.next().unwrap_or_else(|| {
        "/private/tmp/claude-501/-Users-camden-Source-Baboon-local/4803b682-de10-4887-907a-9f81ad3d13d0/scratchpad/ce_extract".to_string()
    });

    // Mount every container (skip global — no directory index).
    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .unwrap_or_else(|e| {
            eprintln!("read_dir {paks}: {e}");
            std::process::exit(1);
        })
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut archives: Vec<Arc<IoStoreArchive>> = Vec::new();
    let mut index: Vec<(String, usize, String)> = Vec::new();
    // group_longname -> count, and hlmt paths (normalized) -> (arch, rel)
    let mut by_group: BTreeMap<String, usize> = BTreeMap::new();
    let mut models: Vec<(String, usize, String)> = Vec::new();

    for utoc in &utocs {
        let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
        let archive = Arc::new(archive);
        let ai = archives.len();
        for e in archive.ublock_entries() {
            let Some((_name, group)) = parse_ublock_stem(&e.path) else { continue };
            *by_group.entry(group.to_string()).or_default() += 1;
            let norm = e.path.to_ascii_lowercase().replace('\\', "/");
            if group == "model" {
                models.push((norm.clone(), ai, e.path.clone()));
            }
            index.push((norm, ai, e.path.clone()));
        }
        archives.push(archive);
    }

    println!("mounted {} containers, {} tag entries", archives.len(), index.len());
    println!(
        "groups of interest: model={} skeleton_model={} collision_model={} physics_model={} model_animation_graph={}",
        by_group.get("model").copied().unwrap_or(0),
        by_group.get("skeleton_model").copied().unwrap_or(0),
        by_group.get("collision_model").copied().unwrap_or(0),
        by_group.get("physics_model").copied().unwrap_or(0),
        by_group.get("model_animation_graph").copied().unwrap_or(0),
    );

    models.sort();
    let target = models
        .iter()
        .find(|(norm, _, _)| filter.is_empty() || norm.contains(&filter.to_ascii_lowercase()));
    let Some((norm, ai, rel)) = target else {
        eprintln!("no .model matched filter {filter:?}. sample models:");
        for (norm, _, _) in models.iter().take(20) {
            eprintln!("  {norm}");
        }
        std::process::exit(1);
    };
    println!("\n=== target model: {norm} ===");

    let resolver = ContainerResolver { archives, index };
    let bytes = resolver.archives[*ai].read(rel).expect("read model");
    let model = TagFile::read_from_bytes(&bytes).expect("parse model");
    let root = model.root();

    let anim_ref = tag_ref_path(&root, "animation");
    let skel_ref = tag_ref_path(&root, "skeleton model");
    let coll_ref = tag_ref_path(&root, "collision model");
    println!("  animation      -> {anim_ref:?}");
    println!("  skeleton model -> {skel_ref:?}");
    println!("  collision model-> {coll_ref:?}");

    let stem = norm
        .rsplit('/')
        .next()
        .and_then(|f| f.split('-').next())
        .unwrap_or("model")
        .to_string();
    let out_path = std::path::Path::new(&out);

    // --- 1. ANIMATIONS (via the model's own animation ref -> jmad) ---
    println!("\n=== animations ({stem}) ===");
    match animations_to_dir(&model, &resolver, out_path, &stem) {
        Ok(s) => {
            println!("  written={} skipped={}", s.written, s.skipped);
            for w in s.warnings.iter().take(6) {
                println!("    warn: {w}");
            }
            if s.warnings.len() > 6 {
                println!("    ... +{} more warnings", s.warnings.len() - 6);
            }
        }
        Err(e) => println!("  FAILED: {e}"),
    }

    // --- 2. COLLISION GEOMETRY (via collision model ref -> coll) ---
    println!("\n=== collision geometry ===");
    match &coll_ref {
        None => println!("  (model has no collision model ref)"),
        Some(cref) => match resolver.read_tag(cref, "collision_model") {
            Err(e) => println!("  resolve/parse failed: {e}"),
            Ok(coll) => match JmsFile::from_collision_model(&coll) {
                Err(e) => println!("  from_collision_model failed: {e}"),
                Ok(jms) => {
                    println!(
                        "  decoded: {} verts, {} tris, {} materials, {} nodes",
                        jms.vertices.len(),
                        jms.triangles.len(),
                        jms.materials.len(),
                        jms.nodes.len(),
                    );
                    let dir = out_path.join(&stem).join("physics");
                    let _ = std::fs::create_dir_all(&dir);
                    let dest = dir.join(format!("{stem}.JMS"));
                    match std::fs::File::create(&dest) {
                        Ok(f) => {
                            let mut w = std::io::BufWriter::new(f);
                            let ver = Game::of(&coll).jms_version();
                            match jms.write(&mut w, ver) {
                                Ok(_) => println!("  wrote {} (v{ver})", dest.display()),
                                Err(e) => println!("  write failed: {e}"),
                            }
                        }
                        Err(e) => println!("  create {}: {e}", dest.display()),
                    }
                }
            },
        },
    }

    // --- 3. SKELETON MODEL dump (nodes/markers/regions) ---
    println!("\n=== skeleton model (render-mesh linkage source) ===");
    match &skel_ref {
        None => println!("  (model has no skeleton model ref)"),
        Some(sref) => match resolver.read_tag(sref, "skeleton_model") {
            Err(e) => println!("  resolve/parse failed: {e}"),
            Ok(skel) => dump_skeleton_model(&skel),
        },
    }
}

fn dump_skeleton_model(skel: &TagFile) {
    let root = skel.root();

    // Nodes
    if let Some(block) = root.field_path("nodes").and_then(|f| f.as_block()) {
        let n = block.len();
        let names: Vec<String> = (0..n)
            .filter_map(|i| block.element(i))
            .filter_map(|e| e.read_string_id("name"))
            .collect();
        println!("  nodes: {n}");
        for (i, name) in names.iter().enumerate().take(24) {
            println!("    [{i:>3}] {name}");
        }
        if names.len() > 24 {
            println!("    ... +{} more", names.len() - 24);
        }
    } else {
        println!("  nodes: <no 'nodes' block>");
    }

    // Regions -> permutations
    if let Some(block) = root.field_path("regions").and_then(|f| f.as_block()) {
        println!("  regions: {}", block.len());
        for i in 0..block.len().min(12) {
            let Some(e) = block.element(i) else { continue };
            let rname = e.read_string_id("name").unwrap_or_default();
            let perms = e
                .field_path("permutations")
                .and_then(|f| f.as_block())
                .map(|b| b.len())
                .unwrap_or(0);
            println!("    {rname} ({perms} perms)");
        }
    }

    // Marker groups -> markers
    if let Some(block) = root.field_path("marker groups").and_then(|f| f.as_block()) {
        let groups = block.len();
        let total: usize = (0..groups)
            .filter_map(|i| block.element(i))
            .filter_map(|e| e.field_path("markers").and_then(|f| f.as_block()).map(|b| b.len()))
            .sum();
        println!("  marker groups: {groups} ({total} markers total)");
    }
}
