//! Every source file in the kit, through its importer, to a tag.
//!
//! The corpus tests elsewhere rebuild a shipped tag from the JMS baked
//! into its own `info` stream, which answers "does this match tool?".
//! This one answers a different and more basic question: given the files
//! an artist actually has on disk, does the pipeline run start to finish
//! without an error?
//!
//! The two are not the same. An importer can agree with tool on every
//! model it manages to build and still refuse half the kit. Everything
//! here is a real file in `H3EK/data`, routed by the folder convention
//! the kit itself uses — `render/`, `collision/`, `physics/` for models
//! and `structure/` for a level — and every stage has to survive:
//! parse, import, serialise, and read the tag back.

use std::path::{Path, PathBuf};

use blam_tags::TagFile;

fn h3ek() -> Option<PathBuf> {
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK"))
    .find(|path| path.join("data").is_dir())
}

fn schema(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc")
        .join(format!("{name}.json"));
    p.exists().then_some(p)
}

/// Every file under `root` with this extension, and every directory that
/// could not be read.
///
/// The errors come back rather than being skipped. A directory this
/// cannot open is source files that never reach the importer, and a
/// silent skip there reads exactly like a clean pass — the count was two
/// short of what the kit holds and nothing said so.
fn walk(root: &Path, ext: &str) -> (Vec<PathBuf>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                errors.push(format!("{}: cannot be listed — {e}", dir.display()));
                continue;
            }
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|x| x.to_str())
                .is_some_and(|x| {
                    // Matched on the whole name, not `extension()`. The
                    // kit ships two 11 MB sources named exactly `.JMS`,
                    // with no stem, and a leading dot makes `extension()`
                    // return None — so they were quietly not imported.
                    let x = x.to_ascii_lowercase();
                    x.len() > ext.len() + 0 && x.ends_with(&format!(".{}", ext.to_ascii_lowercase()))
                })
            {
                out.push(p);
            }
        }
    }
    out.sort();
    (out, errors)
}

/// Which importer a source file belongs to, by the folder it sits in.
fn role(path: &Path) -> Option<&'static str> {
    for part in path.components() {
        let s = part.as_os_str().to_string_lossy().to_ascii_lowercase();
        match s.as_str() {
            "render" => return Some("render"),
            "collision" => return Some("collision"),
            "physics" => return Some("physics"),
            _ => {}
        }
    }
    None
}

/// Is the PRT this model carries well formed?
///
/// The importer writing something is not the same as it writing
/// something a reader can use. Measured off the shipped corpus, PRT
/// ambient is twelve bytes a vertex — three floats, one per channel —
/// and an ambient coefficient is a fraction of the sky, so it lies
/// between zero and Y00. A block that is the wrong length for its mesh,
/// or carries a value outside that, is data the engine would read as
/// nonsense while the tag still looks fine.
fn prt_is_well_formed(tag: &TagFile) -> Result<usize, String> {
    const Y00: f32 = 0.282_094_79;
    let root = tag.root();
    let Some(prt) = root.field_path("render geometry/per_mesh_prt_data").and_then(|f| f.as_block())
    else {
        return Ok(0);
    };
    let meshes = root
        .field_path("render geometry/meshes")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .unwrap_or(0);
    if prt.len() != meshes {
        return Err(format!("{} PRT blocks for {meshes} meshes", prt.len()));
    }

    let mut checked = 0usize;
    for i in 0..prt.len() {
        let Some(el) = prt.element(i) else { continue };
        let Some(bytes) = el.field("mesh pca data").and_then(|f| f.as_data()) else { continue };
        if bytes.is_empty() {
            continue;
        }
        if bytes.len() % 12 != 0 {
            return Err(format!("mesh {i}: {} bytes of PRT, not a whole number of triples", bytes.len()));
        }
        for c in bytes.chunks_exact(4) {
            let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            if !v.is_finite() || !(-1e-4..=Y00 + 1e-4).contains(&v) {
                return Err(format!("mesh {i}: a PRT coefficient of {v}, outside 0..={Y00}"));
            }
        }
        checked += bytes.len() / 12;
    }
    Ok(checked)
}

/// A tag has to serialise and read back, or it is not a tag.
fn survives_a_round_trip(tag: &TagFile) -> Result<(), String> {
    let bytes = tag.write_to_bytes().map_err(|e| format!("will not serialise: {e}"))?;
    TagFile::read_from_bytes(&bytes).map_err(|e| format!("will not parse back: {e}"))?;
    Ok(())
}

#[test]
fn every_source_file_in_the_kit_imports() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: need an H3EK install");
        return;
    };
    let (Some(mode), Some(coll), Some(phmo), Some(sbsp)) = (
        schema("render_model"),
        schema("collision_model"),
        schema("physics_model"),
        schema("scenario_structure_bsp"),
    ) else {
        eprintln!("skipping: need the four schemas");
        return;
    };

    let data = kit.join("data");
    let mut failures: Vec<String> = Vec::new();
    let (mut render, mut collision, mut physics, mut structure) = (0, 0, 0, 0);
    let (mut unparsed, mut unrouted) = (0usize, 0usize);
    let mut prt_meshes = 0usize;
    let mut walked = 0usize;
    let mut prt_checked = 0usize;

    let (jms_files, mut walk_errors) = walk(&data, "jms");
    for path in jms_files {
        walked += 1;
        let Some(role) = role(&path) else {
            unrouted += 1;
            continue;
        };
        let name = path.strip_prefix(&data).unwrap_or(&path).display().to_string();

        let Ok(text) = std::fs::read_to_string(&path) else {
            failures.push(format!("{name}: cannot be read"));
            continue;
        };
        let jms = match blam_tags::jms::JmsFile::parse(&text) {
            Ok((jms, _)) => jms,
            Err(e) => {
                // A source the parser cannot read is a real gap, but
                // report it apart from an importer that refuses: they
                // are different repairs.
                unparsed += 1;
                failures.push(format!("{name}: will not parse — {e}"));
                continue;
            }
        };

        let built = match role {
            "render" => blam_tags::render_import::render_model_from_jms(
                &jms,
                &mode,
                &blam_tags::render_import::RenderOptions::default(),
            )
            .map_err(|e| e.to_string())
            .and_then(|(t, r)| {
                prt_meshes += r.prt_vertices;
                match prt_is_well_formed(&t) {
                    Ok(n) => {
                        prt_checked += n;
                        Ok(t)
                    }
                    Err(e) => Err(format!("PRT is malformed: {e}")),
                }
            }),
            "collision" => {
                blam_tags::collision_import::collision_model_from_jms(&jms, &coll, &blam_tags::collision_import::CollisionOptions::default())
                    .map(|(t, _)| t)
                    .map_err(|e| e.to_string())
            }
            "physics" => blam_tags::physics_import::physics_model_from_jms(
                &jms,
                &phmo,
                &blam_tags::physics_import::PhysicsOptions::default(),
            )
            .map(|(t, _)| t)
            .map_err(|e| e.to_string()),
            _ => unreachable!(),
        };

        match built.and_then(|t| survives_a_round_trip(&t).map_err(|e| e)) {
            Ok(()) => match role {
                "render" => render += 1,
                "collision" => collision += 1,
                _ => physics += 1,
            },
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    let (ass_files, ass_errors) = walk(&data, "ass");
    walk_errors.extend(ass_errors);
    for path in ass_files {
        let name = path.strip_prefix(&data).unwrap_or(&path).display().to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            failures.push(format!("{name}: cannot be read"));
            continue;
        };
        let ass = match blam_tags::ass_parse::parse(&text) {
            Ok((ass, _)) => ass,
            Err(e) => {
                failures.push(format!("{name}: will not parse — {e}"));
                continue;
            }
        };
        match blam_tags::sbsp_import::structure_bsp_from_ass(&ass, &sbsp) {
            Ok((tag, _)) => match survives_a_round_trip(&tag) {
                Ok(()) => structure += 1,
                Err(e) => failures.push(format!("{name}: {e}")),
            },
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    eprintln!("from the kit's own source files ({walked} JMS walked):");
    eprintln!("  render_model     {render}");
    eprintln!("  collision_model  {collision}");
    eprintln!("  physics_model    {physics}");
    eprintln!("  structure bsp    {structure}");
    eprintln!("  vertices with PRT {prt_meshes} ({prt_checked} coefficient triples checked)");
    if unrouted > 0 {
        eprintln!("  {unrouted} JMS in no render/collision/physics folder, not routed");
    }
    if unparsed > 0 {
        eprintln!("  {unparsed} would not parse");
    }
    for f in failures.iter().take(20) {
        eprintln!("    {f}");
    }
    if failures.len() > 20 {
        eprintln!("    ... and {} more", failures.len() - 20);
    }

    for e in &walk_errors {
        eprintln!("    {e}");
    }
    assert!(
        render + collision + physics + structure > 0,
        "no source files were found to import"
    );
    assert!(walk_errors.is_empty(), "{} directories could not be listed", walk_errors.len());
    assert!(failures.is_empty(), "{} source files did not import", failures.len());
}
