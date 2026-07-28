//! Cooked *unversioned* property serialization reader, driven by the embedded
//! `.usmap` schema — enough of it to recover the authoritative Campaign
//! Evolved region→permutation→mesh mapping from a cooked
//! `BlamMeshSynchronizationComponent` export.
//!
//! Cooked IoStore packages serialize object properties without per-property
//! tags: an `FUnversionedHeader` (a run of `skip`/`value` fragments plus an
//! optional zero-mask) says *which* schema properties are present, and the
//! values follow back-to-back. The schema (property order + types) comes from
//! the `.usmap`. Property order within a class is **derived→base**, matching
//! the engine's `UStruct::PropertyLink` walk — see
//! [`Usmap::flattened_properties`](super::usmap::Usmap::flattened_properties).
//!
//! Nested reflected structs (e.g. `FBlamMeshSynchronizationRuntimeRegion`)
//! serialize the same way recursively; a handful of engine structs
//! (`FTransform`, `FVector`, …) instead serialize as fixed-size native blobs
//! and are skipped by byte size.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use super::usmap::{PropertyType, Usmap, UsmapProperty};

/// A decoded property value. Only the shapes this reader needs are modeled;
/// everything else is consumed for correct positioning and discarded.
#[derive(Debug, Clone)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// An `FName` resolved to its display string.
    Name(String),
    Str(String),
    /// An `FPackageIndex` (import if negative, export if positive).
    Object(i32),
    /// An `FSoftObjectPath`: `(PackageName, AssetName, SubPath)`.
    SoftObject(SoftObjectPath),
    Array(Vec<PropValue>),
    /// A `TMap`, preserving insertion order.
    Map(Vec<(PropValue, PropValue)>),
    /// A nested reflected struct: property name → value.
    Struct(BTreeMap<String, PropValue>),
    /// A natively-serialized struct's raw bytes (e.g. `FVector`/`FQuat`), kept
    /// so transforms can be decoded on demand.
    Native(Vec<u8>),
    /// A value consumed but not modeled (delegate, field path, …).
    Opaque,
}

impl PropValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Name(s) | PropValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(PropValue, PropValue)]> {
        match self {
            PropValue::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[PropValue]> {
        match self {
            PropValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_struct(&self) -> Option<&BTreeMap<String, PropValue>> {
        match self {
            PropValue::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_soft_object(&self) -> Option<&SoftObjectPath> {
        match self {
            PropValue::SoftObject(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_native(&self) -> Option<&[u8]> {
        match self {
            PropValue::Native(b) => Some(b),
            _ => None,
        }
    }
}

/// A component-relative transform (`FTransform`) attached to a bone: UE5
/// large-world-coordinate `double`s, so `FQuat`=4×f64 and `FVector`=3×f64.
#[derive(Debug, Clone, Copy)]
pub struct MeshTransform {
    /// `(x, y, z, w)` quaternion.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for MeshTransform {
    fn default() -> Self {
        Self {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl MeshTransform {
    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3]
            && self.rotation == [0.0, 0.0, 0.0, 1.0]
            && self.scale == [1.0; 3]
    }

    /// Decode from a reflected `FTransform` struct value (`Rotation`/
    /// `Translation`/`Scale3D` as native `FQuat`/`FVector` blobs).
    fn from_prop(v: &PropValue) -> Option<MeshTransform> {
        let s = v.as_struct()?;
        let f64s = |name: &str, n: usize| -> Option<Vec<f64>> {
            let b = s.get(name)?.as_native()?;
            if b.len() < n * 8 {
                return None;
            }
            Some(
                (0..n)
                    .map(|i| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()))
                    .collect(),
            )
        };
        let mut t = MeshTransform::default();
        if let Some(r) = f64s("Rotation", 4) {
            t.rotation = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
        }
        if let Some(tr) = f64s("Translation", 3) {
            t.translation = [tr[0] as f32, tr[1] as f32, tr[2] as f32];
        }
        if let Some(sc) = f64s("Scale3D", 3) {
            t.scale = [sc[0] as f32, sc[1] as f32, sc[2] as f32];
        }
        Some(t)
    }
}

/// An `FSoftObjectPath` — a `TopLevelAssetPath` plus optional sub-path.
#[derive(Debug, Clone, Default)]
pub struct SoftObjectPath {
    /// Full package name, e.g. `/Game/Characters/Marine/.../SK_Marine_Torso_01`.
    pub package: String,
    /// Object name within the package, e.g. `SK_Marine_Torso_01`.
    pub asset: String,
    pub sub_path: String,
}

impl SoftObjectPath {
    pub fn is_empty(&self) -> bool {
        self.package.is_empty() && self.asset.is_empty()
    }
}

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
fn native_struct_size(name: &str) -> Option<usize> {
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
        // separate bools the `.usmap` advertises.
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
fn read_shader_value_type(r: &mut Reader, depth: usize) -> Result<()> {
    if depth > 32 {
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

fn read_native_variable_struct(
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
            PropValue::Struct(BTreeMap::new())
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
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
            PropValue::Opaque
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
            PropValue::Struct(s)
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
            PropValue::Struct(s)
        }
        // `FSkeletalMeshAreaWeightedTriangleSampler`, i.e. an
        // `FWeightedRandomSampler`. Measured on `SK_Cov_BigDoor_rig`, whose
        // sampler is the 12 zero bytes of two empty arrays and a zero weight.
        "SkeletalMeshSamplingLODBuiltData" => {
            let mut s = BTreeMap::new();
            s.insert("AreaWeightedTriangleSampler".to_string(), read_weighted_random_sampler(r)?);
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
            s.insert("Name".to_string(), PropValue::Name(r.name()?));
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
            PropValue::Struct(s)
        }
        // `FUniversalObjectLocatorFragment` is polymorphic: it writes the `FName`
        // of its registered fragment type, then that type's payload as an
        // ordinary unversioned property block. Measured on `LS_FrontEnd`:
        // an `FName`, then `00 03` (one value present) and the `FString`
        // `"CameraComponent"` — a sub-object path.
        "UniversalObjectLocatorFragment" => {
            let fragment_type = r.name()?;
            let payload_struct = locator_fragment_payload(&fragment_type).with_context(|| {
                format!("unmapped universal object locator fragment type '{fragment_type}'")
            })?;
            let mut s = BTreeMap::new();
            s.insert("FragmentType".to_string(), PropValue::Name(fragment_type));
            if !payload_struct.is_empty() {
                s.extend(read_struct(r, payload_struct, usmap, depth + 1)?);
            }
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
            let mut tags = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                tags.push(PropValue::Name(r.name()?));
            }
            PropValue::Array(tags)
        }
        "SoftObjectPath" | "SoftClassPath" => {
            let package = r.name()?;
            let asset = r.name()?;
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
            let mut fns = Vec::with_capacity(n.min(4096));
            for _ in 0..n {
                fns.push(read_niagara_generated_function(r)?);
            }
            s.insert("GeneratedFunctions".to_string(), PropValue::Array(fns));
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
            PropValue::Struct(s)
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
fn read_evaluation_tree(r: &mut Reader, item_size: usize) -> Result<PropValue> {
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
    Ok(PropValue::Struct(s))
}

/// A `TArray` written with `BulkSerialize`: the element size, the count, then
/// `count × size` bytes of blittable elements. Returns the element count.
fn read_bulk_array(r: &mut Reader, what: &str) -> Result<usize> {
    let elem = r.i32()?;
    if !(0..=4096).contains(&elem) {
        bail!("implausible {what} element size {elem} @ {}", r.o - 4);
    }
    // Bound the count by the bytes actually left in the export rather than by a
    // flat ceiling. `FRawStaticIndexBuffer` stores its indices as *single-byte*
    // elements, so a 1024x1024 plane's index buffer is a legitimate count of
    // 25,165,824 — which a fixed cap rejects. Sizing against the remainder is
    // both correct for that case and tighter for every smaller one.
    let at = r.o;
    let n = r.i32()?;
    let remaining = r.b.len().saturating_sub(r.o);
    let bytes = usize::try_from(n).ok().and_then(|n| n.checked_mul(elem as usize));
    match bytes {
        Some(b) if b <= remaining => {
            r.take(b)?;
            Ok(n as usize)
        }
        _ => bail!("implausible {what} count {n} (elem {elem}, {remaining} left) @ {at}"),
    }
}

/// `Nanite::FResources::Serialize` (NaniteResources.cpp). The load path ignores
/// the `bCooked` argument — it only changes what a save writes — so the same
/// reader serves both the static-mesh and skeletal-mesh callers.
fn read_nanite_resources(r: &mut Reader) -> Result<()> {
    r.take(2)?; // FStripDataFlags
    r.u32()?; // ResourceFlags
    r.i32()?; // StreamablePages: an FByteBulkData header, an index in a Zen package
    let root = native_count(r, "Nanite RootData")?;
    r.take(root)?;
    let pages = native_count(r, "PageStreamingStates")?;
    r.take(pages * 20)?;
    let nodes = native_count(r, "HierarchyNodes")?;
    // `FPackedHierarchyNode` is NANITE_MAX_BVH_NODE_FANOUT (4) slices of 52
    // bytes each — the float variants, so 208 and not the 304 a double-width
    // read would give.
    r.take(nodes * 208)?;
    let roots = native_count(r, "HierarchyRootOffsets")?;
    r.take(roots * 4)?;
    let deps = native_count(r, "PageDependencies")?;
    r.take(deps * 4)?;
    let imposter = native_count(r, "ImposterAtlas")?;
    r.take(imposter * 2)?;
    r.take(16)?; // NumRootPages, PositionPrecision, NormalPrecision, NumInputTriangles
    r.take(12)?; // NumInputVertices, NumInputMeshes + NumInputTexCoords (u16), NumClusters
    Ok(())
}

/// One `FSkelMeshRenderSection` (`SkeletalMeshLODRenderData.cpp`). Returns
/// whether the section carries cloth mapping data, which decides whether the
/// LOD's cloth buffer is present further down.
fn read_skel_render_section(r: &mut Reader) -> Result<bool> {
    r.u8()?; // global strip flags
    let class_strip = r.u8()?;
    r.take(2)?; // MaterialIndex (u16)
    r.u32()?; // BaseIndex
    r.u32()?; // NumTriangles
    r.u32()?; // bRecomputeTangent
    r.u8()?; // RecomputeTangentsVertexMaskChannel
    r.u32()?; // bCastShadow
    r.u32()?; // bVisibleInRayTracing
    r.u32()?; // BaseVertexIndex
    // `ClothMappingDataLODs`: an array of arrays of `FMeshToMeshVertData`.
    let mut has_cloth = false;
    let outer = native_count(r, "ClothMappingDataLODs")?;
    for _ in 0..outer {
        let inner = native_count(r, "cloth mapping data")?;
        has_cloth |= inner > 0;
        r.take(inner * 80)?; // FMeshToMeshVertData
    }
    let bones = native_count(r, "BoneMap")?;
    r.take(bones * 2)?;
    r.u32()?; // NumVertices
    r.i32()?; // MaxBoneInfluences
    r.take(2)?; // CorrespondClothAssetIndex (i16)
    r.take(20)?; // FClothingSectionData: FGuid + i32
    // The duplicated-vertex buffer is stripped from cooks that do not need it;
    // bit 0 of the class strip flags says so.
    if class_strip & 1 == 0 {
        let dv = native_count(r, "DupVertData")?;
        r.take(dv * 4)?;
        let dvi = native_count(r, "DupVertIndexData")?;
        r.take(dvi * 8)?;
    }
    r.u32()?; // bDisabled
    Ok(has_cloth)
}

/// `FSkeletalMeshLODRenderData::SerializeStreamedData` — everything that lives
/// either inline in the export or, for a streamed LOD, in the `.ubulk` payload.
fn read_skel_streamed_data(r: &mut Reader, has_vertex_colors: bool, has_cloth: bool) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    if t { eprintln!("    streamed data @ {}", r.o); }
    r.take(2)?; // FStripDataFlags
    r.u8()?; // FMultiSizeIndexContainer::DataTypeSize
    read_bulk_array(r, "index buffer")?;
    if t { eprintln!("    positions @ {}", r.o); }
    // FPositionVertexBuffer
    r.i32()?; // Stride
    r.i32()?; // NumVertices
    read_bulk_array(r, "positions")?;
    if t { eprintln!("    static vb @ {}", r.o); }
    // FStaticMeshVertexBuffer
    r.take(2)?; // FStripDataFlags
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    read_bulk_array(r, "tangents")?;
    read_bulk_array(r, "UVs")?;
    if t { eprintln!("    skin weights @ {}", r.o); }
    // FSkinWeightVertexBuffer = a data buffer then a lookup buffer.
    r.take(2)?; // FStripDataFlags
    r.u32()?; // bVariableBonesPerVertex
    r.u32()?; // MaxBoneInfluences
    r.u32()?; // NumBoneWeights
    r.u32()?; // NumVertices
    r.u32()?; // bUse16BitBoneIndex
    r.u32()?; // bUse16BitBoneWeight
    read_bulk_array(r, "skin weights")?;
    r.take(2)?; // FStripDataFlags
    r.u32()?; // FSkinWeightLookupVertexBuffer::NumVertices
    read_bulk_array(r, "skin weight lookup")?;
    if t { eprintln!("    colors? @ {}", r.o); }
    if has_vertex_colors {
        // `FColorVertexBuffer` allocates — and so serializes — its payload only
        // when it actually has vertices.
        r.take(2)?; // FStripDataFlags
        r.i32()?; // Stride
        let n = r.u32()?;
        if t { eprintln!("      color verts {n}"); }
        if n > 0 {
            read_bulk_array(r, "vertex colors")?;
        }
    }
    if t { eprintln!("    cloth? @ {}", r.o); }
    if has_cloth {
        r.take(2)?; // FStripDataFlags
        read_bulk_array(r, "cloth vertices")?;
        let n = native_count(r, "ClothIndexMapping")?;
        r.take(n * 12)?; // FClothBufferIndexMapping
    }
    if t { eprintln!("    profiles @ {}", r.o); }
    // `FSkinWeightProfilesData`: a map from profile name to override data.
    //
    // Note these are plain `TArray`/`TMap` members reached through `Ar <<`, so
    // each is a bare count — unlike the vertex buffers above, whose payloads go
    // through `BulkSerialize` and carry an element size ahead of the count.
    let profiles = native_count(r, "SkinWeightProfiles")?;
    for _ in 0..profiles {
        r.take(8)?; // profile FName
        for what in ["profile BoneIDs", "profile BoneWeights"] {
            let n = native_count(r, what)?;
            r.take(n)?; // TArray<uint8>
        }
        r.u8()?; // NumWeightsPerVertex
        let n = native_count(r, "profile VertexIndexToInfluenceOffset")?;
        r.take(n * 8)?; // TMap<uint32, uint32>
    }
    if t { eprintln!("    raytracing @ {}", r.o); }
    // `FRayTracingGeometry::RawData` is a `TResourceArray<uint8>` written with
    // `Ar <<`, so it is a count and then that many bytes.
    let raw = native_count(r, "SourceRayTracingGeometry")?;
    r.take(raw)?;
    if t { eprintln!("    morph @ {}", r.o); }
    // Compressed morph target render data, present only when the cook wrote it.
    if r.u32()? != 0 {
        let n = native_count(r, "MorphData")?;
        r.take(n * 4)?; // TResourceArray<uint32>, so a bare count
        for what in ["MinimumValuePerMorph", "MaximumValuePerMorph"] {
            let n = native_count(r, what)?;
            r.take(n * 16)?; // FVector4f
        }
        for what in ["BatchStartOffsetPerMorph", "BatchesPerMorph"] {
            let n = native_count(r, what)?;
            r.take(n * 4)?;
        }
        r.take(12)?; // NumTotalBatches, PositionPrecision, TangentZPrecision
    }
    if t { eprintln!("    attributes @ {}", r.o); }
    // Per-vertex attribute buffers, keyed by name.
    let attrs = native_count(r, "VertexAttributeBuffers")?;
    if t { eprintln!("      attribute buffers {attrs}"); }
    for _ in 0..attrs {
        r.take(8)?; // attribute FName
        r.i32()?; // ComponentCount
        r.i32()?; // PixelFormat
        r.i32()?; // ComponentStride
        read_bulk_array(r, "attribute values")?;
    }
    if t { eprintln!("    half edge @ {}", r.o); }
    // The mesh-deformer half-edge buffer, behind its own strip flag.
    let half_edge_global = r.u8()?;
    let half_edge_class = r.u8()?;
    let _ = half_edge_global;
    if half_edge_class & 1 == 0 {
        // Both are `TResourceArray<int32>` written with `Ar <<`: a bare count.
        for what in ["VertexToEdgeData", "EdgeToTwinEdgeData"] {
            let n = native_count(r, what)?;
            r.take(n * 4)?;
        }
    }
    Ok(())
}

/// `FSkeletalMeshLODRenderData::SerializeAvailabilityInfo` — the metadata a
/// streamed LOD leaves behind in the export when its buffers went to `.ubulk`.
fn read_skel_availability_info(r: &mut Reader, has_cloth: bool) -> Result<()> {
    r.u8()?; // FMultiSizeIndexContainer::DataTypeSize
    r.i32()?; // index buffer NumIndices
    // FStaticMeshVertexBuffer metadata comes before the position buffer's here,
    // the opposite order to SerializeStreamedData.
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    r.i32()?; // FPositionVertexBuffer::Stride
    r.i32()?; // FPositionVertexBuffer::NumVertices
    r.i32()?; // FColorVertexBuffer::Stride
    r.u32()?; // FColorVertexBuffer::NumVertices
    r.u32()?; // bVariableBonesPerVertex
    r.u32()?; // MaxBoneInfluences
    r.u32()?; // NumBoneWeights
    r.u32()?; // NumVertices
    r.u32()?; // bUse16BitBoneIndex
    r.u32()?; // bUse16BitBoneWeight
    r.u32()?; // FSkinWeightLookupVertexBuffer::NumVertices
    if has_cloth {
        let n = native_count(r, "ClothIndexMapping")?;
        r.take(n * 12)?;
        r.i32()?; // Stride
        r.u32()?; // NumVertices
    }
    let profiles = native_count(r, "SkinWeightProfileNames")?;
    r.take(profiles * 8)?;
    Ok(())
}

/// One `FSkeletalMeshLODRenderData`.
fn read_skel_lod(r: &mut Reader, has_vertex_colors: bool, bulk_data: &[(i64, i64)]) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    let _ = class_strip;
    let cooked_out = r.u32()? != 0;
    let inlined = r.u32()? != 0;
    let req = native_count(r, "RequiredBones")?;
    r.take(req * 2)?;
    // Everything below is skipped for a server cook or a LOD below the minimum.
    // `EStrippedData::AudioVisual` is bit 1 — bit 0 is `EditorOnly`, which every
    // client cook sets and which must NOT suppress the render buffers.
    if global_strip & 2 != 0 || cooked_out {
        return Ok(());
    }
    let nsec = native_count(r, "RenderSections")?;
    let mut has_cloth = false;
    for _ in 0..nsec {
        has_cloth |= read_skel_render_section(r)?;
    }
    let active = native_count(r, "ActiveBoneIndices")?;
    r.take(active * 2)?;
    r.u32()?; // BuffersSize
    if inlined {
        read_skel_streamed_data(r, has_vertex_colors, has_cloth)?;
    } else {
        // The buffers went to `.ubulk`; only the bulk-data header and the
        // availability metadata stay in the export. A zero-size payload means
        // the LOD was discarded outright and no metadata follows.
        let index = r.i32()?;
        let size = bulk_data.get(index.max(0) as usize).map(|&(_, s)| s).unwrap_or(0);
        if size != 0 {
            read_skel_availability_info(r, has_cloth)?;
        }
    }
    Ok(())
}

/// One RigLogic DNA stream, as `UDNAAsset::Serialize` reads it.
///
/// The DNA container is a foreign format embedded verbatim in the export: a
/// three-byte `DNA` signature, a generation/version pair, a section index, then
/// the sections themselves. Two things make it unlike anything else here —
/// **every scalar in it is big-endian**, and nothing records the stream's total
/// length. Its size is therefore the furthest section end, with section offsets
/// measured from the signature.
///
/// Measured on `SK_Samuel_Marcus_Head_Gameplay`: generation 2, version 5, nine
/// sections (`desc`, `defn`, `bhvr`, `geom`, `mlbh`, `rbfb`, `rbfe`, `jbmd`, …),
/// whose index ends at exactly `desc`'s offset of 155.
/// `UNiagaraScript::SerializeNiagaraShaderMaps` and everything it reaches:
/// `FNiagaraShaderScript::SerializeShaderMap` → `FShaderMapBase::Serialize` →
/// `FMemoryImageResult::LoadFromArchive`.
///
/// This looked like the one part of the corpus that could not be walked, because
/// the payload is a *frozen memory image* — a raw dump of C++ objects whose
/// layout depends on the target platform. But none of it needs interpreting: the
/// frozen image and the shader bytecode are opaque blobs with explicit lengths,
/// and every table around them is a plain count. So the structure is walkable
/// end to end without modelling a single shader.
fn read_niagara_shader_maps(r: &mut Reader) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    let resources = native_count(r, "Niagara shader resources")?;
    if t { eprintln!("    resources {resources} of {} bytes", r.b.len()); }
    for _ in 0..resources {
        let cooked = r.u32()? != 0;
        r.i32()?; // NumPermutations
        let hash = native_count(r, "BaseCompileHash")?;
        r.take(hash)?;
        // An uncooked resource writes nothing more, and a cooked one still says
        // whether a shader map compiled successfully.
        if !cooked || r.u32()? == 0 {
            continue;
        }
        read_shader_map(r, true)?;
    }
    Ok(())
}

/// `FShaderMapBase::Serialize`'s load path (ShaderMap.cpp:238) — the frozen
/// memory image, its pointer table, and the shader code. Shared by every asset
/// type that embeds a cooked shader map.
///
/// `niagara_pointer_table` selects `FNiagaraShaderMapPointerTable`, which
/// appends the data-interface class names its shaders bind to as `FString`s
/// after the base table's hashed names. Omitting them desyncs hundreds of bytes
/// later, inside the patch tables, where the symptom looks nothing like the
/// cause.
fn read_shader_map(r: &mut Reader, niagara_pointer_table: bool) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    // FMemoryImageResult::LoadFromArchive.
    if t { eprintln!("    layout params @ {}", r.o); }
    r.take(8)?; // FPlatformTypeLayoutParameters: MaxFieldAlignment, Flags
    let frozen = r.u32()? as usize;
    r.take(frozen)?;
    // FShaderMapPointerTable::LoadFromArchive: the base class's type
    // dependencies, then the shader and vertex-factory type names.
    if t { eprintln!("    pointer table @ {}", r.o); }
    let deps = native_count(r, "memory image type dependencies")?;
    r.take(deps * 32)?; // FName + uint32 layout size + FSHAHash
    let types = native_count(r, "shader types")?;
    let vf_types = native_count(r, "vertex factory types")?;
    r.take((types + vf_types) * 8)?; // FHashedName
    if niagara_pointer_table {
        let di_types = native_count(r, "data interface types")?;
        for _ in 0..di_types {
            r.fstring()?;
        }
    }
    // The three patch tables are counted up front, then listed in order.
    if t { eprintln!("    patch counts @ {}", r.o); }
    let vtables = native_count(r, "vtable patch tables")?;
    let script_names = native_count(r, "script name patch tables")?;
    let image_names = native_count(r, "memory image name patch tables")?;
    for _ in 0..vtables {
        r.take(8)?; // TypeNameHash
        let n = native_count(r, "vtable patches")?;
        r.take(n * 8)?; // VTableOffset + Offset
    }
    for _ in 0..(script_names + image_names) {
        r.take(8)?; // FName
        let n = native_count(r, "name patches")?;
        r.take(n * 4)?; // Offset
    }
    if t { eprintln!("    share code @ {}", r.o); }
    let share_code = r.u32()? != 0;
    r.take(8)?; // ShaderPlatformName
    if share_code {
        // The code lives in a shared shader library; only its hash is here.
        r.take(20)?; // FSHAHash
    } else {
        // FShaderMapResourceCode::Serialize — an inline copy of the bytecode.
        r.take(20)?; // ResourceHash
        let hashes = native_count(r, "shader hashes")?;
        r.take(hashes * 20)?;
        let code = native_count(r, "shader code resources")?;
        if t { eprintln!("    {code} code resources @ {}", r.o); }
        for _ in 0..code {
            // FShaderCodeResource is two FSharedBuffers, each a uint64 length
            // then that many bytes.
            for _ in 0..2 {
                let len = usize::try_from(r.u64()?).context("implausible shader buffer")?;
                r.take(len)?;
            }
        }
    }
    Ok(())
}

/// On-disk size of one `EPCGMetadataTypes` value. The vector, quaternion,
/// rotator and transform types are the LWC double variants. `None` for the
/// variable-length types (`String`, `SoftObjectPath`, `SoftClassPath`), which no
/// CE attribute uses — a run that hits one reports an unmodeled tail rather than
/// guessing at it.
fn pcg_value_size(type_id: i32) -> Option<usize> {
    Some(match type_id {
        0 => 4,   // Float
        1 => 8,   // Double
        2 => 4,   // Integer32
        3 => 8,   // Integer64
        4 => 16,  // Vector2
        5 => 24,  // Vector
        6 => 32,  // Vector4
        7 => 32,  // Quaternion
        8 => 80,  // Transform
        10 => 4,  // Boolean — an FArchive bool is 32-bit. See pcg_array_element_size.
        11 => 24, // Rotator
        12 => 8,  // Name
        _ => return None,
    })
}

/// The same value's size *inside the `Values` array*, which is not always
/// [`pcg_value_size`].
///
/// `TArray<T>::operator<<` bulk-serializes whenever `sizeof(T) == 1`, so a
/// `TArray<bool>` is written one **byte** per element — while the sibling
/// `DefaultValue` goes through `FArchive::operator<<(bool&)` and is written as
/// a 32-bit int. Same type, same function, two sizes four bytes apart; reading
/// the array at 4 bytes an element sails past the end of one attribute into
/// the next and only surfaces as an implausible count thousands of bytes later.
fn pcg_array_element_size(type_id: i32) -> Option<usize> {
    match type_id {
        10 => Some(1), // Boolean
        other => pcg_value_size(other),
    }
}

fn dna_be32(b: &[u8], o: usize) -> Result<usize> {
    let s = b.get(o..o + 4).context("DNA read past end")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize)
}

/// The absolute end of the DNA stream starting at `start`, when the container
/// records section sizes — which only version 5 and later do. Returns `Ok(None)`
/// for an older header, whose length its own bytes cannot give.
fn dna_stream_end(b: &[u8], start: usize) -> Result<Option<usize>> {
    if b.get(start..start + 3) != Some(b"DNA".as_slice()) {
        bail!("no DNA signature @ {start}");
    }
    let ver = b.get(start + 5..start + 7).context("DNA version past end")?;
    if u16::from_be_bytes([ver[0], ver[1]]) < 5 {
        return Ok(None);
    }
    let count = dna_be32(b, start + 7)?;
    if count > 256 {
        bail!("implausible DNA section count {count} @ {start}");
    }
    let mut end = 0usize;
    for i in 0..count {
        // Each index entry: a four-character id, its generation and version,
        // then the section's offset and size — offsets measured from `start`.
        let p = start + 11 + i * 16;
        end = end.max(dna_be32(b, p + 8)?.saturating_add(dna_be32(b, p + 12)?));
    }
    let stop = start.checked_add(end).filter(|e| *e > start && *e <= b.len());
    Ok(Some(stop.with_context(|| format!("DNA stream ends past the export @ {start}"))?))
}

/// The furthest section offset an unsized (pre-version-5) DNA header records.
/// Its table is eight bare `uint32` offsets and no sizes, so this only bounds
/// where the stream's data must still be running.
fn dna_unsized_floor(b: &[u8], start: usize) -> Result<usize> {
    let mut m = 0usize;
    for i in 0..8 {
        m = m.max(dna_be32(b, start + 7 + i * 4)?);
    }
    Ok(start + m)
}

/// `FReferenceSkeleton`'s `operator<<`: the bone info array, the rest pose, and
/// the name-to-index map. Returns the `FTransform` size it settled on, since the
/// callers that read further pose arrays need the same one.
///
/// `FTransform` is 80 bytes when the engine is built with LWC doubles and 40
/// with floats, and nothing in the stream says which. Disambiguate by which
/// choice leaves the following map count reading back as the bone count — the
/// same test `skeletal_mesh.rs` makes.
fn read_reference_skeleton(r: &mut Reader) -> Result<usize> {
    let nbones = native_count(r, "RawRefBoneInfo")?;
    r.take(nbones * 12)?; // FMeshBoneInfo: FName + i32 ParentIndex
    let npose = native_count(r, "RawRefBonePose")?;
    let tsize = if npose == 0 {
        80
    } else {
        [80usize, 40]
            .into_iter()
            .find(|&ts| {
                r.b.get(r.o + npose * ts..r.o + npose * ts + 4)
                    .and_then(|s| s.try_into().ok())
                    .map(|s| i32::from_le_bytes(s) == nbones as i32)
                    .unwrap_or(false)
            })
            .context("could not size FTransform in FReferenceSkeleton")?
    };
    r.take(npose * tsize)?;
    let nmap = native_count(r, "RawRefBoneNameToIndexMap")?;
    r.take(nmap * 12)?; // FName + i32
    Ok(tsize)
}

/// A natively-serialized array count, with a plausibility guard so a desync
/// fails loudly instead of allocating wildly.
/// An `FByteBulkData` whose payload the cook forced inline.
///
/// In a Zen package the bulk-data *header* is just an `int32` index into the
/// package's bulk-data map; the payload, when inlined, follows immediately.
/// Checking the mapped offset against the cursor is what distinguishes the two
/// — a payload that lives in the sibling `.ubulk` must be left alone.
fn read_inline_bulk_data(r: &mut Reader, bulk_data: &[(i64, i64)], what: &str) -> Result<()> {
    let index = r.i32()?;
    let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) else {
        bail!("{what}: bulk data index {index} out of range");
    };
    if offset as usize == r.o {
        r.take(size.max(0) as usize)?;
    }
    Ok(())
}

/// The delta-serialization prefix shared by `TSet` and `TMap`: a count of
/// entries to remove, followed by that many keys/elements. `INDEX_NONE` means
/// the container is replaced wholesale and nothing follows.
fn read_container_removals(
    r: &mut Reader,
    what: &str,
    mut read_one: impl FnMut(&mut Reader) -> Result<()>,
) -> Result<()> {
    let n = r.i32()?;
    if n == -1 {
        return Ok(());
    }
    if !(0..=1_000_000).contains(&n) {
        bail!("implausible {what} removal count {n} @ {}", r.o - 4);
    }
    for _ in 0..n {
        read_one(r)?;
    }
    Ok(())
}

fn native_count(r: &mut Reader, what: &str) -> Result<usize> {
    let n = r.i32()?;
    if !(0..=10_000_000).contains(&n) {
        bail!("implausible {what} count {n} @ {}", r.o - 4);
    }
    Ok(n as usize)
}

/// `FNiagaraDataInterfaceGeneratedFunction`: definition `FName`, instance
/// `FString`, `(FName, FName)` specifiers, the variadic input/output references,
/// and a `uint16` usage mask.
fn read_niagara_generated_function(r: &mut Reader) -> Result<PropValue> {
    let mut s = BTreeMap::new();
    s.insert("DefinitionName".to_string(), PropValue::Name(r.name()?));
    s.insert("InstanceName".to_string(), PropValue::Str(r.fstring()?));
    let n = native_count(r, "Specifiers")?;
    let mut spec = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let k = PropValue::Name(r.name()?);
        let v = PropValue::Name(r.name()?);
        spec.push(PropValue::Array(vec![k, v]));
    }
    s.insert("Specifiers".to_string(), PropValue::Array(spec));
    // Each variadic entry is an `FNiagaraVariableCommonReference`: an `FName`
    // and an `FPackageIndex`.
    for field in ["VariadicInputs", "VariadicOutputs"] {
        let n = native_count(r, field)?;
        let mut v = Vec::with_capacity(n.min(4096));
        for _ in 0..n {
            let mut e = BTreeMap::new();
            e.insert("Name".to_string(), PropValue::Name(r.name()?));
            e.insert("UnderlyingType".to_string(), PropValue::Object(r.i32()?));
            v.push(PropValue::Struct(e));
        }
        s.insert(field.to_string(), PropValue::Array(v));
    }
    // No trailing `MiscUsageBitMask`: that field is gated on a later Niagara
    // custom version than this build. Measured on `NS_collision`, where the
    // second generated function's `FName` begins immediately after the variadic
    // output count — two bytes earlier than the bitmask would allow.
    Ok(PropValue::Struct(s))
}

/// The payload struct a universal-object-locator fragment type serializes, by
/// its registered `FName`. An empty name means the fragment carries no payload.
///
/// `subobj` is the only type this build's content uses — swept across all 121
/// `LevelSequence` packages. Anything else surfaces as an error naming the
/// unmapped type rather than silently mis-consuming the stream.
fn locator_fragment_payload(fragment_type: &str) -> Option<&'static str> {
    Some(match fragment_type {
        "subobj" => "SubObjectLocator",
        "actor" => "ActorLocatorFragment",
        _ => return None,
    })
}

/// `FText`: `uint32 Flags`, an `int8` history type, then that history's own
/// payload.
///
/// Derived from `DA_VideoHDRSettingsItems`, where the whole 65-byte export
/// resolves exactly: `00 00 00 00` (Flags), `0b` (history type 11 =
/// `StringTableEntry`), the table `FName`, and the 31-byte key
/// `"settings_header_controlpresets"`. Reading the fields in the other order
/// would make Flags `0x0b000000`, which is how the order was settled.
///
/// Unmodeled history types surface as an error naming the type number rather
/// than silently mis-consuming the stream.
fn read_text(r: &mut Reader, depth: usize) -> Result<PropValue> {
    if depth > 16 {
        bail!("FText nesting too deep @ {}", r.o);
    }
    let mut s = BTreeMap::new();
    s.insert("Flags".to_string(), PropValue::Int(r.u32()? as i64));
    let history = r.u8()? as i8;
    s.insert("HistoryType".to_string(), PropValue::Int(history as i64));
    match history {
        // `ETextHistoryType::None` still writes a four-byte
        // `bHasCultureInvariantString` (and the string itself when set).
        // Measured on `NS_collision`: skipping it left the emitter four bytes
        // adrift, and consuming it lands `FixedBounds` exactly on a ±1000 box
        // with an empty event-handler array right after.
        -1 => {
            if r.u32()? != 0 {
                s.insert("CultureInvariantString".to_string(), PropValue::Str(r.fstring()?));
            }
        }
        // `Base`: namespace, key and source string.
        0 => {
            s.insert("Namespace".to_string(), PropValue::Str(r.fstring()?));
            s.insert("Key".to_string(), PropValue::Str(r.fstring()?));
            s.insert("SourceString".to_string(), PropValue::Str(r.fstring()?));
        }
        // `StringTableEntry`: the table id and the row key.
        11 => {
            s.insert("TableId".to_string(), PropValue::Name(r.name()?));
            s.insert("Key".to_string(), PropValue::Str(r.fstring()?));
        }
        // `OrderedFormat`: the source format text, then **positional**
        // arguments — bare values, with none of the names `ArgumentDataFormat`
        // carries. (`FTextHistory_Generated`, which both derive from, writes
        // nothing itself.)
        2 => {
            s.insert("SourceFmt".to_string(), read_text(r, depth + 1)?);
            let n = native_count(r, "FText ordered arguments")?;
            let mut args = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                args.push(read_format_argument(r, depth + 1)?);
            }
            s.insert("Arguments".to_string(), PropValue::Array(args));
        }
        // `NamedFormat` (a `TMap<FString, FFormatArgumentValue>`) and
        // `ArgumentDataFormat` (a `TArray<FFormatArgumentData>`) both come out
        // as a count followed by name/value pairs. Only the latter has been
        // seen in Campaign Evolved.
        1 | 3 => {
            s.insert("SourceFmt".to_string(), read_text(r, depth + 1)?);
            let n = native_count(r, "FText arguments")?;
            let mut args = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                let mut a = BTreeMap::new();
                a.insert("ArgumentName".to_string(), PropValue::Str(r.fstring()?));
                a.insert("ArgumentValue".to_string(), read_format_argument(r, depth + 1)?);
                args.push(PropValue::Struct(a));
            }
            s.insert("Arguments".to_string(), PropValue::Array(args));
        }
        // `AsNumber`/`AsPercent`/`AsCurrency`: the source value, optional
        // number-formatting options, and the target culture. `AsCurrency` leads
        // with the currency code.
        4 | 5 | 6 => {
            if history == 6 {
                s.insert("CurrencyCode".to_string(), PropValue::Str(r.fstring()?));
            }
            s.insert("SourceValue".to_string(), read_format_argument(r, depth + 1)?);
            if r.u32()? != 0 {
                // `FNumberFormattingOptions`: three `FArchive` bools, a rounding
                // mode, then four digit counts.
                let mut o = BTreeMap::new();
                o.insert("AlwaysSign".to_string(), PropValue::Bool(r.u32()? != 0));
                o.insert("UseGrouping".to_string(), PropValue::Bool(r.u32()? != 0));
                o.insert("RoundingMode".to_string(), PropValue::Int(r.u8()? as i64));
                for f in [
                    "MinimumIntegralDigits",
                    "MaximumIntegralDigits",
                    "MinimumFractionalDigits",
                    "MaximumFractionalDigits",
                ] {
                    o.insert(f.to_string(), PropValue::Int(r.i32()? as i64));
                }
                s.insert("FormatOptions".to_string(), PropValue::Struct(o));
            }
            s.insert("TargetCulture".to_string(), PropValue::Str(r.fstring()?));
        }
        other => bail!("FText history type {other} not modeled (@ {})", r.o - 1),
    }
    Ok(PropValue::Struct(s))
}

/// `FFormatArgumentValue`: an `EFormatArgumentType` tag then the value.
fn read_format_argument(r: &mut Reader, depth: usize) -> Result<PropValue> {
    let ty = r.u8()? as i8;
    Ok(match ty {
        0 => PropValue::Int(r.u64()? as i64), // Int (64-bit in this stream version)
        1 => PropValue::Int(r.u64()? as i64), // UInt
        2 => PropValue::Float(r.f32()? as f64),
        3 => PropValue::Float(r.f64()?),
        4 => read_text(r, depth)?,
        other => bail!("FText format argument type {other} not modeled (@ {})", r.o - 1),
    })
}

/// A natively-serialized `TArray<int32>`: count then that many `int32`s.
fn read_native_i32_array(r: &mut Reader) -> Result<PropValue> {
    let n = r.i32()?;
    if !(0..=100_000_000).contains(&n) {
        bail!("implausible native array count {n} @ {}", r.o - 4);
    }
    let mut v = Vec::with_capacity((n as usize).min(4096));
    for _ in 0..n {
        v.push(PropValue::Int(r.i32()? as i64));
    }
    Ok(PropValue::Array(v))
}

/// `FWeightedRandomSampler`: `TArray<float> Prob`, `TArray<int32> Alias`,
/// `float TotalWeight`.
fn read_weighted_random_sampler(r: &mut Reader) -> Result<PropValue> {
    let count = |r: &mut Reader| -> Result<usize> {
        let n = r.i32()?;
        if !(0..=100_000_000).contains(&n) {
            bail!("implausible sampler array count {n} @ {}", r.o - 4);
        }
        Ok(n as usize)
    };
    let n = count(r)?;
    let mut prob = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        prob.push(PropValue::Float(r.f32()? as f64));
    }
    let n = count(r)?;
    let mut alias = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        alias.push(PropValue::Int(r.i32()? as i64));
    }
    let mut s = BTreeMap::new();
    s.insert("Prob".to_string(), PropValue::Array(prob));
    s.insert("Alias".to_string(), PropValue::Array(alias));
    s.insert("TotalWeight".to_string(), PropValue::Float(r.f32()? as f64));
    Ok(PropValue::Struct(s))
}

/// Little-endian byte-cursor over an export's serial data.
struct Reader<'a> {
    b: &'a [u8],
    o: usize,
    names: &'a [String],
    /// The `FField` chain this export defined, if it is `UStruct`-derived.
    ///
    /// `UUserDefinedStruct` writes a default instance of *itself* after its
    /// `UStruct` body, so the schema its property block indexes by is not in
    /// the `.usmap` at all — it is the chain that was just parsed a few bytes
    /// earlier. Stashing it here is what lets a later class in the same chain
    /// walk that block.
    struct_fields: Option<Vec<UsmapProperty>>,
    /// Resolves references out of this package (see [`ExportContext`]).
    resolver: Option<&'a dyn PackageResolver>,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8], names: &'a [String]) -> Self {
        Reader { b, o: 0, names, struct_fields: None, resolver: None }
    }
    fn with_ctx(b: &'a [u8], names: &'a [String], ctx: &ExportContext<'a>) -> Self {
        Reader { resolver: ctx.resolver, ..Reader::new(b, names) }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .b
            .get(self.o..self.o + n)
            .with_context(|| format!("unversioned read past end (+{n} @ {})", self.o))?;
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// `FName`: `i32 index` + `i32 number`, resolved against the package name
    /// map. A non-zero number appends `_{number-1}`, per UE convention.
    fn name(&mut self) -> Result<String> {
        let idx = self.i32()?;
        let number = self.i32()?;
        let base = usize::try_from(idx)
            .ok()
            .and_then(|i| self.names.get(i))
            .with_context(|| format!("FName index {idx} out of range (@ {})", self.o - 8))?;
        Ok(if number > 0 {
            format!("{base}_{}", number - 1)
        } else {
            base.clone()
        })
    }
    /// `FString`: `i32 len`; positive = UTF-8 (NUL-terminated), negative =
    /// UTF-16 (len is negated char count).
    fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let bytes = self.take(n as usize)?;
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
        } else {
            let chars = (-n) as usize;
            let bytes = self.take(chars * 2)?;
            let u16s: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            Ok(String::from_utf16_lossy(&u16s))
        }
    }
}

/// One `FUnversionedHeader` fragment.
struct Fragment {
    skip: u8,
    has_zeroes: bool,
    value_num: u8,
    is_last: bool,
}

impl Fragment {
    fn unpack(p: u16) -> Self {
        Fragment {
            skip: (p & 0x7f) as u8,
            has_zeroes: (p & 0x80) != 0,
            is_last: (p & 0x100) != 0,
            value_num: (p >> 9) as u8,
        }
    }
}

/// Read an `FUnversionedHeader`, returning `(present_schema_indices, ...)`
/// where each present index is paired with whether its value is non-zero (a
/// zero-masked property serializes no bytes — it is the zero value).
fn read_header(r: &mut Reader) -> Result<Vec<(usize, bool)>> {
    let mut frags = Vec::new();
    let mut zero_mask_num = 0usize;
    loop {
        let frag = Fragment::unpack(r.u16()?);
        if frag.has_zeroes {
            zero_mask_num += frag.value_num as usize;
        }
        let last = frag.is_last;
        frags.push(frag);
        if last {
            break;
        }
    }
    // Zero mask: one bit per value in has-zeroes fragments.
    let mut zero_mask = Vec::with_capacity(zero_mask_num);
    if zero_mask_num > 0 {
        let (words, word_bits): (Vec<u32>, usize) = if zero_mask_num <= 8 {
            (vec![r.u8()? as u32], 8)
        } else if zero_mask_num <= 16 {
            (vec![r.u16()? as u32], 16)
        } else {
            let n = zero_mask_num.div_ceil(32);
            let mut w = Vec::with_capacity(n);
            for _ in 0..n {
                w.push(r.u32()?);
            }
            (w, 32)
        };
        for i in 0..zero_mask_num {
            zero_mask.push((words[i / word_bits] >> (i % word_bits)) & 1 == 1);
        }
    }

    let mut present = Vec::new();
    let mut schema_it = 0usize;
    let mut zi = 0usize;
    for frag in &frags {
        schema_it += frag.skip as usize;
        for _ in 0..frag.value_num {
            let non_zero = if frag.has_zeroes {
                let nz = !zero_mask[zi];
                zi += 1;
                nz
            } else {
                true
            };
            present.push((schema_it, non_zero));
            schema_it += 1;
        }
    }
    Ok(present)
}

/// Whether to narrate the walk to stderr (`BLAM_UNVERSIONED_TRACE=1`).
///
/// A desync in this reader is silent by construction — misread bytes still
/// decode as plausible values, and the failure only surfaces much later as an
/// implausible array count. The only reliable way to diagnose one is to watch
/// each property's byte range against the raw export, so that view is built in
/// rather than reconstructed by hand each time.
fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("BLAM_UNVERSIONED_TRACE").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

/// Read a full reflected struct/class instance (its unversioned property
/// block) named `class`, returning present property name→value.
fn read_struct(r: &mut Reader, class: &str, usmap: &Usmap, depth: usize) -> Result<BTreeMap<String, PropValue>> {
    if depth > 32 {
        bail!("unversioned struct nesting too deep at {class}");
    }
    // A `UUserDefinedStruct` used as a property type is in no `.usmap` under
    // any name; its layout has to come back out of the package that defines it.
    if usmap.get(class).is_none() {
        if let Some(fields) = r.resolver.and_then(|p| p.struct_layout(class)) {
            let schema: Vec<&UsmapProperty> = fields.iter().collect();
            return read_struct_with_schema(r, class, &schema, usmap, depth);
        }
    }
    let flat = flattened_schema(class, usmap)?;
    read_struct_with_schema(r, class, &flat, usmap, depth)
}

/// The flattened property schema the unversioned fragment stream indexes by.
fn flattened_schema<'u>(class: &str, usmap: &'u Usmap) -> Result<Vec<&'u UsmapProperty>> {
    // Campaign Evolved ships `Blam*TagDataAsset` classes that appear in neither
    // the `.usmap` nor the UHT dump (`BlamFrameEventListTagDataAsset` alone
    // covers 130 exports). They add no properties of their own over the shared
    // base, so decoding them against `BlamTagDataAssetBase` recovers the whole
    // property block rather than failing outright.
    usmap
        .flattened_properties(class)
        .or_else(|| {
            (class.starts_with("Blam") && class.ends_with("TagDataAsset"))
                .then(|| usmap.flattened_properties("BlamTagDataAssetBase"))
                .flatten()
        })
        .with_context(|| format!("no .usmap schema for struct {class}"))
}

/// Walk one unversioned property block against an explicit schema.
///
/// Split out of [`read_struct`] because not every schema comes from the
/// `.usmap`: a `UUserDefinedStruct`'s default instance and a `UDataTable`'s
/// rows are indexed by a property list recovered from *package* bytes.
/// `label` only names the block in errors and traces.
fn read_struct_with_schema(
    r: &mut Reader,
    label: &str,
    flat: &[&UsmapProperty],
    usmap: &Usmap,
    depth: usize,
) -> Result<BTreeMap<String, PropValue>> {
    let class = label;
    let header_start = r.o;
    let present = read_header(r)?;
    if trace_enabled() {
        eprintln!(
            "{:indent$}{class} @ {header_start} (header {} bytes, present {:?})",
            "",
            r.o - header_start,
            present,
            indent = depth * 2
        );
    }
    let mut out = BTreeMap::new();
    for (idx, non_zero) in present {
        let prop = flat
            .get(idx)
            .with_context(|| format!("{class}: present schema index {idx} beyond {} props", flat.len()))?;
        let start = r.o;
        let value = if non_zero {
            read_value(r, &prop.ty, usmap, depth, false)?
        } else {
            // Zero-masked: the property is its zero value, no bytes consumed.
            zero_value(&prop.ty)
        };
        if trace_enabled() {
            eprintln!(
                "{:indent$}  [{idx}] {} : {:?} @ {start}..{}{}",
                "",
                prop.name,
                prop.ty,
                r.o,
                if non_zero { "" } else { " (zero-masked)" },
                indent = depth * 2
            );
        }
        out.insert(prop.name.clone(), value);
    }
    Ok(out)
}

/// The implicit "zero" for a zero-masked property (no bytes were serialized).
fn zero_value(ty: &PropertyType) -> PropValue {
    match ty {
        PropertyType::Bool => PropValue::Bool(false),
        PropertyType::Int
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { .. } => PropValue::Int(0),
        PropertyType::Float | PropertyType::Double => PropValue::Float(0.0),
        PropertyType::Object | PropertyType::Interface => PropValue::Object(0),
        _ => PropValue::Opaque,
    }
}

/// Reads one property value.
///
/// `in_container` distinguishes the two ways UE reaches a value, which matters
/// for enums and only for enums. At the top level of an unversioned property
/// block, `CanSerializeAsInteger` (UnversionedPropertySerialization.cpp:216)
/// claims every `FNumericProperty` and `FEnumProperty`, so the value is written
/// as a raw integer of the property's alignment width. Inside a container the
/// element is reached through `FArrayProperty`/`FSetProperty`/`FMapProperty`'s
/// `SerializeItem`, which calls the inner property's own `SerializeItem` — and
/// both `FEnumProperty::SerializeItem` (EnumProperty.cpp:275) and
/// `FByteProperty::SerializeItem` (PropertyByte.cpp:51) write the enumerator's
/// **`FName`** so that reordering an enum cannot corrupt saved data. Same
/// property, one byte or eight, depending only on where it sits.
fn read_value(
    r: &mut Reader,
    ty: &PropertyType,
    usmap: &Usmap,
    depth: usize,
    in_container: bool,
) -> Result<PropValue> {
    Ok(match ty {
        PropertyType::Bool => PropValue::Bool(r.u8()? != 0),
        // A `FByteProperty` that names an enum goes out by name inside a
        // container; a plain byte is always one byte.
        PropertyType::Byte { enum_name: Some(_) } if in_container => PropValue::Name(r.name()?),
        PropertyType::Byte { .. } | PropertyType::Int8 => PropValue::Int(r.u8()? as i64),
        PropertyType::Int => PropValue::Int(r.i32()? as i64),
        PropertyType::UInt32 => PropValue::Int(r.u32()? as i64),
        PropertyType::Int16 | PropertyType::UInt16 => PropValue::Int(r.u16()? as i64),
        PropertyType::Int64 | PropertyType::UInt64 => PropValue::Int(r.u64()? as i64),
        PropertyType::Float => PropValue::Float(r.f32()? as f64),
        PropertyType::Double => PropValue::Float(r.f64()?),
        PropertyType::Name => PropValue::Name(r.name()?),
        PropertyType::Str | PropertyType::Utf8Str | PropertyType::AnsiStr => PropValue::Str(r.fstring()?),
        PropertyType::Enum { inner, .. } => {
            if in_container {
                PropValue::Name(r.name()?)
            } else {
                // Top level: the raw underlying integer.
                read_value(r, inner, usmap, depth, false)?
            }
        }
        PropertyType::Object
        | PropertyType::WeakObject
        | PropertyType::LazyObject
        | PropertyType::Interface => PropValue::Object(r.i32()?),
        PropertyType::SoftObject | PropertyType::AssetObject => {
            let package = r.name()?;
            let asset = r.name()?;
            let sub_path = r.fstring()?;
            PropValue::SoftObject(SoftObjectPath { package, asset, sub_path })
        }
        PropertyType::Struct(name) => {
            if let Some(size) = native_struct_size(name) {
                PropValue::Native(r.take(size)?.to_vec())
            } else if let Some(v) = read_native_variable_struct(r, name, usmap, depth)? {
                v
            } else {
                PropValue::Struct(read_struct(r, name, usmap, depth + 1)?)
            }
        }
        PropertyType::Array(inner) => {
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible array count {n} @ {}", r.o - 4);
            }
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_value(r, inner, usmap, depth, true)?);
            }
            PropValue::Array(v)
        }
        // A `TSet` serializes like a `TMap`, not like a `TArray`: it opens with
        // an `NumElementsToRemove` delta-serialization prefix, and **that count
        // is followed by that many elements** — `FSetProperty::SerializeItem`
        // loads and discards them before reading the real `Num`
        // (PropertySet.cpp:258). A count of `INDEX_NONE` means "replace the
        // whole set" and carries no elements.
        PropertyType::Set(inner) => {
            read_container_removals(r, "set", |r| read_value(r, inner, usmap, depth, true).map(|_| ()))?;
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible set count {n} @ {}", r.o - 4);
            }
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_value(r, inner, usmap, depth, true)?);
            }
            PropValue::Array(v)
        }
        // `FMapProperty::SerializeItem`'s load path (PropertyMap.cpp:624):
        // `NumKeysToRemove`, then that many **keys**, then `NumEntries` and the
        // pairs. Reading the removal count without consuming its keys is
        // invisible while the count is zero — which it is for almost every
        // cooked asset — and desyncs catastrophically when it is not.
        PropertyType::Map(k, val) => {
            read_container_removals(r, "map", |r| read_value(r, k, usmap, depth, true).map(|_| ()))?;
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible map count {n} @ {}", r.o - 4);
            }
            let mut m = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let key = read_value(r, k, usmap, depth, true)?;
                let value = read_value(r, val, usmap, depth, true)?;
                m.push((key, value));
            }
            PropValue::Map(m)
        }
        PropertyType::Delegate => {
            // FScriptDelegate: object (FPackageIndex) + function FName.
            r.i32()?;
            r.name()?;
            PropValue::Opaque
        }
        PropertyType::MulticastDelegate => {
            let n = r.i32()?;
            for _ in 0..n.max(0) {
                r.i32()?;
                r.name()?;
            }
            PropValue::Opaque
        }
        PropertyType::FieldPath => {
            // TArray<FName> path + owner object.
            let n = r.i32()?;
            for _ in 0..n.max(0) {
                r.name()?;
            }
            r.i32()?;
            PropValue::Opaque
        }
        PropertyType::Text => read_text(r, 0)?,
        PropertyType::Optional(inner) => {
            // "Is set" is an `FArchive` bool, i.e. **four** bytes, then the
            // value. Measured on a `WorldPartitionRuntimeCellDataHashSet`, whose
            // optional `CellBounds` only resolves to a clean 12800×12800 box
            // with `IsValid = 1` under a four-byte flag.
            if r.u32()? != 0 {
                read_value(r, inner, usmap, depth, true)?
            } else {
                PropValue::Opaque
            }
        }
        PropertyType::Unknown(t) => bail!("unknown property kind {t} in unversioned stream"),
    })
}

// ---------------------------------------------------------------------------
// Typed Campaign Evolved mesh-sync extraction
// ---------------------------------------------------------------------------

/// A single mesh reference within a permutation.
#[derive(Debug, Clone)]
pub struct MeshRef {
    /// Full package path of the mesh asset (`/Game/.../SK_Marine_Torso_01`).
    pub package: String,
    /// Object name (`SK_Marine_Torso_01`).
    pub asset: String,
    /// Component class object name (`BPC_SkeletalMesh_C`,
    /// `BPC_HumanAnatomySkeletalMesh_C`, …), if present.
    pub class: String,
    /// Bone the (static) mesh attaches to, if any.
    pub parent_bone: String,
    /// The component's transform relative to `parent_bone` (identity when
    /// absent). Static pieces (e.g. a pelican's wings) offset from their bone
    /// need this applied on top of the bone's world rest transform.
    pub rel_transform: MeshTransform,
    /// Per-slot material overrides this instance applies to its mesh, as
    /// `(MaterialSlotName, override material-instance name)`. A variant (e.g.
    /// `brute_major`) overrides the base mesh's default slot materials by slot
    /// name; the effective material for a section is its override here, else the
    /// mesh's own default slot material. Empty when the instance uses defaults.
    pub material_overrides: Vec<(String, String)>,
}

/// One material slot on a mesh asset (`FStaticMaterial`/`FSkeletalMaterial`),
/// indexed by a section's `material_index`.
#[derive(Debug, Clone)]
pub struct MaterialSlot {
    /// `MaterialSlotName` — the key material overrides bind to.
    pub slot_name: String,
    /// The default material-instance object reference (`FPackageIndex`: negative
    /// = import, positive = export, 0 = none). The caller resolves it to a
    /// package/asset name through the mesh package's import table.
    pub material_object: i32,
}

/// Decode a mesh asset's material-slot array (`SkeletalMaterials` on a
/// `USkeletalMesh` / `StaticMaterials` on a `UStaticMesh`) from its export's
/// unversioned property block — the authoritative `material_index → (slot name,
/// default material)` mapping. Returns the slots in index order.
///
/// This decodes the mesh's reflected properties (not its native geometry, which
/// follows the property block); a schema surprise surfaces as an `Err` so the
/// caller can degrade to placeholder material names rather than corrupt output.
pub fn read_material_slots(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    is_skeletal: bool,
) -> Result<Vec<MaterialSlot>> {
    let class = if is_skeletal { "SkeletalMesh" } else { "StaticMesh" };
    let mut r = Reader::new(export, names);
    let s = read_struct(&mut r, class, usmap, 0)
        .with_context(|| format!("decoding {class} material slots"))?;
    // `UStaticMesh` names the array `StaticMaterials`; `USkeletalMesh` names it
    // plain `Materials` in this engine version (`SkeletalMaterials` is the older
    // spelling). Missing the live name silently yields zero slots, which reads as
    // "this mesh has no materials" rather than as a decode failure.
    let arr_names: &[&str] =
        if is_skeletal { &["Materials", "SkeletalMaterials"] } else { &["StaticMaterials", "Materials"] };
    let Some(arr) = arr_names.iter().find_map(|n| s.get(*n)).and_then(PropValue::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for el in arr {
        let es = el.as_struct();
        let slot_name = es
            .and_then(|e| e.get("MaterialSlotName"))
            .and_then(PropValue::as_str)
            .unwrap_or_default()
            .to_string();
        // FStaticMaterial::MaterialInterface / FSkeletalMaterial::MaterialInterface
        // (older cooks name the skeletal field `Material`).
        let material_object = es
            .and_then(|e| e.get("MaterialInterface").or_else(|| e.get("Material")))
            .and_then(|v| match v {
                PropValue::Object(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        out.push(MaterialSlot { slot_name, material_object });
    }
    Ok(out)
}

/// A permutation and the meshes it activates.
#[derive(Debug, Clone)]
pub struct Permutation {
    pub name: String,
    pub skeletal_meshes: Vec<MeshRef>,
    pub static_meshes: Vec<MeshRef>,
}

/// A region and its permutations (authoritative CE region→perm→mesh mapping).
#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    pub permutations: Vec<Permutation>,
}

/// The decoded `RuntimeRegions` map of a `BlamMeshSynchronizationComponent`.
#[derive(Debug, Clone, Default)]
pub struct MeshSyncRegions {
    pub regions: Vec<Region>,
    /// `SynchronizedActorType` (`EBlamMeshSynchronizedActorType`):
    /// `0`=WorldRepresentation, `1`=FirstPersonRepresentation. `None` when the
    /// property is unserialized (i.e. the default, WorldRepresentation). Use
    /// [`Self::is_world`] to pick the world (third-person) BP over the `FP`/CINE
    /// variants that share the same data asset.
    pub synchronized_actor_type: Option<i64>,
}

const COMPONENT_CLASS: &str = "BlamMeshSynchronizationComponent";

impl MeshSyncRegions {
    /// Whether this is the world/third-person representation (the one a preview
    /// wants) rather than a first-person or cinematic actor.
    pub fn is_world(&self) -> bool {
        self.synchronized_actor_type.unwrap_or(0) == 0
    }
}

impl MeshSyncRegions {
    /// Decode the authoritative region→permutation→mesh mapping from a cooked
    /// `BlamMeshSynchronizationComponent` export's serial bytes. `names` is the
    /// owning package's name map (`FNameMap::copy_raw_names`).
    pub fn from_component_export(export: &[u8], names: &[String], usmap: &Usmap) -> Result<Self> {
        let mut r = Reader::new(export, names);
        let comp = read_struct(&mut r, COMPONENT_CLASS, usmap, 0)
            .context("decoding BlamMeshSynchronizationComponent properties")?;
        // Anything left after the property block must be zero padding; a
        // non-zero trailing byte means the schema-driven walk desynced.
        if let Some(tail) = export.get(r.o..) {
            if let Some(off) = tail.iter().position(|&b| b != 0) {
                bail!(
                    "unversioned parse desynced: {} non-zero trailing bytes from offset {}",
                    tail.len() - off,
                    r.o + off
                );
            }
        }
        let synchronized_actor_type = comp.get("SynchronizedActorType").and_then(|v| match v {
            PropValue::Int(n) => Some(*n),
            _ => None,
        });
        let runtime_regions = comp
            .get("RuntimeRegions")
            .and_then(PropValue::as_map)
            .context("component has no serialized RuntimeRegions")?;

        let mut regions = Vec::new();
        for (region_key, region_val) in runtime_regions {
            let region_name = region_key.as_str().unwrap_or_default().to_string();
            let perms_map = region_val
                .as_struct()
                .and_then(|s| s.get("Permutations"))
                .and_then(PropValue::as_map);
            let mut permutations = Vec::new();
            if let Some(perms_map) = perms_map {
                for (perm_key, perm_val) in perms_map {
                    let perm_name = perm_key.as_str().unwrap_or_default().to_string();
                    let perm_struct = perm_val.as_struct();
                    let skeletal_meshes = perm_struct
                        .and_then(|s| s.get("SkeletalMeshes"))
                        .map(|v| collect_meshes(v))
                        .unwrap_or_default();
                    let static_meshes = perm_struct
                        .and_then(|s| s.get("StaticMeshes"))
                        .map(|v| collect_meshes(v))
                        .unwrap_or_default();
                    permutations.push(Permutation { name: perm_name, skeletal_meshes, static_meshes });
                }
            }
            regions.push(Region { name: region_name, permutations });
        }
        Ok(MeshSyncRegions { regions, synchronized_actor_type })
    }

    /// The set of skeletal meshes to render for a given `(region, permutation)`.
    /// Returns an empty slice when the region/perm has no skeletal mesh (e.g.
    /// `head`/`helmet` on characters whose head is an external MetaHuman).
    pub fn skeletal_meshes(&self, region: &str, permutation: &str) -> &[MeshRef] {
        self.regions
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(region))
            .and_then(|r| r.permutations.iter().find(|p| p.name.eq_ignore_ascii_case(permutation)))
            .map(|p| p.skeletal_meshes.as_slice())
            .unwrap_or(&[])
    }

    /// The set of rigid static meshes (each with a `parent_bone`) for a given
    /// `(region, permutation)` — vehicle/weapon parts attached to the skeleton.
    pub fn static_meshes(&self, region: &str, permutation: &str) -> &[MeshRef] {
        self.regions
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(region))
            .and_then(|r| r.permutations.iter().find(|p| p.name.eq_ignore_ascii_case(permutation)))
            .map(|p| p.static_meshes.as_slice())
            .unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// UUserDefinedStruct layout recovery + UDataTable row decoding
// ---------------------------------------------------------------------------
//
// Blueprint-generated row structs (e.g. `S_MetaHumanHeads`) are absent from the
// native-reflection `.usmap`, so a cooked `UDataTable`'s rows can't be decoded
// until we recover the row struct's property layout from its
// `UUserDefinedStruct` export and register it into the [`Usmap`].
//
// A cooked `UUserDefinedStruct` export serializes as: a UObject unversioned
// header (its own reflected `FGuid`), then native `UStruct` data — `SuperStruct`
// (i32), an empty `Children` array, a pad word, then the `FField` chain
// (`numFields: i32` followed by that many properties). Each `FField` record is:
//
//   propTypeName: FName   (e.g. "SoftObjectProperty")
//   Name: FName           (e.g. "Head_4_<guid>")
//   Flags: u32
//   ArrayDim: i32
//   ElementSize: i32
//   PropertyFlags: u64
//   RepIndex: u16
//   RepNotifyFunc: FName
//   BlueprintReplicationCondition: u8
//   <type-specific tail>
//
// with the type-specific tail carrying inner properties for containers
// (`ArrayProperty`→Inner, `MapProperty`→Key+Value, `SetProperty`→Element, each a
// nested `FField`) or a class ref for object/struct properties.

/// Strip UE's `_<index>_<32-hex-guid>` decoration (and a trailing `_Value`/`_Key`
/// map-element marker) from a `UUserDefinedStruct` field name, recovering the
/// author-facing base name (`Head_4_BFCB…` → `Head`).
fn deguid(name: &str) -> String {
    let stripped = name
        .strip_suffix("_Value")
        .or_else(|| name.strip_suffix("_Key"))
        .unwrap_or(name);
    let parts: Vec<&str> = stripped.split('_').collect();
    if parts.len() >= 3 {
        let guid = parts[parts.len() - 1];
        let idx = parts[parts.len() - 2];
        if guid.len() == 32
            && guid.bytes().all(|c| c.is_ascii_hexdigit())
            && !idx.is_empty()
            && idx.bytes().all(|c| c.is_ascii_digit())
        {
            return parts[..parts.len() - 2].join("_");
        }
    }
    stripped.to_string()
}

/// Read one `FField` (a `SerializeSingleField`): its type-name FName, then the
/// common `FProperty` header, then the type-specific tail. Returns
/// `(field_name, PropertyType, array_dim)`, or `None` for the `None`
/// terminator/null field.
fn read_single_field(r: &mut Reader) -> Result<Option<(String, PropertyType, u8)>> {
    let type_name = r.name()?;
    if type_name == "None" {
        return Ok(None);
    }
    let field_name = r.name()?;
    let _flags = r.u32()?;
    let array_dim = r.i32()?;
    let _element_size = r.i32()?;
    let _property_flags = r.u64()?;
    let _rep_index = r.u16()?;
    let _rep_notify = r.name()?;
    let _bp_rep_cond = r.u8()?;
    let ty = read_ffield_tail(r, &type_name)?;
    Ok(Some((field_name, ty, array_dim.clamp(1, 255) as u8)))
}

/// The `FProperty`-subclass-specific serialized tail, mapped to the [`PropertyType`]
/// the unversioned value reader understands.
fn read_ffield_tail(r: &mut Reader, type_name: &str) -> Result<PropertyType> {
    Ok(match type_name {
        "BoolProperty" => {
            // FieldSize, ByteOffset, ByteMask, FieldMask, BoolSize, bIsNativeBool.
            r.take(6)?;
            PropertyType::Bool
        }
        "SoftObjectProperty" => {
            r.i32()?; // PropertyClass
            PropertyType::SoftObject
        }
        // `FSoftClassProperty` derives from `FSoftObjectProperty`, so it writes
        // the base `PropertyClass` and then its own `MetaClass`
        // (`PropertySoftClassPtr.cpp`). Reading only one reference desyncs the
        // rest of the field chain.
        "SoftClassProperty" => {
            r.i32()?; // PropertyClass, from FObjectPropertyBase
            r.i32()?; // MetaClass
            PropertyType::SoftObject
        }
        "AssetObjectProperty" => {
            r.i32()?;
            PropertyType::AssetObject
        }
        "ObjectProperty" | "ObjectPtrProperty" => {
            r.i32()?; // PropertyClass
            PropertyType::Object
        }
        // `FClassProperty` (and the pointer variant that derives from it) adds a
        // `MetaClass` after the `FObjectPropertyBase` `PropertyClass`
        // (`PropertyClass.cpp`).
        "ClassProperty" | "ClassPtrProperty" => {
            r.i32()?; // PropertyClass, from FObjectPropertyBase
            r.i32()?; // MetaClass
            PropertyType::Object
        }
        "WeakObjectProperty" => {
            r.i32()?;
            PropertyType::WeakObject
        }
        "LazyObjectProperty" => {
            r.i32()?;
            PropertyType::LazyObject
        }
        "InterfaceProperty" => {
            r.i32()?;
            PropertyType::Interface
        }
        // `FStructProperty` stores only an `FPackageIndex` for the struct it
        // holds, so the *type name* the value reader needs lives outside this
        // export. With a resolver we can name it (and, for a struct that is
        // itself user-defined, carry its whole recovered layout across under a
        // synthetic name); without one the field stays unreadable, which is
        // reported rather than guessed.
        "StructProperty" => {
            let idx = r.i32()?;
            let name = r.resolver.and_then(|p| p.struct_name(idx)).unwrap_or_default();
            PropertyType::Struct(name)
        }
        "ByteProperty" => {
            r.i32()?; // Enum object ref
            PropertyType::Byte { enum_name: None }
        }
        // `FEnumProperty::Serialize` writes `Enum` **before** the underlying
        // property (`EnumProperty.cpp`). Reading them the other way round makes
        // the nested field's type-name FName land on the enum reference — a
        // negative index — which fails the whole chain.
        "EnumProperty" => {
            r.i32()?; // Enum object ref
            let inner = read_single_field(r)?
                .map(|(_, t, _)| t)
                .unwrap_or(PropertyType::Byte { enum_name: None });
            PropertyType::Enum { inner: Box::new(inner), enum_name: String::new() }
        }
        "ArrayProperty" => {
            let inner = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("ArrayProperty inner missing")?;
            PropertyType::Array(Box::new(inner))
        }
        "SetProperty" => {
            let elem = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("SetProperty element missing")?;
            PropertyType::Set(Box::new(elem))
        }
        "MapProperty" => {
            let key = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("MapProperty key missing")?;
            let val = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("MapProperty value missing")?;
            PropertyType::Map(Box::new(key), Box::new(val))
        }
        "StrProperty" => PropertyType::Str,
        "NameProperty" => PropertyType::Name,
        "TextProperty" => PropertyType::Text,
        "IntProperty" => PropertyType::Int,
        "Int8Property" => PropertyType::Int8,
        "Int16Property" => PropertyType::Int16,
        "Int64Property" => PropertyType::Int64,
        "UInt16Property" => PropertyType::UInt16,
        "UInt32Property" => PropertyType::UInt32,
        "UInt64Property" => PropertyType::UInt64,
        "FloatProperty" => PropertyType::Float,
        "DoubleProperty" => PropertyType::Double,
        "FieldPathProperty" => {
            r.name()?; // PropertyClass FName
            PropertyType::FieldPath
        }
        "DelegateProperty"
        | "MulticastInlineDelegateProperty"
        | "MulticastSparseDelegateProperty" => {
            r.i32()?; // SignatureFunction
            PropertyType::Delegate
        }
        other => bail!("unhandled FProperty class '{other}' in UserDefinedStruct layout"),
    })
}

/// Extract the serialized Kismet **bytecode blob** of a `UFunction` export. A
/// `UFunction` serializes as a `UStruct` (UObject unversioned header, then native
/// `SuperStruct` i32, two empty arrays, `numFields` + the `FField` param/local
/// chain) followed by the script: `ScriptBytecodeSize`(i32),
/// `ScriptStorageSize`(i32), then that many bytes of `SerializeExpr`-encoded
/// bytecode (object/name refs inline). Returns the raw storage bytes for a
/// disassembler to walk. `names` is the package name map.
pub fn read_ufunction_script(export: &[u8], names: &[String]) -> Result<Vec<u8>> {
    let mut r = Reader::new(export, names);
    let present = read_header(&mut r)?; // UObject block (usually empty for a UFunction)
    for (_, non_zero) in &present {
        if *non_zero {
            r.take(16)?;
        }
    }
    // Native UStruct prefix, then the script. The two i32s after SuperStruct are
    // empty arrays (Children + script/property-object refs); probe a couple of
    // offsets for the `numFields` that yields a fully-parsing FField chain.
    let base = r.o;
    let mut last_err: Option<anyhow::Error> = None;
    for pad in [3usize, 2, 4, 1, 0] {
        r.o = base + pad * 4;
        match try_read_script(&mut r) {
            Ok(blob) if !blob.is_empty() => return Ok(blob),
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no UFunction script found")))
}

fn try_read_script(r: &mut Reader) -> Result<Vec<u8>> {
    let num = r.i32()?;
    if !(0..=4096).contains(&num) {
        bail!("implausible numFields {num}");
    }
    for _ in 0..num {
        // A wrong offset yields an unknown FProperty class → error, rejecting it.
        read_single_field(r)?;
    }
    let _bytecode_size = r.i32()?;
    let storage = r.i32()?;
    if !(1..=16_000_000).contains(&storage) {
        bail!("implausible ScriptStorageSize {storage}");
    }
    Ok(r.take(storage as usize)?.to_vec())
}

/// Decode a cooked object export's unversioned property block for a known
/// native `class` (present in the `.usmap`), returning present property
/// name→value. General entry point for simple UObject exports (e.g.
/// `SkeletalMeshSocket`) whose serial data is just their reflected properties.
pub fn read_export_struct(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<BTreeMap<String, PropValue>> {
    read_export_struct_len(export, names, usmap, class).map(|(props, _)| props)
}

/// As [`read_export_struct`], but also returning how many bytes the property
/// block consumed.
///
/// This is the corpus gate for this reader. "Decoded without error" is a weak
/// claim — a desynced walk keeps reading plausible values and only trips much
/// later, or not at all. For a class whose export is *nothing but* its property
/// block, `consumed == export.len()` (modulo zero padding) is a real check that
/// every byte was accounted for; for classes with a native tail (a mesh's render
/// data, a texture's platform data) it says where that tail begins.
pub fn read_export_struct_len(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<(BTreeMap<String, PropValue>, usize)> {
    let mut r = Reader::new(export, names);
    let props = read_struct(&mut r, class, usmap, 0)?;
    Ok((props, r.o))
}

/// Resolves the struct references a cooked export makes to things outside
/// itself.
///
/// Two kinds of reference need this. `UDataTable::RowStruct` is an
/// `FPackageIndex` naming the struct its rows are laid out by, and every
/// `FStructProperty` in a recovered `FField` chain stores only an
/// `FPackageIndex` for its type. Either may point at a native struct (a script
/// import, so the `.usmap` has it) or at a `UUserDefinedStruct` cooked into
/// another package entirely — and only the caller holds the import map and the
/// other containers.
///
/// Splitting it in two is what lets the two halves compose: [`struct_name`]
/// depends on the package currently being read, [`struct_layout`] does not, so
/// a struct that nests another user-defined struct resolves by recursion
/// instead of needing a schema table threaded through the reader.
///
/// [`struct_name`]: PackageResolver::struct_name
/// [`struct_layout`]: PackageResolver::struct_layout
pub trait PackageResolver {
    /// The name of the struct an `FPackageIndex` from *this* package points
    /// at. Native structs should come back under their `.usmap` name; anything
    /// else under a name `struct_layout` can find it by.
    fn struct_name(&self, package_index: i32) -> Option<String>;

    /// The property layout of a struct that is not in the `.usmap`, by the
    /// name [`struct_name`](PackageResolver::struct_name) returned. Called only
    /// after a `.usmap` lookup misses.
    fn struct_layout(&self, _name: &str) -> Option<Vec<UsmapProperty>> {
        None
    }
}

/// Package-level context an export's native tail needs but its own bytes
/// cannot supply.
#[derive(Default, Clone, Copy)]
pub struct ExportContext<'a> {
    /// The package's bulk-data map as `(serial_offset, serial_size)`, so tails
    /// with an inline bulk payload can find it.
    pub bulk_data: &'a [(i64, i64)],
    /// Resolves references out of this package. Without one, `UDataTable` rows
    /// and struct-typed user-defined fields are reported as unmodeled tails
    /// rather than guessed at.
    pub resolver: Option<&'a dyn PackageResolver>,
}

impl<'a> ExportContext<'a> {
    /// Context carrying only a bulk-data map — enough for every class whose
    /// tail is self-describing.
    pub fn new(bulk_data: &'a [(i64, i64)]) -> Self {
        ExportContext { bulk_data, resolver: None }
    }
}

/// Decode an export *completely*: its unversioned property block, `UObject`'s
/// trailer, and then each class in its inheritance chain's natively-serialized
/// tail. Returns the properties and the total bytes consumed.
///
/// A cooked export is not just a property block. `UObject::Serialize` appends a
/// four-byte `hasGuid` (plus a 16-byte GUID when set), and every class whose
/// `Serialize` writes raw data appends more after that — base class first, since
/// each override calls `Super::Serialize` before writing its own. Reading only
/// the property block leaves that tail unaccounted for, which is why most
/// exports looked "decoded but incomplete".
///
/// `object_flags` comes from the export map; class-default objects skip the
/// `UObject` trailer.
/// `FRigVMPropertyPathDescription::operator<<` (RigVMPropertyPath.h:55).
fn read_rigvm_property_paths(r: &mut Reader) -> Result<()> {
    let n = native_count(r, "PropertyPathDescriptions")?;
    for _ in 0..n {
        r.i32()?; // PropertyIndex
        r.fstring()?; // HeadCPPType
        r.fstring()?; // SegmentPath
    }
    Ok(())
}

/// `FRigVMOperand::Serialize` (RigVMMemoryCommon.cpp:14): a `uint8` memory type
/// and two `uint16` indices.
const RIGVM_OPERAND: usize = 5;

/// `FRigVMByteCode::Load` (RigVMByteCode.cpp:471). Instructions are *re-encoded*
/// on load rather than copied, so the stream is a sequence of tagged ops rather
/// than a byte blob: a `uint8` opcode, then that op's struct — which opens with
/// its own copy of the opcode, because every op derives from `FRigVMBaseOp`.
fn read_rigvm_bytecode(r: &mut Reader) -> Result<()> {
    // ERigVMOpCode. Values 0..=64 are the deprecated fixed-arity `Execute`
    // forms, which `Load` folds into `Execute` before dispatching.
    const EXECUTE_64: u8 = 64;
    const ZERO: u8 = 65;
    const COPY: u8 = 68;
    const EQUALS: u8 = 71;
    const NOT_EQUALS: u8 = 72;
    const JUMP_ABSOLUTE: u8 = 73;
    const JUMP_BACKWARD: u8 = 75;
    const JUMP_ABSOLUTE_IF: u8 = 76;
    const JUMP_BACKWARD_IF: u8 = 78;
    const EXIT: u8 = 80;
    const BEGIN_BLOCK: u8 = 81;
    const END_BLOCK: u8 = 82;
    const INVOKE_ENTRY: u8 = 99;
    const JUMP_TO_BRANCH: u8 = 100;
    const EXECUTE: u8 = 101;
    const RUN_INSTRUCTIONS: u8 = 102;
    const SETUP_TRAITS: u8 = 103;

    let count = native_count(r, "RigVM instructions")?;
    for _ in 0..count {
        let op = r.u8()?;
        let op = if op <= EXECUTE_64 { EXECUTE } else { op };
        match op {
            // `FRigVMExecuteOp::Serialize`: opcode, `FunctionIndex`,
            // `ArgumentCount`, then the predicate range — all `uint16` — and
            // finally `ArgumentCount` operands.
            EXECUTE => {
                r.u8()?; // FRigVMBaseOp::OpCode
                r.u16()?; // FunctionIndex
                let args = r.u16()? as usize;
                r.take(4)?; // FirstPredicateIndex, PredicateCount
                r.take(args * RIGVM_OPERAND)?;
            }
            // `FRigVMCopyOp`: source, target, `uint16 NumBytes` and a `uint8`
            // register type.
            COPY => {
                r.u8()?;
                r.take(2 * RIGVM_OPERAND + 3)?;
            }
            // Unary ops — `FRigVMUnaryOp` is opcode plus one operand. The
            // deprecated array opcodes reuse the same shapes by arity.
            ZERO..=67 | 69 | 70 | 83 | 98 => {
                r.u8()?;
                r.take(RIGVM_OPERAND)?;
            }
            // `FRigVMComparisonOp`: A, B, Result.
            EQUALS | NOT_EQUALS => {
                r.u8()?;
                r.take(3 * RIGVM_OPERAND)?;
            }
            // `FRigVMJumpOp`: an `int32` instruction index.
            JUMP_ABSOLUTE..=JUMP_BACKWARD => {
                r.u8()?;
                r.i32()?;
            }
            // `FRigVMJumpIfOp`: the condition operand, the target index, and a
            // `bool Condition` — four bytes, being an `FArchive` bool.
            JUMP_ABSOLUTE_IF..=JUMP_BACKWARD_IF => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 8)?;
            }
            // `Exit` and `EndBlock` write nothing at all — `Load` calls
            // `AddExitOp`/`AddEndBlockOp` without touching the archive.
            EXIT | END_BLOCK => {}
            // Binary ops: `BeginBlock` plus the deprecated two-operand array ops.
            BEGIN_BLOCK | 84 | 85 | 90 | 92 | 93 | 95 => {
                r.u8()?;
                r.take(2 * RIGVM_OPERAND)?;
            }
            // Ternary array ops.
            86 | 87 | 88 | 89 | 96 | 97 => {
                r.u8()?;
                r.take(3 * RIGVM_OPERAND)?;
            }
            91 => {
                // ArrayFind, a quaternary op.
                r.u8()?;
                r.take(4 * RIGVM_OPERAND)?;
            }
            94 => {
                // ArrayIterator, a senary op.
                r.u8()?;
                r.take(6 * RIGVM_OPERAND)?;
            }
            // `FRigVMInvokeEntryOp::Serialize` is the one op that does **not**
            // write its opcode: it writes only the entry name, as an `FString`.
            INVOKE_ENTRY => {
                r.fstring()?;
            }
            // `FRigVMJumpToBranchOp`: operand + `int32 FirstBranchInfoIndex`.
            JUMP_TO_BRANCH => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 4)?;
            }
            // `FRigVMRunInstructionsOp`: operand + start/end `int32`s.
            RUN_INSTRUCTIONS => {
                r.u8()?;
                r.take(RIGVM_OPERAND + 8)?;
            }
            // `FRigVMSetupTraitsOp` inherits `FRigVMUnaryOp::Serialize`.
            SETUP_TRAITS => {
                r.u8()?;
                r.take(RIGVM_OPERAND)?;
            }
            _ => bail!("unknown ERigVMOpCode {op} @ {}", r.o - 1),
        }
    }
    // `Entries` round-trip through `ImportText`, so they are stored as strings.
    let entries = native_count(r, "RigVM entries")?;
    for _ in 0..entries {
        r.fstring()?;
    }
    // `FRigVMBranchInfo::Serialize` (RigVMMemoryStorage.cpp:54): `int32 Index`,
    // the label as an `FString`, two `int32` indices and two `uint16`s.
    let branches = native_count(r, "RigVM branch infos")?;
    for _ in 0..branches {
        r.i32()?; // Index
        r.fstring()?; // Label
        r.take(12)?; // InstructionIndex, ArgumentIndex, FirstInstruction, LastInstruction
    }
    r.fstring()?; // PublicContextPathName — an FString, not an FName
    Ok(())
}

/// `URigVM::Load` (RigVM.cpp:157), for a package new enough that every version
/// gate is satisfied — which is every cooked CE asset.
fn read_rigvm(r: &mut Reader, usmap: &Usmap) -> Result<()> {
    let why = std::env::var("BLAM_TAIL_WHY").is_ok();
    macro_rules! stage {
        ($s:expr) => {
            if why {
                eprintln!("  rigvm: {} @ {}", $s, r.o);
            }
        };
    }
    r.u32()?; // CachedVMHash
    read_rigvm_property_paths(r)?; // ExternalPropertyPathDescriptions
    let fns = native_count(r, "RigVM function names")?;
    r.take(fns * 8)?; // FunctionNamesStorage: TArray<FName>
    stage!("bytecode start");
    read_rigvm_bytecode(r)?;
    stage!("parameters");
    // `FRigVMParameter::Load` (RigVM.cpp:65).
    let params = native_count(r, "RigVM parameters")?;
    for _ in 0..params {
        r.u8()?; // Type
        r.name()?; // Name
        r.i32()?; // RegisterIndex
        r.fstring()?; // CPPType
        r.name()?; // ScriptStructPath
    }
    // `OperandToDebugRegisters` is reached by a bare `Ar <<` on the `TMap`, so
    // it uses `TMap`'s own operator — a plain count and that many pairs — not
    // `FMapProperty`'s delta-serialized form.
    stage!("debug register map");
    let debug = native_count(r, "RigVM debug register map")?;
    for _ in 0..debug {
        r.take(RIGVM_OPERAND)?; // key
        let n = native_count(r, "RigVM debug registers")?;
        r.take(n * RIGVM_OPERAND)?;
    }
    stage!("memory storages");
    // `FRigVMMemoryStorageStruct::Serialize` (RigVMMemoryStorageStruct.cpp:39)
    // is `FInstancedPropertyBag`'s, then the memory type and property paths.
    for _ in 0..3 {
        read_native_variable_struct(r, "InstancedPropertyBag", usmap, 0)?;
        r.u8()?; // MemoryType
        read_rigvm_property_paths(r)?;
    }
    Ok(())
}


/// `EManagedArrayType`, in the order `ManagedArrayTypeValues.inl` declares them
/// (`FNoneType` is 0, so index 1 is the first real entry).
const MANAGED_ARRAY_TYPES: &[&str] = &[
    "None", "Vector", "IntVector", "Vector2D", "LinearColor", "Int32", "Bool", "Transform",
    "String", "Float", "Quat", "BoneNode", "MeshSection", "Box", "IntArray", "Guid", "UInt8",
    "VectorArrayPointer", "VectorArrayUniquePointer", "ImplicitObject3Pointer",
    "ImplicitObject3UniquePointer", "ImplicitObject3SerializablePtr", "BVHParticlesFloat3Pointer",
    "BVHParticlesFloat3UniquePointer", "PBDRigidParticleHandle3fPtr",
    "PBDGeometryCollectionParticleHandle3fPtr", "GeometryParticle3fUniquePtr",
    "ImplicitObject3ThreadSafeSharedPointer", "ImplicitObject3SharedPointer",
    "PBDRigidClusteredParticleHandle3fPtr", "ConvexUniquePtr", "Vector2DArray", "Double",
    "IntVector4", "Vector3d", "IntVector2", "IntVector2Array", "Int32Array", "FloatArray",
    "Vector4f", "VectorArray", "PBDRigidParticle3fUniquePtr", "ImplicitObjectRefCountedPtr",
    "ConvexRefCountedPtr", "Transform3f", "IntVector3Array", "Vector4fArray", "PMatrix33d",
    "PMatrix33dArray", "Vector3fNestedArray",
];

/// The types with a `TryBulkSerializeManagedArray` overload (ManagedArray.h:21).
/// Their payload writes an element size *and* a count, so it is self-describing
/// and can be skipped without knowing the type at all.
fn managed_array_is_bulk(t: &str) -> bool {
    matches!(
        t,
        "Vector" | "IntVector" | "Vector2D" | "Int32" | "Bool" | "Float" | "Quat" | "Guid"
            | "UInt8" | "IntVector2"
    )
}

/// Non-bulk types whose element is a fixed size on disk, so `Ar << TArray<T>`
/// is a bare count followed by `count * size` bytes.
fn managed_array_elem(t: &str) -> Option<usize> {
    Some(match t {
        "Transform3f" => 40, // FQuat4f + 2x FVector3f
        "Transform" => 80,   // LWC doubles
        "LinearColor" | "Vector4f" => 16,
        "Vector3d" => 24,
        "Double" => 8,
        "Box" => 49, // two FVector3d and the IsValid byte
        "MeshSection" => 20,
        "PMatrix33d" => 72,
        _ => return None,
    })
}

/// Types whose element is itself an array of fixed-size items.
fn managed_array_nested_elem(t: &str) -> Option<usize> {
    Some(match t {
        "Int32Array" | "FloatArray" | "IntArray" => 4,
        "Vector2DArray" | "IntVector2Array" => 8,
        "IntVector3Array" | "VectorArray" => 12,
        "Vector4fArray" => 16,
        "PMatrix33dArray" => 72,
        _ => return None,
    })
}

/// `FChaosArchive::SerializePtr` (ChaosArchive.h:176) — the object-graph form
/// every Chaos smart pointer goes through: a four-byte `bExists`, and when set
/// an `int32 Tag`. A tag already seen in this archive is a back-reference and
/// carries **no payload**; only its first sighting is followed by the object.
/// Returns true when the caller must now read the object itself.
fn read_chaos_ptr(r: &mut Reader, seen: &mut std::collections::HashSet<i32>) -> Result<bool> {
    if r.u32()? == 0 {
        return Ok(false);
    }
    let tag = r.i32()?;
    Ok(seen.insert(tag))
}

/// `FImplicitObject::SerializationFactory` (ImplicitObject.cpp:406) dispatches on
/// an `int8` type byte and the object then serializes itself.
/// `FImplicitObject::SerializeImp` is the shared prefix: `bIsConvex` and
/// `bDoCollide` (four bytes each, being `FArchive` bools) then a one-byte
/// `CollisionType`.
///
/// CE ships exactly two shapes across all 14 collections — 1440 spheres and
/// 1434 convex hulls — so the rest of the factory's hierarchy (level sets,
/// triangle meshes, unions, height fields and the scaled/instanced wrappers)
/// is deliberately not modelled; hitting one reports an unmodeled tail.
fn read_chaos_implicit_object(r: &mut Reader) -> Result<()> {
    const SPHERE: i8 = 0;
    const CONVEX: i8 = 8;
    let ty = r.u8()? as i8;
    r.take(9)?; // bIsConvex, bDoCollide, CollisionType
    match ty {
        // TSphere: Center then a radius written as `FRealSingle` (it lives in
        // the base class's Margin).
        SPHERE => {
            r.take(16)?;
        }
        // FConvex::SerializeImp (Convex.h:890).
        CONVEX => {
            let planes = native_count(r, "convex planes")?;
            r.take(planes * 24)?; // TPlaneConcrete: MX + MNormal, float3 each
            let verts = native_count(r, "convex vertices")?;
            r.take(verts * 12)?;
            r.take(24)?; // LocalBoundingBox: TAABB (MMin, MMax)
            r.take(4)?; // Volume, as FRealSingle
            r.take(12)?; // CenterOfMass
            r.take(4)?; // Margin, as FRealSingle
            read_convex_structure_data(r)?;
            // Mixed precision in one struct, both measured: the inertia is a
            // float3 while the rotation is a **double** quaternion (its four
            // doubles have norm 1.0000).
            r.take(12 + 32)?; // UnitMassInertiaTensor, RotationOfMass
        }
        _ => bail!("unmodeled Chaos implicit object type {ty} @ {}", r.o - 10),
    }
    Ok(())
}

/// `FConvexStructureData::Serialize` (ConvexStructureData.h:253): an `int8`
/// index width, then the half-edge tables at that width
/// (ConvexHalfEdgeStructureData.h:556) — planes (2 indices each), half-edges
/// (3), vertices (1) and the unique edge list (1).
fn read_convex_structure_data(r: &mut Reader) -> Result<()> {
    let w = match r.u8()? as i8 {
        0 => return Ok(()), // None: no container follows
        1 => 1,             // Small:  uint8
        2 => 2,             // Medium: int16
        3 => 4,             // Large:  int32
        n => bail!("unknown convex structure index type {n} @ {}", r.o - 1),
    };
    for per in [2usize, 3, 1, 1] {
        let n = native_count(r, "convex structure table")?;
        r.take(n * per * w)?;
    }
    Ok(())
}

/// `FBVHParticles::Serialize` (BVHParticles.cpp:62) = `FParticles::Serialize`
/// (Particles.h:122 — a four-byte `bSerialize` then the `MX` positions) followed
/// by the bounding-volume hierarchy (BoundingVolumeHierarchy.cpp:696).
///
/// Everything here is single-precision in CE's build, verified by the boxes
/// being byte-identical copies of the float3 particle positions.
fn read_bvh_particles(r: &mut Reader) -> Result<()> {
    if r.u32()? == 0 {
        return Ok(()); // bSerialize false writes nothing more
    }
    let mx = native_count(r, "BVH particles")?;
    r.take(mx * 12)?; // FVector3f positions
    let globals = native_count(r, "BVH global objects")?;
    r.take(globals * 4)?;
    // `MWorldSpaceBoxes` is a **TMap<int32, TAABB>**, not an array (the second
    // `SerializeAsAABBs` overload, Box.h:528). A bare `Ar << TMap` uses TMap's
    // own operator — a count then key/value pairs — not `FMapProperty`'s
    // delta-serialized form, so each entry carries its int32 key.
    let boxes = native_count(r, "BVH world-space boxes")?;
    r.take(boxes * (4 + 24))?;
    r.i32()?; // MMaxLevels
    let nodes = native_count(r, "BVH nodes")?;
    for _ in 0..nodes {
        // `operator<<(TBVHNode)` (BoundingVolumeHierarchy.h:53) writes
        // LeafIndex, MAxis, MChildren, MMax, MMin — that order, **not**
        // declaration order.
        r.take(8)?; // LeafIndex, MAxis
        let children = native_count(r, "BVH node children")?;
        r.take(children * 4)?;
        r.take(24)?; // MMax, MMin
    }
    let leafs = native_count(r, "BVH leaves")?;
    for _ in 0..leafs {
        let n = native_count(r, "BVH leaf")?;
        r.take(n * 4)?;
    }
    Ok(())
}

/// `FManagedArrayCollection::Serialize` — a generic container of named, typed
/// arrays. The group table and attribute table are self-describing; each
/// attribute's payload shape is set by its `EManagedArrayType`.
fn read_managed_array_collection(r: &mut Reader) -> Result<()> {
    let why = std::env::var("BLAM_TAIL_WHY").is_ok();
    r.i32()?; // Version
    let groups = native_count(r, "collection groups")?;
    r.take(groups * 16)?; // FName key + FGroupInfo{version, size}
    let attrs = native_count(r, "collection attributes")?;
    let mut chaos_tags = std::collections::HashSet::new();
    for _ in 0..attrs {
        r.take(16)?; // key: attribute FName + group FName
        r.i32()?; // FValueType::version
        let ty = r.i32()?;
        r.take(12)?; // GroupIndexDependency FName + bPersistent
        let name = MANAGED_ARRAY_TYPES.get(ty as usize).copied().unwrap_or("?");
        r.i32()?; // the array's own version
        if why {
            eprintln!("  gc attr {name} @ {}", r.o);
        }
        if managed_array_is_bulk(name) {
            let elem = r.i32()?;
            let n = r.i32()?;
            if elem < 0 || n < 0 {
                bail!("implausible bulk managed array {elem}x{n} @ {}", r.o - 8);
            }
            r.take(elem as usize * n as usize)?;
        } else if name == "String" {
            let n = native_count(r, "collection strings")?;
            for _ in 0..n {
                r.fstring()?;
            }
        } else if let Some(inner) = managed_array_nested_elem(name) {
            let n = native_count(r, "collection nested array")?;
            for _ in 0..n {
                let m = native_count(r, "collection nested element")?;
                r.take(m * inner)?;
            }
        } else if let Some(sz) = managed_array_elem(name) {
            let n = native_count(r, "collection array")?;
            r.take(n * sz)?;
        } else if name == "ImplicitObjectRefCountedPtr" || name == "ConvexRefCountedPtr" {
            let n = native_count(r, "collection implicit objects")?;
            for _ in 0..n {
                if read_chaos_ptr(r, &mut chaos_tags)? {
                    read_chaos_implicit_object(r)?;
                }
            }
        } else if name == "BVHParticlesFloat3UniquePointer" {
            let n = native_count(r, "collection BVH particles")?;
            for _ in 0..n {
                if read_chaos_ptr(r, &mut chaos_tags)? {
                    read_bvh_particles(r)?;
                }
            }
        } else {
            bail!("unmodeled managed array type {name} ({ty}) @ {}", r.o);
        }
    }
    Ok(())
}


/// `FGeometryCollectionRenderData::Serialize` (GeometryCollectionRenderData.cpp:722):
/// two cooked flags, then the mesh buffers and description, then Nanite.
fn read_geometry_collection_render_data(r: &mut Reader) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    let has_mesh = r.u32()? != 0;
    let has_nanite = r.u32()? != 0;
    if t {
        eprintln!("  gc render: mesh={has_mesh} nanite={has_nanite} @ {}", r.o);
        for (i, ch) in r.b[r.o..(r.o + 64).min(r.b.len())].chunks(16).enumerate() {
            eprint!("    {:08x}: ", r.o + i * 16);
            for x in ch { eprint!("{x:02x} "); }
            eprintln!();
        }
    }
    if has_mesh {
        // `FGeometryCollectionMeshResources::Serialize` (line 110) — note the
        // index buffer comes **first** here, unlike `FStaticMeshLODResources`,
        // and each buffer writes its own strip flags because they are
        // serialized individually rather than under one shared set.
        read_raw_static_index_buffer(r)?;
        // `FPositionVertexBuffer::Serialize` (PositionVertexBuffer.cpp:162) has
        // **no** strip flags — just `SerializeMetaData` and the vertex data.
        r.i32()?; // Stride
        r.i32()?; // NumVertices
        read_bulk_array(r, "collection positions")?;
        let vb_strip = r.u8()?;
        r.u8()?;
        r.i32()?; // NumTexCoords
        r.i32()?; // NumVertices
        r.u32()?; // bUseFullPrecisionUVs
        r.u32()?; // bUseHighPrecisionTangentBasis
        if vb_strip & 2 == 0 {
            read_bulk_array(r, "collection tangents")?;
            read_bulk_array(r, "collection UVs")?;
        }
        let cb_strip = r.u8()?;
        r.u8()?;
        r.i32()?; // Stride
        let colour_verts = r.i32()?;
        if cb_strip & 2 == 0 && colour_verts > 0 {
            read_bulk_array(r, "collection vertex colours")?;
        }
        // `FBoneMapVertexBuffer::Serialize` (line 62) has no strip flags: a
        // count and then the vertex data.
        r.i32()?; // NumVertices
        read_bulk_array(r, "collection bone map")?;
        if t {
            eprintln!("  gc render: mesh description @ {}", r.o);
        }
        // `FGeometryCollectionMeshDescription::Serialize` (line 126).
        // `FGeometryCollectionMeshElement` is 20 bytes: int16, two uint8s and
        // four uint32s. `SubSections` is written empty in a cooked build.
        r.i32()?; // NumVertices
        r.i32()?; // NumTriangles
        r.take(56)?; // PreSkinnedBounds: FBoxSphereBounds
        for what in ["Sections", "SectionsNoInternal", "SubSections"] {
            let n = native_count(r, what)?;
            r.take(n * 20)?;
        }
    }
    if has_nanite {
        if t {
            eprintln!("  gc render: nanite @ {}", r.o);
        }
        read_nanite_resources(r)?;
    }
    Ok(())
}


/// `FArchive::SerializeCompressedNew`'s load path (Archive.cpp:707).
///
/// The v2 header's tag is `PACKAGE_FILE_TAG | (0x22222222 << 32)` and is
/// followed by the compressor's index; the v1 header is a bare
/// `PACKAGE_FILE_TAG` and names no compressor. The `UncompressedSize` of that
/// first `FCompressedChunkInfo` is not a size at all — it is the chunk size the
/// data was split at, which sets how many chunk infos follow.
fn read_compressed_buffer(r: &mut Reader) -> Result<()> {
    const PACKAGE_FILE_TAG: u64 = 0x9E2A_83C1;
    const V2_HEADER_TAG: u64 = PACKAGE_FILE_TAG | (0x2222_2222u64 << 32);
    const LOADING_COMPRESSION_CHUNK_SIZE: u64 = 131072;

    let at = r.o;
    let tag = r.u64()?;
    let chunk_size_field = r.u64()?;
    match tag {
        V2_HEADER_TAG => {
            // FCompressionUtil::SerializeCompressorName: a `uint8` index, where
            // 0 means an `FString` name follows (1 None, 2 Oodle, 3 Zlib,
            // 4 Gzip, 5 LZ4).
            if r.u8()? == 0 {
                r.fstring()?;
            }
        }
        PACKAGE_FILE_TAG => {}
        _ => bail!("not a compressed-buffer header ({tag:#x}) @ {at}"),
    }
    let chunk_size = if chunk_size_field == PACKAGE_FILE_TAG {
        LOADING_COMPRESSION_CHUNK_SIZE
    } else {
        chunk_size_field
    };
    if chunk_size == 0 {
        bail!("compressed buffer declares a zero chunk size @ {at}");
    }
    r.u64()?; // Summary.CompressedSize
    let total_uncompressed = r.u64()?;
    let chunks = total_uncompressed.div_ceil(chunk_size);
    if chunks > 1_000_000 {
        bail!("implausible compressed chunk count {chunks} @ {at}");
    }
    let mut payload = 0u64;
    for _ in 0..chunks {
        payload += r.u64()?; // this chunk's compressed size
        r.u64()?; // and its uncompressed size
    }
    r.take(usize::try_from(payload).context("implausible compressed payload")?)?;
    Ok(())
}

/// `TDynamicVector<T>::Serialize` (DynamicVector.h:163) in its modern form: an
/// element count, and when non-zero a flag saying whether the blocks were
/// Oodle-compressed as one buffer.
fn read_dynamic_vector(r: &mut Reader, elem: usize) -> Result<()> {
    /// `TDynamicVector`'s default block length.
    const BLOCK_SIZE: usize = 512;
    let n = r.u32()? as usize;
    if n == 0 {
        return Ok(());
    }
    if r.u32()? != 0 {
        return read_compressed_buffer(r);
    }
    // Uncompressed, the blocks are written whole: `Load` reads
    // `min(Num, BlockSize)` elements for each of `ceil(Num / BlockSize)` blocks.
    let blocks = n.div_ceil(BLOCK_SIZE);
    r.take(blocks * n.min(BLOCK_SIZE) * elem)?;
    Ok(())
}

/// `FRefCountVector::Serialize` (RefCountVector.h:523). The free-index list is
/// only written when the data is neither compacted nor compressed — otherwise
/// it is rebuilt on load from the invalid ref-count sentinels.
fn read_ref_count_vector(r: &mut Reader) -> Result<()> {
    let compact = r.u32()? != 0;
    let compressed = r.u32()? != 0;
    let used = r.i32()?;
    read_dynamic_vector(r, 2)?; // RefCounts: TDynamicVector<unsigned short>
    let _ = used;
    if !compact && !compressed {
        read_dynamic_vector(r, 4)?; // FreeIndices
    }
    Ok(())
}

/// `TDynamicVector` behind the four-byte "is set" flag that
/// `SerializeOptionalVector` (DynamicMesh3_Serialization.cpp:70) writes.
fn read_optional_dynamic_vector(r: &mut Reader, elem: usize) -> Result<()> {
    if r.u32()? != 0 {
        read_dynamic_vector(r, elem)?;
    }
    Ok(())
}

/// `TDynamicMeshOverlay::Serialize` (DynamicMeshOverlay.cpp:1745): the element
/// ref counts, the elements themselves (`ElementSize` reals apiece), the parent
/// vertex per element, and the per-triangle element indices.
fn read_dynamic_mesh_overlay(r: &mut Reader, element_size: usize, real: usize) -> Result<()> {
    read_ref_count_vector(r)?;
    read_dynamic_vector(r, real * element_size)?; // Elements
    read_dynamic_vector(r, 4)?; // ParentVertices
    read_dynamic_vector(r, 4)?; // ElementTriangles
    Ok(())
}


/// `FDynamicMeshAttributeSet::Serialize` (DynamicMeshAttributeSet.cpp:1304).
/// Every layer list is a count followed by that many layers; each attribute
/// opens with its name, written through an `FNameAsStringProxyArchive`
/// (`TDynamicAttributeBase::Serialize`, DynamicAttribute.h:215), so it is an
/// `FString` rather than an `FName`.
fn read_dynamic_mesh_attribute_set(r: &mut Reader) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    macro_rules! stage {
        ($s:expr) => {
            if t {
                eprintln!("    attrs: {} @ {}", $s, r.o);
            }
        };
    }
    r.u32()?; // bUseCompression, re-written here
    // `FDynamicMeshUVOverlay` carries two floats per element, normals three.
    stage!("uv layers");
    // Note an overlay's `Serialize` does **not** call `Super::Serialize`, so
    // unlike the vertex/triangle attributes below it writes no name.
    for (what, size) in [("UV", 2usize), ("normal", 3)] {
        let n = native_count(r, what)?;
        for _ in 0..n {
            read_dynamic_mesh_overlay(r, size, 4)?;
        }
    }
    // Polygroups are per-triangle int32s, weights per-vertex floats; both are a
    // name and a single value array.
    stage!("polygroup/weight layers");
    // A vertex/triangle attribute writes its name (via `Super::Serialize`) and
    // then **its own** `bUseCompression` flag before the value array — the
    // overlays above take that flag as a parameter and write nothing.
    for what in ["polygroup", "weight"] {
        let n = native_count(r, what)?;
        for _ in 0..n {
            r.fstring()?;
            r.u32()?; // bUseCompression
            read_dynamic_vector(r, 4)?;
        }
    }
    stage!("colour layer");
    let colours = r.i32()?;
    if colours > 0 {
        read_dynamic_mesh_overlay(r, 4, 4)?;
    }
    stage!("material id");
    if r.u32()? != 0 {
        r.fstring()?;
        r.u32()?; // bUseCompression
        read_dynamic_vector(r, 4)?;
    }
    stage!("skin weights");
    let skins = native_count(r, "skin weight attributes")?;
    for _ in 0..skins {
        r.fstring()?; // key, as a name-through-string
        if r.u32()? != 0 {
            bail!("unmodeled dynamic-mesh skin weight attribute @ {}", r.o);
        }
    }
    stage!("bones");
    if r.u32()? != 0 {
        bail!("unmodeled dynamic-mesh bone attributes @ {}", r.o);
    }
    Ok(())
}

/// `FDynamicMesh3::Serialize` (DynamicMesh3_Serialization.cpp:237). CE cooks
/// every dynamic mesh with `bCompactData` and `bUseCompression` set, which is
/// the `CompactData` variant: unique vertex data, unique triangle data, then
/// the attribute set. The compacted variants write no ref counts or edge data
/// at all — those are rebuilt on load.
fn read_dynamic_mesh(r: &mut Reader) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    r.u32()?; // bPreserveDataLayout
    let compact = r.u32()? != 0;
    r.u32()?; // bUseCompression
    if !compact {
        bail!("unmodeled non-compacted FDynamicMesh3 variant @ {}", r.o - 12);
    }
    if t {
        eprintln!("    dynamic mesh: vertices @ {}", r.o);
    }
    read_dynamic_vector(r, 24)?; // Vertices: FVector3d
    read_optional_dynamic_vector(r, 12)?; // VertexNormals: FVector3f
    read_optional_dynamic_vector(r, 12)?; // VertexColors: FVector3f
    read_optional_dynamic_vector(r, 8)?; // VertexUVs: FVector2f
    if t {
        eprintln!("    dynamic mesh: triangles @ {}", r.o);
    }
    read_dynamic_vector(r, 12)?; // Triangles: FIndex3i
    read_optional_dynamic_vector(r, 4)?; // TriangleGroups
    r.i32()?; // GroupIDCounter
    if t {
        eprintln!("    dynamic mesh: attributes @ {}", r.o);
    }
    if r.u32()? != 0 {
        read_dynamic_mesh_attribute_set(r)?;
    }
    Ok(())
}


/// `URigHierarchy::Load` (RigHierarchy.cpp:251). Like `URigVM` this class never
/// calls `Super::Serialize` on the load path, so the export has no property
/// block: it opens straight at the element count.
///
/// `FRigElementKey::Load` (RigHierarchyDefines.cpp:73) writes the element
/// **type as an `FName`** and then the element's own name — two `FName`s, 16
/// bytes — so the per-element reader dispatches on a resolved string.
fn read_rig_hierarchy(r: &mut Reader) -> Result<()> {
    let t = std::env::var("BLAM_TAIL_WHY").is_ok();
    let count = native_count(r, "rig elements")?;
    let mut types = Vec::with_capacity(count);
    for _ in 0..count {
        let ty = r.name()?;
        r.name()?; // the element's own name
        types.push(ty);
    }
    if t {
        let mut tally: BTreeMap<&str, usize> = BTreeMap::new();
        for ty in &types {
            *tally.entry(ty.as_str()).or_default() += 1;
        }
        eprintln!("  rig hierarchy: {count} elements, keys end @ {}", r.o);
        for (k, v) in tally {
            eprintln!("    {v:5}  {k}");
        }
    }
    // Every element is then loaded twice: once for its own data, once for the
    // links between elements. CE's one hierarchy holds only bones and curves,
    // so the control/null/physics/reference/connector/socket arms are left
    // unmodeled rather than written blind.
    const BONE: &str = "ERigElementType::Bone";
    const CURVE: &str = "ERigElementType::Curve";
    for ty in &types {
        // `FRigBaseElement::Load(StaticData)` re-reads the key.
        r.take(16)?;
        match ty.as_str() {
            // `FRigTransformElement::Load` writes
            // `FRigCurrentAndInitialTransform` — four `FRigComputedTransform`s
            // (current/initial x local/global), each an 80-byte LWC `FTransform`
            // plus a four-byte dirty flag — and `FRigBoneElement::Load` adds the
            // bone type, again **as an `FName`**.
            BONE => {
                r.take(4 * (80 + 4))?;
                r.name()?; // ERigBoneType, by name
            }
            // `FRigCurveElement::Load`: a four-byte `bIsValueSet` and the value.
            CURVE => {
                r.take(8)?;
            }
            _ => bail!("unmodeled rig element type {ty} @ {}", r.o),
        }
    }
    if t {
        eprintln!("  rig hierarchy: inter-element data @ {}", r.o);
    }
    for ty in &types {
        // Only `FRigSingleParentElement::Load` writes anything in this phase:
        // its parent's key. Curves have no parent element at all.
        if ty == BONE {
            r.take(16)?;
        }
    }
    // `PreviousNameMap`, `PreviousParentMap` and the element metadata map are
    // reached by a bare `Ar <<`, so each is `TMap`'s own operator — a plain
    // count and that many pairs — not `FMapProperty`'s delta form. All three
    // are empty in CE.
    for what in ["PreviousNameMap", "PreviousParentMap", "ElementMetadata"] {
        let n = native_count(r, what)?;
        if n != 0 {
            bail!("unmodeled non-empty rig hierarchy {what} ({n}) @ {}", r.o);
        }
    }
    Ok(())
}

pub fn read_export_with_trailer(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<(BTreeMap<String, PropValue>, usize)> {
    /// `RF_ClassDefaultObject`.
    const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

    let mut r = Reader::with_ctx(export, names, ctx);
    // A handful of classes override `Serialize` and deliberately do **not**
    // call `Super::Serialize` on the load path, so the export carries no
    // property block, no `UObject` GUID trailer and no inherited tails — it
    // begins immediately with the class's own data. `URigVM::Serialize`
    // (RigVM.cpp:109) only calls up for reference collection and memory
    // counting; loading goes straight to `Load`.
    if class == "RigVM" || class == "RigHierarchy" {
        if class == "RigHierarchy" {
            let props = BTreeMap::new();
            let at = r.o;
            if read_rig_hierarchy(&mut r).is_err() {
                r.o = at;
            }
            return Ok((props, r.o));
        }
        let props = BTreeMap::new();
        read_rigvm(&mut r, usmap)?;
        return Ok((props, r.o));
    }
    let props = read_struct(&mut r, class, usmap, 0)?;
    if object_flags & RF_CLASS_DEFAULT_OBJECT == 0 && export.len() >= r.o + 4 {
        let at = r.o;
        match r.u32()? {
            0 => {}
            1 => {
                r.take(16)?;
            }
            // Not a boolean, so this export does not follow the trailer model
            // (its property walk stopped early, or the class serializes
            // something else here). Rewind and leave the rest as an unmodeled
            // tail rather than failing an otherwise good property decode.
            _ => {
                r.o = at;
                return Ok((props, r.o));
            }
        }
    }
    // Walk to the root of the chain, then replay it base → derived.
    let mut chain = Vec::new();
    let mut cur = Some(class.to_string());
    while let Some(c) = cur {
        if chain.len() > 64 {
            break;
        }
        cur = usmap.get(&c).and_then(|s| s.super_name.clone());
        chain.push(c);
    }
    chain.reverse();
    let why = std::env::var("BLAM_TAIL_WHY").is_ok();
    for c in &chain {
        let at = r.o;
        let keep_going = read_class_native_tail(&mut r, c, &props, usmap, ctx, object_flags)
            .with_context(|| format!("native tail of {c} (in {class})"))?;
        if why {
            eprintln!(
                "  chain: {c} {at}..{}{}",
                r.o,
                if keep_going { "" } else { "  <- STOPPED" }
            );
        }
        if !keep_going {
            break;
        }
    }
    Ok((props, r.o))
}

/// One class's own natively-serialized tail, or nothing when it writes none.
///
/// Returns `false` when the rest of this export is not modeled, so the caller
/// stops and reports the remainder as an unmodeled tail instead of guessing.
fn read_class_native_tail(
    r: &mut Reader,
    class: &str,
    props: &BTreeMap<String, PropValue>,
    usmap: &Usmap,
    ctx: &ExportContext<'_>,
    object_flags: u32,
) -> Result<bool> {
    let bulk_data = ctx.bulk_data;
    let flag = |name: &str| matches!(props.get(name), Some(PropValue::Bool(true)));
    match class {
        // `UActorComponent`: the sparse UCS-modified-property list. Each
        // `FSimpleMemberReference` is an `FPackageIndex`, an `FName` and an
        // `FGuid` — 28 bytes.
        "ActorComponent" => {
            let n = native_count(r, "UCSModifiedProperties")?;
            r.take(n * 28)?;
        }
        // `USceneComponent` writes its baked bounds only when the component
        // asked for them to be computed once for game.
        "SceneComponent" => {
            if flag("bComputeBoundsOnceForGame") || flag("bComputedBoundsOnceForGame") {
                if r.u32()? != 0 {
                    // `FBoxSphereBounds`: origin, extent, radius.
                    r.take(56)?;
                }
            }
        }
        // `UStaticMeshComponent`: per-LOD info, then the cooked mesh-paint
        // texture reference.
        "StaticMeshComponent" => {
            let n = native_count(r, "LODData")?;
            for _ in 0..n {
                read_static_mesh_component_lod_info(r)?;
            }
            if r.u32()? != 0 {
                r.i32()?; // MeshPaintTextureCooked
            }
        }
        // `UInstancedStaticMeshComponent`: a cooked flag, a
        // "skip-serialization properties" flag and — when set — the per-instance
        // transform and custom-data bulk arrays, then the cooked render data as
        // two more bulk arrays. Each bulk array carries its own element size, so
        // `FInstancedStaticMeshInstanceData` need not be modeled.
        "InstancedStaticMeshComponent" => {
            let cooked = r.u32()? != 0;
            let has_skip_serialization_data = r.u32()? != 0;
            if has_skip_serialization_data {
                read_bulk_array(r, "PerInstanceSMData")?;
                read_bulk_array(r, "PerInstanceSMCustomData")?;
            }
            if cooked && r.u32()? != 0 {
                read_bulk_array(r, "instance render data")?;
                read_bulk_array(r, "instance render data")?;
            }
        }
        // `UMaterialInterface` writes `bSavedCachedExpressionData` and, when
        // set, an `FMaterialCachedExpressionData` block. Missing this flag is
        // what made an earlier `MaterialInstance`-only attempt desync — the
        // interface's flag was being read as the instance's.
        "MaterialInterface" => {
            let at = r.o;
            if r.u32()? != 0 && read_struct(r, "MaterialCachedExpressionData", usmap, 0).is_err() {
                // The cached-expression block is only partly modeled. Rewind and
                // report an unmodeled tail rather than failing the export.
                r.o = at;
                return Ok(false);
            }
        }
        // `UMaterialInstance` then writes its own `bSavedCachedData` and an
        // `FMaterialInstanceCachedData` block. Measured on a
        // `MaterialInstanceDynamic` whose 18-byte tail resolves exactly:
        // `hasGuid` 0, interface flag 0, instance flag 1, then a 2-byte property
        // header and an empty `ParentLayerIndexRemap`.
        //
        // Inline shader maps follow only when the instance has a static
        // permutation resource; those are not modeled, so stop there.
        "MaterialInstance" => {
            let at = r.o;
            if r.u32()? != 0 && read_struct(r, "MaterialInstanceCachedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
            // Inline shader maps follow only for an instance with a static
            // permutation resource.
            if flag("bHasStaticPermutationResource") {
                let at = r.o;
                if skip_inline_shader_maps(r).is_err() {
                    r.o = at;
                    return Ok(false);
                }
            }
        }
        // `UMaterial` always writes its inline shader maps.
        "Material" => {
            let at = r.o;
            if skip_inline_shader_maps(r).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UWorld`: the persistent level, then the extra-referenced-object and
        // streaming-level arrays. Measured on `LI_Mangrove_A`, whose whole
        // 22-byte export resolves as a 2-byte header, one object property,
        // `hasGuid` 0, then `PersistentLevel` = export 3 and two empty arrays.
        "World" => {
            r.i32()?; // PersistentLevel
            for what in ["ExtraReferencedObjects", "StreamingLevels"] {
                let n = native_count(r, what)?;
                r.take(n * 4)?;
            }
        }
        // `UWorldPartitionRuntimeCellData` writes its debug name as an
        // `FString`. Measured on `LI_Mangrove_A`: the export ends exactly after
        // the 37-byte `LI_Mangrove_A_MainPartition_L0_X0_Y0`.
        "WorldPartitionRuntimeCellData" => {
            r.fstring()?;
        }
        // `UWorldPartition::Serialize` (WorldPartition.cpp): a cooked flag and,
        // when it is set, the streaming-policy object reference. UE writes a
        // `bool` through `FArchive` as a 32-bit int, not a byte.
        "WorldPartition" => {
            if r.u32()? != 0 {
                r.i32()?; // StreamingPolicy
            }
        }
        // `USkeletalMesh::Serialize` (SkeletalMesh.cpp): strip flags, the
        // imported bounds, the material list, the reference skeleton, and then —
        // for a cooked package — the whole `FSkeletalMeshRenderData`.
        //
        // `bHasVertexColors` and `bEnablePerPolyCollision` are reflected
        // properties, and both gate part of the native layout, so this reads
        // them out of the already-decoded property block rather than probing.
        "SkeletalMesh" => {
            let at = r.o;
            let t = std::env::var("BLAM_TAIL_WHY").is_ok();
            let ok = (|| -> Result<()> {
                if t { eprintln!("  skel mesh @ {}", r.o); }
                r.take(2)?; // FStripDataFlags
                r.take(56)?; // FBoxSphereBounds ImportedBounds: LWC doubles
                let nmat = native_count(r, "Materials")?;
                if t { eprintln!("  materials {nmat} @ {}", r.o); }
                for _ in 0..nmat {
                    r.i32()?; // MaterialInterface
                    r.take(8)?; // MaterialSlotName
                    // The imported slot name only survives a cook that keeps
                    // editor data, and the flag saying so is itself serialized.
                    if r.u32()? != 0 {
                        r.take(8)?; // ImportedMaterialSlotName
                    }
                    // FMeshUVChannelInfo: two 32-bit bools + four floats.
                    r.take(24)?;
                }
                read_reference_skeleton(r)?;
                if t { eprintln!("  after ref skeleton @ {}", r.o); }
                let cooked = r.u32()?;
                if t { eprintln!("  bCooked {cooked} @ {}", r.o); }
                if cooked != 0 {
                    // FSkeletalMeshRenderData::Serialize. The mobile min-LOD
                    // index ahead of the LODs is written only when
                    // `r.SkeletalMesh.KeepMobileMinLODSettingOnDesktop` is set,
                    // which is off by default and off in this cook.
                    let lods = native_count(r, "LODRenderData")?;
                    if t { eprintln!("  LODs {lods} @ {}", r.o); }
                    for i in 0..lods {
                        read_skel_lod(r, flag("bHasVertexColors"), bulk_data)?;
                        if t { eprintln!("  after LOD {i} @ {}", r.o); }
                    }
                    read_nanite_resources(r)?;
                    r.u8()?; // NumInlinedLODs — a uint8, not an int32
                    r.u8()?; // NumNonOptionalLODs
                }
                let dummies = native_count(r, "legacy DummyObjs")?;
                r.take(dummies * 4)?;
                if flag("bEnablePerPolyCollision") {
                    r.i32()?; // BodySetup
                }
                Ok(())
            })();
            if let Err(e) = ok {
                // This walk is deep enough that a silent rewind hides which
                // buffer went wrong; `BLAM_TAIL_WHY=1` names it.
                if t {
                    eprintln!("  SkeletalMesh tail bailed @ {}: {e:#}", r.o);
                    let lo = r.o.saturating_sub(96);
                    let hi = (r.o + 96).min(r.b.len());
                    for off in (lo..hi).step_by(16) {
                        let end = (off + 16).min(hi);
                        let hex: Vec<String> =
                            r.b[off..end].iter().map(|x| format!("{x:02x}")).collect();
                        eprintln!("    {off:5}: {}", hex.join(" "));
                    }
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `AInstancedFoliageActor::Serialize` (InstancedFoliage.cpp) writes its
        // `FoliageInfos` map: a count, then per entry the `UFoliageType` key, a
        // `uint8 EFoliageImplType`, and that implementation's own payload. The
        // instance arrays and update GUID beside it are editor-only, so a cooked
        // entry is just the key, the type byte, and — for the only type CE ships,
        // `StaticMesh` — the one component reference `FFoliageStaticMesh` writes.
        "InstancedFoliageActor" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let n = native_count(r, "FoliageInfos")?;
                for _ in 0..n {
                    r.i32()?; // UFoliageType* key
                    match r.u8()? {
                        // Unknown: no implementation is constructed, so nothing
                        // follows.
                        0 => {}
                        1 => {
                            r.i32()?; // FFoliageStaticMesh::Component
                        }
                        other => bail!("unmodeled EFoliageImplType {other}"),
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USkeleton::Serialize` (Skeleton.cpp): the `FReferenceSkeleton`, the
        // animation retarget sources, the skeleton `Guid`, the deprecated
        // smart-name container, and a `FStripDataFlags`. The marker names that
        // flag guards are editor-only, so a cooked package ends right after it.
        "Skeleton" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let tsize = read_reference_skeleton(r)?;
                let nret = native_count(r, "AnimRetargetSources")?;
                for _ in 0..nret {
                    r.take(8)?; // map key FName
                    r.take(8)?; // FReferencePose::PoseName
                    let n = native_count(r, "FReferencePose::ReferencePose")?;
                    r.take(n * tsize)?;
                }
                r.take(16)?; // Guid
                // The deprecated smart-name container is a `TMap<FName,
                // FSmartNameMapping>`. Every cooked CE skeleton writes it empty;
                // a non-empty one is reported as an unmodeled tail rather than
                // decoded from a layout no sample here exercises.
                if native_count(r, "SmartNames")? != 0 {
                    bail!("non-empty deprecated SmartNames container");
                }
                r.take(2)?; // FStripDataFlags: global + class
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UNiagaraSpriteRendererProperties::Serialize` ends a cooked export with
        // `FSubUVDerivedData`, which is a single `TArray<FVector2f>` of cutout
        // bounding geometry (`SubUVAnimation.cpp`). Most sprite renderers have no
        // cutout, so the array is usually empty and the tail is just its count.
        "NiagaraSpriteRendererProperties" => {
            let n = native_count(r, "BoundingGeometry")?;
            r.take(n * 8)?; // FVector2f
        }
        // `URigVMMemoryStorageGeneratorClass::Serialize` (RigVMMemoryStorage.cpp)
        // appends two members after `UClass`: the property-path descriptions and
        // the memory type. `FRigVMPropertyPathDescription::operator<<`
        // (RigVMPropertyPath.h) writes `PropertyIndex`, `HeadCPPType` and
        // `SegmentPath` in that order; `ERigVMMemoryType` is a `uint8` enum
        // (Work = 0, Literal = 1).
        "RigVMMemoryStorageGeneratorClass" => {
            let n = native_count(r, "PropertyPathDescriptions")?;
            for _ in 0..n {
                r.i32()?; // PropertyIndex
                r.fstring()?; // HeadCPPType
                r.fstring()?; // SegmentPath
            }
            r.u8()?; // MemoryType
        }
        // `UEnum::Serialize` (Enum.cpp): the `Names` array — an `FName` and an
        // `int64` value per entry — then a `uint8` `CppForm`. Nothing earlier in
        // the chain writes anything, since `UField::Serialize` only emits `Next`
        // on packages older than `RemoveUField_Next`.
        "Enum" => {
            let n = native_count(r, "Enum Names")?;
            r.take(n * 16)?; // FName + int64 per entry
            r.u8()?; // CppForm
        }
        // `UFontFace::Serialize` (FontFace.cpp): a cooked flag, then an
        // inline-data flag; the face bytes follow only when that is set. CE ships
        // every face out of line, so the inline payload is left unmodeled rather
        // than guessed at.
        "FontFace" => {
            let at = r.o;
            r.u32()?; // bCooked
            if r.u32()? != 0 {
                r.o = at;
                return Ok(false);
            }
        }
        // `UAkAudioEvent`: the localized event cooked data as a property block,
        // then the duration/attenuation scalars. The cooked data can carry a
        // bulk payload this reader does not model, so on any mismatch rewind and
        // report an unmodeled tail instead of failing the export.
        "AkAudioEvent" => {
            let at = r.o;
            let ok = read_struct(r, "WwiseLocalizedEventCookedData", usmap, 0).is_ok()
                && r.take(16).is_ok(); // MaximumDuration, MinimumDuration, IsInfinite, MaxAttenuationRadius
            if !ok {
                r.o = at;
                return Ok(false);
            }
        }
        // The rest of the Wwise asset types follow the same shape as
        // `UAkAudioEvent`: each appends its cooked data as an ordinary
        // unversioned property block. The Wwise plugin is not in the UE source
        // tree, but it does not need to be — the `.usmap` describes every one of
        // these structs, so the only thing to know is which struct each class
        // writes.
        "AkStateValue" | "AkSwitchValue" => {
            let at = r.o;
            if read_struct(r, "WwiseGroupValueCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        "AkRtpc" => {
            let at = r.o;
            if read_struct(r, "WwiseGameParameterCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        "AkAuxBus" => {
            let at = r.o;
            if read_struct(r, "WwiseLocalizedAuxBusCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        "AkInitBank" => {
            let at = r.o;
            if read_struct(r, "WwiseInitBankCookedData", usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULevel`: the actor list, the level `FURL`, the BSP model and its
        // components, the level script actor, the nav-list bounds, and the two
        // precomputed lighting/visibility payloads. Any mismatch rewinds and
        // reports an unmodeled tail rather than failing the export.
        "Level" => {
            let at = r.o;
            if read_level_tail(r).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UModel` (BSP): strip flags, bounds, the bulk geometry arrays, the
        // surface table, then the vertex buffer, lighting GUID and lightmass
        // settings.
        "Model" => {
            let at = r.o;
            if let Err(e) = read_model_tail(r) {
                if trace_enabled() {
                    eprintln!("Model tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UNiagaraScript` appends its cooked GPU shader maps. Everything before
        // them in `Serialize` is editor-only, so in a cooked package they follow
        // the property block directly.
        "NiagaraScript" => {
            let at = r.o;
            let ok = read_niagara_shader_maps(r);
            if let Err(e) = ok {
                if std::env::var("BLAM_TAIL_WHY").is_ok() {
                    eprintln!("  NiagaraScript tail bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UDNAAsset::Serialize` (RigLogic plugin) reads **two** DNA streams
        // back to back: the behavior layers, then the geometry. The geometry one
        // ships as a stub in a cooked build, since geometry is kept only for the
        // editor — but it is always written, so the uasset layout matches
        // between editor and game.
        // Version 5 headers carry per-section sizes, so the first stream's end
        // is computable. Version 1 headers carry only offsets, and nothing in
        // them gives a length — but `UDNAAsset` is the last class in the chain,
        // so the *second* stream must end exactly at the end of the export. That
        // makes the boundary derivable rather than guessed: it is the unique
        // later `DNA` signature whose own sized index lands on `r.b.len()`.
        "DNAAsset" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                match dna_stream_end(r.b, r.o)? {
                    Some(end) => r.o = end,
                    None => {
                        let floor = dna_unsized_floor(r.b, r.o)?;
                        let start = (floor..r.b.len().saturating_sub(3))
                            .filter(|&i| &r.b[i..i + 3] == b"DNA")
                            .find(|&i| {
                                matches!(dna_stream_end(r.b, i), Ok(Some(e)) if e == r.b.len())
                            })
                            .with_context(|| {
                                format!("no second DNA stream closing the export after {floor}")
                            })?;
                        r.o = start;
                    }
                }
                let end = dna_stream_end(r.b, r.o)?
                    .context("the second DNA stream is itself unsized")?;
                if end != r.b.len() {
                    bail!("second DNA stream ends at {end}, not the export end {}", r.b.len());
                }
                r.o = end;
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UModelComponent::Serialize`: the owning `UModel`, then its elements —
        // each a `MapBuildDataId` GUID, the component and material references,
        // and the BSP node indices, which are `uint16`.
        "ModelComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // Model
                let elements = native_count(r, "model elements")?;
                for _ in 0..elements {
                    r.take(16)?; // MapBuildDataId
                    r.i32()?; // Component
                    r.i32()?; // Material
                    let nodes = native_count(r, "element nodes")?;
                    r.take(nodes * 2)?;
                }
                // The component closes with its own index and node list.
                r.u32()?; // ComponentIndex
                let nodes = native_count(r, "component nodes")?;
                r.take(nodes * 2)?;
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ARecastNavMesh::Serialize` writes a version and then a byte count that
        // the loader simply **seeks past** — the navmesh is rebuilt rather than
        // loaded. The count is measured from its own position, so this is a
        // self-describing skip. Measured: version 26, then 104 bytes, ending the
        // 108-byte tail exactly.
        "RecastNavMesh" => {
            r.u32()?; // NavMeshVersion
            let at = r.o;
            let size = r.u32()? as usize;
            let end = at.checked_add(size).filter(|e| *e >= r.o && *e <= r.b.len());
            match end {
                Some(e) => r.o = e,
                None => {
                    r.o = at - 4;
                    return Ok(false);
                }
            }
        }
        // `UPCGMetadata::Serialize`: an attribute count, then per attribute its
        // name, an `EPCGMetadataTypes` id, the shared `FPCGMetadataAttributeBase`
        // header, and finally the typed value array plus a default — both sized
        // by that type id.
        "PCGMetadata" => {
            let at = r.o;
            let why = std::env::var("BLAM_TAIL_WHY").is_ok();
            let ok = (|| -> Result<()> {
                let attrs = native_count(r, "PCG attributes")?;
                if why {
                    eprintln!("  PCGMetadata: {attrs} attributes, body ends at {}", r.b.len());
                }
                for ai in 0..attrs {
                    let a0 = r.o;
                    r.take(8)?; // attribute FName
                    let type_id = r.i32()?;
                    // FPCGMetadataAttributeBase::Serialize
                    let entries = native_count(r, "EntryToValueKeyMap")?;
                    r.take(entries * 12)?; // int64 entry key + int32 value key
                    r.i32()?; // ParentAttributeId
                    r.take(8)?; // Name
                    r.i32()?; // AttributeId
                    // `Values` then a single `DefaultValue`, both of that type.
                    let values = native_count(r, "PCG attribute values")?;
                    if why {
                        eprintln!(
                            "    attr {ai}: type {type_id} @ {a0}, {entries} entries, {values} values"
                        );
                    }
                    match pcg_value_size(type_id) {
                        Some(size) => {
                            r.take(values * pcg_array_element_size(type_id).unwrap_or(size))?;
                            r.take(size)?; // DefaultValue
                        }
                        // `String` carries its own length per element.
                        None if type_id == 9 => {
                            for _ in 0..=values {
                                r.fstring()?;
                            }
                        }
                        // `SoftObjectPath`/`SoftClassPath` go through
                        // `FSoftObjectPath::Serialize`: an `FTopLevelAssetPath`
                        // (package and asset `FName`s) then a sub-path
                        // `FString`. Note this is *not* the three-`FName` form
                        // the unversioned property reader uses for the same
                        // type — a plain archive writes the sub-path as a string.
                        None if type_id == 13 || type_id == 14 => {
                            for _ in 0..=values {
                                r.take(16)?; // PackageName + AssetName
                                r.fstring()?; // SubPathString
                            }
                        }
                        None => bail!("unmodeled EPCGMetadataTypes id {type_id} @ {}", r.o),
                    }
                }
                // The metadata closes with its parent entry keys.
                let at_parents = r.o;
                let parents = native_count(r, "ParentKeys")?;
                r.take(parents * 8)?; // PCGMetadataEntryKey is an int64
                if why {
                    eprintln!(
                        "    ParentKeys: {parents} @ {at_parents}, ends {} of {}",
                        r.o,
                        r.b.len()
                    );
                    let lo = at_parents.saturating_sub(16);
                    eprint!("      around count @{lo}:");
                    for x in &r.b[lo..(at_parents + 48).min(r.b.len())] {
                        eprint!(" {x:02x}");
                    }
                    eprintln!();
                    let e = r.o;
                    eprint!("      at end @{e}:");
                    for x in &r.b[e.saturating_sub(16)..(e + 32).min(r.b.len())] {
                        eprint!(" {x:02x}");
                    }
                    eprintln!();
                }
                Ok(())
            })();
            if let Err(e) = ok {
                if std::env::var("BLAM_TAIL_WHY").is_ok() {
                    eprintln!("  PCGMetadata bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `USoundNode::Serialize` writes an `FStripDataFlags`; the graph node it
        // guards is editor-only, so a cook stops there. `USoundCue` does the
        // same after its own `Super::Serialize`.
        "SoundNode" | "SoundCue" => {
            r.take(2)?; // FStripDataFlags
        }
        // `USoundNodeWavePlayer` then writes its wave as a hard reference.
        "SoundNodeWavePlayer" => {
            r.i32()?; // SoundWave, an FPackageIndex
        }
        // `UMorphTarget::Serialize`: strip flags, then the LOD models.
        //
        // Each `FMorphTargetLODModel` opens with
        // `bool bVerticesAreStrippedForCookedBuilds` — **four** bytes, an
        // `FArchive` bool — and a cook always sets it, replacing the whole
        // vertex-delta array with a bare `NumVertices`. The `SourceFilename`
        // that closes the element is written as an *empty* `FString` rather
        // than skipped, so the four zero bytes at the end are load-bearing.
        "MorphTarget" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let strip = r.u16()?;
                if strip & 0x02 != 0 {
                    return Ok(()); // audio-visual data stripped
                }
                let lods = native_count(r, "MorphLODModels")?;
                for _ in 0..lods {
                    if r.u32()? != 0 {
                        r.i32()?; // NumVertices, the array having been stripped
                    } else {
                        // `FMorphTargetDelta`: two `FVector3f`s and a `uint32`.
                        let verts = native_count(r, "morph vertices")?;
                        r.take(verts * 28)?;
                    }
                    r.i32()?; // NumBaseMeshVerts
                    let sections = native_count(r, "SectionIndices")?;
                    r.take(sections * 4)?;
                    r.u32()?; // bGeneratedByEngine
                    r.fstring()?; // SourceFilename, empty in a cook
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USoundWave::Serialize`: a packed `uint32` of flags (bit 0 = cooked),
        // the cue points, the compressed-data GUID and the streamed chunk
        // table.
        //
        // Whether the compressed audio is inline (`FFormatContainer`) or
        // streamed (`FStreamedAudioPlatformData`) is decided at cook time by
        // `IsStreaming()` and is recorded nowhere, so the two layouts can only
        // be told apart by trying one. Campaign Evolved streams every one of
        // its waves, so the streamed form is read and anything that does not
        // account for the export exactly is rewound and reported — `USoundWave`
        // is last in its chain, which makes "ends on the final byte" a real
        // check rather than a plausibility one.
        "SoundWave" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let flags = r.u32()?;
                if flags & 1 == 0 {
                    bail!("uncooked SoundWave");
                }
                // `SerializeCuePoints`, cooked. `FSoundWaveCuePoint` has no
                // hand-written serializer — its `operator<<` calls
                // `SerializeItem`, so each element is an ordinary unversioned
                // property block, not a fixed-size record.
                let cues = native_count(r, "CuePoints")?;
                for _ in 0..cues {
                    read_struct(r, "SoundWaveCuePoint", usmap, 0)?;
                }
                r.take(16)?; // CompressedDataGuid
                let chunks = native_count(r, "audio chunks")?;
                r.name()?; // AudioFormat
                for _ in 0..chunks {
                    // IsCooked 1, HasSeekOffset 2, IsInlined 4.
                    let chunk_flags = r.u32()?;
                    read_inline_bulk_data(r, bulk_data, "audio chunk")?;
                    r.i32()?; // DataSize
                    r.i32()?; // AudioDataSize
                    if chunk_flags & 2 != 0 {
                        r.i32()?; // SeekOffsetInAudioFrames
                    }
                }
                if r.o != r.b.len() {
                    bail!("streamed layout ended at {} of {}", r.o, r.b.len());
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ALevelInstance::Serialize` appends `LevelInstanceActorGuid`; the
        // packed variant's own `PackedVersion` is editor-only.
        "LevelInstance" => {
            r.take(16)?; // FGuid
        }
        // `UVectorFieldStatic::Serialize` appends its volume texture source as
        // a single bulk payload, which the cook forces inline.
        "VectorFieldStatic" => {
            let at = r.o;
            if read_inline_bulk_data(r, bulk_data, "VectorFieldStatic SourceData").is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UPCGLandscapeCache::Serialize`: a count, then per entry a
        // `TPair<FGuid, FIntPoint>` key and `FPCGLandscapeCacheEntry::Serialize`
        // — half-size, stride, the layer names, and a bulk-data handle the cook
        // deliberately keeps *out* of line (`BULKDATA_Force_NOT_InlinePayload`),
        // so only its index is here.
        "PCGLandscapeCache" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let entries = native_count(r, "PCGLandscapeCache entries")?;
                for _ in 0..entries {
                    r.take(16 + 8)?; // FGuid + FIntPoint
                    r.take(24)?; // FVector PointHalfSize
                    r.i32()?; // Stride
                    let names = native_count(r, "LayerDataNames")?;
                    r.take(names * 8)?;
                    read_inline_bulk_data(r, bulk_data, "landscape cache entry")?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `USkyAtmosphereComponent::Serialize` appends `bStaticLightingBuiltGUID`.
        // The version gate around it only excludes components converted from the
        // old `AtmosphericFog`, which CE has none of, and the 16-byte tail on
        // every one of these confirms it.
        "SkyAtmosphereComponent" => {
            r.take(16)?; // FGuid
        }
        // `UPhysicsAsset::Serialize` appends `CollisionDisableTable`, a
        // `TMap<FRigidBodyIndexPair, bool>` — two `int32` body indices and a
        // 32-bit bool per entry. Measured on `PHYS_COV_Door_A`: count 3, then
        // the pairs (0,1), (0,2) and (1,2), all false, ending the export exactly.
        "PhysicsAsset" => {
            let n = native_count(r, "CollisionDisableTable")?;
            r.take(n * 12)?;
        }
        // `UStringTable::Serialize` hands off to `FStringTable::Serialize`: the
        // table namespace, the key/source-string entries, and a per-key
        // meta-data map. Every string here is an `FString`, including the text
        // keys, which serialize as strings rather than as names.
        "StringTable" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.fstring()?; // TableNamespace
                let entries = native_count(r, "StringTable entries")?;
                for _ in 0..entries {
                    r.fstring()?; // Key
                    r.fstring()?; // SourceString
                }
                let keys = native_count(r, "StringTable meta-data keys")?;
                for _ in 0..keys {
                    r.fstring()?; // key
                    let meta = native_count(r, "meta-data entries")?;
                    for _ in 0..meta {
                        r.take(8)?; // meta-data id FName
                        r.fstring()?; // value
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UTexture` writes only its strip flags in a cooked stream.
        "Texture" => {
            r.take(2)?;
        }
        // `UTexture2D`: strip flags, `bCooked`, `bSerializeMipData`, then the
        // cooked platform data — a list of `(pixel-format FName, int64
        // SkipOffset, FTexturePlatformData)` terminated by a `None` name.
        //
        // `SkipOffset` is a delta from its own location to the end of that
        // platform data, so the whole block can be *skipped* rather than
        // modeled. Measured on a 3038-byte texture: format name at 42,
        // SkipOffset 2980 at 50 → 3030, then the 8-byte `None` terminator ends
        // the export exactly.
        "Texture2D" => {
            let at = r.o;
            if read_texture_tail(r, true).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // The other cooked texture shapes share `UTexture::SerializeCookedPlatformData`
        // and differ from `UTexture2D` only in *not* writing its
        // `bSerializeMipData` flag. Between them they carry 179 MB of otherwise
        // unread payload, all of it skippable by the same `SkipOffset`.
        "TextureCube" | "VolumeTexture" | "Texture2DArray" => {
            let at = r.o;
            if read_texture_tail(r, false).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UBodySetup`: the setup GUID, a cooked flag, a has-cooked-data flag,
        // then an `FFormatContainer` — a count, and per format an `FName` and an
        // `FByteBulkData`.
        //
        // In a Zen package an `FByteBulkData` header is just an **int32 index**
        // into the package's bulk-data map; the payload itself is stored inline
        // right after it. Measured on `SM_Basis_HS`: `bulk[0]` is offset 76,
        // size 23802 — 76 is exactly where the index ends, and 76 + 23802 is the
        // export length. The offset is re-checked against the cursor, so a
        // payload that is *not* inline is left alone instead of over-consumed.
        "BodySetup" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(16)?; // BodySetupGuid
                if r.u32()? == 0 {
                    return Ok(()); // not cooked
                }
                r.u32()?; // bHasCookedData
                let n = native_count(r, "CookedFormatData")?;
                for _ in 0..n {
                    r.name()?;
                    let index = r.i32()?;
                    let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) else {
                        bail!("bulk data index {index} out of range");
                    };
                    if offset as usize != r.o {
                        bail!("bulk payload at {offset} is not inline at {}", r.o);
                    }
                    r.take(size.max(0) as usize)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UHierarchicalInstancedStaticMeshComponent` (and the foliage variant
        // that shares its serializer) appends the instance cluster tree as a
        // bulk array, which carries its own element size.
        "HierarchicalInstancedStaticMeshComponent" => {
            let at = r.o;
            if read_bulk_array(r, "ClusterTree").is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UStruct::Serialize` (Class.cpp): `SuperStruct`, then a
        // `TArray<UField*> ChildArray`, then `SerializeProperties` — an `int32`
        // count and that many `FField`s — then the Kismet script.
        //
        // There is no padding anywhere in that sequence, so this reads it
        // straight through. An earlier version probed a few word offsets for the
        // field count; that silently accepted a wrong interpretation whenever
        // the real parse failed, reporting a bogus "decoded" prefix instead of a
        // tail, which is how three `FProperty` layout bugs stayed hidden.
        "Struct" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // SuperStruct
                let children = native_count(r, "ChildArray")?;
                r.take(children * 4)?;
                try_read_struct_fields_and_script(r)
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UScriptStruct::Serialize` (Class.cpp) adds exactly one `uint32` —
        // the non-computed half of `StructFlags`. Everything else about a
        // script struct is recomputed from `CppStructOps` on load.
        "ScriptStruct" => {
            let at = r.o;
            if r.u32().is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UUserDefinedStruct::Serialize` then writes a **default instance of
        // itself** (`SerializeItem`), i.e. one unversioned property block whose
        // schema is the `FField` chain this very export just defined — not
        // anything the `.usmap` knows about. `Struct`'s arm stashed that chain.
        //
        // A class-default object returns before writing it (`HasAnyFlags(
        // RF_ClassDefaultObject)`), so it has no instance to read.
        "UserDefinedStruct" => {
            const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
            if object_flags & RF_CLASS_DEFAULT_OBJECT != 0 {
                return Ok(true);
            }
            let Some(fields) = r.struct_fields.clone() else { return Ok(false) };
            let schema: Vec<&UsmapProperty> = fields.iter().collect();
            let at = r.o;
            if read_struct_with_schema(r, "UserDefinedStruct default", &schema, usmap, 0).is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UDataTable::LoadStructData`: an `int32` row count, then per row an
        // `FName` key and one property block against the table's `RowStruct`.
        //
        // `RowStruct` is an `FPackageIndex` in the table's own property block,
        // so the schema for the rows lives outside this export entirely —
        // either a native struct named by a script import, or a
        // `UUserDefinedStruct` exported by another package. Without a resolver
        // to turn that reference into a schema the rows cannot be walked at
        // all, so report them as an unmodeled tail rather than guess.
        // `UCompositeDataTable` adds nothing of its own and is covered by
        // inheriting this arm.
        "DataTable" => {
            let Some(resolver) = ctx.resolver else { return Ok(false) };
            let Some(PropValue::Object(row_ref)) = props.get("RowStruct") else {
                return Ok(false);
            };
            let Some(row_struct) = resolver.struct_name(*row_ref) else { return Ok(false) };
            let at = r.o;
            let ok = (|| -> Result<()> {
                let rows = native_count(r, "DataTable rows")?;
                for i in 0..rows {
                    let key = r.name()?;
                    read_struct(r, &row_struct, usmap, 0)
                        .with_context(|| format!("row {i} ({key})"))?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UFunction` appends its flags, plus a replication offset for a
        // networked function (`FUNC_Net`).
        "Function" => {
            let flags = r.u32()?;
            if flags & 0x0040 != 0 {
                r.u16()?; // RepOffset, for FUNC_Net
            }
            // Blueprint event-graph fast-call info, always serialized to keep
            // the stream in sync even when the feature is compiled out.
            r.i32()?; // EventGraphFunction
            r.i32()?; // EventGraphCallOffset
        }
        // `UClass`, after `UStruct`: the function map, class flags and
        // ownership, the implemented-interface table, and the class default
        // object.
        "Class" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let funcs = native_count(r, "FuncMap")?;
                r.take(funcs * 12)?; // FName + FPackageIndex
                r.u32()?; // ClassFlags
                r.i32()?; // ClassWithin
                r.name()?; // ClassConfigName
                r.i32()?; // ClassGeneratedBy
                // `FImplementedInterface`: class, pointer offset, and a
                // four-byte "implemented by K2" flag.
                let ifaces = native_count(r, "Interfaces")?;
                r.take(ifaces * 12)?;
                r.u32()?;
                r.name()?;
                r.u32()?; // bCooked
                r.i32()?; // ClassDefaultObject
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UDynamicMesh::Serialize` (UDynamicMesh.cpp:312) appends `Ar << *Mesh`
        // after the tagged properties — nothing else.
        "DynamicMesh" => {
            let at = r.o;
            if let Err(e) = read_dynamic_mesh(r) {
                if std::env::var("BLAM_TAIL_WHY").is_ok() {
                    eprintln!("  DynamicMesh bailed @ {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UGeometryCollection::Serialize` (GeometryCollectionObject.cpp:939)
        // writes a cooked flag after the tagged properties, then the
        // `FManagedArrayCollection` through an `FChaosArchive`, then a second
        // cooked flag gating the render data.
        "GeometryCollection" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.u32()?; // bIsCookedOrCooking
                read_managed_array_collection(r)?;
                let cooked = r.u32()? != 0;
                if cooked {
                    read_geometry_collection_render_data(r)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                if std::env::var("BLAM_TAIL_WHY").is_ok() {
                    eprintln!("  GeometryCollection bailed @ {}: {:#}", r.o, ok.unwrap_err());
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UComputeGraph::Serialize` (ComputeGraph.cpp:43) appends one
        // `FComputeKernelResourceSet` per kernel; each is a count of resources
        // (ComputeGraph.cpp:948) and each resource a cooked flag, a validity
        // flag, and a shader map (ComputeKernelShared.cpp:178). Unlike Niagara's
        // it uses the plain `FShaderMapPointerTable`.
        "ComputeGraph" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let kernels = native_count(r, "compute kernels")?;
                for _ in 0..kernels {
                    let resources = native_count(r, "compute kernel resources")?;
                    for _ in 0..resources {
                        let cooked = r.u32()? != 0;
                        if cooked && r.u32()? != 0 {
                            read_shader_map(r, false)?;
                        }
                    }
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UControlRigBlueprintGeneratedClass::Serialize`
        // (ControlRigBlueprintGeneratedClass.cpp:16) embeds an entire `URigVM`
        // by value — it builds a transient VM and calls `VM->Serialize(Ar)` —
        // and then writes its graph-function store. Note it calls
        // `UBlueprintGeneratedClass::Serialize` directly, deliberately skipping
        // `URigVMBlueprintGeneratedClass`, which is why that class has no arm.
        "ControlRigBlueprintGeneratedClass" => {
            let at = r.o;
            // `FRigVMGraphFunctionStore::operator<<`
            // (RigVMGraphFunctionHost.h:70) writes only `PublicFunctions`;
            // `PrivateFunctions` goes out solely to reference collectors. CE
            // ships the list empty, so rather than guess at
            // `FRigVMGraphFunctionData`'s layout, a non-empty list is reported
            // as an unmodeled tail.
            let ok = read_rigvm(r, usmap).is_ok()
                && matches!(native_count(r, "PublicFunctions"), Ok(0));
            if !ok {
                r.o = at;
                return Ok(false);
            }
        }
        // `UBlueprintGeneratedClass` appends its cooked editor tags, but only
        // when there is more than a trailing word left to read.
        "BlueprintGeneratedClass" => {
            let at = r.o;
            if r.b.len().saturating_sub(r.o) > 4 {
                let ok = (|| -> Result<()> {
                    let n = native_count(r, "EditorTags")?;
                    for _ in 0..n {
                        r.name()?;
                        r.fstring()?;
                    }
                    Ok(())
                })();
                if ok.is_err() {
                    r.o = at;
                    return Ok(false);
                }
            }
        }
        // `UNiagaraSystem` appends one `FNiagaraEmitterCompiledData` property
        // block per emitter. Measured on `NS_collision`: a count of 1 followed
        // by a nine-value property header.
        "NiagaraSystem" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                let n = native_count(r, "NiagaraEmitterCompiledData")?;
                for _ in 0..n {
                    read_struct(r, "NiagaraEmitterCompiledData", usmap, 0)?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULandscapeHeightfieldCollisionComponent`: a cooked flag, then the
        // cooked collision data as a bulk array (which carries its own element
        // size).
        "LandscapeHeightfieldCollisionComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                if r.u32()? != 0 {
                    read_bulk_array(r, "CookedCollisionData")?;
                }
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `ULandscapeComponent`: the grass data — element count, a
        // `TMap<FPackageIndex, int32>` of weight offsets, and the packed
        // height/weight bytes — then a cooked flag. Measured on an A50 landscape
        // component: 4096 elements, two weight offsets, 16384 data bytes, and
        // the four-byte flag land exactly on the 16874-byte export end.
        "LandscapeComponent" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.i32()?; // NumElements
                let offsets = native_count(r, "grass weight offsets")?;
                r.take(offsets * 8)?; // FPackageIndex + int32
                let data = native_count(r, "HeightWeightData")?;
                r.take(data)?;
                r.u32()?; // bCooked
                Ok(())
            })();
            if ok.is_err() {
                r.o = at;
                return Ok(false);
            }
        }
        // `UStaticMesh`: strip flags, cooked flag, body setup / nav collision,
        // lighting GUID, sockets, then `FStaticMeshRenderData`'s LOD array.
        // Anything past the LODs (Nanite resources, ray-tracing proxy, distance
        // fields) is not modeled, so the walk stops there.
        "StaticMesh" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(2)?; // FStripDataFlags
                r.u32()?; // bCooked
                r.i32()?; // BodySetup
                r.i32()?; // NavCollision
                r.take(16)?; // LightingGuid
                let sockets = native_count(r, "Sockets")?;
                r.take(sockets * 4)?;
                let lods = native_count(r, "LODs")?;
                for _ in 0..lods {
                    read_static_mesh_lod(r)?;
                }
                r.u8()?; // numInlinedLODs
                // FNaniteResources
                r.take(2)?; // FStripDataFlags
                r.u32()?; // ResourceFlags
                r.i32()?; // StreamablePages: FByteBulkData index (Zen)
                let root = native_count(r, "Nanite RootData")?;
                r.take(root)?;
                let pages = native_count(r, "PageStreamingStates")?;
                r.take(pages * 20)?;
                let nodes = native_count(r, "HierarchyNodes")?;
                // `FPackedHierarchyNode` = NANITE_MAX_BVH_NODE_FANOUT (4) slices,
                // each `FSphere3f` + `FVector3f` + 2 halves + `FVector3f` + 2
                // uint32 = 52 bytes, so 208 per node.
                r.take(nodes * 208)?;
                let roots = native_count(r, "HierarchyRootOffsets")?;
                r.take(roots * 4)?;
                let deps = native_count(r, "PageDependencies")?;
                r.take(deps * 4)?;
                let imposter = native_count(r, "ImposterAtlas")?;
                r.take(imposter * 2)?;
                r.take(16)?; // NumRootPages, PositionPrecision, NormalPrecision, NumInputTriangles
                r.take(12)?; // NumInputVertices, NumInputMeshes+TexCoords (u16), NumClusters
                let ray_proxy = r.u32()?; // bHasRayTracingProxy
                if ray_proxy != 0 {
                    // FStaticMeshRayTracingProxy: strip flags, a
                    // "using rendering LODs" flag, then one entry per LOD.
                    r.take(2)?;
                    r.u32()?; // bUsingRenderingLODs
                    let proxy_lods = native_count(r, "ray tracing proxy LODs")?;
                    for _ in 0..proxy_lods {
                        if r.u32()? != 0 {
                            // bOwnsBuffers
                            let sec = native_count(r, "proxy sections")?;
                            r.take(sec * 40)?;
                        }
                        r.u32()?; // bOwnsRayTracingGeometry
                        // StreamableData: an `FByteBulkData` index; its payload
                        // is inline only when the map's offset is right here.
                        let index = r.i32()?;
                        if let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) {
                            if offset as usize == r.o {
                                r.take(size.max(0) as usize)?;
                            }
                        }
                    }
                }
                // SerializeInlineDataRepresentations: strip flags, then per LOD
                // a validity flag and an `FDistanceFieldVolumeData5`.
                // `SerializeInlineDataRepresentations` — the **card
                // representation**, not the distance field (that follows).
                // Class strip bit 1 is `CardRepresentationDataStripFlag`.
                let cr_global = r.u8()?;
                let cr_class = r.u8()?;
                if cr_global & 2 == 0 && cr_class & 2 == 0 {
                    for _ in 0..lods {
                        if r.u32()? == 0 {
                            continue;
                        }
                        r.take(49)?; // Bounds (FBox)
                        r.u32()?; // bMostlyTwoSided
                        // `FLumenCardBuildData`: an `FLumenCardOBB` of five
                        // `FVector3f` (60 bytes) plus the axis-aligned direction
                        // index. `MaxLodLevel`/`LODLevel` are not written in 5.1+.
                        let cards = native_count(r, "CardBuildData")?;
                        r.take(cards * 61)?;
                    }
                }
                // Distance-field volumes, then the render data's own bounds and
                // LOD screen sizes. Class strip bit 0 gates the distance fields.
                let df_global = r.u8()?;
                let df_class = r.u8()?;
                if df_global & 2 == 0 && df_class & 1 == 0 {
                    for _ in 0..lods {
                        if r.u32()? == 0 {
                            continue;
                        }
                        // `FDistanceFieldVolumeData5`. `LocalSpaceMeshBounds` is
                        // an **`FBox3f`** — six floats and `IsValid`, 25 bytes,
                        // not the 49-byte double-width `FBox`. Measured on
                        // `SM_Sphere_64Seg`: ±100 bounds as floats, then three
                        // 56-byte `FSparseDistanceFieldMip` (a 6×6×6 indirection,
                        // brick count, UV scale 0.4762, UV add 0.5, scale/bias,
                        // bulk offset and size 864).
                        r.take(25)?;
                        r.u32()?; // bMostlyTwoSided
                        r.take(3 * 56)?;
                        let always = native_count(r, "AlwaysLoadedMip")?;
                        r.take(always)?;
                        r.i32()?; // StreamableMips: FByteBulkData index
                    }
                }
                r.take(56)?; // Bounds: FBoxSphereBounds
                r.u32()?; // bLODsShareStaticLighting
                // `ScreenSize[MAX_STATIC_LODS_UE4]`, each an `FPerPlatformFloat`.
                r.take(8 * 8)?;
                // Two bytes of strip flags close out the render data, then
                // `UStaticMesh` finishes with its SpeedTree flag and material
                // table. Measured on `SM_Basis_HS`, whose final 46 bytes resolve
                // exactly: one `FStaticMaterial` = material import -4, an empty
                // slot `FName`, `bInitialized`, `bOverrideDensities` and the four
                // `LocalUVDensities` floats (404.85, 411.49, 0, 0).
                r.take(2)?;
                r.u32()?; // bHasSpeedTreeWind
                let materials = native_count(r, "StaticMaterials")?;
                r.take(materials * 36)?;
                if trace_enabled() {
                    eprintln!("after render data @ {}", r.o);
                }
                if trace_enabled() {
                    eprintln!(
                        "Nanite: root {root}, {pages} pages, {nodes} nodes, rayproxy {ray_proxy} -> {}",
                        r.o
                    );
                }
                Ok(())
            })();
            if let Err(e) = ok {
                if trace_enabled() {
                    eprintln!("StaticMesh tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `UAnimationAsset::Serialize` writes its `SkeletonGuid` — the 16 bytes
        // that sit between the object trailer and `UAnimSequence`'s strip flags.
        "AnimationAsset" => {
            r.take(16)?;
        }
        // `UAnimSequence`: strip flags (raw animation data is editor-only and
        // stripped in a cook), then the compressed-data block per UE 5.5.4
        // `FCompressedAnimSequence::SerializeCompressedData`.
        "AnimSequence" => {
            let at = r.o;
            let ok = (|| -> Result<()> {
                r.take(2)?; // FStripDataFlags
                if r.u32()? == 0 {
                    return Ok(()); // bSerializeCompressedData
                }
                r.i32()?; // CompressedRawDataSize
                let tracks = native_count(r, "CompressedTrackToSkeletonMapTable")?;
                r.take(tracks * 4)?;
                // `FAnimCompressedCurveIndexedName`'s `operator<<` writes ONLY
                // `CurveName` — its `CurveIndex` is serialized just for
                // `IsCountingMemory()`, so on load the element is 8 bytes, not
                // the 12 the struct declares.
                let curves = native_count(r, "IndexedCurveNames")?;
                r.take(curves * 8)?;
                let num_bytes = native_count(r, "CompressedByteStream")?;
                if trace_enabled() {
                    eprintln!("anim: tracks {tracks} curves {curves} numbytes {num_bytes} @ {}", r.o);
                }
                let use_bulk = r.u32()? != 0;
                if !use_bulk {
                    r.take(num_bytes)?;
                }
                let bone_codec = r.fstring()?;
                let curve_codec = r.fstring()?;
                let curve_bytes = native_count(r, "CompressedCurveByteStream")?;
                r.take(curve_bytes)?;
                if trace_enabled() {
                    eprintln!(
                        "anim: bone codec {bone_codec:?} curve codec {curve_codec:?} @ {}",
                        r.o
                    );
                }
                // The bone codec's own payload. CE compresses with ACL, whose
                // `FACLCompressedAnimDataBase::SerializeCompressedData` writes
                // the base `CompressedNumberOfKeys` then `bCompressionFailed`;
                // the compressed clip itself lives in `CompressedByteStream`,
                // already consumed above.
                r.i32()?; // CompressedNumberOfKeys (ICompressedAnimData base)
                if bone_codec.starts_with("AnimBoneCompressionCodec_ACL") {
                    r.u32()?; // FACLCompressedAnimDataBase::bCompressionFailed
                } else if bone_codec.starts_with("AnimCompress_") {
                    // `FUECompressedAnimData`: four `TEnumAsByte` formats, then
                    // three `SerializeView` counts (the payload itself lives in
                    // `CompressedByteStream`, already read) and `StripSize`.
                    r.take(4)?;
                    for _ in 0..3 {
                        r.i32()?;
                    }
                    r.i32()?; // CompressedScaleOffsets.StripSize
                } else {
                    bail!("unmodeled bone compression codec {bone_codec:?}");
                }
                let _ = curve_codec;
                r.u32()?; // UAnimSequence's trailing bTemp
                Ok(())
            })();
            if let Err(e) = ok {
                if trace_enabled() {
                    eprintln!("AnimSequence tail stopped at {}: {e:#}", r.o);
                }
                r.o = at;
                return Ok(false);
            }
        }
        // `AActor`: the cooked actor label, then its instance GUID pair.
        "Actor" => {
            if r.u32()? != 0 {
                r.fstring()?;
            }
            r.take(32)?; // FActorInstanceGuid = ActorGuid + ActorInstanceGuid
        }
        _ => {}
    }
    Ok(true)
}

/// `ULevel`'s natively-serialized tail.
fn read_level_tail(r: &mut Reader) -> Result<()> {
    let object_array = |r: &mut Reader, what: &str| -> Result<()> {
        let n = native_count(r, what)?;
        r.take(n * 4)?;
        Ok(())
    };
    object_array(r, "Actors")?;
    // `FURL`: protocol, host, map and portal strings, the option list, then the
    // port and a four-byte validity flag.
    for _ in 0..4 {
        r.fstring()?;
    }
    let ops = native_count(r, "URL options")?;
    for _ in 0..ops {
        r.fstring()?;
    }
    r.i32()?; // Port
    r.u32()?; // Valid
    r.i32()?; // Model
    object_array(r, "ModelComponents")?;
    r.i32()?; // LevelScriptActor
    r.i32()?; // NavListStart
    r.i32()?; // NavListEnd
    // `FPrecomputedVisibilityHandler`: bucket origin, cell sizes, bucket counts,
    // then the buckets themselves.
    r.take(16)?; // FVector2D bucket origin
    r.take(16)?; // cell size XY/Z, bucket size XY, bucket count
    let buckets = native_count(r, "visibility buckets")?;
    for _ in 0..buckets {
        r.i32()?; // CellDataSize
        let cells = native_count(r, "visibility cells")?;
        r.take(cells * 28)?; // FVector min + two uint16
        let chunks = native_count(r, "visibility chunks")?;
        for _ in 0..chunks {
            r.u32()?; // bCompressed
            r.i32()?; // UncompressedSize
            let bytes = native_count(r, "visibility chunk data")?;
            r.take(bytes)?;
        }
    }
    // `FPrecomputedVolumeDistanceField`.
    r.f32()?;
    r.take(49)?; // FBox
    r.take(12)?; // volume size X/Y/Z
    let data = native_count(r, "distance field data")?;
    r.take(data * 4)?;
    Ok(())
}

/// `UModel`'s natively-serialized tail.
fn read_model_tail(r: &mut Reader) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    r.take(56)?; // FBoxSphereBounds
    read_bulk_array(r, "Vectors")?;
    read_bulk_array(r, "Points")?;
    read_bulk_array(r, "Nodes")?;
    // `FBspSurf`: two `FPackageIndex`es and six int32s (32 bytes), an
    // `FPlane4f` (16), then `LightMapScale` and `iLightmassIndex` — 56 bytes.
    // `UModel` uses the *float* math variants in UE5, which the stream confirms:
    // its `Vectors`/`Points` bulk arrays have an element size of 12
    // (`FVector3f`), not 24. Using a double-width plane drifts the walk and
    // blows up on the `Verts` element size.
    let surfs = native_count(r, "Surfs")?;
    r.take(surfs * 56)?;
    read_bulk_array(r, "Verts")?;
    r.i32()?; // NumSharedSides
    r.u32()?; // RootOutside
    r.u32()?; // Linked
    r.u32()?; // NumUniqueVertices
    // The vertex buffer is written unless both editor data and the class's
    // vertex-buffer flag are stripped. `FModelVertex` is **56** bytes —
    // `FVector3f` position and tangent X, an `FVector4f` tangent Z, and two
    // `FVector2f` UV pairs — the same float-variant rule the rest of `UModel`
    // follows. The LWC-double reading (112) survived 16,722 models because
    // every one of them has an empty vertex buffer; the two that do not blew
    // up on a 336-vertex buffer.
    if global_strip & 1 == 0 || class_strip & 1 == 0 {
        let verts = native_count(r, "model vertices")?;
        r.take(verts * 56)?;
    }
    r.take(16)?; // LightingGuid
    // `FLightmassPrimitiveSettings`: five four-byte bools and four floats.
    let settings = native_count(r, "LightmassSettings")?;
    r.take(settings * 36)?;
    Ok(())
}

/// A cooked texture tail, skipping the platform data via its `SkipOffset`.
///
/// `UTexture2D`, `UTextureCube`, `UVolumeTexture` and `UTexture2DArray` all
/// write strip flags, a cooked flag, and then `SerializeCookedPlatformData`.
/// The one difference is that **`UTexture2D` alone writes a `bSerializeMipData`
/// flag** between the two (Texture2D.cpp); the other three call the shared
/// serializer directly, so `has_mip_data_flag` selects between them.
fn read_texture_tail(r: &mut Reader, has_mip_data_flag: bool) -> Result<()> {
    r.take(2)?; // FStripDataFlags
    let cooked = r.u32()? != 0;
    if !cooked {
        return Ok(());
    }
    if has_mip_data_flag {
        r.u32()?; // bSerializeMipData
    }
    loop {
        let format = r.name()?;
        if format == "None" {
            return Ok(());
        }
        let loc = r.o;
        let skip = r.u64()? as i64;
        let end = loc
            .checked_add_signed(skip as isize)
            .filter(|e| *e > r.o && *e <= r.b.len())
            .with_context(|| format!("implausible texture SkipOffset {skip} @ {loc}"))?;
        r.o = end;
    }
}

/// A `UStruct`'s `FField` chain followed by its script blob. Unlike
/// [`try_read_script`] this accepts an empty script, which is the normal case
/// for a struct that carries no bytecode.
/// `UStruct::SerializeProperties`: an `int32` count and that many `FField`s,
/// as a schema the unversioned property walker can index by.
fn read_field_chain(r: &mut Reader) -> Result<Vec<UsmapProperty>> {
    let num = r.i32()?;
    if !(0..=4096).contains(&num) {
        bail!("implausible numFields {num}");
    }
    let mut props: Vec<UsmapProperty> = Vec::with_capacity(num as usize);
    for i in 0..num {
        let (name, ty, array_dim) =
            read_single_field(r)?.with_context(|| format!("null field at index {i}"))?;
        // A static array occupies `array_dim` consecutive schema slots, so the
        // next property's index is not simply `i + 1` — the same rule the
        // `.usmap` flattening follows.
        for _ in 0..array_dim.max(1) {
            props.push(UsmapProperty {
                schema_index: props.len() as u16,
                array_dim,
                name: deguid(&name),
                ty: ty.clone(),
            });
        }
    }
    Ok(props)
}

fn try_read_struct_fields_and_script(r: &mut Reader) -> Result<()> {
    r.struct_fields = Some(read_field_chain(r)?);
    let _bytecode_size = r.i32()?;
    let storage = r.i32()?;
    if !(0..=16_000_000).contains(&storage) {
        bail!("implausible ScriptStorageSize {storage}");
    }
    r.take(storage as usize)?;
    Ok(())
}

/// Consume a material's inline shader maps without decoding them.
///
/// `FMaterialResourceProxyReader`'s header ends with a `NumBytes` giving the
/// total size of the resource data that follows, so the whole block can be
/// skipped — the same trick `Texture2D`'s `SkipOffset` allows. Header layout:
/// the shader-map name table (`FString` + two `uint16` hashes each), the
/// `FMaterialResourceLocOnDisk` table (6 bytes each: offset, feature level,
/// quality level), then `NumBytes`.
fn skip_inline_shader_maps(r: &mut Reader) -> Result<()> {
    let resources = r.i32()?;
    if resources <= 0 {
        return Ok(());
    }
    if resources > 1024 {
        bail!("implausible inline shader map resource count {resources}");
    }
    let names = native_count(r, "shader map names")?;
    for _ in 0..names {
        r.fstring()?;
        r.take(4)?; // non-case-preserving + case-preserving hashes
    }
    let locs = native_count(r, "material resource locs")?;
    r.take(locs * 6)?;
    let num_bytes = r.u32()? as usize;
    r.take(num_bytes)?;
    Ok(())
}

/// A `FRawStaticIndexBuffer`: a 32-bit flag, the index bytes as a bulk array,
/// and the "should expand to 32 bit" flag.
fn read_raw_static_index_buffer(r: &mut Reader) -> Result<()> {
    let t = trace_enabled();
    let start = r.o;
    let is32 = r.u32()?;
    let n = read_bulk_array(r, "index buffer")?;
    r.u32()?; // bShouldExpandTo32Bit
    if t {
        eprintln!("      idx @ {start}..{} b32={is32} bytes={n}", r.o);
    }
    Ok(())
}

/// `FStaticMeshLODResources::SerializeBuffers` — the vertex and index buffers.
/// Every payload is a bulk array carrying its own element size, so none of the
/// vertex formats need modeling.
fn read_static_mesh_buffers(r: &mut Reader, sections: usize) -> Result<()> {
    let t = trace_enabled();
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    if t {
        eprintln!("  buffers @ {} strip {global_strip:#x}/{class_strip:#x}", r.o - 2);
    }
    // FPositionVertexBuffer
    r.i32()?; // Stride
    r.i32()?; // NumVertices
    read_bulk_array(r, "positions")?;
    if t { eprintln!("    after positions @ {}", r.o); }
    // FStaticMeshVertexBuffer
    let vb_strip = r.u8()?;
    r.u8()?;
    r.i32()?; // NumTexCoords
    r.i32()?; // NumVertices
    r.u32()?; // bUseFullPrecisionUVs
    r.u32()?; // bUseHighPrecisionTangentBasis
    if vb_strip & 2 == 0 {
        read_bulk_array(r, "tangents")?;
        if t { eprintln!("    after tangents @ {}", r.o); }
        read_bulk_array(r, "UVs")?;
        if t { eprintln!("    after UVs @ {}", r.o); }
    }
    // FColorVertexBuffer
    let cb_strip = r.u8()?;
    r.u8()?;
    r.i32()?; // Stride
    let colour_verts = r.i32()?;
    if cb_strip & 2 == 0 && colour_verts > 0 {
        read_bulk_array(r, "vertex colours")?;
    }
    if t { eprintln!("    after colours @ {}", r.o); }
    read_raw_static_index_buffer(r)?; // IndexBuffer
    if t { eprintln!("    after index buffer @ {}", r.o); }
    // `CDSF_ReversedIndexBuffer` is bit 2 of the class strip flags.
    if class_strip & 4 == 0 {
        read_raw_static_index_buffer(r)?; // ReversedIndexBuffer
    }
    read_raw_static_index_buffer(r)?; // DepthOnlyIndexBuffer
    if class_strip & 4 == 0 {
        read_raw_static_index_buffer(r)?; // ReversedDepthOnlyIndexBuffer
    }
    if global_strip & 1 == 0 {
        read_raw_static_index_buffer(r)?; // WireframeIndexBuffer (editor only)
    }
    // Per UE 5.5.4 `FStaticMeshLODResources::SerializeBuffers`: the ray-tracing
    // geometry's raw data as a bulk array (unless `CDSF_RayTracingResources` is
    // stripped), then one `FStaticMeshSectionAreaWeightedTriangleSampler` per
    // section and one whole-LOD `AreaWeightedSampler`. Each is an
    // `FWeightedRandomSampler` — 12 bytes when empty, which is why a
    // single-section LOD looked like a fixed 24-byte block.
    if class_strip & 8 == 0 {
        read_bulk_array(r, "ray tracing geometry")?;
    }
    for _ in 0..sections {
        read_weighted_random_sampler(r)?;
    }
    read_weighted_random_sampler(r)?;
    Ok(())
}

/// One `FStaticMeshLODResources`.
///
/// The *prologue* is measured on `SM_Basis_HS`: strip flags, one 40-byte
/// `FStaticMeshSection`, `MaxDeviation`, `bIsLODCookedOut`, `bInlined` and
/// `bHasRayTracingGeometry` put `SerializeBuffers`' own strip flags exactly at
/// 0xd8, where the stream reads `05 00 | Stride 12 | NumVertices 148 |
/// bulk(12 × 148)`.
///
/// **NOT verified end to end.** On that same mesh the walk finishes its two LODs
/// at offset 7689, which lands in the middle of float vertex data — so something
/// inside `read_static_mesh_buffers` still drifts. Do not wire `StaticMesh` into
/// the byte-accounting metric until the LOD end lands on `FNaniteResources`
/// (whose first field is a 2-byte `FStripDataFlags`, not floats).
fn read_static_mesh_lod(r: &mut Reader) -> Result<()> {
    let t = trace_enabled();
    let start = r.o;
    let global_strip = r.u8()?;
    let _class_strip = r.u8()?;
    // `FStaticMeshSection`: five int32s then five four-byte flags.
    let sections = native_count(r, "mesh sections")?;
    r.take(sections * 40)?;
    r.f32()?; // MaxDeviation
    let cooked_out = r.u32()? != 0;
    let inlined = r.u32()? != 0;
    if global_strip & 2 == 0 && !cooked_out {
        r.u32()?; // bHasRayTracingGeometry (UE 5.5+)
        if inlined {
            read_static_mesh_buffers(r, sections)?;
        } else {
            r.i32()?; // FByteBulkData index into the package bulk-data map
            r.take(8)?; // DepthOnlyNumTriangles + packed flags
            r.take(72)?; // buffer metadata for each stripped buffer
        }
        // `FStaticMeshBuffersSize` — only for a LOD that actually wrote
        // buffers. A cooked-out LOD ends right after `bInlined`, measured on
        // `SM_UNSC_EscapePods_Exterior_A15_D_Details` where the next LOD's strip
        // flags sit immediately at offset 420.
        r.take(12)?;
    }
    if t {
        eprintln!(
            "LOD @ {start}..{} strip {global_strip:#x} cooked_out {cooked_out} inlined {inlined}",
            r.o
        );
    }
    Ok(())
}

/// `FStaticMeshComponentLODInfo` as written to a cooked, editor-stripped
/// package: strip flags, the map-build-data GUIDs, then the override vertex
/// colour marker.
fn read_static_mesh_component_lod_info(r: &mut Reader) -> Result<()> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    // Bit 1 = audio/visual data stripped.
    if global_strip & 2 == 0 {
        // UE 5.5 cooked: MapBuildDataId then OriginalMapBuildDataId.
        r.take(32)?;
    }
    // Class strip bit 0 = override colours stripped. When they are not, a
    // `uint8 bLoadVertexColorData` says whether an `FColorVertexBuffer`
    // follows: its own strip flags, `Stride` and `NumVertices`, then — only
    // when there are vertices and audio-visual data survived — the colours as
    // a bulk array carrying its own element size.
    if class_strip & 1 == 0 && r.u8()? == 1 {
        let colour_global = r.u8()?;
        r.u8()?; // the colour buffer's own class strip flags
        r.i32()?; // Stride
        let vertices = r.i32()?;
        if vertices > 0 && colour_global & 2 == 0 {
            read_bulk_array(r, "OverrideVertexColors")?;
        }
    }
    Ok(())
}

/// The `UObject` trailer every non-CDO export writes after its property block:
/// a four-byte `hasGuid` and, when set, the 16-byte GUID.
fn read_uobject_trailer(r: &mut Reader, object_flags: u32) -> Result<()> {
    const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
    if object_flags & RF_CLASS_DEFAULT_OBJECT != 0 || r.b.len() < r.o + 4 {
        return Ok(());
    }
    match r.u32()? {
        0 => Ok(()),
        1 => r.take(16).map(|_| ()),
        other => bail!("UObject hasGuid is {other}, not a bool (@ {})", r.o - 4),
    }
}

/// Recover a `UStruct`-derived export's own property layout (in serialization
/// order, `schema_index` assigned) from its cooked bytes, ready to
/// [`Usmap::register_struct`]. `names` is the owning package's name map.
///
/// This walks `UStruct::Serialize` exactly — property block, `UObject`
/// trailer, `SuperStruct`, `ChildArray`, then `SerializeProperties`. An
/// earlier version probed a few word offsets for the field count instead;
/// that silently accepted a wrong reading whenever the real parse failed,
/// which is precisely how three `FProperty` layout bugs stayed hidden.
pub fn read_userdefined_struct_layout(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<Vec<UsmapProperty>> {
    let mut r = Reader::with_ctx(export, names, ctx);
    read_struct(&mut r, "UserDefinedStruct", usmap, 0).context("UserDefinedStruct property block")?;
    read_uobject_trailer(&mut r, object_flags)?;
    r.i32()?; // SuperStruct
    let children = native_count(&mut r, "ChildArray")?;
    r.take(children * 4)?;
    read_field_chain(&mut r)
}

/// Decode a cooked `UDataTable`'s rows into `(row key, field→value)` pairs.
/// `row_struct` must already be registered in `usmap` (see
/// [`read_userdefined_struct_layout`] + [`Usmap::register_struct`]).
pub fn read_datatable(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    row_struct: &str,
    object_flags: u32,
) -> Result<Vec<(String, BTreeMap<String, PropValue>)>> {
    let mut r = Reader::new(export, names);
    // The DataTable's own reflected block (RowStruct ref, import flags, …).
    read_struct(&mut r, "DataTable", usmap, 0).context("DataTable header block")?;
    read_uobject_trailer(&mut r, object_flags)?;
    let flat = flattened_schema(row_struct, usmap)?;
    let num = native_count(&mut r, "DataTable rows")?;
    let mut rows = Vec::with_capacity(num);
    for i in 0..num {
        let key = r.name()?;
        let row = read_struct_with_schema(&mut r, row_struct, &flat, usmap, 0)
            .with_context(|| format!("row {i} ({key})"))?;
        rows.push((key, row));
    }
    Ok(rows)
}

/// Turn a `TArray<FBlamMeshSynchronizationRuntime{Skeletal,Static}Mesh>` value
/// into flat [`MeshRef`]s, dropping entries with an empty asset path.
fn collect_meshes(value: &PropValue) -> Vec<MeshRef> {
    let Some(items) = value.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in items {
        let Some(s) = item.as_struct() else { continue };
        let asset = s.get("Asset").and_then(PropValue::as_soft_object);
        let Some(asset) = asset else { continue };
        if asset.is_empty() {
            continue;
        }
        let class = s
            .get("Class")
            .and_then(PropValue::as_soft_object)
            .map(|c| c.asset.clone())
            .unwrap_or_default();
        let parent_bone = s
            .get("ParentBoneName")
            .and_then(PropValue::as_str)
            .unwrap_or_default()
            .to_string();
        let rel_transform = s
            .get("Transform")
            .and_then(MeshTransform::from_prop)
            .unwrap_or_default();
        let material_overrides = s
            .get("MaterialOverrides")
            .and_then(PropValue::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let os = o.as_struct()?;
                        let slot = os.get("MaterialSlotName").and_then(PropValue::as_str)?;
                        let mi = os.get("OverrideMaterial").and_then(PropValue::as_soft_object)?;
                        if mi.is_empty() {
                            return None;
                        }
                        let name = if !mi.asset.is_empty() {
                            mi.asset.clone()
                        } else {
                            mi.package.rsplit('/').next().unwrap_or(&mi.package).to_string()
                        };
                        Some((slot.to_string(), name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(MeshRef {
            package: asset.package.clone(),
            asset: asset.asset.clone(),
            class,
            parent_bone,
            rel_transform,
            material_overrides,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::usmap::UsmapProperty;

    /// Build a two-property schema — a `TSet` followed by an `int32` — and
    /// decode a hand-written stream through it.
    ///
    /// A `TSet` serializes like a `TMap`: `NumElementsToRemove` *then* `Num`.
    /// Reading it as a bare `TArray` consumes only one of the two, so the
    /// trailing `int32` is read four bytes early. That failure is silent —
    /// the shifted bytes still decode as a perfectly plausible integer — which
    /// is why it needs a test rather than an eyeball.
    #[test]
    fn set_consumes_its_remove_count_so_later_properties_stay_aligned() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SetAlignmentProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "Tags".to_string(),
                    ty: PropertyType::Set(Box::new(PropertyType::Int)),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "Value".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        // One fragment: skip 0, two values present, none zero, is-last.
        // (ValueNum occupies bits 9+, the is-last flag is bit 8.)
        let fragment: u16 = (2 << 9) | 0x0100;
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&0i32.to_le_bytes()); // NumElementsToRemove
        export.extend_from_slice(&0i32.to_le_bytes()); // Num (empty set)
        export.extend_from_slice(&0x1122_3344i32.to_le_bytes()); // Value

        let props = read_export_struct(&export, &[], &usmap, "SetAlignmentProbe")
            .expect("decode probe struct");

        assert!(matches!(props.get("Tags"), Some(PropValue::Array(v)) if v.is_empty()));
        assert!(
            matches!(props.get("Value"), Some(PropValue::Int(0x1122_3344))),
            "int after a set was read from the wrong offset: {:?}",
            props.get("Value")
        );
    }

    /// `FPerPlatformFloat` writes `bool bCooked` before its `Default`, and
    /// `FArchive` writes a bool as **four** bytes — so the cooked struct is
    /// eight bytes, not four.
    ///
    /// Reading it as a bare `float` leaves the stream four bytes short, and the
    /// property after it still decodes as a perfectly plausible number, so the
    /// damage only surfaces much later (or never). This single size was what
    /// blocked `SkeletalMesh` and `StaticMesh` entirely: every LOD's
    /// `ScreenSize` is a `PerPlatformFloat`.
    #[test]
    fn per_platform_float_consumes_its_cooked_flag() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "PerPlatformAlignmentProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "ScreenSize".to_string(),
                    ty: PropertyType::Struct("PerPlatformFloat".to_string()),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "LODHysteresis".to_string(),
                    ty: PropertyType::Float,
                },
            ],
        );

        let mut export = Vec::new();
        let fragment: u16 = (2 << 9) | 0x0100; // skip 0, two values, is-last
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&1i32.to_le_bytes()); // bCooked
        export.extend_from_slice(&1.0f32.to_le_bytes()); // Default
        export.extend_from_slice(&0.02f32.to_le_bytes()); // LODHysteresis

        let (props, used) =
            read_export_struct_len(&export, &[], &usmap, "PerPlatformAlignmentProbe")
                .expect("decode probe struct");

        assert_eq!(used, export.len(), "walk did not consume the whole block");
        assert!(
            matches!(props.get("LODHysteresis"), Some(PropValue::Float(v)) if (*v - 0.02).abs() < 1e-6),
            "float after a PerPlatformFloat was misaligned: {:?}",
            props.get("LODHysteresis")
        );
    }

    /// A non-empty set must still leave the stream aligned.
    #[test]
    fn non_empty_set_stays_aligned() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SetAlignmentProbe2",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "Tags".to_string(),
                    ty: PropertyType::Set(Box::new(PropertyType::Int)),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "Value".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        let fragment: u16 = (2 << 9) | 0x0100;
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&0i32.to_le_bytes()); // NumElementsToRemove
        export.extend_from_slice(&2i32.to_le_bytes()); // Num
        export.extend_from_slice(&7i32.to_le_bytes());
        export.extend_from_slice(&9i32.to_le_bytes());
        export.extend_from_slice(&0x0BAD_F00Di32.to_le_bytes());

        let props = read_export_struct(&export, &[], &usmap, "SetAlignmentProbe2")
            .expect("decode probe struct");

        match props.get("Tags") {
            Some(PropValue::Array(v)) => {
                let ints: Vec<i64> = v
                    .iter()
                    .map(|e| match e {
                        PropValue::Int(n) => *n,
                        other => panic!("expected ints in the set, got {other:?}"),
                    })
                    .collect();
                assert_eq!(ints, vec![7, 9]);
            }
            other => panic!("expected a 2-element set, got {other:?}"),
        }
        assert!(
            matches!(props.get("Value"), Some(PropValue::Int(0x0BAD_F00D))),
            "int after a non-empty set was misaligned: {:?}",
            props.get("Value")
        );
    }
}
