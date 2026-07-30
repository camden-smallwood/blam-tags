//! One property value, by its schema type.
//!
//! `in_container` distinguishes the two ways UE reaches a value, which matters
//! for enums and only for enums — see [`read_value`].

use anyhow::{bail, Context, Result};

use super::archive::{Ar, Reader, Writer};
use super::block::{flattened_schema, read_struct, write_block};
use super::usmap::{PropertyType, Usmap, UsmapProperty};
use super::value::{BlockLayout, PropValue, SoftObjectPath};
use super::common::{read_container_removals, with_removals};
use super::limits::{bounded, MAX_CONTAINER_ELEMENTS, PREALLOC_CAP};
use super::native::NativeStruct;
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
                PropValue::Native(NativeStruct::decode(name, r.take(size)?)?)
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
            let removals =
                read_container_removals(r, "set", |r| read_value(r, inner, usmap, depth, true))?;
            let n = r.i32()?;
            let n = bounded(n, MAX_CONTAINER_ELEMENTS, "set", r.o - 4)?;
            let mut v = Vec::with_capacity(n.min(PREALLOC_CAP));
            for _ in 0..n {
                v.push(read_value(r, inner, usmap, depth, true)?);
            }
            with_removals(removals, PropValue::Set(v))
        }
        // `FMapProperty::SerializeItem`'s load path (PropertyMap.cpp:624):
        // `NumKeysToRemove`, then that many **keys**, then `NumEntries` and the
        // pairs. Reading the removal count without consuming its keys is
        // invisible while the count is zero — which it is for almost every
        // cooked asset — and desyncs catastrophically when it is not.
        PropertyType::Map(k, val) => {
            let removals =
                read_container_removals(r, "map", |r| read_value(r, k, usmap, depth, true))?;
            let n = r.i32()?;
            let n = bounded(n, MAX_CONTAINER_ELEMENTS, "map", r.o - 4)?;
            let mut m = Vec::with_capacity(n.min(PREALLOC_CAP));
            for _ in 0..n {
                let key = read_value(r, k, usmap, depth, true)?;
                let value = read_value(r, val, usmap, depth, true)?;
                m.push((key, value));
            }
            with_removals(removals, PropValue::Map(m))
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


/// Separate a container's removal prefix from the container itself.
///
/// A plain container has an empty prefix; only a [`PropValue::WithRemovals`]
/// carries one. Returning `Option<&Option<Vec<_>>>` keeps `INDEX_NONE`
/// (`Some(None)`) distinct from "remove nothing" (`None`).
fn split_removals(v: &PropValue) -> (Option<&Option<Vec<PropValue>>>, &PropValue) {
    match v {
        PropValue::WithRemovals { removals, inner } => (Some(removals), inner),
        other => (None, other),
    }
}

/// Emit the `NumElementsToRemove`/`NumKeysToRemove` prefix and its entries.
fn write_removals(
    ar: &mut impl Ar,
    removals: Option<&Option<Vec<PropValue>>>,
    ty: &PropertyType,
    usmap: &Usmap,
) -> Result<()> {
    match removals {
        // `INDEX_NONE` replaces the container wholesale and carries no entries.
        Some(None) => ar.i32(&mut -1),
        Some(Some(items)) => {
            ar.i32(&mut (items.len() as i32))?;
            for e in items {
                write_value(ar, ty, e, true, usmap)?;
            }
            Ok(())
        }
        None => ar.i32(&mut 0),
    }
}

/// Emit one property value, mirroring [`read_value`].
///
/// Covers every *leaf* shape: scalars, names, strings, object indices, soft
/// object paths, delegates, field paths, optionals, containers of those, and
/// natively-sized structs whose bytes we kept verbatim.
///
/// It deliberately does **not** cover a nested reflected struct or a
/// hand-written native struct. Both need a property *block* underneath them, and
/// emitting one byte-exactly needs per-slot presence and zero-masking — which
/// the current `BTreeMap` value shape has already thrown away by the time we get
/// here. That is the `PropertyBlock` model, and it is the next piece of work;
/// until it lands those arms return an error naming what is missing rather than
/// writing something plausible and wrong.
pub(super) fn write_value(
    ar: &mut impl Ar,
    ty: &PropertyType,
    v: &PropValue,
    in_container: bool,
    usmap: &Usmap,
) -> Result<()> {
    /// Widths follow the property's alignment, per `SerializeAsInteger`
    /// (UnversionedPropertySerialization.cpp:234) — the same rule `read_value`
    /// reads by.
    fn int_of(v: &PropValue, what: &str) -> Result<i64> {
        match v {
            PropValue::Int(n) => Ok(*n),
            PropValue::Bool(b) => Ok(*b as i64),
            other => bail!("expected an integer for {what}, have {other:?}"),
        }
    }
    match (ty, v) {
        (PropertyType::Bool, PropValue::Bool(b)) => ar.u8(&mut (*b as u8)),
        (PropertyType::Byte { enum_name: Some(_) }, PropValue::Name(n)) if in_container => {
            ar.fname(&mut n.clone())
        }
        (PropertyType::Byte { .. } | PropertyType::Int8, _) => {
            ar.u8(&mut (int_of(v, "byte")? as u8))
        }
        (PropertyType::Int, _) => ar.i32(&mut (int_of(v, "int32")? as i32)),
        (PropertyType::UInt32, _) => ar.u32(&mut (int_of(v, "uint32")? as u32)),
        (PropertyType::Int16 | PropertyType::UInt16, _) => {
            ar.u16(&mut (int_of(v, "int16")? as u16))
        }
        (PropertyType::Int64 | PropertyType::UInt64, _) => {
            ar.u64(&mut (int_of(v, "int64")? as u64))
        }
        (PropertyType::Float, PropValue::Float(f)) => ar.f32(&mut (*f as f32)),
        (PropertyType::Double, PropValue::Float(f)) => ar.f64(&mut f.to_owned()),
        (PropertyType::Name, PropValue::Name(n)) => ar.fname(&mut n.clone()),
        (PropertyType::Str | PropertyType::Utf8Str | PropertyType::AnsiStr, PropValue::Str(s)) => {
            ar.fstring(&mut s.clone())
        }
        (PropertyType::Enum { inner, .. }, _) => {
            if in_container {
                match v {
                    PropValue::Name(n) => ar.fname(&mut n.clone()),
                    other => bail!("expected an enum name in a container, have {other:?}"),
                }
            } else {
                write_value(ar, inner, v, false, usmap)
            }
        }
        (
            PropertyType::Object
            | PropertyType::WeakObject
            | PropertyType::LazyObject
            | PropertyType::Interface,
            PropValue::Object(i),
        ) => ar.i32(&mut i.to_owned()),
        // `FSoftObjectPath`/`FSoftClassPath` carry a custom serializer that
        // writes their parts back-to-back with no property header, so as a
        // *struct* type they take the same shape as the soft-object property
        // above and share its writer. Missing this arm was 12,770 of the 14,304
        // blocks the first round-trip run refused.
        (
            PropertyType::SoftObject
            | PropertyType::AssetObject
            | PropertyType::Struct(_),
            PropValue::SoftObject(p),
        ) => {
            ar.fname(&mut p.package.clone())?;
            ar.fname(&mut p.asset.clone())?;
            ar.fstring(&mut p.sub_path.clone())
        }
        // A natively sized struct kept its bytes, so it goes back exactly.
        (PropertyType::Struct(name), PropValue::Native(n)) => {
            let mut bytes = n.encode(name)?;
            let size = bytes.len();
            ar.raw(&mut bytes, size)
        }
        (PropertyType::Array(inner), PropValue::Array(items)) => {
            ar.i32(&mut (items.len() as i32))?;
            for e in items {
                write_value(ar, inner, e, true, usmap)?;
            }
            Ok(())
        }
        // A set or map always writes a delta-serialization prefix, and its
        // contents are whatever the reader kept — empty for all but 5 exports
        // in the corpus, `INDEX_NONE` for none of them.
        (PropertyType::Set(inner), v) => {
            let (removals, items) = split_removals(v);
            let items = match items {
                PropValue::Set(a) => a.as_slice(),
                // A `TArray` value in a `TSet` slot is accepted: the two
                // serialize alike, and refusing would break anything that built
                // a set before `PropValue::Set` existed.
                PropValue::Array(a) => a.as_slice(),
                other => bail!("expected set elements, have {other:?}"),
            };
            write_removals(ar, removals, inner, usmap)?;
            ar.i32(&mut (items.len() as i32))?;
            for e in items {
                write_value(ar, inner, e, true, usmap)?;
            }
            Ok(())
        }
        (PropertyType::Map(k, val), v) => {
            let (removals, entries) = split_removals(v);
            let entries = match entries {
                PropValue::Map(m) => m.as_slice(),
                other => bail!("expected map entries, have {other:?}"),
            };
            // A map's removals are *keys*, so they serialize as the key type.
            write_removals(ar, removals, k, usmap)?;
            ar.i32(&mut (entries.len() as i32))?;
            for (key, value) in entries {
                write_value(ar, k, key, true, usmap)?;
                write_value(ar, val, value, true, usmap)?;
            }
            Ok(())
        }
        (PropertyType::Delegate, PropValue::Delegate { object, function }) => {
            ar.i32(&mut object.to_owned())?;
            ar.fname(&mut function.clone())
        }
        (PropertyType::MulticastDelegate, PropValue::MulticastDelegate(list)) => {
            ar.i32(&mut (list.len() as i32))?;
            for (object, function) in list {
                ar.i32(&mut object.to_owned())?;
                ar.fname(&mut function.clone())?;
            }
            Ok(())
        }
        (PropertyType::FieldPath, PropValue::FieldPath { path, owner }) => {
            ar.i32(&mut (path.len() as i32))?;
            for n in path {
                ar.fname(&mut n.clone())?;
            }
            ar.i32(&mut owner.to_owned())
        }
        (PropertyType::Optional(_), PropValue::Unset) => ar.u32(&mut 0),
        (PropertyType::Optional(inner), set) => {
            ar.u32(&mut 1)?;
            write_value(ar, inner, set, true, usmap)
        }
        // Anything the reader declined to interpret goes back as it came.
        (_, PropValue::Raw(b)) => {
            let n = b.len();
            ar.raw(&mut b.clone(), n)
        }
        // `FGameplayTagContainer` writes a plain `int32` count then that many
        // `FGameplayTag`s, each just its `FName` — no property header, so like
        // the soft-object paths above it decodes to a bare value rather than a
        // block and needs its own arm.
        (PropertyType::Struct(name), PropValue::Array(tags))
            if name == "GameplayTagContainer" =>
        {
            ar.i32(&mut (tags.len() as i32))?;
            for t in tags {
                match t {
                    PropValue::Name(n) => ar.fname(&mut n.clone())?,
                    other => bail!("expected a gameplay tag name, have {other:?}"),
                }
            }
            Ok(())
        }
        (PropertyType::Struct(name), PropValue::HandWritten(h)) => h.write(ar, name, usmap),
        // A nested struct: either a real property block, whose header is
        // regenerated from the *nested* class's schema, or a hand-written one,
        // which replays its retained bytes and needs no schema at all.
        (PropertyType::Struct(name), PropValue::Struct(b)) => {
            // The block has to be written against the schema it was *read*
            // against, and there are two possible sources: the `.usmap`, and the
            // `FField` chain recovered from whichever package defines the struct.
            // The block records how many slots that schema had, which is enough
            // to tell them apart — a `UUserDefinedStruct` is in no `.usmap` at
            // all, and some natively-serialized structs are in it with members
            // the reader never used.
            let BlockLayout::Unversioned { schema_len, .. } = b.layout;
            let want = schema_len as usize;
            let from_usmap = flattened_schema(name, usmap).ok().filter(|f| f.len() == want);
            if let Some(flat) = from_usmap {
                return write_block(ar, b, &flat, usmap);
            }
            // Resolve before borrowing `ar` mutably.
            let recovered = ar.resolver().and_then(|p| p.struct_layout(name));
            if let Some(fields) = recovered {
                let schema: Vec<(&UsmapProperty, u8, &str)> = fields
                    .iter()
                    .flat_map(|f| (0..f.array_dim.max(1)).map(move |i| (f, i, name.as_str())))
                    .collect();
                if schema.len() == want {
                    return write_block(ar, b, &schema, usmap);
                }
            }
            // Neither matched: fall back to the `.usmap` so `write_block`'s own
            // check reports the disagreement with both lengths named.
            let flat = flattened_schema(name, usmap)?;
            write_block(ar, b, &flat, usmap).with_context(|| format!("struct {name}"))
        }
        // `FText::Serialize` is hand-written; it is typed now, not a span.
        (PropertyType::Text, PropValue::HandWritten(h)) => h.write(ar, "Text", usmap),
        (t, other) => bail!("cannot write {other:?} as {t:?}"),
    }
}

/// Whether a property of this type *may* be zero-masked at all.
///
/// `CanSerializeAsZero` (UnversionedPropertySerialization.cpp:189). A zero-masked
/// property serializes no bytes and `LoadZero` memzeroes its storage, so the
/// engine only allows it where that is a faithful reconstruction: the property
/// must need no destructor and be zero-constructible, a `bool` must be a real
/// `bool` rather than a bitfield, and a struct must be `STRUCT_Atomic` and
/// small. Everything with a heap allocation behind it — strings, containers,
/// text, delegates, field paths, soft object paths — is excluded, which matches
/// the masked population measured across the shipped corpus exactly.
pub fn can_serialize_as_zero(ty: &PropertyType) -> bool {
    match ty {
        PropertyType::Bool
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { .. }
        | PropertyType::Float
        | PropertyType::Double
        | PropertyType::Name
        | PropertyType::Object
        | PropertyType::WeakObject
        | PropertyType::LazyObject
        | PropertyType::Interface => true,
        PropertyType::Enum { inner, .. } => can_serialize_as_zero(inner),
        // The engine's test is `STRUCT_Atomic` and under sixteen words, and the
        // `.usmap` records neither — so this cannot be derived, only permitted.
        // Having a fixed native size is *sufficient* but not necessary:
        // `FBox2f` is atomic and small yet serializes as an ordinary property
        // block when it is non-zero, so it is absent from `native_struct_size`
        // and was refused here. The cooker masks it, the reader materialises an
        // empty block for it, and the writer then could not put it back.
        //
        // Answering `true` is safe because the caller ANDs this with the bit the
        // file itself carries (see `write_block`): a struct is only ever masked
        // where the cooker masked it. Answering `false` was not safe — it turned
        // nine data tables into a refusal.
        PropertyType::Struct(_) => true,
        // An optional inherits its inner type's flags.
        PropertyType::Optional(inner) => can_serialize_as_zero(inner),
        _ => false,
    }
}

/// Whether this value would be written as a zero mask bit rather than as bytes.
///
/// `ShouldSaveAsZero` (UnversionedPropertySerialization.cpp:149). The engine
/// **derives this from the value at save time** — it is not carried in the file
/// and cannot be. Replaying the bit we read instead is indistinguishable for an
/// unmodified block, and wrong the moment a value is edited: setting a property
/// to zero must start masking it, and clearing a mask must start emitting bytes.
///
/// The test is on the *bytes*, which is why it is done by serializing rather
/// than by comparing values: `IsIntZero` memcmps the property's storage, so
/// negative zero is not zero, and for every zero-maskable type the serialized
/// form and the in-memory form coincide.
pub fn should_save_as_zero(ty: &PropertyType, v: &PropValue, usmap: &Usmap) -> bool {
    if !can_serialize_as_zero(ty) {
        return false;
    }
    // A masked struct is read as an *empty* block — `zero_value` builds one, and
    // no bytes were consumed. Serializing it to test for zeroes would need the
    // nested schema and would fail; emptiness is the test.
    if let (PropertyType::Struct(_), PropValue::Struct(b)) = (ty, v) {
        return b.entries.is_empty();
    }
    let mut w = Writer::new();
    if write_value(&mut w, ty, v, false, usmap).is_err() {
        return false;
    }
    let b = w.into_bytes();
    !b.is_empty() && b.iter().all(|&x| x == 0)
}

/// Emit one property value's bytes, given the schema type it is declared as.
///
/// The byte-slice counterpart to [`read_value`], and the write half of this
/// layer's public surface alongside
/// [`emit_header`](super::block::emit_header). Errors — rather than guessing —
/// for the shapes that still need the `PropertyBlock` model: a nested reflected
/// struct, a hand-written native struct, and `FText`.
pub fn emit_value(ty: &PropertyType, v: &PropValue, usmap: &Usmap) -> Result<Vec<u8>> {
    let mut w = Writer::new();
    write_value(&mut w, ty, v, false, usmap)?;
    Ok(w.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::value::{FName, SoftObjectPath};
    use super::super::usmap::Usmap;

    /// Write a value, read it back through the same schema type, and require
    /// the bytes to be identical on a second write. Comparing bytes rather than
    /// values is deliberate: it is the property the writer actually has to have,
    /// and it does not depend on `PropValue` implementing equality.
    fn round_trip(ty: &PropertyType, v: &PropValue, in_container: bool, names: &[String]) {
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let mut w = Writer::new();
        write_value(&mut w, ty, v, in_container, &usmap).expect("write");
        let first = w.into_bytes();

        let mut r = Reader::new(&first, names);
        let back = read_value(&mut r, ty, &usmap, 0, in_container).expect("read back");
        assert_eq!(r.o, first.len(), "reader consumed {} of {} bytes for {ty:?}", r.o, first.len());

        let mut w2 = Writer::new();
        write_value(&mut w2, ty, &back, in_container, &usmap).expect("re-write");
        assert_eq!(first, w2.into_bytes(), "value did not survive a round trip: {ty:?} = {v:?}");
    }

    #[test]
    fn leaf_values_round_trip() {
        let names: Vec<String> = vec!["None".into(), "Rocket".into(), "Fire".into()];
        let n = |i: u32, num: u32, t: &str| FName::new(i, num, t);

        round_trip(&PropertyType::Bool, &PropValue::Bool(true), false, &names);
        round_trip(&PropertyType::Int, &PropValue::Int(-42), false, &names);
        round_trip(&PropertyType::UInt32, &PropValue::Int(0xDEAD_BEEF), false, &names);
        round_trip(&PropertyType::Int64, &PropValue::Int(i32::MIN as i64), false, &names);
        round_trip(&PropertyType::Float, &PropValue::Float(0.25), false, &names);
        round_trip(&PropertyType::Double, &PropValue::Float(-2.5), false, &names);
        round_trip(&PropertyType::Name, &PropValue::Name(n(1, 5, "Rocket_4")), false, &names);
        round_trip(&PropertyType::Str, &PropValue::Str("SK_Marine".into()), false, &names);
        round_trip(&PropertyType::Object, &PropValue::Object(-6), false, &names);
        round_trip(
            &PropertyType::SoftObject,
            &PropValue::SoftObject(SoftObjectPath {
                package: n(1, 0, "Rocket"),
                asset: n(2, 0, "Fire"),
                sub_path: "Sub".into(),
            }),
            false,
            &names,
        );
        round_trip(
            &PropertyType::Delegate,
            &PropValue::Delegate { object: -3, function: n(2, 0, "Fire") },
            false,
            &names,
        );
        round_trip(
            &PropertyType::MulticastDelegate,
            &PropValue::MulticastDelegate(vec![(1, n(1, 0, "Rocket")), (2, n(2, 0, "Fire"))]),
            false,
            &names,
        );
        round_trip(
            &PropertyType::FieldPath,
            &PropValue::FieldPath { path: vec![n(1, 0, "Rocket")], owner: 9 },
            false,
            &names,
        );
        round_trip(
            &PropertyType::Array(Box::new(PropertyType::Int)),
            &PropValue::Array(vec![PropValue::Int(1), PropValue::Int(2)]),
            false,
            &names,
        );
        round_trip(
            &PropertyType::Set(Box::new(PropertyType::Int)),
            &PropValue::Array(vec![PropValue::Int(7)]),
            false,
            &names,
        );
        round_trip(
            &PropertyType::Map(Box::new(PropertyType::Int), Box::new(PropertyType::Int)),
            &PropValue::Map(vec![(PropValue::Int(1), PropValue::Int(2))]),
            false,
            &names,
        );
        // A natively sized struct goes back byte for byte.
        round_trip(
            &PropertyType::Struct("Guid".into()),
            &PropValue::Native(NativeStruct::decode("Guid", &(0u8..16).collect::<Vec<u8>>()).unwrap()),
            false,
            &names,
        );
    }

    /// A container's delta-serialization prefix must survive, contents and all.
    ///
    /// The count used to be read and its entries dropped, which is invisible
    /// while the count is zero — as it is for all but 5 of the 1,153,836 exports
    /// — and made exactly those 5 unwritable.
    #[test]
    fn container_removals_round_trip() {
        let names: Vec<String> = vec!["None".into()];
        let set = PropertyType::Set(Box::new(PropertyType::Int));
        round_trip(
            &set,
            &PropValue::WithRemovals {
                removals: Some(vec![PropValue::Int(7), PropValue::Int(9)]),
                inner: Box::new(PropValue::Array(vec![PropValue::Int(1)])),
            },
            false,
            &names,
        );
        // `INDEX_NONE` — replace wholesale — is a different instruction from
        // "remove nothing" and must not flatten into it.
        round_trip(
            &set,
            &PropValue::WithRemovals {
                removals: None,
                inner: Box::new(PropValue::Array(vec![PropValue::Int(1)])),
            },
            false,
            &names,
        );
        let map = PropertyType::Map(Box::new(PropertyType::Int), Box::new(PropertyType::Int));
        round_trip(
            &map,
            &PropValue::WithRemovals {
                removals: Some(vec![PropValue::Int(3)]),
                inner: Box::new(PropValue::Map(vec![(PropValue::Int(1), PropValue::Int(2))])),
            },
            false,
            &names,
        );
    }

    /// An empty removal prefix leaves the value unwrapped, so the shape 1,153,831
    /// blocks decode to is exactly what it was before removals were modeled.
    #[test]
    fn an_empty_removal_prefix_does_not_wrap() {
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let ty = PropertyType::Set(Box::new(PropertyType::Int));
        let mut w = Writer::new();
        write_value(&mut w, &ty, &PropValue::Array(vec![PropValue::Int(4)]), false, &usmap)
            .expect("write");
        let bytes = w.into_bytes();
        let mut r = Reader::new(&bytes, &[]);
        let back = read_value(&mut r, &ty, &usmap, 0, false).expect("read");
        assert!(
            matches!(back, PropValue::Set(ref a) if a.len() == 1),
            "an empty prefix should not produce a wrapper: {back:?}"
        );
    }

    /// An unset optional writes only its four-byte flag, and stays unset.
    #[test]
    fn unset_optional_round_trips() {
        let names: Vec<String> = vec!["None".into()];
        let ty = PropertyType::Optional(Box::new(PropertyType::Int));
        let usmap = Usmap::meteorite().unwrap();
        let mut w = Writer::new();
        write_value(&mut w, &ty, &PropValue::Unset, false, &usmap).unwrap();
        let bytes = w.into_bytes();
        assert_eq!(bytes, [0, 0, 0, 0]);
        let mut r = Reader::new(&bytes, &names);
        assert!(matches!(read_value(&mut r, &ty, &usmap, 0, false).unwrap(), PropValue::Unset));
    }

    /// A nested reflected struct now writes a real block rather than refusing.
    /// An empty one still has to emit its class's schema length as skips — the
    /// case that is easiest to encode as "nothing at all" and wrong for 62,606
    /// exports.
    #[test]
    fn nested_reflected_struct_writes_its_block() {
        use super::super::value::{BlockLayout, PropertyBlock};
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let schema_len = flattened_schema("Transform", &usmap).unwrap().len();
        let block = PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned { schema_len: schema_len as u32, leading_empty: 0 },
        };
        let mut w = Writer::new();
        write_value(
            &mut w,
            &PropertyType::Struct("Transform".into()),
            &PropValue::Struct(block),
            false,
            &usmap,
        )
        .expect("an empty nested block is writable");
        assert_eq!(
            w.into_bytes(),
            super::super::block::emit_header(&Default::default(), schema_len),
            "an empty block must still encode its schema length as skips"
        );
    }

    /// A block carrying a schema length that disagrees with the schema it is
    /// written against would emit a silently wrong header, so it is refused.
    #[test]
    fn schema_length_disagreement_is_refused() {
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let mut w = Writer::new();
        let e = write_value(
            &mut w,
            &PropertyType::Struct("Transform".into()),
            &PropValue::Struct(Default::default()),
            false,
            &usmap,
        )
        .unwrap_err();
        // The whole chain, not just the outermost context: the refusal now names
        // the struct on the outside and the length disagreement underneath.
        let e = format!("{e:#}");
        assert!(e.contains("schema"), "unhelpful refusal: {e}");
        assert!(e.contains("Transform"), "refusal does not name the struct: {e}");
    }

}
