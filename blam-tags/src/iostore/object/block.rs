//! The `FUnversionedHeader` codec and the walk over one property block.
//!
//! A cooked property block is a run of `skip`/`value` fragments plus an optional
//! zero mask, then the present values back to back. `FUnversionedHeaderBuilder`
//! (UnversionedPropertySerialization.cpp:795) is fully deterministic, so this
//! block can be *regenerated* rather than retained — verified byte-exact against
//! every export in the shipped corpus.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::archive::{trace_enabled, Reader};
use super::usmap::{PropertyType, Usmap, UsmapProperty};
use super::limits::MAX_DEPTH;
use super::value::{FName, PropValue};
use super::property::read_value;
use super::structs::native_struct_size;

/// One `FUnversionedHeader` fragment (`FFragment`,
/// UnversionedPropertySerialization.cpp:621).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Fragment {
    skip: u8,
    has_zeroes: bool,
    value_num: u8,
    is_last: bool,
}

impl Fragment {
    const SKIP_MAX: u8 = 127;
    const VALUE_MAX: u8 = 127;

    fn unpack(p: u16) -> Self {
        Fragment {
            skip: (p & 0x7f) as u8,
            has_zeroes: (p & 0x80) != 0,
            is_last: (p & 0x100) != 0,
            value_num: (p >> 9) as u8,
        }
    }

}

/// What a property block's header says.
///
/// `present` is the schema indices carrying a value, each paired with whether
/// that value is non-zero; a zero-masked property serializes no bytes at all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Header {
    pub present: Vec<(usize, bool)>,
    /// Empty `(skip 0, value 0)` fragments before the first real one.
    ///
    /// `FUnversionedHeaderBuilder` cannot emit these, and nothing Epic cooked
    /// has them — but every one of Campaign Evolved's 12,161 `/Game/Tags`
    /// wrappers begins with exactly two, so i343's tag tool writes its own
    /// headers. UE's loader skips them (`FIterator::Skip` walks past
    /// `ValueNum == 0`), so they are inert; carrying the count is what lets us
    /// reproduce those packages byte-for-byte rather than merely equivalently.
    pub leading_empty: u8,
}

/// Read an `FUnversionedHeader`, returning `(present_schema_indices, ...)`
/// where each present index is paired with whether its value is non-zero (a
/// zero-masked property serializes no bytes — it is the zero value).
pub(super) fn read_header(r: &mut Reader) -> Result<Header> {
    let mut frags = Vec::new();
    let mut zero_mask_num = 0usize;
    let mut leading_empty = 0u8;
    let mut seen_non_empty = false;
    loop {
        let packed = r.u16()?;
        let frag = Fragment::unpack(packed);
        // A wholly empty fragment before any real one is CE tag-wrapper
        // padding (see `Header::leading_empty`). `is_last` terminates the
        // header, so such a fragment is never a lead-in even when it is bare.
        if packed == 0 && !seen_non_empty {
            leading_empty = leading_empty.saturating_add(1);
        } else {
            seen_non_empty = true;
        }
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
    Ok(Header { present, leading_empty })
}


/// Read a full reflected struct/class instance (its unversioned property
/// block) named `class`, returning present property name→value.
pub(super) fn read_struct(r: &mut Reader, class: &str, usmap: &Usmap, depth: usize) -> Result<BTreeMap<String, PropValue>> {
    if depth > MAX_DEPTH {
        bail!("unversioned struct nesting too deep at {class}");
    }
    // A `UUserDefinedStruct` used as a property type is in no `.usmap` under
    // any name; its layout has to come back out of the package that defines it.
    if usmap.get(class).is_none() {
        if let Some(fields) = r.resolver.and_then(|p| p.struct_layout(class)) {
            // A recovered `FField` chain occupies `array_dim` slots too, the
            // same as a `.usmap` one.
            let schema: Vec<(&UsmapProperty, u8)> = fields
                .iter()
                .flat_map(|f| (0..f.array_dim.max(1)).map(move |i| (f, i)))
                .collect();
            return read_struct_with_schema(r, class, &schema, usmap, depth);
        }
    }
    let flat = flattened_schema(class, usmap)?;
    read_struct_with_schema(r, class, &flat, usmap, depth)
}

/// The flattened property schema the unversioned fragment stream indexes by,
/// each slot paired with its index within a static array.
pub(super) fn flattened_schema<'u>(
    class: &str,
    usmap: &'u Usmap,
) -> Result<Vec<(&'u UsmapProperty, u8)>> {
    // Campaign Evolved ships `Blam*TagDataAsset` classes that appear in neither
    // the `.usmap` nor the UHT dump (`BlamFrameEventListTagDataAsset` alone
    // covers 130 exports). They add no properties of their own over the shared
    // base, so decoding them against `BlamTagDataAssetBase` recovers the whole
    // property block rather than failing outright.
    usmap
        .flattened_slots(class)
        .or_else(|| {
            (class.starts_with("Blam") && class.ends_with("TagDataAsset"))
                .then(|| usmap.flattened_slots("BlamTagDataAssetBase"))
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
pub(super) fn read_struct_with_schema(
    r: &mut Reader,
    label: &str,
    flat: &[(&UsmapProperty, u8)],
    usmap: &Usmap,
    depth: usize,
) -> Result<BTreeMap<String, PropValue>> {
    let class = label;
    let header_start = r.o;
    let header = read_header(r)?;
    if trace_enabled() {
        eprintln!(
            "{:indent$}{class} @ {header_start} (header {} bytes, present {:?})",
            "",
            r.o - header_start,
            header.present,
            indent = depth * 2
        );
    }
    let mut out = BTreeMap::new();
    for (idx, non_zero) in header.present {
        let (prop, slot) = *flat
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
                "{:indent$}  [{idx}] {}{} : {:?} @ {start}..{}{}",
                "",
                prop.name,
                if prop.array_dim > 1 { format!("[{slot}]") } else { String::new() },
                prop.ty,
                r.o,
                if non_zero { "" } else { " (zero-masked)" },
                indent = depth * 2
            );
        }
        let dim = prop.array_dim.max(1);
        if dim == 1 {
            out.insert(prop.name.clone(), value);
        } else {
            // A static array's slots are independent schema entries, each
            // present or absent on its own. Keeping them under one name as an
            // array is what stops the last one overwriting its siblings; slots
            // the block never mentions stay `Unset` rather than being invented.
            let entry = out
                .entry(prop.name.clone())
                .or_insert_with(|| PropValue::Array(vec![PropValue::Unset; dim as usize]));
            match entry {
                PropValue::Array(slots) if (slot as usize) < slots.len() => {
                    slots[slot as usize] = value;
                }
                // Two properties sharing a name within one flattened schema —
                // a shadowed field. Last write wins, as before.
                other => *other = value,
            }
        }
    }
    Ok(out)
}

/// The implicit value of a zero-masked property, which serialized no bytes.
///
/// `LoadZero` (UnversionedPropertySerialization.cpp:122) memzeroes the
/// property's storage, so the value is whatever all-zero bytes mean for that
/// type — a real value, not an absence. Returning "unknown" here threw away
/// **1,501,709** values across the shipped corpus (1,280,279 enums and 221,430
/// names), and did so invisibly, because the value being lost is the default
/// one.
///
/// Only types `CanSerializeAsZero` accepts can appear here at all: it demands
/// `CPF_ZeroConstructor | CPF_NoDestructor`, or `STRUCT_Atomic` for a struct.
/// Measured over the whole corpus, the zero-masked population is exactly enums,
/// names and twelve small atomic structs — never a string, array, map, set,
/// text, delegate, soft object or field path, all of which have destructors.
pub(super) fn zero_value(ty: &PropertyType) -> PropValue {
    match ty {
        PropertyType::Bool => PropValue::Bool(false),
        PropertyType::Int
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { enum_name: None } => PropValue::Int(0),
        PropertyType::Float | PropertyType::Double => PropValue::Float(0.0),
        PropertyType::Object
        | PropertyType::Interface
        | PropertyType::WeakObject
        | PropertyType::LazyObject => PropValue::Object(0),
        // An `FName` of index 0 is `NAME_None`, whatever the package name map
        // holds at that slot — the zero is the engine's, not the package's.
        PropertyType::Name => PropValue::Name(FName::none()),
        // A zero enum is its underlying integer's zero. A `TEnumAsByte` naming
        // an enum lands here too, via `FByteProperty`.
        PropertyType::Enum { inner, .. } => zero_value(inner),
        PropertyType::Byte { enum_name: Some(_) } => PropValue::Int(0),
        // An atomic struct's zero is that many zero bytes, which keeps
        // `MeshTransform::from_prop` and friends working on defaulted values
        // instead of seeing an opaque hole.
        PropertyType::Struct(name) => match native_struct_size(name) {
            Some(size) => PropValue::Native(vec![0; size]),
            None => PropValue::Struct(BTreeMap::new()),
        },
        PropertyType::Optional(_) => PropValue::Unset,
        // Everything else is not zero-maskable per `CanSerializeAsZero`, so
        // reaching this arm means either a schema we have misread or an engine
        // change. An empty byte span is the honest answer: no bytes were
        // serialized, and we decline to invent a value.
        _ => PropValue::Raw(Vec::new()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    use super::super::export::{read_export_struct, read_export_struct_len};
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

    /// A `UPROPERTY` declared `Thing[N]` occupies N consecutive schema slots,
    /// each independently present. Keying the output on property name alone
    /// collapses all N into whichever came last — silently, because the result
    /// is a perfectly plausible single value. 76 classes and 42,172 exports in
    /// the shipped corpus have at least one.
    #[test]
    fn static_array_slots_do_not_overwrite_each_other() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "StaticArrayProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 4,
                    name: "Tints".to_string(),
                    ty: PropertyType::Int,
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "After".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        // Five values present (four array slots then `After`), none zero.
        export.extend_from_slice(&(((5u16) << 9) | 0x0100).to_le_bytes());
        for v in [11i32, 22, 33, 44, 0x5EED] {
            export.extend_from_slice(&v.to_le_bytes());
        }

        let (props, used) =
            read_export_struct_len(&export, &[], &usmap, "StaticArrayProbe").expect("decode");
        assert_eq!(used, export.len(), "walk did not consume the whole block");
        match props.get("Tints") {
            Some(PropValue::Array(v)) => {
                let got: Vec<i64> = v
                    .iter()
                    .map(|e| match e {
                        PropValue::Int(n) => *n,
                        other => panic!("expected ints in the static array, got {other:?}"),
                    })
                    .collect();
                assert_eq!(got, vec![11, 22, 33, 44]);
            }
            other => panic!("expected a 4-element static array, got {other:?}"),
        }
        assert!(matches!(props.get("After"), Some(PropValue::Int(0x5EED))));
    }

    /// Slots the block never mentions stay `Unset` rather than being given an
    /// invented value — absent is not the same as zero.
    #[test]
    fn absent_static_array_slots_stay_unset() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SparseArrayProbe",
            None,
            vec![UsmapProperty {
                schema_index: 0,
                array_dim: 3,
                name: "Slots".to_string(),
                ty: PropertyType::Int,
            }],
        );
        let mut export = Vec::new();
        // Skip slot 0, then one value for slot 1, and nothing for slot 2.
        export.extend_from_slice(&(((1u16) << 9) | 0x0100 | 1).to_le_bytes());
        export.extend_from_slice(&7i32.to_le_bytes());
        let props = read_export_struct(&export, &[], &usmap, "SparseArrayProbe").expect("decode");
        match props.get("Slots") {
            Some(PropValue::Array(v)) => {
                assert!(matches!(v[0], PropValue::Unset), "slot 0 was absent");
                assert!(matches!(v[1], PropValue::Int(7)));
                assert!(matches!(v[2], PropValue::Unset), "slot 2 was absent");
            }
            other => panic!("expected a 3-element static array, got {other:?}"),
        }
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

#[cfg(test)]
mod zero_value_tests {
    use super::*;

    /// A zero-masked property is a *value*, not an absence. Returning an opaque
    /// placeholder for the non-scalar cases discarded 1,501,709 values across
    /// the shipped corpus — invisibly, because what goes missing is the default.
    #[test]
    fn zero_masked_names_and_enums_keep_their_value() {
        assert!(
            matches!(zero_value(&PropertyType::Name), PropValue::Name(n) if n == "None"),
            "a zero FName is NAME_None, not an unknown"
        );
        let e = PropertyType::Enum {
            inner: Box::new(PropertyType::Byte { enum_name: None }),
            enum_name: "EFoo".to_string(),
        };
        assert!(matches!(zero_value(&e), PropValue::Int(0)));
        assert!(matches!(
            zero_value(&PropertyType::Byte { enum_name: Some("EFoo".into()) }),
            PropValue::Int(0)
        ));
    }

    /// An atomic struct's zero is that many zero bytes, so decoders that read a
    /// native blob (transforms, GUIDs) still work on a defaulted value.
    #[test]
    fn zero_masked_atomic_structs_are_zero_bytes() {
        match zero_value(&PropertyType::Struct("Guid".to_string())) {
            PropValue::Native(b) => assert_eq!(b, vec![0u8; 16]),
            other => panic!("expected 16 zero bytes for a zero FGuid, got {other:?}"),
        }
        match zero_value(&PropertyType::Struct("Vector".to_string())) {
            PropValue::Native(b) => assert_eq!(b.len(), 24),
            other => panic!("expected a 24-byte zero FVector, got {other:?}"),
        }
    }

    /// Types `CanSerializeAsZero` rejects should never be zero-masked; if one
    /// appears we say so with an empty span rather than inventing a value.
    #[test]
    fn non_zeroable_types_do_not_get_invented_values() {
        assert!(matches!(zero_value(&PropertyType::Str), PropValue::Raw(b) if b.is_empty()));
        assert!(matches!(
            zero_value(&PropertyType::Array(Box::new(PropertyType::Int))),
            PropValue::Raw(b) if b.is_empty()
        ));
    }
}
