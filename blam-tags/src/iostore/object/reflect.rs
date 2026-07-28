//! Recovering schema from *package* bytes rather than the `.usmap`: a
//! `UStruct`-derived export's own `FField` chain, a `UDataTable`'s rows, and a
//! `UFunction`'s Kismet bytecode.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::archive::{ExportContext, Reader};
use super::limits::{bounded, MAX_FIELD_COUNT};
use super::block::{flattened_schema, read_header, read_struct, read_struct_with_schema};
use super::common::native_count;
use super::export::read_uobject_trailer;
use super::usmap::{PropertyType, Usmap, UsmapProperty};
use super::value::PropValue;

/// Strip UE's `_<index>_<32-hex-guid>` decoration (and a trailing `_Value`/`_Key`
/// map-element marker) from a `UUserDefinedStruct` field name, recovering the
/// author-facing base name (`Head_4_BFCB…` → `Head`).
pub(super) fn deguid(name: &str) -> String {
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
pub(super) fn read_single_field(r: &mut Reader) -> Result<Option<(String, PropertyType, u8)>> {
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
pub(super) fn read_ffield_tail(r: &mut Reader, type_name: &str) -> Result<PropertyType> {
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
    for (_, non_zero) in &present.present {
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

pub(super) fn try_read_script(r: &mut Reader) -> Result<Vec<u8>> {
    let num = r.i32()?;
    let num = bounded(num, MAX_FIELD_COUNT, "numFields", r.o - 4)? as i32;
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

/// A `UStruct`'s `FField` chain followed by its script blob. Unlike
/// [`try_read_script`] this accepts an empty script, which is the normal case
/// for a struct that carries no bytecode.
/// `UStruct::SerializeProperties`: an `int32` count and that many `FField`s,
/// as a schema the unversioned property walker can index by.
pub(super) fn read_field_chain(r: &mut Reader) -> Result<Vec<UsmapProperty>> {
    let num = r.i32()?;
    let num = bounded(num, MAX_FIELD_COUNT, "numFields", r.o - 4)? as i32;
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

pub(super) fn try_read_struct_fields_and_script(r: &mut Reader) -> Result<()> {
    r.struct_fields = Some(read_field_chain(r)?);
    let _bytecode_size = r.i32()?;
    let storage = r.i32()?;
    if !(0..=16_000_000).contains(&storage) {
        bail!("implausible ScriptStorageSize {storage}");
    }
    r.take(storage as usize)?;
    Ok(())
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
