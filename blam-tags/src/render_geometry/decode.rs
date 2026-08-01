//! Per-format vertex decoders for the Halo 4 X360 monolithic build.
//!
//! Layouts and decoder semantics are **binary-verified** against the
//! `midnight_tag_test.xex` engine by reading the rasterizer's vertex
//! declaration table (`rasterizer_vertex_get_declaration @
//! 0x83129500`, table at `0x84dfdb50`, 105 bytes per entry).
//!
//! Each declaration entry is 1 declaration_index byte followed by up
//! to 26 four-byte register mappings. Each mapping is
//! `{ attr: u8, fmt: u8, _pad: u16 }` — array terminator has
//! `attr = 0xff`. The buffer layout is the concatenation of each
//! mapping's format size, in mapping order.
//!
//! Engine references used to anchor the decode:
//! - `extract_rigid_vertex_data_compressed @ 0x82ce1868` (validates
//!   `decl[36]` mapping order/sizes).
//! - `extract_rigid_vertex_data @ 0x82ce0cd0` (`decl[2]`).
//! - `extract_world_vertex_data @ 0x82ce1a48` (`decl[1]`).
//! - `uncompress_UDec4_to_real_vector4d_normalized @ 0x82c736f8`.
//! - `XMLoadDHenN3` (D3DDECLTYPE_DHEN3N decoder) `@ 0x82c739e8` with
//!   scales `[1/511, 1/1023, 1/1023]` — DHEN3N is **signed 10/11/11**.
//!
//! ## H4 vertex-format enum
//!
//! Format byte (mapping byte 1) maps to (matches Ares' enum + H4
//! extension at 19):
//!
//! | fmt | name in source         | size | description                 |
//! |-----|------------------------|------|-----------------------------|
//! | 0   | real                   | 4    | one f32 BE                   |
//! | 1   | real_vector2d          | 8    | two f32 BE                   |
//! | 2   | real_vector3d          | 12   | three f32 BE                 |
//! | 3   | real_vector4d          | 16   | four f32 BE                  |
//! | 4   | byte_vector4d          | 4    | four u8                      |
//! | 5   | byte_vector4d_normalized | 4  | four u8 / 255                |
//! | 6   | byte_argb_color        | 4    | D3DCOLOR (ARGB u8 per byte)  |
//! | 7   | short_vector2d         | 4    | two i16 BE                   |
//! | 8   | short_vector4d         | 8    | four i16 BE                  |
//! | 9   | short_vector2d_normalized | 4 | two i16 BE / 32767            |
//! | 10  | short_vector4d_normalized | 8 | four i16 BE / 32767           |
//! | 11  | word_vector2d_normalized | 4  | two u16 BE / 65535           |
//! | 12  | word_vector4d_normalized | 8  | four u16 BE / 65535          |
//! | 13  | 10_11_11               | 4    | unsigned 10/11/11 packed     |
//! | 14  | 10_11_11_normalized    | 4    | **signed** 10/11/11 / 511,1023,1023 (DHEN3N) |
//! | 15  | real16_vector2d        | 4    | two binary16 BE              |
//! | 16  | real16_vector4d        | 8    | four binary16 BE             |
//! | 17  | 10_10_10_normalized    | 4    | **signed** 10/10/10/2 / 511 (DEC3N) |
//! | 19  | (H4 ext.: 10_10_10_2_normalized) | 4 | unsigned 10/10/10/2 / 1023,3 (UDec4N) |
//!
//! ## H4 attribute (semantic) enum
//!
//! Mapping byte 0 (attribute) values used:
//!
//! | attr | meaning       |
//! |------|---------------|
//! | 0    | POSITION      |
//! | 1    | BLEND_INDICES |
//! | 2    | BLEND_WEIGHTS |
//! | 3    | TEXCOORD      |
//! | 4    | NORMAL        |
//! | 6    | TANGENT       |
//! | 0xff | END (terminator) |
//!
//! ## Format usage notes
//!
//! Tangent is decoded as `DEC3N` for every format that carries one;
//! binormal is *never* stored in the buffer — the engine reconstructs
//! it at render time via `binormal = normalize(cross(normal, tangent))
//! * sign`, where the sign bit lives in the W slot of UDec4N
//! positions (`pos.W * 2/3 - 1` ≈ `[-1, +1]`) or UShort4N positions
//! (`pos.W * 2 - 1`). We synthesize binormal via the same cross product
//! using `+1` as the sign (no info loss for the JMS export path —
//! downstream exporters recompute face normals anyway).

use super::MeshVertexType;

/// Decoded author-format vertex. Layout matches the fields the JMS
/// exporter reads from `raw_vertex_block` (`crate::jms::read_vertex`).
#[derive(Debug, Clone, Default)]
pub struct AuthorVertex {
    /// Object-space position. Compressed `[0, 1]` form for formats
    /// where the engine bounds-decompresses (UDec4N, UShort4N);
    /// world-space float for formats with explicit Float positions.
    pub position: [f32; 3],
    /// Unit normal, `[-1, +1]` per component.
    pub normal: [f32; 3],
    /// Unit tangent, `[-1, +1]` per component.
    pub tangent: [f32; 3],
    /// Unit binormal, `[-1, +1]` per component (reconstructed via
    /// cross(normal, tangent) when the buffer carries a tangent).
    pub binormal: [f32; 3],
    /// Texture coordinates. Compressed `[0, 1]` form for UShort2N
    /// formats (decompressed downstream); world-space for half2 / float2.
    pub texcoord: [f32; 2],
    /// Up to 4 `(node_index, weight)` pairs (skinned formats only).
    pub node_sets: Vec<(i16, f32)>,
}

/// Decode error variants. The dispatcher returns these; the
/// hydration layer panics on `Unsupported` / `StrideMismatch`.
#[derive(Debug)]
pub enum VertexDecodeError {
    StrideMismatch { expected: u16, actual: u16 },
    TruncatedBuffer { needed: usize, available: usize },
    Unsupported(MeshVertexType),
}

impl std::fmt::Display for VertexDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StrideMismatch { expected, actual } => write!(
                f, "vertex stride mismatch: expected {expected}, got {actual}",
            ),
            Self::TruncatedBuffer { needed, available } => write!(
                f, "vertex buffer truncated: needed {needed} bytes, have {available}",
            ),
            Self::Unsupported(t) => {
                write!(f, "no decoder for mesh vertex type `{}`", t.schema_name())
            }
        }
    }
}

impl std::error::Error for VertexDecodeError {}

/// Decode a vertex buffer into a list of [`AuthorVertex`].
pub fn decode_vertex_buffer(
    vertex_type: MeshVertexType,
    vertex_count: u32,
    stride: u16,
    bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    let need = (vertex_count as usize) * (stride as usize);
    if bytes.len() < need {
        return Err(VertexDecodeError::TruncatedBuffer {
            needed: need,
            available: bytes.len(),
        });
    }

    match vertex_type {
        MeshVertexType::RigidCompressed => decode_rigid_compressed(vertex_count, stride, bytes),
        MeshVertexType::Rigid => decode_rigid(vertex_count, stride, bytes),
        MeshVertexType::Skinned => decode_skinned(vertex_count, stride, bytes),
        MeshVertexType::World => decode_world(vertex_count, stride, bytes),
        MeshVertexType::Decorator => decode_decorator(vertex_count, stride, bytes),
        MeshVertexType::PositionOnly | MeshVertexType::PositionOnlyAlt => {
            decode_position_only(vertex_count, stride, bytes)
        }
        other => Err(VertexDecodeError::Unsupported(other)),
    }
}

//================================================================================
// Per-format decoders (decl-table-verified)
//================================================================================

/// `rigid_compressed` (16 B, decl[36]): pos UDec4N | uv UShort2N |
/// normal DHEN3N | tangent DEC3N.
fn decode_rigid_compressed(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(16, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 16;
        let pos = read_udec4n(&bytes[off..off + 4]);
        let u = read_ushortn(&bytes[off + 4..off + 6]);
        let v = read_ushortn(&bytes[off + 6..off + 8]);
        let normal = read_dhen3n(&bytes[off + 8..off + 12]);
        let tangent = read_dec3n(&bytes[off + 12..off + 16]);
        out.push(AuthorVertex {
            position: [pos[0], pos[1], pos[2]],
            normal,
            tangent,
            binormal: cross(normal, tangent),
            texcoord: [u, v],
            node_sets: Vec::new(),
        });
    }
    Ok(out)
}

/// `rigid` (20 B, decl[2]): pos UShort4N (W skipped) | uv UShort2N |
/// normal DHEN3N | tangent DEC3N.
fn decode_rigid(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(20, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 20;
        let px = read_ushortn(&bytes[off..off + 2]);
        let py = read_ushortn(&bytes[off + 2..off + 4]);
        let pz = read_ushortn(&bytes[off + 4..off + 6]);
        // bytes 6-7 carry the UShort4N W component (tangent sign at
        // runtime). Author format doesn't track a separate tangent
        // sign — binormal cross product is sufficient.
        let u = read_ushortn(&bytes[off + 8..off + 10]);
        let v = read_ushortn(&bytes[off + 10..off + 12]);
        let normal = read_dhen3n(&bytes[off + 12..off + 16]);
        let tangent = read_dec3n(&bytes[off + 16..off + 20]);
        out.push(AuthorVertex {
            position: [px, py, pz],
            normal,
            tangent,
            binormal: cross(normal, tangent),
            texcoord: [u, v],
            node_sets: Vec::new(),
        });
    }
    Ok(out)
}

/// `skinned` (28 B, decl[3]): rigid + UBYTE4 blend_indices + UBYTE4N
/// blend_weights.
fn decode_skinned(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(28, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 28;
        let px = read_ushortn(&bytes[off..off + 2]);
        let py = read_ushortn(&bytes[off + 2..off + 4]);
        let pz = read_ushortn(&bytes[off + 4..off + 6]);
        let u = read_ushortn(&bytes[off + 8..off + 10]);
        let v = read_ushortn(&bytes[off + 10..off + 12]);
        let normal = read_dhen3n(&bytes[off + 12..off + 16]);
        let tangent = read_dec3n(&bytes[off + 16..off + 20]);
        let idx = &bytes[off + 20..off + 24];
        let wts = &bytes[off + 24..off + 28];
        let mut node_sets = Vec::with_capacity(4);
        for k in 0..4 {
            let w = wts[k] as f32 / 255.0;
            if w > 0.0 {
                node_sets.push((idx[k] as i16, w));
            }
        }
        out.push(AuthorVertex {
            position: [px, py, pz],
            normal,
            tangent,
            binormal: cross(normal, tangent),
            texcoord: [u, v],
            node_sets,
        });
    }
    Ok(out)
}

/// `world` (28 B, decl[1]): pos FLOAT4 (W ignored) | uv half2 |
/// normal DHEN3N | tangent DEC3N.
fn decode_world(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(28, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 28;
        let px = read_float(&bytes[off..off + 4]);
        let py = read_float(&bytes[off + 4..off + 8]);
        let pz = read_float(&bytes[off + 8..off + 12]);
        // bytes 12-15: FLOAT4 W component (unused for position)
        let u = read_half(&bytes[off + 16..off + 18]);
        let v = read_half(&bytes[off + 18..off + 20]);
        let normal = read_dhen3n(&bytes[off + 20..off + 24]);
        let tangent = read_dec3n(&bytes[off + 24..off + 28]);
        out.push(AuthorVertex {
            position: [px, py, pz],
            normal,
            tangent,
            binormal: cross(normal, tangent),
            texcoord: [u, v],
            node_sets: Vec::new(),
        });
    }
    Ok(out)
}

/// `decorator` (16 B, decl[24]): pos UShort4N | uv UShort2N |
/// normal DHEN3N. **No tangent** — distinct from rigid_compressed.
fn decode_decorator(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(16, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 16;
        let px = read_ushortn(&bytes[off..off + 2]);
        let py = read_ushortn(&bytes[off + 2..off + 4]);
        let pz = read_ushortn(&bytes[off + 4..off + 6]);
        let u = read_ushortn(&bytes[off + 8..off + 10]);
        let v = read_ushortn(&bytes[off + 10..off + 12]);
        let normal = read_dhen3n(&bytes[off + 12..off + 16]);
        out.push(AuthorVertex {
            position: [px, py, pz],
            normal,
            tangent: [1.0, 0.0, 0.0],
            binormal: [0.0, 1.0, 0.0],
            texcoord: [u, v],
            node_sets: Vec::new(),
        });
    }
    Ok(out)
}

/// `position_only` (12 B, decl[41]): pos FLOAT3 only.
fn decode_position_only(
    count: u32, stride: u16, bytes: &[u8],
) -> Result<Vec<AuthorVertex>, VertexDecodeError> {
    expect_stride(12, stride)?;
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let off = i * 12;
        let px = read_float(&bytes[off..off + 4]);
        let py = read_float(&bytes[off + 4..off + 8]);
        let pz = read_float(&bytes[off + 8..off + 12]);
        out.push(AuthorVertex {
            position: [px, py, pz],
            normal: [0.0, 0.0, 1.0],
            tangent: [1.0, 0.0, 0.0],
            binormal: [0.0, 1.0, 0.0],
            texcoord: [0.0, 0.0],
            node_sets: Vec::new(),
        });
    }
    Ok(out)
}

//================================================================================
// Element readers (D3D / Xenon vertex primitives)
//================================================================================

#[inline]
fn expect_stride(expected: u16, actual: u16) -> Result<(), VertexDecodeError> {
    if actual == expected { Ok(()) } else {
        Err(VertexDecodeError::StrideMismatch { expected, actual })
    }
}

/// `UDec4N` (H4 fmt 19) — 4-byte packed, 10/10/10 unsigned for XYZ
/// (/ 1023) and 2 bits unsigned for W (/ 3).
fn read_udec4n(bytes: &[u8]) -> [f32; 4] {
    let v = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    [
        ((v >> 0)  & 0x3FF) as f32 / 1023.0,
        ((v >> 10) & 0x3FF) as f32 / 1023.0,
        ((v >> 20) & 0x3FF) as f32 / 1023.0,
        ((v >> 30) & 0x3)   as f32 / 3.0,
    ]
}

/// Single `UShortN` (BE u16) normalized to `[0, 1]`.
fn read_ushortn(bytes: &[u8]) -> f32 {
    u16::from_be_bytes([bytes[0], bytes[1]]) as f32 / 65535.0
}

/// `DHEN3N` (H4 fmt 14) — 4-byte packed, **signed** 10/11/11 in 2's
/// complement, divisors 511 / 1023 / 1023. Output in `[-1, +1]`.
fn read_dhen3n(bytes: &[u8]) -> [f32; 3] {
    let v = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let xi = ((v << 22) as i32) >> 22; // bits  0..9  → signed 10
    let yi = ((v << 11) as i32) >> 21; // bits 10..20 → signed 11
    let zi = (v as i32) >> 21;         // bits 21..31 → signed 11
    [xi as f32 / 511.0, yi as f32 / 1023.0, zi as f32 / 1023.0]
}

/// `DEC3N` (H4 fmt 17) — 4-byte packed, **signed** 10/10/10 in 2's
/// complement (high 2 bits typically reserved / tangent sign).
/// Divisor 511 for all three components. Output in `[-1, +1]`.
fn read_dec3n(bytes: &[u8]) -> [f32; 3] {
    let v = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let xi = ((v << 22) as i32) >> 22;  // bits  0..9  signed 10
    let yi = ((v << 12) as i32) >> 22;  // bits 10..19 signed 10
    let zi = ((v << 2)  as i32) >> 22;  // bits 20..29 signed 10
    [xi as f32 / 511.0, yi as f32 / 511.0, zi as f32 / 511.0]
}

/// IEEE 754 binary16 (half) BE → f32.
fn read_half(bytes: &[u8]) -> f32 {
    let h = u16::from_be_bytes([bytes[0], bytes[1]]);
    half_to_f32(h)
}

/// IEEE 754 binary32 (single) BE → f32.
fn read_float(bytes: &[u8]) -> f32 {
    f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Right-hand cross product, for binormal reconstruction.
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Branchless half→single conversion.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h as u32 & 0x8000) << 16;
    let exp = (h as u32 >> 10) & 0x1F;
    let mant = h as u32 & 0x3FF;
    let bits = match exp {
        0 => {
            if mant == 0 { sign }
            else {
                // Subnormal: shift the mantissa up until it is normalized, and
                // let the exponent walk DOWN with it — one step per leading
                // zero, so it goes negative for all but the largest subnormals.
                //
                // Signed because it is signed: as a `u32` this counted down
                // through `wrapping_sub`, and `e + 127` then overflowed for any
                // half needing two or more shifts. Release builds wrapped
                // straight back to the intended value and never noticed;
                // overflow-checked (dev/test) builds panicked. Reading a Halo 4
                // monolithic build's render geometry is what found it.
                let mut m = mant;
                let mut e = 1i32;
                while (m & 0x400) == 0 { m <<= 1; e -= 1; }
                sign | (((e + 127 - 15) as u32) << 23) | ((m & 0x3FF) << 13)
            }
        }
        0x1F => sign | 0x7F800000 | (mant << 13),
        _    => sign | ((exp + 127 - 15) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dhen3n_unit_vectors_are_unit_length() {
        // +X: x=511 (signed 10-bit max), y=0, z=0 → packed = 0x000001FF
        let n = read_dhen3n(&0x000001ffu32.to_be_bytes());
        assert!((n[0] - 1.0).abs() < 1e-3, "got {:?}", n);
        assert!(n[1].abs() < 1e-3 && n[2].abs() < 1e-3, "got {:?}", n);
        // -X: x=-512 → packed = 0x00000200
        let n = read_dhen3n(&0x00000200u32.to_be_bytes());
        assert!((n[0] + 1.0).abs() < 1e-2, "got {:?}", n);
    }

    #[test]
    fn dec3n_unit_vectors_are_unit_length() {
        // +X: x=511, y=0, z=0 → packed = 0x000001FF
        let n = read_dec3n(&0x000001ffu32.to_be_bytes());
        assert!((n[0] - 1.0).abs() < 1e-3, "got {:?}", n);
        assert!(n[1].abs() < 1e-3 && n[2].abs() < 1e-3, "got {:?}", n);
        // +Y: y=511 in bits 10..19 → packed = 0x0007FC00
        // Actually: y * 1024 = 511 * 1024 = 0x7FC00
        let n = read_dec3n(&0x0007fc00u32.to_be_bytes());
        assert!((n[1] - 1.0).abs() < 1e-3, "got {:?}", n);
    }

    #[test]
    fn udec4n_decodes_corners() {
        let z = read_udec4n(&[0; 4]);
        assert_eq!(z, [0.0, 0.0, 0.0, 0.0]);
        // X=1023, W=3
        let v = read_udec4n(&0xC00003FFu32.to_be_bytes());
        assert!((v[0] - 1.0).abs() < 1e-3, "got {:?}", v);
        assert!((v[3] - 1.0).abs() < 1e-3, "got {:?}", v);
    }

    #[test]
    fn half_to_f32_known_values() {
        assert!((half_to_f32(0x3C00) - 1.0).abs() < 1e-6);
        assert!((half_to_f32(0xBC00) + 1.0).abs() < 1e-6);
        assert_eq!(half_to_f32(0x0000), 0.0);
    }

    /// Every subnormal half, against the closed form `mantissa * 2^-24`.
    ///
    /// The old exponent accumulator was unsigned and counted downwards, so any
    /// half needing two or more normalizing shifts — 0x01FF and below, three
    /// quarters of the subnormal range — overflowed `e + 127` and panicked
    /// under `debug-assertions`. Reading a Halo 4 monolithic build's render
    /// geometry hit it; release builds wrapped back to the right answer, which
    /// is exactly why it survived. Exhaustive because the range is 1023 values
    /// wide and the failure was a function of the shift count, not of any one
    /// value.
    #[test]
    fn every_subnormal_half_converts() {
        let scale = 2f32.powi(-24);
        for mant in 1..=0x3FFu16 {
            let expected = mant as f32 * scale;
            assert_eq!(half_to_f32(mant), expected, "half 0x{mant:04X}");
            assert_eq!(
                half_to_f32(mant | 0x8000),
                -expected,
                "negative half 0x{mant:04X}"
            );
        }
        // The extremes: ten shifts (the deepest, and the first to panic) and
        // one shift (the largest subnormal, which always worked).
        assert_eq!(half_to_f32(0x0001), scale);
        assert_eq!(half_to_f32(0x03FF), 1023.0 * scale);
    }

    #[test]
    fn rigid_compressed_origin_has_unit_tangent_normal() {
        // All-zero buffer → position (0,0,0), normal (0,0,0),
        // tangent (0,0,0). Just exercise the path.
        let bytes = vec![0u8; 16];
        let v = decode_vertex_buffer(MeshVertexType::RigidCompressed, 1, 16, &bytes).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].position, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn stride_mismatch_errors() {
        let bytes = vec![0u8; 40];
        let err = decode_vertex_buffer(MeshVertexType::Rigid, 1, 16, &bytes).err();
        assert!(matches!(err, Some(VertexDecodeError::StrideMismatch { .. })));
    }

    #[test]
    fn skinned_decodes_weights_and_indices() {
        let mut bytes = vec![0u8; 28];
        bytes[20] = 0x05;       // blend_idx[0] = 5
        bytes[24] = 0xff;       // blend_weight[0] = 1.0
        let v = decode_vertex_buffer(MeshVertexType::Skinned, 1, 28, &bytes).unwrap();
        assert_eq!(v[0].node_sets.len(), 1);
        assert_eq!(v[0].node_sets[0].0, 5);
        assert!((v[0].node_sets[0].1 - 1.0).abs() < 1e-3);
    }
}
