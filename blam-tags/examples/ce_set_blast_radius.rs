//! Measure the blast radius of the `TSet` deserialization fix.
//!
//! `TSet` used to be read as a bare `TArray`, missing UE's
//! `NumElementsToRemove` prefix. That left the stream 4 bytes short and
//! silently desynced every property *after* the set in the same struct — so
//! the fix only changes behaviour for structs that actually contain a set,
//! and only for the properties that follow it.
//!
//! This reports, for every class the codebase decodes through the `.usmap`:
//! whether a `TSet` appears anywhere in its (recursive) schema, and if so
//! whether anything is serialized after it. A class with no set is provably
//! unaffected.
//!
//! Run:
//!   cargo run --release -p blam-tags --features iostore --example ce_set_blast_radius

use std::collections::BTreeSet;

use blam_tags::iostore::usmap::{PropertyType, Usmap};

/// Every class this codebase hands to the unversioned reader.
const PARSED_CLASSES: &[&str] = &[
    // Campaign Evolved model/preview path.
    "BlamMeshSynchronizationComponent",
    "DataTable",
    "SkeletalMesh",
    "StaticMesh",
    "SkeletalMeshSocket",
    "MaterialInstanceConstant",
    "MaterialInterface",
    // Wwise audio path (the reason the bug was found).
    "AkAudioEvent",
    "WwiseLocalizedEventCookedData",
    "WwiseEventCookedData",
    "WwiseMediaCookedData",
    "WwiseSoundBankCookedData",
    "WwiseLanguageCookedData",
];

/// Walk a property type looking for a `Set`, following struct references.
fn contains_set(usmap: &Usmap, ty: &PropertyType, seen: &mut BTreeSet<String>) -> bool {
    match ty {
        PropertyType::Set(_) => true,
        PropertyType::Array(inner) | PropertyType::Optional(inner) => {
            contains_set(usmap, inner, seen)
        }
        PropertyType::Map(k, v) => contains_set(usmap, k, seen) || contains_set(usmap, v, seen),
        PropertyType::Struct(name) => {
            if !seen.insert(name.clone()) {
                return false; // already visited (or cyclic)
            }
            struct_contains_set(usmap, name, seen)
        }
        _ => false,
    }
}

fn struct_contains_set(usmap: &Usmap, name: &str, seen: &mut BTreeSet<String>) -> bool {
    let Some(props) = usmap.flattened_properties(name) else { return false };
    props.iter().any(|p| contains_set(usmap, &p.ty, seen))
}

/// A set only matters if something is serialized after it — otherwise the
/// 4-byte shortfall falls off the end of the struct and changes nothing.
fn set_position(usmap: &Usmap, name: &str) -> Option<(usize, usize, String)> {
    let props = usmap.flattened_properties(name)?;
    let total = props.len();
    props.iter().enumerate().find_map(|(i, p)| {
        let mut seen = BTreeSet::new();
        contains_set(usmap, &p.ty, &mut seen).then(|| (i, total, p.name.clone()))
    })
}

fn main() -> anyhow::Result<()> {
    let usmap = Usmap::meteorite()?;

    println!("== classes this codebase decodes ==");
    let mut affected = 0usize;
    for class in PARSED_CLASSES {
        if usmap.get(class).is_none() {
            println!("  {class:<38} (not in usmap)");
            continue;
        }
        let mut seen = BTreeSet::new();
        if struct_contains_set(&usmap, class, &mut seen) {
            affected += 1;
            match set_position(&usmap, class) {
                Some((i, total, prop)) => println!(
                    "  {class:<38} CONTAINS SET  (own prop {} of {total}: {prop}{})",
                    i + 1,
                    if i + 1 < total { ", properties follow" } else { ", nothing follows" }
                ),
                None => println!("  {class:<38} CONTAINS SET  (nested)"),
            }
        } else {
            println!("  {class:<38} no set \u{2014} unaffected");
        }
    }
    println!("\n{affected} of {} decoded classes touch a TSet", PARSED_CLASSES.len());

    // Whole-usmap census, for scale.
    let mut direct = 0usize;
    for s in &usmap.structs {
        if s.properties.iter().any(|p| matches!(p.ty, PropertyType::Set(_))) {
            direct += 1;
        }
    }
    println!(
        "\nacross the whole usmap: {direct} of {} structs declare a TSet directly",
        usmap.structs.len()
    );
    Ok(())
}
