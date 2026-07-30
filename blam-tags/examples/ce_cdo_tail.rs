//! What is in the tail of a Blueprint CDO that no arm can read?
//! Run: `ce_cdo_tail <substring>`
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{read_export_in, ExportContext, Trailer};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let want = std::env::args().nth(1).expect("substring").to_ascii_lowercase();
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    world.register_generated_classes();
    let usmap = world.usmap();

    for a in world.archives() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.contains(&want) || !lo.ends_with(".uasset") {
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
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = world.class_key(&h, ex.class_index) else { continue };
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let Ok(parts) = read_export_in(&b[off..end], &names, usmap, &class, ex.object_flags, &ctx)
                else {
                    continue;
                };
                if parts.tail.is_empty() {
                    continue;
                }
                // Walk the super chain so we can see which arm should own it.
                let mut chain = Vec::new();
                let mut cur = class.clone();
                for _ in 0..32 {
                    chain.push(cur.rsplit('/').next().unwrap_or(&cur).to_string());
                    match usmap.get(&cur).and_then(|s| s.super_name.clone()) {
                        Some(s) => cur = s,
                        None => break,
                    }
                }
                println!(
                    "[{i}] {} : flags={:#x} cdo={} trailer={:?}\n    tail {} bytes: {}\n    chain: {}",
                    h.name_map.get(ex.object_name),
                    ex.object_flags,
                    ex.object_flags & 0x10 != 0,
                    matches!(parts.trailer, Trailer::Absent),
                    parts.tail.len(),
                    parts.tail.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" "),
                    chain.join(" <- ")
                );
            }
            return;
        }
    }
}
