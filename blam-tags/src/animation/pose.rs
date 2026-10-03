//! Skeleton + Pose composition.
//!
//! [`Skeleton::from_tag`] walks the jmad's `definitions/skeleton
//! nodes` (or `resources/skeleton nodes` for older inline-layout
//! tags) into a flat list of [`SkeletonNode`]s. [`super::AnimationClip`]
//! exposes a [`pose`](super::AnimationClip::pose) method that takes a
//! `Skeleton` and resolves every (frame, bone) pair through the
//! per-component flag bitarrays — bones flagged static read from
//! `static_tracks`, bones flagged animated read the right frame from
//! `animated_tracks`, unflagged bones fall back to identity.
//!
//! The output [`Pose`] is the unit the JMA writer
//! ([`super::jma`]) consumes.

use crate::file::TagFile;
use crate::math::{Matrix4, RealPoint3d, RealQuaternion};

use super::{AnimationClip, BitArray, NodeFlags, ObjectSpaceParentNode, TOP_LEVEL_NAMES};

//================================================================================
// Skeleton + Pose
//================================================================================

/// One node in the jmad's `definitions/skeleton nodes` block.
#[derive(Debug, Clone)]
pub struct SkeletonNode {
    pub name: String,
    /// Index into `Skeleton::nodes` of the first child, or `-1`.
    pub first_child: i16,
    /// Index of the next sibling under the same parent, or `-1`.
    pub next_sibling: i16,
    /// Index of the parent node, or `-1` for root.
    pub parent: i16,
}

/// jmad skeleton — the bone hierarchy that animations target.
#[derive(Debug, Clone)]
pub struct Skeleton {
    pub nodes: Vec<SkeletonNode>,
}

impl Skeleton {
    /// Walk `definitions/skeleton nodes` (or `resources/skeleton nodes`
    /// for older inline-layout tags) into a flat list of nodes.
    /// Returns an empty skeleton if the block is missing.
    pub fn from_tag(tag: &TagFile) -> Self {
        let root = tag.root();
        for prefix in TOP_LEVEL_NAMES {
            if let Some(block) = root
                .field_path(&format!("{prefix}/skeleton nodes"))
                .and_then(|f| f.as_block())
            {
                let mut nodes = Vec::with_capacity(block.len());
                for i in 0..block.len() {
                    let Some(elem) = block.element(i) else { continue };
                    let name = elem.read_string_id("name").unwrap_or_default();
                    let first_child = elem.read_block_index("first child node index");
                    let next_sibling = elem.read_block_index("next sibling node index");
                    let parent = elem.read_block_index("parent node index");
                    nodes.push(SkeletonNode { name, first_child, next_sibling, parent });
                }
                return Self { nodes };
            }
        }
        // Halo CE `model_animations` (antr): the skeleton is a root-level
        // `nodes` block, its name an inline string rather than a string-id.
        if let Some(block) = root.field_path("nodes").and_then(|f| f.as_block()) {
            let mut nodes = Vec::with_capacity(block.len());
            for i in 0..block.len() {
                let Some(elem) = block.element(i) else { continue };
                let name = elem.read_string("name")
                    .or_else(|| elem.read_string_id("name"))
                    .unwrap_or_default();
                nodes.push(SkeletonNode {
                    name,
                    first_child: elem.read_block_index("first child node index"),
                    next_sibling: elem.read_block_index("next sibling node index"),
                    parent: elem.read_block_index("parent node index"),
                });
            }
            return Self { nodes };
        }
        Self { nodes: Vec::new() }
    }

    /// Number of skeleton nodes (bones).
    pub fn len(&self) -> usize { self.nodes.len() }

    /// `true` when the tag has no skeleton nodes (e.g. inheriting jmads).
    pub fn is_empty(&self) -> bool { self.nodes.is_empty() }

    /// Convert per-node OBJECT/model-space transforms into parent-LOCAL
    /// transforms (`local = parent_object⁻¹ · node_object`). Reach and
    /// Halo 4 store the rest pose (`additional node data`) in object space;
    /// the JMA family — and [`AnimationClip::pose`] — want parent-local, so
    /// the caller converts before using them as defaults (Foundry's
    /// `world_to_local`). `object[i]` must align with `self.nodes[i]`;
    /// nodes assume parent-before-child order (true for skeleton blocks).
    pub fn object_to_local(&self, object: &[NodeTransform]) -> Vec<NodeTransform> {
        let world: Vec<Matrix4> = object.iter()
            .map(|t| Matrix4::from_loc_rot_scale(t.translation, t.rotation, t.scale))
            .collect();
        self.nodes.iter().enumerate().map(|(i, n)| {
            let local = if n.parent >= 0 && (n.parent as usize) < world.len() {
                world[n.parent as usize].inverse() * world[i]
            } else {
                world[i]
            };
            let (translation, rotation, scale) = local.decompose();
            NodeTransform { translation, rotation, scale }
        }).collect()
    }
}

/// One bone's transform at one frame — the unit JMA writes per
/// `(frame, node)` cell.
#[derive(Debug, Clone, Copy, Default)]
pub struct NodeTransform {
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub scale: f32,
}

impl NodeTransform {
    /// Identity transform: rotation `(0,0,0,1)`, translation
    /// `(0,0,0)`, scale `1.0`. Useful as a fallback when no rest
    /// pose is available — note `Default` is all-zeros (rotation
    /// included) which is *not* identity.
    pub const IDENTITY: Self = Self {
        rotation: RealQuaternion::IDENTITY,
        translation: RealPoint3d { x: 0.0, y: 0.0, z: 0.0 },
        scale: 1.0,
    };
}

/// Per-frame, per-bone transform table. `frames[frame_index][bone_index]`.
#[derive(Debug, Clone)]
pub struct Pose {
    pub frames: Vec<Vec<NodeTransform>>,
}

impl Pose {
    /// Apply object-space parent-node corrections to this pose's frames
    /// and the leading `reference` frame — Foundry's
    /// `_apply_object_space_base_corrections`, used for Reach/H4 3D pose
    /// overlays. No-op when `targets` is empty (e.g. all H3 overlays).
    ///
    /// Each target pins a node's object-space orientation to a fixed
    /// `(translation, rotation, scale)`; the node **and all its
    /// descendants** are rigidly rotated so the node lands on that
    /// orientation, without distorting the subtree. The delta is computed
    /// once per target from a running copy of `base` (deltas compound
    /// across targets, processed shallow-to-deep) and applied uniformly to
    /// the leading reference and every body frame.
    ///
    /// `base` is the composition base (idle/rest first frame);
    /// `skeleton` supplies the parent hierarchy for the forward-kinematic
    /// matrices.
    pub fn apply_object_space_corrections(
        &mut self,
        reference: &mut [NodeTransform],
        skeleton: &Skeleton,
        base: &[NodeTransform],
        targets: &[ObjectSpaceParentNode],
    ) {
        if targets.is_empty() {
            return;
        }
        let n = skeleton.len();

        // (target_index, desired object-space matrix), shallow-to-deep.
        let mut corrections: Vec<(usize, Matrix4)> = Vec::new();
        for t in targets {
            if t.node_index < 0 || (t.node_index as usize) >= n {
                continue;
            }
            let target_index = object_space_target_index(t.node_index as usize, skeleton);
            let matrix = Matrix4::from_loc_rot_scale(t.translation, t.rotation, t.scale);
            corrections.push((target_index, matrix));
        }
        corrections.sort_by_key(|(ti, _)| node_depth(*ti, skeleton));

        // Running base copy used to derive each target's delta (Foundry's
        // `reference_frame`) — separate object from the exported leading
        // frame, but corrected in lock-step so compounding matches.
        let mut correction_ref: Vec<NodeTransform> = base.to_vec();
        if correction_ref.len() < n {
            correction_ref.resize(n, NodeTransform::IDENTITY);
        }

        for (target_index, target_matrix) in corrections {
            let os = frame_object_space_matrices(&correction_ref, skeleton);
            let delta = target_matrix * os[target_index].inverse();
            let descendants = descendant_indices(target_index, skeleton);

            apply_delta_to_frame(&mut correction_ref, skeleton, &descendants, delta);
            apply_delta_to_frame(reference, skeleton, &descendants, delta);
            for frame in &mut self.frames {
                apply_delta_to_frame(frame, skeleton, &descendants, delta);
            }
        }
    }
}

/// The node an object-space parent entry actually re-orients: the
/// targeted node's parent, or the node itself when it is a root. Mirrors
/// Foundry's `_object_space_parent_target_index`.
fn object_space_target_index(node_index: usize, skeleton: &Skeleton) -> usize {
    let parent = skeleton.nodes[node_index].parent;
    if parent < 0 || (parent as usize) >= skeleton.len() {
        node_index
    } else {
        parent as usize
    }
}

/// Depth of `idx` in the hierarchy (root = 1), used to order corrections
/// and descendants shallow-to-deep.
fn node_depth(mut idx: usize, skeleton: &Skeleton) -> usize {
    let n = skeleton.len();
    let mut depth = 0;
    let mut guard = 0;
    while idx < n {
        depth += 1;
        let parent = skeleton.nodes[idx].parent;
        if parent < 0 || (parent as usize) >= n || parent as usize == idx {
            break;
        }
        idx = parent as usize;
        guard += 1;
        if guard > n {
            break; // cycle guard
        }
    }
    depth
}

/// All nodes whose parent-chain reaches `target` (including `target`
/// itself), shallow-to-deep. Mirrors Foundry's
/// `_object_space_descendant_indices`.
fn descendant_indices(target: usize, skeleton: &Skeleton) -> Vec<usize> {
    let n = skeleton.len();
    let mut out = Vec::new();
    for node in 0..n {
        let mut cur = node;
        let mut guard = 0;
        loop {
            if cur == target {
                out.push(node);
                break;
            }
            let parent = skeleton.nodes[cur].parent;
            if parent < 0 || (parent as usize) >= n || parent as usize == cur {
                break;
            }
            cur = parent as usize;
            guard += 1;
            if guard > n {
                break;
            }
        }
    }
    out.sort_by_key(|node| node_depth(*node, skeleton));
    out
}

/// Forward-kinematic object-space matrix per bone for one frame:
/// `os[node] = os[parent] * local(node)`.
fn frame_object_space_matrices(frame: &[NodeTransform], skeleton: &Skeleton) -> Vec<Matrix4> {
    let n = skeleton.len();
    let mut os: Vec<Option<Matrix4>> = vec![None; n];
    for i in 0..n {
        resolve_os(i, frame, skeleton, &mut os);
    }
    os.into_iter().map(|o| o.unwrap_or(Matrix4::IDENTITY)).collect()
}

fn resolve_os(
    i: usize,
    frame: &[NodeTransform],
    skeleton: &Skeleton,
    os: &mut Vec<Option<Matrix4>>,
) -> Matrix4 {
    if let Some(m) = os[i] {
        return m;
    }
    let t = frame.get(i).copied().unwrap_or(NodeTransform::IDENTITY);
    let local = Matrix4::from_loc_rot_scale(t.translation, t.rotation, t.scale);
    let parent = skeleton.nodes[i].parent;
    let m = if parent >= 0 && (parent as usize) < skeleton.len() && parent as usize != i {
        resolve_os(parent as usize, frame, skeleton, os) * local
    } else {
        local
    };
    os[i] = Some(m);
    m
}

/// Apply `delta` (an object-space transform) to `node_indices` and write
/// back the resulting local transforms. The whole listed subtree moves
/// rigidly (delta cancels in the local recompute for descendants whose
/// parent is also listed), so only the subtree root's local actually
/// changes. Mirrors Foundry's `_apply_object_space_delta_to_frame`.
fn apply_delta_to_frame(
    frame: &mut [NodeTransform],
    skeleton: &Skeleton,
    node_indices: &[usize],
    delta: Matrix4,
) {
    let mut os = frame_object_space_matrices(frame, skeleton);
    for &node in node_indices {
        if node < os.len() {
            os[node] = delta * os[node];
        }
    }
    for &node in node_indices {
        if node >= frame.len() {
            continue;
        }
        let parent = skeleton.nodes[node].parent;
        let local = if parent >= 0 && (parent as usize) < os.len() {
            os[parent as usize].inverse() * os[node]
        } else {
            os[node]
        };
        let (translation, rotation, scale) = local.decompose();
        frame[node] = NodeTransform { translation, rotation, scale };
    }
}

impl AnimationClip {
    /// Compose `static_tracks` + `animated_tracks` against the skeleton
    /// using the per-component `node_flags` bitarrays. Result has one
    /// `NodeTransform` per (frame, skeleton bone).
    ///
    /// `defaults` supplies the per-bone rest pose used when a bone is
    /// flagged neither static nor animated. Pass `None` for identity
    /// (legacy behavior — wrong for FP weapons and other tags whose
    /// unflagged bones rely on the render_model's `default
    /// translation` / `default rotation`); pass `Some(&[..])` with one
    /// entry per skeleton bone for the canonical render_model defaults
    /// (matches TagTool's `AnimationDefaultNodeHelper` behavior).
    pub fn pose(&self, skeleton: &Skeleton, defaults: Option<&[NodeTransform]>) -> Pose {
        let bones = skeleton.len();
        let frames_n = self.frame_count.max(1) as usize;
        let mut frames = Vec::with_capacity(frames_n);

        // Pre-resolve every bone's source: which codec stream owns each
        // component, and what the codec_node_index is.
        let resolutions: Vec<BoneResolution> = (0..bones)
            .map(|b| BoneResolution::for_bone(b, self.node_flags.as_ref()))
            .collect();

        for f in 0..frames_n {
            let mut row = Vec::with_capacity(bones);
            for (b, res) in resolutions.iter().enumerate() {
                let default = defaults
                    .and_then(|d| d.get(b))
                    .copied()
                    .unwrap_or(NodeTransform::IDENTITY);
                let rotation = pick_rotation(self, res, f).unwrap_or(default.rotation);
                let translation = pick_translation(self, res, f).unwrap_or(default.translation);
                let scale = pick_scale(self, res, f).unwrap_or(default.scale);
                row.push(NodeTransform { rotation, translation, scale });
            }
            frames.push(row);
        }
        Pose { frames }
    }

    /// Compose an **overlay** (delta) animation onto a base/rest pose,
    /// matching Foundry's `compose_overlay_animation`
    /// (`managed_blam/animation_resource.py`). Returns
    /// `(reference, body)`:
    ///
    /// - `reference` — the per-bone composition base. For each
    ///   component, the **static-track** value when the bone is
    ///   static-flagged for that component, otherwise the supplied
    ///   `base` (rest pose). This is also the leading frame the JMA
    ///   writer prepends.
    /// - `body` — `frame_count` composed frames. An **animated**-flagged
    ///   component applies its per-frame delta on top of the reference
    ///   (translation additive, rotation `reference * delta`, scale
    ///   **multiplicative**); every other component holds the reference
    ///   value unchanged.
    ///
    /// This differs from a plain [`pose`](Self::pose): overlays store
    /// *deltas from rest* for animated nodes (frame 0 = identity), so we
    /// must reconstruct the full local pose. Crucially the scale identity
    /// is `1.0` (multiplicative), not `0.0` — adding it like a
    /// translation delta double-scales every bone. Static deltas are kept
    /// as the reference, not added onto `base`.
    ///
    /// The caller writes `body` verbatim and uses `reference` as the
    /// leading frame, yielding `frame_count + 1` on-disk frames — the
    /// same layout Foundry produces when importing the tag directly.
    pub fn overlay_pose(
        &self,
        skeleton: &Skeleton,
        base: &[NodeTransform],
    ) -> (Vec<NodeTransform>, Pose) {
        let bones = skeleton.len();
        let frames_n = self.frame_count.max(1) as usize;

        let resolutions: Vec<BoneResolution> = (0..bones)
            .map(|b| BoneResolution::for_bone(b, self.node_flags.as_ref()))
            .collect();

        // Per-bone reference: static value where static-flagged, else the
        // rest pose. Animated/identity components fall back to `base`.
        let reference: Vec<NodeTransform> = resolutions
            .iter()
            .enumerate()
            .map(|(b, res)| {
                let base_b = base.get(b).copied().unwrap_or(NodeTransform::IDENTITY);
                NodeTransform {
                    rotation: match res.rotation {
                        TrackSource::Static(_) => pick_rotation(self, res, 0).unwrap_or(base_b.rotation),
                        _ => base_b.rotation,
                    },
                    translation: match res.translation {
                        TrackSource::Static(_) => pick_translation(self, res, 0).unwrap_or(base_b.translation),
                        _ => base_b.translation,
                    },
                    scale: match res.scale {
                        TrackSource::Static(_) => pick_scale(self, res, 0).unwrap_or(base_b.scale),
                        _ => base_b.scale,
                    },
                }
            })
            .collect();

        let mut frames = Vec::with_capacity(frames_n);
        for f in 0..frames_n {
            let mut row = Vec::with_capacity(bones);
            for (b, res) in resolutions.iter().enumerate() {
                let r = reference[b];
                let rotation = match res.rotation {
                    TrackSource::Animated(_) => {
                        r.rotation * pick_rotation(self, res, f).unwrap_or(RealQuaternion::IDENTITY)
                    }
                    _ => r.rotation,
                };
                let translation = match res.translation {
                    TrackSource::Animated(_) => {
                        let d = pick_translation(self, res, f).unwrap_or_default();
                        RealPoint3d { x: r.translation.x + d.x, y: r.translation.y + d.y, z: r.translation.z + d.z }
                    }
                    _ => r.translation,
                };
                let scale = match res.scale {
                    TrackSource::Animated(_) => r.scale * pick_scale(self, res, f).unwrap_or(1.0),
                    _ => r.scale,
                };
                row.push(NodeTransform { rotation, translation, scale });
            }
            frames.push(row);
        }

        (reference, Pose { frames })
    }

    /// Compose a **replacement** animation against a base/rest pose,
    /// matching Foundry's `compose_replacement_animation` and TagTool's
    /// `Animation.Replace()`. Returns `frame_count` body frames (the JMA
    /// writer prepends the rest pose as the leading frame).
    ///
    /// Only **animated**-flagged components take the codec value (a full
    /// pose, not a delta); every other component — including
    /// *static*-flagged ones — takes the `base` (rest) value. Both
    /// reference implementations drop static-track data here (they
    /// condition on the animated flag alone), so this deliberately does
    /// NOT read [`pose`](Self::pose)'s static-first track value.
    pub fn replacement_pose(&self, skeleton: &Skeleton, base: &[NodeTransform]) -> Pose {
        let bones = skeleton.len();
        let frames_n = self.frame_count.max(1) as usize;
        let resolutions: Vec<BoneResolution> = (0..bones)
            .map(|b| BoneResolution::for_bone(b, self.node_flags.as_ref()))
            .collect();

        let mut frames = Vec::with_capacity(frames_n);
        for f in 0..frames_n {
            let mut row = Vec::with_capacity(bones);
            for (b, res) in resolutions.iter().enumerate() {
                let base_b = base.get(b).copied().unwrap_or(NodeTransform::IDENTITY);
                let rotation = match res.rotation {
                    TrackSource::Animated(_) => pick_rotation(self, res, f).unwrap_or(base_b.rotation),
                    _ => base_b.rotation,
                };
                let translation = match res.translation {
                    TrackSource::Animated(_) => pick_translation(self, res, f).unwrap_or(base_b.translation),
                    _ => base_b.translation,
                };
                let scale = match res.scale {
                    TrackSource::Animated(_) => pick_scale(self, res, f).unwrap_or(base_b.scale),
                    _ => base_b.scale,
                };
                row.push(NodeTransform { rotation, translation, scale });
            }
            frames.push(row);
        }
        Pose { frames }
    }
}

#[derive(Debug, Clone, Copy)]
struct BoneResolution {
    rotation: TrackSource,
    translation: TrackSource,
    scale: TrackSource,
}

#[derive(Debug, Clone, Copy)]
enum TrackSource {
    Static(usize),
    Animated(usize),
    Identity,
}

impl BoneResolution {
    fn for_bone(bone: usize, flags: Option<&NodeFlags>) -> Self {
        match flags {
            None => {
                // No flags available — fall back to "all bones use the
                // static track in skeleton order". Right for the most
                // common static-only case (mongoose-class inline
                // layouts), wrong for tagged bone subsets — but those
                // tags carry node_flags so we won't take this path.
                Self {
                    rotation: TrackSource::Static(bone),
                    translation: TrackSource::Static(bone),
                    scale: TrackSource::Static(bone),
                }
            }
            Some(f) => Self {
                rotation: pick_source(bone, &f.static_rotation, &f.animated_rotation),
                translation: pick_source(bone, &f.static_translation, &f.animated_translation),
                scale: pick_source(bone, &f.static_scale, &f.animated_scale),
            }
        }
    }
}

fn pick_source(bone: usize, static_flags: &BitArray, animated_flags: &BitArray) -> TrackSource {
    if static_flags.bit(bone) { TrackSource::Static(static_flags.popcount_below(bone)) }
    else if animated_flags.bit(bone) { TrackSource::Animated(animated_flags.popcount_below(bone)) }
    else { TrackSource::Identity }
}

fn pick_rotation(clip: &AnimationClip, res: &BoneResolution, frame: usize) -> Option<RealQuaternion> {
    match res.rotation {
        TrackSource::Static(i) => clip.static_tracks.rotations.get(i).and_then(|f| f.first()).copied(),
        TrackSource::Animated(i) => clip.animated_tracks.as_ref()
            .and_then(|t| t.rotations.get(i)).and_then(|f| f.get(frame.min(f.len() - 1))).copied(),
        TrackSource::Identity => None,
    }
}
fn pick_translation(clip: &AnimationClip, res: &BoneResolution, frame: usize) -> Option<RealPoint3d> {
    match res.translation {
        TrackSource::Static(i) => clip.static_tracks.translations.get(i).and_then(|f| f.first()).copied(),
        TrackSource::Animated(i) => clip.animated_tracks.as_ref()
            .and_then(|t| t.translations.get(i)).and_then(|f| f.get(frame.min(f.len() - 1))).copied(),
        TrackSource::Identity => None,
    }
}
fn pick_scale(clip: &AnimationClip, res: &BoneResolution, frame: usize) -> Option<f32> {
    match res.scale {
        TrackSource::Static(i) => clip.static_tracks.scales.get(i).and_then(|f| f.first()).copied(),
        TrackSource::Animated(i) => clip.animated_tracks.as_ref()
            .and_then(|t| t.scales.get(i)).and_then(|f| f.get(frame.min(f.len() - 1))).copied(),
        TrackSource::Identity => None,
    }
}
