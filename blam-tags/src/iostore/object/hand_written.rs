//! Structs whose `Serialize` lives in engine code rather than in a schema, as
//! typed values.
//!
//! These are the 23 shapes [`super::structs::read_native_variable_struct`]
//! decodes by hand. Until now each produced a `PropertyBlock` of loose fields
//! carrying a `BlockLayout::Native` span, and the span — not the fields — was
//! what got written. That makes the fields a *view* and the bytes the truth,
//! which is the arrangement this work exists to remove: a caller that edits a
//! field should see the edit come out the other side.
//!
//! Each variant here is read into typed fields and written back **from** them.
//! `ce_semantic_roundtrip` is what holds them to it, and while both mechanisms
//! coexist the retained-span path is still there for the shapes not yet
//! converted — `BlockLayout::Native` is deleted when the last one lands.

use anyhow::Result;

use super::archive::{Ar, Reader};
use super::block::{flattened_schema, read_struct, write_block};
use super::usmap::{PropertyType, Usmap, UsmapProperty};
use super::value::{FName, FStr, PropertyBlock};

/// A hand-written struct, decoded.
#[derive(Debug, Clone)]
pub enum HandWritten {
    /// `FNiagaraVariableBase` and the three shapes that extend it. 1.86M of the
    /// 1.92M hand-written spans in the corpus.
    NiagaraVariable(NiagaraVariable),
    /// `FMovieSceneFloatChannel` / `FMovieSceneDoubleChannel` — the same shape
    /// at two widths, which is what `value_size` distinguishes.
    MovieSceneChannel(MovieSceneChannel),
    /// `FPCGPoint` — a byte mask saying which fields were written.
    PcgPoint(PcgPoint),
    /// `FSkeletalMeshSamplingLODBuiltData` — one area-weighted sampler.
    SkeletalMeshSamplingLod(WeightedRandomSampler),
    /// `FSkeletalMeshSamplingRegionBuiltData`.
    SkeletalMeshSamplingRegion(SkeletalMeshSamplingRegion),
    /// `FNiagaraDataInterfaceGPUParamInfo`.
    NiagaraGpuParamInfo(NiagaraGpuParamInfo),
    /// `FText` — the only genuinely polymorphic shape in the set.
    Text(TextValue),
    /// The MovieScene "inline value" pointers, which name their concrete type
    /// and then write it as an ordinary reflected block.
    MovieSceneInlineValue(MovieSceneInlineValue),
    /// `TMovieSceneEvaluationTree<T>`.
    EvaluationTree(EvaluationTree),
    /// `FShaderValueTypeHandle` — recursive.
    ShaderValueType(ShaderValueType),
    /// `FPerQualityLevelInt` / `FPerQualityLevelFloat`.
    PerQualityLevel(PerQualityLevel),
    /// `FFontData`.
    FontData(FontData),
    /// `FMaterialOverrideNanite`.
    MaterialOverrideNanite(MaterialOverrideNanite),
    /// `FMovieSceneTimeWarpVariant`.
    TimeWarpVariant(TimeWarpVariant),
    /// `FUniversalObjectLocatorFragment`.
    LocatorFragment(LocatorFragment),
    /// `FInstancedPropertyBag` — a struct type invented at runtime.
    InstancedPropertyBag(InstancedPropertyBag),
    /// `FMaterialLayersFunctionsTree`.
    MaterialLayersTree(MaterialLayersTree),
}

/// `FInstancedPropertyBag`: a struct type invented at runtime.
///
/// The payload is **not** a `TArray<uint8>`. `Serialize` (PropertyBag.cpp:2295)
/// reads the descriptors, builds a `UPropertyBag` from them with
/// `GetOrCreateFromDescs`, and then calls `SerializeItem` on that struct — so
/// the bytes are an ordinary unversioned property block against a schema the
/// file carries with it. `SerialSize` exists so a loader that *cannot* build the
/// struct can skip it, which is the only reason it looks skippable.
// No `PartialEq`: a `PropertyBlock` is compared with `semantic_eq`.
#[derive(Debug, Clone)]
pub struct InstancedPropertyBag {
    /// Absent when the bag was written empty.
    pub descriptors: Option<Vec<PropertyBagDesc>>,
    /// The size the file declares for the block below. Kept because it is what
    /// a loader without the schema seeks past, and it is not derivable from the
    /// decoded values.
    pub serial_size: i32,
    /// The values, decoded against the schema the descriptors describe.
    pub values: Option<PropertyBlock>,
}

/// Build the schema a `UPropertyBag` would have been created with.
///
/// `UPropertyBag::GetOrCreateFromDescs` turns the descriptors into a real
/// `UStruct` whose properties are in descriptor order, so the block that follows
/// indexes them exactly like any other schema.
/// `resolver` names the `Struct` and `Enum` descriptors' types: a descriptor
/// carries a `ValueTypeObject` reference, not a name, so the schema cannot be
/// built without the package context — the same dependency `UDataTable`'s row
/// struct has.
pub fn property_bag_schema(
    descs: &[PropertyBagDesc],
    resolver: Option<&dyn super::archive::PackageResolver>,
) -> Vec<UsmapProperty> {
    descs
        .iter()
        .enumerate()
        .map(|(i, d)| UsmapProperty {
            name: d.name.as_str().to_string(),
            schema_index: i as u16,
            array_dim: 1,
            ty: property_bag_type(d, resolver),
        })
        .collect()
}

/// `EPropertyBagPropertyType` (PropertyBag.h:13) wrapped by its container types,
/// innermost last — a descriptor lists `Array`/`Set` outermost first.
fn property_bag_type(
    d: &PropertyBagDesc,
    resolver: Option<&dyn super::archive::PackageResolver>,
) -> PropertyType {
    // A `Struct` or `Enum` descriptor names its type by object reference.
    let named = || {
        resolver.and_then(|p| p.struct_name(d.value_type_object)).unwrap_or_default()
    };
    let mut ty = match d.value_type {
        1 => PropertyType::Bool,
        2 => PropertyType::Byte { enum_name: Option::None },
        3 => PropertyType::Int,
        4 => PropertyType::Int64,
        5 => PropertyType::Float,
        6 => PropertyType::Double,
        7 => PropertyType::Name,
        8 => PropertyType::Str,
        9 => PropertyType::Text,
        10 => PropertyType::Enum {
            inner: Box::new(PropertyType::Byte { enum_name: Option::None }),
            enum_name: named(),
        },
        11 => PropertyType::Struct(named()),
        12 | 14 => PropertyType::Object,
        13 | 15 => PropertyType::SoftObject,
        16 => PropertyType::UInt32,
        17 => PropertyType::UInt64,
        // `None`, and anything a later engine adds.
        other => PropertyType::Unknown(other),
    };
    for c in d.container_types.iter().rev() {
        ty = match c {
            1 => PropertyType::Array(Box::new(ty)),
            2 => PropertyType::Set(Box::new(ty)),
            _ => ty,
        };
    }
    ty
}

/// `FPropertyBagPropertyDesc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyBagDesc {
    pub value_type_object: i32,
    pub id: [u8; 16],
    pub name: FName,
    pub value_type: u8,
    /// The nested container types, one byte each.
    pub container_types: Vec<u8>,
}

/// `FMaterialLayersFunctionsTree` — a node array, a payload array, then the
/// root index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialLayersTree {
    /// Four int32 ids each.
    pub nodes: Vec<[i32; 4]>,
    /// Layer and blend.
    pub payloads: Vec<[i32; 2]>,
    pub root: i32,
}

/// `FMovieSceneEvalTemplatePtr` and its two siblings: a type name, then that
/// type's own property block. An empty name means no payload.
#[derive(Debug, Clone)]
pub struct MovieSceneInlineValue {
    pub type_name: FStr,
    pub payload: Option<PropertyBlock>,
}

/// `FEvaluationTreeEntryHandle` / the entry records — start, size, capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeEntry {
    pub start: i32,
    pub size: i32,
    pub capacity: i32,
}

/// `FMovieSceneEvaluationTreeNode` — 26 bytes, and the arithmetic settles it:
/// `Range` (a `TRange<FFrameNumber>`, 10) + `Parent` (a node handle, two
/// int32s) + `ChildrenID` + `DataID` (an entry handle each). Confirmed against
/// its `operator<<` in MovieSceneEvaluationTree.h.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TreeNode {
    pub range_lower_kind: u8,
    pub range_lower: i32,
    pub range_upper_kind: u8,
    pub range_upper: i32,
    pub parent_children_handle: i32,
    pub parent_index: i32,
    pub children_id: i32,
    pub data_id: i32,
}

/// A `TMovieSceneEvaluationTree<T>`: a root node, then two entry containers —
/// the child nodes and the payload items.
#[derive(Debug, Clone)]
pub struct EvaluationTree {
    pub root: TreeNode,
    pub child_entries: Vec<TreeEntry>,
    pub child_nodes: Vec<TreeNode>,
    pub data_entries: Vec<TreeEntry>,
    pub items: Vec<TreeItem>,
}

/// The payload type differs per tree, and the struct name is what says which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeItem {
    /// `FEntityAndMetaDataIndex` — two int32s.
    EntityAndMetaDataIndex { entity: i32, meta_data: i32 },
    /// `FMovieSceneSubSequenceTreeEntry` — a sequence id and a one-byte flags
    /// enum. The warp counter older streams carried is gone in 5.5.
    SubSequence { sequence_id: u32, flags: u8 },
}

/// `FShaderValueType`, reached through its handle. Recursive: a struct type
/// names itself and its elements, each of which is another value type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShaderValueType {
    pub kind: u8,
    pub is_dynamic_array: bool,
    pub body: ShaderValueTypeBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShaderValueTypeBody {
    /// `kind == 4`.
    Struct { name: FName, elements: Vec<(FName, ShaderValueType)> },
    /// Anything else: a dimension type and the counts it implies, all `uint8`.
    Dimension { dimension: u8, counts: Vec<u8> },
}

/// `FPerQualityLevelInt` / `FPerQualityLevelFloat`. Unlike its `FPerPlatform*`
/// sibling the override map is **not** behind the cooked check, so it is always
/// written — cooking merely strips it to empty.
#[derive(Debug, Clone)]
pub struct PerQualityLevel {
    pub cooked: bool,
    /// The bits, so the int and float forms share one field without rounding.
    pub default_bits: i32,
    pub overrides: Vec<(i32, i32)>,
}

/// `FFontData`.
#[derive(Debug, Clone)]
pub struct FontData {
    pub font_face_asset: i32,
    /// Present only when there is no face asset.
    pub inline_face: Option<InlineFontFace>,
    pub sub_face_index: i32,
}

#[derive(Debug, Clone)]
pub struct InlineFontFace {
    pub filename: FStr,
    pub hinting: u8,
    pub loading_policy: u8,
}

/// `FMaterialOverrideNanite`.
#[derive(Debug, Clone)]
pub struct MaterialOverrideNanite {
    pub cooked: bool,
    pub override_material: Option<i32>,
    /// Its own reflected properties follow the native prefix.
    pub properties: PropertyBlock,
}

/// `FMovieSceneTimeWarpVariant`, through `FMovieSceneNumericVariant`.
#[derive(Debug, Clone)]
pub enum TimeWarpVariant {
    /// A NaN-boxed double.
    Literal(f64),
    /// A `uint8 EMovieSceneTimeWarpType` and that type's payload: `Custom`
    /// writes an object reference, `FixedPlayRate` nothing at all, and the rest
    /// an ordinary block for their own small struct.
    Typed { kind: u8, object: Option<i32>, payload: Option<PropertyBlock> },
}

/// `FUniversalObjectLocatorFragment` — polymorphic on a registered `FName`.
#[derive(Debug, Clone)]
pub struct LocatorFragment {
    pub fragment_type: FName,
    pub payload: Option<PropertyBlock>,
}

/// `FText`: flags, then a history whose type byte decides everything after it.
#[derive(Debug, Clone)]
pub struct TextValue {
    pub flags: u32,
    pub history: TextHistory,
}

/// `ETextHistoryType`. The discriminant is kept on the variants that share a
/// body, because it is what the writer emits and the shapes are not otherwise
/// distinguishable.
#[derive(Debug, Clone)]
pub enum TextHistory {
    /// `None` (-1). Still writes a four-byte `bHasCultureInvariantString`, and
    /// the string itself when set.
    None { culture_invariant: Option<FStr> },
    /// `Base` (0).
    Base { namespace: FStr, key: FStr, source: FStr },
    /// `StringTableEntry` (11).
    StringTableEntry { table_id: FName, key: FStr },
    /// `OrderedFormat` (2) — **positional** arguments, with none of the names
    /// `ArgumentDataFormat` carries.
    OrderedFormat { source_fmt: Box<TextValue>, arguments: Vec<TextFormatArgument> },
    /// `NamedFormat` (1) and `ArgumentDataFormat` (3): both a count followed by
    /// name/value pairs. Only the latter appears in Campaign Evolved.
    NamedFormat {
        kind: i8,
        source_fmt: Box<TextValue>,
        arguments: Vec<(FStr, TextFormatArgument)>,
    },
    /// `AsNumber` (4), `AsPercent` (5), `AsCurrency` (6).
    AsNumber {
        kind: i8,
        /// `AsCurrency` leads with the currency code.
        currency_code: Option<FStr>,
        source_value: TextFormatArgument,
        options: Option<NumberFormattingOptions>,
        target_culture: FStr,
    },
}

/// `FFormatArgumentValue`.
///
/// The type byte is part of the value, not a detail of parsing it: `Int` and
/// `UInt` are both eight bytes on the wire and `Float` and `Double` differ only
/// in width, so collapsing them — as the untyped reader did — loses the
/// information needed to write the argument back.
#[derive(Debug, Clone)]
pub enum TextFormatArgument {
    Int(i64),
    UInt(u64),
    Float(f32),
    Double(f64),
    Text(Box<TextValue>),
}

/// `FNumberFormattingOptions` — three `FArchive` bools, a rounding mode, then
/// four digit counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumberFormattingOptions {
    pub always_sign: bool,
    pub use_grouping: bool,
    pub rounding_mode: u8,
    pub minimum_integral_digits: i32,
    pub maximum_integral_digits: i32,
    pub minimum_fractional_digits: i32,
    pub maximum_fractional_digits: i32,
}

/// `FNiagaraDataInterfaceGPUParamInfo` — the HLSL symbol, the data-interface
/// class name, and the generated-function table.
///
/// There is no `ShaderParametersOffset` in the stream despite the `.usmap`
/// listing one.
#[derive(Debug, Clone)]
pub struct NiagaraGpuParamInfo {
    pub hlsl_symbol: FStr,
    pub di_class_name: FStr,
    pub generated_functions: Vec<NiagaraGeneratedFunction>,
}

/// One entry of `FNiagaraDataInterfaceGPUParamInfo::GeneratedFunctions`.
#[derive(Debug, Clone)]
pub struct NiagaraGeneratedFunction {
    pub definition_name: FName,
    pub instance_name: FStr,
    /// Name/value pairs.
    pub specifiers: Vec<(FName, FName)>,
    pub variadic_inputs: Vec<NiagaraVariableCommonReference>,
    pub variadic_outputs: Vec<NiagaraVariableCommonReference>,
    // No trailing `MiscUsageBitMask`: that field is gated on a later Niagara
    // custom version than this build writes.
}

/// `FNiagaraVariableCommonReference` — a name and an `FPackageIndex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NiagaraVariableCommonReference {
    pub name: FName,
    pub underlying_type: i32,
}

/// `FMovieSceneFloatChannel` and its double-width twin. The key times and
/// values are bulk arrays carrying their own element size.
#[derive(Debug, Clone)]
pub struct MovieSceneChannel {
    pub pre_infinity_extrap: u8,
    pub post_infinity_extrap: u8,
    /// Key times — a bulk array, element size included.
    pub times: BulkArray,
    /// Key values, likewise.
    pub values: BulkArray,
    /// f32 for the float channel, f64 for the double one.
    pub default_value: f64,
    pub has_default_value: bool,
    pub tick_resolution_numerator: i32,
    pub tick_resolution_denominator: i32,
    pub show_curve: bool,
}

/// A `TArray` written with `BulkSerialize`: element size, count, then the
/// elements back to back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkArray {
    pub element_size: i32,
    pub data: Vec<u8>,
}

/// `FPCGPoint`. Every field after the transform is optional, and the leading
/// mask says which are present — so a missing field and a zero field are
/// different things on the wire.
#[derive(Debug, Clone)]
pub struct PcgPoint {
    /// Inside a hand-written serializer an `FTransform` is written raw — an
    /// `FQuat` then translation and scale, 80 bytes — unlike an `FTransform`
    /// *property*, which goes through the unversioned schema.
    pub transform: [f64; 10],
    pub density: Option<f32>,
    pub bounds_min: Option<[f64; 3]>,
    pub bounds_max: Option<[f64; 3]>,
    pub color: Option<[f64; 4]>,
    pub steepness: Option<f32>,
    pub seed: Option<i32>,
    pub metadata_entry: Option<u64>,
}

/// `FWeightedRandomSampler` — parallel probability and alias tables, then the
/// total weight.
#[derive(Debug, Clone)]
pub struct WeightedRandomSampler {
    pub prob: Vec<f32>,
    pub alias: Vec<i32>,
    pub total_weight: f32,
}

/// `FSkeletalMeshSamplingRegionBuiltData`.
#[derive(Debug, Clone)]
pub struct SkeletalMeshSamplingRegion {
    pub triangle_indices: Vec<i32>,
    pub bone_indices: Vec<i32>,
    pub sampler: WeightedRandomSampler,
    /// Written *after* the sampler rather than in declaration order, and gated
    /// on `FNiagaraObjectVersion::SkeletalMeshVertexSampling`.
    pub vertices: Vec<i32>,
}

/// `FNiagaraVariableBase` — a `Name` and a `TypeDefHandle`, the handle
/// serializing an `FNiagaraTypeDefinition` **by value**, which has no serializer
/// of its own and so lands as an ordinary unversioned property block.
#[derive(Debug, Clone)]
pub struct NiagaraVariable {
    pub name: FName,
    /// `FNiagaraTypeDefinition`, a reflected block.
    pub type_def: PropertyBlock,
    pub payload: NiagaraPayload,
}

/// What each of the four subclasses appends after the base.
#[derive(Debug, Clone)]
pub enum NiagaraPayload {
    /// `FNiagaraVariableBase`, `FNiagaraDataChannelVariable` — nothing.
    None,
    /// `FNiagaraVariableWithOffset` — a stride into `ParameterData`.
    Offset(i32),
    /// `FNiagaraVariable` — an inline `TArray<uint8>` payload.
    VarData(Vec<u8>),
}

/// The struct names this module models. Anything not listed still takes the
/// retained-span path in [`super::structs`].
pub const MODELED: &[&str] = &[
    "NiagaraVariableBase",
    "NiagaraVariable",
    "NiagaraVariableWithOffset",
    "NiagaraDataChannelVariable",
    "MovieSceneFloatChannel",
    "MovieSceneDoubleChannel",
    "PCGPoint",
    "SkeletalMeshSamplingLODBuiltData",
    "SkeletalMeshSamplingRegionBuiltData",
    "NiagaraDataInterfaceGPUParamInfo",
    "Text",
    "MovieSceneEvalTemplatePtr",
    "MovieSceneTrackImplementationPtr",
    "MovieSceneSequenceInstanceDataPtr",
    "MovieSceneEvaluationFieldEntityTree",
    "MovieSceneSubSequenceTree",
    "ShaderValueTypeHandle",
    "PerQualityLevelInt",
    "PerQualityLevelFloat",
    "FontData",
    "MaterialOverrideNanite",
    "MovieSceneTimeWarpVariant",
    "UniversalObjectLocatorFragment",
    "InstancedPropertyBag",
    "MaterialLayersFunctionsTree",
];

impl TreeNode {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(TreeNode {
            range_lower_kind: r.u8()?,
            range_lower: r.i32()?,
            range_upper_kind: r.u8()?,
            range_upper: r.i32()?,
            parent_children_handle: r.i32()?,
            parent_index: r.i32()?,
            children_id: r.i32()?,
            data_id: r.i32()?,
        })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.range_lower_kind.to_owned())?;
        ar.i32(&mut self.range_lower.to_owned())?;
        ar.u8(&mut self.range_upper_kind.to_owned())?;
        ar.i32(&mut self.range_upper.to_owned())?;
        ar.i32(&mut self.parent_children_handle.to_owned())?;
        ar.i32(&mut self.parent_index.to_owned())?;
        ar.i32(&mut self.children_id.to_owned())?;
        ar.i32(&mut self.data_id.to_owned())
    }
}

impl TreeEntry {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(TreeEntry { start: r.i32()?, size: r.i32()?, capacity: r.i32()? })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.start.to_owned())?;
        ar.i32(&mut self.size.to_owned())?;
        ar.i32(&mut self.capacity.to_owned())
    }
}

impl ShaderValueType {
    fn read(r: &mut Reader, depth: usize) -> Result<Self> {
        if depth > super::limits::MAX_DEPTH {
            anyhow::bail!("shader value type nested too deep");
        }
        const STRUCT: u8 = 4;
        let kind = r.u8()?;
        let is_dynamic_array = r.u32()? != 0;
        let body = if kind == STRUCT {
            let name = r.fname()?;
            let n = count(r, "shader value type struct elements")?;
            let mut elements = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
            for _ in 0..n {
                elements.push((r.fname()?, ShaderValueType::read(r, depth + 1)?));
            }
            ShaderValueTypeBody::Struct { name, elements }
        } else {
            // EShaderFundamentalDimensionType: Scalar 0, Vector 1, Matrix 2 —
            // each implying how many `uint8` counts follow.
            let dimension = r.u8()?;
            let n = match dimension {
                1 => 1,
                2 => 2,
                _ => 0,
            };
            let mut counts = Vec::with_capacity(n);
            for _ in 0..n {
                counts.push(r.u8()?);
            }
            ShaderValueTypeBody::Dimension { dimension, counts }
        };
        Ok(ShaderValueType { kind, is_dynamic_array, body })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.kind.to_owned())?;
        ar.u32(&mut (self.is_dynamic_array as u32))?;
        match &self.body {
            ShaderValueTypeBody::Struct { name, elements } => {
                ar.fname(&mut name.clone())?;
                ar.i32(&mut (elements.len() as i32))?;
                for (n, t) in elements {
                    ar.fname(&mut n.clone())?;
                    t.write(ar)?;
                }
            }
            ShaderValueTypeBody::Dimension { dimension, counts } => {
                ar.u8(&mut dimension.to_owned())?;
                for c in counts {
                    ar.u8(&mut c.to_owned())?;
                }
            }
        }
        Ok(())
    }
}

impl TextFormatArgument {
    fn read(r: &mut Reader, depth: usize) -> Result<Self> {
        let ty = r.u8()? as i8;
        Ok(match ty {
            0 => TextFormatArgument::Int(r.u64()? as i64),
            1 => TextFormatArgument::UInt(r.u64()?),
            2 => TextFormatArgument::Float(r.f32()?),
            3 => TextFormatArgument::Double(r.f64()?),
            4 => TextFormatArgument::Text(Box::new(TextValue::read(r, depth + 1)?)),
            other => anyhow::bail!("FText format argument type {other} not modeled (@ {})", r.o - 1),
        })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        match self {
            TextFormatArgument::Int(v) => {
                ar.u8(&mut 0)?;
                ar.u64(&mut (*v as u64))
            }
            TextFormatArgument::UInt(v) => {
                ar.u8(&mut 1)?;
                ar.u64(&mut v.to_owned())
            }
            TextFormatArgument::Float(v) => {
                ar.u8(&mut 2)?;
                ar.f32(&mut v.to_owned())
            }
            TextFormatArgument::Double(v) => {
                ar.u8(&mut 3)?;
                ar.f64(&mut v.to_owned())
            }
            TextFormatArgument::Text(t) => {
                ar.u8(&mut 4)?;
                t.write(ar)
            }
        }
    }
    fn semantic_eq(&self, o: &TextFormatArgument) -> bool {
        use TextFormatArgument::*;
        match (self, o) {
            (Int(a), Int(b)) => a == b,
            (UInt(a), UInt(b)) => a == b,
            (Float(a), Float(b)) => a.to_bits() == b.to_bits(),
            (Double(a), Double(b)) => a.to_bits() == b.to_bits(),
            (Text(a), Text(b)) => a.semantic_eq(b),
            _ => false,
        }
    }
}

impl TextValue {
    pub(super) fn read(r: &mut Reader, depth: usize) -> Result<Self> {
        if depth > 16 {
            anyhow::bail!("FText nesting too deep @ {}", r.o);
        }
        let flags = r.u32()?;
        let kind = r.u8()? as i8;
        let history = match kind {
            -1 => TextHistory::None {
                culture_invariant: if r.u32()? != 0 { Some(r.fstring()?) } else { None },
            },
            0 => TextHistory::Base {
                namespace: r.fstring()?,
                key: r.fstring()?,
                source: r.fstring()?,
            },
            11 => TextHistory::StringTableEntry { table_id: r.fname()?, key: r.fstring()? },
            2 => {
                let source_fmt = Box::new(TextValue::read(r, depth + 1)?);
                let n = count(r, "FText ordered arguments")?;
                let mut arguments = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    arguments.push(TextFormatArgument::read(r, depth + 1)?);
                }
                TextHistory::OrderedFormat { source_fmt, arguments }
            }
            1 | 3 => {
                let source_fmt = Box::new(TextValue::read(r, depth + 1)?);
                let n = count(r, "FText arguments")?;
                let mut arguments = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    arguments.push((r.fstring()?, TextFormatArgument::read(r, depth + 1)?));
                }
                TextHistory::NamedFormat { kind, source_fmt, arguments }
            }
            4 | 5 | 6 => {
                let currency_code = if kind == 6 { Some(r.fstring()?) } else { None };
                let source_value = TextFormatArgument::read(r, depth + 1)?;
                let options = if r.u32()? != 0 {
                    Some(NumberFormattingOptions {
                        always_sign: r.u32()? != 0,
                        use_grouping: r.u32()? != 0,
                        rounding_mode: r.u8()?,
                        minimum_integral_digits: r.i32()?,
                        maximum_integral_digits: r.i32()?,
                        minimum_fractional_digits: r.i32()?,
                        maximum_fractional_digits: r.i32()?,
                    })
                } else {
                    None
                };
                TextHistory::AsNumber {
                    kind,
                    currency_code,
                    source_value,
                    options,
                    target_culture: r.fstring()?,
                }
            }
            other => anyhow::bail!("FText history type {other} not modeled (@ {})", r.o - 1),
        };
        Ok(TextValue { flags, history })
    }

    pub(super) fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.flags.to_owned())?;
        match &self.history {
            TextHistory::None { culture_invariant } => {
                ar.u8(&mut (-1i8 as u8))?;
                match culture_invariant {
                    Some(s) => {
                        ar.u32(&mut 1)?;
                        ar.fstring(&mut s.clone())?;
                    }
                    None => ar.u32(&mut 0)?,
                }
            }
            TextHistory::Base { namespace, key, source } => {
                ar.u8(&mut 0)?;
                ar.fstring(&mut namespace.clone())?;
                ar.fstring(&mut key.clone())?;
                ar.fstring(&mut source.clone())?;
            }
            TextHistory::StringTableEntry { table_id, key } => {
                ar.u8(&mut 11)?;
                ar.fname(&mut table_id.clone())?;
                ar.fstring(&mut key.clone())?;
            }
            TextHistory::OrderedFormat { source_fmt, arguments } => {
                ar.u8(&mut 2)?;
                source_fmt.write(ar)?;
                ar.i32(&mut (arguments.len() as i32))?;
                for a in arguments {
                    a.write(ar)?;
                }
            }
            TextHistory::NamedFormat { kind, source_fmt, arguments } => {
                ar.u8(&mut (*kind as u8))?;
                source_fmt.write(ar)?;
                ar.i32(&mut (arguments.len() as i32))?;
                for (name, a) in arguments {
                    ar.fstring(&mut name.clone())?;
                    a.write(ar)?;
                }
            }
            TextHistory::AsNumber {
                kind,
                currency_code,
                source_value,
                options,
                target_culture,
            } => {
                ar.u8(&mut (*kind as u8))?;
                // Only `AsCurrency` carries one, so the pairing is checked
                // rather than assumed.
                match (currency_code, *kind == 6) {
                    (Some(c), true) => ar.fstring(&mut c.clone())?,
                    (None, false) => {}
                    _ => anyhow::bail!("currency code does not match FText history type {kind}"),
                }
                source_value.write(ar)?;
                match options {
                    Some(o) => {
                        ar.u32(&mut 1)?;
                        ar.u32(&mut (o.always_sign as u32))?;
                        ar.u32(&mut (o.use_grouping as u32))?;
                        ar.u8(&mut o.rounding_mode.to_owned())?;
                        for v in [
                            o.minimum_integral_digits,
                            o.maximum_integral_digits,
                            o.minimum_fractional_digits,
                            o.maximum_fractional_digits,
                        ] {
                            ar.i32(&mut v.to_owned())?;
                        }
                    }
                    None => ar.u32(&mut 0)?,
                }
                ar.fstring(&mut target_culture.clone())?;
            }
        }
        Ok(())
    }

    fn semantic_eq(&self, o: &TextValue) -> bool {
        use TextHistory::*;
        if self.flags != o.flags {
            return false;
        }
        let str_eq = |a: &FStr, b: &FStr| a == b && a.wide == b.wide;
        match (&self.history, &o.history) {
            (None { culture_invariant: a }, None { culture_invariant: b }) => match (a, b) {
                (Some(x), Some(y)) => str_eq(x, y),
                (Option::None, Option::None) => true,
                _ => false,
            },
            (Base { namespace: a1, key: a2, source: a3 }, Base { namespace: b1, key: b2, source: b3 }) => {
                str_eq(a1, b1) && str_eq(a2, b2) && str_eq(a3, b3)
            }
            (StringTableEntry { table_id: a1, key: a2 }, StringTableEntry { table_id: b1, key: b2 }) => {
                a1 == b1 && str_eq(a2, b2)
            }
            (OrderedFormat { source_fmt: a1, arguments: a2 }, OrderedFormat { source_fmt: b1, arguments: b2 }) => {
                a1.semantic_eq(b1)
                    && a2.len() == b2.len()
                    && a2.iter().zip(b2).all(|(x, y)| x.semantic_eq(y))
            }
            (NamedFormat { kind: a0, source_fmt: a1, arguments: a2 }, NamedFormat { kind: b0, source_fmt: b1, arguments: b2 }) => {
                a0 == b0
                    && a1.semantic_eq(b1)
                    && a2.len() == b2.len()
                    && a2.iter().zip(b2).all(|((n, x), (m, y))| str_eq(n, m) && x.semantic_eq(y))
            }
            (
                AsNumber { kind: a0, currency_code: a1, source_value: a2, options: a3, target_culture: a4 },
                AsNumber { kind: b0, currency_code: b1, source_value: b2, options: b3, target_culture: b4 },
            ) => {
                a0 == b0
                    && match (a1, b1) {
                        (Some(x), Some(y)) => str_eq(x, y),
                        (Option::None, Option::None) => true,
                        _ => false,
                    }
                    && a2.semantic_eq(b2)
                    && a3 == b3
                    && str_eq(a4, b4)
            }
            _ => false,
        }
    }
}

impl NiagaraGeneratedFunction {
    fn read(r: &mut Reader) -> Result<Self> {
        let definition_name = r.fname()?;
        let instance_name = r.fstring()?;
        let n = count(r, "Specifiers")?;
        let mut specifiers = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
        for _ in 0..n {
            specifiers.push((r.fname()?, r.fname()?));
        }
        let mut refs = [Vec::new(), Vec::new()];
        for slot in refs.iter_mut() {
            let n = count(r, "variadic references")?;
            slot.reserve(n.min(super::limits::PREALLOC_CAP));
            for _ in 0..n {
                slot.push(NiagaraVariableCommonReference {
                    name: r.fname()?,
                    underlying_type: r.i32()?,
                });
            }
        }
        let [variadic_inputs, variadic_outputs] = refs;
        Ok(NiagaraGeneratedFunction {
            definition_name,
            instance_name,
            specifiers,
            variadic_inputs,
            variadic_outputs,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.fname(&mut self.definition_name.clone())?;
        ar.fstring(&mut self.instance_name.clone())?;
        ar.i32(&mut (self.specifiers.len() as i32))?;
        for (k, v) in &self.specifiers {
            ar.fname(&mut k.clone())?;
            ar.fname(&mut v.clone())?;
        }
        for list in [&self.variadic_inputs, &self.variadic_outputs] {
            ar.i32(&mut (list.len() as i32))?;
            for e in list {
                ar.fname(&mut e.name.clone())?;
                ar.i32(&mut e.underlying_type.to_owned())?;
            }
        }
        Ok(())
    }

    fn semantic_eq(&self, o: &NiagaraGeneratedFunction) -> bool {
        self.definition_name == o.definition_name
            && self.instance_name == o.instance_name
            && self.instance_name.wide == o.instance_name.wide
            && self.specifiers == o.specifiers
            && self.variadic_inputs == o.variadic_inputs
            && self.variadic_outputs == o.variadic_outputs
    }
}

/// A count read from the file, bounded before it is trusted.
fn count(r: &mut Reader, what: &str) -> Result<usize> {
    let n = r.i32()?;
    super::limits::bounded(n, super::limits::MAX_CONTAINER_ELEMENTS, what, r.o - 4)
}

impl BulkArray {
    fn read(r: &mut Reader, what: &str) -> Result<Self> {
        let element_size = r.i32()?;
        let n = count(r, what)?;
        let bytes = n
            .checked_mul(element_size.max(0) as usize)
            .ok_or_else(|| anyhow::anyhow!("{what} size overflow"))?;
        Ok(BulkArray { element_size, data: r.take(bytes)?.to_vec() })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        if self.element_size > 0 && self.data.len() % self.element_size as usize != 0 {
            anyhow::bail!("bulk array data is not a whole number of elements");
        }
        ar.i32(&mut self.element_size.to_owned())?;
        let n = if self.element_size > 0 {
            (self.data.len() / self.element_size as usize) as i32
        } else {
            0
        };
        ar.i32(&mut n.to_owned())?;
        let len = self.data.len();
        ar.raw(&mut self.data.clone(), len)
    }
}

impl WeightedRandomSampler {
    fn read(r: &mut Reader) -> Result<Self> {
        let n = count(r, "sampler prob")?;
        let mut prob = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
        for _ in 0..n {
            prob.push(r.f32()?);
        }
        let n = count(r, "sampler alias")?;
        let mut alias = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
        for _ in 0..n {
            alias.push(r.i32()?);
        }
        Ok(WeightedRandomSampler { prob, alias, total_weight: r.f32()? })
    }
    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut (self.prob.len() as i32))?;
        for p in &self.prob {
            ar.f32(&mut p.to_owned())?;
        }
        ar.i32(&mut (self.alias.len() as i32))?;
        for a in &self.alias {
            ar.i32(&mut a.to_owned())?;
        }
        ar.f32(&mut self.total_weight.to_owned())
    }
    fn semantic_eq(&self, o: &WeightedRandomSampler) -> bool {
        self.prob.len() == o.prob.len()
            && self.prob.iter().zip(&o.prob).all(|(a, b)| a.to_bits() == b.to_bits())
            && self.alias == o.alias
            && self.total_weight.to_bits() == o.total_weight.to_bits()
    }
}

fn read_i32_array(r: &mut Reader, what: &str) -> Result<Vec<i32>> {
    let n = count(r, what)?;
    let mut v = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
    for _ in 0..n {
        v.push(r.i32()?);
    }
    Ok(v)
}
fn write_i32_array(ar: &mut impl Ar, v: &[i32]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for x in v {
        ar.i32(&mut x.to_owned())?;
    }
    Ok(())
}
fn read_f64s<const N: usize>(r: &mut Reader) -> Result<[f64; N]> {
    let mut out = [0.0; N];
    for slot in out.iter_mut() {
        *slot = r.f64()?;
    }
    Ok(out)
}
fn write_f64s(ar: &mut impl Ar, v: &[f64]) -> Result<()> {
    for x in v {
        ar.f64(&mut x.to_owned())?;
    }
    Ok(())
}
fn f64_bits_eq(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

impl HandWritten {
    /// Read `name`, or `None` if it is not modeled here yet.
    pub(super) fn read(
        r: &mut Reader,
        name: &str,
        usmap: &Usmap,
        depth: usize,
    ) -> Result<Option<Self>> {
        Ok(match name {
            "NiagaraVariableBase" | "NiagaraVariable" | "NiagaraVariableWithOffset"
            | "NiagaraDataChannelVariable" => {
                let var_name = r.fname()?;
                let type_def = read_struct(r, "NiagaraTypeDefinition", usmap, depth + 1)?;
                let payload = match name {
                    "NiagaraVariableWithOffset" => NiagaraPayload::Offset(r.i32()?),
                    "NiagaraVariable" => {
                        let n = r.i32()?;
                        let n = super::limits::bounded(
                            n,
                            super::limits::MAX_CONTAINER_ELEMENTS,
                            "NiagaraVariable VarData",
                            r.o - 4,
                        )?;
                        NiagaraPayload::VarData(r.take(n)?.to_vec())
                    }
                    _ => NiagaraPayload::None,
                };
                Some(HandWritten::NiagaraVariable(NiagaraVariable {
                    name: var_name,
                    type_def,
                    payload,
                }))
            }
            "MovieSceneFloatChannel" | "MovieSceneDoubleChannel" => {
                let wide = name == "MovieSceneDoubleChannel";
                Some(HandWritten::MovieSceneChannel(MovieSceneChannel {
                    pre_infinity_extrap: r.u8()?,
                    post_infinity_extrap: r.u8()?,
                    times: BulkArray::read(r, "channel times")?,
                    values: BulkArray::read(r, "channel values")?,
                    default_value: if wide { r.f64()? } else { r.f32()? as f64 },
                    has_default_value: r.u32()? != 0,
                    tick_resolution_numerator: r.i32()?,
                    tick_resolution_denominator: r.i32()?,
                    show_curve: r.u32()? != 0,
                }))
            }
            "PCGPoint" => {
                let mask = r.u8()?;
                let transform = read_f64s::<10>(r)?;
                Some(HandWritten::PcgPoint(PcgPoint {
                    transform,
                    density: if mask & 1 != 0 { Some(r.f32()?) } else { None },
                    bounds_min: if mask & 2 != 0 { Some(read_f64s::<3>(r)?) } else { None },
                    bounds_max: if mask & 4 != 0 { Some(read_f64s::<3>(r)?) } else { None },
                    color: if mask & 8 != 0 { Some(read_f64s::<4>(r)?) } else { None },
                    steepness: if mask & 16 != 0 { Some(r.f32()?) } else { None },
                    seed: if mask & 32 != 0 { Some(r.i32()?) } else { None },
                    metadata_entry: if mask & 64 != 0 { Some(r.u64()?) } else { None },
                }))
            }
            "SkeletalMeshSamplingLODBuiltData" => Some(HandWritten::SkeletalMeshSamplingLod(
                WeightedRandomSampler::read(r)?,
            )),
            "MovieSceneEvalTemplatePtr" | "MovieSceneTrackImplementationPtr"
            | "MovieSceneSequenceInstanceDataPtr" => {
                let type_name = r.fstring()?;
                let payload = if type_name.is_empty() {
                    Option::None
                } else {
                    let short = type_name.rsplit('.').next().unwrap_or(&type_name).to_string();
                    Some(read_struct(r, &short, usmap, depth + 1)?)
                };
                Some(HandWritten::MovieSceneInlineValue(MovieSceneInlineValue {
                    type_name,
                    payload,
                }))
            }
            "MovieSceneEvaluationFieldEntityTree" | "MovieSceneSubSequenceTree" => {
                let root = TreeNode::read(r)?;
                let n = count(r, "child node entries")?;
                let mut child_entries = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    child_entries.push(TreeEntry::read(r)?);
                }
                let n = count(r, "child nodes")?;
                let mut child_nodes = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    child_nodes.push(TreeNode::read(r)?);
                }
                let n = count(r, "data entries")?;
                let mut data_entries = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    data_entries.push(TreeEntry::read(r)?);
                }
                let n = count(r, "data items")?;
                let mut items = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                let sub = name == "MovieSceneSubSequenceTree";
                for _ in 0..n {
                    items.push(if sub {
                        TreeItem::SubSequence { sequence_id: r.u32()?, flags: r.u8()? }
                    } else {
                        TreeItem::EntityAndMetaDataIndex { entity: r.i32()?, meta_data: r.i32()? }
                    });
                }
                Some(HandWritten::EvaluationTree(EvaluationTree {
                    root,
                    child_entries,
                    child_nodes,
                    data_entries,
                    items,
                }))
            }
            "ShaderValueTypeHandle" => {
                Some(HandWritten::ShaderValueType(ShaderValueType::read(r, depth)?))
            }
            "PerQualityLevelInt" | "PerQualityLevelFloat" => {
                let cooked = r.u32()? != 0;
                let default_bits = r.i32()?;
                let n = count(r, "PerQuality overrides")?;
                let mut overrides = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    overrides.push((r.i32()?, r.i32()?));
                }
                Some(HandWritten::PerQualityLevel(PerQualityLevel {
                    cooked,
                    default_bits,
                    overrides,
                }))
            }
            "FontData" => {
                if r.u32()? == 0 {
                    anyhow::bail!("uncooked FFontData uses the tagged-property form");
                }
                let font_face_asset = r.i32()?;
                let inline_face = if font_face_asset == 0 {
                    Some(InlineFontFace {
                        filename: r.fstring()?,
                        hinting: r.u8()?,
                        loading_policy: r.u8()?,
                    })
                } else {
                    Option::None
                };
                Some(HandWritten::FontData(FontData {
                    font_face_asset,
                    inline_face,
                    sub_face_index: r.i32()?,
                }))
            }
            "MaterialOverrideNanite" => {
                let cooked = r.i32()? != 0;
                let override_material = if cooked { Some(r.i32()?) } else { Option::None };
                Some(HandWritten::MaterialOverrideNanite(MaterialOverrideNanite {
                    cooked,
                    override_material,
                    properties: read_struct(r, name, usmap, depth + 1)?,
                }))
            }
            "MovieSceneTimeWarpVariant" => Some(HandWritten::TimeWarpVariant(
                if r.u32()? != 0 {
                    TimeWarpVariant::Literal(f64::from_bits(r.u64()?))
                } else {
                    let kind = r.u8()?;
                    let (object, payload) = match kind {
                        0 => (Option::None, Option::None),
                        1 => (Some(r.i32()?), Option::None),
                        _ => {
                            let s = match kind {
                                2 => "MovieSceneTimeWarpFixedFrame",
                                3 => "FrameRate",
                                4 => "MovieSceneTimeWarpLoop",
                                5 => "MovieSceneTimeWarpClamp",
                                6 => "MovieSceneTimeWarpLoopFloat",
                                7 => "MovieSceneTimeWarpClampFloat",
                                other => anyhow::bail!("unknown EMovieSceneTimeWarpType {other}"),
                            };
                            (Option::None, Some(read_struct(r, s, usmap, depth + 1)?))
                        }
                    };
                    TimeWarpVariant::Typed { kind, object, payload }
                },
            )),
            "UniversalObjectLocatorFragment" => {
                let fragment_type = r.fname()?;
                let payload = match super::text::locator_fragment_payload(&fragment_type) {
                    Some("") => Option::None,
                    Some(s) => Some(read_struct(r, s, usmap, depth + 1)?),
                    Option::None => anyhow::bail!(
                        "unmapped universal object locator fragment type '{fragment_type}'"
                    ),
                };
                Some(HandWritten::LocatorFragment(LocatorFragment { fragment_type, payload }))
            }
            "InstancedPropertyBag" => {
                let (descriptors, serial_size, values) = if r.u32()? != 0 {
                    let n = count(r, "property bag descriptors")?;
                    let mut descs = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                    for _ in 0..n {
                        let value_type_object = r.i32()?;
                        let id: [u8; 16] = r.take(16)?.try_into().expect("16 bytes");
                        let name = r.fname()?;
                        let value_type = r.u8()?;
                        let containers = r.u8()? as usize;
                        let container_types = r.take(containers)?.to_vec();
                        if r.u32()? != 0 {
                            anyhow::bail!(
                                "property-bag descriptor carries editor-only metadata"
                            );
                        }
                        descs.push(PropertyBagDesc {
                            value_type_object,
                            id,
                            name,
                            value_type,
                            container_types,
                        });
                    }
                    let serial = r.i32()?;
                    let at = r.o;
                    let schema = property_bag_schema(&descs, r.resolver);
                    let slots: Vec<(&UsmapProperty, u8, &str)> =
                        schema.iter().map(|p| (p, 0u8, "PropertyBag")).collect();
                    let values =
                        super::block::read_struct_with_schema(r, "PropertyBag", &slots, usmap, 0)?;
                    if r.o - at != serial.max(0) as usize {
                        anyhow::bail!(
                            "property bag block consumed {} bytes, the file declares {serial}",
                            r.o - at
                        );
                    }
                    (Some(descs), serial, Some(values))
                } else {
                    (Option::None, 0, Option::None)
                };
                Some(HandWritten::InstancedPropertyBag(InstancedPropertyBag {
                    descriptors,
                    serial_size,
                    values,
                }))
            }
            "MaterialLayersFunctionsTree" => {
                let n = count(r, "layer tree nodes")?;
                let mut nodes = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    nodes.push([r.i32()?, r.i32()?, r.i32()?, r.i32()?]);
                }
                let n = count(r, "layer tree payloads")?;
                let mut payloads = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    payloads.push([r.i32()?, r.i32()?]);
                }
                Some(HandWritten::MaterialLayersTree(MaterialLayersTree {
                    nodes,
                    payloads,
                    root: r.i32()?,
                }))
            }
            "NiagaraDataInterfaceGPUParamInfo" => {
                let hlsl_symbol = r.fstring()?;
                let di_class_name = r.fstring()?;
                let n = count(r, "GeneratedFunctions")?;
                let mut generated_functions = Vec::with_capacity(n.min(super::limits::PREALLOC_CAP));
                for _ in 0..n {
                    generated_functions.push(NiagaraGeneratedFunction::read(r)?);
                }
                Some(HandWritten::NiagaraGpuParamInfo(NiagaraGpuParamInfo {
                    hlsl_symbol,
                    di_class_name,
                    generated_functions,
                }))
            }
            "SkeletalMeshSamplingRegionBuiltData" => {
                Some(HandWritten::SkeletalMeshSamplingRegion(SkeletalMeshSamplingRegion {
                    triangle_indices: read_i32_array(r, "TriangleIndices")?,
                    bone_indices: read_i32_array(r, "BoneIndices")?,
                    sampler: WeightedRandomSampler::read(r)?,
                    vertices: read_i32_array(r, "Vertices")?,
                }))
            }
            _ => None,
        })
    }

    /// Write it back from the typed fields — not from a retained span.
    pub(super) fn write(&self, ar: &mut impl Ar, name: &str, usmap: &Usmap) -> Result<()> {
        match self {
            HandWritten::NiagaraVariable(v) => {
                ar.fname(&mut v.name.clone())?;
                let flat = flattened_schema("NiagaraTypeDefinition", usmap)?;
                write_block(ar, &v.type_def, &flat, usmap)?;
                match &v.payload {
                    NiagaraPayload::None => {}
                    NiagaraPayload::Offset(o) => ar.i32(&mut o.to_owned())?,
                    NiagaraPayload::VarData(bytes) => {
                        ar.i32(&mut (bytes.len() as i32))?;
                        let n = bytes.len();
                        ar.raw(&mut bytes.clone(), n)?;
                    }
                }
                // The payload shape is decided by the struct's *name*, so a
                // value paired with the wrong name would write a different
                // length than it read.
                let expected_none = !matches!(name, "NiagaraVariableWithOffset" | "NiagaraVariable");
                let is_none = matches!(v.payload, NiagaraPayload::None);
                if expected_none != is_none {
                    anyhow::bail!("{name} payload does not match its struct name");
                }
                Ok(())
            }
            HandWritten::MovieSceneChannel(c) => {
                ar.u8(&mut c.pre_infinity_extrap.to_owned())?;
                ar.u8(&mut c.post_infinity_extrap.to_owned())?;
                c.times.write(ar)?;
                c.values.write(ar)?;
                // The channel's width is decided by its name, not by the value.
                if name == "MovieSceneDoubleChannel" {
                    ar.f64(&mut c.default_value.to_owned())?;
                } else {
                    ar.f32(&mut (c.default_value as f32))?;
                }
                ar.u32(&mut (c.has_default_value as u32))?;
                ar.i32(&mut c.tick_resolution_numerator.to_owned())?;
                ar.i32(&mut c.tick_resolution_denominator.to_owned())?;
                ar.u32(&mut (c.show_curve as u32))
            }
            HandWritten::PcgPoint(p) => {
                // The mask is derived from which fields are present, so adding
                // one is just setting it to `Some`.
                let mask = (p.density.is_some() as u8)
                    | ((p.bounds_min.is_some() as u8) << 1)
                    | ((p.bounds_max.is_some() as u8) << 2)
                    | ((p.color.is_some() as u8) << 3)
                    | ((p.steepness.is_some() as u8) << 4)
                    | ((p.seed.is_some() as u8) << 5)
                    | ((p.metadata_entry.is_some() as u8) << 6);
                ar.u8(&mut mask.to_owned())?;
                write_f64s(ar, &p.transform)?;
                if let Some(v) = p.density {
                    ar.f32(&mut v.to_owned())?;
                }
                for v in [&p.bounds_min, &p.bounds_max] {
                    if let Some(v) = v {
                        write_f64s(ar, v)?;
                    }
                }
                if let Some(v) = &p.color {
                    write_f64s(ar, v)?;
                }
                if let Some(v) = p.steepness {
                    ar.f32(&mut v.to_owned())?;
                }
                if let Some(v) = p.seed {
                    ar.i32(&mut v.to_owned())?;
                }
                if let Some(v) = p.metadata_entry {
                    ar.u64(&mut v.to_owned())?;
                }
                Ok(())
            }
            HandWritten::Text(t) => t.write(ar),
            HandWritten::MovieSceneInlineValue(v) => {
                ar.fstring(&mut v.type_name.clone())?;
                match (&v.payload, v.type_name.is_empty()) {
                    (Some(b), false) => {
                        let short =
                            v.type_name.rsplit('.').next().unwrap_or(&v.type_name).to_string();
                        let flat = flattened_schema(&short, usmap)?;
                        write_block(ar, b, &flat, usmap)
                    }
                    (Option::None, true) => Ok(()),
                    _ => anyhow::bail!("inline value payload does not match its type name"),
                }
            }
            HandWritten::EvaluationTree(t) => {
                t.root.write(ar)?;
                ar.i32(&mut (t.child_entries.len() as i32))?;
                for e in &t.child_entries {
                    e.write(ar)?;
                }
                ar.i32(&mut (t.child_nodes.len() as i32))?;
                for n in &t.child_nodes {
                    n.write(ar)?;
                }
                ar.i32(&mut (t.data_entries.len() as i32))?;
                for e in &t.data_entries {
                    e.write(ar)?;
                }
                ar.i32(&mut (t.items.len() as i32))?;
                for item in &t.items {
                    match item {
                        TreeItem::EntityAndMetaDataIndex { entity, meta_data } => {
                            ar.i32(&mut entity.to_owned())?;
                            ar.i32(&mut meta_data.to_owned())?;
                        }
                        TreeItem::SubSequence { sequence_id, flags } => {
                            ar.u32(&mut sequence_id.to_owned())?;
                            ar.u8(&mut flags.to_owned())?;
                        }
                    }
                }
                Ok(())
            }
            HandWritten::ShaderValueType(t) => t.write(ar),
            HandWritten::PerQualityLevel(q) => {
                ar.u32(&mut (q.cooked as u32))?;
                ar.i32(&mut q.default_bits.to_owned())?;
                ar.i32(&mut (q.overrides.len() as i32))?;
                for (k, v) in &q.overrides {
                    ar.i32(&mut k.to_owned())?;
                    ar.i32(&mut v.to_owned())?;
                }
                Ok(())
            }
            HandWritten::FontData(f) => {
                // Only the cooked form is modeled, so the flag is a constant.
                ar.u32(&mut 1)?;
                ar.i32(&mut f.font_face_asset.to_owned())?;
                match (&f.inline_face, f.font_face_asset == 0) {
                    (Some(face), true) => {
                        ar.fstring(&mut face.filename.clone())?;
                        ar.u8(&mut face.hinting.to_owned())?;
                        ar.u8(&mut face.loading_policy.to_owned())?;
                    }
                    (Option::None, false) => {}
                    _ => anyhow::bail!("inline font face does not match the face asset index"),
                }
                ar.i32(&mut f.sub_face_index.to_owned())
            }
            HandWritten::MaterialOverrideNanite(m) => {
                ar.i32(&mut (m.cooked as i32))?;
                match (m.override_material, m.cooked) {
                    (Some(o), true) => ar.i32(&mut o.to_owned())?,
                    (Option::None, false) => {}
                    _ => anyhow::bail!("override material does not match the cooked flag"),
                }
                let flat = flattened_schema(name, usmap)?;
                write_block(ar, &m.properties, &flat, usmap)
            }
            HandWritten::TimeWarpVariant(v) => match v {
                TimeWarpVariant::Literal(d) => {
                    ar.u32(&mut 1)?;
                    ar.u64(&mut d.to_bits())
                }
                TimeWarpVariant::Typed { kind, object, payload } => {
                    ar.u32(&mut 0)?;
                    ar.u8(&mut kind.to_owned())?;
                    if let Some(o) = object {
                        ar.i32(&mut o.to_owned())?;
                    }
                    if let Some(b) = payload {
                        let s = match kind {
                            2 => "MovieSceneTimeWarpFixedFrame",
                            3 => "FrameRate",
                            4 => "MovieSceneTimeWarpLoop",
                            5 => "MovieSceneTimeWarpClamp",
                            6 => "MovieSceneTimeWarpLoopFloat",
                            _ => "MovieSceneTimeWarpClampFloat",
                        };
                        let flat = flattened_schema(s, usmap)?;
                        write_block(ar, b, &flat, usmap)?;
                    }
                    Ok(())
                }
            },
            HandWritten::InstancedPropertyBag(b) => {
                match &b.descriptors {
                    Some(descs) => {
                        ar.u32(&mut 1)?;
                        ar.i32(&mut (descs.len() as i32))?;
                        for d in descs {
                            ar.i32(&mut d.value_type_object.to_owned())?;
                            ar.raw(&mut d.id.to_vec(), 16)?;
                            ar.fname(&mut d.name.clone())?;
                            ar.u8(&mut d.value_type.to_owned())?;
                            ar.u8(&mut (d.container_types.len() as u8))?;
                            let n = d.container_types.len();
                            ar.raw(&mut d.container_types.clone(), n)?;
                            ar.u32(&mut 0)?;
                        }
                        ar.i32(&mut b.serial_size.to_owned())?;
                        let Some(values) = &b.values else {
                            anyhow::bail!("a property bag with descriptors has no values")
                        };
                        let schema = property_bag_schema(descs, ar.resolver());
                        let slots: Vec<(&UsmapProperty, u8, &str)> =
                            schema.iter().map(|p| (p, 0u8, "PropertyBag")).collect();
                        write_block(ar, values, &slots, usmap)?;
                    }
                    Option::None => ar.u32(&mut 0)?,
                }
                Ok(())
            }
            HandWritten::MaterialLayersTree(t) => {
                ar.i32(&mut (t.nodes.len() as i32))?;
                for n in &t.nodes {
                    for v in n {
                        ar.i32(&mut v.to_owned())?;
                    }
                }
                ar.i32(&mut (t.payloads.len() as i32))?;
                for p in &t.payloads {
                    for v in p {
                        ar.i32(&mut v.to_owned())?;
                    }
                }
                ar.i32(&mut t.root.to_owned())
            }
            HandWritten::LocatorFragment(f) => {
                ar.fname(&mut f.fragment_type.clone())?;
                if let Some(b) = &f.payload {
                    let s = super::text::locator_fragment_payload(&f.fragment_type)
                        .ok_or_else(|| anyhow::anyhow!("unmapped locator fragment"))?;
                    let flat = flattened_schema(s, usmap)?;
                    write_block(ar, b, &flat, usmap)?;
                }
                Ok(())
            }
            HandWritten::NiagaraGpuParamInfo(p) => {
                ar.fstring(&mut p.hlsl_symbol.clone())?;
                ar.fstring(&mut p.di_class_name.clone())?;
                ar.i32(&mut (p.generated_functions.len() as i32))?;
                for f in &p.generated_functions {
                    f.write(ar)?;
                }
                Ok(())
            }
            HandWritten::SkeletalMeshSamplingLod(s) => s.write(ar),
            HandWritten::SkeletalMeshSamplingRegion(rg) => {
                write_i32_array(ar, &rg.triangle_indices)?;
                write_i32_array(ar, &rg.bone_indices)?;
                rg.sampler.write(ar)?;
                write_i32_array(ar, &rg.vertices)
            }
        }
    }

    /// See [`super::value::PropertyBlock::semantic_eq`].
    pub fn semantic_eq(&self, other: &HandWritten) -> bool {
        match (self, other) {
            (HandWritten::NiagaraVariable(a), HandWritten::NiagaraVariable(b)) => {
                a.name == b.name
                    && a.type_def.semantic_eq(&b.type_def)
                    && match (&a.payload, &b.payload) {
                        (NiagaraPayload::None, NiagaraPayload::None) => true,
                        (NiagaraPayload::Offset(x), NiagaraPayload::Offset(y)) => x == y,
                        (NiagaraPayload::VarData(x), NiagaraPayload::VarData(y)) => x == y,
                        _ => false,
                    }
            }
            (HandWritten::MovieSceneChannel(a), HandWritten::MovieSceneChannel(b)) => {
                a.pre_infinity_extrap == b.pre_infinity_extrap
                    && a.post_infinity_extrap == b.post_infinity_extrap
                    && a.times == b.times
                    && a.values == b.values
                    && a.default_value.to_bits() == b.default_value.to_bits()
                    && a.has_default_value == b.has_default_value
                    && a.tick_resolution_numerator == b.tick_resolution_numerator
                    && a.tick_resolution_denominator == b.tick_resolution_denominator
                    && a.show_curve == b.show_curve
            }
            (HandWritten::PcgPoint(a), HandWritten::PcgPoint(b)) => {
                let opt3 = |x: &Option<[f64; 3]>, y: &Option<[f64; 3]>| match (x, y) {
                    (Some(p), Some(q)) => f64_bits_eq(p, q),
                    (None, None) => true,
                    _ => false,
                };
                f64_bits_eq(&a.transform, &b.transform)
                    && a.density.map(f32::to_bits) == b.density.map(f32::to_bits)
                    && opt3(&a.bounds_min, &b.bounds_min)
                    && opt3(&a.bounds_max, &b.bounds_max)
                    && match (&a.color, &b.color) {
                        (Some(p), Some(q)) => f64_bits_eq(p, q),
                        (None, None) => true,
                        _ => false,
                    }
                    && a.steepness.map(f32::to_bits) == b.steepness.map(f32::to_bits)
                    && a.seed == b.seed
                    && a.metadata_entry == b.metadata_entry
            }
            (HandWritten::Text(a), HandWritten::Text(b)) => a.semantic_eq(b),
            (HandWritten::MovieSceneInlineValue(a), HandWritten::MovieSceneInlineValue(b)) => {
                a.type_name == b.type_name
                    && a.type_name.wide == b.type_name.wide
                    && match (&a.payload, &b.payload) {
                        (Some(x), Some(y)) => x.semantic_eq(y),
                        (Option::None, Option::None) => true,
                        _ => false,
                    }
            }
            // These carry no floats, so structural equality is already bit
            // equality — the concern that made `semantic_eq` necessary does not
            // arise for them.
            (HandWritten::EvaluationTree(a), HandWritten::EvaluationTree(b)) => {
                a.root == b.root
                    && a.child_entries == b.child_entries
                    && a.child_nodes == b.child_nodes
                    && a.data_entries == b.data_entries
                    && a.items == b.items
            }
            (HandWritten::ShaderValueType(a), HandWritten::ShaderValueType(b)) => a == b,
            (HandWritten::PerQualityLevel(a), HandWritten::PerQualityLevel(b)) => {
                a.cooked == b.cooked
                    && a.default_bits == b.default_bits
                    && a.overrides == b.overrides
            }
            (HandWritten::FontData(a), HandWritten::FontData(b)) => {
                a.font_face_asset == b.font_face_asset
                    && a.sub_face_index == b.sub_face_index
                    && match (&a.inline_face, &b.inline_face) {
                        (Some(x), Some(y)) => {
                            x.filename == y.filename
                                && x.filename.wide == y.filename.wide
                                && x.hinting == y.hinting
                                && x.loading_policy == y.loading_policy
                        }
                        (Option::None, Option::None) => true,
                        _ => false,
                    }
            }
            (HandWritten::MaterialOverrideNanite(a), HandWritten::MaterialOverrideNanite(b)) => {
                a.cooked == b.cooked
                    && a.override_material == b.override_material
                    && a.properties.semantic_eq(&b.properties)
            }
            (HandWritten::TimeWarpVariant(a), HandWritten::TimeWarpVariant(b)) => match (a, b) {
                (TimeWarpVariant::Literal(x), TimeWarpVariant::Literal(y)) => {
                    x.to_bits() == y.to_bits()
                }
                (
                    TimeWarpVariant::Typed { kind: k1, object: o1, payload: p1 },
                    TimeWarpVariant::Typed { kind: k2, object: o2, payload: p2 },
                ) => {
                    k1 == k2
                        && o1 == o2
                        && match (p1, p2) {
                            (Some(x), Some(y)) => x.semantic_eq(y),
                            (Option::None, Option::None) => true,
                            _ => false,
                        }
                }
                _ => false,
            },
            (HandWritten::InstancedPropertyBag(a), HandWritten::InstancedPropertyBag(b)) => {
                a.descriptors == b.descriptors
                    && a.serial_size == b.serial_size
                    && match (&a.values, &b.values) {
                        (Some(x), Some(y)) => x.semantic_eq(y),
                        (Option::None, Option::None) => true,
                        _ => false,
                    }
            }
            (HandWritten::MaterialLayersTree(a), HandWritten::MaterialLayersTree(b)) => a == b,
            (HandWritten::LocatorFragment(a), HandWritten::LocatorFragment(b)) => {
                a.fragment_type == b.fragment_type
                    && match (&a.payload, &b.payload) {
                        (Some(x), Some(y)) => x.semantic_eq(y),
                        (Option::None, Option::None) => true,
                        _ => false,
                    }
            }
            (HandWritten::NiagaraGpuParamInfo(a), HandWritten::NiagaraGpuParamInfo(b)) => {
                a.hlsl_symbol == b.hlsl_symbol
                    && a.hlsl_symbol.wide == b.hlsl_symbol.wide
                    && a.di_class_name == b.di_class_name
                    && a.di_class_name.wide == b.di_class_name.wide
                    && a.generated_functions.len() == b.generated_functions.len()
                    && a.generated_functions
                        .iter()
                        .zip(&b.generated_functions)
                        .all(|(x, y)| x.semantic_eq(y))
            }
            (HandWritten::SkeletalMeshSamplingLod(a), HandWritten::SkeletalMeshSamplingLod(b)) => {
                a.semantic_eq(b)
            }
            (
                HandWritten::SkeletalMeshSamplingRegion(a),
                HandWritten::SkeletalMeshSamplingRegion(b),
            ) => {
                a.triangle_indices == b.triangle_indices
                    && a.bone_indices == b.bone_indices
                    && a.sampler.semantic_eq(&b.sampler)
                    && a.vertices == b.vertices
            }
            _ => false,
        }
    }

    /// Bytes still untyped inside this value. What `ce_decode_coverage` counts.
    ///
    /// Only `FInstancedPropertyBag`'s payload is left: its layout is described
    /// by the bag's own descriptors, and nothing in the corpus ships one to
    /// model it against.
    pub fn untyped_bytes(&self) -> usize {
        match self {
            // Nothing: the bag's values are a property block now, not a span.
            HandWritten::InstancedPropertyBag(_) => 0,
            _ => 0,
        }
    }
}
