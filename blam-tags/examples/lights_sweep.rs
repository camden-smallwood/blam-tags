//! Sweep scenario_lights_resource files: count placed lights + their
//! `lightmap type` (baked vs dynamic). Usage: lights_sweep <FILE...>
//!
//! Structure: root `scenario_lights_resource_struct` → block `lights`
//! (one element per placed light) → struct `light data` with the
//! `type` and `lightmap type` enums.

use std::collections::BTreeMap;
use std::error::Error;
use blam_tags::{TagFile, TagStruct};

fn collect_lights(s: TagStruct<'_>, out: &mut Vec<(String, String)>) {
    for field in s.fields() {
        if field.name() == "lights" {
            if let Some(block) = field.as_block() {
                for elem in block.iter() {
                    let ld = elem.field("light data").and_then(|f| f.as_struct());
                    let (ty, lm) = match ld {
                        Some(ld) => (
                            ld.read_enum_name("type").unwrap_or_else(|| "?".into()),
                            ld.read_enum_name("lightmap type")
                                .unwrap_or_else(|| "<none>".into()),
                        ),
                        None => ("?".into(), "<no light data>".into()),
                    };
                    out.push((ty, lm));
                }
            }
        } else if let Some(res) = field.as_resource() {
            if let Some(h) = res.as_struct() { collect_lights(h, out); }
        } else if let Some(nested) = field.as_struct() {
            collect_lights(nested, out);
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let (mut g_dyn, mut g_baked) = (0usize, 0usize);
    for path in std::env::args().skip(1) {
        let tag = match TagFile::read(&path) {
            Ok(t) => t,
            Err(e) => { println!("{:42} ERR {e}", short(&path)); continue; }
        };
        let mut lights = Vec::new();
        collect_lights(tag.root(), &mut lights);
        let mut dynamic = 0;
        let mut baked = 0;
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for (_ty, lm) in &lights {
            if lm.contains("lightmaps only") { baked += 1; } else { dynamic += 1; }
            *kinds.entry(lm.clone()).or_default() += 1;
        }
        g_dyn += dynamic; g_baked += baked;
        let ks: Vec<String> = kinds.iter().map(|(k, v)| format!("{v}×{k}")).collect();
        println!("{:42} lights={:2}  dyn={:2} baked={:2}  [{}]",
                 short(&path), lights.len(), dynamic, baked, ks.join(", "));
    }
    println!("\nTOTAL: dynamic-capable={g_dyn}  baked-only={g_baked}");
    Ok(())
}

fn short(p: &str) -> String {
    p.rsplit('/').find(|s| s.ends_with(".scenario_lights_resource"))
        .unwrap_or(p).replace(".scenario_lights_resource", "")
}
