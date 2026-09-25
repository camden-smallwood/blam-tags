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
//! - **Quaternion conjugate** serialization: JMA writes
//!   `(-i, -j, -k, w)`.
//! - **No separate movement section**. The H3 JMA spec (version
//!   16392) is just `header + nodes + per-frame per-bone transforms`
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
            "dx,dy,dz,dyaw" | "dx,dy,dz,dangle_axis" | "xyz,absolute" | "xyz_absolute"
            | "x,y,z,absolute" => Self::Jmz,
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
    ) -> std::io::Result<()> {
        let codec_count = self.frames.len();
        // Tool re-importers expect codec_count + 1 frames: a leading
        // rest pose for JMR/JMO, or a held trailing frame for the
        // base kinds and JMW.
        let total_frames = if codec_count == 0 { 0 } else { codec_count + 1 };

        // Header.
        writeln!(writer, "16392")?;
        writeln!(writer, "{total_frames}")?;
        writeln!(writer, "30")?;
        writeln!(writer, "1")?;
        writeln!(writer, "{actor_name}")?;
        writeln!(writer, "{}", skeleton.len())?;
        writeln!(writer, "{node_list_checksum}")?;

        // Skeleton.
        for node in &skeleton.nodes {
            writeln!(writer, "{}", node.name)?;
            writeln!(writer, "{}", node.first_child)?;
            writeln!(writer, "{}", node.next_sibling)?;
        }

        if codec_count == 0 {
            return writer.flush();
        }

        // Optional leading rest-pose frame for JMR/JMO. Movement
        // accumulation hasn't started yet, so the rest pose is
        // emitted unmodified.
        if kind.prepends_rest_pose() {
            for transform in defaults {
                write_transform(writer, *transform)?;
            }
        }

        // Movement folding state — accumulated through every codec
        // frame. The accumulator is advanced *after* each frame is
        // written, so frame 0 holds the root still and movement begins
        // accumulating from frame 1. This mirrors Foundry's prepended
        // zero movement frame (`_movement_data_from_second_frame`); the
        // trailing held frame then carries the final (full) accumulation.
        let mut accumulated_translation = RealPoint3d::default();
        let mut accumulated_rotation = RealQuaternion::IDENTITY;
        let absolute = movement.map(|m| m.kind.is_absolute()).unwrap_or(false);

        for (frame_idx, frame) in self.frames.iter().enumerate() {
            for (bone_idx, transform) in frame.iter().enumerate() {
                let composed = compose_frame_bone(
                    *transform,
                    bone_idx,
                    accumulated_translation,
                    accumulated_rotation,
                    kind,
                );
                write_transform(writer, composed)?;
            }

            // Advance AFTER writing so the next frame reflects this
            // frame's delta (Foundry's frame-0-is-rest convention).
            if kind.folds_movement() {
                if let Some(local) = movement.and_then(|m| m.frames.get(frame_idx)) {
                    advance_movement(
                        &mut accumulated_translation,
                        &mut accumulated_rotation,
                        local,
                        absolute,
                    );
                }
            }
        }

        // Trailing held frame — duplicate of the last codec frame's
        // pose, carrying the final accumulated movement (which now
        // includes the last codec frame's delta).
        if kind.appends_held_frame() {
            let last_idx = codec_count - 1;
            let last_frame = &self.frames[last_idx];
            for (bone_idx, transform) in last_frame.iter().enumerate() {
                let composed = compose_frame_bone(
                    *transform,
                    bone_idx,
                    accumulated_translation,
                    accumulated_rotation,
                    kind,
                );
                write_transform(writer, composed)?;
            }
        }

        writer.flush()?;
        Ok(())
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
/// JMA-on-disk format: translation `× 100` (cm convention),
/// quaternion **conjugate** (`(-i, -j, -k, w)`), scale unchanged.
fn write_transform<W: std::io::Write>(writer: &mut W, t: NodeTransform) -> std::io::Result<()> {
    let p = t.translation;
    write_floats(writer, &[p.x * 100.0, p.y * 100.0, p.z * 100.0])?;
    let q = t.rotation;
    write_floats(writer, &[-q.i, -q.j, -q.k, q.w])?;
    write_floats(writer, &[t.scale])?;
    Ok(())
}

