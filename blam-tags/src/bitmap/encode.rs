//! Packing straight-RGBA8 back into an uncompressed bitmap format.
//!
//! The inverse of the uncompressed half of [`super::decode`], and only that
//! half: nothing here compresses. It exists because a build can store the same
//! image in a block format on one platform and an uncompressed one on the
//! other — Halo's `ctx1`, `dxn_mono_alpha` and the `dxt3a`/`dxt5a` family are
//! Xbox 360 formats that MCC's PC build ships decoded — so moving such a tag
//! means decode, then pack into whatever the destination declares.

use super::BitmapError;
use super::format::BitmapFormat;

/// Pack `rgba` (4 bytes per pixel, row-major) into `format`.
///
/// `None` for a format this cannot write, which is every block-compressed one
/// and anything with no fixed byte width. A caller that gets `None` has to keep
/// the pixels in the format they arrived in and say so.
pub fn pack_from_rgba8(format: BitmapFormat, rgba: &[u8]) -> Option<Vec<u8>> {
    use BitmapFormat::*;
    let pixels = rgba.len() / 4;
    let px = |i: usize| -> [u8; 4] {
        let o = i * 4;
        [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
    };
    // Rec. 601 luma, matching what the decoders use going the other way.
    let luma = |p: [u8; 4]| -> u8 {
        ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8
    };
    let mut out = Vec::with_capacity(pixels * 4);
    match format {
        A8 => out.extend((0..pixels).map(|i| px(i)[3])),
        Y8 | R8 => out.extend((0..pixels).map(|i| luma(px(i)))),
        // One channel read as both luminance and alpha; the decoder writes it
        // to all four, so any one of them recovers it.
        Ay8 => out.extend((0..pixels).map(|i| px(i)[3])),
        A8y8 => {
            for i in 0..pixels {
                let p = px(i);
                out.push(luma(p));
                out.push(p[3]);
            }
        }
        // A signed two-channel normal map. The decoder maps -128..127 onto
        // 0..255, so this is the same shift back.
        V8u8 => {
            for i in 0..pixels {
                let p = px(i);
                out.push(p[0].wrapping_sub(128));
                out.push(p[1].wrapping_sub(128));
            }
        }
        G8b8 => {
            for i in 0..pixels {
                let p = px(i);
                out.push(p[1].wrapping_sub(128));
                out.push(p[2].wrapping_sub(128));
            }
        }
        A8r8g8b8 => {
            for i in 0..pixels {
                let p = px(i);
                out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
        }
        X8r8g8b8 => {
            for i in 0..pixels {
                let p = px(i);
                out.extend_from_slice(&[p[2], p[1], p[0], 0xFF]);
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Re-pack one whole mip chain from `source` format into `target`.
///
/// Levels are walked with the source's own sizes and written with the target's,
/// which is the whole point: the two formats disagree about how many bytes a
/// level takes, and the destination's `pixels size` is computed from its own.
pub fn transcode_levels(
    source: BitmapFormat,
    target: BitmapFormat,
    width: u32,
    height: u32,
    levels: u32,
    layers: u32,
    data: &[u8],
    p8_palette: super::p8::P8Palette,
) -> Result<Vec<u8>, BitmapError> {
    let mut out = Vec::with_capacity(data.len() * 2);
    let mut offset = 0usize;
    for _layer in 0..layers.max(1) {
        for level in 0..levels.max(1) {
            let level_width = (width >> level).max(1);
            let level_height = (height >> level).max(1);
            let size = source.level_bytes(level_width, level_height) as usize;
            let end = offset + size;
            if end > data.len() {
                return Err(BitmapError::PixelSliceOutOfBounds {
                    offset: offset as u64,
                    size: size as u64,
                    available: data.len() as u64,
                });
            }
            let rgba = super::decode::decode_to_rgba8(
                source,
                level_width,
                level_height,
                &data[offset..end],
                p8_palette,
            )?;
            offset = end;
            let packed = pack_from_rgba8(target, &rgba)
                .ok_or(BitmapError::FormatNotSupported(format!("{target:?}")))?;
            out.extend_from_slice(&packed);
        }
    }
    Ok(out)
}
