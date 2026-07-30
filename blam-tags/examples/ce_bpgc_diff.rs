//! Why does a Blueprint-generated class's export not round-trip?
//!
//! Run: `ce_bpgc_diff <substring-of-package-path>`
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{
    flattened_schema, parse_header, read_export_in, write_export_in, ExportContext,
};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn main() {
    let want = std::env::args().nth(1).expect("package substring").to_ascii_lowercase();
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    world.register_generated_classes();
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
            for ex in &h.export_map {
                let Some(class) = world.class_key(&h, ex.class_index) else { continue };
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let parts = match read_export_in(body, &names, usmap, &class, ex.object_flags, &ctx)
                {
                    Ok(p) => p,
                    Err(err) => {
                        println!("{} :: {class}\n  READ FAILED: {err:#}\n", h.package_name());
                        continue;
                    }
                };
                let out = match write_export_in(&class, &parts, usmap, Some(&resolver)) {
                    Ok(o) => o,
                    Err(err) => {
                        println!("{} :: {class}\n  WRITE FAILED: {err:#}\n", h.package_name());
                        continue;
                    }
                };
                if out == body {
                    continue;
                }
                println!("{} :: {}", h.package_name(), h.name_map.get(ex.object_name));
                println!("  class {class}");
                if let Ok(flat) = flattened_schema(&class, usmap) {
                    println!("  flattened schema ({} slots):", flat.len());
                    for (i, (p, slot, owner)) in flat.iter().enumerate() {
                        println!("    {i:>3} {}[{slot}] {:?}  <- {owner}", p.name, p.ty);
                    }
                }
                if let Ok((hdr, used)) = parse_header(body) {
                    println!("  header {used}B present={:?}", hdr.present);
                }
                println!("  in  {}", hex(body));
                println!("  out {}", hex(&out));
                println!("  block {:#?}\n", parts.properties());
                return;
            }
        }
    }
}
