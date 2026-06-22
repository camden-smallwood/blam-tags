//! Tool-importable RGBA8 TIFF writer.
//!
//! Tag profile cross-referenced against SnowyMouse's
//! `halo2-color-plate-extractor` (`main.cpp`) — that's the known-good
//! libtiff field set Tool re-imports cleanly. The `tiff = "0.9"`
//! crate sets most of the relevant tags automatically via the
//! [`RGBA8`] colortype; the three we add manually are the ones the
//! crate omits but Tool's `tiff_import` reads.
//!
//! What the crate sets for us (via `new_image::<RGBA8>`):
//! - `ImageWidth` / `ImageLength`
//! - `Compression = NONE`
//! - `BitsPerSample = [8, 8, 8, 8]`
//! - `SampleFormat = [Uint, Uint, Uint, Uint]`
//! - `PhotometricInterpretation = RGB`
//! - `SamplesPerPixel = 4`
//! - `RowsPerStrip` (auto-sized for ~1 MB strips)
//!
//! What we add explicitly:
//! - `ExtraSamples = 2` (UNASSALPHA — alpha is unassociated /
//!   straight, not premultiplied; matches the SnowyMouse reference)
//! - `Orientation = 1` (TOPLEFT)
//! - `PlanarConfiguration = 1` (Chunky / CONTIG — interleaved)
//!
//! Phase 2 scope is 2D textures only (mip 0, layer 0). Cube maps
//! emit the +X face; layered / array textures emit layer 0. The
//! horizontal-cross cube layout and vertical-strip array layout
//! land in Phase 4.

use std::io::{Cursor, Write};

use tiff::encoder::{colortype::RGBA8, TiffEncoder};
use tiff::tags::Tag;
use tiff::TiffError;

use super::{BitmapError, BitmapImage};
use super::decode::decode_to_rgba8;
use super::layout::{compose_cube_cross, compose_layer_strip};

/// Write `rgba` (memory order `[R, G, B, A]`, length `width * height
/// * 4`) as a Tool-importable TIFF. Buffers the whole encode
/// internally because the underlying `TiffEncoder` needs `Seek` and
/// most callers want to stream into a non-seekable `File`.
pub fn write_rgba8_tiff(
    out: &mut impl Write,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), BitmapError> {
    let expected = (width as usize) * (height as usize) * 4;
    if rgba.len() != expected {
        return Err(BitmapError::PixelSliceOutOfBounds {
            offset: 0,
            size: expected as u64,
            available: rgba.len() as u64,
        });
    }

    let mut buf: Vec<u8> = Vec::with_capacity(expected + 4096);
    {
        let cursor = Cursor::new(&mut buf);
        let mut encoder = TiffEncoder::new(cursor)
            .map_err(tiff_err)?;

        let mut image = encoder
            .new_image::<RGBA8>(width, height)
            .map_err(tiff_err)?;

        // Match SnowyMouse's libtiff profile. Most values mirror
        // TIFF defaults, but writing them explicitly avoids reader
        // ambiguity on older / stricter loaders (and Tool-era
        // libtiff is older).
        let dir = image.encoder();
        // ExtraSamples: one extra sample (the alpha channel),
        // declared as unassociated (= straight alpha).
        dir.write_tag(Tag::ExtraSamples, &[2u16][..])
            .map_err(tiff_err)?;
        dir.write_tag(Tag::Orientation, 1u16) // TOPLEFT
            .map_err(tiff_err)?;
        dir.write_tag(Tag::PlanarConfiguration, 1u16) // Chunky / CONTIG
            .map_err(tiff_err)?;

        image.write_data(rgba).map_err(tiff_err)?;
    }
    out.write_all(&buf)?;
    Ok(())
}

fn tiff_err(e: TiffError) -> BitmapError {
    BitmapError::Tiff(e.to_string())
}

/// Decode a `BitmapImage`'s mip 0 to RGBA8 and emit it as a TIFF.
/// Cube maps go to a 4×3 horizontal cross; 2D arrays go to a
/// vertical strip; plain 2D textures emit a flat width×height
/// image.
pub fn write_image_tiff(
    image: &BitmapImage<'_>,
    out: &mut impl Write,
) -> Result<(), BitmapError> {
    // 3D volumes haven't been observed in MCC corpora; the layout
    // strip math would need slice-major addressing rather than the
    // layer-major form `compose_layer_strip` uses. Defer until a
    // tag actually shows up.
    if image.type_name().is_some_and(|t| t.eq_ignore_ascii_case("3d texture")) {
        return Err(BitmapError::TiffLayoutDeferred("3D texture".into()));
    }

    let (composite_w, composite_h, rgba) = if image.is_cube() {
        compose_cube_cross(image)?
    } else if image.is_array() {
        compose_layer_strip(image)?
    } else {
        // Plain 2D — single decode of mip 0.
        let format = image.format()?;
        let width = image.width();
        let height = image.height();
        let bytes = image.pixel_bytes()?;
        let mip0_len = format.level_bytes(width, height) as usize;
        if bytes.len() < mip0_len {
            return Err(BitmapError::PixelSliceOutOfBounds {
                offset: 0,
                size: mip0_len as u64,
                available: bytes.len() as u64,
            });
        }
        let rgba = decode_to_rgba8(format, width, height, &bytes[..mip0_len], image.p8_palette)?;
        (width, height, rgba)
    };

    write_rgba8_tiff(out, composite_w, composite_h, &rgba)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_tiff_decoder() {
        // Construct a 2x2 RGBA8 image, write it, read it back, and
        // assert the pixel bytes survive intact. Catches strip
        // layout / endian / orientation mistakes.
        let pixels: Vec<u8> = vec![
            0xFF, 0x00, 0x00, 0xFF,  // red opaque
            0x00, 0xFF, 0x00, 0x80,  // green half-alpha
            0x00, 0x00, 0xFF, 0x40,  // blue quarter-alpha
            0x80, 0x80, 0x80, 0x00,  // grey transparent
        ];
        let mut buf = Vec::new();
        write_rgba8_tiff(&mut buf, 2, 2, &pixels).unwrap();

        let cursor = std::io::Cursor::new(&buf);
        let mut decoder = tiff::decoder::Decoder::new(cursor).unwrap();
        let (w, h) = decoder.dimensions().unwrap();
        assert_eq!((w, h), (2, 2));
        let img = decoder.read_image().unwrap();
        match img {
            tiff::decoder::DecodingResult::U8(out) => assert_eq!(out, pixels),
            other => panic!("unexpected decoder result: {other:?}"),
        }
    }

    #[test]
    fn rejects_short_pixel_buffer() {
        let mut buf = Vec::new();
        let err = write_rgba8_tiff(&mut buf, 4, 4, &[0u8; 12]);
        assert!(matches!(err, Err(BitmapError::PixelSliceOutOfBounds { .. })));
    }
}
