//! Codec dispatch + per-slot decoders for jmad animation blobs.
//!
//! Slots 0..=8 are present in Halo 3 (verified against
//! `g_codec_descriptions[9]` at `0x181170f90` in
//! `halo3_dllcache_play.dll`). Slots 9..=11 added in Reach / Halo
//! Online / later. Each decoder turns a contiguous run of bytes
//! starting with the codec selector into an [`AnimationTracks`]
//! (`(rotations, translations, scales)` outer-indexed by codec node,
//! inner-indexed by frame). Compose against a [`super::Skeleton`]
//! via [`super::AnimationClip::pose`] to get a per-frame transform
//! table.
//!
//! Engine-specific ordering quirks (H3 hardcoded total counts vs
//! Reach cumulative-sum) are handled by the offsets we read out of
//! the codec header rather than per-slot branches. See
//! `project_jmad_extraction_shipped` in auto-memory for the
//! reference binaries.

use crate::math::{RealPoint3d, RealQuaternion, RealVector3d};

use super::{
    AnimatedStreamStatus, AnimationClip, AnimationError, AnimationGroup, AnimationTracks,
    BitArray, MovementData, MovementFrame, MovementKind, NodeFlags, SizeLayout,
};

//================================================================================
// Codec dispatch + per-slot decoders
//================================================================================

/// Animation codec selector — the first byte of every animation's
/// codec stream. Slots 0..=8 are present in Halo 3; 9..=11 added in
/// Reach / Halo Online / later. Verified against
/// `g_codec_descriptions[9]` at `0x181170f90` in
/// `halo3_dllcache_play.dll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Codec {
    NoCompression = 0,
    UncompressedStatic = 1,
    UncompressedAnimated = 2,
    EightByteQuantizedRotationOnly = 3,
    ByteKeyframeLightlyQuantized = 4,
    WordKeyframeLightlyQuantized = 5,
    ReverseByteKeyframeLightlyQuantized = 6,
    ReverseWordKeyframeLightlyQuantized = 7,
    BlendScreen = 8,
    Curve = 9,
    RevisedCurve = 10,
    SharedStatic = 11,
}

impl Codec {
    /// Map the codec byte at the start of an `animation_data` blob to
    /// a [`Codec`] variant. Returns `None` for bytes outside `0..=11`.
    pub fn from_byte(b: u8) -> Option<Self> {
        Some(match b {
            0 => Self::NoCompression,
            1 => Self::UncompressedStatic,
            2 => Self::UncompressedAnimated,
            3 => Self::EightByteQuantizedRotationOnly,
            4 => Self::ByteKeyframeLightlyQuantized,
            5 => Self::WordKeyframeLightlyQuantized,
            6 => Self::ReverseByteKeyframeLightlyQuantized,
            7 => Self::ReverseWordKeyframeLightlyQuantized,
            8 => Self::BlendScreen,
            9 => Self::Curve,
            10 => Self::RevisedCurve,
            11 => Self::SharedStatic,
            _ => return None,
        })
    }
}

/// Fields we actually use from the 20-byte `s_animation_codec_header`.
/// `compression_type` (byte 0) is dispatched separately via
/// [`Codec::from_byte`]; `error_value` and `compression_rate` are
/// surfaced via [`PackedDataSizes`] when needed. The struct shape is
/// verified against `animation_compute_orientations_interface.h` in
/// the Halo 3 PDB; `SIZE` reflects the on-disk size, not this Rust
/// struct's size.
#[derive(Debug, Clone, Copy)]
struct AnimationCodecHeader {
    total_rotated_nodes: u8,
    total_translated_nodes: u8,
    total_scaled_nodes: u8,
    translation_offset: u32,
    scale_offset: u32,
}

impl AnimationCodecHeader {
    const SIZE: usize = 20;

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
        Some(Self {
            total_rotated_nodes: bytes[1],
            total_translated_nodes: bytes[2],
            total_scaled_nodes: bytes[3],
            translation_offset: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
            scale_offset: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
        })
    }
}

/// 32-byte fullframe codec header — base + three per-component
/// strides (bytes per node × frame_count for animated codecs, bytes
/// per node for static).
#[derive(Debug, Clone, Copy)]
struct FullframeCodecHeader {
    base: AnimationCodecHeader,
    rotation_stride: u32,
    translation_stride: u32,
    scale_stride: u32,
}

impl FullframeCodecHeader {
    const SIZE: usize = 32;

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
        Some(Self {
            base: AnimationCodecHeader::from_bytes(bytes)?,
            rotation_stride: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            translation_stride: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            scale_stride: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
        })
    }
}

/// 48-byte keyframe codec header — base + per-component time-table
/// and payload-table byte offsets (from blob start). Order verified
/// against `s_keyframe_codec_header` in `compression_tools.h` from
/// the Halo 3 PDB.
#[derive(Debug, Clone, Copy)]
struct KeyframeCodecHeader {
    base: AnimationCodecHeader,
    rotation_key_time_offset: u32,
    translation_key_time_offset: u32,
    scale_key_time_offset: u32,
    rotation_key_payload_offset: u32,
    translation_key_payload_offset: u32,
    scale_key_payload_offset: u32,
}

impl KeyframeCodecHeader {
    const SIZE: usize = 48;

    fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::SIZE { return None; }
        Some(Self {
            base: AnimationCodecHeader::from_bytes(bytes)?,
            rotation_key_time_offset: u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            translation_key_time_offset: u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            scale_key_time_offset: u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            rotation_key_payload_offset: u32::from_le_bytes(bytes[32..36].try_into().unwrap()),
            translation_key_payload_offset: u32::from_le_bytes(bytes[36..40].try_into().unwrap()),
            scale_key_payload_offset: u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            // bytes[44..48] is the trailing pad we don't need.
        })
    }
}
impl<'a> AnimationGroup<'a> {
    /// Decode the blob into an [`AnimationClip`] — both the static
    /// rest-pose stream and the per-frame animated stream, plus
    /// per-bone flag bitarrays and per-frame movement deltas.
    ///
    /// Animated-stream codecs that aren't implemented (currently just
    /// `SharedStatic`) surface as
    /// [`AnimatedStreamStatus::Unsupported`] on the returned clip
    /// rather than an `Err` — the static rest pose is independently
    /// useful even if the animated stream can't be read.
    ///
    /// Hard errors (`Err`) only fire for genuinely malformed input:
    /// truncated codec headers, codec_byte ∉ 0..=11, no payload at all.
    pub fn decode(&self) -> Result<AnimationClip, AnimationError> {
        let codec_byte = self.codec_byte.ok_or(AnimationError::NoCodecPayload)?;
        let codec = Codec::from_byte(codec_byte).ok_or(AnimationError::UnknownCodec(codec_byte))?;
        // Some Reach animations have no static rest pose at all —
        // the blob starts directly with an animated codec. Detect via
        // either codec ≠ UncompressedStatic OR (Reach AND data_sizes[0] == 0).
        let static_first_size = self.data_sizes.as_ref()
            .and_then(|d| d.fields.first())
            .map(|(_, v)| *v as usize)
            .unwrap_or(0);
        let has_static_stream = matches!(codec, Codec::UncompressedStatic)
            && static_first_size > 0;
        let static_tracks = if has_static_stream {
            decode_uncompressed_static(self.blob)?
        } else {
            // No static stream — start with empty static tracks so
            // pose composition has something to fall back to. The
            // animated stream then starts at offset 0.
            AnimationTracks {
                codec: Codec::UncompressedStatic,
                frame_count: 1,
                rotations: Vec::new(),
                translations: Vec::new(),
                scales: Vec::new(),
            }
        };

        // Animated stream (if any) starts at the `default_data`
        // offset. TagTool's `AnimationResourceData.Read` consumes the
        // static stream then jumps to this position to find the next
        // codec_byte.
        //
        // When the animated codec isn't yet supported we don't fail
        // the whole decode — the static rest pose is independently
        // useful, so we surface the animated-stream outcome via
        // `animated_status` instead.
        let frame_count = self.codec_frame_count
            .or(Some(self.frame_count))
            .map(|f| f.max(1) as u16)
            .unwrap_or(1);
        // Reach uses cumulative-sum from positional indices in the
        // (renamed-but-misleading) `data sizes` struct: index 0 = static
        // codec stream, index 1 = animated codec stream, 2/3 = flag
        // triplets, 4 = movement, 5 = pill, 6+ = Reach-only extras.
        // H3 instead has hardcoded offsets where `default_data` is the
        // static codec stream, animated codec immediately follows.
        let layout = self.data_sizes.as_ref().map(|d| d.layout()).unwrap_or(SizeLayout::H3);
        let static_size = if has_static_stream {
            match layout {
                SizeLayout::H3 => self.data_sizes.as_ref().map(|d| d.get("default_data") as usize).unwrap_or(0),
                SizeLayout::Reach => static_first_size,
            }
        } else { 0 };
        // Per the Reach binary's `c_animation_data::get_animation_compression_codec`
        // (`animation_data.cpp:74`), the animated codec_byte ALWAYS lives at
        // `get_data_offset(e_internal_data_type::1) = cumsum(sizes[0..0])`.
        // For Reach with cumulative-sum layout that's `static_size` (which is 0
        // when no static stream). For H3 the static codec stream is at offset 0
        // when present, so the animated section starts at `static_size` either
        // way — no-static animations get the whole blob routed through the
        // animated decoder.
        let animated_offset = static_size;
        let animated_blob_len = match layout {
            SizeLayout::Reach => self.data_sizes.as_ref()
                .and_then(|d| d.fields.get(1))
                .map(|(_, v)| *v as usize)
                .unwrap_or(0),
            SizeLayout::H3 => self.blob.len().saturating_sub(animated_offset),
        };
        let (animated_tracks, animated_status, animated_codec_size) = if animated_offset >= self.blob.len() || animated_blob_len == 0 {
            (None, AnimatedStreamStatus::NoAnimatedStream, None)
        } else {
            let anim_end = (animated_offset + animated_blob_len).min(self.blob.len());
            let anim_blob = &self.blob[animated_offset..anim_end];
            let anim_byte = anim_blob[0];
            let (tracks, status) = match Codec::from_byte(anim_byte) {
                None => (None, AnimatedStreamStatus::Unknown(anim_byte)),
                Some(c @ Codec::EightByteQuantizedRotationOnly) =>
                    try_animated(c, || decode_fullframe(anim_blob, c, frame_count, true)),
                Some(c @ (Codec::UncompressedAnimated | Codec::BlendScreen)) =>
                    try_animated(c, || decode_fullframe(anim_blob, c, frame_count, false)),
                Some(c @ (Codec::ByteKeyframeLightlyQuantized
                    | Codec::ReverseByteKeyframeLightlyQuantized)) =>
                    try_animated(c, || decode_keyframe(anim_blob, c, frame_count, 1)),
                Some(c @ (Codec::WordKeyframeLightlyQuantized
                    | Codec::ReverseWordKeyframeLightlyQuantized)) =>
                    try_animated(c, || decode_keyframe(anim_blob, c, frame_count, 2)),
                Some(c @ Codec::Curve) =>
                    try_animated(c, || decode_curve(anim_blob, c, frame_count, false)),
                Some(c @ Codec::RevisedCurve) =>
                    try_animated(c, || decode_curve(anim_blob, c, frame_count, true)),
                Some(other) => (None, AnimatedStreamStatus::Unsupported(other)),
            };
            // For Reach, the animated codec size is recorded
            // explicitly at positional index 1 in `data sizes`. For
            // H3 we infer it from the codec header / payload extents.
            let size = match layout {
                SizeLayout::Reach => self.data_sizes.as_ref()
                    .and_then(|d| d.fields.get(1))
                    .map(|(_, v)| *v as usize),
                SizeLayout::H3 => matches!(status, AnimatedStreamStatus::Decoded)
                    .then(|| Codec::from_byte(anim_byte).and_then(|c| animated_codec_stream_size(anim_blob, c)))
                    .flatten(),
            };
            (tracks, status, size)
        };

        let node_flags = self.data_sizes.as_ref().and_then(|d| {
            // Reach stores flags at positional indices 2 + 3 with
            // explicit sizes; H3 places them right after the animated
            // codec with sizes carried by the named fields.
            let (off, static_total, animated_total) = match layout {
                SizeLayout::Reach => {
                    let cumsum = d.fields.iter().take(2).map(|(_, v)| *v as usize).sum::<usize>();
                    let s = d.fields.get(2).map(|(_, v)| *v as usize).unwrap_or(0);
                    let a = d.fields.get(3).map(|(_, v)| *v as usize).unwrap_or(0);
                    (cumsum, s, a)
                }
                SizeLayout::H3 => (
                    static_size + animated_codec_size?,
                    d.get("static_node_flags") as usize,
                    d.get("animated_node_flags") as usize,
                ),
            };
            read_node_flags(self.blob, off, static_total, animated_total)
        });

        // Movement offset+size: Reach has it at positional index 4;
        // H3 places it sequentially right after the animated node flags
        // (static codec → animated codec → static flags → animated flags → movement),
        // matching TagTool's `AnimationResourceData.Read` and Foundry's
        // `_read_movement_data`. The `pill_offset_data` field tracks pill data
        // elsewhere in the blob and isn't a `blob.len() - pill - movement` anchor.
        let movement = self.data_sizes.as_ref().map(|d| {
            let (off, size) = match layout {
                SizeLayout::Reach => (
                    d.fields.iter().take(4).map(|(_, v)| *v as usize).sum::<usize>(),
                    d.fields.get(4).map(|(_, v)| *v as usize).unwrap_or(0),
                ),
                SizeLayout::H3 => {
                    let m = d.get("movement_data") as usize;
                    let static_total = d.get("static_node_flags") as usize;
                    let animated_total = d.get("animated_node_flags") as usize;
                    let off = static_size
                        + animated_codec_size.unwrap_or(0)
                        + static_total
                        + animated_total;
                    (off, m)
                }
            };
            read_movement_at(
                self.blob, off, size,
                self.movement_type.as_deref().or(self.frame_info_type.as_deref()),
                frame_count as usize,
            )
        }).unwrap_or_default();

        Ok(AnimationClip {
            frame_count,
            static_tracks,
            animated_tracks,
            animated_status,
            node_flags,
            movement,
        })
    }
}

/// Read per-frame root movement from `blob[offset..offset+size]`.
/// Returns the `MovementKind::None` empty default if the slice is
/// out of bounds, the kind doesn't divide cleanly into the slice, or
/// `frame_info_type` is `none`.
///
/// `DxDyDzDangleAxis` reads the 3-vector at `+12..+24` and uses the
/// component at `+20` as dyaw. Full angle-axis composition isn't
/// implemented — kept as-is for parity with TagTool/Foundry.
fn read_movement_at(
    blob: &[u8],
    offset: usize,
    movement_bytes: usize,
    frame_info_type: Option<&str>,
    frame_count: usize,
) -> MovementData {
    let kind = frame_info_type.map(MovementKind::from_schema_name).unwrap_or(MovementKind::None);
    if kind == MovementKind::None || movement_bytes == 0 { return MovementData::default(); }
    let bpf = kind.bytes_per_frame();
    if bpf == 0 || movement_bytes % bpf != 0 { return MovementData::default(); }
    if offset.checked_add(movement_bytes).map_or(true, |end| end > blob.len()) {
        return MovementData::default();
    }
    let read_count = (movement_bytes / bpf).min(frame_count);
    let mut frames = Vec::with_capacity(read_count);
    for i in 0..read_count {
        let off = offset + i * bpf;
        let f = match kind {
            MovementKind::DxDy => MovementFrame {
                dx: f32_at(blob, off), dy: f32_at(blob, off + 4),
                ..Default::default()
            },
            MovementKind::DxDyDyaw => MovementFrame {
                dx: f32_at(blob, off), dy: f32_at(blob, off + 4),
                dyaw: f32_at(blob, off + 8), ..Default::default()
            },
            MovementKind::DxDyDzDyaw => MovementFrame {
                dx: f32_at(blob, off), dy: f32_at(blob, off + 4),
                dz: f32_at(blob, off + 8), dyaw: f32_at(blob, off + 12),
            },
            MovementKind::DxDyDzDangleAxis => MovementFrame {
                dx: f32_at(blob, off), dy: f32_at(blob, off + 4),
                dz: f32_at(blob, off + 8), dyaw: f32_at(blob, off + 20),
            },
            MovementKind::None => MovementFrame::default(),
        };
        frames.push(f);
    }
    MovementData { kind, frames }
}

/// Read the 6 node-flag BitArrays from a blob given the start offset
/// and total byte sizes for each flag triplet (rotation/translation/
/// scale × static/animated). Each triplet is split into 3 equal-sized
/// u32 bitarrays. Returns `None` if both triplets are zero or sizes
/// don't divide cleanly into 3.
fn read_node_flags(
    blob: &[u8],
    static_off: usize,
    static_total: usize,
    animated_total: usize,
) -> Option<NodeFlags> {
    if static_total == 0 && animated_total == 0 { return None; }
    if static_total % 3 != 0 || animated_total % 3 != 0 { return None; }
    let static_end = static_off.checked_add(static_total)?;
    let animated_end = static_end.checked_add(animated_total)?;
    if animated_end > blob.len() { return None; }
    let static_per = static_total / 3;
    let animated_per = animated_total / 3;
    let mut out = NodeFlags::default();
    if static_per > 0 {
        out.static_rotation = BitArray::from_bytes(&blob[static_off..static_off + static_per]);
        out.static_translation = BitArray::from_bytes(&blob[static_off + static_per..static_off + 2 * static_per]);
        out.static_scale = BitArray::from_bytes(&blob[static_off + 2 * static_per..static_end]);
    }
    if animated_per > 0 {
        let a = static_end;
        out.animated_rotation = BitArray::from_bytes(&blob[a..a + animated_per]);
        out.animated_translation = BitArray::from_bytes(&blob[a + animated_per..a + 2 * animated_per]);
        out.animated_scale = BitArray::from_bytes(&blob[a + 2 * animated_per..animated_end]);
    }
    Some(out)
}

/// Compute the on-disk byte size of an animated codec stream so the
/// caller can locate the flag table that follows it.
///
/// - Fullframe codecs (slots 1, 2, 3, 8): size = `32 + n_rot * rot_stride
///   + n_trans * trans_stride + n_scale * scale_stride`. Equivalent to
///   `scale_offset + n_scale * scale_stride` but we compute from the
///   header strides directly to be robust to MCC reorderings.
/// - Keyframe codecs (slots 4-7): size = max-payload-end across the
///   three components. `*_key_payload_offset + count * sizeof(elem)`
///   for whichever component had the highest payload offset.
fn animated_codec_stream_size(blob: &[u8], codec: Codec) -> Option<usize> {
    use Codec::*;
    match codec {
        UncompressedStatic | UncompressedAnimated | EightByteQuantizedRotationOnly | BlendScreen => {
            let h = FullframeCodecHeader::from_bytes(blob)?;
            let n_rot = h.base.total_rotated_nodes as usize;
            let n_trans = h.base.total_translated_nodes as usize;
            let n_scale = h.base.total_scaled_nodes as usize;
            // Use header offsets when set (translation_offset and
            // scale_offset are absolute from codec base); fall back to
            // contiguous stride math when zero.
            let rot_end = 32 + n_rot * h.rotation_stride as usize;
            let trans_end = if n_trans > 0 {
                h.base.translation_offset as usize + n_trans * h.translation_stride as usize
            } else { 0 };
            let scale_end = if n_scale > 0 {
                h.base.scale_offset as usize + n_scale * h.scale_stride as usize
            } else { 0 };
            Some(rot_end.max(trans_end).max(scale_end))
        }
        ByteKeyframeLightlyQuantized | WordKeyframeLightlyQuantized
        | ReverseByteKeyframeLightlyQuantized | ReverseWordKeyframeLightlyQuantized => {
            let h = KeyframeCodecHeader::from_bytes(blob)?;
            let n_rot = h.base.total_rotated_nodes as usize;
            let n_trans = h.base.total_translated_nodes as usize;
            let n_scale = h.base.total_scaled_nodes as usize;
            // Per-node packed_data array sits right after the 48-byte
            // header. Sum the per-node `count` (low 12 bits) across
            // each component to get total keys.
            let key_count = |start: usize, count: usize| -> usize {
                (start..start + count)
                    .filter_map(|i| {
                        let off = 48 + i * 4;
                        let pd = u32::from_le_bytes(blob.get(off..off + 4)?.try_into().ok()?);
                        Some((pd & 0xFFF) as usize)
                    })
                    .sum()
            };
            let rot_keys = key_count(0, n_rot);
            let trans_keys = key_count(n_rot, n_trans);
            let scale_keys = key_count(n_rot + n_trans, n_scale);
            let rot_payload_end = h.rotation_key_payload_offset as usize + rot_keys * 8;
            let trans_payload_end = h.translation_key_payload_offset as usize + trans_keys * 12;
            let scale_payload_end = h.scale_key_payload_offset as usize + scale_keys * 4;
            // Time tables also have ends — include for safety.
            let time_size = match codec {
                ByteKeyframeLightlyQuantized | ReverseByteKeyframeLightlyQuantized => 1,
                _ => 2,
            };
            let rot_time_end = h.rotation_key_time_offset as usize + rot_keys * time_size;
            let trans_time_end = h.translation_key_time_offset as usize + trans_keys * time_size;
            let scale_time_end = h.scale_key_time_offset as usize + scale_keys * time_size;
            Some(
                rot_payload_end
                    .max(trans_payload_end)
                    .max(scale_payload_end)
                    .max(rot_time_end)
                    .max(trans_time_end)
                    .max(scale_time_end),
            )
        }
        _ => None,
    }
}

/// Slot 1: `c_uncompressed_static_data_codec`. One frame's worth of
/// transforms, packed:
///
/// - `rotations[i]`: `total_rotated_nodes` × 4× i16 (i,j,k,w),
///   contiguous starting at byte 32 (right after the fullframe header).
///   Each component decoded as `s / 32767.0`, then quaternion
///   normalized.
/// - `translations[i]`: `total_translated_nodes` × 3× f32, contiguous
///   starting at `translation_offset` from the codec base.
/// - `scales[i]`: `total_scaled_nodes` × 1× f32, contiguous starting
///   at `scale_offset` from the codec base.
///
/// Verified against `c_uncompressed_static_data_codec::decompress_*`
/// in the Halo 3 PDB / dllcache.
fn decode_uncompressed_static(blob: &[u8]) -> Result<AnimationTracks, AnimationError> {
    decode_fullframe(blob, Codec::UncompressedStatic, 1, /*quat_8byte=*/true)
}

/// Wrap a codec decode so any error demotes to
/// `AnimatedStreamStatus::Unsupported(codec)`. Used for animated-stream
/// dispatch where a Reach blob's coincidental codec_byte match
/// shouldn't fail the whole `decode()`.
fn try_animated(
    codec: Codec,
    decode: impl FnOnce() -> Result<AnimationTracks, AnimationError>,
) -> (Option<AnimationTracks>, AnimatedStreamStatus) {
    match decode() {
        Ok(t) => (Some(t), AnimatedStreamStatus::Decoded),
        Err(_) => (None, AnimatedStreamStatus::Unsupported(codec)),
    }
}

/// Shared fullframe decoder used by slots 1 (`frame_count = 1`) and 3
/// (`frame_count = animation.frame_count`). Generic over per-frame
/// count; the rotation-stride / translation-offset / scale-offset
/// fields in the header drive the per-node-outermost layout.
///
/// `quat_8byte = true` → 4× i16 quaternion (8 bytes). The slot-2 raw
/// `real_quaternion` variant (slot 8 BlendScreen also) is a future
/// path with `quat_8byte = false` (4× f32, 16 bytes per quat).
fn decode_fullframe(
    blob: &[u8],
    codec: Codec,
    frame_count: u16,
    quat_8byte: bool,
) -> Result<AnimationTracks, AnimationError> {
    let header = FullframeCodecHeader::from_bytes(blob)
        .ok_or(AnimationError::TruncatedHeader {
            codec, want: FullframeCodecHeader::SIZE, have: blob.len(),
        })?;

    let n_rot = header.base.total_rotated_nodes as usize;
    let n_trans = header.base.total_translated_nodes as usize;
    let n_scale = header.base.total_scaled_nodes as usize;
    let frames = frame_count as usize;
    let quat_size = if quat_8byte { 8 } else { 16 };

    // Per-node stride. The fields at header bytes 20/24/28 carry
    // `elem_size × frame_count` for animated codecs (Uncompressed-
    // Animated, EightByteQuantizedRotationOnly, BlendScreen). For
    // the *static* codec they're left zero in MCC-authored data —
    // both TagTool's `UncompressedStaticDataCodec.Read` and
    // Foundry's `animation_resource.py` ignore them and read
    // sequentially. We mirror that: when the field is zero, fall
    // back to `elem_size × frame_count`, which gives the correct
    // sequential stride for static (frame_count=1) and matches the
    // animated-codec value when the field is populated.
    let stride_or = |stored: u32, elem_size: usize| -> usize {
        if stored == 0 { elem_size * frames } else { stored as usize }
    };
    let rot_stride = stride_or(header.rotation_stride, quat_size);
    let trans_stride = stride_or(header.translation_stride, 12);
    let scale_stride = stride_or(header.scale_stride, 4);

    let rot_start = FullframeCodecHeader::SIZE;
    let rot_end = rot_start
        .checked_add(n_rot.checked_mul(rot_stride).unwrap_or(usize::MAX))
        .ok_or(AnimationError::TruncatedPayload { codec, want_end: usize::MAX, blob_size: blob.len() })?;
    if rot_end > blob.len() {
        return Err(AnimationError::TruncatedPayload { codec, want_end: rot_end, blob_size: blob.len() });
    }

    let trans_start = header.base.translation_offset as usize;
    let trans_end = trans_start
        .checked_add(n_trans.checked_mul(trans_stride).unwrap_or(usize::MAX))
        .ok_or(AnimationError::TruncatedPayload { codec, want_end: usize::MAX, blob_size: blob.len() })?;
    if trans_end > blob.len() {
        return Err(AnimationError::TruncatedPayload { codec, want_end: trans_end, blob_size: blob.len() });
    }

    let scale_start = header.base.scale_offset as usize;
    let scale_end = scale_start
        .checked_add(n_scale.checked_mul(scale_stride).unwrap_or(usize::MAX))
        .ok_or(AnimationError::TruncatedPayload { codec, want_end: usize::MAX, blob_size: blob.len() })?;
    if scale_end > blob.len() {
        return Err(AnimationError::TruncatedPayload { codec, want_end: scale_end, blob_size: blob.len() });
    }

    let mut rotations = Vec::with_capacity(n_rot);
    for node in 0..n_rot {
        let mut frames_vec = Vec::with_capacity(frames);
        for f in 0..frames {
            let off = rot_start + node * rot_stride + f * quat_size;
            let q = if quat_8byte {
                RealQuaternion {
                    i: i16_to_unit(blob, off),
                    j: i16_to_unit(blob, off + 2),
                    k: i16_to_unit(blob, off + 4),
                    w: i16_to_unit(blob, off + 6),
                }
            } else {
                RealQuaternion {
                    i: f32_at(blob, off),
                    j: f32_at(blob, off + 4),
                    k: f32_at(blob, off + 8),
                    w: f32_at(blob, off + 12),
                }
            };
            frames_vec.push(q.normalized());
        }
        rotations.push(frames_vec);
    }

    let mut translations = Vec::with_capacity(n_trans);
    for node in 0..n_trans {
        let mut frames_vec = Vec::with_capacity(frames);
        for f in 0..frames {
            let off = trans_start + node * trans_stride + f * 12;
            frames_vec.push(RealPoint3d {
                x: f32_at(blob, off),
                y: f32_at(blob, off + 4),
                z: f32_at(blob, off + 8),
            });
        }
        translations.push(frames_vec);
    }

    let mut scales = Vec::with_capacity(n_scale);
    for node in 0..n_scale {
        let mut frames_vec = Vec::with_capacity(frames);
        for f in 0..frames {
            let off = scale_start + node * scale_stride + f * 4;
            frames_vec.push(f32_at(blob, off));
        }
        scales.push(frames_vec);
    }

    Ok(AnimationTracks {
        codec,
        frame_count,
        rotations,
        translations,
        scales,
    })
}


/// Slots 4 / 5 / 6 / 7 — `c_keyframe_codec_template`.
///
/// Layout, after the 48-byte [`KeyframeCodecHeader`]:
/// - Per-node `packed_data` u32 array, in order: rotation nodes,
///   translation nodes, scale nodes. Each entry encodes
///   `(time_offset << 12) | count` — `time_offset` is the index into
///   that component's time/payload tables where this node's keys
///   start; `count` is how many keys this node has.
/// - Time table for each component starts at the matching
///   `*_key_time_offset` from the header. Entries are u8 (slots 4/6)
///   or u16 (slots 5/7).
/// - Payload table for each component starts at the matching
///   `*_key_payload_offset` from the header. Entries are sizeof(elem):
///   8 bytes for the 4×i16 quaternion, 12 for `real_point3d`, 4 for
///   `float`.
///
/// Forward (slots 4/5) and reverse (slots 6/7) keyfinder variants
/// produce **bit-identical** byte layouts — the same decoder reads
/// both. (TagTool's `.Reverse()` workaround in
/// `ReverseKeyframeLightlyQuantizedCodec.Read` is a bug stemming from
/// not parsing `packed_data` properly; we don't replicate it.)
///
/// Output is densely interpolated to `frame_count` per node — short-arc
/// nlerp for quaternions, linear for translations and scales. Single-
/// key nodes hold the value across all frames; out-of-bracket frame
/// indices clamp to the first/last key.
fn decode_keyframe(
    blob: &[u8],
    codec: Codec,
    frame_count: u16,
    time_byte_size: usize,
) -> Result<AnimationTracks, AnimationError> {
    let header = KeyframeCodecHeader::from_bytes(blob)
        .ok_or(AnimationError::TruncatedHeader {
            codec, want: KeyframeCodecHeader::SIZE, have: blob.len(),
        })?;

    let n_rot = header.base.total_rotated_nodes as usize;
    let n_trans = header.base.total_translated_nodes as usize;
    let n_scale = header.base.total_scaled_nodes as usize;

    // Per-node packed_data array starts at offset 48 (right after
    // header). Order: rotation nodes, translation nodes, scale nodes.
    let packed_start = KeyframeCodecHeader::SIZE;
    let packed_total = n_rot + n_trans + n_scale;
    let packed_end = packed_start
        .checked_add(packed_total.checked_mul(4).unwrap_or(usize::MAX))
        .ok_or(AnimationError::TruncatedPayload { codec, want_end: usize::MAX, blob_size: blob.len() })?;
    if packed_end > blob.len() {
        return Err(AnimationError::TruncatedPayload { codec, want_end: packed_end, blob_size: blob.len() });
    }

    let read_packed = |idx: usize| -> (u32, u32) {
        let off = packed_start + idx * 4;
        let pd = u32::from_le_bytes(blob[off..off + 4].try_into().unwrap());
        (pd >> 12, pd & 0xFFF) // (time_offset, key_count)
    };

    // Decode one component (rotations, translations, or scales) for
    // all of its nodes. Generic over element type via closures so
    // we don't repeat the per-node bracket-finding loop three times.
    fn decode_component<T, F>(
        blob: &[u8],
        codec: Codec,
        frame_count: u16,
        time_byte_size: usize,
        time_table_start: usize,
        payload_table_start: usize,
        element_size: usize,
        node_packs: impl Iterator<Item = (u32, u32)>,
        identity: T,
        read_element: impl Fn(&[u8], usize) -> T,
        interpolate: F,
    ) -> Result<Vec<Vec<T>>, AnimationError>
    where
        T: Clone,
        F: Fn(&T, &T, f32) -> T,
    {
        let mut out = Vec::new();
        for (time_off, key_count) in node_packs {
            let key_count = key_count as usize;
            let frames_count = frame_count as usize;
            if key_count == 0 {
                out.push(vec![identity.clone(); frames_count]);
                continue;
            }
            let time_start = time_table_start + (time_off as usize) * time_byte_size;
            let time_end = time_start + key_count * time_byte_size;
            let payload_start = payload_table_start + (time_off as usize) * element_size;
            let payload_end = payload_start + key_count * element_size;
            if time_end > blob.len() || payload_end > blob.len() {
                return Err(AnimationError::TruncatedPayload {
                    codec, want_end: time_end.max(payload_end), blob_size: blob.len(),
                });
            }
            let read_time = |i: usize| -> u32 {
                let off = time_start + i * time_byte_size;
                match time_byte_size {
                    1 => blob[off] as u32,
                    2 => u16::from_le_bytes([blob[off], blob[off + 1]]) as u32,
                    _ => unreachable!("time_byte_size must be 1 or 2"),
                }
            };
            let read_value = |key_idx: usize| -> T {
                let off = payload_start + key_idx * element_size;
                read_element(blob, off)
            };

            let mut frames = Vec::with_capacity(frames_count);
            if key_count == 1 {
                let v = read_value(0);
                for _ in 0..frames_count { frames.push(v.clone()); }
                out.push(frames);
                continue;
            }

            // Bracket finder: largest i in [0, key_count) such that
            // time_table[i] <= frame_idx. Linear scan is fine — keys
            // per node are typically a handful, rarely more than ~30.
            for frame_idx in 0..frames_count as u32 {
                let mut bracket = 0usize;
                for i in 0..key_count {
                    if read_time(i) <= frame_idx { bracket = i; } else { break; }
                }
                if bracket == key_count - 1 {
                    frames.push(read_value(bracket));
                    continue;
                }
                let t_a = read_time(bracket) as f32;
                let t_b = read_time(bracket + 1) as f32;
                let t = if t_b > t_a { (frame_idx as f32 - t_a) / (t_b - t_a) } else { 0.0 };
                let va = read_value(bracket);
                let vb = read_value(bracket + 1);
                frames.push(interpolate(&va, &vb, t));
            }
            out.push(frames);
        }
        Ok(out)
    }

    let rot_packs: Vec<_> = (0..n_rot).map(read_packed).collect();
    let trans_packs: Vec<_> = (n_rot..n_rot + n_trans).map(read_packed).collect();
    let scale_packs: Vec<_> = (n_rot + n_trans..packed_total).map(read_packed).collect();

    let rotations = decode_component(
        blob, codec, frame_count, time_byte_size,
        header.rotation_key_time_offset as usize,
        header.rotation_key_payload_offset as usize,
        /*element_size=*/8,
        rot_packs.into_iter(),
        RealQuaternion::IDENTITY,
        |b, off| RealQuaternion {
            i: i16_to_unit(b, off),
            j: i16_to_unit(b, off + 2),
            k: i16_to_unit(b, off + 4),
            w: i16_to_unit(b, off + 6),
        }.normalized(),
        nlerp_short_arc,
    )?;
    let translations = decode_component(
        blob, codec, frame_count, time_byte_size,
        header.translation_key_time_offset as usize,
        header.translation_key_payload_offset as usize,
        /*element_size=*/12,
        trans_packs.into_iter(),
        RealPoint3d::default(),
        |b, off| RealPoint3d {
            x: f32_at(b, off), y: f32_at(b, off + 4), z: f32_at(b, off + 8),
        },
        |a, b, t| RealPoint3d {
            x: a.x + (b.x - a.x) * t,
            y: a.y + (b.y - a.y) * t,
            z: a.z + (b.z - a.z) * t,
        },
    )?;
    let scales = decode_component(
        blob, codec, frame_count, time_byte_size,
        header.scale_key_time_offset as usize,
        header.scale_key_payload_offset as usize,
        /*element_size=*/4,
        scale_packs.into_iter(),
        1.0f32,
        |b, off| f32_at(b, off),
        |a, b, t| a + (b - a) * t,
    )?;

    Ok(AnimationTracks { codec, frame_count, rotations, translations, scales })
}


/// Tiny seekable byte cursor — lets the curve decoder mirror Foundry's
/// position-based read pattern (read forward, occasionally `skip(-6)`
/// to back up so the next keyframe's `p1` reads where the previous
/// keyframe's `p2` was).
struct Cursor<'a> { data: &'a [u8], pos: usize }
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }
    fn seek(&mut self, off: usize) -> Result<(), AnimationError> {
        if off > self.data.len() {
            return Err(AnimationError::TruncatedPayload {
                codec: Codec::Curve, want_end: off, blob_size: self.data.len(),
            });
        }
        self.pos = off; Ok(())
    }
    fn skip(&mut self, delta: i32) {
        if delta >= 0 { self.pos = self.pos.saturating_add(delta as usize); }
        else { self.pos = self.pos.saturating_sub((-delta) as usize); }
    }
    fn read_u8(&mut self) -> Result<u8, AnimationError> {
        let v = *self.data.get(self.pos).ok_or(AnimationError::TruncatedPayload {
            codec: Codec::Curve, want_end: self.pos + 1, blob_size: self.data.len(),
        })?;
        self.pos += 1; Ok(v)
    }
    fn read_u16(&mut self) -> Result<u16, AnimationError> {
        let bs = self.data.get(self.pos..self.pos + 2).ok_or(AnimationError::TruncatedPayload {
            codec: Codec::Curve, want_end: self.pos + 2, blob_size: self.data.len(),
        })?;
        let v = u16::from_le_bytes([bs[0], bs[1]]);
        self.pos += 2; Ok(v)
    }
    fn read_s16(&mut self) -> Result<i16, AnimationError> {
        Ok(self.read_u16()? as i16)
    }
    fn read_u32(&mut self) -> Result<u32, AnimationError> {
        let bs = self.data.get(self.pos..self.pos + 4).ok_or(AnimationError::TruncatedPayload {
            codec: Codec::Curve, want_end: self.pos + 4, blob_size: self.data.len(),
        })?;
        let v = u32::from_le_bytes([bs[0], bs[1], bs[2], bs[3]]);
        self.pos += 4; Ok(v)
    }
    fn read_f32(&mut self) -> Result<f32, AnimationError> {
        Ok(f32::from_bits(self.read_u32()?))
    }
}

/// Slot 9 — Curve codec. Per-component (rotation/translation/scale),
/// each node has a packed payload starting at
/// `payload_data_offset + node_offset` (where `node_offset` is read
/// from a per-node u32 array right after the codec header).
///
/// Per-node payload header:
/// - u16 (unused), u16 key_count, u8 flags, u8 (unused), s16 (unused)
/// - For translation: + 4 f32 (offset_x/y/z, scale)
/// - For scale:       + 2 f32 (offset, scale)
///
/// If `flags & 1` is set, each frame stores a direct value. Otherwise,
/// `key_count` u8 deltas (cumulative-sum into keyframe indices) are
/// followed by per-frame curve segments. A keyframe segment is:
/// `p1 (i16s), tangent_bytes (4×u8 for quat / 3×u8 for vec / 1×u8 for
/// scalar), p2 (i16s)` — then the cursor backs up by `2 × element_size`
/// so `p2` becomes the next segment's `p1`. Frames between keyframes
/// are produced by cubic Hermite using the tangent bytes.
///
/// Quaternion decompression: input has 3 i16 values (i, j, w);
/// the missing component k is reconstructed via
/// `k = sqrt(max(1 - i² - j², 0))`, sign-flipped if `w < 0`, then
/// `w := 2|w| - 1` and all components scale by `sqrt(max(1 - w², 0))`
/// before final normalization. Mirrors Foundry's `_decompress_quat`.
fn decode_curve(
    blob: &[u8],
    codec: Codec,
    frame_count: u16,
    revised: bool,
) -> Result<AnimationTracks, AnimationError> {
    let mut c = Cursor::new(blob);
    if blob.len() < 32 {
        return Err(AnimationError::TruncatedHeader { codec, want: 32, have: blob.len() });
    }
    // 12-byte base header (we already validated codec_byte before
    // dispatching here — just consume).
    c.skip(12);
    let translation_data_offset = c.read_u32()? as usize;
    let scale_data_offset = c.read_u32()? as usize;
    let payload_data_offset = c.read_u32()? as usize;
    let total_compressed_size = c.read_u32()? as usize;
    c.read_u32()?; // reserved/unused

    let n_rot = blob[1] as usize;
    let n_trans = blob[2] as usize;
    let n_scale = blob[3] as usize;
    let frames = frame_count as usize;

    // Per-rotation-node u32 offsets array sits right after the 32-byte
    // header (we're at position 32 now after reading the 5 u32 + skip 12).
    let mut rotation_offsets = Vec::with_capacity(n_rot);
    for _ in 0..n_rot { rotation_offsets.push(c.read_u32()? as usize); }

    let mut rotations = Vec::with_capacity(n_rot);
    for &node_off in &rotation_offsets {
        c.seek(payload_data_offset + node_off)?;
        rotations.push(read_curve_rotation_node(&mut c, frames, revised)?);
    }

    let mut translations = Vec::with_capacity(n_trans);
    if n_trans > 0 {
        c.seek(payload_data_offset + translation_data_offset)?;
        let mut trans_offsets = Vec::with_capacity(n_trans);
        for _ in 0..n_trans { trans_offsets.push(c.read_u32()? as usize); }
        for &node_off in &trans_offsets {
            c.seek(payload_data_offset + node_off)?;
            translations.push(read_curve_translation_node(&mut c, frames)?);
        }
    }

    let mut scales = Vec::with_capacity(n_scale);
    if n_scale > 0 {
        c.seek(payload_data_offset + scale_data_offset)?;
        let mut scale_offsets = Vec::with_capacity(n_scale);
        for _ in 0..n_scale { scale_offsets.push(c.read_u32()? as usize); }
        for &node_off in &scale_offsets {
            c.seek(payload_data_offset + node_off)?;
            scales.push(read_curve_scale_node(&mut c, frames)?);
        }
    }

    // Position is now wherever the last per-node read left it; the
    // Reach get_data_offset model uses the explicit cumulative-sum
    // size from `data sizes`, so we don't need to advance to
    // total_compressed_size — but skip if needed for correctness.
    let _ = total_compressed_size;

    Ok(AnimationTracks { codec, frame_count, rotations, translations, scales })
}

fn read_curve_rotation_node(c: &mut Cursor<'_>, frames: usize, revised: bool) -> Result<Vec<RealQuaternion>, AnimationError> {
    c.read_u16()?; // unused
    let key_count = c.read_u16()? as usize;
    let flags = c.read_u8()?;
    c.read_u8()?; // unused
    c.read_s16()?; // unused
    let keyframes = if flags & 1 == 0 { read_curve_keyframe_deltas(c, key_count)? } else { Vec::new() };

    let read_quat = |c: &mut Cursor<'_>| -> Result<RealQuaternion, AnimationError> {
        let v3 = c.read_s16()?;
        let v4 = c.read_s16()?;
        let v5 = c.read_s16()?;
        Ok(if revised {
            decompress_revised_quat(v3, v4, v5)
        } else {
            decompress_curve_quat(
                v3 as f32 / i16::MAX as f32,
                v4 as f32 / i16::MAX as f32,
                v5 as f32 / i16::MAX as f32,
            )
        })
    };

    let mut out = Vec::with_capacity(frames);
    let mut p1 = RealQuaternion::IDENTITY;
    let mut p2 = RealQuaternion::IDENTITY;
    let mut tangent_bytes = [0u8; 4];
    let mut current_kf = 0u32;
    let mut next_kf = 0u32;
    let mut keyframe_index = 0usize;
    for frame_index in 0..frames as u32 {
        let q = if flags & 1 != 0 {
            read_quat(c)?
        } else {
            if keyframe_index < keyframes.len()
                && keyframes[keyframe_index] == frame_index
                && frame_index < frames as u32 - 1
            {
                p1 = read_quat(c)?;
                tangent_bytes = [c.read_u8()?, c.read_u8()?, c.read_u8()?, c.read_u8()?];
                p2 = read_quat(c)?;
                current_kf = keyframes[keyframe_index];
                next_kf = keyframes.get(keyframe_index + 1).copied().unwrap_or(current_kf + 1);
                keyframe_index += 1;
                c.skip(-6); // p2 becomes next segment's p1
            }
            let span = (next_kf.saturating_sub(current_kf) as f32).max(1.0);
            let t = (frame_index.saturating_sub(current_kf) as f32) / span;
            let tan1 = curve_tangent_quat(
                ((tangent_bytes[0] >> 4) as i32) - 7,
                ((tangent_bytes[1] >> 4) as i32) - 7,
                ((tangent_bytes[2] >> 4) as i32) - 7,
                ((tangent_bytes[3] >> 4) as i32) - 7,
                p1, p2,
            );
            let tan2 = curve_tangent_quat(
                ((tangent_bytes[0] & 0x0F) as i32) - 7,
                ((tangent_bytes[1] & 0x0F) as i32) - 7,
                ((tangent_bytes[2] & 0x0F) as i32) - 7,
                ((tangent_bytes[3] & 0x0F) as i32) - 7,
                p1, p2,
            );
            curve_position_quat(t, tan1, tan2, p1, p2)
        };
        out.push(q);
    }
    Ok(out)
}

fn read_curve_translation_node(c: &mut Cursor<'_>, frames: usize) -> Result<Vec<RealPoint3d>, AnimationError> {
    c.read_u16()?; // unused
    let key_count = c.read_u16()? as usize;
    let flags = c.read_u8()?;
    c.read_u8()?; // unused
    c.read_u16()?; // unused
    let offset_x = c.read_f32()?;
    let offset_y = c.read_f32()?;
    let offset_z = c.read_f32()?;
    let scale = c.read_f32()?;
    let keyframes = if flags & 1 == 0 { read_curve_keyframe_deltas(c, key_count)? } else { Vec::new() };

    let mut out = Vec::with_capacity(frames);
    let mut p1 = RealPoint3d::default();
    let mut p2 = RealPoint3d::default();
    let mut tangent_bytes = [0u8; 3];
    let mut current_kf = 0u32;
    let mut next_kf = 0u32;
    let mut keyframe_index = 0usize;
    for frame_index in 0..frames as u32 {
        let v = if flags & 1 != 0 {
            RealPoint3d {
                x: c.read_s16()? as f32 / i16::MAX as f32,
                y: c.read_s16()? as f32 / i16::MAX as f32,
                z: c.read_s16()? as f32 / i16::MAX as f32,
            }
        } else {
            if keyframe_index < keyframes.len()
                && keyframes[keyframe_index] == frame_index
                && frame_index < frames as u32 - 1
            {
                let x1 = c.read_s16()? as f32 / i16::MAX as f32;
                let y1 = c.read_s16()? as f32 / i16::MAX as f32;
                let z1 = c.read_s16()? as f32 / i16::MAX as f32;
                tangent_bytes = [c.read_u8()?, c.read_u8()?, c.read_u8()?];
                let x2 = c.read_s16()? as f32 / i16::MAX as f32;
                let y2 = c.read_s16()? as f32 / i16::MAX as f32;
                let z2 = c.read_s16()? as f32 / i16::MAX as f32;
                p1 = RealPoint3d { x: x1, y: y1, z: z1 };
                p2 = RealPoint3d { x: x2, y: y2, z: z2 };
                current_kf = keyframes[keyframe_index];
                next_kf = keyframes.get(keyframe_index + 1).copied().unwrap_or(current_kf + 1);
                keyframe_index += 1;
                c.skip(-6);
            }
            let span = (next_kf.saturating_sub(current_kf) as f32).max(1.0);
            let t = (frame_index.saturating_sub(current_kf) as f32) / span;
            let tan1 = curve_tangent_vec(
                ((tangent_bytes[0] >> 4) as i32) - 7,
                ((tangent_bytes[1] >> 4) as i32) - 7,
                ((tangent_bytes[2] >> 4) as i32) - 7,
                p1, p2,
            );
            let tan2 = curve_tangent_vec(
                ((tangent_bytes[0] & 0x0F) as i32) - 7,
                ((tangent_bytes[1] & 0x0F) as i32) - 7,
                ((tangent_bytes[2] & 0x0F) as i32) - 7,
                p1, p2,
            );
            curve_position_vec(t, tan1, tan2, p1, p2)
        };
        out.push(RealPoint3d {
            x: scale * v.x + offset_x,
            y: scale * v.y + offset_y,
            z: scale * v.z + offset_z,
        });
    }
    Ok(out)
}

fn read_curve_scale_node(c: &mut Cursor<'_>, frames: usize) -> Result<Vec<f32>, AnimationError> {
    c.read_u16()?;
    let key_count = c.read_u16()? as usize;
    let flags = c.read_u8()?;
    c.read_u8()?;
    c.read_u16()?;
    let offset = c.read_f32()?;
    let scale = c.read_f32()?;
    let keyframes = if flags & 1 == 0 { read_curve_keyframe_deltas(c, key_count)? } else { Vec::new() };

    let mut out = Vec::with_capacity(frames);
    let mut p1 = 0.0f32;
    let mut p2 = 0.0f32;
    let mut tangent_byte = 0u8;
    let mut current_kf = 0u32;
    let mut next_kf = 0u32;
    let mut keyframe_index = 0usize;
    for frame_index in 0..frames as u32 {
        let v = if flags & 1 != 0 {
            c.read_s16()? as f32 / i16::MAX as f32
        } else {
            if keyframe_index < keyframes.len()
                && keyframes[keyframe_index] == frame_index
                && frame_index < frames as u32 - 1
            {
                p1 = c.read_s16()? as f32 / i16::MAX as f32;
                tangent_byte = c.read_u8()?;
                p2 = c.read_s16()? as f32 / i16::MAX as f32;
                current_kf = keyframes[keyframe_index];
                next_kf = keyframes.get(keyframe_index + 1).copied().unwrap_or(current_kf + 1);
                keyframe_index += 1;
                c.skip(-2);
            }
            let span = (next_kf.saturating_sub(current_kf) as f32).max(1.0);
            let t = (frame_index.saturating_sub(current_kf) as f32) / span;
            let tan1 = curve_tangent_scalar(((tangent_byte >> 4) as i32) - 7, p1, p2);
            let tan2 = curve_tangent_scalar(((tangent_byte & 0x0F) as i32) - 7, p1, p2);
            curve_position_scalar(t, tan1, tan2, p1, p2)
        };
        out.push(v * scale + offset);
    }
    Ok(out)
}

/// Read `key_count` u8 keyframe deltas, prepended by an implicit 0,
/// cumulative-summed into absolute frame indices. Mirrors Foundry's
/// `_read_curve_keyframe_data`.
fn read_curve_keyframe_deltas(c: &mut Cursor<'_>, key_count: usize) -> Result<Vec<u32>, AnimationError> {
    let mut keyframes = Vec::with_capacity(key_count + 1);
    keyframes.push(0u32);
    let mut total = 0u32;
    for _ in 0..key_count {
        total = total.saturating_add(c.read_u8()? as u32);
        keyframes.push(total);
    }
    Ok(keyframes)
}

fn decompress_curve_quat(i: f32, j: f32, w: f32) -> RealQuaternion {
    let mut k = (1.0 - i * i - j * j).max(0.0).sqrt();
    if w < 0.0 { k = -k; }
    let w_unfolded = w.abs() * 2.0 - 1.0;
    let scale = (1.0 - w_unfolded * w_unfolded).max(0.0).sqrt();
    RealQuaternion { i: i * scale, j: j * scale, k: k * scale, w: w_unfolded }.normalized()
}

/// Slot 10 (RevisedCurve, H4-era) quaternion decompression. Stores 3
/// of 4 components as i16, with the low bit of each value stealing
/// metadata: bit 0 of `v3` flips the sign of the reconstructed
/// component; bits 0 of `v4` (×2) and `v5` together encode which of
/// the four output slots holds the reconstructed (largest-magnitude)
/// component. Components are scaled by `sqrt(0.5)` because the
/// largest-magnitude component is at most that value (when the other
/// three are equal in unit-length quaternions).
///
/// Implementation matches Foundry's `_decompress_revised_quat`
/// (`animation_resource.py:747`) using the "cache" rotation_layout
/// which is what MCC re-imports use. The "h4_source" layout is for
/// raw H4 source jmads (uncompiled .source variant) and isn't seen
/// in the MCC corpus.
fn decompress_revised_quat(v3: i16, v4: i16, v5: i16) -> RealQuaternion {
    const SQRT_HALF: f32 = 0.707_106_77;
    // Strip the low metadata bit from each value, preserving sign.
    let i = ((v3 & !1i16) as f32 / i16::MAX as f32) * SQRT_HALF;
    let j = ((v4 & !1i16) as f32 / i16::MAX as f32) * SQRT_HALF;
    let k = ((v5 & !1i16) as f32 / i16::MAX as f32) * SQRT_HALF;
    let mut missing = (1.0 - i * i - j * j - k * k).max(0.0).sqrt();
    if v3 & 1 != 0 { missing = -missing; }
    let component_index = ((v5 & 1) as usize) | ((2 * (v4 & 1)) as usize);
    // Cache layout: place i/j/k at offsets +1 / -2 / -1 from the
    // missing-component slot, mod 4. Indices are bounded to 0..=3.
    let mut output = [0.0f32; 4];
    output[(component_index + 1) & 3] = i;
    output[(component_index + 2) & 3] = j; // (-2 mod 4) == +2
    output[(component_index + 3) & 3] = k; // (-1 mod 4) == +3
    output[component_index] = missing;
    RealQuaternion { i: output[0], j: output[1], k: output[2], w: output[3] }.normalized()
}

/// Curve tangent for a single component. `tangent_signed` is the
/// nibble's signed value (0..=15 → -7..=8). Result is the Hermite
/// tangent used in `curve_position_scalar`.
fn curve_tangent_scalar(tangent_signed: i32, p1: f32, p2: f32) -> f32 {
    let t = tangent_signed as f32 / 7.0;
    t.abs() * (t * 0.300_000_011_920_929) + (p2 - p1)
}

fn curve_tangent_quat(it: i32, jt: i32, kt: i32, wt: i32, p1: RealQuaternion, p2: RealQuaternion) -> RealQuaternion {
    RealQuaternion {
        i: curve_tangent_scalar(it, p1.i, p2.i),
        j: curve_tangent_scalar(jt, p1.j, p2.j),
        k: curve_tangent_scalar(kt, p1.k, p2.k),
        w: curve_tangent_scalar(wt, p1.w, p2.w),
    }
}

fn curve_tangent_vec(xt: i32, yt: i32, zt: i32, p1: RealPoint3d, p2: RealPoint3d) -> RealVector3d {
    RealVector3d {
        i: curve_tangent_scalar(xt, p1.x, p2.x),
        j: curve_tangent_scalar(yt, p1.y, p2.y),
        k: curve_tangent_scalar(zt, p1.z, p2.z),
    }
}

/// Cubic Hermite curve evaluation at `time` ∈ [0, 1].
fn curve_position_scalar(t: f32, tan1: f32, tan2: f32, p1: f32, p2: f32) -> f32 {
    let t2 = t * t;
    let t3 = t2 * t;
    let h1 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h2 = t3 - 2.0 * t2 + t;
    let h3 = 3.0 * t2 - 2.0 * t3;
    let h4 = t3 - t2;
    h1 * p1 + h2 * tan1 + h3 * p2 + h4 * tan2
}

fn curve_position_quat(t: f32, tan1: RealQuaternion, tan2: RealQuaternion, p1: RealQuaternion, p2: RealQuaternion) -> RealQuaternion {
    RealQuaternion {
        i: curve_position_scalar(t, tan1.i, tan2.i, p1.i, p2.i),
        j: curve_position_scalar(t, tan1.j, tan2.j, p1.j, p2.j),
        k: curve_position_scalar(t, tan1.k, tan2.k, p1.k, p2.k),
        w: curve_position_scalar(t, tan1.w, tan2.w, p1.w, p2.w),
    }
    .normalized()
}

fn curve_position_vec(t: f32, tan1: RealVector3d, tan2: RealVector3d, p1: RealPoint3d, p2: RealPoint3d) -> RealPoint3d {
    RealPoint3d {
        x: curve_position_scalar(t, tan1.i, tan2.i, p1.x, p2.x),
        y: curve_position_scalar(t, tan1.j, tan2.j, p1.y, p2.y),
        z: curve_position_scalar(t, tan1.k, tan2.k, p1.z, p2.z),
    }
}

/// Short-arc normalized lerp for unit quaternions. Thin wrapper over
/// [`RealQuaternion::nlerp`] kept here because `decode_component` takes
/// the interpolator as an `fn(&Q, &Q, f32) -> Q` pointer (so it can
/// pass-through a `(p1, p2, _) -> p1` for non-quat tracks); the math
/// type's inherent method has a `(self, Self, f32)` shape that doesn't
/// match. Mirrors the H3 binary's
/// `fast_short_arc_quaternion_interpolate_and_normalize`.
fn nlerp_short_arc(a: &RealQuaternion, b: &RealQuaternion, t: f32) -> RealQuaternion {
    a.nlerp(*b, t)
}

/// Decode an int16 component as `s / 32767.0`. Matches the H3 binary's
/// `c_quantized_quaternion_8byte::decompress` (constant 0x38000100,
/// approximately 1/32767).
fn i16_to_unit(blob: &[u8], off: usize) -> f32 {
    let raw = i16::from_le_bytes([blob[off], blob[off + 1]]);
    raw as f32 / i16::MAX as f32
}

fn f32_at(blob: &[u8], off: usize) -> f32 {
    f32::from_le_bytes(blob[off..off + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a synthetic uncompressed_static blob with
    /// (n_rot, n_trans, n_scale) nodes, tightly packed.
    fn build_static(n_rot: u8, n_trans: u8, n_scale: u8) -> Vec<u8> {
        let rot_start = FullframeCodecHeader::SIZE;
        let trans_start = rot_start + (n_rot as usize) * 8;
        let scale_start = trans_start + (n_trans as usize) * 12;
        let total = scale_start + (n_scale as usize) * 4;

        let mut out = vec![0u8; total];
        out[0] = Codec::UncompressedStatic as u8;
        out[1] = n_rot;
        out[2] = n_trans;
        out[3] = n_scale;
        // error_value (4..8), compression_rate (8..12) left as 0.
        out[12..16].copy_from_slice(&(trans_start as u32).to_le_bytes());
        out[16..20].copy_from_slice(&(scale_start as u32).to_le_bytes());
        // strides
        out[20..24].copy_from_slice(&8u32.to_le_bytes());
        out[24..28].copy_from_slice(&12u32.to_le_bytes());
        out[28..32].copy_from_slice(&4u32.to_le_bytes());
        out
    }

    /// Build a synthetic slot-3 (8byte_quantized_rotation_only) blob
    /// with per-frame strides. Same header shape as static but with
    /// `frame_count` frames per node.
    fn build_animated_8byte(n_rot: u8, n_trans: u8, n_scale: u8, frame_count: u16) -> Vec<u8> {
        let f = frame_count as usize;
        let rot_start = FullframeCodecHeader::SIZE;
        let trans_start = rot_start + (n_rot as usize) * 8 * f;
        let scale_start = trans_start + (n_trans as usize) * 12 * f;
        let total = scale_start + (n_scale as usize) * 4 * f;

        let mut out = vec![0u8; total];
        out[0] = Codec::EightByteQuantizedRotationOnly as u8;
        out[1] = n_rot;
        out[2] = n_trans;
        out[3] = n_scale;
        out[12..16].copy_from_slice(&(trans_start as u32).to_le_bytes());
        out[16..20].copy_from_slice(&(scale_start as u32).to_le_bytes());
        out[20..24].copy_from_slice(&((8 * f) as u32).to_le_bytes());
        out[24..28].copy_from_slice(&((12 * f) as u32).to_le_bytes());
        out[28..32].copy_from_slice(&((4 * f) as u32).to_le_bytes());
        out
    }

    #[test]
    fn empty_animation_decodes() {
        let blob = build_static(0, 0, 0);
        let tracks = decode_uncompressed_static(&blob).unwrap();
        assert_eq!(tracks.frame_count, 1);
        assert!(tracks.rotations.is_empty());
        assert!(tracks.translations.is_empty());
        assert!(tracks.scales.is_empty());
    }

    #[test]
    fn one_node_identity_quaternion() {
        let mut blob = build_static(1, 0, 0);
        // Identity quat: (0, 0, 0, 1) = i16 (0, 0, 0, 32767).
        blob[32..34].copy_from_slice(&0i16.to_le_bytes());
        blob[34..36].copy_from_slice(&0i16.to_le_bytes());
        blob[36..38].copy_from_slice(&0i16.to_le_bytes());
        blob[38..40].copy_from_slice(&i16::MAX.to_le_bytes());
        let tracks = decode_uncompressed_static(&blob).unwrap();
        let q = tracks.rotations[0][0];
        assert!((q.i.abs()) < 1e-6);
        assert!((q.j.abs()) < 1e-6);
        assert!((q.k.abs()) < 1e-6);
        assert!((q.w - 1.0).abs() < 1e-6);
    }

    /// Regression: in MCC-authored static codecs, the stride fields
    /// at header bytes 20/24/28 are left zero — TagTool/Foundry both
    /// ignore them and read sequential elements. Before the fix our
    /// decoder used the stored stride (=0) and read offset 32 for
    /// every node, returning identity for all rotations. This test
    /// builds a static blob with stride=0 and expects each bone to
    /// receive its own quaternion.
    #[test]
    fn static_stride_zero_falls_back_to_elem_size() {
        let mut blob = build_static(3, 0, 0);
        // Wipe the stride fields the on-disk way.
        blob[20..32].fill(0);
        // Three distinct quaternions: bone 0 = (0,0,0,1), bone 1 = (1,0,0,0), bone 2 = (0,1,0,0).
        // Encoded as i16/32767 → 0 / 32767 / 0.
        let mk = |i: i16, j: i16, k: i16, w: i16| {
            [i.to_le_bytes(), j.to_le_bytes(), k.to_le_bytes(), w.to_le_bytes()].concat()
        };
        blob[32..40].copy_from_slice(&mk(0, 0, 0, i16::MAX));
        blob[40..48].copy_from_slice(&mk(i16::MAX, 0, 0, 0));
        blob[48..56].copy_from_slice(&mk(0, i16::MAX, 0, 0));

        let tracks = decode_uncompressed_static(&blob).unwrap();
        assert_eq!(tracks.rotations.len(), 3);
        let q0 = tracks.rotations[0][0];
        let q1 = tracks.rotations[1][0];
        let q2 = tracks.rotations[2][0];
        assert!(q0.w > 0.99 && q0.i.abs() < 1e-3, "bone 0 should be identity-ish, got {q0:?}");
        assert!(q1.i > 0.99 && q1.w.abs() < 1e-3, "bone 1 should have i≈1, got {q1:?}");
        assert!(q2.j > 0.99 && q2.w.abs() < 1e-3, "bone 2 should have j≈1, got {q2:?}");
    }

    #[test]
    fn translation_uses_header_offset_not_implicit() {
        let mut blob = build_static(2, 1, 0);
        let trans_off = u32::from_le_bytes(blob[12..16].try_into().unwrap()) as usize;
        blob[trans_off..trans_off + 4].copy_from_slice(&1.5f32.to_le_bytes());
        blob[trans_off + 4..trans_off + 8].copy_from_slice(&(-2.0f32).to_le_bytes());
        blob[trans_off + 8..trans_off + 12].copy_from_slice(&3.25f32.to_le_bytes());
        let tracks = decode_uncompressed_static(&blob).unwrap();
        let t = tracks.translations[0][0];
        assert_eq!(t.x, 1.5);
        assert_eq!(t.y, -2.0);
        assert_eq!(t.z, 3.25);
    }

    /// Build a fullframe blob with raw 16-byte real_quaternions
    /// (slots 2 / 8). Same shape as the 8byte builder but with
    /// 16-byte rotation strides.
    fn build_animated_raw_quat(n_rot: u8, frame_count: u16) -> Vec<u8> {
        let f = frame_count as usize;
        let rot_start = FullframeCodecHeader::SIZE;
        let trans_start = rot_start + (n_rot as usize) * 16 * f;
        let scale_start = trans_start;
        let total = scale_start;
        let mut out = vec![0u8; total];
        out[0] = Codec::BlendScreen as u8;
        out[1] = n_rot;
        out[12..16].copy_from_slice(&(trans_start as u32).to_le_bytes());
        out[16..20].copy_from_slice(&(scale_start as u32).to_le_bytes());
        out[20..24].copy_from_slice(&((16 * f) as u32).to_le_bytes());
        out[24..28].copy_from_slice(&0u32.to_le_bytes());
        out[28..32].copy_from_slice(&0u32.to_le_bytes());
        out
    }

    #[test]
    fn animated_raw_quat_per_frame() {
        // 1 rotated node, 2 frames, raw f32 quaternions.
        let mut blob = build_animated_raw_quat(1, 2);
        // Frame 0: identity.
        blob[32..36].copy_from_slice(&0.0f32.to_le_bytes());
        blob[36..40].copy_from_slice(&0.0f32.to_le_bytes());
        blob[40..44].copy_from_slice(&0.0f32.to_le_bytes());
        blob[44..48].copy_from_slice(&1.0f32.to_le_bytes());
        // Frame 1: (0.5, 0.5, 0.5, 0.5) — already unit length.
        blob[48..52].copy_from_slice(&0.5f32.to_le_bytes());
        blob[52..56].copy_from_slice(&0.5f32.to_le_bytes());
        blob[56..60].copy_from_slice(&0.5f32.to_le_bytes());
        blob[60..64].copy_from_slice(&0.5f32.to_le_bytes());
        let tracks = decode_fullframe(&blob, Codec::BlendScreen, 2, /*quat_8byte=*/false).unwrap();
        assert_eq!(tracks.codec, Codec::BlendScreen);
        assert_eq!(tracks.frame_count, 2);
        let f0 = tracks.rotations[0][0];
        assert!((f0.w - 1.0).abs() < 1e-6);
        let f1 = tracks.rotations[0][1];
        assert!((f1.i - 0.5).abs() < 1e-6);
        assert!((f1.j - 0.5).abs() < 1e-6);
        assert!((f1.k - 0.5).abs() < 1e-6);
        assert!((f1.w - 0.5).abs() < 1e-6);
    }

    /// Build a synthetic keyframe blob with the given per-component
    /// node packs. Each `pack = (time_offset, key_count)` from the
    /// caller's perspective. Time entries (u8) and quaternion payloads
    /// are written tightly packed in the order rotation/translation/scale.
    fn build_keyframe_byte(
        rot_packs: &[(u32, u32)],
        rot_keys: &[(u8, [i16; 4])],     // (time, quat_components)
        trans_packs: &[(u32, u32)],
        trans_keys: &[(u8, [f32; 3])],
        scale_packs: &[(u32, u32)],
        scale_keys: &[(u8, f32)],
    ) -> Vec<u8> {
        let n_rot = rot_packs.len() as u8;
        let n_trans = trans_packs.len() as u8;
        let n_scale = scale_packs.len() as u8;
        let packed_count = (n_rot as usize) + (n_trans as usize) + (n_scale as usize);

        // Layout: header (48) | packed_data (packed_count*4)
        //       | rot times (rot_keys.len()*1) | trans times | scale times
        //       | rot payload (rot_keys.len()*8) | trans payload (n*12) | scale payload (n*4)
        let packed_start = 48;
        let rot_time_start = packed_start + packed_count * 4;
        let trans_time_start = rot_time_start + rot_keys.len();
        let scale_time_start = trans_time_start + trans_keys.len();
        let rot_payload_start = scale_time_start + scale_keys.len();
        let trans_payload_start = rot_payload_start + rot_keys.len() * 8;
        let scale_payload_start = trans_payload_start + trans_keys.len() * 12;
        let total = scale_payload_start + scale_keys.len() * 4;

        let mut out = vec![0u8; total];
        out[0] = Codec::ByteKeyframeLightlyQuantized as u8;
        out[1] = n_rot;
        out[2] = n_trans;
        out[3] = n_scale;
        // bytes 4..12 (error_value, compression_rate) left zero.
        // bytes 12..20 (translation_offset, scale_offset) — base
        // header fields not used by the keyframe decoder. Leave zero.
        out[20..24].copy_from_slice(&(rot_time_start as u32).to_le_bytes());
        out[24..28].copy_from_slice(&(trans_time_start as u32).to_le_bytes());
        out[28..32].copy_from_slice(&(scale_time_start as u32).to_le_bytes());
        out[32..36].copy_from_slice(&(rot_payload_start as u32).to_le_bytes());
        out[36..40].copy_from_slice(&(trans_payload_start as u32).to_le_bytes());
        out[40..44].copy_from_slice(&(scale_payload_start as u32).to_le_bytes());

        let mut idx = 0;
        for &(t, c) in rot_packs.iter().chain(trans_packs.iter()).chain(scale_packs.iter()) {
            let pd = (t << 12) | (c & 0xFFF);
            out[packed_start + idx * 4..packed_start + idx * 4 + 4]
                .copy_from_slice(&pd.to_le_bytes());
            idx += 1;
        }

        for (i, (t, _)) in rot_keys.iter().enumerate() { out[rot_time_start + i] = *t; }
        for (i, (t, _)) in trans_keys.iter().enumerate() { out[trans_time_start + i] = *t; }
        for (i, (t, _)) in scale_keys.iter().enumerate() { out[scale_time_start + i] = *t; }

        for (i, (_, q)) in rot_keys.iter().enumerate() {
            let off = rot_payload_start + i * 8;
            out[off..off + 2].copy_from_slice(&q[0].to_le_bytes());
            out[off + 2..off + 4].copy_from_slice(&q[1].to_le_bytes());
            out[off + 4..off + 6].copy_from_slice(&q[2].to_le_bytes());
            out[off + 6..off + 8].copy_from_slice(&q[3].to_le_bytes());
        }
        for (i, (_, p)) in trans_keys.iter().enumerate() {
            let off = trans_payload_start + i * 12;
            out[off..off + 4].copy_from_slice(&p[0].to_le_bytes());
            out[off + 4..off + 8].copy_from_slice(&p[1].to_le_bytes());
            out[off + 8..off + 12].copy_from_slice(&p[2].to_le_bytes());
        }
        for (i, (_, s)) in scale_keys.iter().enumerate() {
            let off = scale_payload_start + i * 4;
            out[off..off + 4].copy_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn keyframe_single_key_constant() {
        // One rotated node with a single key — value held across all
        // frames regardless of frame_count.
        let blob = build_keyframe_byte(
            &[(0, 1)],
            &[(0, [0, 0, 0, i16::MAX])], // identity quat at time 0
            &[], &[], &[], &[],
        );
        let tracks = decode_keyframe(&blob, Codec::ByteKeyframeLightlyQuantized, 5, 1).unwrap();
        assert_eq!(tracks.rotations.len(), 1);
        assert_eq!(tracks.rotations[0].len(), 5);
        for q in &tracks.rotations[0] {
            assert!((q.w - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn keyframe_two_keys_lerp() {
        // Translation node with keys at frames 0 and 4: (0,0,0) → (4,0,0).
        // Frame 2 should land halfway: (2, 0, 0).
        let blob = build_keyframe_byte(
            &[],
            &[],
            &[(0, 2)],
            &[(0, [0.0, 0.0, 0.0]), (4, [4.0, 0.0, 0.0])],
            &[], &[],
        );
        let tracks = decode_keyframe(&blob, Codec::ByteKeyframeLightlyQuantized, 5, 1).unwrap();
        let t = &tracks.translations[0];
        assert!((t[0].x - 0.0).abs() < 1e-6);
        assert!((t[2].x - 2.0).abs() < 1e-6);
        assert!((t[4].x - 4.0).abs() < 1e-6);
    }

    #[test]
    fn keyframe_packed_data_skips_to_correct_node() {
        // Two rotated nodes; node 0 has 1 key starting at time_offset 0,
        // node 1 has 1 key starting at time_offset 1. Verify node 1
        // reads its OWN payload (not node 0's).
        let blob = build_keyframe_byte(
            &[(0, 1), (1, 1)],
            &[
                (0, [0, 0, 0, i16::MAX]),     // node 0: identity
                (0, [i16::MAX, 0, 0, 0]),     // node 1: (1, 0, 0, 0)
            ],
            &[], &[], &[], &[],
        );
        let tracks = decode_keyframe(&blob, Codec::ByteKeyframeLightlyQuantized, 1, 1).unwrap();
        assert!((tracks.rotations[0][0].w - 1.0).abs() < 1e-6);
        assert!((tracks.rotations[1][0].i - 1.0).abs() < 1e-6);
    }

    #[test]
    fn keyframe_clamp_past_last_key() {
        // Single translation node, keys at time 0 and 2. Frame 4 (past
        // last key) should clamp to the last key's value, not extrapolate.
        let blob = build_keyframe_byte(
            &[],
            &[],
            &[(0, 2)],
            &[(0, [0.0, 0.0, 0.0]), (2, [2.0, 0.0, 0.0])],
            &[], &[],
        );
        let tracks = decode_keyframe(&blob, Codec::ByteKeyframeLightlyQuantized, 5, 1).unwrap();
        assert!((tracks.translations[0][3].x - 2.0).abs() < 1e-6);
        assert!((tracks.translations[0][4].x - 2.0).abs() < 1e-6);
    }

    #[test]
    fn revised_quat_decompresses_unit_length() {
        // Encode an identity quaternion with `missing` at slot 3 (w):
        // component_index = 3 means v5_low=1 AND v4_low=1.
        // For identity, i=j=k=0 (encoded zeros), missing=w=1.
        // With component_index=3, the layout maps:
        //   output[(3+1)&3=0] = i = 0
        //   output[(3+2)&3=1] = j = 0
        //   output[(3+3)&3=2] = k = 0
        //   output[3] = missing = 1
        // RealQuaternion fields {i,j,k,w} = {output[0..3]}.
        // v3 = 0, v4 has bit 0 set (=1), v5 has bit 0 set (=1)
        let q = decompress_revised_quat(0, 1, 1);
        assert!(q.i.abs() < 1e-5, "i={}", q.i);
        assert!(q.j.abs() < 1e-5, "j={}", q.j);
        assert!(q.k.abs() < 1e-5, "k={}", q.k);
        assert!((q.w - 1.0).abs() < 1e-5, "w={}", q.w);
    }

    #[test]
    fn revised_quat_sign_bit_negates_missing() {
        // v3 bit 0 set should flip the sign of the reconstructed (missing) component.
        let q_pos = decompress_revised_quat(0, 1, 1);
        let q_neg = decompress_revised_quat(1, 1, 1);
        assert!((q_pos.w - 1.0).abs() < 1e-5);
        assert!((q_neg.w + 1.0).abs() < 1e-5);
    }

    #[test]
    fn nlerp_short_arc_picks_shorter_path() {
        let a = RealQuaternion::IDENTITY;
        // -a should be treated as +a (same orientation, opposite sign).
        let neg_a = RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: -1.0 };
        let mid = nlerp_short_arc(&a, &neg_a, 0.5);
        // After short-arc flip, mid should be ≈ identity (not zero).
        assert!((mid.w - 1.0).abs() < 1e-6 || (mid.w + 1.0).abs() < 1e-6);
    }

    #[test]
    fn animated_8byte_per_frame_quaternions() {
        // 1 rotated node, 3 frames, distinct quats per frame.
        let mut blob = build_animated_8byte(1, 0, 0, 3);
        // frame 0: (0, 0, 0, 32767) = identity
        // frame 1: (32767, 0, 0, 0)
        // frame 2: (0, 32767, 0, 0)
        let writes = [
            (32, [0i16, 0, 0, i16::MAX]),
            (40, [i16::MAX, 0, 0, 0]),
            (48, [0, i16::MAX, 0, 0]),
        ];
        for (off, vals) in writes {
            for (i, v) in vals.iter().enumerate() {
                blob[off + i * 2..off + i * 2 + 2].copy_from_slice(&v.to_le_bytes());
            }
        }
        let tracks = decode_fullframe(&blob, Codec::EightByteQuantizedRotationOnly, 3, /*quat_8byte=*/true).unwrap();
        assert_eq!(tracks.codec, Codec::EightByteQuantizedRotationOnly);
        assert_eq!(tracks.frame_count, 3);
        assert_eq!(tracks.rotations.len(), 1);
        assert_eq!(tracks.rotations[0].len(), 3);
        // Frame 0 = identity (w ≈ 1).
        assert!((tracks.rotations[0][0].w - 1.0).abs() < 1e-6);
        // Frame 1 = (1, 0, 0, 0) after normalize.
        assert!((tracks.rotations[0][1].i - 1.0).abs() < 1e-6);
        // Frame 2 = (0, 1, 0, 0) after normalize.
        assert!((tracks.rotations[0][2].j - 1.0).abs() < 1e-6);
    }

    #[test]
    fn truncated_blob_errors() {
        let blob = vec![0u8; 10]; // less than header
        let err = decode_uncompressed_static(&blob).unwrap_err();
        assert!(matches!(err, AnimationError::TruncatedHeader { .. }));
    }

    // (Quaternion `normalized()` behavior — including the
    // zero-magnitude no-op and unit-vector renormalize cases — is
    // covered by tests in `crate::math`.)
}
