//! Turning an Xbox 360 animation blob round into the byte order a PC tag reads.
//!
//! It owns the map of where an `animation_data` blob keeps its multi-byte
//! values; decoding those values is [`super::codec`]'s business and reading the
//! blob out of a 360 resource is [`super::resource`]'s.
//!
//! A blob is a run of sections whose byte sizes the member's `data sizes` gives,
//! laid end to end in that order. Two of them are codec streams with a shape of
//! their own; the rest are plain arrays. Nothing in the blob says which is
//! which, so the section index is the map, and a section this does not recognise
//! is refused rather than swapped on a guess -- a wrongly swapped animation
//! plays, which is a worse failure than one that does not convert.

use super::codec::{Codec, curve_word_offsets};

/// Why a blob could not be turned round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapRefusal {
    /// The section sizes do not add up to the blob.
    SizesDisagree { sections: usize, blob: usize },
    /// A codec whose stream shape this does not know.
    UnknownCodec(u8),
    /// A codec that is known but whose stream would not walk.
    UnreadableStream(Codec),
    /// A section that carries something, at an index with no known shape.
    UnknownSection { index: usize, size: usize },
    /// A stream ran past the end of its section.
    Truncated { section: usize },
}

impl std::fmt::Display for SwapRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SizesDisagree { sections, blob } => write!(
                f,
                "the section sizes add up to {sections} bytes but the blob is {blob}",
            ),
            Self::UnknownCodec(byte) => {
                write!(f, "animation codec {byte} is not one this knows the shape of")
            }
            Self::UnreadableStream(codec) => {
                write!(f, "the {codec:?} stream would not read back")
            }
            Self::UnknownSection { index, size } => write!(
                f,
                "section {index} carries {size} bytes and this does not know its shape",
            ),
            Self::Truncated { section } => {
                write!(f, "section {section} runs past the end of the blob")
            }
        }
    }
}

/// The `data sizes` index of each section, in the order they sit in the blob.
///
/// Reach keeps the field names an older engine used and changes what several of
/// them mean -- `static_node_flags` is the static codec stream's byte size, not
/// the static flags' -- so these are named for what they hold. Matches the
/// positional reading in [`super::codec`].
const STATIC_CODEC: usize = 0;
const ANIMATED_CODEC: usize = 1;
const STATIC_FLAGS: usize = 2;
const ANIMATED_FLAGS: usize = 3;
const MOVEMENT: usize = 4;
const PILL_OFFSET: usize = 5;

/// Sections that are flat arrays of 32-bit values -- floats or flag words,
/// which swap the same way. Everything from `default_data` on is one of these:
/// per-node offsets, object-space transforms, IK chain data, anchor points.
const WORD_ARRAY_SECTIONS: &[usize] = &[
    STATIC_FLAGS,
    ANIMATED_FLAGS,
    MOVEMENT,
    PILL_OFFSET,
    6,  // default_data
    7,  // uncompressed_data
    8,  // compressed_data
    9,  // blend_screen_data
    10, // object_space_offset_data
    11, // ik_chain_event_data
    12, // ik_chain_control_data
    13, // ik_chain_proxy_data
    14, // ik_chain_pole_vector_data
    15, // uncompressed_object_space_data
    16, // fik_anchor_data
];

/// Turn one `animation_data` blob round in place.
///
/// `data_sizes` is the member's own, in schema order; `frame_count` is what the
/// member declares, which the keyframe and curve codecs need to walk their
/// streams.
pub fn swap_animation_blob(
    blob: &mut [u8],
    data_sizes: &[i32],
    frame_count: u16,
) -> Result<(), SwapRefusal> {
    let sections: usize = data_sizes.iter().map(|size| (*size).max(0) as usize).sum();
    if sections != blob.len() {
        return Err(SwapRefusal::SizesDisagree { sections, blob: blob.len() });
    }
    let mut at = 0usize;
    for (index, size) in data_sizes.iter().enumerate() {
        let size = (*size).max(0) as usize;
        if size == 0 {
            continue;
        }
        let end = at + size;
        let section = blob.get_mut(at..end).ok_or(SwapRefusal::Truncated { section: index })?;
        if index == STATIC_CODEC || index == ANIMATED_CODEC {
            swap_codec_stream(section, frame_count, index == STATIC_CODEC)?;
        } else if WORD_ARRAY_SECTIONS.contains(&index) {
            swap_words(section, 4);
        } else {
            return Err(SwapRefusal::UnknownSection { index, size });
        }
        at = end;
    }
    Ok(())
}

/// Turn one codec stream round.
///
/// The static stream is one frame of rest pose whatever its header says, which
/// is why the frame count is not enough on its own to place a fullframe
/// stream's payload.
fn swap_codec_stream(
    stream: &mut [u8],
    frame_count: u16,
    is_static: bool,
) -> Result<(), SwapRefusal> {
    let byte = *stream.first().ok_or(SwapRefusal::Truncated { section: 0 })?;
    let codec = Codec::from_byte(byte).ok_or(SwapRefusal::UnknownCodec(byte))?;
    let frames = if is_static { 1 } else { frame_count.max(1) };
    match codec {
        // The three node counts and the codec byte are the first four bytes and
        // stay as they are; everything else in a fullframe header is a word.
        Codec::UncompressedStatic
        | Codec::UncompressedAnimated
        | Codec::EightByteQuantizedRotationOnly
        | Codec::BlendScreen => swap_fullframe(stream, codec, frames),
        Codec::ByteKeyframeLightlyQuantized | Codec::ReverseByteKeyframeLightlyQuantized => {
            swap_keyframe(stream, codec, 1)
        }
        Codec::WordKeyframeLightlyQuantized | Codec::ReverseWordKeyframeLightlyQuantized => {
            swap_keyframe(stream, codec, 2)
        }
        Codec::Curve | Codec::RevisedCurve => {
            let revised = codec == Codec::RevisedCurve;
            let words = curve_word_offsets(stream, codec, frames, revised)
                .ok_or(SwapRefusal::UnreadableStream(codec))?;
            for (at, width) in words {
                swap_one(stream, at, width as usize);
            }
            Ok(())
        }
        // A stream of int16 indices into a graph-level pool.
        Codec::SharedStatic => {
            swap_words(&mut stream[4..], 2);
            Ok(())
        }
        Codec::NoCompression => Err(SwapRefusal::UnknownCodec(byte)),
    }
}

/// Header of 32 bytes, then rotations, translations and scales, each a run of
/// per-node arrays whose stride the header gives.
fn swap_fullframe(stream: &mut [u8], codec: Codec, frames: u16) -> Result<(), SwapRefusal> {
    const HEADER: usize = 32;
    if stream.len() < HEADER {
        return Err(SwapRefusal::UnreadableStream(codec));
    }
    let counts = [stream[1] as usize, stream[2] as usize, stream[3] as usize];
    // Read before the header is turned round, because these are its own words.
    let word = |at: usize| u32::from_be_bytes(stream[at..at + 4].try_into().unwrap()) as usize;
    let translation_offset = word(12);
    let scale_offset = word(16);
    let strides = [word(20), word(24), word(28)];
    swap_words(&mut stream[4..HEADER], 4);

    // A quaternion is four 16-bit values in the quantized codecs and four
    // floats in the raw ones; a translation is three floats and a scale one.
    let quat_width = match codec {
        Codec::UncompressedAnimated | Codec::BlendScreen => 4,
        _ => 2,
    };
    let quat_size = quat_width * 4;
    let element = [quat_size, 12, 4];
    let width = [quat_width, 4, 4];
    let starts = [HEADER, translation_offset, scale_offset];
    for component in 0..3 {
        let count = counts[component];
        if count == 0 {
            continue;
        }
        // A zero stride means the stream is written sequentially, which is what
        // a static pose does -- one frame per node, back to back.
        let stride = if strides[component] == 0 {
            element[component] * frames as usize
        } else {
            strides[component]
        };
        let start = starts[component];
        for node in 0..count {
            let at = start + node * stride;
            for frame in 0..frames as usize {
                let at = at + frame * element[component];
                if at + element[component] > stream.len() {
                    return Err(SwapRefusal::UnreadableStream(codec));
                }
                swap_words(&mut stream[at..at + element[component]], width[component]);
            }
        }
    }
    Ok(())
}

/// Header of 48 bytes, a packed word per node, then a key-time table and a
/// payload table per component.
fn swap_keyframe(
    stream: &mut [u8],
    codec: Codec,
    time_width: usize,
) -> Result<(), SwapRefusal> {
    const HEADER: usize = 48;
    if stream.len() < HEADER {
        return Err(SwapRefusal::UnreadableStream(codec));
    }
    let counts = [stream[1] as usize, stream[2] as usize, stream[3] as usize];
    let word = |at: usize| u32::from_be_bytes(stream[at..at + 4].try_into().unwrap()) as usize;
    let time_starts = [word(20), word(24), word(28)];
    let payload_starts = [word(32), word(36), word(40)];
    swap_words(&mut stream[4..HEADER], 4);

    let packed_count = counts.iter().sum::<usize>();
    let packed_end = HEADER + packed_count * 4;
    if packed_end > stream.len() {
        return Err(SwapRefusal::UnreadableStream(codec));
    }
    // How far each component's tables run: the last key of the last node.
    let mut keys_end = [0usize; 3];
    let mut index = 0usize;
    for (component, count) in counts.iter().enumerate() {
        for _ in 0..*count {
            let at = HEADER + index * 4;
            let packed = u32::from_be_bytes(stream[at..at + 4].try_into().unwrap());
            let end = (packed >> 12) as usize + (packed & 0xFFF) as usize;
            keys_end[component] = keys_end[component].max(end);
            index += 1;
        }
    }
    swap_words(&mut stream[HEADER..packed_end], 4);

    let element = [8usize, 12, 4];
    let width = [2usize, 4, 4];
    for component in 0..3 {
        if keys_end[component] == 0 {
            continue;
        }
        if time_width > 1 {
            let start = time_starts[component];
            let end = start + keys_end[component] * time_width;
            if end > stream.len() {
                return Err(SwapRefusal::UnreadableStream(codec));
            }
            swap_words(&mut stream[start..end], time_width);
        }
        let start = payload_starts[component];
        let end = start + keys_end[component] * element[component];
        if end > stream.len() {
            return Err(SwapRefusal::UnreadableStream(codec));
        }
        swap_words(&mut stream[start..end], width[component]);
    }
    Ok(())
}

/// Reverse each `width`-byte group. A trailing part-group is left alone: it is
/// not a whole value, so there is nothing to turn round.
fn swap_words(bytes: &mut [u8], width: usize) {
    if width < 2 {
        return;
    }
    for word in bytes.chunks_exact_mut(width) {
        word.reverse();
    }
}

fn swap_one(bytes: &mut [u8], at: usize, width: usize) {
    if width < 2 {
        return;
    }
    if let Some(word) = bytes.get_mut(at..at + width) {
        word.reverse();
    }
}
