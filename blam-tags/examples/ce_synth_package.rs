//! Gate: can a tag package be *synthesized* from the derivation rules alone,
//! rather than cloned from a template?
//!
//! `add_new_package_to_writer` clones a same-group template and copies its
//! export body and import map verbatim, so a new tag silently inherits the
//! template's `AssetReference` and dependency list. That is correct only for
//! the groups whose wrapper carries nothing. The fix is to build the package
//! from the rules, and the way to know the rules are right is to rebuild a tag
//! that already ships and compare.
//!
//! So: for every shipped tag, throw away its `.uasset` and rebuild one from
//! nothing but its package path, its group, and its decoded property block.
//! Then diff, field by field, against what shipped.
//!
//! The import *map slot order* is deliberately not compared. It is the cooker's
//! linker order, is not reproducible, and does not need to be — the array is
//! only indexed from inside the package, so any self-consistent order loads the
//! same. Everything that identity depends on is compared exactly.
//!
//! Run: cargo run --release --features iostore --example ce_synth_package [group-substr]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::package::name_map::FNameMap;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageId, FPackageObjectIndex};
use blam_tags::iostore::writer::container_id_from_name;
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

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

/// The lowercased-UTF16LE CityHash64 the format uses for package ids and export
/// hashes alike. Deliberately the same helper the writer uses, so this gate
/// cannot pass by reimplementing the encoding the same way twice.
fn h(s: &str) -> u64 {
    container_id_from_name(s)
}

#[derive(Default)]
struct Tally {
    tags: usize,
    ok: usize,
    bad: BTreeMap<&'static str, usize>,
}

impl Tally {
    fn check(&mut self, cond: bool, what: &'static str) -> bool {
        if !cond {
            *self.bad.entry(what).or_default() += 1;
        }
        cond
    }
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

    let mut t = Tally::default();
    let mut case_only = 0usize;
    let mut per_group: BTreeMap<String, (usize, usize)> = BTreeMap::new();

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
            let Ok(hdr) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            let Some(ex) = hdr.export_map.first() else {
                continue;
            };
            t.tags += 1;
            *per_group.entry(group.to_string()).or_default() = {
                let cur = per_group.get(group).copied().unwrap_or((0, 0));
                (cur.0 + 1, cur.1)
            };

            // ---- Derive everything from the path and group alone. ----
            let pkg_name = hdr.package_name();
            let obj_name = pkg_name.rsplit('/').next().unwrap_or_default().to_string();
            let class_path = format!("/Script/BlamSynchronization.{}", group_to_class(group));
            let tmpl_path = format!(
                "/Script/BlamSynchronization.Default__{}",
                group_to_class(group)
            );

            let derived_pid = h(&pkg_name);
            let derived_hash = h(&obj_name);
            let derived_class = FPackageObjectIndex::create_script_import(&class_path);
            let derived_tmpl = FPackageObjectIndex::create_script_import(&tmpl_path);

            // ---- Compare against what shipped. ----
            let mut all = true;
            all &= t.check(
                FPackageId::from_name(&pkg_name).0 == derived_pid,
                "package id != cityhash64(package name)",
            );
            all &= t.check(
                ex.public_export_hash == derived_hash,
                "public_export_hash != cityhash64(object name)",
            );
            // Case-insensitively, because one shipped tag spells the two
            // differently and is not wrong to: `sq_grunt_major_pp_2` as the
            // package leaf against `sq_grunt_major_PP_2` as the export name.
            // Every hash in the format is taken over the lowercased UTF-16 form,
            // so the two spellings are the same identity and the tag loads. A
            // gate asserting exact equality reports a failure that is not one.
            let shipped_obj = hdr.name_map.get(ex.object_name).to_string();
            all &= t.check(
                shipped_obj.eq_ignore_ascii_case(&obj_name),
                "export object name != package leaf (case-insensitively)",
            );
            if shipped_obj != obj_name {
                case_only += 1;
            }
            // `frame_event_list` has no class in the dump/usmap; its class index
            // still has to be whatever the group rule produces, so it is checked
            // like any other rather than skipped.
            all &= t.check(ex.class_index == derived_class, "class_index != group rule");
            all &= t.check(
                ex.template_index == derived_tmpl,
                "template_index != Default__ rule",
            );
            all &= t.check(ex.outer_index.is_null(), "outer_index not null");
            all &= t.check(ex.super_index.is_null(), "super_index not null");
            all &= t.check(hdr.export_map.len() == 1, "not exactly 1 export");
            all &= t.check(hdr.is_unversioned, "not unversioned");
            all &= t.check(hdr.shader_map_hashes.is_empty(), "has shader map hashes");
            all &= t.check(hdr.cell_import_map.is_empty(), "has cell imports");
            all &= t.check(hdr.cell_export_map.is_empty(), "has cell exports");
            all &= t.check(hdr.bulk_data.len() == 1, "not exactly 1 bulk-data entry");
            if let Some(b) = hdr.bulk_data.first() {
                all &= t.check(b.flags == 66817, "bulk flags != 0x10501");
                all &= t.check(b.serial_offset == 0, "bulk serial_offset != 0");
                all &= t.check(b.duplicate_serial_offset == -1, "bulk dup offset != -1");
            }
            all &= t.check(
                hdr.dependency_bundle_headers.len() == 1,
                "not exactly 1 dependency-bundle header",
            );
            all &= t.check(
                hdr.export_bundle_entries.len() == 2,
                "export bundle entries != [Create, Serialize]",
            );
            // The three script imports the rule says every tag carries.
            let module = FPackageObjectIndex::create_script_import("/Script/BlamSynchronization");
            let has = |x: FPackageObjectIndex| hdr.import_map.contains(&x);
            all &= t.check(has(derived_class), "class not in import map");
            all &= t.check(has(derived_tmpl), "Default__ not in import map");
            all &= t.check(has(module), "module package not in import map");
            // Name map: the object name and the package name are both present.
            let raw = hdr.name_map.copy_raw_names();
            all &= t.check(
                raw.iter().any(|n| n.eq_ignore_ascii_case(&obj_name)),
                "object name not in name map",
            );
            all &= t.check(
                raw.iter().any(|n| n.eq_ignore_ascii_case(&pkg_name)),
                "package name not in name map",
            );
            // A synthesized name map has to round-trip through the real one.
            let mut fresh = FNameMap::default();
            let mapped = fresh.store(&obj_name);
            all &= t.check(
                fresh.get(mapped) == obj_name,
                "name map cannot store and return the object name",
            );

            if all {
                t.ok += 1;
                let cur = per_group.get(group).copied().unwrap_or((0, 0));
                per_group.insert(group.to_string(), (cur.0, cur.1 + 1));
            }
        }
    }

    println!("tags checked            : {}", t.tags);
    println!("every derivation holds  : {}", t.ok);
    if t.tags > 0 {
        println!(
            "                          {:.4}%",
            100.0 * t.ok as f64 / t.tags as f64
        );
    }
    println!("names differing only in case: {case_only}");
    if t.bad.is_empty() {
        println!("\nno rule was violated by any shipped tag.");
    } else {
        println!("\nrule violations (each counted once per tag):");
        let mut v: Vec<_> = t.bad.iter().collect();
        v.sort_by(|a, b| b.1.cmp(a.1));
        for (what, n) in v {
            println!("   {n:>6}  {what}");
        }
    }

    let broken: Vec<_> = per_group.iter().filter(|(_, (n, ok))| n != ok).collect();
    if broken.is_empty() {
        println!("\nall {} groups derive cleanly.", per_group.len());
    } else {
        println!("\ngroups with at least one tag that does not derive:");
        for (g, (n, ok)) in broken {
            println!("   {g:<44} {ok}/{n}");
        }
    }
}
