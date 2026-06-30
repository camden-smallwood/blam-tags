//! Dump near-fade + sort fields for every particle system in the ash weather
//! effects — to check whether protomorph's near-fade is suppressing density.
use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    for p in [
        "fx/scenery_fx/weather/ash/ash_01",
        "fx/scenery_fx/weather/ash/ash_02",
        "fx/scenery_fx/weather/falling_ash/falling_ash",
    ] {
        let path = format!("{root}/{p}.effect");
        let tag = match TagFile::read(&path) {
            Ok(t) => t,
            Err(e) => { println!("\n== {p}: read err {e}"); continue; }
        };
        let d = EffectDefinition::from_tag(&tag).unwrap();
        println!("\n==== {p} ({} events) ====", d.events.len());
        for (ei, ev) in d.events.iter().enumerate() {
            for (si, ps) in ev.particle_systems.iter().enumerate() {
                println!(
                    "  ev{ei} sys{si} particle='{}'\n     near_fade_range={} cutoff={} override={} flags={:?} sort_bias={} camera_mode={:?}",
                    ps.particle_tag_path,
                    ps.near_fade_range, ps.near_fade_cutoff, ps.near_fade_override,
                    ps.flags, ps.sort_bias, ps.camera_mode,
                );
            }
        }
    }
}
