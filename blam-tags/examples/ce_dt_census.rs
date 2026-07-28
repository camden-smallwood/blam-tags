//! What `RowStruct` does every cooked `UDataTable`/`UCompositeDataTable` point
//! at, and can the schema for it be reached?
//!
//! The rows are a property block per row against the *row struct's* schema, so
//! before writing any reader we need to know whether that struct is a native
//! one (in the `.usmap`) or a `UUserDefinedStruct` exported by some other
//! package — the two need very different plumbing.
//!
//! Run: ce_dt_census
use std::collections::BTreeMap;
use std::io::Cursor;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex, FPackageObjectIndexType};
use blam_tags::iostore::unversioned::{read_export_struct, PropValue};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut usmap = Usmap::parse(blam_tags::iostore::usmap::METEORITE_USMAP).unwrap();
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let script = ScriptObjects::load(format!("{PAKS}/global.utoc")).unwrap();

    let classes = [
        ("DataTable", FPackageObjectIndex::create_script_import("/Script/Engine.DataTable")),
        ("CompositeDataTable", FPackageObjectIndex::create_script_import("/Script/Engine.CompositeDataTable")),
    ];

    let mut u: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    u.sort();

    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut total = 0usize;
    for utoc in &u {
        let Ok(a) = IoStoreArchive::open(utoc) else { continue };
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") { continue }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None) else { continue };
            for (cls, idx) in &classes {
                for ex in h.export_map.iter().filter(|x| x.class_index == *idx) {
                    total += 1;
                    let names = h.name_map.copy_raw_names();
                    let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                    let end = (off + ex.cooked_serial_size as usize).min(b.len());
                    if off >= b.len() { continue }
                    let Ok(props) = read_export_struct(&b[off..end], &names, &usmap, cls) else {
                        *tally.entry("<property block failed>".into()).or_default() += 1;
                        continue;
                    };
                    let Some(PropValue::Object(pi)) = props.get("RowStruct") else {
                        *tally.entry("<no RowStruct property>".into()).or_default() += 1;
                        continue;
                    };
                    // FPackageIndex: negative = import (−idx−1), positive = export.
                    let desc = if *pi < 0 {
                        let i = (-*pi - 1) as usize;
                        match h.import_map.get(i) {
                            None => "<import out of range>".to_string(),
                            Some(oi) => match oi.kind() {
                                FPackageObjectIndexType::ScriptImport => {
                                    *kinds.entry("script import (native)").or_default() += 1;
                                    script.resolve(oi.raw_index()).unwrap_or("<unresolved script>").to_string()
                                }
                                FPackageObjectIndexType::PackageImport => {
                                    *kinds.entry("package import (user-defined, other package)").or_default() += 1;
                                    let r = oi.package_import().unwrap();
                                    h.imported_package_names.get(r.imported_package_index as usize)
                                        .cloned().unwrap_or_else(|| format!("<pkg {}>", r.imported_package_index))
                                }
                                k => format!("<{k:?}>"),
                            },
                        }
                    } else if *pi > 0 {
                        *kinds.entry("export in same package").or_default() += 1;
                        format!("<export {}>", *pi - 1)
                    } else {
                        *kinds.entry("null RowStruct").or_default() += 1;
                        "<null>".to_string()
                    };
                    *tally.entry(desc).or_default() += 1;
                }
            }
        }
    }
    println!("{total} DataTable/CompositeDataTable exports\n");
    for (k, n) in &kinds { println!("  {n:5}  {k}"); }
    println!("\nrow structs:");
    let mut v: Vec<_> = tally.into_iter().collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (k, n) in v { println!("  {n:5}  {k}"); }
}
