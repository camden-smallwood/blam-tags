//! `jms-to-collision` — build a `.collision_model` from a JMS, without
//! `tool.exe`.
//!
//! Welds the JMS's vertices, builds the BSP3D/BSP2D trees and the
//! winged-edge geometry, and writes a complete `coll` tag. `tool model
//! <dir>` is not involved at any point.
//!
//! Two things worth knowing before trusting the output:
//!
//! * A bsp2d leaf holds exactly **one** surface. Coplanar faces that no
//!   2D line separates therefore cannot all be represented, and a few
//!   get dropped — the count is reported. Tool discards them too. On
//!   the shipped corpus this is 0.6% at worst, and 0% on most models.
//! * The tree depth is checked against the game's traversal stack. A
//!   model that would build a deeper tree is refused rather than
//!   written, because the overflow is a crash in the runtime, not here.
//!
//! The ceiling: a permutation may carry up to 64 BSPs and each holds
//! 32,767 surfaces. Against Tool's practical output that is about
//! **1.8x**, not the 7x an early sample suggested.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use blam_tags::collision_import::{collision_model_from_jms, CollisionOptions};
use blam_tags::jms::JmsFile;

use crate::context::CliContext;

pub fn run(
    ctx: &CliContext,
    input: &str,
    output: Option<&str>,
    scale: Option<f32>,
    overwrite: bool,
) -> Result<()> {
    let game = ctx.require_game("jms-to-collision")?;
    let schema = PathBuf::from("definitions").join(game).join("collision_model.json");
    if !schema.exists() {
        return Err(anyhow!(
            "schema not found: {} (run from the workspace root, or check `definitions/{}/`)",
            schema.display(),
            game,
        ));
    }

    let text = std::fs::read_to_string(input).with_context(|| format!("read {input}"))?;
    let (jms, version) = JmsFile::parse(&text).map_err(|e| anyhow!("{input}: {e}"))?;

    let mut opts = CollisionOptions::default();
    if let Some(s) = scale {
        if !(s.is_finite() && s > 0.0) {
            bail!("--scale must be a positive finite number, got {s}");
        }
        opts.scale = s;
    }

    let out: PathBuf = match output {
        Some(p) => PathBuf::from(p),
        None => {
            let stem = std::path::Path::new(input)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("collision");
            PathBuf::from(format!("{stem}.collision_model"))
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
        "  source: {} vertices, {} triangles, {} materials, {} nodes",
        jms.vertices.len(),
        jms.triangles.len(),
        jms.materials.len(),
        jms.nodes.len()
    );

    let (tag, report) = collision_model_from_jms(&jms, &schema, &opts)
        .map_err(|e| anyhow!("cannot build a collision_model from {input}: {e}"))?;

    println!(
        "  built:  {} regions, {} permutations, {} bsps, {} materials, {} nodes",
        report.regions, report.permutations, report.bsps, report.materials, report.nodes
    );
    println!(
        "          {} surfaces, {} edges, {} vertices, {} planes",
        report.surfaces, report.edges, report.vertices, report.planes
    );
    println!(
        "          {} bsp3d nodes, {} leaves, {} bsp2d nodes, {} bsp2d refs, depth {}",
        report.bsp3d_nodes,
        report.leaves,
        report.bsp2d_nodes,
        report.bsp2d_references,
        report.max_depth
    );
    if report.dropped_surfaces > 0 {
        println!(
            "  dropped {} coplanar surface(s) no 2D line separates — that collision will not \
             be there. Tool drops them too.",
            report.dropped_surfaces
        );
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

