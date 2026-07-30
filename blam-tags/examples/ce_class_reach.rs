//! How many exports does the corpus have, and how many can we name the class of?
//!
//! Every gate resolves an export's class through the global script objects and
//! skips it when that lookup misses. That is correct for a native class and
//! silently wrong for a Blueprint-generated one, whose `class_index` is a
//! *package* import pointing at another package's export. Those exports have
//! never been counted, let alone round-tripped, and "1,153,987 exports" is the
//! size of what we looked at rather than of the corpus.
//!
//! Run: `ce_class_reach [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::has_schema;
use blam_tags::iostore::package::ue_types::FPackageObjectIndexType;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    let (registered, failed) = world.register_generated_classes();
    println!("registered {registered} generated classes ({failed} exports yielded no layout)\n");
    let usmap = world.usmap();

    let mut kinds: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut total = 0u64;
    // Which other-package classes, by the imported package they live in.
    let mut bp_classes: BTreeMap<String, u64> = BTreeMap::new();

    for a in world.archives() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            for ex in &h.export_map {
                total += 1;
                let ci = ex.class_index;
                match ci.kind() {
                    FPackageObjectIndexType::ScriptImport => {
                        let named = world.class_name(ci.raw_index());
                        *kinds
                            .entry(match named {
                                Some(n) if has_schema(n, usmap) => "script class, has schema",
                                Some(_) => "script class, no schema",
                                None => "script class, not in global.utoc",
                            })
                            .or_default() += 1;
                    }
                    FPackageObjectIndexType::PackageImport => {
                        let key = world.class_key(&h, ci);
                        *kinds
                            .entry(match key.as_deref() {
                                Some(k) if has_schema(k, usmap) => {
                                    "other package's class, now has a schema"
                                }
                                Some(_) => "other package's class, still no schema",
                                None => "other package's class, unresolvable key",
                            })
                            .or_default() += 1;
                        if let Some(r) = ci.package_import() {
                            if let Some(p) =
                                h.imported_package_names.get(r.imported_package_index as usize)
                            {
                                *bp_classes.entry(p.clone()).or_default() += 1;
                            }
                        }
                    }
                    FPackageObjectIndexType::Export => {
                        let key = world.class_key(&h, ci);
                        *kinds
                            .entry(match key.as_deref() {
                                Some(k) if has_schema(k, usmap) => {
                                    "own-package class, now has a schema"
                                }
                                Some(_) => "own-package class, still no schema",
                                None => "own-package class, unresolvable key",
                            })
                            .or_default() += 1;
                    }
                    _ => *kinds.entry("class index null/other").or_default() += 1,
                }
            }
        }
    }

    println!("exports in the corpus  {total}");
    for (k, n) in &kinds {
        println!("  {n:>9}  {k}");
    }
    println!("\ntop packages providing an export's class:");
    let mut v: Vec<_> = bp_classes.iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (p, n) in v.iter().take(15) {
        println!("  {n:>7}  {p}");
    }
    println!("  ({} distinct class-providing packages)", bp_classes.len());
}
