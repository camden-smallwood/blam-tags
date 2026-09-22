//! `split-jms` — cut oversized sections across extra regions so
//! `tool.exe` will accept them.
//!
//! `tool.exe` refuses a section over 32,767 vertices and never splits
//! anything to make it fit. Shipped content works around that in the
//! source — `lich`'s `exterior01/02/03` are one mesh cut into three —
//! and this does the same cut automatically, by rewriting material
//! definition lines. Geometry is untouched; only which section owns each
//! triangle changes.
//!
//! `--dry-run` reports what would happen and writes nothing, which is
//! the right first move on an asset you have not split before.

use anyhow::{bail, Result};
use std::path::Path;

use blam_tags::jms::JmsFile;
use blam_tags::jms_split::{split_oversized_sections, MaterialLabel, SplitBudget};

pub fn run(
    input: &str,
    output: Option<&str>,
    ratio: Option<f32>,
    max_vertices: Option<usize>,
    dry_run: bool,
) -> Result<()> {
    let text = std::fs::read_to_string(input)
        .map_err(|e| anyhow::anyhow!("{input}: {e}"))?;
    let (mut jms, version) =
        JmsFile::parse(&text).map_err(|e| anyhow::anyhow!("{input}: {e}"))?;

    let mut budget = SplitBudget::default();
    if let Some(r) = ratio {
        if !(r.is_finite() && r > 0.0) {
            bail!("--ratio must be a positive finite number, got {r}");
        }
        budget.vertices_per_triangle = r;
    }
    if let Some(v) = max_vertices {
        if v == 0 {
            bail!("--max-vertices must be greater than zero");
        }
        budget.max_vertices_per_section = v;
    }

    // Show the shape of the file before touching it — the section list is
    // usually the thing you actually want to see.
    let labels: Vec<MaterialLabel> =
        jms.materials.iter().map(|m| MaterialLabel::parse(&m.material_name)).collect();
    let mut sections: std::collections::BTreeMap<(String, String, String), usize> =
        Default::default();
    for tri in &jms.triangles {
        if let Some(label) = labels.get(tri.material as usize) {
            *sections.entry(label.section_key()).or_default() += 1;
        }
    }

    let limit = budget.triangles_per_section();
    println!("{input}  (JMS {version})");
    println!(
        "  {} vertices, {} triangles, {} sections; budget {limit} triangles per section",
        jms.vertices.len(),
        jms.triangles.len(),
        sections.len()
    );
    for ((lod, perm, region), n) in &sections {
        let over = if *n > limit { "  OVER BUDGET" } else { "" };
        let lod = if lod.is_empty() { String::new() } else { format!("{lod} ") };
        println!("    {lod}{perm} {region}: {n} triangles{over}");
    }

    let report = split_oversized_sections(&mut jms, &budget)
        .map_err(|e| anyhow::anyhow!("cannot split {input}: {e}"))?;

    if !report.changed() {
        println!("  nothing over budget — no change needed");
        return Ok(());
    }

    println!(
        "  {} section(s) split: {} -> {} sections, {} -> {} regions",
        report.splits.len(),
        report.sections_before,
        report.sections_after,
        report.regions_before,
        report.regions_after
    );
    for s in &report.splits {
        println!("    {} ({} triangles) -> {}", s.original_region, s.triangles, s.regions.join(", "));
    }
    println!(
        "  largest section is now {} triangles",
        report.largest_section_triangles
    );

    if dry_run {
        println!("  --dry-run: nothing written");
        return Ok(());
    }

    // Default to overwriting in place, which is what a pre-processing
    // step in a build usually wants; `--output` keeps the original.
    let out = output.unwrap_or(input);
    if let Some(dir) = Path::new(out).parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut buf = Vec::new();
    jms.write(&mut buf, version)
        .map_err(|e| anyhow::anyhow!("cannot write JMS: {e}"))?;
    std::fs::write(out, &buf)?;
    println!("  wrote {out} ({} bytes)", buf.len());
    Ok(())
}
