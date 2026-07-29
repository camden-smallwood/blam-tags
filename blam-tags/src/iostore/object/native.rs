//! Fixed-size native structs, as values rather than bytes.
//!
//! `FVector`, `FGuid`, `FQuat` and their kin serialize as a fixed run of bytes
//! with no property header, so the reader kept them as
//! `PropValue::Native(Vec<u8>)` and decoded them on demand — `MeshTransform`
//! pulls three of them apart by hand. They round-trip perfectly that way, which
//! is exactly why every byte-oriented gate was blind to the fact that nothing
//! had *understood* them: 41.9 MB of the corpus.
//!
//! The sizes were already known ([`native_struct_size`](super::structs::native_struct_size));
//! what was missing is the field layout behind each one. Note that size does not
//! determine layout — `FVector2D` (2 × f64), `FLinearColor` (4 × f32) and
//! `FGuid` (4 × u32) are all sixteen bytes and all different.
//!
//! [`NativeStruct::Opaque`] is the escape hatch that lets this land
//! incrementally: a struct whose size is known but whose fields are not yet
//! modeled keeps its bytes, and `ce_decode_coverage` still counts it as untyped.

use anyhow::{bail, Result};
use std::sync::Arc;

use super::structs::native_struct_size;

/// One decoded fixed-size native struct.
///
/// Variants are named for the *layout*, not for every type that uses it —
/// `FVector` and `FRotator` are both three doubles, and which one a value is
/// comes from the property's declared type, which the writer always has.
#[derive(Debug, Clone, PartialEq)]
pub enum NativeStruct {
    /// `FVector`, `FRotator` — UE5 large-world coordinates, so f64.
    Vec3d([f64; 3]),
    /// `FVector4`, `FQuat`, `FSphere` (centre + radius), `FPlane` (normal + W).
    Vec4d([f64; 4]),
    /// `FVector2D`.
    Vec2d([f64; 2]),
    /// `FVector3f`, `FRotator3f`.
    Vec3f([f32; 3]),
    /// `FVector2f`, `FDeprecateSlateVector2D`, `FSimpleCurveKey` (time, value).
    Vec2f([f32; 2]),
    /// `FTwoVectors` — two `FVector`s.
    TwoVec3d([f64; 6]),
    /// `FMatrix` — 4 × 4 doubles.
    Mat4d(Box<[f64; 16]>),
    /// `FMatrix44f`.
    Mat4f([f32; 16]),
    /// `FIntVector`, `FIntVector2`/`FInt32Point`, `FIntVector4`, and the 64-bit
    /// and unsigned variants — kept as i64 so every width round-trips.
    Ints(Vec<i64>),
    /// `FGuid` — four uint32s, in the order the file stores them.
    Guid([u32; 4]),
    /// `FColor` — B, G, R, A.
    Color([u8; 4]),
    /// `FLinearColor` — R, G, B, A as floats.
    LinearColor([f32; 4]),
    /// `FBox` — min, max, and an `IsValid` byte.
    Box3d { min: [f64; 3], max: [f64; 3], is_valid: u8 },
    /// `FRichCurveKey` — three `uint8` enums then six floats.
    RichCurveKey { interp_mode: u8, tangent_mode: u8, tangent_weight_mode: u8, values: [f32; 6] },
    /// `FFontCharacter` — four int32s, a `uint8 TextureIndex`, an int32, and
    /// deliberately unpadded.
    FontCharacter { start_u: i32, start_v: i32, size_u: i32, size_v: i32, texture_index: u8, vertical_offset: i32 },
    /// `FNavAgentSelector` — a packed `uint32` bitfield, not the sixteen bools
    /// the `.usmap` advertises.
    PackedBits(u32),
    /// `FFrameNumber` and the MovieScene identifiers — a single int32.
    I32(i32),
    /// `FDateTime`, `FTimespan` — ticks.
    I64(i64),
    /// `TRange<FFrameNumber>` — each bound is a `TEnumAsByte` plus an int32, so
    /// five bytes per bound and ten in total, *not* the padded sixteen.
    FrameRange { lower_kind: u8, lower: i32, upper_kind: u8, upper: i32 },
    /// `FMovieSceneEvaluationKey` — sequence id, track identifier, section index.
    EvaluationKey([u32; 3]),
    /// `FPerPlatform*` / `FPerQualityLevel*` — a four-byte `FArchive` bool then
    /// the default value. The override map is editor-only and absent when cooked.
    PerPlatform { cooked: bool, value: PerPlatformValue },
    /// A fixed-size native struct whose fields are not modeled yet. Its size is
    /// known and its bytes are kept; `ce_decode_coverage` counts it as untyped.
    Opaque { name: Arc<str>, bytes: Vec<u8> },
}

/// The `Default` of an `FPerPlatform*` / `FPerQualityLevel*`.
#[derive(Debug, Clone, PartialEq)]
pub enum PerPlatformValue {
    Int(i32),
    Float(f32),
    /// Itself a four-byte `FArchive` bool.
    Bool(bool),
    /// `FFrameRate` — numerator and denominator.
    FrameRate(i32, i32),
}

impl PerPlatformValue {
    /// See [`NativeStruct::semantic_eq`].
    pub fn semantic_eq(&self, other: &PerPlatformValue) -> bool {
        match (self, other) {
            (PerPlatformValue::Float(a), PerPlatformValue::Float(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        }
    }
}

fn f64s<const N: usize>(b: &[u8]) -> [f64; N] {
    core::array::from_fn(|i| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()))
}
fn f32s<const N: usize>(b: &[u8]) -> [f32; N] {
    core::array::from_fn(|i| f32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()))
}
fn u32s<const N: usize>(b: &[u8]) -> [u32; N] {
    core::array::from_fn(|i| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap()))
}
fn i32_at(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn f64_bits_eq(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}
fn f32_bits_eq(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

impl NativeStruct {
    /// Equality for the round-trip contract, comparing floats by their **bits**.
    ///
    /// Derived equality is wrong here in both directions, and one of them is
    /// dangerous: `-0.0 == 0.0` would report a value unchanged when its bytes
    /// genuinely differ — a false pass in the gate — while `NaN != NaN` would be
    /// a false failure. Bits get both right.
    pub fn semantic_eq(&self, other: &NativeStruct) -> bool {
        use NativeStruct::*;
        match (self, other) {
            (Vec3d(a), Vec3d(b)) => f64_bits_eq(a, b),
            (Vec4d(a), Vec4d(b)) => f64_bits_eq(a, b),
            (Vec2d(a), Vec2d(b)) => f64_bits_eq(a, b),
            (TwoVec3d(a), TwoVec3d(b)) => f64_bits_eq(a, b),
            (Mat4d(a), Mat4d(b)) => f64_bits_eq(a.as_slice(), b.as_slice()),
            (Vec3f(a), Vec3f(b)) => f32_bits_eq(a, b),
            (Vec2f(a), Vec2f(b)) => f32_bits_eq(a, b),
            (Mat4f(a), Mat4f(b)) => f32_bits_eq(a, b),
            (LinearColor(a), LinearColor(b)) => f32_bits_eq(a, b),
            (RichCurveKey { interp_mode: ai, tangent_mode: at, tangent_weight_mode: aw, values: av },
             RichCurveKey { interp_mode: bi, tangent_mode: bt, tangent_weight_mode: bw, values: bv }) => {
                ai == bi && at == bt && aw == bw && f32_bits_eq(av, bv)
            }
            (Box3d { min: amin, max: amax, is_valid: av },
             Box3d { min: bmin, max: bmax, is_valid: bv }) => {
                f64_bits_eq(amin, bmin) && f64_bits_eq(amax, bmax) && av == bv
            }
            (PerPlatform { cooked: ac, value: avv }, PerPlatform { cooked: bc, value: bvv }) => {
                ac == bc && avv.semantic_eq(bvv)
            }
            // Everything else is integral or bytes, where derived equality is
            // already bit equality.
            (a, b) => a == b,
        }
    }

    /// Bytes that are still *untyped* inside this value — zero for every
    /// modeled variant, and the span length for [`NativeStruct::Opaque`]. What
    /// `ce_decode_coverage` counts.
    pub fn untyped_bytes(&self) -> usize {
        match self {
            NativeStruct::Opaque { bytes, .. } => bytes.len(),
            _ => 0,
        }
    }

    /// Decode the bytes of a fixed-size native struct named `name`.
    pub fn decode(name: &str, b: &[u8]) -> Result<Self> {
        let want = native_struct_size(name)
            .ok_or_else(|| anyhow::anyhow!("{name} is not a fixed-size native struct"))?;
        if b.len() != want {
            bail!("{name} is {want} bytes but {} were given", b.len());
        }
        Ok(match name {
            "Vector" | "Rotator" => NativeStruct::Vec3d(f64s::<3>(b)),
            "Vector4" | "Quat" | "Sphere" | "Plane" => NativeStruct::Vec4d(f64s::<4>(b)),
            "Vector2D" => NativeStruct::Vec2d(f64s::<2>(b)),
            "Vector3f" | "Rotator3f" => NativeStruct::Vec3f(f32s::<3>(b)),
            "Vector2f" | "DeprecateSlateVector2D" | "SimpleCurveKey" => {
                NativeStruct::Vec2f(f32s::<2>(b))
            }
            "TwoVectors" => NativeStruct::TwoVec3d(f64s::<6>(b)),
            "Matrix" => NativeStruct::Mat4d(Box::new(f64s::<16>(b))),
            "Matrix44f" => NativeStruct::Mat4f(f32s::<16>(b)),
            "Guid" => NativeStruct::Guid(u32s::<4>(b)),
            "Color" => NativeStruct::Color(b.try_into().unwrap()),
            "LinearColor" => NativeStruct::LinearColor(f32s::<4>(b)),
            "NavAgentSelector" => NativeStruct::PackedBits(u32s::<1>(b)[0]),
            "FrameNumber" | "MovieSceneTrackIdentifier" | "MovieSceneSequenceID"
            | "MovieSceneSegmentIdentifier" => NativeStruct::I32(i32_at(b, 0)),
            "DateTime" | "Timespan" => {
                NativeStruct::I64(i64::from_le_bytes(b[..8].try_into().unwrap()))
            }
            "MovieSceneEvaluationKey" => NativeStruct::EvaluationKey(u32s::<3>(b)),
            "MovieSceneFrameRange" => NativeStruct::FrameRange {
                lower_kind: b[0],
                lower: i32_at(b, 1),
                upper_kind: b[5],
                upper: i32_at(b, 6),
            },
            "Box" => NativeStruct::Box3d {
                min: f64s::<3>(&b[..24]),
                max: f64s::<3>(&b[24..48]),
                is_valid: b[48],
            },
            "RichCurveKey" => NativeStruct::RichCurveKey {
                interp_mode: b[0],
                tangent_mode: b[1],
                tangent_weight_mode: b[2],
                values: f32s::<6>(&b[3..]),
            },
            "FontCharacter" => NativeStruct::FontCharacter {
                start_u: i32_at(b, 0),
                start_v: i32_at(b, 4),
                size_u: i32_at(b, 8),
                size_v: i32_at(b, 12),
                texture_index: b[16],
                vertical_offset: i32_at(b, 17),
            },
            "PerPlatformInt" => NativeStruct::PerPlatform {
                cooked: u32s::<1>(b)[0] != 0,
                value: PerPlatformValue::Int(i32_at(b, 4)),
            },
            "PerPlatformFloat" => NativeStruct::PerPlatform {
                cooked: u32s::<1>(b)[0] != 0,
                value: PerPlatformValue::Float(f32s::<1>(&b[4..])[0]),
            },
            "PerPlatformBool" => NativeStruct::PerPlatform {
                cooked: u32s::<1>(b)[0] != 0,
                value: PerPlatformValue::Bool(u32s::<1>(&b[4..])[0] != 0),
            },
            "PerPlatformFrameRate" => NativeStruct::PerPlatform {
                cooked: u32s::<1>(b)[0] != 0,
                value: PerPlatformValue::FrameRate(i32_at(b, 4), i32_at(b, 8)),
            },
            // Every integer-vector variant: width and count vary, the shape does
            // not. Kept as i64 so a u64 element round-trips without wrapping.
            "IntVector" | "IntVector2" | "IntVector4" | "Int32Point" | "Int32Vector2"
            | "Int64Point" | "Int64Vector" | "Int64Vector4" | "UintVector" | "UintVector2"
            | "UintVector4" | "Uint32Point" | "UInt64Point" | "UInt64Vector" | "UInt64Vector4"
            | "IntPoint" => NativeStruct::Ints(decode_ints(name, b)),
            _ => NativeStruct::Opaque { name: Arc::from(name), bytes: b.to_vec() },
        })
    }

    /// Re-encode. Inverse of [`NativeStruct::decode`] for the same `name`.
    pub fn encode(&self, name: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match self {
            NativeStruct::Vec3d(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Vec4d(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Vec2d(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::TwoVec3d(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Mat4d(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Vec3f(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Vec2f(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Mat4f(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::LinearColor(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Guid(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::EvaluationKey(v) => v.iter().for_each(|x| out.extend(x.to_le_bytes())),
            NativeStruct::Color(v) => out.extend_from_slice(v),
            NativeStruct::PackedBits(v) => out.extend(v.to_le_bytes()),
            NativeStruct::I32(v) => out.extend(v.to_le_bytes()),
            NativeStruct::I64(v) => out.extend(v.to_le_bytes()),
            NativeStruct::FrameRange { lower_kind, lower, upper_kind, upper } => {
                out.push(*lower_kind);
                out.extend(lower.to_le_bytes());
                out.push(*upper_kind);
                out.extend(upper.to_le_bytes());
            }
            NativeStruct::Box3d { min, max, is_valid } => {
                min.iter().for_each(|x| out.extend(x.to_le_bytes()));
                max.iter().for_each(|x| out.extend(x.to_le_bytes()));
                out.push(*is_valid);
            }
            NativeStruct::RichCurveKey {
                interp_mode,
                tangent_mode,
                tangent_weight_mode,
                values,
            } => {
                out.extend_from_slice(&[*interp_mode, *tangent_mode, *tangent_weight_mode]);
                values.iter().for_each(|x| out.extend(x.to_le_bytes()));
            }
            NativeStruct::FontCharacter {
                start_u,
                start_v,
                size_u,
                size_v,
                texture_index,
                vertical_offset,
            } => {
                for v in [start_u, start_v, size_u, size_v] {
                    out.extend(v.to_le_bytes());
                }
                out.push(*texture_index);
                out.extend(vertical_offset.to_le_bytes());
            }
            NativeStruct::PerPlatform { cooked, value } => {
                out.extend((*cooked as u32).to_le_bytes());
                match value {
                    PerPlatformValue::Int(v) => out.extend(v.to_le_bytes()),
                    PerPlatformValue::Float(v) => out.extend(v.to_le_bytes()),
                    PerPlatformValue::Bool(v) => out.extend((*v as u32).to_le_bytes()),
                    PerPlatformValue::FrameRate(n, d) => {
                        out.extend(n.to_le_bytes());
                        out.extend(d.to_le_bytes());
                    }
                }
            }
            NativeStruct::Ints(v) => encode_ints(name, v, &mut out)?,
            NativeStruct::Opaque { bytes, .. } => out.extend_from_slice(bytes),
        }
        if let Some(want) = native_struct_size(name) {
            if out.len() != want {
                bail!("{name} encoded to {} bytes, expected {want}", out.len());
            }
        }
        Ok(out)
    }
}

/// Element width in bytes for each integer-vector type.
fn int_width(name: &str) -> usize {
    match name {
        "Int64Point" | "Int64Vector" | "Int64Vector4" | "UInt64Point" | "UInt64Vector"
        | "UInt64Vector4" => 8,
        _ => 4,
    }
}

fn decode_ints(name: &str, b: &[u8]) -> Vec<i64> {
    let w = int_width(name);
    let signed = !name.to_ascii_lowercase().starts_with('u');
    b.chunks_exact(w)
        .map(|c| match (w, signed) {
            (8, true) => i64::from_le_bytes(c.try_into().unwrap()),
            (8, false) => u64::from_le_bytes(c.try_into().unwrap()) as i64,
            (_, true) => i32::from_le_bytes(c.try_into().unwrap()) as i64,
            _ => u32::from_le_bytes(c.try_into().unwrap()) as i64,
        })
        .collect()
}

fn encode_ints(name: &str, v: &[i64], out: &mut Vec<u8>) -> Result<()> {
    let w = int_width(name);
    for x in v {
        match w {
            8 => out.extend((*x as u64).to_le_bytes()),
            _ => out.extend((*x as u32).to_le_bytes()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixed-size native struct must satisfy the round-trip contract
    /// `decode(encode(decode(x))) == decode(x)`, and — where nothing in it
    /// normalizes — must also come back byte-identical.
    ///
    /// The distinction is real, not a hedge. An `FArchive` bool is four bytes
    /// that the engine itself reads as `!= 0` and writes back as `1`, so a
    /// struct containing one cannot preserve an arbitrary non-boolean pattern
    /// and neither can UE. The contract it *can* meet is that the value
    /// survives, which is exactly the bar `ce_semantic_roundtrip` sets.
    #[test]
    fn every_native_struct_round_trips() {
        /// Structs with an `FArchive` bool, which normalizes to 0 or 1.
        const NORMALIZING: &[&str] =
            &["PerPlatformInt", "PerPlatformFloat", "PerPlatformBool", "PerPlatformFrameRate"];

        let mut opaque = Vec::new();
        for name in super::super::structs::NATIVE_STRUCT_NAMES {
            let size = native_struct_size(name).expect("listed name has a size");
            // Deterministic and non-degenerate: zeroes would hide a field the
            // encoder forgot to write.
            let bytes: Vec<u8> =
                (0..size).map(|i| (i as u8).wrapping_mul(7).wrapping_add(3)).collect();

            let first = NativeStruct::decode(name, &bytes)
                .unwrap_or_else(|e| panic!("{name}: decode: {e}"));
            if matches!(first, NativeStruct::Opaque { .. }) {
                opaque.push(*name);
            }
            let encoded = first.encode(name).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
            let second = NativeStruct::decode(name, &encoded)
                .unwrap_or_else(|e| panic!("{name}: re-decode: {e}"));

            assert_eq!(first, second, "{name}: value did not survive the round trip");
            if !NORMALIZING.contains(name) {
                assert_eq!(encoded, bytes, "{name}: lossless struct changed its bytes");
            }
        }
        // Recorded rather than asserted to zero: this is the list A2 works
        // through, and it must only ever shrink.
        assert!(opaque.is_empty(), "native structs still unmodeled: {opaque:?}");
    }

    /// Size does not determine layout — three sixteen-byte structs, three
    /// different shapes. Getting this wrong would round-trip bytes perfectly
    /// while reporting nonsense values.
    #[test]
    fn same_size_different_layout() {
        let b: Vec<u8> = (0..16).collect();
        assert!(matches!(NativeStruct::decode("Vector2D", &b).unwrap(), NativeStruct::Vec2d(_)));
        assert!(matches!(NativeStruct::decode("LinearColor", &b).unwrap(), NativeStruct::LinearColor(_)));
        assert!(matches!(NativeStruct::decode("Guid", &b).unwrap(), NativeStruct::Guid(_)));
    }

    /// `TRange<FFrameNumber>` is five bytes per bound, unpadded.
    #[test]
    fn frame_range_bounds_are_unpadded() {
        let mut b = vec![1u8];
        b.extend(100i32.to_le_bytes());
        b.push(2);
        b.extend(200i32.to_le_bytes());
        assert_eq!(b.len(), 10);
        match NativeStruct::decode("MovieSceneFrameRange", &b).unwrap() {
            NativeStruct::FrameRange { lower_kind, lower, upper_kind, upper } => {
                assert_eq!((lower_kind, lower, upper_kind, upper), (1, 100, 2, 200));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// A cooked `FPerPlatformFloat` is a four-byte `FArchive` bool then the
    /// default — eight bytes, not four.
    #[test]
    fn per_platform_float_is_a_cooked_flag_and_a_value() {
        let mut b = 1u32.to_le_bytes().to_vec();
        b.extend(1.0f32.to_le_bytes());
        match NativeStruct::decode("PerPlatformFloat", &b).unwrap() {
            NativeStruct::PerPlatform { cooked, value } => {
                assert!(cooked);
                assert_eq!(value, PerPlatformValue::Float(1.0));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
