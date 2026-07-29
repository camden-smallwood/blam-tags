//! Changing a decoded export and making the change survive a rebuild.
//!
//! Everything else in this layer is symmetric — read a thing, write the same
//! thing back. Editing is the asymmetric case, and it has one requirement the
//! round-trip path never hits: a value may need a *name that is not in the
//! package yet*. An `FName` is an index into the package's name map, so setting
//! a name property to a new string means growing that map and using the index it
//! comes back with. Handing the writer an `FName` whose index points at nothing
//! is the obvious failure, and a silent one — the index is just a number.

use anyhow::{bail, Result};

use super::value::{FName, PropValue, PropertyBlock};
use crate::iostore::package::name_map::FNameMap;

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

/// Set a property in a block to a new name, interning it into the package.
///
/// Fails rather than inventing a property: a block only carries the properties
/// the export actually serialized, and adding one means placing it in the
/// class's schema, which is [`PropertyBlock`]-level surgery rather than an edit.
pub fn set_name_property(
    block: &mut PropertyBlock,
    name_map: &mut FNameMap,
    property: &str,
    text: &str,
) -> Result<()> {
    let value = PropValue::Name(intern_name(name_map, text));
    match block.entries.iter_mut().find(|e| &*e.name == property) {
        Some(entry) => {
            entry.value = value;
            Ok(())
        }
        None => bail!("no property {property} in this block"),
    }
}

/// Set a property to a string value.
///
/// An `FString` is stored inline rather than through the name map, so unlike
/// [`set_name_property`] this needs nothing from the package.
pub fn set_string_property(block: &mut PropertyBlock, property: &str, text: &str) -> Result<()> {
    match block.entries.iter_mut().find(|e| &*e.name == property) {
        Some(entry) => {
            entry.value = PropValue::Str(text.to_string());
            Ok(())
        }
        None => bail!("no property {property} in this block"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::package::name_map::EMappedNameType;

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
        assert_eq!(map.copy_raw_names().len(), 2, "an existing name must not be appended");

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
        let mut map =
            FNameMap::create_from_names(EMappedNameType::Package, vec!["Rocket".into()]);
        let n = intern_name(&mut map, "Rocket_4");
        assert_eq!((n.index, n.number), (0, 5), "should reuse `Rocket` with number 5");
        assert_eq!(map.copy_raw_names(), vec!["Rocket".to_string()]);
        assert_eq!(map.get(crate::iostore::package::name_map::FMappedName::create(
            n.index,
            EMappedNameType::Package,
            n.number
        )), "Rocket_4");
    }
}
