//! Engine structs that serialize *natively* — a `Serialize` override rather
//! than a property block. Fixed-size ones are a size table; the rest have a
//! hand-written reader each, cited to the engine source it was read from.

use anyhow::{bail, Result};
use std::collections::BTreeMap;

use super::archive::Reader;
use super::common::native_count;
use super::limits::PREALLOC_CAP;
use super::usmap::Usmap;
use super::value::{PropValue, SoftObjectPath};

/// Fixed serialized byte sizes for engine structs that serialize *natively*
/// (a `SerializeNative`/`Serialize` override), so unversioned serialization
/// emits their raw bytes with no inner property header. The math primitives
/// use UE5 large-world-coordinate `double`s.
///
/// Note `FTransform` is deliberately *absent*: in this build it has no native
/// serializer and instead serializes as an unversioned struct
/// (`Rotation`/`Translation`/`Scale3D`), with zero-value components masked out
/// — so it is parsed via the schema like any other reflected struct, and its
/// `FQuat`/`FVector` members fall through to the native sizes below.

/// Every struct name [`native_struct_size`] knows, so the table can be
/// enumerated — by the test below, and by `ce_native_struct_census`, which
/// reports which of these the shipped corpus actually exercises.
///
/// A size the corpus never exercises is an unverified guess: the coverage
/// matrix proves every entry that *appears* in Campaign Evolved's data, and
/// says nothing about the rest.
///
/// Measured by `ce_native_struct_census`: **27 of these 50 are exercised**, the
/// most-read being `Guid` (788,623), `Vector` (597,297), `Rotator` (190,999)
/// and `Quat` (43,320). The other 23 are claims:
///
/// ```text
/// DateTime, Int32Point, Int32Vector2, Int64Point, Int64Vector4, IntVector4,
/// Matrix, MovieSceneSegmentIdentifier, NavAgentSelector, PerPlatformBool,
/// Plane, Rotator3f, SimpleCurveKey, Sphere, Timespan, TwoVectors,
/// UInt64Point, UInt64Vector, UInt64Vector4, Uint32Point, UintVector,
/// UintVector2, UintVector4
/// ```
///
/// Most are integer-vector variants whose size is arithmetic (N components of a
/// known width). The ones worth a citation are the rest, and `NavAgentSelector`
/// now has one — see its entry.
pub const NATIVE_STRUCT_NAMES: &[&str] = &[
    "Box",
    "Color",
    "DateTime",
    "DeprecateSlateVector2D",
    "FontCharacter",
    "FrameNumber",
    "Guid",
    "Int32Point",
    "Int32Vector2",
    "Int64Point",
    "Int64Vector",
    "Int64Vector4",
    "IntPoint",
    "IntVector",
    "IntVector2",
    "IntVector4",
    "LinearColor",
    "Matrix",
    "Matrix44f",
    "MovieSceneEvaluationKey",
    "MovieSceneFrameRange",
    "MovieSceneSegmentIdentifier",
    "MovieSceneSequenceID",
    "MovieSceneTrackIdentifier",
    "NavAgentSelector",
    "PerPlatformBool",
    "PerPlatformFloat",
    "PerPlatformFrameRate",
    "PerPlatformInt",
    "Plane",
    "Quat",
    "RichCurveKey",
    "Rotator",
    "Rotator3f",
    "SimpleCurveKey",
    "Sphere",
    "Timespan",
    "TwoVectors",
    "UInt64Point",
    "UInt64Vector",
    "UInt64Vector4",
    "Uint32Point",
    "UintVector",
    "UintVector2",
    "UintVector4",
    "Vector",
    "Vector2D",
    "Vector2f",
    "Vector3f",
    "Vector4",
];

pub fn native_struct_size(name: &str) -> Option<usize> {
    Some(match name {
        "Vector" | "Rotator" => 24,               // 3 × f64
        "Vector4" | "Quat" => 32,                 // 4 × f64
        "Vector2D" | "LinearColor" | "Guid" => 16,
        // `FBox3d`: two `FVector`s and a `uint8 IsValid`. Measured on
        // `NS_Brute_Beatdown_BloodSpurt_Human`, whose fixed bounds are
        // (-500,-500,-500)..(500,500,500) with `IsValid = 1`.
        "Box" => 49,
        "Vector3f" | "Rotator3f" | "IntVector" => 12, // 3 × f32 / 3 × i32
        // 3 × i64. Measured on a `HaloAudioZonePartialVoxelGrid`, whose voxel
        // LOD dimensions read (80, 110, 20).
        "Int64Vector" | "UInt64Vector" => 24,
        "Int64Vector4" | "UInt64Vector4" => 32,
        "Int64Point" | "UInt64Point" => 16,
        "Int32Point" | "Int32Vector2" | "Uint32Point" | "IntVector2" | "UintVector2" => 8,
        "IntVector4" | "UintVector4" => 16,
        "UintVector" => 12,
        // `FNavAgentSelector` is a single packed `uint32` bitfield, not the 16
        // separate bools the `.usmap` advertises: the bitfields sit in a union
        // over `uint32 PackedBits` (NavAgentSelector.h:55). Cited rather than
        // measured — Campaign Evolved never serializes one, so the corpus
        // cannot confirm it.
        "NavAgentSelector" => 4,
        // `FRichCurveKey`: three `uint8` enums then six `float`s (Time, Value,
        // Arrive/LeaveTangent and their weights).
        "RichCurveKey" => 27,
        // `FSimpleCurveKey` / `FNameCurveKey` / `FStringCurveKey` lead with a
        // `float Time`; only the simple one is a fixed-size pair.
        "SimpleCurveKey" => 8,
        "Sphere" => 32,     // centre (3 × f64) + radius
        "TwoVectors" => 48, // 2 × FVector
        "Plane" => 32,      // FVector normal + f64 W
        // Four `FPlane`s, not sixteen bare doubles — the same 128 bytes, but the
        // reflected members are planes (NoExportTypes.h:1743, 5.5.4). Cited
        // rather than measured: six schemas declare an `FMatrix` and no export
        // in the corpus ever serializes one.
        "Matrix" => 128,
        "Matrix44f" => 64,
        "FrameNumber" | "MovieSceneTrackIdentifier" | "MovieSceneSequenceID"
        | "MovieSceneSegmentIdentifier" => 4,
        // Both are a single `int64` — `FTimespan` is `int64 Ticks`
        // (NoExportTypes.h:2229) and `FDateTime` a tick count likewise. Four
        // schemas declare an `FTimespan`; none is serialized, so this is cited.
        "DateTime" | "Timespan" => 8,
        // `TRange<FFrameNumber>`: each bound is a `TEnumAsByte` type plus an
        // `int32`, so 5 bytes per bound and 10 in total — *not* the 16 a
        // padded in-memory layout would suggest. Measured on `WBP_HUD_Main`,
        // whose playback range reads frames 0 (inclusive) → 18001 (exclusive).
        "MovieSceneFrameRange" => 10,
        // Three `uint32`s: sequence id, track identifier, section index.
        "MovieSceneEvaluationKey" => 12,
        // `FDeprecateSlateVector2D` derives `FVector2f` and serializes as one.
        "Vector2f" | "IntPoint" | "DeprecateSlateVector2D" => 8,
        "Color" => 4,
        // `FFontCharacter` declares `WithSerializer`, so it is written by its
        // own `operator<<` rather than as a property block: four `int32`s, a
        // `uint8 TextureIndex`, then `int32 VerticalOffset` — 21 bytes, and
        // deliberately unpadded.
        "FontCharacter" => 21,
        // `FPerPlatform*`/`FPerQualityLevel*` serialize a leading `bool bCooked`
        // and then their `Default` scalar; the override map that follows is
        // editor-only, so a cooked stream stops there. `FArchive`'s `bool`
        // operator writes **four** bytes, so the cooked form is 4 + sizeof(Default)
        // — measured on `SkeletalMeshLODInfo::ScreenSize`, whose cooked bytes are
        // `01 00 00 00` (bCooked) then `00 00 80 3f` (1.0), followed by a
        // `LODHysteresis` of 0.02. Reading these as bare 4-byte scalars desyncs
        // every property after a mesh's `ScreenSize`/`MinLOD`/`MinQualityLevelLOD`.
        "PerPlatformInt" | "PerPlatformFloat" => 8,
        // `Default` is a `bool`, itself four bytes through `FArchive`.
        "PerPlatformBool" => 8,
        // Same shape, but `Default` is an `FFrameRate` (Numerator + Denominator).
        // Measured on `A_GenDoors_Open`: `01 00 00 00 | 18 00 00 00 01 00 00 00`
        // — cooked, 24/1 fps.
        "PerPlatformFrameRate" => 12,
        _ => return None,
    })
}


/// The two hand-written structs that decode to a *value* rather than to a
/// struct, so they never needed a typed model of their own.
///
/// Everything else that used to live here is now in [`super::hand_written`],
/// read into typed fields and written back from them. The retained-span wrapper
/// that used to surround this function is gone with them.
pub(super) fn read_native_variable_struct(
    r: &mut Reader,
    name: &str,
    usmap: &Usmap,
    depth: usize,
) -> Result<Option<PropValue>> {
    if let Some(typed) = super::hand_written::HandWritten::read(r, name, usmap, depth)? {
        return Ok(Some(PropValue::HandWritten(typed)));
    }
    Ok(Some(match name {
        // `FGameplayTagContainer` writes a plain array of `FGameplayTag`, each
        // just its tag `FName`.
        "GameplayTagContainer" => {
            let n = native_count(r, "GameplayTags")?;
            let mut tags = Vec::with_capacity(n.min(PREALLOC_CAP));
            for _ in 0..n {
                tags.push(PropValue::Name(r.fname()?));
            }
            PropValue::Array(tags)
        }
        // `FSoftObjectPath` carries a custom serializer, so despite listing
        // `AssetPath`/`SubPathString` in the `.usmap` it writes its parts
        // back-to-back with no property header — the same shape as the
        // `SoftObjectProperty` value reader. `FSoftClassPath` derives from it
        // and inherits the serializer.
        //
        // NOTE: `FTopLevelAssetPath` is deliberately NOT handled here. Its
        // fields are written natively only as *part of* `FSoftObjectPath`'s
        // serializer; as a property in its own right it uses ordinary reflected
        // serialization, and treating it as native broke all 72 of them.
        "SoftObjectPath" | "SoftClassPath" => {
            let package = r.fname()?;
            let asset = r.fname()?;
            let sub_path = r.fstring()?;
            PropValue::SoftObject(SoftObjectPath { package, asset, sub_path })
        }
        _ => return Ok(None),
    }))
}




/// `FWeightedRandomSampler`: `TArray<float> Prob`, `TArray<int32> Alias`,
/// `float TotalWeight`.
pub(super) fn read_weighted_random_sampler(r: &mut Reader) -> Result<PropValue> {
    let count = |r: &mut Reader| -> Result<usize> {
        let n = r.i32()?;
        if !(0..=100_000_000).contains(&n) {
            bail!("implausible sampler array count {n} @ {}", r.o - 4);
        }
        Ok(n as usize)
    };
    let n = count(r)?;
    let mut prob = Vec::with_capacity(n.min(PREALLOC_CAP));
    for _ in 0..n {
        prob.push(PropValue::Float(r.f32()? as f64));
    }
    let n = count(r)?;
    let mut alias = Vec::with_capacity(n.min(PREALLOC_CAP));
    for _ in 0..n {
        alias.push(PropValue::Int(r.i32()? as i64));
    }
    let mut s = BTreeMap::new();
    s.insert("Prob".to_string(), PropValue::Array(prob));
    s.insert("Alias".to_string(), PropValue::Array(alias));
    s.insert("TotalWeight".to_string(), PropValue::Float(r.f32()? as f64));
    Ok(PropValue::Struct(s.into()))
}


// ---------------------------------------------------------------------------
// Typed Campaign Evolved mesh-sync extraction
// ---------------------------------------------------------------------------

#[cfg(test)]
mod native_size_tests {
    use super::*;

    /// The enumerable list and the match must not drift apart. This catches a
    /// name removed from the match; a name added to the match without being
    /// listed is not detectable from here, which is why the list sits directly
    /// above it.
    #[test]
    fn every_listed_native_struct_has_a_size() {
        for name in NATIVE_STRUCT_NAMES {
            assert!(
                native_struct_size(name).is_some(),
                "{name} is listed in NATIVE_STRUCT_NAMES but has no size"
            );
        }
    }

    /// A handful of sizes that are easy to get wrong by assuming the in-memory
    /// layout, each one having actually desynced a real decode.
    #[test]
    fn the_sizes_that_are_not_what_they_look_like() {
        // `FArchive` writes a bool as four bytes, so a cooked `FPerPlatformFloat`
        // is 8, not 4. This single size blocked SkeletalMesh and StaticMesh.
        assert_eq!(native_struct_size("PerPlatformFloat"), Some(8));
        assert_eq!(native_struct_size("PerPlatformBool"), Some(8));
        // `TRange<FFrameNumber>` is 5 bytes per bound, not the padded 8.
        assert_eq!(native_struct_size("MovieSceneFrameRange"), Some(10));
        // Deliberately unpadded.
        assert_eq!(native_struct_size("FontCharacter"), Some(21));
        assert_eq!(native_struct_size("RichCurveKey"), Some(27));
        // A packed bitfield, not the 16 bools the .usmap advertises.
        assert_eq!(native_struct_size("NavAgentSelector"), Some(4));
        // UE5 large-world coordinates: doubles, not floats.
        assert_eq!(native_struct_size("Vector"), Some(24));
        assert_eq!(native_struct_size("Vector3f"), Some(12));
    }
}
