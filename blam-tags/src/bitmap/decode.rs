//! Decode raw `.bitmap` pixel bytes into RGBA8 (memory order
//! `[R, G, B, A]`) for downstream TIFF / preview pipelines.
//!
//! Phase 1 covered the 14 uncompressed formats. Phase 3 wires in
//! BC1/2/3/4/5 via `bcdec_rs` plus the Halo-specific
//! `dxn_mono_alpha` codec. `ctx1` is the only block-compressed
//! schema variant still unsupported (not observed in MCC corpora).
//!
//! Channel-mapping conventions matched to the engine pipeline:
//! - Packed integer formats (`a8r8g8b8`, `x8r8g8b8`, `a4r4g4b4`,
//!   `a8y8`) follow D3D9 MSB-to-LSB naming → little-endian memory
//!   has the lowest-named channel at byte 0.
//! - "Multi-cell" formats with one cell per channel (`abgrfp16`,
//!   `abgrfp32`, `a16b16g16r16`, `signedr16g16b16a16`,
//!   `q8w8v8u8`) use DXGI-equivalent memory order (R first), per
//!   the `dxgi_format` map in [`super::dds`].
//! - Single-channel formats expand to RGBA8 with the engine's
//!   "useful preview" convention: `a8` → white-with-alpha, `y8` →
//!   replicated grey, `r8` → red-only.
//! - Signed normalmap formats (`v8u8`, `q8w8v8u8`,
//!   `signedr16g16b16a16`) are biased by +128 (or +32768 for
//!   16-bit) per channel, matching the engine's
//!   `s*(1/256)+0.5` rebias in `extract_debug_plate_copy`.
//! - HDR float formats clamp to `[0, 1]` and scale to 255 — the
//!   same loss the engine's debug-plate path applies. A future
//!   float-TIFF path can take a different lane.

use super::{BitmapError, BitmapFormat};

/// Decode one mip level of the given format into RGBA8 (memory order
/// `[R, G, B, A]`). Output length is always `width * height * 4`.
///
/// `input` must hold at least `format.level_bytes(width, height)`
/// bytes — block-compressed formats round dimensions up to the 4×4
/// block grid.
pub fn decode_to_rgba8(
    format: BitmapFormat,
    width: u32,
    height: u32,
    input: &[u8],
) -> Result<Vec<u8>, BitmapError> {
    let need = format.level_bytes(width, height) as usize;
    if input.len() < need {
        return Err(BitmapError::PixelSliceOutOfBounds {
            offset: 0,
            size: need as u64,
            available: input.len() as u64,
        });
    }

    let pixels = (width as usize) * (height as usize);
    let mut out = vec![0u8; pixels * 4];

    use BitmapFormat::*;
    match format {
        // 8-bit single-channel
        A8 => decode_a8(&input[..need], &mut out),
        Y8 => decode_y8(&input[..need], &mut out),
        R8 => decode_r8(&input[..need], &mut out),
        Ay8 => decode_ay8(&input[..need], &mut out),

        // 16-bit packed
        A8y8 => decode_a8y8(&input[..need], &mut out),
        R5g6b5 => decode_r5g6b5(&input[..need], &mut out),
        A1r5g5b5 => decode_a1r5g5b5(&input[..need], &mut out),
        A4r4g4b4 => decode_a4r4g4b4(&input[..need], &mut out),
        A4r4g4b4Font => decode_a4r4g4b4_font(&input[..need], &mut out),
        G8b8 => decode_g8b8(&input[..need], &mut out),
        V8u8 => decode_v8u8(&input[..need], &mut out),
        L16 => decode_l16(&input[..need], &mut out),
        F16Mono => decode_f16_mono(&input[..need], &mut out),
        F16Red => decode_f16_red(&input[..need], &mut out),

        // 32-bit packed
        X8r8g8b8 => decode_x8r8g8b8(&input[..need], &mut out),
        A8r8g8b8 => decode_a8r8g8b8(&input[..need], &mut out),
        Q8w8v8u8 => decode_q8w8v8u8(&input[..need], &mut out),
        A2r10g10b10 => decode_a2r10g10b10(&input[..need], &mut out),
        V16u16 => decode_v16u16(&input[..need], &mut out),
        R16g16 => decode_r16g16(&input[..need], &mut out),

        // 64-bit / 128-bit multi-cell
        A16b16g16r16 => decode_a16b16g16r16(&input[..need], &mut out),
        Signedr16g16b16a16 => decode_signedr16g16b16a16(&input[..need], &mut out),
        Abgrfp16 => decode_abgrfp16(&input[..need], &mut out),
        Abgrfp32 => decode_abgrfp32(&input[..need], &mut out),

        // Block-compressed formats — bcdec_rs ports of bcdec.
        Dxt1 => decode_bc1(&input[..need], width, height, &mut out),
        Dxt3 => decode_bc2(&input[..need], width, height, &mut out),
        Dxt5 => decode_bc3(&input[..need], width, height, &mut out),
        Dxt5a => decode_bc4_rgba(&input[..need], width, height, &mut out, ChannelMask::ALL),
        Dxt5aMono => decode_bc4_rgba(&input[..need], width, height, &mut out, ChannelMask::RGB_ONLY),
        Dxt5aAlpha => decode_bc4_rgba(&input[..need], width, height, &mut out, ChannelMask::ALPHA_ONLY),
        Dxn => decode_bc5(&input[..need], width, height, &mut out),
        Dxt3a => decode_dxt3a(&input[..need], width, height, &mut out, ChannelMask::ALL),
        Dxt3aMono => decode_dxt3a(&input[..need], width, height, &mut out, ChannelMask::RGB_ONLY),
        Dxt3aAlpha => decode_dxt3a(&input[..need], width, height, &mut out, ChannelMask::ALPHA_ONLY),
        Dxt3a1111 => decode_dxt3a_1111(&input[..need], width, height, &mut out),
        Dxt5nm => decode_dxt5nm(&input[..need], width, height, &mut out),
        Ctx1 => decode_ctx1(&input[..need], width, height, &mut out),
        Dxt5Red => decode_bc3_single_channel(&input[..need], width, height, &mut out, 0),
        Dxt5Green => decode_bc3_single_channel(&input[..need], width, height, &mut out, 1),
        Dxt5Blue => decode_bc3_single_channel(&input[..need], width, height, &mut out, 2),
        DxnMonoAlpha => decode_dxn_mono_alpha_rgba(&input[..need], width, height, &mut out),

        // Schema-reserved / not-observed slots. Surface as an
        // explicit unsupported error rather than silently producing
        // garbage — keeps caller behavior predictable for future
        // corpora that surface these.
        Unused2 | Unused3 | Unused4 | Unused7 | Unused8 | Unused9
        | SoftwareRgbfp32 | Depth24 => {
            return Err(BitmapError::FormatNotSupported(format!("{format:?}")));
        }
    }

    Ok(out)
}

/// Decode a single pixel from a multi-mip compressed or uncompressed
/// bitmap input. Walks the mip chain to `mip_index` (clamped to
/// available mips by the caller — this function only validates byte
/// bounds, not mip count), then fetches the pixel.
///
/// Returns `None` when the format isn't in the supported subset:
/// currently `Dxt1` / `Dxt3` / `Dxt5` / `A8r8g8b8` / `X8r8g8b8`. These
/// cover the formats observed on Halo 3 MCC postprocess albedo +
/// blend_map textures. Other formats can round-trip through
/// [`decode_to_rgba8`] which allocates a per-mip RGBA8 surface; this
/// helper exists so hot-path callers (decorator/lightprobe bakes
/// running 100K+ sample queries) can keep per-call allocation at zero.
pub fn decode_pixel_at(
    format: BitmapFormat,
    base_width: u32,
    base_height: u32,
    input: &[u8],
    mip_index: u32,
    x: u32,
    y: u32,
) -> Option<[u8; 4]> {
    let mut w = base_width.max(1);
    let mut h = base_height.max(1);
    let mut offset = 0usize;
    for _ in 0..mip_index {
        offset = offset.checked_add(format.level_bytes(w, h) as usize)?;
        w = (w / 2).max(1);
        h = (h / 2).max(1);
    }
    let mip_bytes = format.level_bytes(w, h) as usize;
    if offset + mip_bytes > input.len() {
        return None;
    }
    let mip = &input[offset..offset + mip_bytes];
    if x >= w || y >= h {
        return None;
    }

    use BitmapFormat::*;
    match format {
        Dxt1 => bc_decode_pixel(mip, w, x, y, 8, |block, out| bcdec_rs::bc1(block, out, 16)),
        Dxt3 => bc_decode_pixel(mip, w, x, y, 16, |block, out| bcdec_rs::bc2(block, out, 16)),
        Dxt5 => bc_decode_pixel(mip, w, x, y, 16, |block, out| bcdec_rs::bc3(block, out, 16)),
        A8r8g8b8 => {
            // Stored little-endian as (B, G, R, A) per dword.
            let off = ((y * w + x) * 4) as usize;
            Some([mip[off + 2], mip[off + 1], mip[off], mip[off + 3]])
        }
        X8r8g8b8 => {
            // Same layout as A8r8g8b8 with alpha forced to 0xFF.
            let off = ((y * w + x) * 4) as usize;
            Some([mip[off + 2], mip[off + 1], mip[off], 0xFF])
        }
        _ => None,
    }
}

fn bc_decode_pixel(
    mip: &[u8],
    mip_width: u32,
    x: u32,
    y: u32,
    block_bytes: usize,
    decode: impl FnOnce(&[u8], &mut [u8]),
) -> Option<[u8; 4]> {
    let bx = (x / 4) as usize;
    let by = (y / 4) as usize;
    let blocks_w = ((mip_width + 3) / 4).max(1) as usize;
    let block_idx = by * blocks_w + bx;
    let block_off = block_idx * block_bytes;
    if block_off + block_bytes > mip.len() {
        return None;
    }
    let mut staging = [0u8; 64];
    decode(&mip[block_off..block_off + block_bytes], &mut staging);
    let in_x = (x & 3) as usize;
    let in_y = (y & 3) as usize;
    let p = (in_y * 4 + in_x) * 4;
    Some([staging[p], staging[p + 1], staging[p + 2], staging[p + 3]])
}

/// Channel-mask flag for BC4-derived decoders that all share the
/// "decode a single value per pixel and splat it" shape. The R/G/B
/// flags carry the value to the named output channel; `ALPHA` carries
/// it to the alpha channel. Flags are independent so callers can pick
/// any subset.
#[derive(Debug, Clone, Copy)]
struct ChannelMask {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl ChannelMask {
    /// Splat the source value into all four channels. Matches
    /// TagTool's `DecompressDXT5a` / `DecompressDXT3a` (full).
    const ALL: Self = Self { r: 0xFF, g: 0xFF, b: 0xFF, a: 0xFF };
    /// Splat into RGB only; alpha forced to `0`. Matches the
    /// `*Mono` variants.
    const RGB_ONLY: Self = Self { r: 0xFF, g: 0xFF, b: 0xFF, a: 0 };
    /// Splat into alpha only; RGB forced to `0`. Matches the
    /// `*Alpha` variants.
    const ALPHA_ONLY: Self = Self { r: 0, g: 0, b: 0, a: 0xFF };
}

//================================================================================
// Single-channel formats
//================================================================================

/// `a8`: 1 byte = alpha. Expand as `(255, 255, 255, alpha)` so the
/// alpha channel carries the data and viewers see white-on-alpha.
fn decode_a8(input: &[u8], out: &mut [u8]) {
    for (i, &a) in input.iter().enumerate() {
        let p = i * 4;
        out[p] = 255;
        out[p + 1] = 255;
        out[p + 2] = 255;
        out[p + 3] = a;
    }
}

/// `y8`: 1 byte = luminance. Replicate to RGB, full alpha.
fn decode_y8(input: &[u8], out: &mut [u8]) {
    for (i, &y) in input.iter().enumerate() {
        let p = i * 4;
        out[p] = y;
        out[p + 1] = y;
        out[p + 2] = y;
        out[p + 3] = 255;
    }
}

/// `r8`: 1 byte = red. Other channels zero, full alpha.
fn decode_r8(input: &[u8], out: &mut [u8]) {
    for (i, &r) in input.iter().enumerate() {
        let p = i * 4;
        out[p] = r;
        out[p + 1] = 0;
        out[p + 2] = 0;
        out[p + 3] = 255;
    }
}

/// `ay8`: 1 byte replicated to alpha *and* luminance. Output the byte
/// in all four channels.
fn decode_ay8(input: &[u8], out: &mut [u8]) {
    for (i, &v) in input.iter().enumerate() {
        let p = i * 4;
        out[p] = v;
        out[p + 1] = v;
        out[p + 2] = v;
        out[p + 3] = v;
    }
}

//================================================================================
// 16-bit packed
//================================================================================

/// `a8y8`: u16 LE = `(A << 8) | Y`. Memory `[Y, A]`. Replicate Y to
/// RGB; A goes to alpha.
fn decode_a8y8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let y = chunk[0];
        let a = chunk[1];
        let p = i * 4;
        out[p] = y;
        out[p + 1] = y;
        out[p + 2] = y;
        out[p + 3] = a;
    }
}

/// `a4r4g4b4`: u16 LE with bits `AAAA RRRR GGGG BBBB`. Each 4-bit
/// nibble expanded to 8 bits via `n * 0x11` (bit replication).
fn decode_a4r4g4b4(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let a = ((v >> 12) & 0xF) as u8;
        let r = ((v >> 8) & 0xF) as u8;
        let g = ((v >> 4) & 0xF) as u8;
        let b = (v & 0xF) as u8;
        let p = i * 4;
        out[p] = r * 0x11;
        out[p + 1] = g * 0x11;
        out[p + 2] = b * 0x11;
        out[p + 3] = a * 0x11;
    }
}

/// `v8u8`: 16-bit signed normalmap. u16 packed `V<<8 | U` → memory
/// `[U, V]` (V is the high byte). Maps to `(V, U)` in `(R, G)` per
/// the existing DDS pixelformat. Bias by `+128` so signed bytes
/// display as unsigned.
fn decode_v8u8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let u = chunk[0] as i8;
        let v = chunk[1] as i8;
        let p = i * 4;
        out[p] = (v as i16 + 128) as u8;       // R = V
        out[p + 1] = (u as i16 + 128) as u8;   // G = U
        out[p + 2] = 128;                      // B = 0.5 (z implied)
        out[p + 3] = 255;
    }
}

//================================================================================
// 32-bit packed
//================================================================================

/// `x8r8g8b8`: u32 LE bytes `[B, G, R, X]`. Output `(R, G, B, 255)`.
fn decode_x8r8g8b8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let p = i * 4;
        out[p] = chunk[2];
        out[p + 1] = chunk[1];
        out[p + 2] = chunk[0];
        out[p + 3] = 255;
    }
}

/// `a8r8g8b8`: u32 LE bytes `[B, G, R, A]`. Output `(R, G, B, A)`.
fn decode_a8r8g8b8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let p = i * 4;
        out[p] = chunk[2];
        out[p + 1] = chunk[1];
        out[p + 2] = chunk[0];
        out[p + 3] = chunk[3];
    }
}

/// `q8w8v8u8`: u32 packed `Q<<24 | W<<16 | V<<8 | U` MSB-to-LSB.
/// Memory `[U, V, W, Q]`. Per DXGI map, treats as
/// `R8G8B8A8_SNORM` → `(U, V, W, Q)` in `(R, G, B, A)`. Bias each
/// signed byte by `+128`.
fn decode_q8w8v8u8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let p = i * 4;
        for c in 0..4 {
            out[p + c] = ((chunk[c] as i8) as i16 + 128) as u8;
        }
    }
}

//================================================================================
// 64-bit / 128-bit multi-cell
//================================================================================

/// `a16b16g16r16`: 4×u16 LE in memory order `[R, G, B, A]`. Truncate
/// each u16 to its high byte (`>> 8`).
fn decode_a16b16g16r16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(8).enumerate() {
        let r = u16::from_le_bytes([chunk[0], chunk[1]]);
        let g = u16::from_le_bytes([chunk[2], chunk[3]]);
        let b = u16::from_le_bytes([chunk[4], chunk[5]]);
        let a = u16::from_le_bytes([chunk[6], chunk[7]]);
        let p = i * 4;
        out[p] = (r >> 8) as u8;
        out[p + 1] = (g >> 8) as u8;
        out[p + 2] = (b >> 8) as u8;
        out[p + 3] = (a >> 8) as u8;
    }
}

/// `signedr16g16b16a16`: 4×i16 LE in memory order `[R, G, B, A]`.
/// Bias by `+32768`, then truncate to high byte.
fn decode_signedr16g16b16a16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(8).enumerate() {
        let r = i16::from_le_bytes([chunk[0], chunk[1]]) as i32 + 32768;
        let g = i16::from_le_bytes([chunk[2], chunk[3]]) as i32 + 32768;
        let b = i16::from_le_bytes([chunk[4], chunk[5]]) as i32 + 32768;
        let a = i16::from_le_bytes([chunk[6], chunk[7]]) as i32 + 32768;
        let p = i * 4;
        out[p] = (r >> 8) as u8;
        out[p + 1] = (g >> 8) as u8;
        out[p + 2] = (b >> 8) as u8;
        out[p + 3] = (a >> 8) as u8;
    }
}

/// `abgrfp16`: 4×half-float in memory order `[R, G, B, A]`. Clamp
/// `[0, 1]`, scale to 255. Lossy for HDR — float TIFF will skip
/// this path.
fn decode_abgrfp16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(8).enumerate() {
        let r = half_to_f32(u16::from_le_bytes([chunk[0], chunk[1]]));
        let g = half_to_f32(u16::from_le_bytes([chunk[2], chunk[3]]));
        let b = half_to_f32(u16::from_le_bytes([chunk[4], chunk[5]]));
        let a = half_to_f32(u16::from_le_bytes([chunk[6], chunk[7]]));
        let p = i * 4;
        out[p] = clamp_to_u8(r);
        out[p + 1] = clamp_to_u8(g);
        out[p + 2] = clamp_to_u8(b);
        out[p + 3] = clamp_to_u8(a);
    }
}

/// `abgrfp32`: 4×f32 in memory order `[R, G, B, A]`. Same clamp as
/// `abgrfp16`.
fn decode_abgrfp32(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(16).enumerate() {
        let r = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let g = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        let b = f32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]);
        let a = f32::from_le_bytes([chunk[12], chunk[13], chunk[14], chunk[15]]);
        let p = i * 4;
        out[p] = clamp_to_u8(r);
        out[p + 1] = clamp_to_u8(g);
        out[p + 2] = clamp_to_u8(b);
        out[p + 3] = clamp_to_u8(a);
    }
}

fn clamp_to_u8(v: f32) -> u8 {
    let clamped = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    (clamped * 255.0 + 0.5) as u8
}

/// IEEE 754 half (1 sign + 5 exponent + 10 mantissa) → f32. Handles
/// zero, subnormals, infinity, and NaN. Hand-rolled to avoid a
/// dependency for one decoder path.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = (h >> 10) & 0x1F;
    let mant = h & 0x3FF;
    let sign_f = if sign == 1 { -1.0_f32 } else { 1.0 };

    if exp == 0 {
        if mant == 0 {
            sign_f * 0.0
        } else {
            // Subnormal: value = (-1)^s * mant/2^10 * 2^-14
            sign_f * (mant as f32) * 2.0_f32.powi(-24)
        }
    } else if exp == 0x1F {
        if mant == 0 { sign_f * f32::INFINITY } else { f32::NAN }
    } else {
        // Normal: value = (-1)^s * (1 + mant/2^10) * 2^(exp-15)
        let exponent = exp as i32 - 15;
        let mantissa = 1.0 + (mant as f32) / 1024.0;
        sign_f * mantissa * 2.0_f32.powi(exponent)
    }
}

//================================================================================
// Block-compressed formats
//================================================================================
//
// All BC walkers share the same outer shape:
//   1. Allocate a 4×4 staging buffer (RGBA8 = 64 bytes; smaller for
//      single/dual-channel BC4/BC5).
//   2. For each block, decode into staging via bcdec_rs.
//   3. Blit the (clipped) staging rectangle into the output mip,
//      converting channel layout for BC4/BC5 (which produce R8 / RG8,
//      not RGBA8).

/// Block-decode into a 4×4 RGBA8 staging buffer, then copy the
/// in-bounds pixels into the destination mip at `(bx, by)`.
fn blit_rgba_block(
    staging: &[u8; 64],
    out: &mut [u8],
    width: u32,
    height: u32,
    bx: u32,
    by: u32,
) {
    let w = width as usize;
    for j in 0..4u32 {
        let py = by * 4 + j;
        if py >= height { break; }
        for i in 0..4u32 {
            let px = bx * 4 + i;
            if px >= width { break; }
            let dst = (py as usize * w + px as usize) * 4;
            let src = ((j * 4 + i) as usize) * 4;
            out[dst..dst + 4].copy_from_slice(&staging[src..src + 4]);
        }
    }
}

/// `dxt1` → BC1. 8-byte block. Direct RGBA8 output from bcdec_rs.
fn decode_bc1(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 8..(block_idx + 1) * 8];
            let mut staging = [0u8; 64];
            bcdec_rs::bc1(block, &mut staging, 16);
            blit_rgba_block(&staging, out, width, height, bx, by);
        }
    }
}

/// `dxt3` → BC2. 16-byte block.
fn decode_bc2(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];
            let mut staging = [0u8; 64];
            bcdec_rs::bc2(block, &mut staging, 16);
            blit_rgba_block(&staging, out, width, height, bx, by);
        }
    }
}

/// `dxt5` → BC3. 16-byte block.
fn decode_bc3(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];
            let mut staging = [0u8; 64];
            bcdec_rs::bc3(block, &mut staging, 16);
            blit_rgba_block(&staging, out, width, height, bx, by);
        }
    }
}

/// `dxt5a`-family → BC4 (single 8-byte block, 8 values, 3-bit
/// indices). The decoded value is splatted into the channels selected
/// by `mask`, with the unselected channels zeroed. Matches TagTool's
/// `DecompressDXT5aX` family — different masks produce `Dxt5a` (all),
/// `Dxt5aMono` (RGB), and `Dxt5aAlpha` (alpha-only).
fn decode_bc4_rgba(
    input: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
    mask: ChannelMask,
) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 8..(block_idx + 1) * 8];
            let mut values = [0u8; 8];
            let indices = unpack_bc4_alpha_block(block, &mut values);
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let bit_offset = 3 * (j * 4 + i);
                    let idx = ((indices >> bit_offset) & 0x07) as usize;
                    let v = values[idx];
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = v & mask.r;
                    out[dst + 1] = v & mask.g;
                    out[dst + 2] = v & mask.b;
                    out[dst + 3] = v & mask.a;
                }
            }
        }
    }
}

/// `dxn` → BC5 (two-channel). Output `(R, G, 128, 255)` matching the
/// normalmap convention V8U8 already uses (B = z = 0.5 implied).
fn decode_bc5(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];
            let mut staging = [0u8; 32]; // 4×4 RG8
            bcdec_rs::bc5(block, &mut staging, 8, false);
            let w = width as usize;
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { break; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { break; }
                    let src = ((j * 4 + i) * 2) as usize;
                    let r = staging[src];
                    let g = staging[src + 1];
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = r;
                    out[dst + 1] = g;
                    out[dst + 2] = 128;
                    out[dst + 3] = 255;
                }
            }
        }
    }
}

/// `dxn_mono_alpha` → custom Halo codec. Each 16-byte block is two
/// BC4-style sub-blocks back to back: `red` carries luminance,
/// `green` carries alpha. Output `(L, L, L, A)`.
///
/// Same numerical work as [`super::dds::decode_dxn_mono_alpha`] but
/// inlined here for the per-mip RGBA8 output convention. The two
/// produce byte-identical pixel data because R = G = B = luminance,
/// so the BGRA / RGBA distinction collapses.
fn decode_dxn_mono_alpha_rgba(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];

            let mut red_values = [0u8; 8];
            let red_indices = unpack_bc4_alpha_block(&block[0..8], &mut red_values);
            let mut green_values = [0u8; 8];
            let green_indices = unpack_bc4_alpha_block(&block[8..16], &mut green_values);

            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let bit_offset = 3 * (j * 4 + i);
                    let red_idx = ((red_indices >> bit_offset) & 0x07) as usize;
                    let green_idx = ((green_indices >> bit_offset) & 0x07) as usize;
                    let r = red_values[red_idx];
                    let g = green_values[green_idx];
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = r;
                    out[dst + 1] = r;
                    out[dst + 2] = r;
                    out[dst + 3] = g;
                }
            }
        }
    }
}

/// `r5g6b5`: u16 LE with bits `RRRRR GGGGGG BBBBB`. Bit-replication
/// expansion (`r5 → r8 = (r5 << 3) | (r5 >> 2)`, etc.) matches what
/// hardware samplers do.
fn decode_r5g6b5(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let r = ((v >> 11) & 0x1F) as u8;
        let g = ((v >> 5) & 0x3F) as u8;
        let b = (v & 0x1F) as u8;
        let p = i * 4;
        out[p] = (r << 3) | (r >> 2);
        out[p + 1] = (g << 2) | (g >> 4);
        out[p + 2] = (b << 3) | (b >> 2);
        out[p + 3] = 255;
    }
}

/// `a1r5g5b5`: u16 LE with bits `A RRRRR GGGGG BBBBB`. The single
/// alpha bit expands to `0` or `255` (1 → fully opaque).
fn decode_a1r5g5b5(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let a = ((v >> 15) & 0x1) as u8;
        let r = ((v >> 10) & 0x1F) as u8;
        let g = ((v >> 5) & 0x1F) as u8;
        let b = (v & 0x1F) as u8;
        let p = i * 4;
        out[p] = (r << 3) | (r >> 2);
        out[p + 1] = (g << 3) | (g >> 2);
        out[p + 2] = (b << 3) | (b >> 2);
        out[p + 3] = a * 0xFF;
    }
}

/// `a4r4g4b4 font`: same 16-bit storage as [`decode_a4r4g4b4`] but
/// engine treats it as palette index → grayscale-with-alpha. Mirrors
/// TagTool's `DecodeP8`: each source byte is replicated into RGB
/// with full alpha.
fn decode_a4r4g4b4_font(input: &[u8], out: &mut [u8]) {
    for (i, &b) in input.iter().enumerate() {
        let p = i * 4;
        out[p] = b;
        out[p + 1] = b;
        out[p + 2] = b;
        out[p + 3] = 255;
    }
}

/// `g8b8`: unsigned 16-bit two-channel. Memory `[G, B]` per the
/// schema enum name. Output `(0, G, B, 255)`.
fn decode_g8b8(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let p = i * 4;
        out[p] = 0;
        out[p + 1] = chunk[0];
        out[p + 2] = chunk[1];
        out[p + 3] = 255;
    }
}

/// `l16`: 16-bit unsigned luminance LE. Truncate to high byte and
/// replicate into RGB; alpha = 255.
fn decode_l16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let v = u16::from_le_bytes([chunk[0], chunk[1]]);
        let y = (v >> 8) as u8;
        let p = i * 4;
        out[p] = y;
        out[p + 1] = y;
        out[p + 2] = y;
        out[p + 3] = 255;
    }
}

/// `16f_mono`: single half-float per pixel. Clamp to `[0, 1]` and
/// replicate into RGB; alpha = 255.
fn decode_f16_mono(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let f = half_to_f32(u16::from_le_bytes([chunk[0], chunk[1]]));
        let v = clamp_to_u8(f);
        let p = i * 4;
        out[p] = v;
        out[p + 1] = v;
        out[p + 2] = v;
        out[p + 3] = 255;
    }
}

/// `16f_red`: single half-float per pixel routed to the red channel.
/// Other channels zero; alpha = 255.
fn decode_f16_red(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(2).enumerate() {
        let f = half_to_f32(u16::from_le_bytes([chunk[0], chunk[1]]));
        let p = i * 4;
        out[p] = clamp_to_u8(f);
        out[p + 1] = 0;
        out[p + 2] = 0;
        out[p + 3] = 255;
    }
}

/// `a2r10g10b10`: u32 LE with bits `AA RRRRRRRRRR GGGGGGGGGG
/// BBBBBBBBBB`. Top 8 bits of each 10-bit channel; alpha bit
/// pair expands to `0/85/170/255`.
fn decode_a2r10g10b10(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let v = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let a = ((v >> 30) & 0x3) as u8;
        let r = ((v >> 20) & 0x3FF) as u16;
        let g = ((v >> 10) & 0x3FF) as u16;
        let b = (v & 0x3FF) as u16;
        let p = i * 4;
        // Take the top 8 bits of each 10-bit channel.
        out[p] = (r >> 2) as u8;
        out[p + 1] = (g >> 2) as u8;
        out[p + 2] = (b >> 2) as u8;
        // 2-bit alpha: 0=0, 1=85, 2=170, 3=255 (= a * 85).
        out[p + 3] = a.saturating_mul(85);
    }
}

/// `v16u16`: signed 32-bit two-channel normalmap. Memory `[U_lo, U_hi,
/// V_lo, V_hi]` as two i16s. Bias by `+32768` and truncate to high
/// byte; output `(V, U, 128, 255)` matching the V8U8 convention.
fn decode_v16u16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let u = i16::from_le_bytes([chunk[0], chunk[1]]) as i32 + 32768;
        let v = i16::from_le_bytes([chunk[2], chunk[3]]) as i32 + 32768;
        let p = i * 4;
        out[p] = (v >> 8) as u8;
        out[p + 1] = (u >> 8) as u8;
        out[p + 2] = 128;
        out[p + 3] = 255;
    }
}

/// `r16g16`: unsigned 32-bit two-channel. Memory `[R_lo, R_hi, G_lo,
/// G_hi]`. Truncate each to high byte; B = 0, alpha = 255.
fn decode_r16g16(input: &[u8], out: &mut [u8]) {
    for (i, chunk) in input.chunks_exact(4).enumerate() {
        let r = u16::from_le_bytes([chunk[0], chunk[1]]);
        let g = u16::from_le_bytes([chunk[2], chunk[3]]);
        let p = i * 4;
        out[p] = (r >> 8) as u8;
        out[p + 1] = (g >> 8) as u8;
        out[p + 2] = 0;
        out[p + 3] = 255;
    }
}

/// `dxt3a`-family: 8-byte block carrying 16 × 4-bit alpha values.
/// Each 4-bit value expands to 8-bit via `* 17` (bit replication),
/// then splatted into channels by `mask`. Different masks produce
/// the `Dxt3a` (all), `Dxt3aMono` (RGB), and `Dxt3aAlpha` (alpha-only)
/// variants. Mirrors TagTool's `DecompressDXT3aX`.
fn decode_dxt3a(input: &[u8], width: u32, height: u32, out: &mut [u8], mask: ChannelMask) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 8..(block_idx + 1) * 8];
            let alpha_data = u64::from_le_bytes([
                block[0], block[1], block[2], block[3],
                block[4], block[5], block[6], block[7],
            ]);
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let shift = 4 * (4 * j + i);
                    let nibble = ((alpha_data >> shift) & 0xF) as u8;
                    let value = nibble.wrapping_mul(17);
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = value & mask.r;
                    out[dst + 1] = value & mask.g;
                    out[dst + 2] = value & mask.b;
                    out[dst + 3] = value & mask.a;
                }
            }
        }
    }
}

/// `dxt3a_1111`: same 8-byte block as `Dxt3a` but the 4 bits per
/// pixel are 4 binary channels (R, G, B, A). Each bit expands to
/// `0` or `255`.
fn decode_dxt3a_1111(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 8..(block_idx + 1) * 8];
            let bits = u64::from_le_bytes([
                block[0], block[1], block[2], block[3],
                block[4], block[5], block[6], block[7],
            ]);
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let shift = 4 * (4 * j + i);
                    let nibble = ((bits >> shift) & 0xF) as u8;
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = ((nibble >> 0) & 1) * 255;
                    out[dst + 1] = ((nibble >> 1) & 1) * 255;
                    out[dst + 2] = ((nibble >> 2) & 1) * 255;
                    out[dst + 3] = ((nibble >> 3) & 1) * 255;
                }
            }
        }
    }
}

/// `dxt5nm`: BC3-shaped 16-byte block as a normal map. BC4 alpha
/// half carries the X (red) component, the color-block green carries
/// Y, and Z (blue) is reconstructed from `sqrt(1 - x² - y²)`. Alpha
/// is forced to `255`.
fn decode_dxt5nm(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];
            let mut alpha_values = [0u8; 8];
            let alpha_indices = unpack_bc4_alpha_block(&block[0..8], &mut alpha_values);
            let mut staging = [0u8; 64];
            bcdec_rs::bc1(&block[8..16], &mut staging, 16);
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let bit_offset = 3 * (j * 4 + i);
                    let a_idx = ((alpha_indices >> bit_offset) & 0x07) as usize;
                    let r = alpha_values[a_idx];
                    let g = staging[((j * 4 + i) as usize) * 4 + 1];
                    let z = calculate_normal_z(r, g);
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = r;
                    out[dst + 1] = g;
                    out[dst + 2] = z;
                    out[dst + 3] = 255;
                }
            }
        }
    }
}

/// `ctx1`: BC1-shaped 8-byte block carrying two 2-channel endpoints
/// (R/G as 8/8 instead of 5/6/5 RGB) and 32 bits of 2-bit per-pixel
/// indices into 4 lerped endpoints. Z is reconstructed from X/Y;
/// alpha = 255. Mirrors TagTool's `DecompressCTX1`.
fn decode_ctx1(input: &[u8], width: u32, height: u32, out: &mut [u8]) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 8..(block_idx + 1) * 8];

            // Endpoints: 2 × (R, G) pairs at the start. TagTool reads
            // `(R = block[1], G = block[0])` then the second pair the
            // same way — the byte order matches Halo's `g8b8`-style
            // packing.
            let mut endpoints: [[u8; 2]; 4] = [[0; 2]; 4];
            endpoints[0] = [block[1], block[0]];
            endpoints[1] = [block[3], block[2]];
            // 2/3 endpoint0 + 1/3 endpoint1 and the inverse.
            endpoints[2] = [
                ((2 * endpoints[0][0] as u32 + endpoints[1][0] as u32) / 3) as u8,
                ((2 * endpoints[0][1] as u32 + endpoints[1][1] as u32) / 3) as u8,
            ];
            endpoints[3] = [
                ((endpoints[0][0] as u32 + 2 * endpoints[1][0] as u32) / 3) as u8,
                ((endpoints[0][1] as u32 + 2 * endpoints[1][1] as u32) / 3) as u8,
            ];

            let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);

            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let shift = 2 * (4 * j + i);
                    let idx = ((indices >> shift) & 0x3) as usize;
                    let r = endpoints[idx][0];
                    let g = endpoints[idx][1];
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = r;
                    out[dst + 1] = g;
                    out[dst + 2] = calculate_normal_z(r, g);
                    out[dst + 3] = 255;
                }
            }
        }
    }
}

/// `dxt5_red/green/blue` (Reach+): BC3-shaped 16-byte block where
/// the BC4 alpha sub-block carries the single channel of interest
/// and the BC1 color sub-block is ignored. Routes the decoded value
/// to the channel index `target` (0=R, 1=G, 2=B). Other channels
/// zero; alpha = 255.
fn decode_bc3_single_channel(
    input: &[u8],
    width: u32,
    height: u32,
    out: &mut [u8],
    target: usize,
) {
    let blocks_w = ((width + 3) / 4).max(1);
    let blocks_h = ((height + 3) / 4).max(1);
    let w = width as usize;
    for by in 0..blocks_h {
        for bx in 0..blocks_w {
            let block_idx = (by * blocks_w + bx) as usize;
            let block = &input[block_idx * 16..(block_idx + 1) * 16];
            let mut values = [0u8; 8];
            let indices = unpack_bc4_alpha_block(&block[0..8], &mut values);
            for j in 0..4u32 {
                let py = by * 4 + j;
                if py >= height { continue; }
                for i in 0..4u32 {
                    let px = bx * 4 + i;
                    if px >= width { continue; }
                    let bit_offset = 3 * (j * 4 + i);
                    let idx = ((indices >> bit_offset) & 0x07) as usize;
                    let v = values[idx];
                    let dst = (py as usize * w + px as usize) * 4;
                    out[dst] = 0;
                    out[dst + 1] = 0;
                    out[dst + 2] = 0;
                    out[dst + target] = v;
                    out[dst + 3] = 255;
                }
            }
        }
    }
}

/// Reconstruct the Z (blue) component of a unit normal from its X
/// (red) and Y (green) components, both stored as unsigned bytes in
/// `[-1, +1]` range. Mirrors `BitmapUtils.CalculateNormalZ` in
/// TagTool: `z = sqrt(clamp(1 - x² - y², 0, 1))`, then re-biased.
fn calculate_normal_z(r: u8, g: u8) -> u8 {
    let x = (r as f32) / 127.5 - 1.0;
    let y = (g as f32) / 127.5 - 1.0;
    let z = (1.0 - x * x - y * y).max(0.0).min(1.0).sqrt();
    ((z + 1.0) * 127.5 + 0.5) as u8
}

/// 8-byte BC4-style alpha sub-block: 2 endpoint bytes + 6 bytes of
/// 3-bit indices. Fills `values` with the 8-entry palette and
/// returns the 48-bit index field as a `u64`. (Mirror of the helper
/// in [`super::dds`].)
fn unpack_bc4_alpha_block(block: &[u8], values: &mut [u8; 8]) -> u64 {
    let v0 = block[0] as u32;
    let v1 = block[1] as u32;
    values[0] = v0 as u8;
    values[1] = v1 as u8;

    if v0 > v1 {
        for i in 0..6u32 {
            values[(2 + i) as usize] = (((6 - i) * v0 + (1 + i) * v1) / 7) as u8;
        }
    } else {
        for i in 0..4u32 {
            values[(2 + i) as usize] = (((4 - i) * v0 + (1 + i) * v1) / 5) as u8;
        }
        values[6] = 0;
        values[7] = 255;
    }

    (block[2] as u64)
        | ((block[3] as u64) << 8)
        | ((block[4] as u64) << 16)
        | ((block[5] as u64) << 24)
        | ((block[6] as u64) << 32)
        | ((block[7] as u64) << 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(r: u8, g: u8, b: u8, a: u8) -> [u8; 4] { [r, g, b, a] }

    #[test]
    fn a8_white_with_alpha() {
        let out = decode_to_rgba8(BitmapFormat::A8, 4, 1, &[0x00, 0x80, 0xFF, 0x40]).unwrap();
        assert_eq!(&out[0..4], &rgba(255, 255, 255, 0x00));
        assert_eq!(&out[4..8], &rgba(255, 255, 255, 0x80));
        assert_eq!(&out[8..12], &rgba(255, 255, 255, 0xFF));
        assert_eq!(&out[12..16], &rgba(255, 255, 255, 0x40));
    }

    #[test]
    fn y8_replicates_to_rgb() {
        let out = decode_to_rgba8(BitmapFormat::Y8, 2, 1, &[0x00, 0x80]).unwrap();
        assert_eq!(&out[0..4], &rgba(0, 0, 0, 255));
        assert_eq!(&out[4..8], &rgba(0x80, 0x80, 0x80, 255));
    }

    #[test]
    fn r8_red_only() {
        let out = decode_to_rgba8(BitmapFormat::R8, 1, 1, &[0xCC]).unwrap();
        assert_eq!(&out, &rgba(0xCC, 0, 0, 255));
    }

    #[test]
    fn ay8_replicates_to_all_four() {
        let out = decode_to_rgba8(BitmapFormat::Ay8, 1, 1, &[0x40]).unwrap();
        assert_eq!(&out, &rgba(0x40, 0x40, 0x40, 0x40));
    }

    #[test]
    fn a8y8_y_in_rgb_a_in_alpha() {
        // bytes [Y, A] in memory
        let out = decode_to_rgba8(BitmapFormat::A8y8, 1, 1, &[0x80, 0x40]).unwrap();
        assert_eq!(&out, &rgba(0x80, 0x80, 0x80, 0x40));
    }

    #[test]
    fn a4r4g4b4_nibble_replication() {
        // u16 LE = 0xFEDC → AAAA=0xF RRRR=0xE GGGG=0xD BBBB=0xC
        let out = decode_to_rgba8(BitmapFormat::A4r4g4b4, 1, 1, &[0xDC, 0xFE]).unwrap();
        assert_eq!(&out, &rgba(0xEE, 0xDD, 0xCC, 0xFF));
    }

    #[test]
    fn x8r8g8b8_alpha_forced_to_255() {
        // Memory [B, G, R, X]
        let out = decode_to_rgba8(BitmapFormat::X8r8g8b8, 1, 1, &[0x10, 0x20, 0x30, 0xAA]).unwrap();
        assert_eq!(&out, &rgba(0x30, 0x20, 0x10, 0xFF));
    }

    #[test]
    fn a8r8g8b8_bgra_to_rgba() {
        // Memory [B, G, R, A]
        let out = decode_to_rgba8(BitmapFormat::A8r8g8b8, 1, 1, &[0x10, 0x20, 0x30, 0x40]).unwrap();
        assert_eq!(&out, &rgba(0x30, 0x20, 0x10, 0x40));
    }

    #[test]
    fn v8u8_signed_bias_to_unsigned() {
        // Memory [U, V] = [-1, 0] → expected (V+128, U+128, 128, 255) = (128, 127, 128, 255)
        let out = decode_to_rgba8(BitmapFormat::V8u8, 1, 1, &[0xFF, 0x00]).unwrap();
        assert_eq!(&out, &rgba(128, 127, 128, 255));

        // Memory [U=+127, V=-128] → (0, 255, 128, 255)
        let out = decode_to_rgba8(BitmapFormat::V8u8, 1, 1, &[0x7F, 0x80]).unwrap();
        assert_eq!(&out, &rgba(0, 255, 128, 255));
    }

    #[test]
    fn q8w8v8u8_signed_bias_per_channel() {
        // Memory [U, V, W, Q] = [-128, 0, +127, +1] → (0, 128, 255, 129)
        let out = decode_to_rgba8(BitmapFormat::Q8w8v8u8, 1, 1, &[0x80, 0x00, 0x7F, 0x01]).unwrap();
        assert_eq!(&out, &rgba(0, 128, 255, 129));
    }

    #[test]
    fn a16b16g16r16_high_byte_only() {
        // R=0xFF00, G=0x8000, B=0x0100, A=0xFFFF (LE bytes)
        let bytes = [0x00, 0xFF, 0x00, 0x80, 0x00, 0x01, 0xFF, 0xFF];
        let out = decode_to_rgba8(BitmapFormat::A16b16g16r16, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &rgba(0xFF, 0x80, 0x01, 0xFF));
    }

    #[test]
    fn signedr16g16b16a16_bias_by_32768() {
        // R=-32768 → 0, G=0 → 128, B=+32767 → 255, A=-1 → 127
        let r = (-32768i16).to_le_bytes();
        let g = 0i16.to_le_bytes();
        let b = 32767i16.to_le_bytes();
        let a = (-1i16).to_le_bytes();
        let bytes = [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]];
        let out = decode_to_rgba8(BitmapFormat::Signedr16g16b16a16, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &rgba(0, 128, 255, 127));
    }

    #[test]
    fn abgrfp16_clamp_and_scale() {
        // Half-float encodings:
        //   1.0 = 0x3C00, 0.0 = 0x0000, 0.5 = 0x3800, 2.0 = 0x4000 (clamps to 1.0)
        let bytes = [
            0x00, 0x3C, // R = 1.0
            0x00, 0x38, // G = 0.5
            0x00, 0x00, // B = 0.0
            0x00, 0x40, // A = 2.0 → clamps to 1.0
        ];
        let out = decode_to_rgba8(BitmapFormat::Abgrfp16, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &rgba(255, 128, 0, 255));
    }

    #[test]
    fn abgrfp32_clamp_and_scale() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1.0_f32.to_le_bytes());   // R
        bytes.extend_from_slice(&0.5_f32.to_le_bytes());   // G
        bytes.extend_from_slice(&(-0.5_f32).to_le_bytes()); // B → clamps to 0
        bytes.extend_from_slice(&3.0_f32.to_le_bytes());   // A → clamps to 1
        let out = decode_to_rgba8(BitmapFormat::Abgrfp32, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &rgba(255, 128, 0, 255));
    }

    #[test]
    fn input_too_short_returns_oob() {
        let err = decode_to_rgba8(BitmapFormat::A8r8g8b8, 2, 2, &[0u8; 12]);
        assert!(matches!(err, Err(BitmapError::PixelSliceOutOfBounds { .. })));
    }

    // --- BC sanity tests --------------------------------------------------
    //
    // We're testing our walker / dispatch / channel-mapping rather
    // than bcdec_rs's correctness — the upstream library has its
    // own test suite. So each test crafts a block whose decoded
    // output is trivially predictable (uniform color) and checks
    // we splat it across all 16 pixels with the right channel order.

    /// BC1 block with both endpoints = solid red (R5G6B5 = 0xF800).
    /// Expect every pixel to be opaque red.
    #[test]
    fn bc1_solid_red_block() {
        let mut block = [0u8; 8];
        block[0..2].copy_from_slice(&0xF800u16.to_le_bytes()); // color0
        block[2..4].copy_from_slice(&0xF800u16.to_le_bytes()); // color1
        // index bits all 0 → palette[0] = color0 = red
        let out = decode_to_rgba8(BitmapFormat::Dxt1, 4, 4, &block).unwrap();
        for i in 0..16 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0xFF, 0x00, 0x00, 0xFF]);
        }
    }

    /// BC4 block (DXT5A) with both endpoints = 0x80. TagTool splats
    /// the decoded value into all four channels (`ChannelMask::ALL`),
    /// so alpha matches the source value rather than being forced to
    /// 0xFF — match that.
    #[test]
    fn bc4_dxt5a_replicates_to_all_four() {
        let mut block = [0u8; 8];
        block[0] = 0x80;
        block[1] = 0x80;
        let out = decode_to_rgba8(BitmapFormat::Dxt5a, 4, 4, &block).unwrap();
        for i in 0..16 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x80, 0x80, 0x80, 0x80]);
        }
    }

    /// `Dxt5aMono` splats the decoded value into RGB and zeros alpha.
    #[test]
    fn bc4_dxt5a_mono_rgb_only() {
        let mut block = [0u8; 8];
        block[0] = 0x60;
        block[1] = 0x60;
        let out = decode_to_rgba8(BitmapFormat::Dxt5aMono, 2, 2, &block).unwrap();
        for i in 0..4 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x60, 0x60, 0x60, 0x00]);
        }
    }

    /// `Dxt5aAlpha` splats the decoded value into alpha only.
    #[test]
    fn bc4_dxt5a_alpha_only() {
        let mut block = [0u8; 8];
        block[0] = 0xA0;
        block[1] = 0xA0;
        let out = decode_to_rgba8(BitmapFormat::Dxt5aAlpha, 2, 2, &block).unwrap();
        for i in 0..4 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x00, 0x00, 0x00, 0xA0]);
        }
    }

    /// `Dxt3a` block where every nibble = 0xF expands to `0xF * 17`
    /// = 0xFF replicated to all four channels.
    #[test]
    fn dxt3a_all_max_nibbles() {
        let block = [0xFFu8; 8];
        let out = decode_to_rgba8(BitmapFormat::Dxt3a, 4, 4, &block).unwrap();
        for i in 0..16 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0xFF, 0xFF, 0xFF, 0xFF]);
        }
    }

    /// `Dxt3a1111` decomposes each 4-bit value into 4 binary
    /// channels. With nibble `0b1010`, we expect (R=0, G=255, B=0,
    /// A=255).
    #[test]
    fn dxt3a_1111_unpacks_to_four_binary_channels() {
        let block = [0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA, 0xAA];
        let out = decode_to_rgba8(BitmapFormat::Dxt3a1111, 2, 2, &block).unwrap();
        for i in 0..4 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x00, 0xFF, 0x00, 0xFF]);
        }
    }

    /// `R5g6b5` 0xF800 = pure red (R=31, G=0, B=0) → (0xFF, 0, 0, 255).
    #[test]
    fn r5g6b5_pure_red() {
        let bytes = [0x00, 0xF8]; // LE u16 = 0xF800
        let out = decode_to_rgba8(BitmapFormat::R5g6b5, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &[0xFF, 0x00, 0x00, 0xFF]);
    }

    /// `A1r5g5b5` 0x8000 = (A=1, R=0, G=0, B=0) → (0, 0, 0, 255).
    #[test]
    fn a1r5g5b5_alpha_only() {
        let bytes = [0x00, 0x80]; // LE u16 = 0x8000
        let out = decode_to_rgba8(BitmapFormat::A1r5g5b5, 1, 1, &bytes).unwrap();
        assert_eq!(&out, &[0x00, 0x00, 0x00, 0xFF]);
    }

    /// `Ctx1` with both endpoints set to mid-range (R=128, G=128)
    /// decodes to a uniform `(128, 128, 128, 255)` after Z-recovery
    /// (X=Y=0 → Z=1 → byte 0xFF... but only matters if endpoints
    /// differ; for equal endpoints the index field is irrelevant).
    #[test]
    fn ctx1_uniform_mid_range_endpoints() {
        // Endpoint0: bytes[0]=G=0x80, bytes[1]=R=0x80
        // Endpoint1: bytes[2]=G=0x80, bytes[3]=R=0x80
        // Indices: any value (all 4 endpoints decode to same color)
        let block = [0x80, 0x80, 0x80, 0x80, 0x00, 0x00, 0x00, 0x00];
        let out = decode_to_rgba8(BitmapFormat::Ctx1, 2, 2, &block).unwrap();
        let z = calculate_normal_z(0x80, 0x80);
        for i in 0..4 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x80, 0x80, z, 0xFF]);
        }
    }

    /// BC5 block (DXN) with red sub-block = 0x40 and green sub-block
    /// = 0xC0. Expect (R=0x40, G=0xC0, B=128, A=255) at every pixel.
    #[test]
    fn bc5_dxn_two_channel_with_neutral_blue() {
        let mut block = [0u8; 16];
        // Red sub-block: both endpoints = 0x40
        block[0] = 0x40;
        block[1] = 0x40;
        // Green sub-block: both endpoints = 0xC0
        block[8] = 0xC0;
        block[9] = 0xC0;
        let out = decode_to_rgba8(BitmapFormat::Dxn, 4, 4, &block).unwrap();
        for i in 0..16 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0x40, 0xC0, 0x80, 0xFF]);
        }
    }

    /// `dxn_mono_alpha` block with luminance=0xA0, alpha=0x60.
    /// Expect (0xA0, 0xA0, 0xA0, 0x60).
    #[test]
    fn dxn_mono_alpha_lum_in_rgb_alpha_in_alpha() {
        let mut block = [0u8; 16];
        // Red sub-block (luminance) endpoints
        block[0] = 0xA0;
        block[1] = 0xA0;
        // Green sub-block (alpha) endpoints
        block[8] = 0x60;
        block[9] = 0x60;
        let out = decode_to_rgba8(BitmapFormat::DxnMonoAlpha, 4, 4, &block).unwrap();
        for i in 0..16 {
            assert_eq!(&out[i * 4..i * 4 + 4], &[0xA0, 0xA0, 0xA0, 0x60]);
        }
    }

    /// Sub-4-pixel mip (1×1) for a BC format: input is still one
    /// 4×4 block, output should hold one valid pixel.
    #[test]
    fn bc1_1x1_mip_single_pixel_decodes() {
        let mut block = [0u8; 8];
        block[0..2].copy_from_slice(&0x07E0u16.to_le_bytes()); // R5G6B5 green
        block[2..4].copy_from_slice(&0x07E0u16.to_le_bytes());
        let out = decode_to_rgba8(BitmapFormat::Dxt1, 1, 1, &block).unwrap();
        assert_eq!(out.len(), 4);
        assert_eq!(&out, &[0x00, 0xFF, 0x00, 0xFF]);
    }
}
