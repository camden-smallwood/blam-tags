//! Does `walk_export` reach the end of every export?
//!
//! They must: the walker is what every coverage census measures with, and the
//! reader is what the codec ships. A disagreement means one of them is wrong
//! about the same class, context and bytes.
//!
//! Run: `ce_walk_vs_read [substring]`
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{
    read_export_in, walk_export, ExportBlock, ExportContext,
};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let want = std::env::args().nth(1).unwrap_or_default().to_ascii_lowercase();
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    world.register_generated_classes();
    let usmap = world.usmap();

    let mut disagreements = 0u64;
    let mut checked = 0u64;
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
                let Ok(parts) = read_export_in(body, &names, usmap, &class, ex.object_flags, &ctx)
                else {
                    continue;
                };
                // An unreflected export is all one span; nothing to compare.
                if matches!(parts.block, ExportBlock::Unreflected(_)) {
                    continue;
                }
                let Ok(walk) = walk_export(body, &names, usmap, &class, ex.object_flags, &ctx)
                else {
                    continue;
                };
                checked += 1;
                let read_consumed = body.len() - parts.tail.len();
                // The walker consumes the tail as well, so it should reach the
                // end; the reader stops at the trailer. Only the walker falling
                // short is a defect.
                if walk.consumed != body.len() {
                    disagreements += 1;
                    if disagreements <= 8 {
                        println!(
                            "{} :: {} [{class}]\n    read stopped at {read_consumed}, walk at {} (export {})",
                            h.package_name(),
                            h.name_map.get(ex.object_name),
                            walk.consumed,
                            body.len()
                        );
                    }
                }
            }
        }
    }
    println!("\n{checked} exports compared, {disagreements} disagreements");
}
