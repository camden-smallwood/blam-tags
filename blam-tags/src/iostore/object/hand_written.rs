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
use super::usmap::Usmap;
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
];

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

    /// Bytes still untyped inside this value — zero for everything modeled
    /// here. What `ce_decode_coverage` counts.
    pub fn untyped_bytes(&self) -> usize {
        0
    }
}
