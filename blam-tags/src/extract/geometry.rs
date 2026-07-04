//! Scenario geometry extraction: walk a scenario's `structure_bsps[]`,
//! pair each entry with its lighting_info (`.stli`), and emit one source
//! file per BSP.
//!
//! - Halo 2 / Halo 3 → one ASS per BSP (`<out>/<stem>/structure/<bsp>.ASS`).
//!   H2 uses the section-format reader + ASS v2 and has no re-extractable
//!   structure lights; H3 uses the gen3 reader + ASS v7 and layers in the
//!   paired `.stli`'s `GENERIC_LIGHT`s.
//! - Halo CE → render + collision JMS per BSP (CE compiles levels from
//!   JMS, not ASS).
//!
//! Mirrors `blam-tag-shell`'s `extract-geometry` scenario path.

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::game::Game;
use crate::paths::tag_ref_path;
use crate::{AssFile, AssObjectPayload, JmsFile, TagFile};

use super::{ExtractError, TagResolver};

/// One emitted geometry source file.
#[derive(Debug)]
pub struct EmittedGeom {
    /// Index of the `structure_bsps[]` entry this file came from.
    pub bsp_index: usize,
    /// Path the file was written to.
    pub path: PathBuf,
    /// Human-readable content stats (e.g. "73 mats, 597 objects …").
    pub summary: String,
}

/// Outcome of a scenario geometry export.
#[derive(Debug, Default)]
pub struct GeomSummary {
    /// Files written, in `structure_bsps[]` order.
    pub emitted: Vec<EmittedGeom>,
    /// Non-fatal per-BSP notes (missing refs, unreadable lighting, …).
    pub warnings: Vec<String>,
}

/// Extract all structure-BSP geometry from `scenario` into the nested
/// source-tree layout `<out_dir>/<scenario_stem>/structure/`.
pub fn scenario_geometry_to_dir(
    scenario: &TagFile,
    resolver: &dyn TagResolver,
    out_dir: &Path,
    scenario_stem: &str,
) -> Result<GeomSummary, ExtractError> {
    scenario_geometry(scenario, resolver, out_dir, scenario_stem, false)
}

/// Extract all structure-BSP geometry from `scenario`.
///
/// `flat` mirrors the CLI's `--flat`: files land directly under
/// `out_root` as `<scenario_stem>.<bsp_stem>.{ass,…}` instead of the
/// nested `<scenario_stem>/structure/` layout.
pub fn scenario_geometry(
    scenario: &TagFile,
    resolver: &dyn TagResolver,
    out_root: &Path,
    scenario_stem: &str,
    flat: bool,
) -> Result<GeomSummary, ExtractError> {
    let is_ce = Game::of(scenario) == Game::Halo1;
    let is_h2 = Game::of(scenario) == Game::Halo2;

    let bsps_block = scenario
        .root()
        .field_path("structure bsps")
        .and_then(|f| f.as_block())
        .ok_or_else(|| ExtractError::msg("scenario has no `structure bsps` block"))?;
    if bsps_block.is_empty() {
        return Err(ExtractError::msg(
            "scenario has zero structure_bsps entries — nothing to extract",
        ));
    }

    let mut summary = GeomSummary::default();

    for bi in 0..bsps_block.len() {
        let entry = bsps_block.element(bi).unwrap();
        let bsp_ref_path = tag_ref_path(&entry, "structure bsp");
        let lighting_ref_path = tag_ref_path(&entry, "structure lighting_info");

        let Some(bsp_rel) = bsp_ref_path else {
            summary
                .warnings
                .push(format!("structure_bsps[{bi}]: no structure_bsp ref — skipped"));
            continue;
        };

        let bsp_tag = match resolver.resolve(
            &bsp_rel,
            "scenario_structure_bsp",
            u32::from_be_bytes(*b"sbsp"),
        ) {
            Ok(t) => t,
            Err(e) => {
                summary
                    .warnings
                    .push(format!("structure_bsps[{bi}]: read `{bsp_rel}` failed — {e}"));
                continue;
            }
        };

        let bsp_stem = bsp_rel.rsplit('\\').next().unwrap_or("bsp").to_owned();

        // Halo CE: emit render + collision JMS per BSP (no ASS, no stli).
        if is_ce {
            let bsp_root = if flat {
                out_root.to_path_buf()
            } else {
                out_root.join(scenario_stem).join("structure")
            };
            let prefix = if flat {
                format!("{scenario_stem}.{bsp_stem}")
            } else {
                bsp_stem.clone()
            };
            match emit_ce_bsp_jms(&bsp_tag, &bsp_root, &prefix, flat) {
                Ok(items) => summary.emitted.extend(items.into_iter().map(|(path, s)| {
                    EmittedGeom {
                        bsp_index: bi,
                        path,
                        summary: s,
                    }
                })),
                Err(e) => summary
                    .warnings
                    .push(format!("structure_bsps[{bi}]: `{bsp_rel}` — {e}")),
            }
            continue;
        }

        // Halo 2 emits ASS v2 with no recoverable structure lights; Halo 3
        // emits v7 and layers in the paired stli's GENERIC_LIGHTs.
        let ass_version: u32 = if is_h2 { 2 } else { 7 };
        let mut ass = if is_h2 {
            AssFile::from_scenario_structure_bsp_h2(&bsp_tag).map_err(|e| {
                ExtractError::msg(format!("structure_bsps[{bi}]: build H2 ASS from `{bsp_rel}`: {e}"))
            })?
        } else {
            AssFile::from_scenario_structure_bsp(&bsp_tag).map_err(|e| {
                ExtractError::msg(format!("structure_bsps[{bi}]: build ASS from `{bsp_rel}`: {e}"))
            })?
        };

        if is_h2 {
            // H2 has no stli; nothing to layer in.
        } else if let Some(lighting_rel) = lighting_ref_path {
            match resolver.resolve(
                &lighting_rel,
                "scenario_structure_lighting_info",
                u32::from_be_bytes(*b"stli"),
            ) {
                Ok(stli) => {
                    if let Err(e) = ass.add_lights_from_stli(&stli) {
                        summary
                            .warnings
                            .push(format!("structure_bsps[{bi}]: lighting layer failed — {e}"));
                    }
                }
                Err(e) => summary.warnings.push(format!(
                    "structure_bsps[{bi}]: lighting tag `{lighting_rel}` unreadable — {e}"
                )),
            }
        } else {
            summary.warnings.push(format!(
                "structure_bsps[{bi}]: no lighting_info ref — emitting without lights"
            ));
        }

        let path = if flat {
            out_root.join(format!("{scenario_stem}.{bsp_stem}.ass"))
        } else {
            out_root
                .join(scenario_stem)
                .join("structure")
                .join(format!("{bsp_stem}.ASS"))
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut writer = BufWriter::new(File::create(&path)?);
        ass.write_version(&mut writer, ass_version)?;

        let total_verts: usize = ass.objects.iter().map(|o| o.vertices_len()).sum();
        let total_tris: usize = ass.objects.iter().map(|o| o.triangles_len()).sum();
        let light_count = ass
            .objects
            .iter()
            .filter(|o| matches!(&o.payload, AssObjectPayload::GenericLight(_)))
            .count();
        summary.emitted.push(EmittedGeom {
            bsp_index: bi,
            path,
            summary: format!(
                "{} mats, {} objects ({} lights), {} instances, {} verts, {} tris",
                ass.materials.len(),
                ass.objects.len(),
                light_count,
                ass.instances.len(),
                total_verts,
                total_tris,
            ),
        });
    }

    if summary.emitted.is_empty() {
        return Err(ExtractError::msg(
            "no geometry emitted — all structure_bsps entries failed to load",
        ));
    }
    Ok(summary)
}

/// Emit a Halo CE structure BSP as render + collision JMS. `stem` names
/// the output files; returns `(path, summary)` per file written.
pub fn emit_ce_bsp_jms(
    tag: &TagFile,
    out_root: &Path,
    stem: &str,
    flat: bool,
) -> Result<Vec<(PathBuf, String)>, ExtractError> {
    let version = Game::Halo1.jms_version();
    let mut items = Vec::new();

    let render = JmsFile::from_scenario_structure_bsp_ce(tag)?;
    let rpath = ce_output_path(out_root, stem, "render", flat, "jms");
    write_jms(&rpath, &render, version)?;
    items.push((rpath, format!("[render: JMS] {}", jms_summary(&render))));

    let coll = JmsFile::from_scenario_structure_bsp_ce_collision(tag)?;
    let cpath = ce_output_path(out_root, stem, "collision", flat, "jms");
    write_jms(&cpath, &coll, version)?;
    items.push((cpath, format!("[collision] {}", jms_summary(&coll))));

    Ok(items)
}

/// CE JMS output path: nested `<out_root>/<stem>/<kind>/<stem>.<EXT>` or
/// flat `<out_root>/<stem>.<kind>.<ext>`. Matches the CLI's
/// `output_path_for`.
fn ce_output_path(out_root: &Path, stem: &str, kind: &str, flat: bool, ext: &str) -> PathBuf {
    if flat {
        out_root.join(format!("{stem}.{kind}.{ext}"))
    } else {
        out_root
            .join(stem)
            .join(kind)
            .join(format!("{stem}.{}", ext.to_uppercase()))
    }
}

fn write_jms(path: &Path, jms: &JmsFile, version: u16) -> Result<(), ExtractError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut writer = BufWriter::new(File::create(path)?);
    jms.write(&mut writer, version)?;
    Ok(())
}

fn jms_summary(jms: &JmsFile) -> String {
    let mut parts = Vec::new();
    if !jms.nodes.is_empty() {
        parts.push(format!("{} nodes", jms.nodes.len()));
    }
    if !jms.materials.is_empty() {
        parts.push(format!("{} mats", jms.materials.len()));
    }
    if !jms.markers.is_empty() {
        parts.push(format!("{} markers", jms.markers.len()));
    }
    if !jms.vertices.is_empty() {
        parts.push(format!("{} verts", jms.vertices.len()));
    }
    if !jms.triangles.is_empty() {
        parts.push(format!("{} tris", jms.triangles.len()));
    }
    if !jms.spheres.is_empty() {
        parts.push(format!("{} spheres", jms.spheres.len()));
    }
    if !jms.boxes.is_empty() {
        parts.push(format!("{} boxes", jms.boxes.len()));
    }
    if !jms.capsules.is_empty() {
        parts.push(format!("{} capsules", jms.capsules.len()));
    }
    if !jms.convex_shapes.is_empty() {
        parts.push(format!("{} convex", jms.convex_shapes.len()));
    }
    if !jms.ragdolls.is_empty() {
        parts.push(format!("{} ragdolls", jms.ragdolls.len()));
    }
    if !jms.hinges.is_empty() {
        parts.push(format!("{} hinges", jms.hinges.len()));
    }
    parts.join(", ")
}
