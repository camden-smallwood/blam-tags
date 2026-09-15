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
use blam_tags::{AssFile, JmsFile, TagFieldData, TagFile};

use crate::context::{CliContext, CtxResolver};
use blam_tags::paths::{tag_ref_path, tag_stem};

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
        // Halo CE references a `gbxmodel` (mod2) directly — there's no
        // `.model` (hlmt) wrapper — so accept it as a direct render input.
        b"mod2" => {
            reject_hlmt_only_args(kinds, force, "gbxmodel")?;
            run_gbxmodel(ctx, output, flat)
        }
        // Direct render input. H2/H3 store render geometry in a
        // standalone `render_model` (mode); objects reach it via the
        // `.model` (hlmt) wrapper, but accept it directly too for a
        // plain render JMS (matching the `mod2`/`coll`/`phmo` direct
        // paths). The JMS-vs-ASS instance-geometry decision is hlmt-only;
        // a bare `mode` always emits JMS.
        b"mode" => {
            reject_hlmt_only_args(kinds, force, "render_model")?;
            run_render_model(ctx, output, flat)
        }
        // Direct collision input. Halo CE collision lives in a standalone
        // `model_collision_geometry`, and CE objects reference it directly
        // (no `.model` wrapper), so it's the only way to reach CE collision
        // geometry. H2/H3 `collision_model` is also accepted here for a
        // standalone BSP-local-space dump (no skeleton composition).
        b"coll" => {
            reject_hlmt_only_args(kinds, force, "collision_model")?;
            run_collision(ctx, output, flat)
        }
        // Direct physics input. H2/H3 standalone `physics_model` → JMS
        // shapes. No skeleton on a standalone tag, so shape transforms
        // stay node-local (pass the `.model` for skeleton composition).
        b"phmo" => {
            reject_hlmt_only_args(kinds, force, "physics_model")?;
            run_physics(ctx, output, flat)
        }
        // `particle_model` — the merged mesh Tool builds from a JMI
        // manifest. Splits back into one JMS per listed object plus the
        // `.jmi` that indexes them. `pmdf` is H3/Reach/H4; `PRTM` is
        // Halo 2's unrelated (and richer — it keeps object names) tag.
        b"pmdf" | b"PRTM" => {
            reject_hlmt_only_args(kinds, force, "particle_model")?;
            run_particle_model(ctx, output, flat)
        }
        _ => anyhow::bail!(
            "extract-geometry expects `.model` (hlmt), `.render_model` (mode), \
             `.gbxmodel` (mod2, Halo CE), \
             `.collision_model`/`.model_collision_geometry` (coll), `.physics_model` (phmo), \
             `.particle_model` (pmdf/PRTM), \
             `.scenario` (scnr), or `.scenario_structure_bsp` (sbsp) — got group `{}`.",
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

/// Build the render-model JMS using the engine-correct reader. Halo 2
/// stores render geometry in a section-based structure distinct from
/// Halo 3+'s `render geometry/per mesh temporary`.
fn read_render_jms(tag: &TagFile, game: blam_tags::game::Game) -> Result<JmsFile> {
    use blam_tags::game::Game;
    Ok(match game {
        Game::Halo1 => JmsFile::from_gbxmodel(tag)?,
        Game::Halo2 => JmsFile::from_h2_render_model(tag)?,
        Game::Halo3 => JmsFile::from_render_model(tag)?,
    })
}

/// Build the physics_model JMS using the engine-correct reader. Halo 2
/// stores each Havok shape FLAT (name/material/radius/transform on the
/// shape block, parented by name) whereas Halo 3 uses nested substructs
/// + a rigid-body shape reference. Halo CE has no `physics_model` (it
/// uses the older `physics` tag), so it never reaches here.
fn read_physics_jms(tag: &TagFile, skeleton: &[blam_tags::JmsNode], game: blam_tags::game::Game) -> Result<JmsFile> {
    use blam_tags::game::Game;
    Ok(match game {
        Game::Halo2 => JmsFile::from_physics_model_h2_with_skeleton(tag, skeleton)?,
        _ => JmsFile::from_physics_model_with_skeleton(tag, skeleton)?,
    })
}

fn run_hlmt(
    ctx: &mut CliContext,
    kinds: &[String],
    output: Option<&str>,
    flat: bool,
    force: Option<Force>,
) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;

    // Engine drives both the render-model reader (Halo 2 uses a different
    // tag structure) and the JMS text-format version (CE 8200 / H2 8210 /
    // H3+ 8213). The `.model` and every tag it references share the
    // engine, so resolve once from the loaded tag.
    let game = blam_tags::game::Game::of(&loaded.tag);
    let jms_version = game.jms_version();

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

    let stem = tag_stem(&loaded.path, "model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    let render_ref = tag_ref_path(&loaded.tag.root(), Kind::Render.model_field());
    let collision_ref = tag_ref_path(&loaded.tag.root(), Kind::Collision.model_field());
    let physics_ref = tag_ref_path(&loaded.tag.root(), Kind::Physics.model_field());

    // Always load the render_model first when ANY kind is selected:
    // render-side dispatch needs it, and coll/phmo need its skeleton.
    let render_tag = match &render_ref {
        Some(r) => Some(
            ctx.load_referenced_tag(r, Kind::Render.extension())
                .with_context(|| format!("read render_model `{r}`"))?,
        ),
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
            Some(read_render_jms(t, game).context("build render_model JMS")?),
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
                            .unwrap_or_else(|| read_render_jms(rt, game))
                            .context("build render_model JMS")?;
                        let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                        write_to(&path, |w| Ok(jms.write(w, jms_version)?))?;
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
            Kind::Collision => match (&collision_ref, skeleton) {
                (Some(r), Some(skel)) => {
                    let t = ctx
                        .load_referenced_tag(r, Kind::Collision.extension())
                        .with_context(|| format!("read collision_model `{r}`"))?;
                    let jms = JmsFile::from_collision_model_with_skeleton(&t, skel)
                        .context("build collision_model JMS")?;
                    let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                    write_to(&path, |w| Ok(jms.write(w, jms_version)?))?;
                    emitted.push((kind, path, format!("[collision] {}", jms_summary(&jms))));
                }
                (Some(_), None) => skipped.push((kind, "needs render_model for skeleton".to_owned())),
                (None, _) => skipped.push((kind, "no collision_model reference".to_owned())),
            },
            Kind::Physics => match (&physics_ref, skeleton) {
                (Some(r), Some(skel)) => {
                    let t = ctx
                        .load_referenced_tag(r, Kind::Physics.extension())
                        .with_context(|| format!("read physics_model `{r}`"))?;
                    let jms = read_physics_jms(&t, skel, game)
                        .context("build physics_model JMS")?;
                    let path = output_path_for(&out_root, &stem, kind, flat, "jms");
                    write_to(&path, |w| Ok(jms.write(w, jms_version)?))?;
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

/// Walk a scenario's `structure_bsps[]` and emit one ASS (H2/H3) or
/// render + collision JMS (Halo CE) per BSP, via the shared
/// [`blam_tags::extract::geometry`] orchestration. Output layout:
/// - default: `<DIR>/<scenario_stem>/structure/<bsp_stem>.ASS`
/// - `--flat`: `<DIR>/<scenario_stem>.<bsp_stem>.ass`
fn run_scenario(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let scenario_stem = tag_stem(&loaded.path, "scenario");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let resolver = CtxResolver { ctx: &*ctx };
    let summary = blam_tags::extract::geometry::scenario_geometry(
        &loaded.tag,
        &resolver,
        &out_root,
        &scenario_stem,
        flat,
    )?;
    for emitted in &summary.emitted {
        println!(
            "{}: [bsp{}] {}",
            emitted.path.display(),
            emitted.bsp_index,
            emitted.summary,
        );
    }
    for w in &summary.warnings {
        eprintln!("warning: {w}");
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
/// Halo CE direct `gbxmodel` → render JMS (8200). CE has no `.model`
/// wrapper and no instance/physics in the gbxmodel itself, so this emits
/// a single render JMS.
fn run_gbxmodel(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let game = blam_tags::game::Game::of(&loaded.tag);
    let jms = JmsFile::from_gbxmodel(&loaded.tag).context("build gbxmodel JMS")?;
    let stem = tag_stem(&loaded.path, "gbxmodel");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = output_path_for(&out_root, &stem, Kind::Render, flat, "jms");
    write_to(&path, |w| Ok(jms.write(w, game.jms_version())?))?;
    println!("{}: [render: JMS] {}", path.display(), jms_summary(&jms));
    Ok(())
}

/// Direct `mode` input → render JMS. H2/H3 store render geometry in a
/// standalone `render_model` referenced by the `.model` (hlmt) wrapper;
/// this dumps it directly (engine-dispatched reader) without skeleton
/// composition beyond the model's own nodes. Instance-placement ASS
/// output stays hlmt-only.
fn run_render_model(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    let loaded = ctx.loaded("extract-geometry")?;
    let game = blam_tags::game::Game::of(&loaded.tag);
    let jms = read_render_jms(&loaded.tag, game).context("build render_model JMS")?;
    let stem = tag_stem(&loaded.path, "render_model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = output_path_for(&out_root, &stem, Kind::Render, flat, "jms");
    write_to(&path, |w| Ok(jms.write(w, game.jms_version())?))?;
    println!("{}: [render: JMS] {}", path.display(), jms_summary(&jms));
    Ok(())
}

/// Direct `coll` input → render-less collision JMS. Dispatches on
/// engine: Halo CE's `model_collision_geometry` stores BSPs per-node
/// (no region/permutation nesting), H2/H3's `collision_model` stores
/// them per region/permutation. Vertices stay in their tag-local space
/// (no skeleton to compose against on a standalone collision tag).
fn run_collision(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    use blam_tags::game::Game;
    let loaded = ctx.loaded("extract-geometry")?;
    let game = Game::of(&loaded.tag);
    let jms = match game {
        Game::Halo1 => JmsFile::from_model_collision_geometry(&loaded.tag)
            .context("build model_collision_geometry JMS")?,
        _ => JmsFile::from_collision_model(&loaded.tag)
            .context("build collision_model JMS")?,
    };
    let stem = tag_stem(&loaded.path, "collision_model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = output_path_for(&out_root, &stem, Kind::Collision, flat, "jms");
    write_to(&path, |w| Ok(jms.write(w, game.jms_version())?))?;
    println!("{}: [collision] {}", path.display(), jms_summary(&jms));
    Ok(())
}

/// Direct `phmo` input → physics JMS shapes. Dispatches on engine (H2
/// flat shapes vs H3 nested). No skeleton on a standalone tag, so shape
/// transforms stay node-local; pass the parent `.model` for world-space
/// composition against the render skeleton.
fn run_physics(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    use blam_tags::game::Game;
    let loaded = ctx.loaded("extract-geometry")?;
    let game = Game::of(&loaded.tag);
    let jms = read_physics_jms(&loaded.tag, &[], game).context("build physics_model JMS")?;
    let stem = tag_stem(&loaded.path, "physics_model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let path = output_path_for(&out_root, &stem, Kind::Physics, flat, "jms");
    write_to(&path, |w| Ok(jms.write(w, game.jms_version())?))?;
    println!("{}: [physics] {}", path.display(), jms_summary(&jms));
    Ok(())
}

/// Direct `particle_model` input → `.jmi` manifest + one JMS per
/// object, in the layout Tool's `import particle model` reads back.
///
/// Halo 2's `PRTM` recovers the shipped object names from its
/// `models[].model name` string_ids; the gen3 `pmdf` tag stores none,
/// so names are synthesized from the tag stem and that is reported.
fn run_particle_model(ctx: &mut CliContext, output: Option<&str>, flat: bool) -> Result<()> {
    use blam_tags::extract::particle_model::particle_model_geometry;

    let loaded = ctx.loaded("extract-geometry")?;
    let stem = tag_stem(&loaded.path, "particle_model");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    let summary = particle_model_geometry(&loaded.tag, &out_root, &stem, flat)
        .context("extract particle_model geometry")?;

    for e in &summary.emitted {
        match &e.object {
            Some(name) => println!("{}: [{name}] {}", e.path.display(), e.summary),
            None => println!("{}: [manifest] {}", e.path.display(), e.summary),
        }
    }
    for w in &summary.warnings {
        eprintln!("warning: {w}");
    }
    Ok(())
}

fn run_sbsp(ctx: &mut CliContext, output: Option<&str>) -> Result<()> {
    use blam_tags::game::Game;
    let loaded = ctx.loaded("extract-geometry")?;
    let stem = tag_stem(&loaded.path, "bsp");
    let out_root = output.map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));

    let game = Game::of(&loaded.tag);

    // Halo CE level geometry compiles from JMS, not ASS — dispatch by engine.
    if game == Game::Halo1 {
        for (path, summary) in
            blam_tags::extract::geometry::emit_ce_bsp_jms(&loaded.tag, &out_root, &stem, false)?
        {
            println!("{}: {summary}", path.display());
        }
        return Ok(());
    }

    let path = out_root.join(format!("{stem}.ASS"));

    // Halo 2 stores BSP render geometry in the section format and emits
    // ASS version 2; Halo 3 uses the gen3 mesh format and version 7.
    let ass_version = game.ass_version().unwrap_or(7);
    let ass = if game == Game::Halo2 {
        AssFile::from_scenario_structure_bsp_h2(&loaded.tag)
            .with_context(|| format!("build H2 ASS from {}", loaded.path.display()))?
    } else {
        AssFile::from_scenario_structure_bsp(&loaded.tag)
            .with_context(|| format!("build ASS from {}", loaded.path.display()))?
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    let mut writer = BufWriter::new(File::create(&path)
        .with_context(|| format!("create {}", path.display()))?);
    ass.write_version(&mut writer, ass_version as u32)?;

    let total_verts: usize = ass.objects.iter().map(|o| o.vertices_len()).sum();
    let total_tris: usize = ass.objects.iter().map(|o| o.triangles_len()).sum();
    println!(
        "{}: [sbsp] {} mats, {} objects, {} instances, {} verts, {} tris (no lighting — pass scenario for lights)",
        path.display(), ass.materials.len(), ass.objects.len(), ass.instances.len(), total_verts, total_tris,
    );
    Ok(())
}
