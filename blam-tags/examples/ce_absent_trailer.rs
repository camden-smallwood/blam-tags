//! Which classes end their property block in the wrong place?
//!
//! The four bytes after a block are the object-guid presence bool
//! (`FLazyObjectPtr::PossiblySerializeObjectGuid`), so they are 0 or 1 for every
//! well-decoded export. `read_export` rewinds when they are neither and lets
//! them fall into the tail, which keeps the round trip exact and hides the
//! finding — the export rebuilds byte-for-byte either way. `Trailer::Absent` on
//! an export that *has* a tail is therefore a soft failure signal no gate
//! reports, and it is the only evidence that a block decode stopped early.
//!
//! Run: `ce_absent_trailer [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{
    read_export_in, ExportContext, ExportBlock, Trailer, NO_PROPERTY_BLOCK,
};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_default();
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let mut world = World::open(PAKS, usmap).expect("mount Paks");
    world.register_generated_classes();
    let usmap = world.usmap();

    // class -> (exports, bytes stranded in the tail, one example)
    let mut hits: BTreeMap<String, (u64, u64, String)> = BTreeMap::new();
    let mut total = 0u64;

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
            let names = h.name_map.copy_raw_names();
            let resolver = world.resolver(&h, &b, &names);
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for ex in &h.export_map {
                let Some(class) = world.class_key(&h, ex.class_index) else { continue };
                if NO_PROPERTY_BLOCK.contains(&class.as_str()) {
                    continue;
                }
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let Ok(parts) =
                    read_export_in(&b[off..end], &names, usmap, &class, ex.object_flags, &ctx)
                else {
                    continue;
                };
                // An unreflected block swallows everything by design, and an
                // export with no tail had nothing to misread.
                if matches!(parts.block, ExportBlock::Unreflected(_)) || parts.tail.is_empty() {
                    continue;
                }
                if parts.trailer != Trailer::Absent {
                    continue;
                }
                total += 1;
                let slot = hits.entry(class.clone()).or_default();
                slot.0 += 1;
                slot.1 += parts.tail.len() as u64;
                if slot.2.is_empty() {
                    slot.2 = format!(
                        "{} :: {} — tail starts {}",
                        h.package_name(),
                        h.name_map.get(ex.object_name),
                        parts
                            .tail
                            .iter()
                            .take(8)
                            .map(|x| format!("{x:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                }
            }
        }
    }

    println!("{total} exports whose trailer flag is not a boolean\n");
    let mut v: Vec<_> = hits.iter().collect();
    v.sort_by_key(|(_, (n, _, _))| std::cmp::Reverse(*n));
    for (class, (n, bytes, example)) in v {
        println!("{n:>5} exports, {bytes:>9} B  {class}\n        {example}");
    }
}
