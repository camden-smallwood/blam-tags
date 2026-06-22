//! Compose multi-layer bitmaps (cube maps, 2D arrays, 3D volumes)
//! into a single Tool-importable RGBA8 layout.
//!
//! Three layouts:
//!
//! - **Cube map** → 4×3 horizontal cross, Bungie color-plate
//!   convention. Six faces arranged with `+Y` on top, `-Y` on
//!   bottom, and the middle row as `+X +Z -X -Z` (DirectX storage
//!   order [+X, -X, +Y, -Y, +Z, -Z] mapped to display positions
//!   top=2, middle=[0,4,1,5], bottom=3). Empty cells filled with
//!   Bungie's magic blue (R=0, G=0, B=255, A=255), the color-plate
//!   "ignore this region" marker.
//!
//! - **2D array** (`type = array`, `depth > 1`) → vertical strip
//!   `width × (height × depth)` with each array layer stacked
//!   top-to-bottom.
//!
//! - **3D volume** (`type = 3D`, currently unobserved in MCC
//!   corpora — kept here for completeness) → same vertical strip
//!   as 2D array.
//!
//! Each face / layer is decoded independently via
//! [`decode_to_rgba8`], so this module doesn't care about the
//! source pixel format.
//!
//! Engine-debug-plate caveat: `extract_build_debug_plate` in
//! ManagedBlam uses a different middle-row order (SSE constant at
//! `0x180D20B10` = `[4, 0, 1, 2]`). That's a runtime debug
//! visualization, not Tool's documented `Cube Map Slicer` input
//! format. We commit to the DX cross layout here and verify
//! re-import against Tool empirically (Phase 5).

use super::decode::decode_to_rgba8;
use super::{BitmapError, BitmapImage};

/// Number of faces in a cube map. Mirrors `k_cube_map_faces_count`
/// from H3 source `bitmaps.h`.
const CUBE_FACE_COUNT: u32 = 6;

/// DX cube cross — `(column, row, storage_face_index)` for each of
/// the 6 occupied cells. Columns 0..3, rows 0..2. Storage faces
/// follow `e_cube_map_face` order:
/// 0=+X, 1=-X, 2=+Y, 3=-Y, 4=+Z, 5=-Z.
const CUBE_CROSS_CELLS: [(u32, u32, u32); 6] = [
    (1, 0, 2), // top:        +Y
    (0, 1, 0), // middle L:   +X
    (1, 1, 4), // middle:     +Z
    (2, 1, 1), // middle:     -X
    (3, 1, 5), // middle R:   -Z
    (1, 2, 3), // bottom:     -Y
];

/// RGBA8 fill for the unused cube-cross cells. Tool's color-plate
/// pipeline treats pure (0,0,255) blue as "skip" / "background".
const CROSS_BG: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF];

/// Pull mip 0 of layer `layer_index` from a multi-layer bitmap.
/// Layers in `processed pixel data` are laid out chain-major:
/// each layer holds its full mip chain contiguously, then the next
/// layer begins.
fn layer_mip0_bytes<'a>(
    image: &BitmapImage<'a>,
    layer_index: u32,
) -> Result<&'a [u8], BitmapError> {
    let format = image.format()?;
    let width = image.width();
    let height = image.height();
    let bytes = image.pixel_bytes()?;

    let chain_bytes = format.surface_bytes(width, height, image.mipmap_levels()) as usize;
    let mip0_bytes = format.level_bytes(width, height) as usize;
    let start = (layer_index as usize) * chain_bytes;
    let end = start + mip0_bytes;

    if end > bytes.len() {
        return Err(BitmapError::PixelSliceOutOfBounds {
            offset: start as u64,
            size: mip0_bytes as u64,
            available: bytes.len() as u64,
        });
    }
    Ok(&bytes[start..end])
}

/// Decode `image`'s six cube-map faces and compose them into the
/// DX cross layout. Returns `(composite_width, composite_height,
/// rgba_bytes)`. Composite size is `(4 * width, 3 * height)`.
pub fn compose_cube_cross(
    image: &BitmapImage<'_>,
) -> Result<(u32, u32, Vec<u8>), BitmapError> {
    let format = image.format()?;
    let cell_w = image.width();
    let cell_h = image.height();
    let out_w = cell_w * 4;
    let out_h = cell_h * 3;

    let mut out = make_filled(out_w, out_h, CROSS_BG);

    for &(col, row, face) in &CUBE_CROSS_CELLS {
        if face >= CUBE_FACE_COUNT {
            // Defensive — `CUBE_CROSS_CELLS` is a constant under
            // our control, but a bad edit shouldn't write off the
            // end of the source bytes.
            unreachable!("cube-cross cell points at out-of-range face {face}");
        }
        let layer_bytes = layer_mip0_bytes(image, face)?;
        let face_rgba = decode_to_rgba8(format, cell_w, cell_h, layer_bytes, image.p8_palette)?;

        let dst_x = col * cell_w;
        let dst_y = row * cell_h;
        blit_rgba(&face_rgba, cell_w, cell_h, &mut out, out_w, dst_x, dst_y);
    }

    Ok((out_w, out_h, out))
}

/// Decode each array / 3D layer of `image` and stack them into a
/// vertical strip. Returns `(width, total_height, rgba_bytes)` with
/// `total_height = height * layer_count`.
pub fn compose_layer_strip(
    image: &BitmapImage<'_>,
) -> Result<(u32, u32, Vec<u8>), BitmapError> {
    let format = image.format()?;
    let width = image.width();
    let height = image.height();
    let layers = image.layer_count();
    let total_height = height.saturating_mul(layers);

    let mut out = vec![0u8; (width as usize) * (total_height as usize) * 4];

    for layer in 0..layers {
        let layer_bytes = layer_mip0_bytes(image, layer)?;
        let layer_rgba = decode_to_rgba8(format, width, height, layer_bytes, image.p8_palette)?;
        let dst_y = layer * height;
        blit_rgba(&layer_rgba, width, height, &mut out, width, 0, dst_y);
    }

    Ok((width, total_height, out))
}

/// Allocate a `width × height` RGBA8 buffer pre-filled with `pixel`.
fn make_filled(width: u32, height: u32, pixel: [u8; 4]) -> Vec<u8> {
    let len = (width as usize) * (height as usize) * 4;
    let mut out = vec![0u8; len];
    for chunk in out.chunks_exact_mut(4) {
        chunk.copy_from_slice(&pixel);
    }
    out
}

/// Copy a `src_w × src_h` RGBA8 image into `dst` at `(dst_x, dst_y)`.
/// `dst` is a `dst_w × ?` row-major RGBA8 buffer.
fn blit_rgba(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dst_w: u32,
    dst_x: u32,
    dst_y: u32,
) {
    let row_bytes = (src_w as usize) * 4;
    for y in 0..src_h {
        let src_offset = (y as usize) * row_bytes;
        let dst_offset = ((dst_y + y) as usize) * (dst_w as usize) * 4 + (dst_x as usize) * 4;
        dst[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&src[src_offset..src_offset + row_bytes]);
    }
}

