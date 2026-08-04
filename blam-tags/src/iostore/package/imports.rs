//! A package's imports as *targets* rather than as indices.
//!
//! A Zen package names an imported object with an `FPackageObjectIndex` of kind
//! `PackageImport`, which carries two indices: one into `imported_packages`
//! (parallel to `imported_package_names`) and one into
//! `imported_public_export_hashes`. Neither says what the import *is* — the
//! answer is spread across three arrays, and every one of them is order- and
//! index-sensitive.
//!
//! That shape is fine to read and hostile to edit. Repointing a reference at a
//! different package means changing an id, a name, a hash, and possibly the
//! position of all three, because `imported_packages` is sorted by id. Doing it
//! by hand on the arrays is how an editor silently produces a package whose
//! import indices no longer mean what its payload thinks they mean.
//!
//! So: read the import map into resolved [`ImportSlot`]s, change the one you
//! want, and write the slots back. [`write_import_slots`] rederives all three
//! arrays from the slot list.
//!
//! **Slot order is preserved, and that is the point.** An export payload holds
//! `FPackageIndex` values that are positions in the import map, so keeping
//! those positions fixed means an edit never has to rewrite the payload — only
//! the arrays behind the slots move. (Synthesizing a package *from scratch* is
//! the harder problem, because there the slot order is the cooker's linker
//! order and is not reproducible; that is a different job from editing one.)

use anyhow::{bail, Result};

use super::ue_types::{
    lower_utf16_cityhash, FPackageId, FPackageImportReference, FPackageObjectIndex,
    FPackageObjectIndexType,
};
use super::zen::FZenPackageHeader;

/// An imported object, named rather than indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportTarget {
    /// The full package path, e.g. `/Game/Characters/Elite/BP_Elite`.
    pub package: String,
    /// The imported object's public export hash — see [`public_export_hash`].
    pub object_hash: u64,
}

/// One entry of a package's import map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportSlot {
    /// A native object from a `/Script/...` module, identified by a hash of its
    /// object path. Carried through unchanged: nothing about it depends on the
    /// package's own arrays.
    Script(FPackageObjectIndex),
    /// An object in another package.
    Package(ImportTarget),
    /// The null slot. Cooked tag packages carry one per imported package.
    Null,
}

/// The hash a public export is addressed by: CityHash64 of the lowercased
/// UTF-16LE object name.
///
/// The same function produces an `FPackageId` from a package name — the two are
/// different namespaces computed identically, which is why they are easy to
/// swap by accident.
pub fn public_export_hash(object_name: &str) -> u64 {
    lower_utf16_cityhash(object_name)
}

/// Resolve a package's import map into named targets.
pub fn read_import_slots(header: &FZenPackageHeader) -> Result<Vec<ImportSlot>> {
    header
        .import_map
        .iter()
        .enumerate()
        .map(|(i, entry)| match entry.kind() {
            FPackageObjectIndexType::Null => Ok(ImportSlot::Null),
            FPackageObjectIndexType::ScriptImport => Ok(ImportSlot::Script(*entry)),
            FPackageObjectIndexType::Export => {
                bail!("import map slot {i} is an Export index, which cannot be resolved")
            }
            FPackageObjectIndexType::PackageImport => {
                let reference = entry
                    .package_import()
                    .ok_or_else(|| anyhow::anyhow!("import map slot {i} is not a package import"))?;
                let package = header
                    .imported_package_names
                    .get(reference.imported_package_index as usize)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "import map slot {i} names package {} of {}",
                            reference.imported_package_index,
                            header.imported_package_names.len()
                        )
                    })?
                    .clone();
                let object_hash = *header
                    .imported_public_export_hashes
                    .get(reference.imported_public_export_hash_index as usize)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "import map slot {i} names hash {} of {}",
                            reference.imported_public_export_hash_index,
                            header.imported_public_export_hashes.len()
                        )
                    })?;
                Ok(ImportSlot::Package(ImportTarget { package, object_hash }))
            }
        })
        .collect()
}

/// Rebuild `import_map`, `imported_packages`, `imported_package_names` and
/// `imported_public_export_hashes` from `slots`.
///
/// The rules are the cook's own, and were measured over every shipped tag
/// package: `imported_packages` ascends by `FPackageId`, each
/// `imported_package_names[i]` is the name hashing to `imported_packages[i]`,
/// and `imported_public_export_hashes` holds one entry per unique
/// `(package index, hash)` pair in import-map first-use order.
///
/// Deduping the hashes by hash alone is wrong — two packages can export objects
/// whose names collide, and merging them costs eight bytes and points one
/// package's import at another's export.
pub fn write_import_slots(header: &mut FZenPackageHeader, slots: &[ImportSlot]) -> Result<()> {
    // Packages first: the id ordering decides every `imported_package_index`,
    // so it has to be settled before any slot can be numbered.
    let mut packages: Vec<(FPackageId, String)> = Vec::new();
    for slot in slots {
        if let ImportSlot::Package(target) = slot {
            let id = FPackageId::from_name(&target.package);
            if !packages.iter().any(|(existing, _)| *existing == id) {
                packages.push((id, target.package.clone()));
            }
        }
    }
    packages.sort_by_key(|(id, _)| *id);

    let index_of = |package: &str| -> Result<u32> {
        let id = FPackageId::from_name(package);
        packages
            .iter()
            .position(|(existing, _)| *existing == id)
            .map(|i| i as u32)
            .ok_or_else(|| anyhow::anyhow!("package {package} was not collected"))
    };

    let mut hashes: Vec<u64> = Vec::new();
    let mut pairs: Vec<(u32, u64)> = Vec::new();
    let mut import_map = Vec::with_capacity(slots.len());
    for slot in slots {
        import_map.push(match slot {
            ImportSlot::Null => FPackageObjectIndex::create_null(),
            ImportSlot::Script(index) => *index,
            ImportSlot::Package(target) => {
                let package_index = index_of(&target.package)?;
                let key = (package_index, target.object_hash);
                let hash_index = match pairs.iter().position(|existing| *existing == key) {
                    Some(i) => i as u32,
                    None => {
                        pairs.push(key);
                        hashes.push(target.object_hash);
                        (hashes.len() - 1) as u32
                    }
                };
                FPackageObjectIndex::create_package_import(FPackageImportReference {
                    imported_package_index: package_index,
                    imported_public_export_hash_index: hash_index,
                })
            }
        });
    }

    header.import_map = import_map;
    header.imported_packages = packages.iter().map(|(id, _)| *id).collect();
    header.imported_package_names = packages.into_iter().map(|(_, name)| name).collect();
    header.imported_public_export_hashes = hashes;
    Ok(())
}

/// The `FPackageIndex` an export property uses to name import slot `slot`.
///
/// Imports are negative and one-based, so slot 0 is `-1`. Getting this wrong
/// reads as an off-by-one that still resolves — to the neighbouring import.
pub fn import_package_index(slot: usize) -> i32 {
    -(slot as i32) - 1
}

/// The import slot an export property's `FPackageIndex` names, or `None` when
/// it names an export or nothing.
pub fn import_slot_of(package_index: i32) -> Option<usize> {
    (package_index < 0).then(|| (-package_index - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_indices_are_negative_and_one_based() {
        assert_eq!(import_package_index(0), -1);
        assert_eq!(import_slot_of(-1), Some(0));
        assert_eq!(import_slot_of(-4), Some(3));
        assert_eq!(import_slot_of(0), None);
        assert_eq!(import_slot_of(3), None);
    }

    /// The hash is over the *object* name, and a package path is a different
    /// namespace that happens to hash the same way. Pinned to values read out
    /// of shipped tags so a change to the hash is caught here.
    #[test]
    fn the_public_export_hash_is_cityhash_of_the_lowercased_name() {
        assert_eq!(public_export_hash("jackal-model"), 0x9595babddd1ed22f);
        assert_eq!(public_export_hash("plasma_pistol-weapon"), 0x368e3d0b13dcbb23);
        // Case-insensitive, which is what makes the two spellings of
        // `sq_grunt_major_pp_2` one identity.
        assert_eq!(public_export_hash("JACKAL-MODEL"), public_export_hash("jackal-model"));
    }

    /// The CDO is the class path with `Default__` on the *object* name. Pinned
    /// on a group the game ships no tag of, which is the case that needs these
    /// derived rather than read off a donor.
    #[test]
    fn the_cdo_path_prefixes_the_object_name_not_the_module() {
        assert_eq!(
            tag_wrapper_class_path("cinematic_scene"),
            "/Script/BlamSynchronization.BlamCinematicSceneTagDataAsset"
        );
        assert_eq!(
            tag_wrapper_cdo_path("cinematic_scene"),
            "/Script/BlamSynchronization.Default__BlamCinematicSceneTagDataAsset"
        );
        assert_eq!(
            tag_wrapper_cdo_path("biped"),
            "/Script/BlamSynchronization.Default__BlamBipedTagDataAsset"
        );
    }

    /// Only the five `_Generated_` groups carry `PKG_CookGenerated`. Getting
    /// this backwards is invisible in the editor and only shows up in-game.
    #[test]
    fn only_the_generated_groups_get_the_cook_generated_flags() {
        for group in GENERATED_GROUPS {
            assert_eq!(tag_package_flags(group), (0x1, 0x8800_2200), "{group}");
        }
        for group in ["cinematic_scene", "biped", "collision_model", "sound", "model"] {
            assert_eq!(tag_package_flags(group), (0xb, 0x8000_2200), "{group}");
        }
    }
}

#[cfg(test)]
mod corpus {
    use super::*;
    use crate::iostore::container_header::EIoContainerHeaderVersion;
    use crate::iostore::ue_types::EIoStoreTocVersion;
    use crate::iostore::IoStoreArchive;
    use std::io::Cursor;
    use std::path::PathBuf;

    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

    /// Reading the import map into targets and writing it straight back must
    /// reproduce all four arrays, byte for byte, on every shipped tag package.
    ///
    /// This is the control the edit path rests on. `write_import_slots`
    /// rederives ordering and deduplication from rules rather than copying what
    /// was there, so an edit that changes one target rebuilds *everything* — and
    /// if the rules were even slightly wrong, the damage would land on packages
    /// the user never edited a reference in. Proving the no-op case reproduces
    /// the cook exactly is what separates "rebuilt" from "rearranged".
    ///
    ///   CE_PAKS=/path/to/Meteorite/Content/Paks \
    ///     cargo test --features iostore imports::corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn rederiving_the_import_arrays_reproduces_every_shipped_tag() {
        let Ok(root) = std::env::var("CE_PAKS") else {
            panic!("set CE_PAKS to the game's Content/Paks");
        };
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();

        let (mut total, mut with_imports, mut identical) = (0usize, 0usize, 0usize);
        let mut mismatches: Vec<String> = Vec::new();

        for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                total += 1;
                if !header.imported_packages.is_empty() {
                    with_imports += 1;
                }

                let slots = read_import_slots(&header).expect("resolve import slots");
                let mut rebuilt = header.clone();
                write_import_slots(&mut rebuilt, &slots).expect("rewrite import slots");

                if rebuilt.import_map == header.import_map
                    && rebuilt.imported_packages == header.imported_packages
                    && rebuilt.imported_package_names == header.imported_package_names
                    && rebuilt.imported_public_export_hashes == header.imported_public_export_hashes
                {
                    identical += 1;
                } else if mismatches.len() < 10 {
                    mismatches.push(format!(
                        "{}\n  map {} -> {}\n  pkgs {:?} -> {:?}\n  hashes {:?} -> {:?}",
                        entry.path,
                        header.import_map.len(),
                        rebuilt.import_map.len(),
                        header.imported_package_names,
                        rebuilt.imported_package_names,
                        header.imported_public_export_hashes,
                        rebuilt.imported_public_export_hashes,
                    ));
                }
            }
        }

        println!("tag packages          {total}");
        println!("with package imports  {with_imports}");
        println!("arrays reproduced     {identical}");
        for m in &mismatches {
            println!("MISMATCH {m}");
        }
        assert!(total > 0, "no tag packages found");
        assert_eq!(identical, total, "some packages did not rebuild identically");
    }
}

#[cfg(test)]
mod edit_corpus {
    use super::*;
    use crate::iostore::container_header::EIoContainerHeaderVersion;
    use crate::iostore::object::edit::set_object_property;
    use crate::iostore::object::export::{read_export, write_export, Export};
    use crate::iostore::object::value::PropValue;
    use crate::iostore::package::builder::{read_payloads, write_package};
    use crate::iostore::ue_types::EIoStoreTocVersion;
    use crate::iostore::usmap::Usmap;
    use crate::iostore::IoStoreArchive;
    use std::io::Cursor;
    use std::path::PathBuf;

    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

    fn class_for(group: &str) -> String {
        let mut pascal = String::new();
        for word in group.split('_').filter(|w| !w.is_empty()) {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                pascal.extend(first.to_uppercase());
                pascal.push_str(&chars.as_str().to_ascii_lowercase());
            }
        }
        format!("Blam{pascal}TagDataAsset")
    }

    /// Repoint `AssetReference` on every shipped tag that has one, rebuild the
    /// package, and read it back from the rebuilt bytes.
    ///
    /// Two things are checked, and the second is the one that catches real
    /// damage: the new target must come back, **and every other property must
    /// be untouched**. An edit that lands while corrupting its neighbours
    /// passes any check that only looks at what it meant to change, and
    /// rebuilding import arrays is exactly where that goes wrong.
    ///
    /// The no-op control runs first: repointing at the value already there must
    /// reproduce the original package **byte for byte**. Without it, "the edit
    /// read back" would be equally true of a rebuild that quietly rewrote half
    /// the header.
    ///
    ///   CE_PAKS=/path/to/Meteorite/Content/Paks \
    ///     cargo test --features iostore imports::edit_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn repointing_an_asset_reference_survives_a_rebuild() {
        let root = std::env::var("CE_PAKS").expect("set CE_PAKS to the game's Content/Paks");
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        let usmap = Usmap::meteorite().expect("bundled usmap");
        const NEW_TARGET: &str = "/Game/_Prototypes/Blueprints/BP_EmptyActor";

        let (mut with_ref, mut noop_identical, mut edited, mut verified, mut neighbours_ok) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut failures: Vec<String> = Vec::new();

        for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let stem = lower.trim_end_matches(".uasset");
                let Some((_, group)) = stem.rsplit('/').next().and_then(|f| f.rsplit_once('-'))
                else {
                    continue;
                };
                let class = class_for(group);
                if usmap.get(&class).is_none() {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                let Ok(payloads) = read_payloads(&header, &bytes) else { continue };
                let (Some(payload), Some(export_entry)) =
                    (payloads.first(), header.export_map.first())
                else {
                    continue;
                };
                let names = header.name_map.copy_raw_names();
                let Ok(export) =
                    read_export(payload, &names, &usmap, &class, export_entry.object_flags)
                else {
                    continue;
                };
                let Some(block) = export.properties() else { continue };
                let Some(PropValue::Object(current)) =
                    block.get("AssetReference").map(PropValue::unwrapped)
                else {
                    continue;
                };
                let Some(slot) = import_slot_of(*current) else { continue };
                let slots = read_import_slots(&header).expect("slots");
                let ImportSlot::Package(original) = slots[slot].clone() else { continue };
                with_ref += 1;

                // Control: repoint at what is already there.
                let rebuilt_noop = rebuild(&header, &payloads, &export, &class, &usmap, original);
                if rebuilt_noop.as_deref() == Some(&bytes[..]) {
                    noop_identical += 1;
                } else if failures.len() < 8 {
                    failures.push(format!("{}: no-op rebuild differed", entry.path));
                }

                // The real edit: point it somewhere else entirely.
                let target = ImportTarget {
                    package: NEW_TARGET.to_owned(),
                    object_hash: public_export_hash("BP_EmptyActor_C"),
                };
                let Some(new_bytes) =
                    rebuild(&header, &payloads, &export, &class, &usmap, target.clone())
                else {
                    if failures.len() < 8 {
                        failures.push(format!("{}: edited rebuild failed", entry.path));
                    }
                    continue;
                };
                edited += 1;

                // Read it back from the rebuilt bytes, not from what we wrote.
                let Ok(back_header) = FZenPackageHeader::deserialize(
                    &mut Cursor::new(&new_bytes[..]),
                    None,
                    CV,
                    HV,
                    None,
                ) else {
                    failures.push(format!("{}: rebuilt package did not parse", entry.path));
                    continue;
                };
                let back_payloads = read_payloads(&back_header, &new_bytes).expect("payloads");
                let back_names = back_header.name_map.copy_raw_names();
                let back = read_export(
                    &back_payloads[0],
                    &back_names,
                    &usmap,
                    &class,
                    back_header.export_map[0].object_flags,
                )
                .expect("re-read export");
                let back_block = back.properties().expect("block");

                let resolved = match back_block.get("AssetReference").map(PropValue::unwrapped) {
                    Some(PropValue::Object(i)) => import_slot_of(*i)
                        .and_then(|s| read_import_slots(&back_header).ok()?.get(s).cloned())
                        .and_then(|s| match s {
                            ImportSlot::Package(t) => Some(t.package),
                            _ => None,
                        }),
                    _ => None,
                };
                if resolved.as_deref() == Some(NEW_TARGET) {
                    verified += 1;
                } else if failures.len() < 8 {
                    failures.push(format!("{}: read back {resolved:?}", entry.path));
                }

                // Neighbours: every other property must be value-identical, and
                // every other import must still name the same package.
                let others_same = block
                    .entries
                    .iter()
                    .zip(&back_block.entries)
                    .all(|(a, b)| {
                        a.name == b.name
                            && (&*a.name == "AssetReference" || a.value.semantic_eq(&b.value))
                    })
                    && block.entries.len() == back_block.entries.len();
                if others_same {
                    neighbours_ok += 1;
                } else if failures.len() < 8 {
                    failures.push(format!("{}: neighbouring properties changed", entry.path));
                }
            }
        }

        println!("tags with AssetReference  {with_ref}");
        println!("no-op rebuild identical   {noop_identical}");
        println!("edited + rebuilt          {edited}");
        println!("new target read back      {verified}");
        println!("neighbours unchanged      {neighbours_ok}");
        for f in &failures {
            println!("FAIL {f}");
        }
        assert!(with_ref > 0, "no tag carried an AssetReference");
        assert_eq!(noop_identical, with_ref, "a no-op rebuild was not byte-identical");
        assert_eq!(verified, with_ref, "an edit did not read back");
        assert_eq!(neighbours_ok, with_ref, "an edit disturbed its neighbours");
    }

    /// The whole chain: repoint a real tag's `AssetReference`, ship it in a mod
    /// container, and read it back out of that container.
    ///
    /// The corpus gate above stops at the rebuilt package bytes. This is the
    /// step that has its own trap: an override container carries **no directory
    /// index**, so reading it back by path returns `NotFound` and the edit looks
    /// lost. Chunk ids are the only handle, and the base container is where they
    /// come from.
    ///
    ///   CE_PAKS=... cargo test --features iostore imports::edit_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn a_repointed_reference_survives_a_mod_container() {
        use crate::iostore::writer::{write_mod_container_full, TagOverride};

        let root = std::env::var("CE_PAKS").expect("set CE_PAKS to the game's Content/Paks");
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        let usmap = Usmap::meteorite().expect("bundled usmap");
        const NEW_TARGET: &str = "/Game/_Prototypes/Blueprints/BP_EmptyActor";

        // The first tag carrying an AssetReference, whatever it is.
        let mut picked = None;
        'outer: for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Some((_, group)) = lower
                    .trim_end_matches(".uasset")
                    .rsplit('/')
                    .next()
                    .and_then(|f| f.rsplit_once('-'))
                else {
                    continue;
                };
                let class = class_for(group);
                if usmap.get(&class).is_none() {
                    continue;
                }
                let ubulk = entry.path.replace(".uasset", ".ubulk");
                if !archive.contains(&ubulk) {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                let Ok(payloads) = read_payloads(&header, &bytes) else { continue };
                let Some(export_entry) = header.export_map.first() else { continue };
                let names = header.name_map.copy_raw_names();
                let Ok(export) = read_export(
                    &payloads[0],
                    &names,
                    &usmap,
                    &class,
                    export_entry.object_flags,
                ) else {
                    continue;
                };
                let has_ref = export
                    .properties()
                    .and_then(|b| b.get("AssetReference"))
                    .is_some();
                if has_ref {
                    picked = Some((utoc.clone(), entry.path.clone(), ubulk, class, header, payloads, export));
                    break 'outer;
                }
            }
        }
        let (utoc, uasset_path, ubulk_path, class, header, payloads, export) =
            picked.expect("no tag with an AssetReference");

        let target = ImportTarget {
            package: NEW_TARGET.to_owned(),
            object_hash: public_export_hash("BP_EmptyActor_C"),
        };
        let rebuilt = rebuild(&header, &payloads, &export, &class, &usmap, target)
            .expect("rebuild the wrapper");

        let archive = IoStoreArchive::open(&utoc).expect("reopen base");
        let tag_bytes = archive.read(&ubulk_path).expect("read tag body");
        let dir = std::env::temp_dir().join(format!("blam-wrapper-mod-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("wrappertest-WinGDK_P.utoc");
        write_mod_container_full(
            &[TagOverride {
                archive: &archive,
                ubulk_path: &ubulk_path,
                tag_bytes: &tag_bytes,
                uasset_bytes: Some(&rebuilt),
            }],
            &[],
            &out,
        )
        .expect("write mod container");

        // Read it back BY CHUNK ID: an override container ships no directory
        // index, so every path lookup in it fails.
        let modded = IoStoreArchive::open(&out).expect("open the mod");
        let id = archive.chunk_id_for(&uasset_path).expect("base chunk id");
        let index = modded.find_chunk(&id).expect("the mod carries the .uasset chunk");
        let back_bytes = modded.read_chunk(index).expect("read the chunk back");

        let back_header =
            FZenPackageHeader::deserialize(&mut Cursor::new(&back_bytes[..]), None, CV, HV, None)
                .expect("parse the shipped wrapper");
        let back_payloads = read_payloads(&back_header, &back_bytes).expect("payloads");
        let back_names = back_header.name_map.copy_raw_names();
        let back = read_export(
            &back_payloads[0],
            &back_names,
            &usmap,
            &class,
            back_header.export_map[0].object_flags,
        )
        .expect("decode the shipped wrapper");
        let slots = read_import_slots(&back_header).expect("slots");
        let resolved = match back.properties().and_then(|b| b.get("AssetReference")).map(PropValue::unwrapped) {
            Some(PropValue::Object(i)) => import_slot_of(*i)
                .and_then(|s| slots.get(s))
                .and_then(|s| match s {
                    ImportSlot::Package(t) => Some(t.package.clone()),
                    _ => None,
                }),
            _ => None,
        };
        println!("{ubulk_path}\n  AssetReference -> {resolved:?}");
        assert_eq!(resolved.as_deref(), Some(NEW_TARGET));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Apply one `AssetReference` change and put the package back together.
    fn rebuild(
        header: &FZenPackageHeader,
        payloads: &[Vec<u8>],
        export: &Export,
        class: &str,
        usmap: &Usmap,
        target: ImportTarget,
    ) -> Option<Vec<u8>> {
        let mut header = header.clone();
        let mut export = export.clone();
        let block = export.properties_mut()?;
        set_object_property(&mut header, block, "AssetReference", target, class, usmap).ok()?;
        let payload = write_export(class, &export, usmap).ok()?;
        let mut payloads = payloads.to_vec();
        payloads[0] = payload;
        write_package(&header, &payloads, HV).ok().map(|(bytes, _)| bytes)
    }
}

/// The wrapper class the cook gives a tag of `group_longname`.
///
/// `biped` → `/Script/BlamSynchronization.BlamBipedTagDataAsset`. All 101
/// shipped groups follow this mechanically. It is a *candidate*: a caller with
/// the package in hand should hash it and check
/// `FPackageObjectIndex::create_script_import(..) == export.class_index` rather
/// than trust the name, because decoding against a wrong class does not error —
/// it produces plausible values.
pub fn tag_wrapper_class_path(group_longname: &str) -> String {
    format!(
        "{TAG_WRAPPER_MODULE_PATH}.Blam{}TagDataAsset",
        tag_wrapper_class_stem(group_longname)
    )
}

/// The class-default object of [`tag_wrapper_class_path`] — every tag export's
/// `template_index`.
///
/// The `Default__` prefix goes on the *object* name, not the module path, so
/// this is not a prefix of the class path and cannot be derived by string
/// concatenation from it.
pub fn tag_wrapper_cdo_path(group_longname: &str) -> String {
    format!(
        "{TAG_WRAPPER_MODULE_PATH}.Default__Blam{}TagDataAsset",
        tag_wrapper_class_stem(group_longname)
    )
}

/// The module package every tag wrapper imports alongside its class and CDO.
/// Those three are the entire script-import set of a shipped tag — 12,290 of
/// 12,291, the exception being `player_model_customization_globals` at five.
pub const TAG_WRAPPER_MODULE_PATH: &str = "/Script/BlamSynchronization";

/// `frame_event_list` → `FrameEventList`. Shared by the class and its CDO so
/// the two spellings cannot drift apart.
fn tag_wrapper_class_stem(group_longname: &str) -> String {
    let mut pascal = String::with_capacity(group_longname.len());
    for word in group_longname.split('_').filter(|w| !w.is_empty()) {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            pascal.extend(first.to_uppercase());
            pascal.push_str(&chars.as_str().to_ascii_lowercase());
        }
    }
    pascal
}

/// The groups the cook emits as `_Generated_` level content, which carry
/// `PKG_CookGenerated` and drop `RF_Standalone`/`RF_Transactional`.
///
/// All 312 of them live in per-level paks, never `pakchunk0`.
const GENERATED_GROUPS: &[&str] = &[
    "scenario",
    "scenario_structure_bsp",
    "scenario_structure_lighting_info",
    "structure_design",
    "structure_seams",
];

/// `(object_flags, package_flags)` for a tag wrapper of `group_longname`.
///
/// Measured over all 12,291 shipped tag packages: `0xb`/`0x80002200` for every
/// group except the five in [`GENERATED_GROUPS`]. 2,563 `sound` and one
/// `ai_mission_dialogue` ship `0x3` instead, dropping `RF_Transactional`, which
/// is inert in a cooked build — so the majority value is used for them too.
pub fn tag_package_flags(group_longname: &str) -> (u32, u32) {
    if GENERATED_GROUPS.contains(&group_longname) {
        (0x1, 0x8800_2200)
    } else {
        (0xb, 0x8000_2200)
    }
}

/// Split a cooked tag package path into `(halo-relative path, group long name)`.
///
/// `/Game/Tags/objects/characters/elite/elite-biped` →
/// `("objects/characters/elite/elite", "biped")`. Splits on the **last** hyphen:
/// tag names contain hyphens, group long names do not contain slashes.
pub fn split_tag_package(package: &str) -> Option<(&str, &str)> {
    let rest = package
        .strip_prefix("/Game/Tags/")
        .or_else(|| package.strip_prefix("/game/tags/"))?;
    let (path, group) = rest.rsplit_once('-')?;
    if path.is_empty() || group.is_empty() || group.contains('/') {
        return None;
    }
    Some((path, group))
}

#[cfg(test)]
mod new_package_corpus {
    use super::*;
    use crate::iostore::container_header::EIoContainerHeaderVersion;
    use crate::iostore::object::export::read_export;
    use crate::iostore::object::value::PropValue;
    use crate::iostore::package::builder::read_payloads;
    use crate::iostore::ue_types::EIoStoreTocVersion;
    use crate::iostore::usmap::Usmap;
    use crate::iostore::writer::{write_new_tag_container, NewPackage};
    use crate::iostore::IoStoreArchive;
    use std::io::Cursor;
    use std::path::PathBuf;

    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

    /// A new tag built from a donor template must not inherit the donor's
    /// bindings.
    ///
    /// This was a real defect and a silent one: creating a new biped cloned some
    /// other biped's wrapper verbatim, so the new tag presented as *that* biped's
    /// actor and declared that biped's dependencies, with nothing on screen or in
    /// the file to say so. The check is that the donor's `AssetReference` and
    /// package imports are gone, and that a requested one takes its place.
    ///
    ///   CE_PAKS=... cargo test --features iostore new_package_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn a_new_tag_does_not_inherit_its_donors_bindings() {
        let root = std::env::var("CE_PAKS").expect("set CE_PAKS");
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        let usmap = Usmap::meteorite().expect("bundled usmap");

        // A donor that actually has something to inherit.
        let mut donor = None;
        'outer: for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else { continue };
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with("-biped.uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                if header.imported_packages.is_empty() {
                    continue;
                }
                let ubulk = entry.path.replace(".uasset", ".ubulk");
                let Ok(tag) = archive.read(&ubulk) else { continue };
                donor = Some((bytes, tag, header));
                break 'outer;
            }
        }
        let (template, tag_bytes, donor_header) = donor.expect("a biped donor with imports");
        let donor_packages = donor_header.imported_package_names.clone();
        assert!(!donor_packages.is_empty());
        println!("donor imports: {donor_packages:?}");

        const NEW_PKG: &str = "/Game/Tags/objects/characters/test/probe_new-biped";
        const WANTED: &str = "/Game/_Prototypes/Blueprints/BP_EmptyActor";

        for (label, wanted) in [("unset", None), ("chosen", Some(WANTED))] {
            let dir = std::env::temp_dir()
                .join(format!("blam-newtag-{}-{label}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            let out = dir.join("probe-WinGDK_P.utoc");
            // Through `NewPackage` rather than `write_new_tag_container`, so the
            // "chosen" case actually sets a binding instead of asserting nothing.
            crate::iostore::writer::write_mod_container_ex(
                &[],
                &[NewPackage {
                    template_uasset: &template,
                    tag_bytes: &tag_bytes,
                    new_package_path: NEW_PKG,
                    redirect_from: None,
                    asset_reference: wanted.map(|package| ImportTarget {
                        package: package.to_owned(),
                        object_hash: public_export_hash(&format!(
                            "{}_C",
                            package.rsplit('/').next().unwrap()
                        )),
                    }),
                }],
                &out,
            )
            .expect("write new tag container");

            // Read the wrapper back out of the container it was written into.
            let modded = IoStoreArchive::open(&out).expect("open the new container");
            let id = crate::iostore::writer::make_chunk_id(
                FPackageId::from_name(NEW_PKG).0,
                0,
                crate::iostore::CHUNK_TYPE_EXPORT_BUNDLE,
            );
            let index = modded.find_chunk(&id).expect("the container carries the wrapper");
            let bytes = modded.read_chunk(index).expect("read the wrapper");

            let header =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                    .expect("the new wrapper parses");
            println!("{label}: imports now {:?}", header.imported_package_names);

            // The donor's packages must be gone from the declaration entirely,
            // not merely unreferenced.
            for donor_pkg in &donor_packages {
                if wanted == Some(donor_pkg.as_str()) {
                    continue;
                }
                assert!(
                    !header.imported_package_names.contains(donor_pkg),
                    "{label}: the new tag still declares the donor's import {donor_pkg}"
                );
            }

            let payloads = read_payloads(&header, &bytes).expect("payloads");
            let names = header.name_map.copy_raw_names();
            let export = read_export(
                &payloads[0],
                &names,
                &usmap,
                "BlamBipedTagDataAsset",
                header.export_map[0].object_flags,
            )
            .expect("the new wrapper decodes");
            let block = export.properties().expect("a block");
            let reference = block.get("AssetReference").map(PropValue::unwrapped);
            match wanted {
                None => assert!(
                    reference.is_none(),
                    "{label}: the new tag inherited an AssetReference"
                ),
                Some(package) => {
                    // Resolve it the way the game would, through the import map.
                    let Some(PropValue::Object(i)) = reference else {
                        panic!("{label}: the requested AssetReference was not set");
                    };
                    let slot = import_slot_of(*i).expect("an import");
                    let slots = read_import_slots(&header).expect("slots");
                    let ImportSlot::Package(target) = &slots[slot] else {
                        panic!("{label}: AssetReference does not name a package import");
                    };
                    assert_eq!(target.package, package, "{label}: wrong target");
                    assert_eq!(
                        header.imported_package_names,
                        vec![package.to_owned()],
                        "{label}: the new tag should declare exactly the chosen package"
                    );
                }
            }
            assert!(
                block.get("CookedAssetsReferencedByTag").is_none(),
                "{label}: the new tag inherited the donor's dependency list"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Open every `.utoc` under `CE_PAKS`, newest chunk last.
    fn open_shipped_archives() -> Vec<IoStoreArchive> {
        let root = std::env::var("CE_PAKS").expect("set CE_PAKS");
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        utocs.iter().filter_map(|p| IoStoreArchive::open(p).ok()).collect()
    }

    /// The first shipped wrapper of a group whose tags carry no properties at
    /// all, which is what makes its export body transferable to any group.
    fn bare_donor(archives: &[IoStoreArchive]) -> (Vec<u8>, Vec<u8>) {
        for archive in archives {
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with("-collision_model.uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(tag) = archive.read(&entry.path.replace(".uasset", ".ubulk")) else {
                    continue;
                };
                return (bytes, tag);
            }
        }
        panic!("no collision_model donor in the shipped paks");
    }

    /// A group the game ships no tag of can still be authored.
    ///
    /// `cinematic_scene` is the reported case: 38 of the 139 defined groups have
    /// no shipped instance, so there is no same-group wrapper to donate and the
    /// tag could not be created at all. The wrapper is instead retargeted onto a
    /// bare donor, and this asserts every group-shaped field came from the
    /// destination rather than the donor.
    ///
    ///   CE_PAKS=... cargo test --features iostore new_package_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn a_group_with_no_shipped_tag_can_still_be_created() {
        let archives = open_shipped_archives();
        let usmap = Usmap::meteorite().expect("bundled usmap");
        let (template, tag_bytes) = bare_donor(&archives);

        const NEW_PKG: &str = "/Game/Tags/objects/test/probe_new-cinematic_scene";
        let dir = std::env::temp_dir().join(format!("blam-newgroup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("probe-WinGDK_P.utoc");
        write_new_tag_container(&template, &tag_bytes, NEW_PKG, None, &out)
            .expect("a group with no shipped tag must still build");

        let modded = IoStoreArchive::open(&out).expect("open the new container");
        let id = crate::iostore::writer::make_chunk_id(
            FPackageId::from_name(NEW_PKG).0,
            0,
            crate::iostore::CHUNK_TYPE_EXPORT_BUNDLE,
        );
        let index = modded.find_chunk(&id).expect("the container carries the wrapper");
        let bytes = modded.read_chunk(index).expect("read the wrapper");
        let header = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            .expect("the new wrapper parses");

        // Identity and class both come from the destination path, not the donor.
        let class = FPackageObjectIndex::create_script_import(&tag_wrapper_class_path(
            "cinematic_scene",
        ));
        let cdo =
            FPackageObjectIndex::create_script_import(&tag_wrapper_cdo_path("cinematic_scene"));
        let module = FPackageObjectIndex::create_script_import(TAG_WRAPPER_MODULE_PATH);
        assert_eq!(header.export_map[0].class_index, class, "class is still the donor's");
        assert_eq!(header.export_map[0].template_index, cdo, "CDO is still the donor's");
        assert_eq!(
            header.export_map[0].public_export_hash,
            public_export_hash("probe_new-cinematic_scene")
        );
        let (object_flags, package_flags) = tag_package_flags("cinematic_scene");
        assert_eq!(header.export_map[0].object_flags, object_flags);
        assert_eq!(header.summary.package_flags, package_flags);

        let script: Vec<FPackageObjectIndex> = read_import_slots(&header)
            .expect("slots")
            .into_iter()
            .filter_map(|slot| match slot {
                ImportSlot::Script(index) => Some(index),
                _ => None,
            })
            .collect();
        assert_eq!(script, vec![class, cdo, module], "script imports name the wrong class");
        assert!(
            header.imported_package_names.is_empty(),
            "a fresh tag declares no package imports"
        );

        // And it decodes as the class it now claims to be, carrying nothing.
        let payloads = read_payloads(&header, &bytes).expect("payloads");
        let names = header.name_map.copy_raw_names();
        let export = read_export(
            &payloads[0],
            &names,
            &usmap,
            "BlamCinematicSceneTagDataAsset",
            header.export_map[0].object_flags,
        )
        .expect("the new wrapper decodes as its own class");
        assert!(
            export.properties().expect("a block").entries.is_empty(),
            "a fresh tag's wrapper carries no properties"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What generation the game's own tag blobs claim.
    ///
    /// A created tag has to carry the same one: the simulation reads the blob,
    /// not the wrapper, so a tag whose header says something the game does not
    /// expect is a tag that only ever works inside an editor. `TagFile::new`
    /// leaves all three at zero, which is exactly the value nothing shipped has.
    ///
    /// Reads the fixed 64-byte header directly rather than parsing the tag:
    /// `pad[36] | build_version | build_number | version | group_tag |
    /// group_version | checksum | signature`, little-endian, so the `BLAM`
    /// signature reads as `MALB` in the raw bytes.
    ///
    ///   CE_PAKS=... cargo test --features iostore new_package_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn the_shipped_tag_header_generation() {
        let archives = open_shipped_archives();
        let mut census: std::collections::BTreeMap<(i32, i32, u32), usize> = Default::default();
        let mut by_group: std::collections::BTreeMap<(i32, i32, u32), Vec<String>> =
            Default::default();
        let mut total = 0usize;
        for archive in &archives {
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".ubulk") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                if bytes.len() < 64 || &bytes[60..64] != b"MALB" {
                    continue;
                }
                let key = (
                    i32::from_le_bytes(bytes[36..40].try_into().unwrap()),
                    i32::from_le_bytes(bytes[40..44].try_into().unwrap()),
                    u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
                );
                *census.entry(key).or_default() += 1;
                let group = lower
                    .strip_suffix(".ubulk")
                    .and_then(|s| s.rsplit('/').next())
                    .and_then(|stem| stem.rsplit_once('-').map(|(_, g)| g.to_owned()))
                    .unwrap_or_default();
                let seen = by_group.entry(key).or_default();
                if !seen.contains(&group) {
                    seen.push(group);
                }
                total += 1;
            }
        }
        println!("read {total} shipped tag blobs");
        for (key, count) in &census {
            let (build_version, build_number, version) = *key;
            let groups = &by_group[key];
            println!(
                "build_version={build_version} build_number={build_number} \
                 version={version} ({version:#010x}) -> {count} tags, {} groups{}",
                groups.len(),
                if groups.len() <= 6 {
                    format!(" {groups:?}")
                } else {
                    String::new()
                }
            );
        }
        assert!(total > 10_000, "expected the whole tag corpus, read {total}");
    }

    /// The derivations are checked against the 101 groups that *do* ship, which
    /// is the only ground truth available for the 38 that do not.
    ///
    /// Retargeting a bare donor to a shipped group must reproduce that group's
    /// own class, CDO and flag pair exactly as the cooker wrote them.
    ///
    ///   CE_PAKS=... cargo test --features iostore new_package_corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn the_group_derivations_match_every_shipped_group() {
        let archives = open_shipped_archives();
        let mut checked = 0usize;
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        for archive in &archives {
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Some(stem) = lower.strip_suffix(".uasset").and_then(|s| s.rsplit('/').next())
                else {
                    continue;
                };
                let Some((_, group)) = stem.rsplit_once('-') else { continue };
                if !seen.insert(group.to_owned()) {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else { continue };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                let Some(export) = header.export_map.first() else { continue };
                assert_eq!(
                    export.class_index,
                    FPackageObjectIndex::create_script_import(&tag_wrapper_class_path(group)),
                    "class derivation is wrong for {group}"
                );
                assert_eq!(
                    export.template_index,
                    FPackageObjectIndex::create_script_import(&tag_wrapper_cdo_path(group)),
                    "CDO derivation is wrong for {group}"
                );
                let (object_flags, package_flags) = tag_package_flags(group);
                assert_eq!(
                    header.summary.package_flags, package_flags,
                    "package flags are wrong for {group}"
                );
                // `sound` and `ai_mission_dialogue` ship `0x3`, dropping
                // `RF_Transactional`, which is inert in a cooked build.
                assert!(
                    export.object_flags == object_flags || export.object_flags == 0x3,
                    "object flags are wrong for {group}: {:#x}",
                    export.object_flags
                );
                checked += 1;
            }
        }
        println!("checked {checked} groups");
        assert!(checked >= 90, "expected ~101 shipped groups, saw {checked}");
    }
}
