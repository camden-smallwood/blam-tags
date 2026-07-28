//! One property value, by its schema type.
//!
//! `in_container` distinguishes the two ways UE reaches a value, which matters
//! for enums and only for enums — see [`read_value`].

use anyhow::{bail, Result};

use super::archive::Reader;
use super::block::read_struct;
use super::usmap::{PropertyType, Usmap};
use super::value::{PropValue, SoftObjectPath};
use super::common::read_container_removals;
use super::limits::{bounded, MAX_CONTAINER_ELEMENTS, PREALLOC_CAP};
use super::structs::{native_struct_size, read_native_variable_struct};
use super::text::read_text;

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
pub(super) fn read_value(
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
        PropertyType::Byte { enum_name: Some(_) } if in_container => PropValue::Name(r.fname()?),
        PropertyType::Byte { .. } | PropertyType::Int8 => PropValue::Int(r.u8()? as i64),
        PropertyType::Int => PropValue::Int(r.i32()? as i64),
        PropertyType::UInt32 => PropValue::Int(r.u32()? as i64),
        PropertyType::Int16 | PropertyType::UInt16 => PropValue::Int(r.u16()? as i64),
        PropertyType::Int64 | PropertyType::UInt64 => PropValue::Int(r.u64()? as i64),
        PropertyType::Float => PropValue::Float(r.f32()? as f64),
        PropertyType::Double => PropValue::Float(r.f64()?),
        PropertyType::Name => PropValue::Name(r.fname()?),
        PropertyType::Str | PropertyType::Utf8Str | PropertyType::AnsiStr => PropValue::Str(r.fstring()?),
        PropertyType::Enum { inner, .. } => {
            if in_container {
                PropValue::Name(r.fname()?)
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
            let package = r.fname()?;
            let asset = r.fname()?;
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
            let n = bounded(n, MAX_CONTAINER_ELEMENTS, "array", r.o - 4)?;
            let mut v = Vec::with_capacity(n.min(PREALLOC_CAP));
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
            let n = bounded(n, MAX_CONTAINER_ELEMENTS, "set", r.o - 4)?;
            let mut v = Vec::with_capacity(n.min(PREALLOC_CAP));
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
            let n = bounded(n, MAX_CONTAINER_ELEMENTS, "map", r.o - 4)?;
            let mut m = Vec::with_capacity(n.min(PREALLOC_CAP));
            for _ in 0..n {
                let key = read_value(r, k, usmap, depth, true)?;
                let value = read_value(r, val, usmap, depth, true)?;
                m.push((key, value));
            }
            PropValue::Map(m)
        }
        // `FDelegateProperty::SerializeItem` (PropertyDelegate.cpp:85) is
        // `FScriptDelegate::Serialize`: the bound object as an `FPackageIndex`
        // and the function's `FName`.
        PropertyType::Delegate => {
            let object = r.i32()?;
            let function = r.fname()?;
            PropValue::Delegate { object, function }
        }
        // `FMulticastScriptDelegate::Serialize`: a count and that many bindings.
        PropertyType::MulticastDelegate => {
            let n = r.i32()?;
            let mut list = Vec::with_capacity(n.max(0) as usize);
            for _ in 0..n.max(0) {
                let object = r.i32()?;
                let function = r.fname()?;
                list.push((object, function));
            }
            PropValue::MulticastDelegate(list)
        }
        // `FFieldPath`: a `TArray<FName>` path then the owner object.
        PropertyType::FieldPath => {
            let n = r.i32()?;
            let mut path = Vec::with_capacity(n.max(0) as usize);
            for _ in 0..n.max(0) {
                path.push(r.fname()?);
            }
            let owner = r.i32()?;
            PropValue::FieldPath { path, owner }
        }
        PropertyType::Text => read_text(r, 0)?,
        // `FOptionalProperty::SerializeItem` (PropertyOptional.cpp:203) encodes
        // "is set" through `TryEnterField`, i.e. an `FArchive` bool — **four**
        // bytes — and then, when set, the inner value through its own
        // `SerializeItem`, which is the container path. Measured on a
        // `WorldPartitionRuntimeCellDataHashSet`, whose optional `CellBounds`
        // only resolves to a clean 12800×12800 box under a four-byte flag.
        PropertyType::Optional(inner) => {
            if r.u32()? != 0 {
                read_value(r, inner, usmap, depth, true)?
            } else {
                PropValue::Unset
            }
        }
        PropertyType::Unknown(t) => bail!("unknown property kind {t} in unversioned stream"),
    })
}
