//! Which exports fail to *read*, and why?
//!
//! `ce_export_roundtrip` reports a skip count and swallows the reason, which is
//! fine when the count is zero and useless the moment it is not.
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{ExportContext, has_schema, read_export_in};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::package::builder::read_payloads;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";


fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    // An export whose class is a Blueprint-generated one is reached through a
    // package import, not the global script objects. Without this it has no
    // schema and is skipped *before* being counted — 89,762 of the corpus's
    // 1,243,749 exports, which is why every gate used to say 1,153,987.
    let (registered, no_layout) = world.register_generated_classes();
    println!("registered {registered} generated classes ({no_layout} without a layout)");
    let usmap = world.usmap();

    let mut n = 0;

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
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let resolver = world.resolver(&h, &b, &names);
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(short) = world.class_key(&h, ex.class_index) else { continue };
                let short = short.as_str();
                if !has_schema(short, usmap) {
                    continue;
                }
                if let Err(err) =
                    read_export_in(&payloads[i], &names, usmap, short, ex.object_flags, &ctx)
                {
                    n += 1;
                    println!("{} :: {short}[{i}]: {err:#}", h.package_name());
                    if n >= 10 {
                        return;
                    }
                }
            }
        }
    }
    println!("{n} exports failed to read");
}
