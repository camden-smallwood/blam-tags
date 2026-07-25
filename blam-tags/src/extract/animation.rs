//! Animation extraction: decode every animation in a jmad (or the tag
//! that points at one) and write JMA-family source files.
//!
//! Per-bone rest pose source priority (see [`build_defaults`]):
//!   1. `render_model.nodes[i]/default {translation, rotation}` —
//!      authoritative; used when a render_model is resolved.
//!   2. `jmad.additional node data[i]` — denormalized cache inside the
//!      jmad; the only source reachable from a bare jmad.
//!   3. Identity — bones absent from both.
//!
//! The kind (`JMM/JMA/JMT/JMZ/JMO/JMR/JMW`) is picked from each
//! animation's `animation type` × `frame info type` × `world relative`
//! bit; overlays/replacements compose against the graph-resolved base
//! pose, everything else against the rest pose. This mirrors the logic
//! that used to live in `blam-tag-shell`'s `extract-animation` command.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::animation::classic::{CeAnimation, CeAnimations};
use crate::animation::SizeLayout;
use crate::math::{RealPoint3d, RealQuaternion, RealVector3d};
use crate::paths::tag_ref_path;
use crate::{
    Animation, AnimationClip, AnimationGraph, AnimationGroup, JmaKind, NodeTransform, Pose,
    Skeleton, TagFieldData, TagFile,
};

use super::{ExtractError, TagResolver};

/// Outcome of a directory animation export.
#[derive(Debug, Default)]
pub struct AnimSummary {
    /// Number of JMA-family files written.
    pub written: usize,
    /// Number of animations skipped (composite/runtime-blend with no
    /// codec payload, or destination-path collisions).
    pub skipped: usize,
    /// Non-fatal notes (skips, collisions).
    pub warnings: Vec<String>,
}

/// Owned tags resolved from an extraction input. `jmad` is `None` for a
/// direct jmad input (the caller reuses the input borrow); `.model` /
/// object inputs populate `jmad` (from the `animation` ref) and
/// optionally `render_model` (from the `render model` ref).
#[derive(Debug, Default)]
pub struct ResolvedAnimation {
    /// The model_animation_graph tag, when loaded from a reference. `None`
    /// means "the input tag is itself the jmad".
    pub jmad: Option<TagFile>,
    /// The render_model tag, when the owning `.model` referenced one. For
    /// Campaign Evolved this instead holds the `skeleton_model` (its
    /// render_model-minus-geometry), used for the rest pose.
    pub render_model: Option<TagFile>,
    /// Whether the source is Campaign Evolved — detected by the `.model`
    /// carrying a `skeleton model` ref in place of a `render model`. CE
    /// re-rigged on MetaHuman skeletons whose bone basis is 90° off Halo's, so
    /// its JMA export needs [`halo_bone_reorientation`]. Every other game leaves
    /// this `false`.
    pub campaign_evolved: bool,
}

/// Extract every animation in `input` to `<out_dir>/<actor_name>/animations/`.
///
/// `input` may be a `.model_animation_graph` (jmad), a `.model` (hlmt),
/// any object-inheriting tag that points at a `.model`, or a Halo CE
/// `.model_animations` (antr). `actor_name` is used both as the folder
/// stem and as the JMA header actor name.
pub fn animations_to_dir(
    input: &TagFile,
    resolver: &dyn TagResolver,
    out_dir: &Path,
    actor_name: &str,
) -> Result<AnimSummary, ExtractError> {
    // Halo CE `model_animations` (antr) predates the gen3 codec-pack model
    // entirely — route it through the classic decoder.
    if &input.header.group_tag.to_be_bytes() == b"antr" {
        return ce_animations_to_dir(input, out_dir, actor_name);
    }

    let resolved = resolve_animation_inputs(input, resolver)?;
    let jmad_tag: &TagFile = resolved.jmad.as_ref().unwrap_or(input);
    let render_model = resolved.render_model.as_ref();

    let animation = Animation::new(jmad_tag)?;
    if animation.is_empty() {
        return Err(ExtractError::msg(format!(
            "tag has no local animations (parent: {:?}) — nothing to extract",
            animation.parent(),
        )));
    }

    let skeleton = Skeleton::from_tag(jmad_tag);
    if skeleton.is_empty() {
        return Err(ExtractError::msg(
            "jmad has no skeleton nodes — JMA export needs a skeleton",
        ));
    }

    let object_space = additional_node_data_is_object_space(&animation);
    let defaults = build_defaults(&skeleton, jmad_tag, render_model, object_space);
    // Reorient MetaHuman-convention rigs to Halo X-down-bone so bones don't
    // import splayed — ONLY for Campaign Evolved; every other game is untouched.
    let reorient = resolved
        .campaign_evolved
        .then(|| halo_bone_reorientation(&skeleton, &defaults))
        .flatten();
    let graph = AnimationGraph::from_tag(jmad_tag);

    let dir = out_dir.join(actor_name).join("animations");
    let mut summary = AnimSummary::default();
    let mut seen: HashMap<PathBuf, ()> = HashMap::new();

    for group in animation.iter() {
        // Composite / runtime-blend animations (common in Halo 4:
        // `locomote`, `*_composite`) carry no codec payload — nothing to
        // export. Skip with a note rather than aborting the whole tag.
        if group.blob.is_empty() {
            summary.skipped += 1;
            summary.warnings.push(format!(
                "skipped '{}' — no codec payload (composite/runtime-blend animation)",
                display_name(group),
            ));
            continue;
        }

        let kind = jma_kind_for(group);
        let filename = anim_filename(group.name.as_deref(), group.index, kind.extension());
        let dest = dir.join(&filename);
        if seen.insert(dest.clone(), ()).is_some() {
            summary.warnings.push(format!(
                "skipped '{}' — output path collides with an earlier animation: {}",
                display_name(group),
                dest.display(),
            ));
            summary.skipped += 1;
            continue;
        }

        let clip = group.decode()?;
        write_group_jma(
            group, &clip, &animation, &graph, &skeleton, &defaults, reorient.as_deref(),
            actor_name, &dest,
        )?;
        summary.written += 1;
    }

    Ok(summary)
}

/// Halo CE `model_animations` (antr) directory export. CE stores each
/// animation's frames inline (no gen3 codec pack), with the skeleton in
/// the tag's own `nodes` block; poses are self-contained and need no
/// render_model.
fn ce_animations_to_dir(
    tag: &TagFile,
    out_dir: &Path,
    actor_name: &str,
) -> Result<AnimSummary, ExtractError> {
    let animations = CeAnimations::new(tag);
    if animations.is_empty() {
        return Err(ExtractError::msg(
            "model_animations has no animations to extract",
        ));
    }
    let skeleton = Skeleton::from_tag(tag);
    if skeleton.is_empty() {
        return Err(ExtractError::msg(
            "model_animations has no nodes — JMA export needs a skeleton",
        ));
    }
    // Halo CE `additional node data` is parent-local (no conversion).
    // (This is Halo 1 `antr` — a classic Bungie X-down rig, never Campaign
    // Evolved — so no bone-convention reorientation applies.)
    let defaults = build_defaults(&skeleton, tag, None, false);

    let dir = out_dir.join(actor_name).join("animations");
    let mut summary = AnimSummary::default();
    let mut seen: HashMap<PathBuf, ()> = HashMap::new();

    for group in animations.iter() {
        let kind = JmaKind::from_metadata(
            group.animation_type.as_deref(),
            group.frame_info_type.as_deref(),
            group.world_relative,
        );
        let name = group
            .name
            .clone()
            .unwrap_or_else(|| format!("anim_{}", group.index));
        let filename = format!("{}.{}", sanitize(&name), kind.extension());
        let dest = dir.join(&filename);
        if seen.insert(dest.clone(), ()).is_some() {
            summary.warnings.push(format!(
                "skipped '{name}' — output path collides with an earlier animation: {}",
                dest.display(),
            ));
            summary.skipped += 1;
            continue;
        }

        let clip = group.decode();
        write_ce_group_jma(group, &clip, &skeleton, &defaults, None, actor_name, &dest)?;
        summary.written += 1;
    }

    Ok(summary)
}

/// Dispatch on input group_tag to find the jmad + (optional) render_model:
///   - `jmad` → use the input tag as-is; no render_model.
///   - `hlmt` → follow `animation` + `render model` refs.
///   - any other tag with a `model` field (bipd/vehi/scen/weap/eqip/…) →
///     follow `model` to a hlmt, then recurse the hlmt case.
pub fn resolve_animation_inputs(
    input: &TagFile,
    resolver: &dyn TagResolver,
) -> Result<ResolvedAnimation, ExtractError> {
    let group = input.header.group_tag.to_be_bytes();
    match &group {
        b"jmad" => Ok(ResolvedAnimation {
            jmad: None,
            render_model: None,
            campaign_evolved: false,
        }),
        b"hlmt" => resolve_from_model(input, resolver),
        _ => {
            let model_rel = find_object_model_ref(input).ok_or_else(|| {
                ExtractError::msg(format!(
                    "input group `{}` has no `model` ref — pass a .model_animation_graph, a \
                     .model, or any object-inheriting tag (.biped, .scenery, .weapon, …)",
                    std::str::from_utf8(&group).unwrap_or("?"),
                ))
            })?;
            let model_tag =
                resolver.resolve(&model_rel, "model", u32::from_be_bytes(*b"hlmt"))?;
            resolve_from_model(&model_tag, resolver)
        }
    }
}

/// Pull `animation` + rest-pose refs off a hlmt tag. The rest pose comes from
/// the `render model` ref; when it's null/missing we drop back to
/// additional_node_data only — except Campaign Evolved, which keeps rendering
/// geometry in Unreal and instead carries a `skeleton model` (a render_model
/// minus geometry) whose `nodes` block is the same shape and drops into the
/// same overlay.
fn resolve_from_model(
    model_tag: &TagFile,
    resolver: &dyn TagResolver,
) -> Result<ResolvedAnimation, ExtractError> {
    let root = model_tag.root();
    let jmad_rel = tag_ref_path(&root, "animation")
        .ok_or_else(|| ExtractError::msg("`.model` has no `animation` ref — nothing to extract"))?;
    let jmad = resolver.resolve(
        &jmad_rel,
        "model_animation_graph",
        u32::from_be_bytes(*b"jmad"),
    )?;

    let render_ref = tag_ref_path(&root, "render model");
    let skel_ref = tag_ref_path(&root, "skeleton model");
    // Campaign Evolved is exactly the case with a skeleton_model and no
    // render_model — the tag-intrinsic signal that the rig is MetaHuman-convention.
    let campaign_evolved = render_ref.is_none() && skel_ref.is_some();

    let render_model = match render_ref {
        Some(render_rel) => {
            Some(resolver.resolve(&render_rel, "render_model", u32::from_be_bytes(*b"mode"))?)
        }
        // CE has no render_model; use the skeleton_model rest pose if present.
        // Tolerant (`.ok()`): a missing skel is just no overlay, not a failure.
        None => skel_ref
            .and_then(|r| resolver.resolve(&r, "skeleton_model", u32::from_be_bytes(*b"skel")).ok()),
    };

    Ok(ResolvedAnimation {
        jmad: Some(jmad),
        render_model,
        campaign_evolved,
    })
}

/// Find the inherited `model` tag_reference on an object-inheriting tag.
/// Every object-inheriting group uses one of these inheritance paths; we
/// probe in order and use the first match.
fn find_object_model_ref(tag: &TagFile) -> Option<String> {
    const PATHS: &[&str] = &[
        "unit/object/model",
        "item/object/model",
        "device/object/model",
        "object/model",
    ];
    let root = tag.root();
    PATHS.iter().find_map(|p| match root.field_path(p)?.value()? {
        TagFieldData::TagReference(r) => r
            .group_tag_and_name
            .map(|(_, name)| name)
            .filter(|s| !s.is_empty()),
        _ => None,
    })
}

/// Build a per-skeleton-bone rest-pose defaults table. Render_model
/// entries (when supplied) take priority; gaps fall through to the jmad's
/// `additional node data`; bones absent from both fall back to identity.
pub fn build_defaults(
    skeleton: &Skeleton,
    jmad: &TagFile,
    render_model: Option<&TagFile>,
    object_space_anim_data: bool,
) -> Vec<NodeTransform> {
    // Lower priority: jmad's `additional node data`, indexed per skeleton
    // node. Reach/H4 store these in object/model space; H2/H3 store them
    // parent-local. Build the per-node table first so we can convert the
    // whole object-space set to local in one parent-aware pass.
    let mut anim_by_name: HashMap<String, NodeTransform> = HashMap::new();
    if let Some(block) = jmad
        .root()
        .field_path("additional node data")
        .and_then(|f| f.as_block())
    {
        for i in 0..block.len() {
            let Some(elem) = block.element(i) else {
                continue;
            };
            let Some(name) = elem.read_string_id("node name") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            anim_by_name.insert(
                name,
                NodeTransform {
                    translation: elem.read_point3d("default translation"),
                    rotation: elem.read_quat("default rotation"),
                    scale: elem.read_real("default scale").unwrap_or(1.0),
                },
            );
        }
    }
    let mut anim: Vec<NodeTransform> = skeleton
        .nodes
        .iter()
        .map(|n| {
            anim_by_name
                .get(&n.name)
                .copied()
                .unwrap_or(NodeTransform::IDENTITY)
        })
        .collect();
    // Reach/H4 `additional node data` is object-space → convert to local
    // (Foundry's world_to_local). H2/H3 are already local — leave as-is.
    if object_space_anim_data {
        anim = skeleton.object_to_local(&anim);
    }

    // Higher priority: render_model `nodes[]` (always parent-local). Build
    // a name lookup and overlay it on top of the (now-local) anim defaults.
    let mut rm_by_name: HashMap<String, NodeTransform> = HashMap::new();
    if let Some(rm) = render_model
        && let Some(block) = rm.root().field_path("nodes").and_then(|f| f.as_block())
    {
        for i in 0..block.len() {
            let Some(elem) = block.element(i) else {
                continue;
            };
            let Some(name) = elem.read_string_id("name") else {
                continue;
            };
            if name.is_empty() {
                continue;
            }
            rm_by_name.insert(
                name,
                NodeTransform {
                    translation: elem.read_point3d("default translation"),
                    rotation: elem.read_quat("default rotation"),
                    // Render_model's `default scale` is buried inside the
                    // inverse matrix; animation rest poses have scale=1.0.
                    scale: 1.0,
                },
            );
        }
    }

    skeleton
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| rm_by_name.get(&node.name).copied().unwrap_or(anim[i]))
        .collect()
}

/// Forward-kinematic object-space rotation + position of each node from a set
/// of parent-local transforms.
fn forward_kinematics(
    skeleton: &Skeleton,
    local: &[NodeTransform],
) -> (Vec<RealQuaternion>, Vec<RealVector3d>) {
    let n = skeleton.nodes.len();
    let mut oq = vec![RealQuaternion::IDENTITY; n];
    let mut op = vec![RealVector3d::ZERO; n];
    for i in 0..n {
        let t = local[i].translation;
        let lt = RealVector3d { i: t.x, j: t.y, k: t.z };
        let lr = local[i].rotation.normalized();
        match skeleton.nodes[i].parent {
            p if p < 0 => {
                oq[i] = lr;
                op[i] = lt;
            }
            p => {
                let pi = p as usize;
                oq[i] = (oq[pi] * lr).normalized();
                op[i] = op[pi] + oq[pi].rotate(lt);
            }
        }
    }
    (oq, op)
}

/// Reconstruct a clean bind-pose orientation for every bone directly from the
/// skeleton HIERARCHY, returned as a per-bone correction so it composes with
/// [`apply_reorientation`]. **Only call this for Campaign Evolved content** (gate
/// on [`ResolvedAnimation::campaign_evolved`]); other games ship correctly-
/// oriented Halo rigs and must never be touched.
///
/// Campaign Evolved's `skeleton_model`s are UE-derived: characters sit on
/// MetaHuman skeletons (bones down local Y), and vehicle/prop rigs carry UE's
/// per-bone/mirrored bone bases (left bones 180°-flipped). Neither draws down
/// local X the way Halo tooling (Blender toolset, `tool.exe`) expects, so bones
/// import splayed. There is no mirror flag in the tag or the UE skeleton to key
/// off, and the flips are entangled with real joint rotations — so rather than
/// *detect* anything, we rebuild each bone's orientation from the joint
/// positions: point its local **+X down-the-bone** (toward its first child; a
/// leaf continues its parent's direction) with a stable roll (local +Z toward
/// world up). This is ordinary armature construction — convention-independent,
/// clean for any rig — and it drops into the existing pipeline: the geometry is
/// baked against the ORIGINAL node transforms and only the emitted skeleton is
/// re-oriented, so the correction `Cᵢ = worldᵢ⁻¹·world'ᵢ` preserves every joint
/// POSITION (and therefore the skinned result) exactly.
pub fn halo_bone_reorientation(
    skeleton: &Skeleton,
    rest: &[NodeTransform],
) -> Option<Vec<RealQuaternion>> {
    let n = skeleton.nodes.len();
    if n == 0 || rest.len() != n {
        return None;
    }
    let (oq, op) = forward_kinematics(skeleton, rest);

    // Object-space "down-bone" direction: toward the first child, or — for a
    // leaf — continuing the direction of the bone leading into it.
    let down_dir = |i: usize| -> Option<RealVector3d> {
        let fc = skeleton.nodes[i].first_child;
        let d = if fc >= 0 {
            op[fc as usize] - op[i]
        } else {
            let p = skeleton.nodes[i].parent;
            if p < 0 {
                return None;
            }
            op[i] - op[p as usize]
        };
        let v = d.normalized();
        (v != RealVector3d::ZERO).then_some(v)
    };

    let world_up = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };
    let alt_up = RealVector3d { i: 0.0, j: 1.0, k: 0.0 };
    let mut corr = Vec::with_capacity(n);
    for i in 0..n {
        // Target world orientation: local +X along the bone, +Z toward world up
        // (falling back to +Y when the bone is near-vertical so the roll stays
        // well-defined). Bones with no direction (root, coincident joints) keep
        // their authored orientation.
        let new_world = match down_dir(i) {
            Some(x) => {
                let up = if x.dot(world_up).abs() < 0.99 { world_up } else { alt_up };
                let y = up.cross(x).normalized();
                if y == RealVector3d::ZERO {
                    oq[i]
                } else {
                    let z = x.cross(y).normalized();
                    RealQuaternion::from_basis_columns(x, y, z)
                }
            }
            None => oq[i],
        };
        corr.push(oq[i].conjugate() * new_world);
    }
    Some(corr)
}

/// Apply a reorientation (from [`halo_bone_reorientation`]) to one frame of
/// parent-local transforms in place: `L'_i = C_parent⁻¹ · L_i · C_i`, with the
/// local translation rotated by `C_parent⁻¹`. World transforms are unchanged.
pub fn apply_reorientation(
    frame: &mut [NodeTransform],
    skeleton: &Skeleton,
    corr: &[RealQuaternion],
) {
    for i in 0..frame.len() {
        let cp_inv = match skeleton.nodes[i].parent {
            p if p < 0 => RealQuaternion::IDENTITY,
            p => corr[p as usize].conjugate(),
        };
        let new_rot = (cp_inv * frame[i].rotation * corr[i]).normalized();
        let t = frame[i].translation;
        let nt = cp_inv.rotate(RealVector3d { i: t.x, j: t.y, k: t.z });
        frame[i].rotation = new_rot;
        frame[i].translation = RealPoint3d { x: nt.i, y: nt.j, z: nt.k };
    }
}

/// Whether a jmad's `additional node data` rest pose is in object/model
/// space (Reach/H4) rather than parent-local (H2/H3).
pub fn additional_node_data_is_object_space(animation: &Animation<'_>) -> bool {
    animation
        .iter()
        .any(|g| g.data_sizes.as_ref().map(|d| d.layout()) == Some(SizeLayout::Reach))
}

/// Pick the JMA-family kind for a gen3 animation group from its metadata.
pub fn jma_kind_for(group: &AnimationGroup<'_>) -> JmaKind {
    JmaKind::from_metadata(
        group.animation_type.as_deref(),
        group.frame_info_type.as_deref(),
        group.world_relative,
    )
}

/// Compose and write one gen3 animation group as a JMA-family file at
/// `dest`. `defaults` is the per-bone rest pose from [`build_defaults`].
#[allow(clippy::too_many_arguments)]
pub fn write_group_jma(
    group: &AnimationGroup<'_>,
    clip: &AnimationClip,
    animation: &Animation<'_>,
    graph: &AnimationGraph,
    skeleton: &Skeleton,
    defaults: &[NodeTransform],
    reorient: Option<&[RealQuaternion]>,
    actor_name: &str,
    dest: &Path,
) -> Result<(), ExtractError> {
    let kind = jma_kind_for(group);
    // Overlay/replacement codec values are deltas authored against a
    // *base* pose (the matching stance), not the bind pose. Resolve that
    // base's first frame; fall back to the rest pose when none is found.
    let base = match kind {
        JmaKind::Jmo | JmaKind::Jmr => animation
            .overlay_base_pose(graph, group, skeleton, defaults)
            .unwrap_or_else(|| defaults.to_vec()),
        _ => defaults.to_vec(),
    };
    let (mut pose, mut leading): (Pose, Vec<NodeTransform>) = match kind {
        JmaKind::Jmo => {
            let (mut reference, mut body) = clip.overlay_pose(skeleton, &base);
            body.apply_object_space_corrections(
                &mut reference,
                skeleton,
                &base,
                &group.object_space_parents,
            );
            (body, reference)
        }
        JmaKind::Jmr => {
            let mut body = clip.replacement_pose(skeleton, &base);
            let mut reference = base.clone();
            body.apply_object_space_corrections(
                &mut reference,
                skeleton,
                &base,
                &group.object_space_parents,
            );
            (body, reference)
        }
        _ => (clip.pose(skeleton, Some(defaults)), defaults.to_vec()),
    };

    // Reorient to Halo bone convention (CE MetaHuman rigs), post-composition so
    // every written frame + the leading reference share the correction.
    if let Some(corr) = reorient {
        for frame in &mut pose.frames {
            apply_reorientation(frame, skeleton, corr);
        }
        apply_reorientation(&mut leading, skeleton, corr);
    }

    ensure_parent_dir(dest)?;
    let mut writer = BufWriter::new(File::create(dest)?);
    pose.write_jma(
        &mut writer,
        skeleton,
        &leading,
        group.node_list_checksum,
        kind,
        actor_name,
        Some(&clip.movement),
    )?;
    writer.flush()?;
    Ok(())
}

/// Compose and write one Halo CE animation group as a JMA-family file.
#[allow(clippy::too_many_arguments)]
pub fn write_ce_group_jma(
    group: &CeAnimation<'_>,
    clip: &AnimationClip,
    skeleton: &Skeleton,
    defaults: &[NodeTransform],
    reorient: Option<&[RealQuaternion]>,
    actor_name: &str,
    dest: &Path,
) -> Result<(), ExtractError> {
    let kind = JmaKind::from_metadata(
        group.animation_type.as_deref(),
        group.frame_info_type.as_deref(),
        group.world_relative,
    );
    // CE has no per-graph base resolution; overlays/replacements compose
    // onto the skeleton rest pose (aim/look overlays are the common case).
    let (mut pose, mut leading) = match kind {
        JmaKind::Jmo => {
            let (reference, body) = clip.overlay_pose(skeleton, defaults);
            (body, reference)
        }
        JmaKind::Jmr => (clip.replacement_pose(skeleton, defaults), defaults.to_vec()),
        _ => (clip.pose(skeleton, Some(defaults)), defaults.to_vec()),
    };

    if let Some(corr) = reorient {
        for frame in &mut pose.frames {
            apply_reorientation(frame, skeleton, corr);
        }
        apply_reorientation(&mut leading, skeleton, corr);
    }

    ensure_parent_dir(dest)?;
    let mut writer = BufWriter::new(File::create(dest)?);
    pose.write_jma(
        &mut writer,
        skeleton,
        &leading,
        group.node_list_checksum,
        kind,
        actor_name,
        Some(&clip.movement),
    )?;
    writer.flush().ok();
    Ok(())
}

/// `<sanitized_name_or_anim_index>.<ext>` for an animation group.
fn anim_filename(name: Option<&str>, index: usize, ext: &str) -> String {
    let base = name
        .map(sanitize)
        .unwrap_or_else(|| format!("anim_{index}"));
    format!("{base}.{ext}")
}

fn display_name(group: &AnimationGroup<'_>) -> String {
    group
        .name
        .clone()
        .unwrap_or_else(|| format!("[{}]", group.index))
}

/// Map a tag-internal animation name to an importable on-disk file stem.
///
/// Halo uses `:` as the animation-name token separator; Tool/HABT take the
/// file basename verbatim as the animation name and Foundry's `data_name`
/// is `name.replace(":", " ")`, so the importable stem replaces every `:`
/// with a space and keeps underscores. Only genuinely path-/filename-illegal
/// characters are scrubbed to `_`.
pub fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            ':' => ' ',
            '/' | '\\' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect()
}

fn ensure_parent_dir(path: &Path) -> Result<(), ExtractError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    fs::create_dir_all(parent)?;
    Ok(())
}
