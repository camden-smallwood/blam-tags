//! Changing a decoded export and making the change survive a rebuild.
//!
//! Everything else in this layer is symmetric — read a thing, write the same
//! thing back. Editing is the asymmetric case, and it has one requirement the
//! round-trip path never hits: a value may need a *name that is not in the
//! package yet*. An `FName` is an index into the package's name map, so setting
//! a name property to a new string means growing that map and using the index it
//! comes back with. Handing the writer an `FName` whose index points at nothing
//! is the obvious failure, and a silent one — the index is just a number.

use anyhow::{Context, Result, bail};

use std::sync::Arc;

use super::block::{flattened_schema, zero_value};
use super::hand_written::{HandWritten, TextHistory, TextValue};
use super::usmap::{PropertyType, Usmap};
use super::value::{
    BlockLayout, FName, PropValue, PropertyBlock, PropertyEntry, SchemaSlot, SoftObjectPath,
};
use crate::iostore::package::imports::{
    ImportSlot, ImportTarget, import_package_index, import_slot_of, read_import_slots,
    write_import_slots,
};
use crate::iostore::package::name_map::FNameMap;
use crate::iostore::package::zen::FZenPackageHeader;

/// Resolve `text` against a package's name map, appending it if absent, and
/// return the [`FName`] a property can hold.
///
/// Splitting a trailing `_N` into an instance number is what the engine does
/// when it interns a name, so authoring `Rocket_4` produces base `Rocket` with
/// number 5 — the same bytes the cooker would have written. That is the *right*
/// behaviour here, and the opposite of the read path, which keeps whatever
/// split the file already chose rather than re-deriving one.
pub fn intern_name(name_map: &mut FNameMap, text: &str) -> FName {
    let mapped = name_map.store(text);
    FName::new(mapped.index(), mapped.number, text)
}

/// Resolve the declared type for an entry in a decoded property block.
///
/// The value alone is deliberately insufficient: every integer width and enum
/// collapses to [`PropValue::Int`], object flavours share one representation,
/// and an empty container carries no element from which an editor could infer
/// its type.  Editors should use this before choosing a widget or constructing
/// a new container element.
pub fn property_type_for_slot(
    class: &str,
    slot: SchemaSlot,
    usmap: &Usmap,
) -> Result<PropertyType> {
    let flat = flattened_schema(class, usmap)?;
    let (property, array_index, _) = flat
        .get(slot.index as usize)
        .ok_or_else(|| anyhow::anyhow!("class {class} has no schema slot {}", slot.index))?;
    if *array_index != slot.array_index {
        bail!(
            "class {class} schema slot {} is static-array index {}, not {}",
            slot.index,
            array_index,
            slot.array_index
        );
    }
    Ok(property.ty.clone())
}

/// Every editable reflected slot declared for a class, in serialized schema
/// order. This is the authoritative catalog an editor should use when it
/// offers to materialize a cooker-omitted property or static-array element.
pub fn editable_schema_slots(
    class: &str,
    usmap: &Usmap,
) -> Result<Vec<(String, SchemaSlot, PropertyType)>> {
    let flat = flattened_schema(class, usmap)?;
    Ok(flat
        .iter()
        .enumerate()
        .map(|(index, (property, array_index, _))| {
            (
                property.name.clone(),
                SchemaSlot {
                    index: index as u32,
                    array_index: *array_index,
                    zero_masked: false,
                },
                property.ty.clone(),
            )
        })
        .collect())
}

/// Construct the canonical editable default for a declared property type.
///
/// This differs from the private zero-mask helper for destructor-bearing
/// values: those can never be zero-masked, but an editor still needs a valid
/// empty string/container/reference when the user adds an element.  Unknown
/// property kinds are refused rather than represented by invented bytes.
pub fn default_value_for_type(ty: &PropertyType, usmap: &Usmap) -> Result<PropValue> {
    Ok(match ty {
        PropertyType::Bool
        | PropertyType::Int
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Float
        | PropertyType::Double
        | PropertyType::Object
        | PropertyType::Interface
        | PropertyType::WeakObject
        | PropertyType::LazyObject
        | PropertyType::Name
        | PropertyType::Byte { .. }
        | PropertyType::Enum { .. } => zero_value(ty),
        PropertyType::AssetObject => PropValue::Object(0),
        PropertyType::Str | PropertyType::Utf8Str | PropertyType::AnsiStr => {
            PropValue::Str(Default::default())
        }
        PropertyType::SoftObject => PropValue::SoftObject(SoftObjectPath::default()),
        PropertyType::Array(_) => PropValue::Array(Vec::new()),
        PropertyType::Set(_) => PropValue::Set(Vec::new()),
        PropertyType::Map(_, _) => PropValue::Map(Vec::new()),
        PropertyType::Delegate => PropValue::Delegate {
            object: 0,
            function: FName::none(),
        },
        PropertyType::MulticastDelegate => PropValue::MulticastDelegate(Vec::new()),
        PropertyType::FieldPath => PropValue::FieldPath {
            path: Vec::new(),
            owner: 0,
        },
        PropertyType::Optional(_) => PropValue::Unset,
        PropertyType::Text => PropValue::HandWritten(HandWritten::Text(TextValue {
            flags: 0,
            history: TextHistory::None {
                culture_invariant: None,
            },
        })),
        PropertyType::Struct(name) => match zero_value(ty) {
            PropValue::Struct(_) => {
                let schema_len = usmap
                    .flattened_slots(name)
                    .with_context(|| format!("USMAP has no struct {name}"))?
                    .len() as u32;
                PropValue::Struct(PropertyBlock {
                    entries: Vec::new(),
                    layout: BlockLayout::Unversioned {
                        schema_len,
                        leading_empty: 0,
                    },
                })
            }
            value => value,
        },
        PropertyType::Unknown(kind) => {
            bail!("USMAP property kind {kind} is unknown; Chimp will preserve it read-only")
        }
    })
}

/// Check that an edited value still has the representation required by its
/// declared type.  Writers perform the final byte-level validation; this gives
/// UI callers a cheap, deterministic error before they mark a document dirty.
pub fn validate_value_for_type(ty: &PropertyType, value: &PropValue) -> Result<()> {
    use PropertyType as T;
    let valid = match (ty, value) {
        (T::Bool, PropValue::Bool(_)) => true,
        (
            T::Byte { .. }
            | T::Int
            | T::UInt64
            | T::UInt32
            | T::UInt16
            | T::Int64
            | T::Int16
            | T::Int8,
            PropValue::Int(_),
        ) => true,
        (T::Enum { inner, .. }, value) => return validate_value_for_type(inner, value),
        (T::Float | T::Double, PropValue::Float(_)) => true,
        (T::Name, PropValue::Name(_)) => true,
        (T::Str | T::Utf8Str | T::AnsiStr, PropValue::Str(_)) => true,
        (
            T::Object | T::Interface | T::WeakObject | T::LazyObject | T::AssetObject,
            PropValue::Object(_),
        ) => true,
        (T::SoftObject, PropValue::SoftObject(_)) => true,
        (T::Array(inner), PropValue::Array(values)) | (T::Set(inner), PropValue::Set(values)) => {
            for item in values {
                validate_value_for_type(inner, item)?;
            }
            true
        }
        (T::Map(key, item), PropValue::Map(values)) => {
            for (k, v) in values {
                validate_value_for_type(key, k)?;
                validate_value_for_type(item, v)?;
            }
            true
        }
        (T::Array(_) | T::Set(_) | T::Map(_, _), PropValue::WithRemovals { inner, .. }) => {
            return validate_value_for_type(ty, inner);
        }
        (T::Struct(_), PropValue::Struct(_) | PropValue::Native(_) | PropValue::HandWritten(_)) => {
            true
        }
        (T::Text, PropValue::HandWritten(HandWritten::Text(_))) => true,
        (T::Delegate, PropValue::Delegate { .. }) => true,
        (T::MulticastDelegate, PropValue::MulticastDelegate(_)) => true,
        (T::FieldPath, PropValue::FieldPath { .. }) => true,
        (T::Optional(_), PropValue::Unset) => true,
        (T::Optional(inner), value) => return validate_value_for_type(inner, value),
        (T::Unknown(_), PropValue::Raw(_)) => true,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        bail!("value {value:?} does not match declared property type {ty:?}")
    }
}

/// Set a property, **inserting it when the block does not carry it**.
///
/// The cooker omits any property whose value equals its class default
/// (`IsDefault`, UnversionedPropertySerialization.cpp:989), so a large share of
/// a class's schema is simply absent from any given export. Without this, those
/// properties are uneditable — which is most of them, and exactly the ones a
/// user is likely to want to change *away* from the default.
///
/// Inserting one is not the same as reproducing what the cooker would write.
/// Deciding to omit a property needs the class default object, which cooked
/// data does not carry, so a property set to its default value is written out
/// longhand where the cooker would have dropped it. The engine loads both to the
/// same value; only byte-identity with the cooker is lost, and a mod does not
/// need that.
///
/// Entries are kept in ascending schema order, which
/// `write_block` requires — `FUnversionedHeaderBuilder`
/// walks the schema forwards and cannot express going back.
pub fn set_property(
    block: &mut PropertyBlock,
    class: &str,
    property: &str,
    value: PropValue,
    usmap: &Usmap,
) -> Result<()> {
    set_property_slot(block, class, property, 0, value, usmap)
}

/// As [`set_property`], but for one slot of a static array.
pub fn set_property_slot(
    block: &mut PropertyBlock,
    class: &str,
    property: &str,
    array_index: u8,
    value: PropValue,
    usmap: &Usmap,
) -> Result<()> {
    let flat = flattened_schema(class, usmap)?;
    let schema_index = flat
        .iter()
        .position(|(p, slot, _)| p.name == property && *slot == array_index)
        .with_context(|| {
            if array_index == 0 {
                format!("class {class} has no property {property}")
            } else {
                format!("class {class} has no property {property}[{array_index}]")
            }
        })? as u32;

    // The block records the schema it was read against; a property beyond it
    // would produce a header the loader walks off the end of.
    let BlockLayout::Unversioned { schema_len, .. } = block.layout;
    if schema_index >= schema_len {
        bail!(
            "{property} is schema index {schema_index} but the block was read against a \
             {schema_len}-property schema"
        );
    }

    match block
        .entries
        .iter_mut()
        .find(|e| e.slot.is_some_and(|s| s.index == schema_index))
    {
        Some(entry) => {
            entry.value = value;
            // A property that was zero-masked and is being given a value must
            // stop being masked; the writer derives that, but leaving a stale
            // flag here would be a lie about what was read.
            if let Some(slot) = entry.slot.as_mut() {
                slot.zero_masked = false;
            }
        }
        None => {
            let at = block
                .entries
                .iter()
                .position(|e| e.slot.is_some_and(|s| s.index > schema_index))
                .unwrap_or(block.entries.len());
            block.entries.insert(
                at,
                PropertyEntry {
                    name: Arc::from(property),
                    value,
                    slot: Some(SchemaSlot {
                        index: schema_index,
                        array_index,
                        // Never masked: this property was absent, so the file
                        // has no opinion, and writing it longhand is the safe
                        // encoding for every type.
                        zero_masked: false,
                    }),
                },
            );
        }
    }
    Ok(())
}

/// Remove a property from a block, returning whether it was there.
///
/// The engine then falls back to the class default for it, which is what the
/// cooker's own omission means.
pub fn remove_property(block: &mut PropertyBlock, property: &str) -> bool {
    let before = block.entries.len();
    block.entries.retain(|e| &*e.name != property);
    block.entries.len() != before
}

/// Set a property to a name, interning it into the package's name map.
///
/// Inserts the property when the block does not carry it, like
/// [`set_property`].
pub fn set_name_property(
    block: &mut PropertyBlock,
    class: &str,
    name_map: &mut FNameMap,
    property: &str,
    text: &str,
    usmap: &Usmap,
) -> Result<()> {
    let value = PropValue::Name(intern_name(name_map, text));
    set_property(block, class, property, value, usmap)
}

/// Set a property to a string value.
///
/// An `FString` is stored inline rather than through the name map, so unlike
/// [`set_name_property`] this needs nothing from the package.
pub fn set_string_property(
    block: &mut PropertyBlock,
    class: &str,
    property: &str,
    text: &str,
    usmap: &Usmap,
) -> Result<()> {
    set_property(block, class, property, PropValue::Str(text.into()), usmap)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::object::block::emit_block;
    use crate::iostore::object::export::read_export_struct_len;
    use crate::iostore::object::usmap::{PropertyType, UsmapProperty};
    use crate::iostore::package::name_map::EMappedNameType;

    /// A three-property class, so a block can plausibly be missing the middle
    /// one — which is the case the cooker actually produces.
    fn probe() -> Usmap {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "InsertProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "First".into(),
                    ty: PropertyType::Int,
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "Middle".into(),
                    ty: PropertyType::Int,
                },
                UsmapProperty {
                    schema_index: 2,
                    array_dim: 1,
                    name: "Last".into(),
                    ty: PropertyType::Int,
                },
            ],
        );
        usmap
    }

    /// A property the cooker omitted — because its value equalled the class
    /// default — must still be settable. Most of a class's schema is absent
    /// from any given export, and those are exactly the properties a user wants
    /// to change away from the default.
    #[test]
    fn a_property_the_block_never_had_can_be_inserted() {
        let usmap = probe();
        // First and Last present, Middle absent: skip 0/value 1, skip 1/value 1.
        let mut export = Vec::new();
        export.extend_from_slice(&((1u16 << 9) | 0).to_le_bytes());
        export.extend_from_slice(&(((1u16) << 9) | 0x0100 | 1).to_le_bytes());
        export.extend_from_slice(&11i32.to_le_bytes());
        export.extend_from_slice(&33i32.to_le_bytes());

        let (mut block, used) =
            read_export_struct_len(&export, &[], &usmap, "InsertProbe").expect("decode");
        assert_eq!(used, export.len());
        assert_eq!(block.len(), 2, "the fixture should start without Middle");
        assert!(block.get("Middle").is_none());

        set_property(
            &mut block,
            "InsertProbe",
            "Middle",
            PropValue::Int(22),
            &usmap,
        )
        .expect("insert");

        let out = emit_block("InsertProbe", &block, &usmap).expect("emit");
        let (back, _) = read_export_struct_len(&out, &[], &usmap, "InsertProbe").expect("re-read");
        assert!(matches!(back.get("First"), Some(PropValue::Int(11))));
        assert!(
            matches!(back.get("Middle"), Some(PropValue::Int(22))),
            "the inserted property is missing"
        );
        assert!(matches!(back.get("Last"), Some(PropValue::Int(33))));
    }

    /// Entries must stay in ascending schema order — `FUnversionedHeaderBuilder`
    /// walks the schema forwards and cannot express going back, so an
    /// out-of-order insert would emit a header describing different properties
    /// than the values that follow it.
    #[test]
    fn an_inserted_property_lands_in_schema_order() {
        let usmap = probe();
        let mut block = PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned {
                schema_len: 3,
                leading_empty: 0,
            },
        };
        // Deliberately inserted out of order.
        set_property(&mut block, "InsertProbe", "Last", PropValue::Int(3), &usmap).unwrap();
        set_property(
            &mut block,
            "InsertProbe",
            "First",
            PropValue::Int(1),
            &usmap,
        )
        .unwrap();
        set_property(
            &mut block,
            "InsertProbe",
            "Middle",
            PropValue::Int(2),
            &usmap,
        )
        .unwrap();

        let order: Vec<u32> = block
            .entries
            .iter()
            .map(|e| e.slot.unwrap().index)
            .collect();
        assert_eq!(order, vec![0, 1, 2], "entries are not in schema order");

        let out = emit_block("InsertProbe", &block, &usmap).expect("emit");
        let (back, _) = read_export_struct_len(&out, &[], &usmap, "InsertProbe").expect("re-read");
        assert!(matches!(back.get("First"), Some(PropValue::Int(1))));
        assert!(matches!(back.get("Middle"), Some(PropValue::Int(2))));
        assert!(matches!(back.get("Last"), Some(PropValue::Int(3))));
    }

    /// Setting a property the class does not declare is a caller bug, not
    /// something to invent a schema slot for.
    #[test]
    fn setting_an_unknown_property_is_refused() {
        let usmap = probe();
        let mut block = PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned {
                schema_len: 3,
                leading_empty: 0,
            },
        };
        let e = set_property(
            &mut block,
            "InsertProbe",
            "NoSuchThing",
            PropValue::Int(1),
            &usmap,
        )
        .unwrap_err()
        .to_string();
        assert!(
            e.contains("no property NoSuchThing"),
            "unhelpful refusal: {e}"
        );
    }

    /// Removing a property makes the engine fall back to the class default,
    /// which is what the cooker's own omission means.
    #[test]
    fn removing_a_property_drops_it_from_the_block() {
        let usmap = probe();
        let mut block = PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned {
                schema_len: 3,
                leading_empty: 0,
            },
        };
        set_property(
            &mut block,
            "InsertProbe",
            "Middle",
            PropValue::Int(2),
            &usmap,
        )
        .unwrap();
        assert!(remove_property(&mut block, "Middle"));
        assert!(
            !remove_property(&mut block, "Middle"),
            "removing twice should report nothing"
        );
        let out = emit_block("InsertProbe", &block, &usmap).expect("emit");
        let (back, _) = read_export_struct_len(&out, &[], &usmap, "InsertProbe").expect("re-read");
        assert!(back.get("Middle").is_none());
    }

    /// A name the package already has must reuse its index, and a new one must
    /// extend the map rather than collide with something.
    #[test]
    fn interning_reuses_an_existing_name_and_appends_a_new_one() {
        let mut map = FNameMap::create_from_names(
            EMappedNameType::Package,
            vec!["None".into(), "Rocket".into()],
        );
        let existing = intern_name(&mut map, "Rocket");
        assert_eq!((existing.index, existing.number), (1, 0));
        assert_eq!(
            map.copy_raw_names().len(),
            2,
            "an existing name must not be appended"
        );

        let fresh = intern_name(&mut map, "BlamEditProbe");
        assert_eq!(fresh.index, 2);
        assert_eq!(map.copy_raw_names().len(), 3);
        assert_eq!(fresh.as_str(), "BlamEditProbe");
    }

    /// A trailing `_N` is an instance number, as it is everywhere else in the
    /// engine — so authoring one reuses the base name rather than adding a
    /// second entry that renders identically.
    #[test]
    fn interning_splits_a_trailing_number_like_the_engine_does() {
        let mut map = FNameMap::create_from_names(EMappedNameType::Package, vec!["Rocket".into()]);
        let n = intern_name(&mut map, "Rocket_4");
        assert_eq!(
            (n.index, n.number),
            (0, 5),
            "should reuse `Rocket` with number 5"
        );
        assert_eq!(map.copy_raw_names(), vec!["Rocket".to_string()]);
        assert_eq!(
            map.get(crate::iostore::package::name_map::FMappedName::create(
                n.index,
                EMappedNameType::Package,
                n.number
            )),
            "Rocket_4"
        );
    }

    #[test]
    fn editor_defaults_keep_container_element_types() {
        let usmap = probe();
        let array = PropertyType::Array(Box::new(PropertyType::Int16));
        let map = PropertyType::Map(
            Box::new(PropertyType::Name),
            Box::new(PropertyType::SoftObject),
        );
        assert!(matches!(
            default_value_for_type(&array, &usmap).unwrap(),
            PropValue::Array(values) if values.is_empty()
        ));
        assert!(matches!(
            default_value_for_type(&map, &usmap).unwrap(),
            PropValue::Map(values) if values.is_empty()
        ));
        assert!(default_value_for_type(&PropertyType::Unknown(222), &usmap).is_err());
    }

    #[test]
    fn editor_validation_walks_nested_containers() {
        let ty = PropertyType::Map(
            Box::new(PropertyType::Name),
            Box::new(PropertyType::Array(Box::new(PropertyType::Float))),
        );
        let good = PropValue::Map(vec![(
            PropValue::Name(FName::none()),
            PropValue::Array(vec![PropValue::Float(1.25)]),
        )]);
        let bad = PropValue::Map(vec![(
            PropValue::Int(0),
            PropValue::Array(vec![PropValue::Float(1.25)]),
        )]);
        validate_value_for_type(&ty, &good).unwrap();
        assert!(validate_value_for_type(&ty, &bad).is_err());
    }

    #[test]
    fn an_entry_slot_resolves_back_to_its_declared_type() {
        let usmap = probe();
        let slot = SchemaSlot {
            index: 1,
            array_index: 0,
            zero_masked: false,
        };
        assert_eq!(
            property_type_for_slot("InsertProbe", slot, &usmap).unwrap(),
            PropertyType::Int
        );
    }

    #[test]
    fn editable_slot_catalog_stays_in_schema_order() {
        let usmap = probe();
        let slots = editable_schema_slots("InsertProbe", &usmap).unwrap();
        assert!(
            slots
                .windows(2)
                .all(|pair| pair[0].1.index < pair[1].1.index)
        );
        assert!(!slots.is_empty());
        for (_, slot, ty) in slots {
            assert_eq!(
                property_type_for_slot("InsertProbe", slot, &usmap).unwrap(),
                ty
            );
        }
    }
}

/// Repoint an object property at a different package, adjusting the package's
/// import structures to match.
///
/// This is what "rebind this tag to a different Blueprint" is: `AssetReference`
/// on a Campaign Evolved tag wrapper holds an `FPackageIndex` into the import
/// map, and the map does not name a package — three parallel arrays behind it
/// do. Setting the property alone would leave it pointing at whatever the old
/// slot still says.
///
/// `block` must be the package's **only** export's block, or at least every
/// block that can reference an import, because deciding whether a slot may be
/// rewritten in place means knowing whether anything else points at it. For a CE
/// tag wrapper that is exactly satisfied: all 12,291 have precisely one export.
///
/// A slot referenced only by this property is retargeted in place, keeping the
/// import map's length and slot order — which is what lets every other
/// `FPackageIndex` in the payload stay valid. A slot something *else* also
/// points at gets a new slot appended instead, because rewriting it would
/// silently repoint the other reference too.
pub fn set_object_property(
    header: &mut FZenPackageHeader,
    block: &mut PropertyBlock,
    property: &str,
    target: ImportTarget,
    class: &str,
    usmap: &Usmap,
) -> Result<()> {
    // Absent is the normal case, not an error: the cooker omits any property
    // equal to its class default, and a tag that binds nothing has no
    // `AssetReference` at all. Refusing here made "give this tag an actor"
    // impossible for exactly the tags that need it most.
    if !block.entries.iter().any(|e| &*e.name == property) {
        set_property(block, class, property, PropValue::Object(0), usmap)
            .with_context(|| format!("inserting {property}"))?;
    }
    let Some(entry) = block.entries.iter().position(|e| &*e.name == property) else {
        bail!("the block has no property named {property}");
    };
    let current = match block.entries[entry].value.unwrapped() {
        PropValue::Object(index) => *index,
        other => bail!("{property} is not an object reference (it is {other:?})"),
    };

    let mut slots = read_import_slots(header)?;
    let slot = match import_slot_of(current) {
        // Already an import, and nothing else uses it: retarget in place, so
        // the import map keeps its length and every other index stays valid.
        Some(slot)
            if slot < slots.len()
                && matches!(slots[slot], ImportSlot::Package(_))
                && count_object_references(block, current) == 1 =>
        {
            slots[slot] = ImportSlot::Package(target);
            slot
        }
        // Null, an export, out of range, or a slot shared with another
        // reference — take a fresh slot rather than disturb anything.
        _ => {
            slots.push(ImportSlot::Package(target));
            slots.len() - 1
        }
    };

    write_import_slots(header, &slots)?;
    block.entries[entry].value = PropValue::Object(import_package_index(slot));
    Ok(())
}

/// How many object references in `block`, at any depth, name `package_index`.
///
/// Nested on purpose: `CookedAssetsReferencedByTag` is an array of references,
/// so a top-level-only count would report zero for exactly the properties most
/// likely to share a slot.
fn count_object_references(block: &PropertyBlock, package_index: i32) -> usize {
    fn count_value(value: &PropValue, package_index: i32) -> usize {
        match value.unwrapped() {
            PropValue::Object(i) => usize::from(*i == package_index),
            PropValue::Array(items) | PropValue::Set(items) => {
                items.iter().map(|v| count_value(v, package_index)).sum()
            }
            PropValue::Map(pairs) => pairs
                .iter()
                .map(|(k, v)| count_value(k, package_index) + count_value(v, package_index))
                .sum(),
            PropValue::Struct(inner) => count_object_references(inner, package_index),
            PropValue::Delegate { object, .. } => usize::from(*object == package_index),
            PropValue::MulticastDelegate(list) => {
                list.iter().filter(|(o, _)| *o == package_index).count()
            }
            _ => 0,
        }
    }
    block
        .entries
        .iter()
        .map(|e| count_value(&e.value, package_index))
        .sum()
}
