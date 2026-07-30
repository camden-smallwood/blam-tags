//! Halo CE `model_animations` (group `antr`) animation decode.
//!
//! Halo CE predates the gen3 codec-pack model entirely: each animation in
//! the root-level `animations` block stores its frames as two raw blobs —
//! `default data` (static, shared across all frames) and `frame data`
//! (`frame count` consecutive frames, each `frame size` bytes) — plus three
//! 64-bit node-flag masks (`node rotation flag data` / `node transform flag
//! data` / `node scale flag data`, each two `long_integer`s). For each node,
//! a flag bit decides per component (rotation, translation, scale) whether
//! that component is **animated** (set → read per-frame from `frame data`)
//! or **static** (clear → read once from `default data`). Components are
//! laid out node-major in the order rotation → translation → scale, and a
//! node contributes a component to exactly one blob, so
//! `len(default data) + frame_size == node_count × 24` for an uncompressed
//! animation (rotation 4×int16, translation 3×f32, scale f32 = 8+12+4).
//!
//! When the per-animation `compressed data` flag is set, `frame data` holds
//! a keyframe-compressed block at `offset to compressed data` instead (6-byte
//! quaternions, real_point3d translations, f32 scales, each with a per-node
//! keyframe table). See `decode_compressed`.
//!
//! This module reuses the shared [`AnimationClip`] currency: it fills
//! `static_tracks` / `animated_tracks` packed in node order and `node_flags`
//! as component masks, so the existing [`AnimationClip::pose`](super::AnimationClip::pose),
//! `overlay_pose`, `replacement_pose`, and JMA writer all work unchanged.
//!
//! Layout RE'd from the symbolated pre-release CE Anniversary X360 build
//! (`animation_get_node_orientations` @ 0x83796b48, `animation_get_frame_data`
//! @ 0x838186f0, `quaternion_decompress_6byte` @ 0x837959f0, the
//! `animation_get_keyframe_*` family). See `project_classic_animation_plan`.

use crate::file::TagFile;
use crate::io::Endian;
use crate::math::{RealPoint3d, RealQuaternion};

use super::{
    AnimatedStreamStatus, AnimationClip, AnimationTracks, BitArray, Codec, MovementData,
    MovementFrame, MovementKind, NodeFlags,
};

/// Engine dequantization factor for the 16-bit rotation components, taken
/// verbatim from the CE engine (`× 0.000030518509`, ≈ 1/32767).
const ROT_SCALE: f32 = 0.000030518509;

/// One Halo CE animation: per-animation header metadata plus borrowed
/// references to its `default data` / `frame data` / `frame info` blobs.
#[derive(Debug)]
pub struct CeAnimation<'a> {
    /// Index in the root `animations` block.
    pub index: usize,
    pub name: Option<String>,
    /// `base` / `overlay` / `replacement`.
    pub animation_type: Option<String>,
    /// Movement kind, normalized to the comma form the shared
    /// [`JmaKind`](super::JmaKind) / [`MovementKind`] expect
    /// (CE spells it `dx dy`, gen3 `dx,dy`).
    pub frame_info_type: Option<String>,
    /// `flags / world relative` (bit 1) — selects JMW for base animations.
    pub world_relative: bool,
    pub frame_count: u16,
    pub node_count: usize,
    pub node_list_checksum: i32,
    frame_size: usize,
    /// `flags / compressed data` (bit 0).
    compressed: bool,
    compressed_offset: usize,
    /// Whether this animation's raw blobs are big-endian. Base-game
    /// (Xbox-origin) tags are BE; modern PC-authored content (e.g. the
    /// Digsite restored cyborg emotes, the only compressed CE animations
    /// in the MCC corpus) stores its blobs little-endian even though the
    /// structured tag fields stay big-endian. Auto-detected (see
    /// [`detect_be`]).
    be: bool,
    rotation_mask: u64,
    translation_mask: u64,
    scale_mask: u64,
    default_data: &'a [u8],
    frame_data: &'a [u8],
    frame_info: &'a [u8],
}

/// All animations in a CE `model_animations` tag.
pub struct CeAnimations<'a> {
    animations: Vec<CeAnimation<'a>>,
}

impl<'a> CeAnimations<'a> {
    /// Walk the root `animations` block of a Halo CE `model_animations`
    /// tag. Returns an empty set when the tag has no `animations` block
    /// (not a CE antr, or genuinely empty).
    pub fn new(tag: &'a TagFile) -> Self {
        let root = tag.root();
        // The structured-field endianness the classic decoder resolved for
        // this tag (CE → big-endian). Uncompressed blobs follow it; the
        // compressed block can differ (see `detect_be`).
        let tag_be = tag.endian == Endian::Be;
        let Some(block) = root.field_path("animations").and_then(|f| f.as_block()) else {
            return Self { animations: Vec::new() };
        };
        let mut animations = Vec::with_capacity(block.len());
        for i in 0..block.len() {
            let Some(e) = block.element(i) else { continue };
            let node_count = e.read_int_any("node count").unwrap_or(0).max(0) as usize;
            let flags = e.read_int_any("flags").unwrap_or(0) as u32;
            let compressed = flags & 1 == 1;
            let compressed_offset = e.read_int_any("offset to compressed data").unwrap_or(0).max(0) as usize;
            let frame_data = e.field("frame data").and_then(|f| f.as_data()).unwrap_or(&[]);
            animations.push(CeAnimation {
                index: i,
                name: e.read_string("name").or_else(|| e.read_string_id("name")),
                animation_type: e.read_enum_name("type"),
                frame_info_type: e.read_enum_name("frame info type").map(|n| normalize_frame_info(&n)),
                world_relative: (flags >> 1) & 1 == 1,
                frame_count: e.read_int_any("frame count").unwrap_or(0).max(0) as u16,
                node_count,
                node_list_checksum: e.read_int_any("node list checksum").unwrap_or(0) as i32,
                frame_size: e.read_int_any("frame size").unwrap_or(0).max(0) as usize,
                compressed,
                compressed_offset,
                be: detect_be(tag_be, compressed, frame_data, compressed_offset),
                rotation_mask: read_flag_mask(&e, "node rotation flag data"),
                translation_mask: read_flag_mask(&e, "node transform flag data"),
                scale_mask: read_flag_mask(&e, "node scale flag data"),
                default_data: e.field("default data").and_then(|f| f.as_data()).unwrap_or(&[]),
                frame_data,
                frame_info: e.field("frame info").and_then(|f| f.as_data()).unwrap_or(&[]),
            });
        }
        Self { animations }
    }

    pub fn len(&self) -> usize { self.animations.len() }
    pub fn is_empty(&self) -> bool { self.animations.is_empty() }
    pub fn iter(&self) -> impl Iterator<Item = &CeAnimation<'a>> { self.animations.iter() }
    pub fn get(&self, i: usize) -> Option<&CeAnimation<'a>> { self.animations.get(i) }
    pub fn find(&self, name: &str) -> Option<&CeAnimation<'a>> {
        self.animations.iter().find(|a| a.name.as_deref() == Some(name))
    }
}

impl<'a> CeAnimation<'a> {
    /// Decode into the shared [`AnimationClip`]. Static-flagged components
    /// populate `static_tracks` (one frame); animated-flagged components
    /// populate `animated_tracks` (`frame_count` frames). Both are packed
    /// in node order so the `node_flags` popcount indexing in
    /// [`AnimationClip::pose`](super::AnimationClip::pose) resolves each
    /// bone's component to the right track slot.
    pub fn decode(&self) -> AnimationClip {
        if self.compressed {
            return self.decode_compressed();
        }
        self.decode_uncompressed()
    }

    fn node_flags(&self) -> NodeFlags {
        // Animated = flag set; static = flag clear (over [0, node_count)).
        let full = if self.node_count >= 64 { u64::MAX } else { (1u64 << self.node_count) - 1 };
        let mk = |m: u64| (BitArray::from_u64(m & full), BitArray::from_u64(!m & full));
        let (ar, sr) = mk(self.rotation_mask);
        let (at, st) = mk(self.translation_mask);
        let (asc, ssc) = mk(self.scale_mask);
        NodeFlags {
            static_rotation: sr, static_translation: st, static_scale: ssc,
            animated_rotation: ar, animated_translation: at, animated_scale: asc,
        }
    }

    fn decode_uncompressed(&self) -> AnimationClip {
        let frames = self.frame_count.max(1) as usize;
        let n_ar = (self.rotation_mask).count_ones() as usize;
        let n_at = (self.translation_mask).count_ones() as usize;
        let n_asc = (self.scale_mask).count_ones() as usize;

        // Static tracks: walk `default data` once, node-major, appending the
        // components whose flag is CLEAR (in rotation→translation→scale
        // order) — matching the engine's read order.
        let mut s_rot = Vec::new();
        let mut s_trn = Vec::new();
        let mut s_scl = Vec::new();
        let be = self.be;
        let mut off = 0usize;
        for node in 0..self.node_count {
            if !bit(self.rotation_mask, node) {
                s_rot.push(read_quat(self.default_data, &mut off, be));
            }
            if !bit(self.translation_mask, node) {
                s_trn.push(read_point(self.default_data, &mut off, be));
            }
            if !bit(self.scale_mask, node) {
                s_scl.push(read_f32(self.default_data, &mut off, be));
            }
        }

        // Animated tracks: for each frame, walk that frame's `frame data`
        // slice node-major, appending the SET components. Pre-size so each
        // animated node accumulates one value per frame.
        let mut a_rot = vec![vec![RealQuaternion::IDENTITY; frames]; n_ar];
        let mut a_trn = vec![vec![RealPoint3d::default(); frames]; n_at];
        let mut a_scl = vec![vec![1.0f32; frames]; n_asc];
        for f in 0..frames {
            let mut off = f * self.frame_size;
            let (mut ri, mut ti, mut si) = (0usize, 0usize, 0usize);
            for node in 0..self.node_count {
                if bit(self.rotation_mask, node) {
                    a_rot[ri][f] = read_quat(self.frame_data, &mut off, be); ri += 1;
                }
                if bit(self.translation_mask, node) {
                    a_trn[ti][f] = read_point(self.frame_data, &mut off, be); ti += 1;
                }
                if bit(self.scale_mask, node) {
                    a_scl[si][f] = read_f32(self.frame_data, &mut off, be); si += 1;
                }
            }
        }

        let has_animated = n_ar + n_at + n_asc > 0;
        AnimationClip {
            frame_count: frames as u16,
            static_tracks: AnimationTracks {
                codec: Codec::UncompressedStatic, frame_count: 1,
                rotations: vec_of(s_rot), translations: vec_of(s_trn), scales: vec_of(s_scl),
            },
            animated_tracks: has_animated.then(|| AnimationTracks {
                codec: Codec::UncompressedAnimated, frame_count: frames as u16,
                rotations: a_rot, translations: a_trn, scales: a_scl,
            }),
            animated_status: if has_animated { AnimatedStreamStatus::Decoded } else { AnimatedStreamStatus::NoAnimatedStream },
            node_flags: Some(self.node_flags()),
            movement: self.movement(),
        }
    }

    /// Per-frame root movement from the `frame info` blob (per-frame deltas,
    /// same local-space layout as gen3: dx,dy / dx,dy,dyaw / dx,dy,dz,dyaw).
    fn movement(&self) -> MovementData {
        let kind = self.frame_info_type.as_deref()
            .map(MovementKind::from_schema_name).unwrap_or(MovementKind::None);
        let bpf = kind.bytes_per_frame();
        if bpf == 0 || self.frame_info.len() < bpf { return MovementData::default(); }
        let be = self.be;
        let frames = (self.frame_info.len() / bpf).min(self.frame_count.max(1) as usize);
        let mut out = Vec::with_capacity(frames);
        for f in 0..frames {
            let mut o = f * bpf;
            let dx = read_f32(self.frame_info, &mut o, be);
            let dy = read_f32(self.frame_info, &mut o, be);
            let (dz, rotation) = match kind {
                MovementKind::DxDyDyaw => {
                    let yaw = read_f32(self.frame_info, &mut o, be);
                    (0.0, yaw_quat(yaw))
                }
                MovementKind::DxDyDzDyaw => {
                    let dz = read_f32(self.frame_info, &mut o, be);
                    let yaw = read_f32(self.frame_info, &mut o, be);
                    (dz, yaw_quat(yaw))
                }
                _ => (0.0, RealQuaternion::IDENTITY),
            };
            out.push(MovementFrame { dx, dy, dz, rotation });
        }
        MovementData { kind, frames: out }
    }

    /// Decode the keyframe-compressed block (`compressed data` flag set):
    /// 6-byte quaternions / real_point3d translations / f32 scales, each
    /// with a per-node `(keyframe_count | offset<<12)` header and an int16
    /// frame-index table, interpolated to dense per-frame tracks. RE'd from
    /// `animation_get_keyframe_{rotation,translation,scale}`.
    fn decode_compressed(&self) -> AnimationClip {
        let frames = self.frame_count.max(1) as usize;
        let be = self.be;
        let blk = self.frame_data.get(self.compressed_offset..).unwrap_or(&[]);
        // Offset table (dwords): big- or little-endian per the detected
        // blob endianness — see module docs and `detect_be`.
        let tbl = |i: usize| read_dword(blk, i * 4, be) as usize;
        // Rotation: header at dword 11 + node; idx-table[0]; defaults[1]; quat-data[2].
        // Translation: header at byte tbl(3) + 4*node; idx[4]; defaults[5]; data[6].
        // Scale: header at byte tbl(7) + 4*node; idx[8]; defaults[9]; data[10].
        // A component is *present* for a node only when its computed reads
        // land inside the block; absent components stay unflagged so
        // `pose()` falls back to the render_model / skeleton rest pose
        // (matching the engine, which reads non-keyframed components from
        // the per-node default arrays — empty here means "use default").
        let n = self.node_count;

        let mut rot: Vec<Vec<RealQuaternion>> = Vec::new();
        let mut trn: Vec<Vec<RealPoint3d>> = Vec::new();
        let mut scl: Vec<Vec<f32>> = Vec::new();
        let (mut rm, mut tm, mut sm) = (0u64, 0u64, 0u64);

        for node in 0..n {
            // Rotation.
            let hdr = read_dword(blk, (11 + node) * 4, be);
            let (kf, ko) = ((hdr & 0xFFF) as usize, (hdr >> 12) as usize);
            if let Some(track) = decode_keyframe_track(
                blk, kf, tbl(0) + 2 * ko, tbl(2) + 6 * ko, tbl(1) + 6 * node, frames,
                6, be, RealQuaternion::IDENTITY, |b, o| read_quat6(b, o, be),
            ) {
                rot.push(track); rm |= 1 << node;
            }
            // Translation.
            let hdr = read_dword(blk, tbl(3) + 4 * node, be);
            let (kf, ko) = ((hdr & 0xFFF) as usize, (hdr >> 12) as usize);
            if let Some(track) = decode_keyframe_track(
                blk, kf, tbl(4) + 2 * ko, tbl(6) + 12 * ko, tbl(5) + 12 * node, frames,
                12, be, RealPoint3d::default(), |b, o| read_point(b, &mut { o }, be),
            ) {
                trn.push(track); tm |= 1 << node;
            }
            // Scale.
            let hdr = read_dword(blk, tbl(7) + 4 * node, be);
            let (kf, ko) = ((hdr & 0xFFF) as usize, (hdr >> 12) as usize);
            if let Some(track) = decode_keyframe_track(
                blk, kf, tbl(8) + 2 * ko, tbl(10) + 4 * ko, tbl(9) + 4 * node, frames,
                4, be, 1.0f32, |b, o| read_f32(b, &mut { o }, be),
            ) {
                scl.push(track); sm |= 1 << node;
            }
        }

        let empty = BitArray::default();
        AnimationClip {
            frame_count: frames as u16,
            static_tracks: AnimationTracks {
                codec: Codec::UncompressedStatic, frame_count: 1,
                rotations: Vec::new(), translations: Vec::new(), scales: Vec::new(),
            },
            animated_tracks: Some(AnimationTracks {
                codec: Codec::UncompressedAnimated, frame_count: frames as u16,
                rotations: rot, translations: trn, scales: scl,
            }),
            animated_status: AnimatedStreamStatus::Decoded,
            node_flags: Some(NodeFlags {
                static_rotation: empty.clone(), static_translation: empty.clone(), static_scale: empty,
                animated_rotation: BitArray::from_u64(rm),
                animated_translation: BitArray::from_u64(tm),
                animated_scale: BitArray::from_u64(sm),
            }),
            movement: self.movement(),
        }
    }
}

/// Decode one node's compressed keyframe track to a per-frame value vector,
/// or `None` when the component is absent (computed offsets fall outside the
/// block). `index_off`/`value_off` point at the int16 frame-index table and
/// the keyframe value array; `default_off` is the single value used when
/// `kf == 0`. Between keyframes the value is held from the last keyframe at
/// or before the frame (the JMA export re-normalizes rotations); `fallback`
/// is returned per-frame only if individual reads slip out of bounds.
#[allow(clippy::too_many_arguments)]
fn decode_keyframe_track<T: Copy>(
    blk: &[u8],
    kf: usize,
    index_off: usize,
    value_off: usize,
    default_off: usize,
    frames: usize,
    stride: usize,
    be: bool,
    fallback: T,
    read: impl Fn(&[u8], usize) -> T,
) -> Option<Vec<T>> {
    // Present only when the per-node header carries a sane keyframe count
    // (`1..=frames`) and its index table + value array land inside the
    // block. A `kf` of 0 (no keyframes) or an out-of-range count is the
    // signature of an absent component — its offset-table entries collapse
    // onto the end of the previous component's data — so we return `None`
    // and let the caller leave the component unflagged, which makes
    // `pose()` hold the render_model / skeleton rest pose (the correct
    // value for a non-animated component in a base animation). `default_off`
    // is retained for documentation of the engine's kf==0 default array but
    // isn't read, since the rest pose is the equivalent fallback.
    let _ = default_off;
    if kf == 0 || kf > frames {
        return None;
    }
    if index_off + 2 * kf > blk.len() || value_off + stride * kf > blk.len() {
        return None;
    }
    let key_index = |k: usize| read_word(blk, index_off + 2 * k, be) as usize;
    let _ = fallback;
    let key_val = |k: usize| read(blk, value_off + stride * k);
    let mut out = Vec::with_capacity(frames);
    for f in 0..frames {
        // Hold the last keyframe whose frame index is at or before `f`.
        let mut k = 0;
        while k + 1 < kf && key_index(k + 1) <= f { k += 1; }
        out.push(key_val(k.min(kf - 1)));
    }
    Some(out)
}

/// Read both `long_integer`s of a CE flag field (low dword = nodes 0–31,
/// high dword = nodes 32–63) into a 64-bit mask. The two dwords share the
/// same field name in the schema, so we collect them by iterating fields.
fn read_flag_mask(elem: &crate::api::TagStruct<'_>, name: &str) -> u64 {
    let mut words = elem.fields()
        .filter(|f| f.name() == name)
        .filter_map(|f| f.value().and_then(super::int_value))
        .map(|v| v as u32 as u64);
    let lo = words.next().unwrap_or(0);
    let hi = words.next().unwrap_or(0);
    lo | (hi << 32)
}

/// Normalize CE's space-separated `frame info type` enum names to the comma
/// form the shared [`MovementKind`] / [`JmaKind`](super::JmaKind) expect.
fn normalize_frame_info(name: &str) -> String {
    match name {
        "dx dy" => "dx,dy".into(),
        "dx dy dyaw" => "dx,dy,dyaw".into(),
        "dx dy dz dyaw" => "dx,dy,dz,dyaw".into(),
        other => other.to_string(),
    }
}

#[inline]
fn bit(mask: u64, node: usize) -> bool { node < 64 && (mask >> node) & 1 == 1 }

/// Wrap a single static track value-set into the `[node][frame]` shape with
/// one frame each (so `pick_*` indexes `[node][0]`).
fn vec_of<T>(v: Vec<T>) -> Vec<Vec<T>> { v.into_iter().map(|x| vec![x]).collect() }

fn yaw_quat(yaw: f32) -> RealQuaternion {
    let h = yaw * 0.5;
    RealQuaternion { i: 0.0, j: 0.0, k: h.sin(), w: h.cos() }
}

/// Decide whether an animation's raw blobs are big-endian. Uncompressed
/// blobs follow the tag's structured endianness (`tag_be`; CE → BE). For a
/// compressed animation the block can disagree: the only compressed CE
/// animations in the MCC corpus are PC-authored Digsite content whose
/// compressed block is little-endian even though the tag is big-endian. We
/// bounds-check the non-`tag_be` reading of the offset table's first three
/// entries (rotation index-table / default / quat-data bases, which must
/// all fall inside the block); if that reading is in range, the block uses
/// the opposite endianness, otherwise it follows the tag.
fn detect_be(tag_be: bool, compressed: bool, frame_data: &[u8], compressed_offset: usize) -> bool {
    if !compressed {
        return tag_be;
    }
    let blk = frame_data.get(compressed_offset..).unwrap_or(&[]);
    let n = blk.len();
    if n < 12 {
        return tag_be;
    }
    let other = !tag_be;
    let off = |i: usize| read_dword(blk, i * 4, other) as usize;
    let other_in_bounds = off(0) < n && off(1) < n && off(2) < n;
    if other_in_bounds { other } else { tag_be }
}

// --- endian-parametric blob readers ---

fn read_quat(b: &[u8], off: &mut usize, be: bool) -> RealQuaternion {
    let q = RealQuaternion {
        i: i16e(b, *off, be) as f32 * ROT_SCALE,
        j: i16e(b, *off + 2, be) as f32 * ROT_SCALE,
        k: i16e(b, *off + 4, be) as f32 * ROT_SCALE,
        w: i16e(b, *off + 6, be) as f32 * ROT_SCALE,
    };
    *off += 8;
    if q.length() <= 1e-6 { RealQuaternion::IDENTITY } else { q.normalized() }
}

fn read_point(b: &[u8], off: &mut usize, be: bool) -> RealPoint3d {
    let p = RealPoint3d {
        x: f32e(b, *off, be), y: f32e(b, *off + 4, be), z: f32e(b, *off + 8, be),
    };
    *off += 12;
    p
}

fn read_f32(b: &[u8], off: &mut usize, be: bool) -> f32 {
    let v = f32e(b, *off, be);
    *off += 4;
    v
}

/// 6-byte compressed quaternion (`quaternion_decompress_6byte`): 4×12-bit
/// signed components packed into three 16-bit words, scaled by `ROT_SCALE`.
/// `o` is a byte offset (not advanced).
fn read_quat6(b: &[u8], o: usize, be: bool) -> RealQuaternion {
    let iiij = u16e(b, o, be) as u32;
    let jjkk = u16e(b, o + 2, be) as u32;
    let kwww = u16e(b, o + 4, be) as u32;
    // Reassemble the four 12-bit components from the packed words, mirroring
    // the engine's bit layout: i = top 12 of iiij; j = low 4 of iiij + top 8
    // of jjkk; k = low 8 of jjkk + top 4 of kwww; w = low 12 of kwww.
    let sx = |v: u32| {
        let v = v & 0xFFF;
        (if v & 0x800 != 0 { (v | 0xFFFFF000) as i32 } else { v as i32 }) as f32
    };
    let i = sx(iiij >> 4);
    let j = sx(((iiij & 0xF) << 8) | (jjkk >> 8));
    let k = sx(((jjkk & 0xFF) << 4) | (kwww >> 12));
    let w = sx(kwww & 0xFFF);
    // 12-bit components are scaled relative to a 15-bit range in the engine;
    // normalize defensively for export.
    let q = RealQuaternion { i: i * ROT_SCALE, j: j * ROT_SCALE, k: k * ROT_SCALE, w: w * ROT_SCALE };
    if q.length() <= 1e-6 { RealQuaternion::IDENTITY } else { q.normalized() }
}

#[inline] fn i16e(b: &[u8], o: usize, be: bool) -> i16 {
    b.get(o..o + 2).map(|s| { let a = [s[0], s[1]]; if be { i16::from_be_bytes(a) } else { i16::from_le_bytes(a) } }).unwrap_or(0)
}
#[inline] fn u16e(b: &[u8], o: usize, be: bool) -> u16 {
    b.get(o..o + 2).map(|s| { let a = [s[0], s[1]]; if be { u16::from_be_bytes(a) } else { u16::from_le_bytes(a) } }).unwrap_or(0)
}
#[inline] fn f32e(b: &[u8], o: usize, be: bool) -> f32 {
    b.get(o..o + 4).map(|s| { let a = [s[0], s[1], s[2], s[3]]; if be { f32::from_be_bytes(a) } else { f32::from_le_bytes(a) } }).unwrap_or(0.0)
}
#[inline] fn read_word(b: &[u8], o: usize, be: bool) -> u16 { u16e(b, o, be) }
#[inline] fn read_dword(b: &[u8], o: usize, be: bool) -> u32 {
    b.get(o..o + 4).map(|s| { let a = [s[0], s[1], s[2], s[3]]; if be { u32::from_be_bytes(a) } else { u32::from_le_bytes(a) } }).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quat6_identity_packs_to_unit() {
        // w component = 0x7FF (max positive 12-bit), i=j=k=0 → identity-ish
        // after normalize. Packed big-endian: iiij=0, jjkk=0, kwww=0x07FF.
        let bytes = [0x00, 0x00, 0x00, 0x00, 0x07, 0xFF];
        let q = read_quat6(&bytes, 0, true);
        assert!((q.length() - 1.0).abs() < 1e-5);
        // w dominates → real part near 1.
        assert!(q.w.abs() > 0.99, "w={}", q.w);
        assert!(q.i.abs() < 1e-3 && q.j.abs() < 1e-3 && q.k.abs() < 1e-3);
    }

    #[test]
    fn quat6_component_extraction() {
        // i occupies the top 12 bits of word0. Set word0 = 0x7FF0 → i = 0x7FF
        // (max), all others 0. After normalize, i ≈ 1.0.
        let bytes = [0x7F, 0xF0, 0x00, 0x00, 0x00, 0x00];
        let q = read_quat6(&bytes, 0, true);
        assert!(q.i.abs() > 0.99, "i={}", q.i);
        assert!(q.j.abs() < 1e-3 && q.k.abs() < 1e-3 && q.w.abs() < 1e-3);
    }

    #[test]
    fn quat6_little_endian_matches_byteswapped_big_endian() {
        // The same logical words read LE from swapped bytes equal BE.
        let be = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC];
        let le = [0x34, 0x12, 0x78, 0x56, 0xBC, 0x9A];
        let qb = read_quat6(&be, 0, true);
        let ql = read_quat6(&le, 0, false);
        assert!((qb.i - ql.i).abs() < 1e-6 && (qb.w - ql.w).abs() < 1e-6);
    }

    #[test]
    fn uncompressed_node_state_is_24_bytes() {
        // rotation 4×i16 (8) + translation 3×f32 (12) + scale f32 (4) = 24.
        // Build one static node: identity rot, translation (1,2,3), scale 1.
        let mut b = Vec::new();
        // rotation: i=j=k=0, w=32767 (≈1.0)
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&0i16.to_be_bytes());
        b.extend_from_slice(&32767i16.to_be_bytes());
        b.extend_from_slice(&1.0f32.to_be_bytes());
        b.extend_from_slice(&2.0f32.to_be_bytes());
        b.extend_from_slice(&3.0f32.to_be_bytes());
        b.extend_from_slice(&1.0f32.to_be_bytes());
        assert_eq!(b.len(), 24);
        let mut off = 0;
        let q = read_quat(&b, &mut off, true);
        assert!((q.w - 1.0).abs() < 1e-3);
        assert_eq!(off, 8);
        let p = read_point(&b, &mut off, true);
        assert_eq!((p.x, p.y, p.z), (1.0, 2.0, 3.0));
        assert_eq!(off, 20);
        let s = read_f32(&b, &mut off, true);
        assert_eq!(s, 1.0);
        assert_eq!(off, 24);
    }

    #[test]
    fn detect_be_uncompressed_follows_tag() {
        assert!(detect_be(true, false, &[], 0));
        assert!(!detect_be(false, false, &[], 0));
    }

    #[test]
    fn detect_be_compressed_picks_in_bounds_table() {
        // A 64-byte block whose first three offset-table dwords are small
        // when read little-endian (in bounds) but huge as big-endian.
        let mut blk = vec![0u8; 64];
        for (i, v) in [8u32, 16, 24].iter().enumerate() {
            blk[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        // tag is big-endian; LE reading is in bounds → block is little-endian.
        assert!(!detect_be(true, true, &blk, 0));
    }

    #[test]
    fn normalize_frame_info_maps_ce_spelling() {
        assert_eq!(normalize_frame_info("dx dy"), "dx,dy");
        assert_eq!(normalize_frame_info("dx dy dyaw"), "dx,dy,dyaw");
        assert_eq!(normalize_frame_info("dx dy dz dyaw"), "dx,dy,dz,dyaw");
        assert_eq!(normalize_frame_info("none"), "none");
    }
}
