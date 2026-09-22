//! `gltf-to-jms` — convert a glTF 2.0 file into Halo intermediate
//! geometry that `tool.exe` can import.
//!
//! `tool.exe`'s own `fbx-to-jms` verb works the same way: it reads the
//! foreign format, builds the importer's intermediate scene, and writes
//! it back out as JMS text. Text is the sanctioned seam, so a new front
//! end only has to reach it.
//!
//! `--split` chains straight into the section splitter, which is what
//! gets a mesh past `tool.exe`'s 32,767-vertices-per-section limit
//! without touching `tool.exe` at all.

use anyhow::{bail, Result};
use std::path::Path;

use blam_tags::gltf::{jms_from_gltf_path, GltfOptions, METRES_TO_JMS};
use blam_tags::jms_split::{split_oversized_sections, SplitBudget};

#[allow(clippy::too_many_arguments)]
pub fn run(
    input: &str,
    output: &str,
    scale: Option<f32>,
    metres: bool,
    keep_axes: bool,
    region: &str,
    permutation: &str,
    split: bool,
    ratio: Option<f32>,
    version: u16,
) -> Result<()> {
    if metres && scale.is_some() {
        bail!("--metres and --scale set the same thing; pass one or the other");
    }
    let scale = if metres { METRES_TO_JMS } else { scale.unwrap_or(1.0) };
    if !(scale.is_finite() && scale > 0.0) {
        bail!("--scale must be a positive finite number, got {scale}");
    }

    let opts = GltfOptions {
        scale,
        y_up_to_z_up: !keep_axes,
        permutation: permutation.to_owned(),
        region: region.to_owned(),
    };

    let in_path = Path::new(input);
    let mut jms = jms_from_gltf_path(in_path, &opts)
        .map_err(|e| anyhow::anyhow!("{input}: {e}"))?;

    println!("{input}");
    println!(
        "  {} nodes, {} markers, {} materials, {} vertices, {} triangles",
        jms.nodes.len(),
        jms.markers.len(),
        jms.materials.len(),
        jms.vertices.len(),
        jms.triangles.len()
    );
    println!(
        "  scale x{scale}, axes {}",
        if keep_axes { "unchanged" } else { "Y-up -> Z-up" }
    );

    if split {
        let mut budget = SplitBudget::default();
        if let Some(r) = ratio {
            if !(r.is_finite() && r > 0.0) {
                bail!("--ratio must be a positive finite number, got {r}");
            }
            budget.vertices_per_triangle = r;
        }
        let report = split_oversized_sections(&mut jms, &budget)
            .map_err(|e| anyhow::anyhow!("cannot split: {e}"))?;
        if report.changed() {
            println!(
                "  split {} section(s): {} -> {} sections, {} -> {} regions",
                report.splits.len(),
                report.sections_before,
                report.sections_after,
                report.regions_before,
                report.regions_after
            );
            for s in &report.splits {
                println!("    {} ({} triangles) -> {}", s.original_region, s.triangles,
                    s.regions.join(", "));
            }
        } else {
            println!(
                "  nothing over budget ({} triangles per section)",
                budget.triangles_per_section()
            );
        }
    }

    let out_path = Path::new(output);
    if let Some(dir) = out_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut buf = Vec::new();
    jms.write(&mut buf, version)
        .map_err(|e| anyhow::anyhow!("cannot write JMS: {e}"))?;
    std::fs::write(out_path, &buf)?;
    println!("  wrote {output} ({} bytes, version {version})", buf.len());
    Ok(())
}
