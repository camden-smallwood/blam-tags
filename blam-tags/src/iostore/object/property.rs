//! One property value, by its schema type.
//!
//! `in_container` distinguishes the two ways UE reaches a value, which matters
//! for enums and only for enums — see [`read_value`].

use anyhow::{bail, Context, Result};

use super::archive::{Ar, Reader, Writer};
use super::block::{flattened_schema, read_struct, write_block};
use super::usmap::{PropertyType, Usmap};
use super::value::{BlockLayout, PropValue, SoftObjectPath};
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
        (PropertyType::Struct(name), PropValue::Native(b)) => {
            let size = native_struct_size(name)
                .with_context(|| format!("no native size for struct {name}"))?;
            ar.raw(&mut b.clone(), size)
        }
        (PropertyType::Array(inner), PropValue::Array(items)) => {
            ar.i32(&mut (items.len() as i32))?;
            for e in items {
                write_value(ar, inner, e, true, usmap)?;
            }
            Ok(())
        }
        // The removal prefix is reconstructible from the schema — a set or map
        // always writes one — but its *contents* were not retained. Five exports
        // in the shipped corpus have a non-empty one; they will surface as
        // round-trip failures rather than as silently wrong bytes.
        (PropertyType::Set(inner), PropValue::Array(items)) => {
            ar.i32(&mut 0)?;
            ar.i32(&mut (items.len() as i32))?;
            for e in items {
                write_value(ar, inner, e, true, usmap)?;
            }
            Ok(())
        }
        (PropertyType::Map(k, val), PropValue::Map(entries)) => {
            ar.i32(&mut 0)?;
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
        // A nested struct: either a real property block, whose header is
        // regenerated from the *nested* class's schema, or a hand-written one,
        // which replays its retained bytes and needs no schema at all.
        (PropertyType::Struct(name), PropValue::Struct(b)) => match &b.layout {
            BlockLayout::Native { .. } => write_block(ar, b, &[], usmap),
            BlockLayout::Unversioned { .. } => {
                let flat = flattened_schema(name, usmap)?;
                write_block(ar, b, &flat, usmap)
            }
        },
        // `FText::Serialize` is hand-written, so it arrives as a native block.
        (PropertyType::Text, PropValue::Struct(b)) => write_block(ar, b, &[], usmap),
        (t, other) => bail!("cannot write {other:?} as {t:?}"),
    }
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
            &PropValue::Native((0u8..16).collect()),
            false,
            &names,
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
        .unwrap_err()
        .to_string();
        assert!(e.contains("schema"), "unhelpful refusal: {e}");
    }

    /// A hand-written struct replays the exact bytes it consumed, so it round
    /// trips before its layout is modeled — and needs no schema to do it.
    #[test]
    fn native_layout_block_replays_its_bytes() {
        use super::super::value::{BlockLayout, PropertyBlock};
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let bytes: Vec<u8> = (0u8..9).collect();
        let block = PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Native { name: "ShaderValueTypeHandle".into(), bytes: bytes.clone() },
        };
        let mut w = Writer::new();
        write_value(
            &mut w,
            &PropertyType::Struct("ShaderValueTypeHandle".into()),
            &PropValue::Struct(block),
            false,
            &usmap,
        )
        .expect("a native block writes its span");
        assert_eq!(w.into_bytes(), bytes);
    }
}
