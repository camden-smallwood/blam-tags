//! Can a Blueprint-generated class's declared field layout be read?
//!
//! 89,762 exports have such a class and none of them decode today, because the
//! class is reached through a package import rather than the global script
//! objects. Before building the registration pass, check the premise: that
//! `UStruct::Serialize`'s prefix is readable off a `BlueprintGeneratedClass`
//! export the same way it is off a `UserDefinedStruct`.
//!
//! Run: `ce_bpgc_probe [usmap-path]`
use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::object::unversioned::{read_ustruct_layout, ExportContext};
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::world::{World, CE_HEADER_VERSION as HV, CE_TOC_VERSION as CV};
use blam_tags::iostore::zen::FZenPackageHeader;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let mut usmap = Usmap::meteorite().expect("bundled usmap");
    if let Some(p) = std::env::args().nth(1) {
        if let Ok(b) = std::fs::read(p) {
            usmap = Usmap::parse(&b).expect("parse usmap");
        }
    }
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    let world = World::open(PAKS, usmap).expect("mount Paks");
    let usmap = world.usmap();

    // Classes that provide other packages' export classes, biggest first.
    let targets = [
        "Lighting/LightCone_v2/BPC_LightCone",
        "Lighting/EnvLensFlare/BPC_EnvLensFlare",
        "Lighting/LightCone_v2/BP_LightCone",
    ];

    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();
    for a in world.archives() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !targets.iter().any(|t| lo.contains(&t.to_ascii_lowercase())) {
                continue;
            }
            if !lo.ends_with(".uasset") {
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
                let Some(class) = world.class_name(ex.class_index.raw_index()) else { continue };
                if !class.ends_with("GeneratedClass") {
                    continue;
                }
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let obj = h.name_map.get(ex.object_name);
                let key = format!("{}.{obj} [{class}]", h.package_name());
                let out = match read_ustruct_layout(
                    &b[off..end],
                    &names,
                    usmap,
                    class,
                    ex.object_flags,
                    &ctx,
                ) {
                    Ok((sup, props)) => format!(
                        "super={sup} {} own fields: {}",
                        props.len(),
                        props
                            .iter()
                            .map(|p| format!("{}:{:?}", p.name, p.ty))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    Err(e) => format!("FAILED: {e:#}"),
                };
                outcomes.insert(key, out);
            }
        }
    }
    for (k, v) in &outcomes {
        println!("{k}\n    {v}\n");
    }
}
