//! JMA-family text export.
//!
//! [`Pose::write_jma`] serializes a composed pose into one of the
//! JMA-family text formats — JMM (base), JMA (dx/dy), JMT
//! (dx/dy/dyaw), JMZ (dx/dy/dz/dyaw), JMO (overlay), JMR
//! (replacement), or JMW (world-relative). [`JmaKind::from_metadata`]
//! picks the right kind from the animation's `animation type` ×
//! `frame info type` × `internal flags / world relative` schema fields.
//!
//! Halo→JMA conventions applied here:
//! - **Translation `× 100`** (Halo world-units → JMA centimeter).
//! - **Version-dependent layout** ([`Pose::write_jma`]'s `version`): Halo
//!   CE's 16392 writes `first child / next sibling` node links,
//!   parent-relative transforms and **conjugated** quaternions
//!   `(-i, -j, -k, w)`; 16394 ([`JMA_ABSOLUTE_VERSION`], Halo 2 onwards)
//!   writes `parent` links, object-space transforms and quaternions as
//!   they are.
//! - **No separate movement section**. Neither version has more than
//!   `header + nodes + per-frame per-bone transforms`
//!   — no trailing per-frame movement table. Movement deltas are
//!   instead **folded into the root bone (index 0)** at write time:
//!   `dx/dy` rotate from local to world space by the accumulated
//!   yaw (per Foundry commit `850d680d` — fixes TagTool's
//!   actor-slides-backwards bug on yawed-during-walk anims), then
//!   the running translation+yaw is added/multiplied onto the root
//!   bone's pose. Verified against `General-101/Halo-Asset-Blender-
//!   Development-Toolset` (HABT) `process_file_retail.py` and
//!   TagTool's `Animation.Process()`.
//! - **Type-specific frame layout**:
//!     - Base (JMM/JMA/JMT/JMZ) and JMW: codec frames + a duplicated
//!       trailing frame (Tool expects a held terminal pose for
//!       blending into the next anim).
//!     - Replacement (JMR): a leading rest-pose frame, then codec
//!       frames. Tool subtracts the leading frame at re-build time
//!       to derive deltas.
//!     - Overlay (JMO): a leading *reference* frame, then the composed
//!       full poses. Overlay codec values are deltas-from-rest, so the
//!       caller composes them onto the rest pose via
//!       [`AnimationClip::overlay_pose`](super::AnimationClip::overlay_pose)
//!       (Foundry's `compose_overlay_animation` rules) **before** the
//!       writer — the writer just emits the result and prepends the
//!       reference as `defaults`. The writer no longer composes.
//!
//!   In all cases the final on-disk frame count is `codec_count + 1`.

use crate::geometry::write_floats;
use crate::math::{RealPoint3d, RealQuaternion, RealVector3d};

use super::{MovementData, MovementFrame, NodeTransform, Pose, Skeleton};

/// JMA-family file extension — picked from the animation's
/// `animation type` × `frame info type` × world-relative flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JmaKind {
    /// Base animation, no movement data.
    Jmm,
    /// Base + dx/dy.
    Jma,
    /// Base + dx/dy/dyaw.
    Jmt,
    /// Base + dx/dy/dz/dyaw.
    Jmz,
    /// Overlay animation.
    Jmo,
    /// Replacement animation.
    Jmr,
    /// World-relative (no movement, but selected via
    /// `internal_flags / world relative` rather than `animation type`).
    Jmw,
}

impl JmaKind {
    /// Uppercase JMA-family file extension (no leading dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Jmm => "JMM", Self::Jma => "JMA", Self::Jmt => "JMT", Self::Jmz => "JMZ",
            Self::Jmo => "JMO", Self::Jmr => "JMR", Self::Jmw => "JMW",
        }
    }

    /// Pick the right JMA-family kind from the per-animation metadata.
    ///
    /// `world_relative` is the `internal flags / world relative` bit
    /// from the jmad's `animations[i]` block — JMW is base + this
    /// bit, NOT a separate `animation_type` enum value (the schema
    /// only has `base / overlay / replacement`). Mirrors TagTool's
    /// `GetAnimationExtension(type, frame_info, worldRelative)` and
    /// Foundry's `internal_flags.TestBit("world relative")`.
    pub fn from_metadata(
        animation_type: Option<&str>,
        frame_info_type: Option<&str>,
        world_relative: bool,
    ) -> Self {
        match animation_type.unwrap_or("base") {
            "overlay" => return Self::Jmo,
            "replacement" => return Self::Jmr,
            _ => {}
        }
        if world_relative {
            return Self::Jmw;
        }
        match frame_info_type.unwrap_or("none") {
            "dx,dy" => Self::Jma,
            "dx,dy,dyaw" => Self::Jmt,
            // dz-bearing movement (incl. angle-axis and absolute) → JMZ
            // so the writer folds it into the root bone.
            "dx,dy,dz,dyaw" | "dx,dy,dz,dangle-axis" | "x,y,z - absolute" => Self::Jmz,
            _ => Self::Jmm,
        }
    }

    /// Whether this kind accumulates per-frame movement deltas into
    /// the root bone at write time. Only the base kinds with movement
    /// data (`Jma / Jmt / Jmz`) do; the rest emit per-bone transforms
    /// without any movement folding.
    pub fn folds_movement(self) -> bool {
        matches!(self, Self::Jma | Self::Jmt | Self::Jmz)
    }

    /// `JMR / JMO` prepend a leading reference frame so Tool's
    /// importer can derive deltas/composition cleanly during re-build.
    /// For `JMR` the leading frame is the rest pose; for `JMO` it is the
    /// overlay's per-bone *reference* (static value where static-flagged,
    /// else rest) — both supplied to the writer as `defaults`. Overlay
    /// composition itself is done before the writer, by
    /// [`AnimationClip::overlay_pose`](super::AnimationClip::overlay_pose).
    pub fn prepends_rest_pose(self) -> bool {
        matches!(self, Self::Jmo | Self::Jmr)
    }

    /// Base kinds (`JMM / JMA / JMT / JMZ`) and `JMW` append a
    /// duplicated trailing frame as the held terminal pose.
    pub fn appends_held_frame(self) -> bool {
        matches!(self, Self::Jmm | Self::Jma | Self::Jmt | Self::Jmz | Self::Jmw)
    }
}

impl Pose {
    /// Write this pose as a JMA-family text file (`.JMM/.JMA/.JMT/...`).
    /// See the [module docs](self) for the full layout convention.
    ///
    /// `defaults` supplies the leading frame prepended for JMR/JMO. For
    /// JMR it is the per-skeleton-bone rest pose (built from the
    /// render_model's `nodes[]` defaults plus the jmad's `additional node
    /// data` fallback); for JMO it is the *reference* frame returned by
    /// [`AnimationClip::overlay_pose`](super::AnimationClip::overlay_pose)
    /// (which already composed the body `Pose` against the rest pose). The
    /// writer performs no overlay composition of its own.
    ///
    /// `movement` carries per-frame root deltas in **local space**.
    /// For movement-bearing kinds (JMA/JMT/JMZ) the writer rotates
    /// `dx/dy` by the accumulated yaw before adding it to the root
    /// bone's pose — Foundry-style local→world fix per commit
    /// `850d680d`. JMM/JMW/JMO/JMR ignore `movement` entirely.
    ///
    /// `version` picks the on-disk layout; see [`crate::game::Game::jma_version`].
    /// Below [`JMA_ABSOLUTE_VERSION`] (Halo CE's 16392): checksum after the
    /// node count, `first child / next sibling` node links, parent-relative
    /// transforms and conjugated quaternions. From it on (Halo 2 onwards):
    /// checksum second, `parent` node links, object-space transforms and
    /// quaternions as they are — read off Halo 2 MCC tool.exe's
    /// `intermediate_animation.cpp`, which turns each node back into its
    /// parent's space with `inverse(parent) * node` after reading.
    #[allow(clippy::too_many_arguments)] // each arg is load-bearing; bundling adds a builder type for one call site
    pub fn write_jma<W: std::io::Write>(
        &self,
        writer: &mut W,
        skeleton: &Skeleton,
        defaults: &[NodeTransform],
        node_list_checksum: i32,
        kind: JmaKind,
        actor_name: &str,
        movement: Option<&MovementData>,
        version: u16,
    ) -> std::io::Result<()> {
        let codec_count = self.frames.len();
        // Tool re-importers expect codec_count + 1 frames: a leading
        // rest pose for JMR/JMO, or a held trailing frame for the
        // base kinds and JMW.
        let total_frames = if codec_count == 0 { 0 } else { codec_count + 1 };
        let absolute = version >= JMA_ABSOLUTE_VERSION;

        // Header.
        writeln!(writer, "{version}")?;
        if absolute {
            writeln!(writer, "{node_list_checksum}")?;
        }
        writeln!(writer, "{total_frames}")?;
        writeln!(writer, "30")?;
        writeln!(writer, "1")?;
        writeln!(writer, "{actor_name}")?;
        writeln!(writer, "{}", skeleton.len())?;
        if !absolute {
            writeln!(writer, "{node_list_checksum}")?;
        }

        // Skeleton.
        for node in &skeleton.nodes {
            writeln!(writer, "{}", node.name)?;
            if absolute {
                writeln!(writer, "{}", node.parent)?;
            } else {
                writeln!(writer, "{}", node.first_child)?;
                writeln!(writer, "{}", node.next_sibling)?;
            }
        }

        if codec_count == 0 {
            return writer.flush();
        }

        // Optional leading rest-pose frame for JMR/JMO. Movement
        // accumulation hasn't started yet, so the rest pose is
        // emitted unmodified.
        if kind.prepends_rest_pose() {
            write_frame(writer, skeleton, defaults, version)?;
        }

        // Movement folding state — accumulated through every codec
        // frame. The accumulator is advanced *after* each frame is
        // written, so frame 0 holds the root still and movement begins
        // accumulating from frame 1. This mirrors Foundry's prepended
        // zero movement frame (`_movement_data_from_second_frame`); the
        // trailing held frame then carries the final (full) accumulation.
        let mut accumulated_translation = RealPoint3d::default();
        let mut accumulated_rotation = RealQuaternion::IDENTITY;
        let movement_absolute = movement.map(|m| m.kind.is_absolute()).unwrap_or(false);

        let compose = |frame: &[NodeTransform], translation: RealPoint3d, rotation: RealQuaternion| {
            frame
                .iter()
                .enumerate()
                .map(|(bone_idx, transform)| {
                    compose_frame_bone(*transform, bone_idx, translation, rotation, kind)
                })
                .collect::<Vec<_>>()
        };

        for (frame_idx, frame) in self.frames.iter().enumerate() {
            let composed = compose(frame, accumulated_translation, accumulated_rotation);
            write_frame(writer, skeleton, &composed, version)?;

            // Advance AFTER writing so the next frame reflects this
            // frame's delta (Foundry's frame-0-is-rest convention).
            if kind.folds_movement() {
                if let Some(local) = movement.and_then(|m| m.frames.get(frame_idx)) {
                    advance_movement(
                        &mut accumulated_translation,
                        &mut accumulated_rotation,
                        local,
                        movement_absolute,
                    );
                }
            }
        }

        // Trailing held frame — duplicate of the last codec frame's
        // pose, carrying the final accumulated movement (which now
        // includes the last codec frame's delta).
        if kind.appends_held_frame() {
            let last_frame = &self.frames[codec_count - 1];
            let composed = compose(last_frame, accumulated_translation, accumulated_rotation);
            write_frame(writer, skeleton, &composed, version)?;
        }

        writer.flush()?;
        Ok(())
    }
}

/// The first JMA version whose nodes name their parent and whose frames hold
/// object-space transforms with unconjugated quaternions. Halo 2 MCC
/// tool.exe refuses anything older ("ANIMATION FILE IS OUTDATED! … expected at
/// least version 16394"), and Halo CE MCC tool.exe accepts nothing newer than
/// 16393.
pub const JMA_ABSOLUTE_VERSION: u16 = 16394;

/// Write one frame of parent-relative transforms in `version`'s layout.
fn write_frame<W: std::io::Write>(
    writer: &mut W,
    skeleton: &Skeleton,
    frame: &[NodeTransform],
    version: u16,
) -> std::io::Result<()> {
    if version < JMA_ABSOLUTE_VERSION {
        for transform in frame {
            write_transform(writer, *transform, true)?;
        }
        return Ok(());
    }
    for transform in object_space_frame(skeleton, frame) {
        write_transform(writer, transform, false)?;
    }
    Ok(())
}

/// Compose a frame of parent-relative transforms into object space, each node
/// through its chain of parents. A node with no parent, or a parent outside
/// the frame, is already in object space.
pub(crate) fn object_space_frame(skeleton: &Skeleton, frame: &[NodeTransform]) -> Vec<NodeTransform> {
    let mut resolved: Vec<Option<NodeTransform>> = vec![None; frame.len()];
    for index in 0..frame.len() {
        // Walk up to the nearest resolved ancestor (or the root), then come
        // back down composing. Bounded by the node count, so a malformed
        // parent cycle stops rather than looping.
        let mut chain = vec![index];
        while chain.len() <= frame.len() {
            let node = *chain.last().unwrap();
            let parent = skeleton.nodes.get(node).map_or(-1, |n| n.parent);
            if parent < 0 || parent as usize >= frame.len() || resolved[parent as usize].is_some() {
                break;
            }
            chain.push(parent as usize);
        }
        for &node in chain.iter().rev() {
            if resolved[node].is_some() {
                continue;
            }
            let parent = skeleton.nodes.get(node).map_or(-1, |n| n.parent);
            let local = frame[node];
            resolved[node] = Some(match parent {
                p if p >= 0 && (p as usize) < frame.len() && p as usize != node => match resolved[p as usize] {
                    Some(parent) => compose_transforms(parent, local),
                    None => local,
                },
                _ => local,
            });
        }
    }
    resolved.into_iter().map(|t| t.unwrap_or(NodeTransform::IDENTITY)).collect()
}

/// `parent * child`, as Halo 2 tool.exe composes a real orientation
/// (`sub_682D50`): rotations multiply, the child's translation is rotated and
/// scaled by the parent's before the parent's is added, and scales multiply.
pub(crate) fn compose_transforms(parent: NodeTransform, child: NodeTransform) -> NodeTransform {
    let rotated = parent.rotation.rotate(RealVector3d {
        i: child.translation.x,
        j: child.translation.y,
        k: child.translation.z,
    });
    NodeTransform {
        rotation: parent.rotation * child.rotation,
        translation: RealPoint3d {
            x: parent.translation.x + parent.scale * rotated.i,
            y: parent.translation.y + parent.scale * rotated.j,
            z: parent.translation.z + parent.scale * rotated.k,
        },
        scale: parent.scale * child.scale,
    }
}

/// Apply this frame's local movement delta to the running accumulators.
/// Translation is rotated into world space by the rotation accumulated
/// so far, then added; rotation is composed afterwards (Foundry's
/// `apply_movement_data` order — rotate first, accumulate after). For
/// absolute movement ([`MovementKind::XyzAbsolute`]) the translation is
/// a per-frame absolute position and no rotation is accumulated.
fn advance_movement(
    translation: &mut RealPoint3d,
    accumulated_rotation: &mut RealQuaternion,
    local: &MovementFrame,
    absolute: bool,
) {
    if absolute {
        translation.x = local.dx;
        translation.y = local.dy;
        translation.z = local.dz;
        return;
    }
    let world = *accumulated_rotation * RealVector3d { i: local.dx, j: local.dy, k: local.dz };
    translation.x += world.i;
    translation.y += world.j;
    translation.z += world.k;
    *accumulated_rotation = (*accumulated_rotation * local.rotation).normalized();
}

/// Fold accumulated movement into the root bone for the given JMA kind.
/// Overlay/replacement composition is done upstream (see
/// [`AnimationClip::pose`](super::AnimationClip::pose) /
/// [`overlay_pose`](super::AnimationClip::overlay_pose)); this only
/// applies the movement deltas, which live on the root bone (index 0) of
/// the movement-bearing base kinds (`JMA / JMT / JMZ`). Returns the
/// transform to write (post-conjugate / scale-by-100 are still applied
/// by [`write_transform`]).
fn compose_frame_bone(
    transform: NodeTransform,
    bone_idx: usize,
    accumulated_translation: RealPoint3d,
    accumulated_rotation: RealQuaternion,
    kind: JmaKind,
) -> NodeTransform {
    let mut t = transform.translation;
    let mut q = transform.rotation;
    let s = transform.scale;

    if kind.folds_movement() && bone_idx == 0 {
        t = RealPoint3d {
            x: t.x + accumulated_translation.x,
            y: t.y + accumulated_translation.y,
            z: t.z + accumulated_translation.z,
        };
        q = accumulated_rotation * q;
    }

    NodeTransform { translation: t, rotation: q, scale: s }
}

/// Write one (translation, rotation, scale) bone-frame triple in
/// JMA-on-disk format: translation `× 100` (cm convention), the quaternion
/// **conjugated** (`(-i, -j, -k, w)`) when `conjugate` (versions before
/// [`JMA_ABSOLUTE_VERSION`], whose readers negate `w` back) or as it is, and
/// scale unchanged.
fn write_transform<W: std::io::Write>(writer: &mut W, t: NodeTransform, conjugate: bool) -> std::io::Result<()> {
    let p = t.translation;
    write_floats(writer, &[p.x * 100.0, p.y * 100.0, p.z * 100.0])?;
    let q = t.rotation;
    if conjugate {
        write_floats(writer, &[-q.i, -q.j, -q.k, q.w])?;
    } else {
        write_floats(writer, &[q.i, q.j, q.k, q.w])?;
    }
    write_floats(writer, &[t.scale])?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::SkeletonNode;

    fn node(name: &str, first_child: i16, next_sibling: i16, parent: i16) -> SkeletonNode {
        SkeletonNode { name: name.to_owned(), first_child, next_sibling, parent }
    }

    /// `root` turned 90° about Z and lifted 1 unit, with `hand` 1 unit along
    /// root's X and `tip` 1 unit along hand's X, half scale.
    fn arm() -> (Skeleton, Vec<NodeTransform>) {
        let skeleton = Skeleton {
            nodes: vec![node("root", 1, -1, -1), node("hand", 2, -1, 0), node("tip", -1, -1, 1)],
        };
        let half = std::f32::consts::FRAC_1_SQRT_2;
        let frame = vec![
            NodeTransform {
                rotation: RealQuaternion { i: 0.0, j: 0.0, k: half, w: half },
                translation: RealPoint3d { x: 0.0, y: 0.0, z: 1.0 },
                scale: 1.0,
            },
            NodeTransform {
                rotation: RealQuaternion::IDENTITY,
                translation: RealPoint3d { x: 1.0, y: 0.0, z: 0.0 },
                scale: 0.5,
            },
            NodeTransform {
                rotation: RealQuaternion::IDENTITY,
                translation: RealPoint3d { x: 1.0, y: 0.0, z: 0.0 },
                scale: 1.0,
            },
        ];
        (skeleton, frame)
    }

    fn write(version: u16) -> String {
        let (skeleton, frame) = arm();
        let pose = Pose { frames: vec![frame.clone()] };
        let mut out = Vec::new();
        pose.write_jma(&mut out, &skeleton, &frame, 1234, JmaKind::Jmm, "actor", None, version)
            .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn floats(line: &str) -> Vec<f32> {
        line.split('\t').map(|v| v.parse().unwrap()).collect()
    }

    /// Halo CE's layout is what every game got before 16394 existed here.
    #[test]
    fn version_16392_keeps_the_parent_relative_layout() {
        let text = write(16392);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[..16],
            [
                "16392", "2", "30", "1", "actor", "3", "1234",
                "root", "1", "-1", "hand", "2", "-1", "tip", "-1", "-1",
            ]
        );
        // Frame 0, root: local, quaternion conjugated.
        assert_eq!(floats(lines[16]), [0.0, 0.0, 100.0]);
        let q = floats(lines[17]);
        assert!(q[2] < 0.0 && q[3] > 0.0, "root quaternion not conjugated: {q:?}");
        // hand: still in root's space.
        assert_eq!(floats(lines[19]), [100.0, 0.0, 0.0]);
    }

    #[test]
    fn version_16394_writes_parents_and_object_space_transforms() {
        let text = write(16394);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[..13],
            ["16394", "1234", "2", "30", "1", "actor", "3", "root", "-1", "hand", "0", "tip", "1"]
        );
        let close = |got: Vec<f32>, want: [f32; 3]| {
            assert!(
                got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-3),
                "{got:?} vs {want:?}"
            );
        };
        // root as it is; its quaternion unconjugated.
        close(floats(lines[13]), [0.0, 0.0, 100.0]);
        let q = floats(lines[14]);
        assert!(q[2] > 0.0 && q[3] > 0.0, "root quaternion conjugated: {q:?}");
        // hand: root's X is world Y.
        close(floats(lines[16]), [0.0, 100.0, 100.0]);
        // tip: half scale from hand, so half a unit further along world Y.
        close(floats(lines[19]), [0.0, 150.0, 100.0]);
        assert_eq!(floats(lines[21]), [0.5]);
    }

    /// Read a 16394 frame back the way Halo 2 MCC tool.exe does
    /// (`intermediate_animation.cpp`): translation × 0.01, quaternion as
    /// written, then from the last node to the first,
    /// `node = inverse(parent) * node`. It must give back the pose that was
    /// written.
    #[test]
    fn version_16394_reads_back_through_the_tool_s_inverse() {
        let (skeleton, frame) = arm();
        let text = write(16394);
        let lines: Vec<&str> = text.lines().collect();
        let mut read: Vec<NodeTransform> = (0..3)
            .map(|n| {
                let t = floats(lines[13 + n * 3]);
                let q = floats(lines[14 + n * 3]);
                let s = floats(lines[15 + n * 3]);
                NodeTransform {
                    translation: RealPoint3d { x: t[0] * 0.01, y: t[1] * 0.01, z: t[2] * 0.01 },
                    rotation: RealQuaternion { i: q[0], j: q[1], k: q[2], w: q[3] }.normalized(),
                    scale: s[0],
                }
            })
            .collect();
        // The tool's `real_orientation` inverse (`sub_682960`).
        let inverse = |t: NodeTransform| {
            let rotation = RealQuaternion { i: t.rotation.i, j: t.rotation.j, k: t.rotation.k, w: -t.rotation.w };
            let scale = 1.0 / t.scale;
            let moved = rotation.rotate(RealVector3d { i: t.translation.x, j: t.translation.y, k: t.translation.z });
            NodeTransform {
                rotation,
                translation: RealPoint3d { x: -moved.i * scale, y: -moved.j * scale, z: -moved.k * scale },
                scale,
            }
        };
        for index in (0..3).rev() {
            let parent = skeleton.nodes[index].parent;
            if parent >= 0 {
                read[index] = compose_transforms(inverse(read[parent as usize]), read[index]);
            }
        }
        for (index, (got, want)) in read.iter().zip(&frame).enumerate() {
            let same_rotation = (got.rotation.i * want.rotation.i
                + got.rotation.j * want.rotation.j
                + got.rotation.k * want.rotation.k
                + got.rotation.w * want.rotation.w)
                .abs();
            assert!(same_rotation > 0.9999, "node {index} rotation {got:?} vs {want:?}");
            for (g, w) in [
                (got.translation.x, want.translation.x),
                (got.translation.y, want.translation.y),
                (got.translation.z, want.translation.z),
                (got.scale, want.scale),
            ] {
                assert!((g - w).abs() < 1e-4, "node {index}: {got:?} vs {want:?}");
            }
        }
    }

    /// Every movement option any game's schema offers resolves to its
    /// movement kind and extension. The names are matched literally, so a
    /// spelling the schemas never use (Reach's `dangle-axis` was matched as
    /// `dangle_axis`) silently drops the animation's movement.
    #[test]
    fn every_schema_frame_info_option_has_a_movement_kind() {
        use crate::animation::MovementKind;
        let mut checked = 0;
        for entry in std::fs::read_dir("../definitions").unwrap().flatten() {
            for group in ["model_animation_graph.json", "model_animations.json"] {
                let Ok(text) = std::fs::read_to_string(entry.path().join(group)) else { continue };
                let schema: serde_json::Value = serde_json::from_str(&text).unwrap();
                let Some(options) = schema["enums_flags"]["frame_info_type_enum"]["options"].as_array() else { continue };
                for option in options.iter().filter_map(|option| option.as_str()) {
                    let movement = MovementKind::from_schema_name(option);
                    let kind = JmaKind::from_metadata(Some("base"), Some(option), false);
                    let want = match movement {
                        MovementKind::None => JmaKind::Jmm,
                        MovementKind::DxDy => JmaKind::Jma,
                        MovementKind::DxDyDyaw => JmaKind::Jmt,
                        _ => JmaKind::Jmz,
                    };
                    // Halo 4 and H2A also offer `auto`; what it stores isn't
                    // established, so it stays unmapped rather than guessed.
                    assert!(
                        matches!(option, "none" | "auto") || movement != MovementKind::None,
                        "{}: {option:?} maps to no movement", entry.path().display(),
                    );
                    assert_eq!(kind, want, "{}: {option:?}", entry.path().display());
                    checked += 1;
                }
            }
        }
        assert!(checked >= 32, "only {checked} options found under ../definitions");
    }
}
