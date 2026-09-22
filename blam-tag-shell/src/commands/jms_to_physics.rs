//! `jms-to-physics` — build a `.physics_model` from a JMS, without
//! `tool.exe`.
//!
//! Reads the JMS's physics primitives (spheres, capsules, boxes, convex
//! shapes), rebuilds each convex hull, and writes a complete `phmo` tag.
//! `tool physics <dir>` is not involved at any point.
//!
//! Two things this deliberately refuses rather than approximating:
//!
//! * A rigid body with **five or more shapes**, which is where Tool
//!   compiles a Havok MOPP. A list without one is not equivalent at that
//!   size, and MOPP cannot be produced outside `tool.exe`.
//! * A convex shape whose points do not form a solid, unless they are
//!   merely coplanar — those get the token thickness Havok's convex
//!   radius would give them anyway.
//!
//! Constraints, phantoms and powered chains are not written. Tool does
//! not author them either; it copies them from the tag it is replacing.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use blam_tags::jms::JmsFile;
use blam_tags::physics_import::{physics_model_from_jms, PhysicsOptions};

use crate::context::CliContext;

#[allow(clippy::too_many_arguments)]
pub fn run(
    ctx: &CliContext,
    input: &str,
    output: Option<&str>,
    scale: Option<f32>,
    convex_radius: Option<f32>,
    overwrite: bool,
) -> Result<()> {
    let game = ctx.require_game("jms-to-physics")?;
    let schema = PathBuf::from("definitions").join(game).join("physics_model.json");
    if !schema.exists() {
        return Err(anyhow!(
            "schema not found: {} (run from the workspace root, or check `definitions/{}/`)",
            schema.display(),
            game,
        ));
    }

    let text = std::fs::read_to_string(input).with_context(|| format!("read {input}"))?;
    let (jms, version) =
        JmsFile::parse(&text).map_err(|e| anyhow!("{input}: {e}"))?;

    let mut opts = PhysicsOptions::default();
    if let Some(s) = scale {
        if !(s.is_finite() && s > 0.0) {
            bail!("--scale must be a positive finite number, got {s}");
        }
        opts.scale = s;
    }
    if let Some(r) = convex_radius {
        if !(r.is_finite() && r > 0.0) {
            bail!("--convex-radius must be a positive finite number, got {r}");
        }
        opts.convex_radius = r;
    }

    let out: PathBuf = match output {
        Some(p) => PathBuf::from(p),
        None => {
            // `.../physics/foo.JMS` is the source-tree layout, so name
            // the tag after the model folder the way Tool would.
            let stem = std::path::Path::new(input)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("physics");
            PathBuf::from(format!("{stem}.physics_model"))
        }
    };
    if out.exists() && !overwrite {
        return Err(anyhow!(
            "refusing to overwrite {} (pass --overwrite if that is what you want)",
            out.display()
        ));
    }

    println!("{input}  (JMS {version})");
    println!(
        "  source: {} spheres, {} capsules, {} boxes, {} convex shapes, {} nodes",
        jms.spheres.len(),
        jms.capsules.len(),
        jms.boxes.len(),
        jms.convex_shapes.len(),
        jms.nodes.len()
    );

    let (tag, report) = physics_model_from_jms(&jms, &schema, &opts)
        .map_err(|e| anyhow!("cannot build a physics_model from {input}: {e}"))?;

    println!(
        "  built:  {} rigid bodies, {} materials, {} regions, {} nodes",
        report.rigid_bodies, report.materials, report.regions, report.nodes
    );
    println!(
        "          {} spheres, {} pills, {} boxes, {} polyhedra, {} lists",
        report.spheres, report.pills, report.boxes, report.polyhedra, report.lists
    );
    if report.dropped_null > 0 {
        println!("  dropped {} primitive(s) named `null`", report.dropped_null);
    }
    for s in &report.skipped {
        println!("  NOT WRITTEN: {s}");
    }

    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    tag.write(&out).with_context(|| format!("write {}", out.display()))?;
    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!("  wrote {} ({size} bytes)", out.display());
    println!(
        "  NOTE: nothing produced by this path has been loaded by the game yet. Verify in \
         Sapien before relying on it."
    );
    Ok(())
}

