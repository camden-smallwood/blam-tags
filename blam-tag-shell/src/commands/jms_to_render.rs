//! `jms-to-render` — build a `.render_model` from a JMS, without
//! `tool.exe`.
//!
//! Welds the JMS's vertices, generates tangent frames, stripifies each
//! material's triangles and writes a complete `mode` tag. `tool model
//! <dir>` is not involved at any point.
//!
//! What this does not do, and where that matters:
//!
//! * **PRT is not computed.** Every section is written `No PRT`. Tool's
//!   ray-traced per-vertex transfer is a separate solve; a model without
//!   it lights flatly rather than wrongly.
//! * The welder is close to Tool's but not identical — it lands about
//!   0.886x Tool's vertex count on shipped content, i.e. it welds
//!   slightly harder. Geometry is unaffected; section sizes shift a
//!   little.
//!
//! `--max-vertices` is where the ceiling work shows up. Tool refuses a
//! section over 32,767 vertices; the format holds 65,535. Raise it and
//! the index run binds instead.
//!
//! Measured against tool on byte-identical source geometry, this
//! importer costs **2.018 indices per source triangle against tool's
//! 1.826**, so one section holds about **32,500 triangles** here on the
//! 65,535-index budget against tool's ~35,900.
//!
//! Whether that is a gain depends on the mesh, because tool has the
//! other limit as well:
//!
//! | mesh | tool's binding limit | tool | here |
//! |---|---|---|---|
//! | split, 1.6 verts/triangle | 32,767 vertices | ~20,500 tris | ~32,500 |
//! | welded, 0.65 verts/triangle | 65,535 indices | ~35,900 tris | ~32,500 |
//!
//! So it pays on geometry that splits vertices and costs ~10% on
//! geometry that does not. Indices per *vertex* is the wrong unit for
//! this comparison — this welder keeps a median 0.888x tool's vertices,
//! which flatters or flatters-not depending on the mesh; per triangle
//! is the unit welding cannot move.
//!
//! `--split` cuts anything still over the line into extra regions
//! instead of failing.

use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use blam_tags::jms::JmsFile;
use blam_tags::jms_split::{split_oversized_sections, SplitBudget};
use blam_tags::render_import::{
    render_model_from_jms, RenderOptions, MAX_VERTICES_PER_MESH,
};

use crate::context::CliContext;

#[allow(clippy::too_many_arguments)]
pub fn run(
    ctx: &CliContext,
    input: &str,
    output: Option<&str>,
    scale: Option<f32>,
    max_vertices: Option<usize>,
    split: bool,
    overwrite: bool,
    prt_samples: Option<usize>,
    prt_order: u32,
) -> Result<()> {
    let game = ctx.require_game("jms-to-render")?;
    let schema = PathBuf::from("definitions").join(game).join("render_model.json");
    if !schema.exists() {
        return Err(anyhow!(
            "schema not found: {} (run from the workspace root, or check `definitions/{}/`)",
            schema.display(),
            game,
        ));
    }

    let text = std::fs::read_to_string(input).with_context(|| format!("read {input}"))?;
    let (mut jms, version) = JmsFile::parse(&text).map_err(|e| anyhow!("{input}: {e}"))?;

    let mut opts = RenderOptions::default();
    opts.prt_samples = prt_samples;
    opts.prt_order = prt_order.min(2);
    if let Some(s) = scale {
        if !(s.is_finite() && s > 0.0) {
            bail!("--scale must be a positive finite number, got {s}");
        }
        opts.scale = s;
    }
    if let Some(m) = max_vertices {
        if m == 0 || m > 65_535 {
            bail!("--max-vertices must be between 1 and 65535, got {m}");
        }
        opts.max_vertices_per_mesh = m;
    }

    let out: PathBuf = match output {
        Some(p) => PathBuf::from(p),
        None => {
            let stem = std::path::Path::new(input)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("render");
            PathBuf::from(format!("{stem}.render_model"))
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
        "  source: {} vertices, {} triangles, {} materials, {} nodes, {} markers",
        jms.vertices.len(),
        jms.triangles.len(),
        jms.materials.len(),
        jms.nodes.len(),
        jms.markers.len()
    );

    if split {
        let budget =
            SplitBudget { max_vertices_per_section: opts.max_vertices_per_mesh, ..Default::default() };
        let sr = split_oversized_sections(&mut jms, &budget)
            .map_err(|e| anyhow!("cannot split {input}: {e}"))?;
        if sr.changed() {
            println!(
                "  split:  {} section(s) cut, {} -> {} sections, {} -> {} regions",
                sr.splits.len(),
                sr.sections_before,
                sr.sections_after,
                sr.regions_before,
                sr.regions_after
            );
        } else {
            println!("  split:  nothing was over the line");
        }
    }

    let (tag, report) = render_model_from_jms(&jms, &schema, &opts)
        .map_err(|e| anyhow!("cannot build a render_model from {input}: {e}"))?;

    println!(
        "  built:  {} regions, {} permutations, {} meshes, {} materials, {} nodes, {} markers",
        report.regions,
        report.permutations,
        report.meshes,
        report.materials,
        report.nodes,
        report.markers
    );
    println!(
        "          {} vertices welded to {}, {} triangles as {} indices, largest mesh {}",
        report.source_vertices,
        report.welded_vertices,
        report.triangles,
        report.indices,
        report.largest_mesh_vertices
    );
    if opts.max_vertices_per_mesh != MAX_VERTICES_PER_MESH {
        println!(
            "  NOTE: --max-vertices {} is above tool's own {MAX_VERTICES_PER_MESH}. The format \
             holds 65535, but nothing shipped exercises that range.",
            opts.max_vertices_per_mesh
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

