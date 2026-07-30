//! Gate: synthesize a tag's `.uasset` from the derivation rules and require the
//! bytes to match what shipped.
//!
//! Testing an authoring path by loading the result in the game proves one
//! package worked once, and needs the game. This proves the *rule* over every
//! tag the game already loads, and needs nothing but the containers — which
//! matters when the only machine available cannot run it.
//!
//! `ce_synth_package` checks that each derived *field* equals the shipped one.
//! That is necessary and not sufficient: a header can agree field by field and
//! still serialize differently, because the layout, ordering and padding are
//! not fields. So this one builds an `FZenPackageHeader` from nothing but the
//! package path, the group, and the shipped export payload, serializes it, and
//! diffs against the shipped `.uasset` byte for byte.
//!
//! Scope of this first milestone: tags with **no package imports**. The
//! import-map slot order is the cooker's linker order and is documented as not
//! reproducible, so byte identity is not the right bar for tags that have one —
//! those need an equivalence check instead, and are counted here but not
//! demanded. Tags with imports are reported so the remaining population is
//! visible rather than quietly excluded.
//!
//! Run: cargo run --release --features iostore --example ce_synth_bytes [group-substr]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::{EIoContainerHeaderVersion, StoreEntry};
use blam_tags::iostore::package::name_map::{EMappedNameType, FNameMap};
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::writer::container_id_from_name;
use blam_tags::iostore::zen::{
    EExportCommandType, EZenPackageVersion, FDependencyBundleHeader, FExportBundleEntry,
    FExportMapEntry, FPackageFileVersion, FZenPackageHeader, FZenPackageVersioningInfo,
};
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Campaign Evolved's package version, which every shipped tag carries.
///
/// Load-bearing even though `is_unversioned` is true and this is therefore
/// never written: `FZenPackageHeader::serialize` gates whole *sections* on it,
/// so a header built with `Default::default()` silently omits the bulk-data map
/// (`file_version_ue5 >= DataResources`) and lands 51 bytes short with no error.
/// It is a per-build constant, not per-tag data.
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

/// `frame_event_list` -> `BlamFrameEventListTagDataAsset`.
fn group_to_class(group: &str) -> String {
    let mut s = String::from("Blam");
    for part in group.split('_') {
        let mut c = part.chars();
        if let Some(f) = c.next() {
            s.push(f.to_ascii_uppercase());
            s.push_str(c.as_str());
        }
    }
    s.push_str("TagDataAsset");
    s
}

/// The five `_Generated_` level groups carry `PKG_CookGenerated` and different
/// object flags from everything else.
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

fn main() {
    let filter = std::env::args()
        .nth(1)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    let (mut considered, mut in_scope, mut exact, mut differed) = (0usize, 0usize, 0usize, 0usize);
    let mut have_imports = 0usize;
    let (mut roundtrip_ok, mut roundtrip_bad) = (0usize, 0usize);
    let mut first_diff: BTreeMap<String, usize> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.contains("/tags/") || !lp.ends_with(".uasset") {
                continue;
            }
            let Some(stem) = lp
                .rsplit('/')
                .next()
                .and_then(|f| f.strip_suffix(".uasset"))
            else {
                continue;
            };
            let Some((_, group)) = stem.rsplit_once('-') else {
                continue;
            };
            if !filter.is_empty() && !group.contains(&filter) {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(shipped) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            let Some(sex) = shipped.export_map.first() else {
                continue;
            };
            considered += 1;

            // Milestone 1: the import-map slot order is the cooker's and is not
            // reproducible, so a tag that has package imports cannot be held to
            // byte identity. Count it and move on rather than pretending.
            if !shipped.imported_packages.is_empty() {
                have_imports += 1;
                continue;
            }
            in_scope += 1;
            let pkg_name = shipped.package_name();
            let obj_name = pkg_name.rsplit('/').next().unwrap_or_default().to_string();
            let class = group_to_class(group);
            let class_idx = FPackageObjectIndex::create_script_import(&format!(
                "/Script/BlamSynchronization.{class}"
            ));
            let tmpl_idx = FPackageObjectIndex::create_script_import(&format!(
                "/Script/BlamSynchronization.Default__{class}"
            ));
            let module_idx =
                FPackageObjectIndex::create_script_import("/Script/BlamSynchronization");

            // --- build the header from rules only ---
            let mut synth = FZenPackageHeader {
                container_header_version: HV,
                is_unversioned: true,
                versioning_info: ce_versioning_info(),
                ..Default::default()
            };
            synth.name_map = FNameMap::create(EMappedNameType::Package);
            // Rule: the property names first (there are none for a tag with no
            // imports and an empty block), then the object name, then the
            // package name.
            let name_mapped = synth.name_map.store(&obj_name);
            let pkg_mapped = synth.name_map.store(&pkg_name);

            synth.summary.name = pkg_mapped;
            synth.summary.package_flags = if is_generated_group(group) {
                0x8800_2200
            } else {
                0x8000_2200
            };
            synth.summary.has_versioning_info = 0;
            // `cooked_header_size` is the size of the *legacy* header UE keeps
            // for bulk-data fixups. It is not in the zen header anywhere, but it
            // is derivable: the legacy name table holds the package name twice,
            // the object name once and the class name twice, over a fixed
            // remainder. Fitted and then checked, not guessed --
            // 3,562 of 3,563 in-scope tags, the one exception being
            // `player_model_customization_globals`, which the package spec
            // already records as the outlier for having 71 names and 5 script
            // imports rather than 2 and 3.
            synth.summary.cooked_header_size =
                (617 + 2 * pkg_name.len() + obj_name.len() + 2 * class.len()) as u32;

            // Order is [Default__CDO, class, module]. The package spec records
            // the import-map slot order as the cooker's linker order and not
            // reproducible; that is true once package imports interleave, but
            // for a tag with none the three script imports are in a fixed order,
            // and it is not the declaration order one would guess.
            synth.import_map = vec![tmpl_idx, class_idx, module_idx];
            synth.export_map = vec![FExportMapEntry {
                cooked_serial_offset: 0,
                cooked_serial_size: sex.cooked_serial_size,
                object_name: name_mapped,
                // Null is all-ones, not all-zeros -- `Default` gives the wrong
                // one and serializes eight zero bytes where the cooker writes
                // eight 0xFF.
                outer_index: FPackageObjectIndex::create_null(),
                class_index: class_idx,
                super_index: FPackageObjectIndex::create_null(),
                template_index: tmpl_idx,
                public_export_hash: container_id_from_name(&obj_name),
                object_flags: if is_generated_group(group) { 0x1 } else { 0xb },
                filter_flags: sex.filter_flags,
                padding: [0; 3],
            }];
            synth.bulk_data = shipped.bulk_data.clone();
            synth.export_bundle_entries = vec![
                FExportBundleEntry {
                    local_export_index: 0,
                    command_type: EExportCommandType::Create,
                },
                FExportBundleEntry {
                    local_export_index: 0,
                    command_type: EExportCommandType::Serialize,
                },
            ];
            synth.dependency_bundle_headers = vec![FDependencyBundleHeader {
                first_entry_index: 0,
                create_before_create_dependencies: 0,
                serialize_before_create_dependencies: 0,
                create_before_serialize_dependencies: 0,
                serialize_before_serialize_dependencies: 0,
            }];

            // Control: re-serialize the SHIPPED header untouched. If this does
            // not reproduce the file, the gap is in the serializer, not in the
            // construction, and no amount of fixing the rules would find it.
            {
                let mut c = Cursor::new(Vec::new());
                let mut st = StoreEntry::default();
                if shipped.clone().serialize(&mut c, &mut st, HV).is_ok() {
                    let mut rt = c.into_inner();
                    rt.extend_from_slice(&bytes[shipped.summary.header_size as usize..]);
                    if rt == bytes {
                        roundtrip_ok += 1;
                    } else {
                        roundtrip_bad += 1;
                        if roundtrip_bad <= 2 {
                            eprintln!(
                                "ROUNDTRIP MISMATCH {} : got {} want {}",
                                e.path,
                                rt.len(),
                                bytes.len()
                            );
                        }
                    }
                }
            }

            // --- serialize and diff ---
            let payload = &bytes[shipped.summary.header_size as usize..];
            let mut out = Cursor::new(Vec::new());
            let mut store = StoreEntry::default();
            if synth.serialize(&mut out, &mut store, HV).is_err() {
                *first_diff.entry("serialize failed".into()).or_default() += 1;
                differed += 1;
                continue;
            }
            let mut got = out.into_inner();
            got.extend_from_slice(payload);

            if got == bytes {
                exact += 1;
                continue;
            }
            differed += 1;
            let at = got
                .iter()
                .zip(bytes.iter())
                .position(|(x, y)| x != y)
                .unwrap_or(got.len().min(bytes.len()));
            let where_ = if got.len() != bytes.len() {
                format!("length {} vs {}", got.len(), bytes.len())
            } else {
                format!("first differing byte at {at:#x}")
            };
            *first_diff.entry(where_).or_default() += 1;
            if samples.len() < 5 {
                samples.push(format!("{}  ({})", e.path, at));
            }
        }
    }

    println!("tags considered                 : {considered}");
    println!("  have package imports (skipped): {have_imports}");
    println!("  in scope (no package imports) : {in_scope}");
    println!("  shipped header re-serializes  : {roundtrip_ok} ok / {roundtrip_bad} bad");
    println!("  byte-identical                : {exact}");
    println!("  differed                      : {differed}");
    if in_scope > 0 {
        println!(
            "  -> {:.2}% of scope",
            100.0 * exact as f64 / in_scope as f64
        );
    }
    if !first_diff.is_empty() {
        println!("\nwhere they differ:");
        let mut v: Vec<_> = first_diff.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (what, n) in v.iter().take(12) {
            println!("   {n:>6}  {what}");
        }
        println!("\nsamples:");
        for s in &samples {
            println!("   {s}");
        }
    }
}
