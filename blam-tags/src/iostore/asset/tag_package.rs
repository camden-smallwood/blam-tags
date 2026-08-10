//! Build a Campaign Evolved tag's Unreal `.uasset` wrapper from the derivation
//! rules, with no donor package.
//!
//! Every shipped tag is a `<name>-<group>.uasset` (a Zen package holding one
//! `Blam*TagDataAsset` export) paired with a `.ubulk` whose bytes are the Reach
//! tag itself. Authoring a *new* tag therefore needs a wrapper, and until now
//! the only way to get one was to clone some existing tag of the same group and
//! rewrite its identity. That fails outright for a group the game ships no tag
//! of — 26 of Campaign Evolved's 140 defined groups, `cinematic_scene` among
//! them — because there is nothing to clone.
//!
//! There does not need to be. The wrapper is fully derivable from the package
//! path and the group, and [`build_bare_tag_package`] derives it. The claim is
//! gated by `ce_synth_bytes`, which rebuilds every in-scope shipped tag from
//! these rules alone and requires the bytes to match what shipped.
//!
//! # Scope
//!
//! Groups whose `Blam*TagDataAsset` class adds no properties over
//! [`BlamTagDataAssetBase`][base] — no `AssetReference`, so no package imports,
//! so nothing in the header depends on what the tag happens to reference. That
//! is what makes the result derivable rather than merely plausible. A group
//! that does carry properties still needs a donor, and
//! [`build_bare_tag_package`] refuses it rather than writing a wrapper that
//! omits them.
//!
//! [base]: https://github.com/EpicGames/UnrealEngine

use anyhow::{bail, Result};

use crate::iostore::container_header::{EIoContainerHeaderVersion, StoreEntry};
use crate::iostore::object::block::flattened_schema;
use crate::iostore::object::export::{write_export, Export, ExportBlock, Trailer};
use crate::iostore::object::usmap::Usmap;
use crate::iostore::object::value::{BlockLayout, PropertyBlock};
use crate::iostore::package::builder::write_package;
use crate::iostore::package::name_map::{EMappedNameType, FNameMap};
use crate::iostore::package::ue_types::FPackageObjectIndex;
use crate::iostore::package::zen::{
    EExportCommandType, EExportFilterFlags, EZenPackageVersion, FBulkDataMapEntry,
    FDependencyBundleHeader, FExportBundleEntry, FExportMapEntry, FPackageFileVersion,
    FZenPackageHeader, FZenPackageVersioningInfo,
};
use crate::iostore::writer::container_id_from_name;

/// The module every `Blam*TagDataAsset` class is declared in.
const BLAM_MODULE: &str = "/Script/BlamSynchronization";

/// The container header version Campaign Evolved's packages are written for.
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// The single bulk-data entry shape every tag package carries.
///
/// Measured, not assumed: across the 3,563 shipped tag packages with no package
/// imports there is exactly **one** distinct shape — offset 0, duplicate offset
/// -1, flags 66817, cooked index 0 — with only `serial_size` varying, which is
/// the `.ubulk` length. So the entry is a constant plus the tag's own size.
const BULK_DATA_FLAGS: u32 = 66817;

/// Campaign Evolved's package version, which every shipped tag carries.
///
/// Load-bearing for *layout* even though `is_unversioned` is true and this is
/// therefore never written: [`FZenPackageHeader::serialize`] gates whole
/// sections on it, so a header built with `Default::default()` silently omits
/// the bulk-data map and lands 51 bytes short with no error at all. A per-build
/// constant, not per-tag data.
fn ce_versioning_info() -> FZenPackageVersioningInfo {
    FZenPackageVersioningInfo {
        zen_version: EZenPackageVersion::ExportDependencies,
        package_file_version: FPackageFileVersion {
            file_version_ue4: 522,
            file_version_ue5: 1013,
        },
        licensee_version: 0,
        custom_versions: Vec::new(),
    }
}

/// `cinematic_scene` -> `BlamCinematicSceneTagDataAsset`.
///
/// Mechanical over all 101 shipped groups, and the reason a group with no
/// shipped tag is still nameable: the class exists in the game whether or not
/// anything was ever authored against it.
pub fn group_to_class(group: &str) -> String {
    let mut out = String::from("Blam");
    for part in group.split('_') {
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out.push_str("TagDataAsset");
    out
}

/// The five groups whose packages are cooked per level and carry
/// `PKG_CookGenerated`, with different object flags from everything else.
fn is_generated_group(group: &str) -> bool {
    matches!(
        group,
        "scenario"
            | "scenario_structure_bsp"
            | "scenario_structure_lighting_info"
            | "structure_design"
            | "structure_seams"
    )
}

/// True when `group`'s class adds nothing over the shared tag-data base, so its
/// wrapper needs no donor.
///
/// The three base properties are `CookedAssetsReferencedByTag`,
/// `BinaryBlobSize` and `NativeClass`; the last two are never serialized. A
/// class carrying anything else — `AssetReference` above all — names other
/// packages, and naming them is what an import map is for.
pub fn is_bare_group(group: &str, usmap: &Usmap) -> bool {
    extra_properties(&group_to_class(group), usmap).is_ok_and(|extra| extra.is_empty())
}

/// The properties a tag class declares beyond the three every wrapper has.
///
/// Empty means the group is derivable: a wrapper can be built from the group
/// alone. Non-empty is the reason it cannot, and the names are worth surfacing
/// rather than swallowing — "cannot be derived" is a dead end, while "declares
/// Model and Materials" tells you what a donor would have had to supply.
pub fn extra_properties(class: &str, usmap: &Usmap) -> Result<Vec<String>> {
    const BASE: [&str; 3] = [
        "CookedAssetsReferencedByTag",
        "BinaryBlobSize",
        "NativeClass",
    ];
    Ok(flattened_schema(class, usmap)?
        .into_iter()
        .map(|(property, _, _)| property.name.to_string())
        .filter(|name| !BASE.contains(&name.as_str()))
        .collect())
}

/// Build the `.uasset` wrapper for a brand-new tag of a bare group.
///
/// `group` is the Halo group long name (`cinematic_scene`), `package_path` the
/// target UE package (`/Game/Tags/cinematics/test-cinematic_scene`), and
/// `tag_len` the length of the `.ubulk` the wrapper is paired with. Returns the
/// package bytes and the [`StoreEntry`] the container index needs for it.
///
/// Refuses a group whose class carries properties: the caller would get a
/// structurally valid wrapper that silently declares none of them, which is the
/// failure mode cloning already had and the reason this exists.
pub fn build_bare_tag_package(
    group: &str,
    package_path: &str,
    tag_len: u64,
    usmap: &Usmap,
) -> Result<(Vec<u8>, StoreEntry)> {
    let class = group_to_class(group);
    let extra = extra_properties(&class, usmap)?;
    if !extra.is_empty() {
        bail!(
            "{group} is not a bare group: {class} declares {}, which names other packages and \
             needs an import map this cannot derive",
            extra.join(", ")
        );
    }
    let object_name = package_path
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .ok_or_else(|| anyhow::anyhow!("package path {package_path} has no object name"))?;

    // The export payload: a property block with nothing present. `schema_len` is
    // still load-bearing -- `FUnversionedHeaderBuilder::Finalize` pops trailing
    // skips only down to one, so an empty block encodes the schema's length --
    // and Campaign Evolved's tag wrappers all carry two empty leading fragments,
    // which the builder itself cannot emit.
    let schema_len = flattened_schema(&class, usmap)?.len() as u32;
    let export = Export {
        block: ExportBlock::Reflected(PropertyBlock {
            entries: Vec::new(),
            layout: BlockLayout::Unversioned {
                schema_len,
                leading_empty: 2,
            },
        }),
        trailer: Trailer::NoGuid,
        tail: Vec::new(),
    };
    let payload = write_export(&class, &export, usmap)?;

    let class_index = FPackageObjectIndex::create_script_import(&format!("{BLAM_MODULE}.{class}"));
    let template_index =
        FPackageObjectIndex::create_script_import(&format!("{BLAM_MODULE}.Default__{class}"));
    let module_index = FPackageObjectIndex::create_script_import(BLAM_MODULE);

    let mut header = FZenPackageHeader {
        container_header_version: HV,
        is_unversioned: true,
        versioning_info: ce_versioning_info(),
        ..Default::default()
    };
    header.name_map = FNameMap::create(EMappedNameType::Package);
    // Order matters and is measured: the object name is stored before the
    // package name. A block with nothing present contributes no property names
    // ahead of them.
    let object_mapped = header.name_map.store(object_name);
    let package_mapped = header.name_map.store(package_path);

    header.summary.name = package_mapped;
    header.summary.has_versioning_info = 0;
    header.summary.package_flags = if is_generated_group(group) {
        0x8800_2200
    } else {
        0x8000_2200
    };
    // The size of the *legacy* header UE keeps for bulk-data fixups. It appears
    // nowhere in the zen header, but it is derivable: the legacy name table
    // holds the package name twice, the object name once and the class name
    // twice, over a fixed remainder. Fitted, then required against every
    // in-scope shipped tag rather than left as a guess.
    header.summary.cooked_header_size =
        (617 + 2 * package_path.len() + object_name.len() + 2 * class.len()) as u32;

    // `[Default__CDO, class, module]`, which is not the declaration order one
    // would reach for. The import-map slot order is the cooker's linker order
    // and unreproducible in general -- but that only bites once *package*
    // imports interleave, and a bare group has none.
    header.import_map = vec![template_index, class_index, module_index];
    header.export_map = vec![FExportMapEntry {
        cooked_serial_offset: 0,
        cooked_serial_size: payload.len() as u64,
        object_name: object_mapped,
        // Null is all-ones. `Default` is all-zeros and would serialize eight
        // zero bytes where the cooker writes eight 0xFF -- the kind of thing
        // that dumps fine and fails in the loader.
        outer_index: FPackageObjectIndex::create_null(),
        class_index,
        super_index: FPackageObjectIndex::create_null(),
        template_index,
        public_export_hash: container_id_from_name(object_name),
        object_flags: if is_generated_group(group) { 0x1 } else { 0xb },
        filter_flags: EExportFilterFlags::None,
        padding: [0; 3],
    }];
    header.bulk_data = vec![FBulkDataMapEntry {
        serial_offset: 0,
        duplicate_serial_offset: -1,
        serial_size: tag_len as i64,
        flags: BULK_DATA_FLAGS,
        cooked_index: 0,
        pad: [0; 3],
    }];
    header.export_bundle_entries = vec![
        FExportBundleEntry {
            local_export_index: 0,
            command_type: EExportCommandType::Create,
        },
        FExportBundleEntry {
            local_export_index: 0,
            command_type: EExportCommandType::Serialize,
        },
    ];
    header.dependency_bundle_headers = vec![FDependencyBundleHeader {
        first_entry_index: 0,
        create_before_create_dependencies: 0,
        serialize_before_create_dependencies: 0,
        create_before_serialize_dependencies: 0,
        serialize_before_serialize_dependencies: 0,
    }];

    write_package(&header, &[payload], HV)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usmap() -> Usmap {
        Usmap::meteorite().expect("bundled meteorite usmap")
    }

    #[test]
    fn group_names_map_to_their_classes() {
        assert_eq!(
            group_to_class("cinematic_scene"),
            "BlamCinematicSceneTagDataAsset"
        );
        assert_eq!(group_to_class("biped"), "BlamBipedTagDataAsset");
        assert_eq!(
            group_to_class("scenario_hs_source_file"),
            "BlamScenarioHsSourceFileTagDataAsset"
        );
    }

    /// The distinction the whole module rests on, checked in both directions so
    /// that "bare" is a claim something could fail.
    #[test]
    fn bareness_separates_groups_that_carry_an_asset_reference() {
        let usmap = usmap();
        assert!(is_bare_group("cinematic_scene", &usmap));
        assert!(is_bare_group("scenario_hs_source_file", &usmap));
        assert!(!is_bare_group("biped", &usmap), "biped has AssetReference");
        assert!(!is_bare_group("cinematic", &usmap));
    }

    #[test]
    fn a_group_that_needs_a_donor_is_refused_rather_than_written_incomplete() {
        let error = build_bare_tag_package("biped", "/Game/Tags/x/y-biped", 64, &usmap())
            .expect_err("biped must not synthesize");
        assert!(
            error.to_string().contains("AssetReference"),
            "the refusal should name what is missing, got: {error}"
        );
    }

    #[test]
    fn a_bare_group_builds_a_package_that_parses_back() {
        use crate::iostore::package::zen::FZenPackageHeader;
        use crate::iostore::ue_types::EIoStoreTocVersion;
        use std::io::Cursor;

        let package = "/Game/Tags/cinematics/test-cinematic_scene";
        let (bytes, store) = build_bare_tag_package("cinematic_scene", package, 4096, &usmap())
            .expect("synthesize a cinematic_scene wrapper");
        assert!(store.export_bundles_size > 0);

        let header = FZenPackageHeader::deserialize(
            &mut Cursor::new(&bytes),
            None,
            EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash,
            HV,
            None,
        )
        .expect("the synthesized package must parse");
        assert_eq!(header.package_name(), package);
        assert_eq!(header.export_map.len(), 1);
        assert_eq!(header.import_map.len(), 3, "class, CDO and module only");
        assert!(
            header.imported_packages.is_empty(),
            "a bare group names no other package"
        );
        assert_eq!(header.bulk_data.len(), 1);
        assert_eq!(header.bulk_data[0].serial_size, 4096);
        assert_eq!(header.bulk_data[0].duplicate_serial_offset, -1);
        assert_eq!(header.bulk_data[0].flags, BULK_DATA_FLAGS);
    }
}
