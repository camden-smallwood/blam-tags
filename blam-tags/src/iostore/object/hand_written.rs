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
use super::value::{FName, PropertyBlock};

/// A hand-written struct, decoded.
#[derive(Debug, Clone)]
pub enum HandWritten {
    /// `FNiagaraVariableBase` and the three shapes that extend it. 1.86M of the
    /// 1.92M hand-written spans in the corpus.
    NiagaraVariable(NiagaraVariable),
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
];

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
        }
    }

    /// Bytes still untyped inside this value — zero for everything modeled
    /// here. What `ce_decode_coverage` counts.
    pub fn untyped_bytes(&self) -> usize {
        0
    }
}
