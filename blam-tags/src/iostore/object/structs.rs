//! Engine structs that serialize *natively* — a `Serialize` override rather
//! than a property block. Fixed-size ones are a size table; the rest have a
//! hand-written reader each, cited to the engine source it was read from.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::archive::Reader;
use super::block::read_struct;
use super::common::{native_count, read_bulk_array};
use super::limits::{MAX_DEPTH, PREALLOC_CAP};
use super::text::locator_fragment_payload;
use super::usmap::Usmap;
use super::value::{BlockLayout, PropValue, PropertyBlock, SoftObjectPath};

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
        "Matrix" => 128,    // 4 × 4 × f64
        "Matrix44f" => 64,
        "FrameNumber" | "MovieSceneTrackIdentifier" | "MovieSceneSequenceID"
        | "MovieSceneSegmentIdentifier" => 4,
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

/// Structs serialized by a hand-written `Serialize` whose length is
/// data-dependent, so [`native_struct_size`] cannot describe them. They carry no
/// reflected members at all, which is exactly how they present in the `.usmap`:
/// a struct with zero properties. Returns `None` for anything not modeled here,
/// so the caller falls back to the schema-driven walk.
/// `operator<<(FArchive&, FShaderValueTypeHandle&)`. Split out because a
/// `Struct` value type holds a list of members whose own types are handles, so
/// the reader has to recurse.
pub(super) fn read_shader_value_type(r: &mut Reader, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        bail!("shader value type nested past 32 levels");
    }
    const STRUCT: u8 = 4;
    let ty = r.u8()?;
    r.u32()?; // bIsDynamicArray
    if ty == STRUCT {
        r.name()?; // Name
        let n = native_count(r, "shader value type struct elements")?;
        for _ in 0..n {
            r.name()?; // FStructElement::Name
            read_shader_value_type(r, depth + 1)?;
        }
    } else {
        // EShaderFundamentalDimensionType: Scalar 0, Vector 1, Matrix 2.
        match r.u8()? {
            1 => {
                r.u8()?; // VectorElemCount
            }
            2 => {
                r.take(2)?; // MatrixRowCount, MatrixColumnCount
            }
            _ => {}
        }
    }
    Ok(())
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

/// A `TMovieSceneEvaluationTree<T>`: a root node, then two entry containers —
/// the child nodes and the payload items. Each container is an array of 12-byte
/// `FEntry` records (start/size/capacity) followed by an array of its elements.
///
/// A node is a `TRange<FFrameNumber>` (10 bytes, per the `FMovieSceneFrameRange`
/// measurement) plus a parent handle and two entry handles: 26 bytes. Only
/// `item_size` varies between the concrete trees.
pub(super) fn read_evaluation_tree(r: &mut Reader, item_size: usize) -> Result<PropValue> {
    const NODE: usize = 26;
    let mut s = BTreeMap::new();
    s.insert("RootNode".to_string(), PropValue::Raw(r.take(NODE)?.to_vec()));
    let entries = native_count(r, "child node entries")?;
    r.take(entries * 12)?;
    let nodes = native_count(r, "child nodes")?;
    r.take(nodes * NODE)?;
    let data_entries = native_count(r, "data entries")?;
    r.take(data_entries * 12)?;
    let items = native_count(r, "data items")?;
    r.take(items * item_size)?;
    s.insert("NumChildNodes".to_string(), PropValue::Int(nodes as i64));
    s.insert("NumItems".to_string(), PropValue::Int(items as i64));
    Ok(PropValue::Struct(s.into()))
}

/// `FNiagaraDataInterfaceGeneratedFunction`: definition `FName`, instance
/// `FString`, `(FName, FName)` specifiers, the variadic input/output references,
/// and a `uint16` usage mask.
pub(super) fn read_niagara_generated_function(r: &mut Reader) -> Result<PropValue> {
    let mut s = BTreeMap::new();
    s.insert("DefinitionName".to_string(), PropValue::Name(r.fname()?));
    s.insert("InstanceName".to_string(), PropValue::Str(r.fstring()?));
    let n = native_count(r, "Specifiers")?;
    let mut spec = Vec::with_capacity(n.min(PREALLOC_CAP));
    for _ in 0..n {
        let k = PropValue::Name(r.fname()?);
        let v = PropValue::Name(r.fname()?);
        spec.push(PropValue::Array(vec![k, v]));
    }
    s.insert("Specifiers".to_string(), PropValue::Array(spec));
    // Each variadic entry is an `FNiagaraVariableCommonReference`: an `FName`
    // and an `FPackageIndex`.
    for field in ["VariadicInputs", "VariadicOutputs"] {
        let n = native_count(r, field)?;
        let mut v = Vec::with_capacity(n.min(PREALLOC_CAP));
        for _ in 0..n {
            let mut e = BTreeMap::new();
            e.insert("Name".to_string(), PropValue::Name(r.fname()?));
            e.insert("UnderlyingType".to_string(), PropValue::Object(r.i32()?));
            v.push(PropValue::Struct(e.into()));
        }
        s.insert(field.to_string(), PropValue::Array(v));
    }
    // No trailing `MiscUsageBitMask`: that field is gated on a later Niagara
    // custom version than this build. Measured on `NS_collision`, where the
    // second generated function's `FName` begins immediately after the variadic
    // output count — two bytes earlier than the bitmask would allow.
    Ok(PropValue::Struct(s.into()))
}

/// A natively-serialized `TArray<int32>`: count then that many `int32`s.
pub(super) fn read_native_i32_array(r: &mut Reader) -> Result<PropValue> {
    let n = r.i32()?;
    if !(0..=100_000_000).contains(&n) {
        bail!("implausible native array count {n} @ {}", r.o - 4);
    }
    let mut v = Vec::with_capacity((n as usize).min(PREALLOC_CAP));
    for _ in 0..n {
        v.push(PropValue::Int(r.i32()? as i64));
    }
    Ok(PropValue::Array(v))
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
