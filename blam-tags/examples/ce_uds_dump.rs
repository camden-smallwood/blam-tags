//! Dump a `UUserDefinedStruct` export's recovered field chain, byte by byte.
//!
//! Run: `ce_uds_dump <substring-of-package-path>`
use std::io::Cursor;

use blam_tags::iostore::object::archive::PackageResolver;
use blam_tags::iostore::object::unversioned::{read_ustruct_layout, ExportContext};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let want = std::env::args().nth(1).expect("package substring").to_ascii_lowercase();
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let world = World::open(PAKS, usmap).expect("mount Paks");
    let usmap = world.usmap();

    for a in world.archives() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.contains(&want) || !(lo.ends_with(".uasset") || lo.ends_with(".umap")) {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let names = h.name_map.copy_raw_names();
            let resolver = world.resolver(&h, &b, &names);
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            println!("== {} ({} exports)", h.package_name(), h.export_map.len());
            for k in 0..h.import_map.len().min(12) {
                let idx = -(k as i32) - 1;
                println!("  import[{k}] (index {idx}) -> {:?}", resolver.struct_name(idx));
            }
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = world.class_key(&h, ex.class_index) else { continue };
                let obj = h.name_map.get(ex.object_name);
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                println!(
                    "  [{i}] {obj} : {class}  hash={:016x}  {} bytes",
                    ex.public_export_hash,
                    end.saturating_sub(off)
                );
                if !class.contains("Struct") && !class.contains("GeneratedClass") {
                    continue;
                }
                if off >= b.len() || off > end {
                    continue;
                }
                match read_ustruct_layout(&b[off..end], &names, usmap, &class, ex.object_flags, &ctx)
                {
                    Ok((sup, props)) => {
                        println!(
                            "      super={sup} -> {:?}   {} fields",
                            resolver.struct_name(sup),
                            props.len()
                        );
                        for p in &props {
                            println!("        {:>3} {} {:?}", p.schema_index, p.name, p.ty);
                        }
                    }
                    Err(err) => println!("      LAYOUT FAILED: {err:#}"),
                }
            }
        }
    }
}
