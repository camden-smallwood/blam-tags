//! `extract-geometry` — geometry source files for any geometry-bearing
//! tag, dispatched on input group:
//!
//! - `hlmt` (`.model`): per-purpose render / collision / physics source
//!   files in the H3EK source-tree layout. Render-side auto-picks JMS
//!   or ASS based on whether the render_model carries
//!   `instance mesh index >= 0` + populated `instance placements[]`
//!   (the brute, decorators, level objects). Coll/phys always JMS.
//!   `--force {jms,ass}` overrides the render-side decision.
//! - `scnr` (`.scenario`): one ASS per `structure_bsps[]` entry,
//!   pairing the referenced sbsp with its lighting_info (.stli).
//!   Always ASS — JMS has no representation for level geometry.
//! - `sbsp` (`.scenario_structure_bsp`): a single ASS file for that
//!   BSP. No paired stli (caller must reach for the scenario to get
//!   lighting), so light objects are absent.
//!
//! The positional `[KINDS...]` arg and `--force` are hlmt-only — both
//! are rejected with a clear error if passed with a scenario or sbsp
//! input. ASS is the only format on those paths.
//!
//! Replaced both `extract-jms` (hlmt → JMS) and `extract-ass`
//! (scnr → per-BSP ASS) — the merged verb is the single entry point
//! for tag → geometry source files. Direct sbsp input is new.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blam_tags::{AssFile, AssObjectPayload, JmsFile, TagFieldData, TagFile};

use crate::context::CliContext;
use crate::paths::{derive_tags_root, resolve_tag_path, tag_ref_path, tag_stem};

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind { Render, Collision, Physics }

impl Kind {
    fn as_str(self) -> &'static str {
        match self { Self::Render => "render", Self::Collision => "collision", Self::Physics => "physics" }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Render => "render_model",
            Self::Collision => "collision_model",
            Self::Physics => "physics_model",
        }
    }
    fn model_field(self) -> &'static str {
        match self {
            Self::Render => "render model",
            Self::Collision => "collision model",
            Self::Physics => "physics_model",
        }
    }
}

/// Render-side output format selector. Collision and physics always
/// emit JMS regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Force { Jms, Ass }

pub fn run(
    ctx: &mut CliContext,
    kinds: &[String],
    output: Option<&str>,
    flat: bool,
    force: Option<Force>,
) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let group = loaded.tag.header.group_tag.to_be_bytes();

    match &group {
        b"hlmt" => run_hlmt(ctx, kinds, output, flat, force),
        b"scnr" => {
            reject_hlmt_only_args(kinds, force, "scenario")?;
            run_scenario(ctx, output, flat)
        }
        b"sbsp" => {
            reject_hlmt_only_args(kinds, force, "scenario_structure_bsp")?;
            run_sbsp(ctx, output)
        }
        _ => anyhow::bail!(
            "extract-geometry expects `.model` (hlmt), `.scenario` (scnr), or \
             `.scenario_structure_bsp` (sbsp) — got group `{}`.",
            std::str::from_utf8(&group).unwrap_or("?"),
        ),
    }
}

/// Reject `[KINDS...]` and `--force` for non-hlmt inputs. Both are
/// hlmt-only — scenario/sbsp always emit ASS over the entire scene.
fn reject_hlmt_only_args(kinds: &[String], force: Option<Force>, input_kind: &str) -> Result<()> {
    if !kinds.is_empty() {
        anyhow::bail!(
            "the [KINDS...] positional (render/collision/physics/all) is `.model`-only — \
             a {input_kind} input always emits ASS over the whole scene. \
             Drop the positional and re-run.",
        );
    }
    if force.is_some() {
        anyhow::bail!(
            "`--force` is `.model`-only — a {input_kind} input must emit ASS \
             (JMS has no representation for level/BSP geometry). \
             Drop `--force` and re-run.",
        );
    }
    Ok(())
}

fn run_hlmt(
    ctx: &mut CliContext,
    kinds: &[String],
    output: Option<&str>,
    flat: bool,
    force: Option<Force>,
) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;

    let selected: HashSet<Kind> = if kinds.is_empty() || kinds.iter().any(|k| k == "all") {
        [Kind::Render, Kind::Collision, Kind::Physics].into_iter().collect()
    } else {
        kinds.iter().filter_map(|k| match k.as_str() {
            "render" => Some(Kind::Render),
            "collision" => Some(Kind::Collision),
            "physics" => Some(Kind::Physics),
            _ => None,
        }).collect()
    };

    let tags_root = derive_tags_root(&loaded.path)
        .context("failed to derive tags root from input path — input must live under a `tags/` directory")?;
    let stem = tag_stem(&loaded.path, "model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    let render_path = resolve_child_ref(&loaded.tag, Kind::Render, &tags_root);
    let collision_path = resolve_child_ref(&loaded.tag, Kind::Collision, &tags_root);
    let physics_path = resolve_child_ref(&loaded.tag, Kind::Physics, &tags_root);

    // Always load the render_model first when ANY kind is selected:
    // render-side dispatch needs it, and coll/phmo need its skeleton.
    let render_tag = match &render_path {
        Some(p) => Some(TagFile::read(p)
            .with_context(|| format!("read render_model {}", p.display()))?),
        None => None,
    };

    // Render-side dispatch.
    let render_format: Option<Force> = if selected.contains(&Kind::Render) {
        let detected = render_tag.as_ref().map(detect_render_format);
        Some(force.or(detected.flatten()).unwrap_or(Force::Jms))
    } else {
        None
    };

    // The skeleton coll/phmo need always comes from the render_model
    // JMS view (even when render-side output is ASS). Build the JMS
    // skeleton on demand if we don't already need it for output.
    let need_skeleton = selected.contains(&Kind::Collision) || selected.contains(&Kind::Physics);
    let render_jms = match &render_tag {
        Some(t) if matches!(render_format, Some(Force::Jms)) || need_skeleton =>
            Some(JmsFile::from_render_model(t).context("build render_model JMS")?),
        _ => None,
    };
    let skeleton = render_jms.as_ref().map(|j| j.nodes.as_slice());

    let mut emitted = Vec::new();
    let mut skipped = Vec::new();

    for kind in [Kind::Render, Kind::Collision, Kind::Physics] {
        if !selected.contains(&kind) { continue; }

        match kind {
            Kind::Render => {
                let Some(rt) = render_tag.as_ref() else {
                    skipped.push((kind, "no render_model reference".to_owned()));
                    continue;
                };
                match render_format.unwrap_or(Force::Jms) {
                    Force::Jms => {
                        let jms = render_jms.clone()
                            .map(Ok)
                            .unwrap_or_else(|| JmsFile::from_render_model(rt))
                            .context("build render_model JMS")?;
                        let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                        write_to(&path, |w| Ok(jms.write(w)?))?;
                        emitted.push((kind, path, format!("[render: JMS]  {}", jms_summary(&jms))));
                    }
                    Force::Ass => {
                        let ass = AssFile::from_render_model(rt)
                            .context("build render_model ASS")?;
                        let path = output_path_for(&out_root, &stem, kind, flat, "ass");
                        write_to(&path, |w| Ok(ass.write(w)?))?;
                        emitted.push((kind, path, format!("[render: ASS]  {}", ass_summary(&ass))));
                    }
                }
            }
            Kind::Collision => match (&collision_path, skeleton) {
                (Some(p), Some(skel)) => {
                    let t = TagFile::read(p)
                        .with_context(|| format!("read collision_model {}", p.display()))?;
                    let jms = JmsFile::from_collision_model_with_skeleton(&t, skel)
                        .context("build collision_model JMS")?;
                    let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                    write_to(&path, |w| Ok(jms.write(w)?))?;
                    emitted.push((kind, path, format!("[collision] {}", jms_summary(&jms))));
                }
                (Some(_), None) => skipped.push((kind, "needs render_model for skeleton".to_owned())),
                (None, _) => skipped.push((kind, "no collision_model reference".to_owned())),
            },
            Kind::Physics => match (&physics_path, skeleton) {
                (Some(p), Some(skel)) => {
                    let t = TagFile::read(p)
                        .with_context(|| format!("read physics_model {}", p.display()))?;
                    let jms = JmsFile::from_physics_model_with_skeleton(&t, skel)
                        .context("build physics_model JMS")?;
                    let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                    write_to(&path, |w| Ok(jms.write(w)?))?;
                    emitted.push((kind, path, format!("[physics]   {}", jms_summary(&jms))));
                }
                (Some(_), None) => skipped.push((kind, "needs render_model for skeleton".to_owned())),
                (None, _) => skipped.push((kind, "no physics_model reference".to_owned())),
            },
        }
    }

    for (_kind, path, summary) in &emitted {
        println!("{}: {}", path.display(), summary);
    }
    for (kind, reason) in &skipped {
        eprintln!("skipped {}: {}", kind.as_str(), reason);
    }
    if emitted.is_empty() {
        anyhow::bail!("nothing emitted — all selected kinds were skipped");
    }
    Ok(())
}

/// Auto-detect render-side format from the render_model tag's
/// `instance mesh index` field. Returns `Some(Ass)` when the tag
/// carries instance geometry; `Some(Jms)` otherwise. Never returns
/// `None` — the caller can still override via `--force`.
fn detect_render_format(tag: &TagFile) -> Option<Force> {
    let root = tag.root();
    let instance_mesh_index = root.field("instance mesh index")
        .and_then(|f| f.value())
        .and_then(|v| match v {
            TagFieldData::LongBlockIndex(n) => Some(n as i64),
            TagFieldData::CustomLongBlockIndex(n) => Some(n as i64),
            TagFieldData::ShortBlockIndex(n) => Some(n as i64),
            TagFieldData::LongInteger(n) => Some(n as i64),
            _ => None,
        });
    let placements_len = root.field("instance placements")
        .and_then(|f| f.as_block())
        .map(|b| b.len())
        .unwrap_or(0);
    if instance_mesh_index.unwrap_or(-1) >= 0 && placements_len > 0 {
        Some(Force::Ass)
    } else {
        Some(Force::Jms)
    }
}

fn resolve_child_ref(tag: &TagFile, kind: Kind, tags_root: &Path) -> Option<PathBuf> {
    let rel = tag_ref_path(&tag.root(), kind.model_field())?;
    Some(resolve_tag_path(tags_root, &rel, kind.extension()))
}

fn output_path_for(out_root: &Path, stem: &str, kind: Kind, flat: bool, ext: &str) -> PathBuf {
    if flat {
        out_root.join(format!("{stem}.{}.{ext}", kind.as_str()))
    } else {
        out_root.join(stem).join(kind.as_str()).join(format!("{stem}.{}", ext.to_uppercase()))
    }
}

fn write_to<F>(path: &Path, f: F) -> Result<()>
where
    F: FnOnce(&mut BufWriter<File>) -> Result<(), Box<dyn std::error::Error>>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut writer = BufWriter::new(File::create(path)
        .with_context(|| format!("create {}", path.display()))?);
    f(&mut writer).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

fn jms_summary(jms: &JmsFile) -> String {
    let mut parts = Vec::new();
    if !jms.nodes.is_empty() { parts.push(format!("{} nodes", jms.nodes.len())); }
    if !jms.materials.is_empty() { parts.push(format!("{} mats", jms.materials.len())); }
    if !jms.markers.is_empty() { parts.push(format!("{} markers", jms.markers.len())); }
    if !jms.vertices.is_empty() { parts.push(format!("{} verts", jms.vertices.len())); }
    if !jms.triangles.is_empty() { parts.push(format!("{} tris", jms.triangles.len())); }
    if !jms.spheres.is_empty() { parts.push(format!("{} spheres", jms.spheres.len())); }
    if !jms.boxes.is_empty() { parts.push(format!("{} boxes", jms.boxes.len())); }
    if !jms.capsules.is_empty() { parts.push(format!("{} capsules", jms.capsules.len())); }
    if !jms.convex_shapes.is_empty() { parts.push(format!("{} convex", jms.convex_shapes.len())); }
    if !jms.ragdolls.is_empty() { parts.push(format!("{} ragdolls", jms.ragdolls.len())); }
    if !jms.hinges.is_empty() { parts.push(format!("{} hinges", jms.hinges.len())); }
    parts.join(", ")
}

fn ass_summary(ass: &AssFile) -> String {
    format!(
        "{} mats, {} objects, {} instances",
        ass.materials.len(),
        ass.objects.len(),
        ass.instances.len(),
    )
}

/// Walk a scenario's `structure_bsps[]`, pair each entry with its
/// stli, and emit one ASS per BSP.
///
/// Output layout:
/// - default: `<DIR>/<scenario_stem>/structure/<bsp_stem>.ASS`
/// - `--flat`: `<DIR>/<scenario_stem>.<bsp_stem>.ass`
fn run_scenario(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let tags_root = derive_tags_root(&loaded.path)
        .context("failed to derive tags root from input path — input must live under a `tags/` directory")?;
    let scenario_stem = tag_stem(&loaded.path, "scenario");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    let bsps_block = loaded.tag.root().field_path("structure bsps").and_then(|f| f.as_block())
        .context("scenario has no `structure bsps` block")?;
    if bsps_block.is_empty() {
        anyhow::bail!("scenario has zero structure_bsps entries — nothing to extract");
    }

    let mut emitted = Vec::new();
    let mut warnings = Vec::new();

    for bi in 0..bsps_block.len() {
        let entry = bsps_block.element(bi).unwrap();
        let bsp_ref_path = tag_ref_path(&entry, "structure bsp");
        let lighting_ref_path = tag_ref_path(&entry, "structure lighting_info");

        let Some(bsp_rel) = bsp_ref_path else {
            warnings.push(format!("structure_bsps[{bi}]: no structure_bsp ref — skipped"));
            continue;
        };
        let bsp_abs = resolve_tag_path(&tags_root, &bsp_rel, "scenario_structure_bsp");
        let bsp_tag = match TagFile::read(&bsp_abs) {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!("structure_bsps[{bi}]: read {} failed — {}", bsp_abs.display(), e));
                continue;
            }
        };

        let mut ass = AssFile::from_scenario_structure_bsp(&bsp_tag)
            .with_context(|| format!("structure_bsps[{bi}]: build ASS from {}", bsp_abs.display()))?;

        if let Some(lighting_rel) = lighting_ref_path {
            let lighting_abs = resolve_tag_path(&tags_root, &lighting_rel, "scenario_structure_lighting_info");
            match TagFile::read(&lighting_abs) {
                Ok(stli) => {
                    if let Err(e) = ass.add_lights_from_stli(&stli) {
                        warnings.push(format!("structure_bsps[{bi}]: lighting layer failed — {e}"));
                    }
                }
                Err(e) => warnings.push(format!(
                    "structure_bsps[{bi}]: lighting tag {} unreadable — {e}", lighting_abs.display()
                )),
            }
        } else {
            warnings.push(format!("structure_bsps[{bi}]: no lighting_info ref — emitting without lights"));
        }

        let bsp_stem = bsp_abs.file_stem().and_then(|s| s.to_str()).unwrap_or("bsp").to_owned();
        let path = if flat {
            out_root.join(format!("{scenario_stem}.{bsp_stem}.ass"))
        } else {
            out_root.join(&scenario_stem).join("structure").join(format!("{bsp_stem}.ASS"))
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut writer = BufWriter::new(File::create(&path)
            .with_context(|| format!("create {}", path.display()))?);
        ass.write(&mut writer)?;
        let total_verts: usize = ass.objects.iter().map(|o| o.vertices_len()).sum();
        let total_tris: usize = ass.objects.iter().map(|o| o.triangles_len()).sum();
        let light_count = ass.objects.iter()
            .filter(|o| matches!(&o.payload, AssObjectPayload::GenericLight(_))).count();
        emitted.push((bi, path, format!(
            "{} mats, {} objects ({} lights), {} instances, {} verts, {} tris",
            ass.materials.len(), ass.objects.len(), light_count,
            ass.instances.len(), total_verts, total_tris,
        )));
    }

    for (bi, path, summary) in &emitted {
        println!("{}: [bsp{bi}] {}", path.display(), summary);
    }
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    if emitted.is_empty() {
        anyhow::bail!("no ASS files emitted — all structure_bsps entries failed to load");
    }
    Ok(())
}

/// Direct sbsp input — emit a single ASS for that BSP.
///
/// Lighting (the per-bsp `.stli` pairing) is unreachable here since
/// we have no scenario context; the ASS is emitted without
/// GENERIC_LIGHT objects. Use `.scenario` input if you need lights.
///
/// Output: `<output_or_cwd>/<sbsp_stem>.ASS` (no nesting — single file).
fn run_sbsp(ctx: &mut CliContext, output: Option<&str>) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let stem = tag_stem(&loaded.path, "bsp");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = out_root.join(format!("{stem}.ASS"));

    let ass = AssFile::from_scenario_structure_bsp(&loaded.tag)
        .with_context(|| format!("build ASS from {}", loaded.path.display()))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut writer = BufWriter::new(File::create(&path)
        .with_context(|| format!("create {}", path.display()))?);
    ass.write(&mut writer)?;

    let total_verts: usize = ass.objects.iter().map(|o| o.vertices_len()).sum();
    let total_tris: usize = ass.objects.iter().map(|o| o.triangles_len()).sum();
    println!(
        "{}: [sbsp] {} mats, {} objects, {} instances, {} verts, {} tris (no lighting — pass scenario for lights)",
        path.display(), ass.materials.len(), ass.objects.len(), ass.instances.len(), total_verts, total_tris,
    );
    Ok(())
}
