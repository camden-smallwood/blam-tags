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

/// A struct whose layout lives in hand-written `Serialize` code rather than in
/// a schema.
///
/// The decoded fields are what readers want; the bytes are what a writer needs,
/// because regenerating one of these means re-implementing its `Serialize` in
/// the write direction and there are about thirty of them. Retaining the span
/// makes the round trip exact today, and converting any single struct to a real
/// writer later is then verifiable *against the span it replaces* — which is the
/// opposite of having to trust a new layout model.
pub(super) fn read_native_variable_struct(
    r: &mut Reader,
    name: &str,
    usmap: &Usmap,
    depth: usize,
) -> Result<Option<PropValue>> {
    let start = r.o;
    let decoded = read_native_variable_struct_inner(r, name, usmap, depth)?;
    Ok(decoded.map(|v| match v {
        PropValue::Struct(mut b) => {
            b.layout = BlockLayout::Native { name: Arc::from(name), bytes: r.since(start) };
            PropValue::Struct(b)
        }
        // A few of these decode to a scalar rather than a struct; those are
        // already writable by their own type's rules.
        other => other,
    }))
}

fn read_native_variable_struct_inner(
    r: &mut Reader,
    name: &str,
    usmap: &Usmap,
    depth: usize,
) -> Result<Option<PropValue>> {
    Ok(Some(match name {
        // `FShaderValueTypeHandle` declares `WithSerializer`
        // (ShaderParamTypeDefinition.h), so it is never a property block — it
        // is `operator<<(FArchive&, FShaderValueTypeHandle&)`
        // (ShaderParamTypeDefinition.cpp:624), which writes the pointed-at
        // `FShaderValueType` inline: `uint8 Type`, the `bIsDynamicArray` flag
        // (four bytes, being an `FArchive` bool), and then one of two shapes.
        // A `Struct` type writes its name and its elements — each of which is
        // an `FName` and another handle, so this recurses. Anything else
        // writes a `uint8` dimension type plus the counts that dimension
        // implies; all three counts are `uint8`.
        "ShaderValueTypeHandle" => {
            read_shader_value_type(r, depth)?;
            PropValue::Struct(PropertyBlock::default())
        }
        // `FMovieSceneTimeWarpVariant::Serialize` (MovieSceneTimeWarpVariant.cpp:171)
        // runs through `FMovieSceneNumericVariant::SerializeCustom`
        // (MovieSceneNumericVariant.cpp:215), which opens with an `FArchive`
        // bool: a literal variant is just the NaN-boxed `double`, and anything
        // else is a `uint8 EMovieSceneTimeWarpType` followed by that type's
        // payload. Only `Custom` writes an object reference; the rest write an
        // ordinary property block for their own small struct, and
        // `FixedPlayRate` writes nothing at all.
        "MovieSceneTimeWarpVariant" => {
            let mut s = BTreeMap::new();
            if r.u32()? != 0 {
                s.insert("Literal".to_string(), PropValue::Float(f64::from_bits(r.u64()?)));
            } else {
                let ty = r.u8()?;
                s.insert("Type".to_string(), PropValue::Int(ty as i64));
                let payload = match ty {
                    0 => None,                                    // FixedPlayRate
                    1 => {
                        r.i32()?; // Custom: UMovieSceneNumericVariantGetter*
                        None
                    }
                    2 => Some("MovieSceneTimeWarpFixedFrame"),
                    3 => Some("FrameRate"),
                    4 => Some("MovieSceneTimeWarpLoop"),
                    5 => Some("MovieSceneTimeWarpClamp"),
                    6 => Some("MovieSceneTimeWarpLoopFloat"),
                    7 => Some("MovieSceneTimeWarpClampFloat"),
                    _ => bail!("unknown EMovieSceneTimeWarpType {ty} @ {}", r.o - 1),
                };
                if let Some(p) = payload {
                    s.insert(
                        "Payload".to_string(),
                        PropValue::Struct(read_struct(r, p, usmap, depth + 1)?),
                    );
                }
            }
            PropValue::Struct(s.into())
        }
        // `FPerQualityLevelProperty` looks like its `FPerPlatform*` sibling —
        // `bool bCooked` then `Default` — but its `PerQuality` override map is
        // **not** behind the cooked check (`PerQualityLevelProperties.cpp`), so
        // the `TMap<int32, Value>` is always written and the struct is not a
        // fixed 8 bytes. Cooking strips the map to empty, which is why reading
        // it as 8 only drifts by the map's four count bytes — enough to land
        // the next property's `FloatInterval` on a cull distance of 10000 and
        // read `0x2710` as an unversioned header.
        "PerQualityLevelInt" | "PerQualityLevelFloat" => {
            let mut s = BTreeMap::new();
            s.insert("bCooked".to_string(), PropValue::Bool(r.u32()? != 0));
            let default = r.i32()?;
            s.insert(
                "Default".to_string(),
                if name.ends_with("Float") {
                    PropValue::Float(f32::from_bits(default as u32) as f64)
                } else {
                    PropValue::Int(default as i64)
                },
            );
            let n = native_count(r, "PerQuality overrides")?;
            r.take(n * 8)?; // int32 quality level -> int32/float value
            PropValue::Struct(s.into())
        }
        // `FInstancedPropertyBag` holds a struct type invented at runtime, so
        // there is no schema for its contents anywhere — it serializes the
        // property *descriptors* and then the value block laid out by them.
        // Helpfully it also writes that block's byte length, so the payload can
        // be skipped outright without reconstructing the bag's layout.
        //
        // `FPropertyBagPropertyDesc` is 34 bytes plus one byte per nested
        // container: `ValueTypeObject` (`FPackageIndex`), `ID` (`FGuid`),
        // `Name` (`FName`), `ValueType` (a `uint8` enum), the container-type
        // list (`uint8` count + that many bytes), and `bHasMetaData` — four
        // bytes, being an `FArchive` bool. Metadata itself is editor-only.
        "InstancedPropertyBag" => {
            let start = r.o;
            if r.u32()? != 0 {
                let descs = native_count(r, "property bag descriptors")?;
                for _ in 0..descs {
                    r.i32()?; // ValueTypeObject
                    r.take(16)?; // ID
                    r.name()?; // Name
                    r.u8()?; // ValueType
                    let containers = r.u8()? as usize;
                    r.take(containers)?;
                    if r.u32()? != 0 {
                        bail!("property-bag descriptor carries editor-only metadata");
                    }
                }
                let serial = r.i32()?;
                if serial < 0 {
                    bail!("negative property-bag payload size {serial}");
                }
                r.take(serial as usize)?;
            }
            PropValue::Raw(r.since(start))
        }
        // `FFontData::Serialize` writes a compact cooked form instead of a
        // property block: `bool bIsCooked` (four bytes) then the font-face
        // asset reference, and only when there is no face asset the filename,
        // hinting and loading policy. `SubFaceIndex` always follows.
        "FontData" => {
            let mut s = BTreeMap::new();
            if r.u32()? == 0 {
                bail!("uncooked FFontData uses the tagged-property form");
            }
            let face = r.i32()?;
            s.insert("FontFaceAsset".to_string(), PropValue::Object(face));
            if face == 0 {
                s.insert("FontFilename".to_string(), PropValue::Str(r.fstring()?));
                r.u8()?; // Hinting
                r.u8()?; // LoadingPolicy
            }
            s.insert("SubFaceIndex".to_string(), PropValue::Int(r.i32()? as i64));
            PropValue::Struct(s.into())
        }
        // A `Serialize` override that returns **false** means "I wrote a prefix
        // but did not consume the struct" — `UScriptStruct::SerializeItem` then
        // still writes the normal unversioned property block after it.
        // `FMaterialOverrideNanite` does exactly that: `bool bCooked` (four bytes)
        // and, when cooked, the resolved override material as an `FPackageIndex`
        // (its editor-side soft ref is not written), *followed by* its own
        // property block. Measured on `MI_Spartan_Shield_Screen_Recharge_Mesh`
        // and `MI_Elite_Shield_Shockwave`: `01 00 00 00 | fe ff ff ff` then a
        // 2- or 3-byte property block, after which `ScalarParameterValues`
        // counts read as 13 and 2. Consuming only the prefix desyncs the
        // parameter arrays that follow.
        "MaterialOverrideNanite" => {
            let mut s = BTreeMap::new();
            let cooked = r.i32()? != 0;
            s.insert("bCooked".to_string(), PropValue::Bool(cooked));
            if cooked {
                s.insert("OverrideMaterial".to_string(), PropValue::Object(r.i32()?));
            }
            s.extend(read_struct(r, name, usmap, depth + 1)?);
            PropValue::Struct(s.into())
        }
        // `FSkeletalMeshAreaWeightedTriangleSampler`, i.e. an
        // `FWeightedRandomSampler`. Measured on `SK_Cov_BigDoor_rig`, whose
        // sampler is the 12 zero bytes of two empty arrays and a zero weight.
        "SkeletalMeshSamplingLODBuiltData" => {
            let mut s = BTreeMap::new();
            s.insert("AreaWeightedTriangleSampler".to_string(), read_weighted_random_sampler(r)?);
            PropValue::Struct(s.into())
        }
        // `TArray<int32> TriangleIndices`, `TArray<int32> BoneIndices`, then the
        // region's area-weighted sampler. Measured on `SK_Manny`: 112 triangle
        // indices (every one a multiple of 3, i.e. index-buffer offsets around
        // 35,049), 62 bone indices (12..73 — far too small to be vertex indices
        // for those triangles), then a 112-entry sampler whose `Prob` values all
        // lie in [0,1].
        // `Vertices` is written last and is gated on
        // `FNiagaraObjectVersion::SkeletalMeshVertexSampling` — note it comes
        // *after* the sampler, not in declaration order between the triangle
        // and bone arrays. Omitting it leaves every region element short by
        // `4 + 4 * NumVertices`, which on `SK_Manny` accumulates until the
        // `UObject` `hasGuid` trailer lands on garbage and the whole class chain
        // stops before `USkeletalMesh::Serialize` ever runs.
        "SkeletalMeshSamplingRegionBuiltData" => {
            let mut s = BTreeMap::new();
            s.insert("TriangleIndices".to_string(), read_native_i32_array(r)?);
            s.insert("BoneIndices".to_string(), read_native_i32_array(r)?);
            s.insert("AreaWeightedSampler".to_string(), read_weighted_random_sampler(r)?);
            s.insert("Vertices".to_string(), read_native_i32_array(r)?);
            PropValue::Struct(s.into())
        }
        // `FNiagaraVariableBase` writes its `Name` and `TypeDefHandle` natively;
        // the handle in turn serializes an `FNiagaraTypeDefinition` *by value*,
        // which — having no serializer of its own — lands as an ordinary
        // unversioned property block. `FNiagaraVariableWithOffset` then appends
        // its `Offset`. (Its `StructConverter` is not written to a cooked
        // stream.) Measured on `NS_Brute_Beatdown_Stomp_var4`, whose 13 sorted
        // parameter offsets read 0, 16, 32, … — the strides into `ParameterData`.
        "NiagaraVariableBase" | "NiagaraVariableWithOffset" | "NiagaraVariable"
        | "NiagaraDataChannelVariable" => {
            let mut s = BTreeMap::new();
            s.insert("Name".to_string(), PropValue::Name(r.fname()?));
            s.insert(
                "TypeDefHandle".to_string(),
                PropValue::Struct(read_struct(r, "NiagaraTypeDefinition", usmap, depth + 1)?),
            );
            // `FNiagaraVariableWithOffset` appends its `Offset`;
            // `FNiagaraVariable` instead appends its `VarData` payload
            // (`TArray<uint8>`) — measured on `NS_collision`.
            if name == "NiagaraVariableWithOffset" {
                s.insert("Offset".to_string(), PropValue::Int(r.i32()? as i64));
            } else if name == "NiagaraVariable" {
                let n = r.i32()?;
                if !(0..=100_000_000).contains(&n) {
                    bail!("implausible NiagaraVariable VarData length {n} @ {}", r.o - 4);
                }
                s.insert("VarData".to_string(), PropValue::Native(r.take(n as usize)?.to_vec()));
            }
            PropValue::Struct(s.into())
        }
        // `FUniversalObjectLocatorFragment` is polymorphic: it writes the `FName`
        // of its registered fragment type, then that type's payload as an
        // ordinary unversioned property block. Measured on `LS_FrontEnd`:
        // an `FName`, then `00 03` (one value present) and the `FString`
        // `"CameraComponent"` — a sub-object path.
        "UniversalObjectLocatorFragment" => {
            let fragment_type = r.fname()?;
            let payload_struct = locator_fragment_payload(&fragment_type).with_context(|| {
                format!("unmapped universal object locator fragment type '{fragment_type}'")
            })?;
            let mut s = BTreeMap::new();
            s.insert("FragmentType".to_string(), PropValue::Name(fragment_type));
            if !payload_struct.is_empty() {
                s.extend(read_struct(r, payload_struct, usmap, depth + 1)?);
            }
            PropValue::Struct(s.into())
        }
        // `FMaterialLayersFunctionsTree`: a node array (four int32 ids each), a
        // payload array (layer + blend), then the root index.
        "MaterialLayersFunctionsTree" => {
            let nodes = native_count(r, "layer tree nodes")?;
            r.take(nodes * 16)?;
            let payloads = native_count(r, "layer tree payloads")?;
            r.take(payloads * 8)?;
            let mut s = BTreeMap::new();
            s.insert("Root".to_string(), PropValue::Int(r.i32()? as i64));
            PropValue::Struct(s.into())
        }
        // `FSoftObjectPath` carries a custom serializer, so despite listing
        // `AssetPath`/`SubPathString` in the `.usmap` it writes its parts
        // back-to-back with no property header — the same shape as the
        // `SoftObjectProperty` value reader. Measured on `LS_C10_PerfCap_01`:
        // two `FName`s then the 67-character sub-path
        // `PersistentLevel.CineCameraActor…`.
        // `FSoftClassPath` derives from `FSoftObjectPath` and inherits its
        // serializer. Measured on a `NavigationSystemModuleConfig`, whose
        // 29-byte export resolves exactly: two `FName`s and an empty sub-path.
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
        "SoftObjectPath" | "SoftClassPath" => {
            let package = r.fname()?;
            let asset = r.fname()?;
            let sub_path = r.fstring()?;
            PropValue::SoftObject(SoftObjectPath { package, asset, sub_path })
        }
        // NOTE: `FTopLevelAssetPath` is deliberately NOT handled here. Its
        // fields are written natively only as *part of* `FSoftObjectPath`'s
        // serializer; as a property in its own right it uses ordinary reflected
        // serialization. Treating it as native broke all 72
        // `WorldPartitionLevelStreamingPolicy` exports, whose
        // `SourceWorldAssetPath` begins with its own fragment header.
        // `FNiagaraDataInterfaceGPUParamInfo`: the HLSL symbol and DI class name
        // as `FString`s, then the generated-function table. Note there is no
        // `ShaderParametersOffset` in the stream despite the `.usmap` listing
        // one. Confirmed against CUE4Parse's reader and against
        // `NS_collision`, where the first entry reads `Emitter_GridMesh` /
        // `NiagaraDataInterfaceDynamicMesh` with two generated functions.
        "NiagaraDataInterfaceGPUParamInfo" => {
            let mut s = BTreeMap::new();
            s.insert("DataInterfaceHLSLSymbol".to_string(), PropValue::Str(r.fstring()?));
            s.insert("DIClassName".to_string(), PropValue::Str(r.fstring()?));
            let n = native_count(r, "GeneratedFunctions")?;
            let mut fns = Vec::with_capacity(n.min(PREALLOC_CAP));
            for _ in 0..n {
                fns.push(read_niagara_generated_function(r)?);
            }
            s.insert("GeneratedFunctions".to_string(), PropValue::Array(fns));
            PropValue::Struct(s.into())
        }
        // `FMovieSceneChannel<T>`: two extrapolation enums, then the key times
        // and key values as **bulk** arrays — each preceded by its own
        // serialized element size, which lets them be consumed without modeling
        // `FMovieSceneValue`/`FMovieSceneTangentData` at all — then the default
        // value, a four-byte `bHasDefaultValue`, the `FFrameRate` tick
        // resolution and a four-byte `bShowCurve`.
        "MovieSceneFloatChannel" | "MovieSceneDoubleChannel" => {
            let value_size = if name == "MovieSceneFloatChannel" { 4 } else { 8 };
            let mut s = BTreeMap::new();
            s.insert("PreInfinityExtrap".to_string(), PropValue::Int(r.u8()? as i64));
            s.insert("PostInfinityExtrap".to_string(), PropValue::Int(r.u8()? as i64));
            let times = read_bulk_array(r, "Times")?;
            let values = read_bulk_array(r, "Values")?;
            s.insert("NumKeys".to_string(), PropValue::Int(times as i64));
            s.insert("NumValues".to_string(), PropValue::Int(values as i64));
            s.insert(
                "DefaultValue".to_string(),
                if value_size == 4 {
                    PropValue::Float(r.f32()? as f64)
                } else {
                    PropValue::Float(r.f64()?)
                },
            );
            s.insert("bHasDefaultValue".to_string(), PropValue::Bool(r.u32()? != 0));
            s.insert("TickResolutionNumerator".to_string(), PropValue::Int(r.i32()? as i64));
            s.insert("TickResolutionDenominator".to_string(), PropValue::Int(r.i32()? as i64));
            s.insert("bShowCurve".to_string(), PropValue::Bool(r.u32()? != 0));
            PropValue::Struct(s.into())
        }
        // `FPCGPoint` leads with a byte mask saying which of its fields were
        // written, then the transform, then only the flagged fields. The
        // transform goes through this build's *reflected* `FTransform` (it has
        // no native serializer here), not a fixed-size blob.
        "PCGPoint" => {
            let mask = r.u8()?;
            let mut s = BTreeMap::new();
            // Inside a hand-written serializer an `FTransform` is written raw —
            // `FQuat` (4 × f64) then translation and scale (3 × f64 each), 80
            // bytes — unlike an `FTransform` *property*, which goes through the
            // unversioned schema. Measured here: identity rotation and unit
            // scale land exactly, and `BoundsMin` follows at the right offset.
            s.insert("Transform".to_string(), PropValue::Native(r.take(80)?.to_vec()));
            if mask & (1 << 0) != 0 {
                s.insert("Density".to_string(), PropValue::Float(r.f32()? as f64));
            }
            for (bit, field) in [(1usize, "BoundsMin"), (2, "BoundsMax")] {
                if mask & (1 << bit) != 0 {
                    s.insert(field.to_string(), PropValue::Native(r.take(24)?.to_vec()));
                }
            }
            if mask & (1 << 3) != 0 {
                s.insert("Color".to_string(), PropValue::Native(r.take(32)?.to_vec()));
            }
            if mask & (1 << 4) != 0 {
                s.insert("Steepness".to_string(), PropValue::Float(r.f32()? as f64));
            }
            if mask & (1 << 5) != 0 {
                s.insert("Seed".to_string(), PropValue::Int(r.i32()? as i64));
            }
            if mask & (1 << 6) != 0 {
                s.insert("MetadataEntry".to_string(), PropValue::Int(r.u64()? as i64));
            }
            PropValue::Struct(s.into())
        }
        // `FMovieSceneEvaluationFieldEntityTree` is a
        // `TMovieSceneEvaluationTree<FEntityAndMetaDataIndex>`: a root node,
        // then two entry containers (child nodes, and the payload data). Each
        // container is an array of 12-byte `FEntry` records followed by an array
        // of items. A tree node is a `TRange<FFrameNumber>` (10 bytes, as
        // measured for `FMovieSceneFrameRange`) plus a parent handle and two
        // entry handles — 26 bytes.
        // `FEntityAndMetaDataIndex` is two `int32`s.
        "MovieSceneEvaluationFieldEntityTree" => read_evaluation_tree(r, 8)?,
        // `FMovieSceneSubSequenceTreeEntry` is a `uint32` sequence id and a
        // one-byte flags enum (the warp counter that older streams carried is
        // gone in 5.5).
        "MovieSceneSubSequenceTree" => read_evaluation_tree(r, 5)?,
        // The MovieScene "inline value" pointers are polymorphic: an `FString`
        // naming the concrete struct type (a full script path), then — when it
        // is non-empty — that type's ordinary unversioned property block.
        "MovieSceneEvalTemplatePtr"
        | "MovieSceneTrackImplementationPtr"
        | "MovieSceneSequenceInstanceDataPtr" => {
            let type_name = r.fstring()?;
            let mut s = BTreeMap::new();
            s.insert("TypeName".to_string(), PropValue::Str(type_name.clone()));
            if !type_name.is_empty() {
                let short = type_name.rsplit('.').next().unwrap_or(&type_name).to_string();
                s.extend(read_struct(r, &short, usmap, depth + 1)?);
            }
            PropValue::Struct(s.into())
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
    s.insert("RootNode".to_string(), PropValue::Native(r.take(NODE)?.to_vec()));
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
