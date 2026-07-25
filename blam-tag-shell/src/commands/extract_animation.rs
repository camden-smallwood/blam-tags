//! `extract-animation` — decode animations from a
//! `.model_animation_graph`, the bundle `.model` (hlmt) that owns
//! one, or any object-inheriting tag (.biped, .vehicle, .scenery,
//! .weapon, .equipment, …) that points at a .model. Two output
//! formats:
//!
//! - `--format json` — full per-frame transform table for both
//!   static and animated codec streams; useful for diagnostics.
//! - `--format jma` (default) — JMA-family text file (`.JMM/.JMA/.JMT/
//!   .JMZ/.JMO/.JMR/.JMW`) re-importable by Halo content tooling.
//!   The kind is picked from the animation's `animation type` ×
//!   `frame info type` × `internal flags / world relative` (JMW = base
//!   + the world-relative bit). Movement deltas are folded into the
//!   root bone — H3 JMA has no separate movement section. See
//!   [`blam_tags::animation::jma`] for the full layout convention.
//!
//! `<anim>` is optional. When omitted, every animation in the tag is
//! extracted. Otherwise it is an integer index into
//! `definitions/animations[]` or a string-id name.
//!
//! Per-bone rest pose source priority:
//!   1. `render_model.nodes[i]/default {translation, rotation}` —
//!      authoritative; used when we can resolve a render_model.
//!   2. `jmad.additional node data[i]` — denormalized cache inside
//!      the jmad. Populated at jmad-build time from the source
//!      render_model. Per the Foundry maintainer there are rare
//!      discrepancies vs the render_model, so we prefer (1) when
//!      available and fall back to (2) per-bone for synthetic nodes
//!      that the render_model lacks (e.g. `camera_control`).
//!   3. Identity — last resort for bones not found in either source.
//!
//! Resolution by input group:
//!   - `.model_animation_graph` (jmad) → only (2) is reachable.
//!   - `.model` (hlmt) → follow `animation` + `render model` refs,
//!     get both sources.
//!   - object-inheriting (biped/scenery/vehicle/weapon/equipment/…)
//!     → follow `model` ref to a hlmt, then the hlmt case.
//!
//! Output layout matches Tool's `model-animations` source-tree
//! convention (`<source-directory>/animations/`) so the result drops
//! straight into an H3EK source tree alongside `extract-jms`'s
//! `render/` output. Files land as
//! `<root>/<jmad_stem>/animations/<anim_name>.<EXT>`.
//!
//! `--output` semantics:
//!   - omitted → `<root>` = `.` (cwd). Single-anim `json` with no
//!     `--output` still prints to stdout for piping.
//!   - ends in a JMA-family extension or `.json` → exact filename
//!     (single-anim only); skips the `<stem>/animations/` nesting.
//!   - any other path → that path becomes `<root>`.
//!
//! `--flat` flattens to `<root>/<tag_stem>.<anim_name>.<EXT>` (no
//! nested subdirs), matching `extract-jms --flat`. Ignored when
//! `--output` is an exact filename — that path is taken verbatim.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::json;

use blam_tags::animation::classic::{CeAnimation, CeAnimations};
use blam_tags::extract::animation::{
    additional_node_data_is_object_space, build_defaults, halo_bone_reorientation, jma_kind_for,
    resolve_animation_inputs, sanitize, write_ce_group_jma, write_group_jma,
};
use blam_tags::{Animation, AnimationGraph, AnimationGroup, JmaKind, Skeleton, TagFile};

use crate::context::{CliContext, CtxResolver};
use blam_tags::paths::tag_stem;

/// Output format selector for [`run`]. `Jma` writes a JMA-family
/// text file (kind picked from the animation's metadata); `Json`
/// dumps the decoded transforms for diagnostics.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Format {
    /// Bungie JMA-family text (`.JMM` / `.JMA` / `.JMT` / …).
    Jma,
    /// JSON dump of decoded static + animated tracks.
    Json,
}

pub fn run(
    ctx: &mut CliContext,
    anim: Option<&str>,
    output: Option<&str>,
    flat: bool,
    format: Format,
) -> Result<()> {
    let loaded = ctx.loaded("extract-animation")?;

    // Halo CE `model_animations` (group `antr`) predates the gen3
    // codec-pack model entirely — route it through the classic decoder.
    if &loaded.tag.header.group_tag.to_be_bytes() == b"antr" {
        return run_ce(&loaded.tag, &loaded.path, anim, output, flat, format);
    }

    // Resolve the input to an owned (jmad, optional render_model) pair via
    // the shared orchestration, using the CLI's filesystem/cache loader.
    // For a direct jmad input no fresh copy is loaded — `resolved.jmad` is
    // `None` and we reuse `loaded.tag`.
    let resolver = CtxResolver { ctx: &*ctx };
    let resolved = resolve_animation_inputs(&loaded.tag, &resolver)?;
    let jmad_tag: &TagFile = resolved.jmad.as_ref().unwrap_or(&loaded.tag);
    let render_model: Option<&TagFile> = resolved.render_model.as_ref();

    let animation = Animation::new(jmad_tag)
        .with_context(|| format!("failed to walk animations in {}", loaded.path.display()))?;

    if animation.is_empty() {
        anyhow::bail!(
            "tag has no local animations (parent: {:?}) — nothing to extract",
            animation.parent(),
        );
    }

    let skeleton = Skeleton::from_tag(jmad_tag);
    if matches!(format, Format::Jma) && skeleton.is_empty() {
        anyhow::bail!("jmad has no skeleton nodes — JMA export needs a skeleton");
    }

    // Build per-bone defaults from render_model first (authoritative)
    // and fill gaps with the jmad's `additional node data`. Bones
    // missing from both fall back to identity. Reach/H4 store that data
    // in object space, so convert it to parent-local first.
    let object_space = additional_node_data_is_object_space(&animation);
    let defaults = build_defaults(&skeleton, jmad_tag, render_model, object_space);
    // Only Campaign Evolved needs bone reorientation (MetaHuman rig); gate on it.
    let reorient = resolved
        .campaign_evolved
        .then(|| halo_bone_reorientation(&skeleton, &defaults))
        .flatten();

    // Graph tree (`content/modes[]`) drives overlay/replacement base
    // resolution — see `Animation::overlay_base_pose`.
    let graph = AnimationGraph::from_tag(jmad_tag);

    let target = OutputTarget::from_args(output);
    let stem = tag_stem(&loaded.path, "animation");

    let groups: Vec<&AnimationGroup<'_>> = match anim {
        Some(a) => vec![pick_animation(&animation, a)?],
        None => animation.iter().collect(),
    };

    if matches!(target, OutputTarget::ExactFile(_)) && groups.len() > 1 {
        anyhow::bail!(
            "{} animations selected; --output as a filename only works for a single \
             animation. Pass a directory path or omit --output.",
            groups.len(),
        );
    }

    // Single-anim json with no --output keeps the legacy stdout
    // behavior so callers can pipe into jq.
    let json_to_stdout = matches!(format, Format::Json)
        && matches!(target, OutputTarget::Default)
        && groups.len() == 1;

    // Resolve every destination up front so we can fail loudly on
    // post-sanitize name collisions (distinct tag names that scrub to
    // the same on-disk stem) instead of silently overwriting.
    let destinations: Vec<PathBuf> = if json_to_stdout {
        Vec::new()
    } else {
        let resolved: Vec<PathBuf> = groups
            .iter()
            .map(|g| resolve_destination(&target, &stem, g, format, flat))
            .collect();
        check_unique_destinations(&resolved, &groups)?;
        resolved
    };

    let mut skipped = 0usize;
    for (i, group) in groups.iter().enumerate() {
        // Composite / runtime-blend animations carry no codec payload —
        // common in Halo 4 (`locomote`, `aim_locomote_*`, `*_composite`):
        // they're synthesized at runtime from blend axes and have no
        // keyframe data to export. Skip them with a note rather than
        // aborting the whole tag.
        if group.blob.is_empty() {
            eprintln!(
                "skipping '{}' — no codec payload (composite/runtime-blend animation)",
                display_name(group),
            );
            skipped += 1;
            continue;
        }

        let clip = group
            .decode()
            .with_context(|| format!("decode animation '{}'", display_name(group)))?;

        match format {
            Format::Json if json_to_stdout => write_json_stdout(group, &clip)?,
            Format::Json => write_json_file(group, &clip, &destinations[i])?,
            Format::Jma => {
                write_group_jma(
                    group,
                    &clip,
                    &animation,
                    &graph,
                    &skeleton,
                    &defaults,
                    reorient.as_deref(),
                    &stem,
                    &destinations[i],
                )?;
                let kind = jma_kind_for(group);
                println!(
                    "{}: {} frames ({}+1) × {} bones [{}]  movement={:?}",
                    destinations[i].display(),
                    clip.frame_count.saturating_add(1),
                    clip.frame_count,
                    skeleton.len(),
                    kind.extension(),
                    clip.movement.kind,
                );
            }
        }
    }
    if skipped > 0 {
        eprintln!("skipped {skipped} composite/empty animation(s) with no codec payload");
    }
    Ok(())
}

/// Halo CE `model_animations` (antr) extraction. CE stores each
/// animation's frames inline (no gen3 codec pack / tgrc resource), with
/// the skeleton in the tag's own `nodes` block and the rest pose carried
/// implicitly by the static (`default data`) stream — so CE poses are
/// self-contained and need no render_model. Overlays/replacements compose
/// onto the skeleton rest pose (CE has no per-graph base resolution like
/// gen3; aim/look overlays are the common case and compose correctly onto
/// rest). See `blam_tags::animation::classic`.
fn run_ce(
    tag: &TagFile,
    path: &Path,
    anim: Option<&str>,
    output: Option<&str>,
    flat: bool,
    format: Format,
) -> Result<()> {
    let animations = CeAnimations::new(tag);
    if animations.is_empty() {
        anyhow::bail!("model_animations has no animations to extract");
    }
    let skeleton = Skeleton::from_tag(tag);
    if matches!(format, Format::Jma) && skeleton.is_empty() {
        anyhow::bail!("model_animations has no nodes — JMA export needs a skeleton");
    }
    // Halo 1 `antr` — a classic Bungie X-down rig, never Campaign Evolved — so
    // no bone-convention reorientation applies.
    let defaults = build_defaults(&skeleton, tag, None, false);

    let target = OutputTarget::from_args(output);
    let stem = tag_stem(path, "animation");

    let groups: Vec<&CeAnimation<'_>> = match anim {
        Some(a) => {
            let g = a.parse::<usize>().ok().and_then(|i| animations.get(i))
                .or_else(|| animations.find(a))
                .ok_or_else(|| anyhow::anyhow!(
                    "no animation named or indexed '{a}' (use `list-animations` to see names)"))?;
            vec![g]
        }
        None => animations.iter().collect(),
    };

    if matches!(target, OutputTarget::ExactFile(_)) && groups.len() > 1 {
        anyhow::bail!(
            "{} animations selected; --output as a filename only works for a single \
             animation. Pass a directory path or omit --output.",
            groups.len(),
        );
    }

    for group in &groups {
        let clip = group.decode();
        let kind = JmaKind::from_metadata(
            group.animation_type.as_deref(),
            group.frame_info_type.as_deref(),
            group.world_relative,
        );
        let name = group.name.clone().unwrap_or_else(|| format!("anim_{}", group.index));
        let ext = match format { Format::Json => "json", Format::Jma => kind.extension() };
        let filename = format!("{}.{ext}", sanitize(&name));
        let dest = ce_destination(&target, &stem, &filename, flat);

        if matches!(format, Format::Json) {
            ensure_parent_dir(&dest)?;
            let json = serde_json::to_string_pretty(&json!({
                "name": group.name, "type": group.animation_type,
                "frame_info_type": group.frame_info_type, "frame_count": group.frame_count,
                "node_count": group.node_count, "world_relative": group.world_relative,
            }))?;
            fs::write(&dest, json).with_context(|| format!("write {}", dest.display()))?;
            println!("{}", dest.display());
            continue;
        }

        // Base kinds pose against the rest defaults; overlay/replacement
        // compose deltas onto the rest pose (CE base resolution is N/A).
        write_ce_group_jma(group, &clip, &skeleton, &defaults, None, &stem, &dest)?;
        println!("{}: {} frames × {} bones [{}]  movement={:?}",
            dest.display(), clip.frame_count, skeleton.len(), kind.extension(), clip.movement.kind);
    }
    Ok(())
}

/// `resolve_destination` for the CE path — same layout rules, but keyed on
/// a pre-rendered filename rather than an `AnimationGroup`.
fn ce_destination(target: &OutputTarget, stem: &str, filename: &str, flat: bool) -> PathBuf {
    let flat_filename = format!("{stem}.{filename}");
    match (target, flat) {
        (OutputTarget::ExactFile(p), _) => p.clone(),
        (OutputTarget::Root(dir), true) => dir.join(flat_filename),
        (OutputTarget::Root(dir), false) => dir.join(stem).join("animations").join(filename),
        (OutputTarget::Default, true) => PathBuf::from(flat_filename),
        (OutputTarget::Default, false) => PathBuf::from(stem).join("animations").join(filename),
    }
}

/// Resolved meaning of the `--output` argument. The CLI is overloaded:
/// the flag can name a source-tree root (default-shaped) or an exact
/// file path that bypasses the source-tree layout entirely.
enum OutputTarget {
    /// `--output <dir>` — a path that becomes the source-tree root.
    /// Files land at `<dir>/<tag_stem>/animations/<anim_name>.<EXT>`,
    /// matching Tool's `model-animations` source-directory convention.
    Root(PathBuf),
    /// `--output <file>` — a path ending in a JMA-family or `.json`
    /// extension. Skips the source-tree layout; single-anim only.
    ExactFile(PathBuf),
    /// `--output` omitted. Equivalent to `Root(".")`.
    Default,
}

impl OutputTarget {
    fn from_args(output: Option<&str>) -> Self {
        let Some(raw) = output else { return Self::Default };
        let path = PathBuf::from(raw);
        let trailing_slash = raw.ends_with('/') || raw.ends_with(std::path::MAIN_SEPARATOR);
        if trailing_slash || path.is_dir() {
            return Self::Root(path);
        }
        if has_known_output_extension(&path) {
            Self::ExactFile(path)
        } else {
            Self::Root(path)
        }
    }
}

/// JMA-family + json extensions that signal "user named an exact
/// file". Anything else (no extension, or some unrelated extension)
/// gets treated as a directory, matching `extract-bitmap`'s rule.
fn has_known_output_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else { return false };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jmm" | "jma" | "jmt" | "jmz" | "jmo" | "jmr" | "jmw" | "json",
    )
}

fn resolve_destination(
    target: &OutputTarget,
    stem: &str,
    group: &AnimationGroup<'_>,
    format: Format,
    flat: bool,
) -> PathBuf {
    let ext = match format {
        Format::Json => "json",
        Format::Jma => jma_kind_for(group).extension(),
    };
    let nested_filename = default_filename(group, ext);
    // `--flat` prefixes the stem onto the filename (e.g. multiple
    // tags' anims dropped into the same dir don't collide), matching
    // `extract-jms --flat`'s `<stem>.<kind>.jms` shape.
    let flat_filename = format!("{stem}.{nested_filename}");
    match (target, flat) {
        (OutputTarget::ExactFile(p), _) => p.clone(),
        (OutputTarget::Root(dir), true) => dir.join(flat_filename),
        (OutputTarget::Root(dir), false) => dir.join(stem).join("animations").join(nested_filename),
        (OutputTarget::Default, true) => PathBuf::from(flat_filename),
        (OutputTarget::Default, false) => PathBuf::from(stem).join("animations").join(nested_filename),
    }
}

/// Bail with a clear listing if any two animations resolved to the
/// same output path. Sanitization (non-alphanumerics → `_`) can fold
/// distinct names together; rather than silently clobber, surface it.
fn check_unique_destinations(
    paths: &[PathBuf],
    groups: &[&AnimationGroup<'_>],
) -> Result<()> {
    let mut seen: HashMap<&Path, usize> = HashMap::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        if let Some(&j) = seen.get(p.as_path()) {
            anyhow::bail!(
                "two animations resolve to the same output file `{}`: \
                 [{}] '{}' and [{}] '{}'. Rename one in the tag, or extract them \
                 individually with explicit --output paths.",
                p.display(),
                groups[j].index,
                display_name(groups[j]),
                groups[i].index,
                display_name(groups[i]),
            );
        }
        seen.insert(p.as_path(), i);
    }
    Ok(())
}

fn write_json_stdout(group: &AnimationGroup<'_>, clip: &blam_tags::AnimationClip) -> Result<()> {
    let json_text = serde_json::to_string_pretty(&json_payload(group, clip))?;
    println!("{json_text}");
    Ok(())
}

fn write_json_file(
    group: &AnimationGroup<'_>,
    clip: &blam_tags::AnimationClip,
    path: &Path,
) -> Result<()> {
    let json_text = serde_json::to_string_pretty(&json_payload(group, clip))?;
    ensure_parent_dir(path)?;
    let mut writer = BufWriter::new(
        File::create(path).with_context(|| format!("create {}", path.display()))?,
    );
    writer.write_all(json_text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    println!(
        "{}: {} frames, animated={:?}",
        path.display(),
        clip.frame_count,
        clip.animated_status,
    );
    Ok(())
}

fn json_payload(group: &AnimationGroup<'_>, clip: &blam_tags::AnimationClip) -> serde_json::Value {
    let tracks_json = |t: &blam_tags::AnimationTracks| {
        json!({
            "codec": format!("{:?}", t.codec),
            "frame_count": t.frame_count,
            "rotations": t.rotations.iter().map(|frames| {
                frames.iter().map(|q| json!([q.i, q.j, q.k, q.w])).collect::<Vec<_>>()
            }).collect::<Vec<_>>(),
            "translations": t.translations.iter().map(|frames| {
                frames.iter().map(|p| json!([p.x, p.y, p.z])).collect::<Vec<_>>()
            }).collect::<Vec<_>>(),
            "scales": t.scales,
        })
    };
    json!({
        "name": group.name,
        "index": group.index,
        "frame_count": clip.frame_count,
        "static": tracks_json(&clip.static_tracks),
        "animated": clip.animated_tracks.as_ref().map(tracks_json),
        "animated_status": format!("{:?}", clip.animated_status),
    })
}

/// `<anim_name>.<ext>` for a group, falling back to `anim_<index>`
/// when the animation has no resolvable string-id name.
fn default_filename(group: &AnimationGroup<'_>, ext: &str) -> String {
    let safe_name = group
        .name
        .as_deref()
        .map(sanitize)
        .unwrap_or_else(|| format!("anim_{}", group.index));
    format!("{safe_name}.{ext}")
}

fn display_name(group: &AnimationGroup<'_>) -> String {
    group
        .name
        .clone()
        .unwrap_or_else(|| format!("[{}]", group.index))
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else { return Ok(()) };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))
}

fn pick_animation<'a, 'b>(
    animation: &'b Animation<'a>,
    anim: &str,
) -> Result<&'b AnimationGroup<'a>> {
    if let Ok(index) = anim.parse::<usize>() {
        return animation.get(index).ok_or_else(|| {
            anyhow::anyhow!(
                "animation index {index} out of range (have {} animations)",
                animation.len(),
            )
        });
    }
    animation.find(anim).ok_or_else(|| {
        anyhow::anyhow!("no animation named '{anim}' (use `list-animations` to see names)")
    })
}
