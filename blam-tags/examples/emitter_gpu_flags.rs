use blam_tags::effect::EffectDefinition;
use blam_tags::TagFile;

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    let effects = [
        "fx/scenery_fx/weather/snow/snow_turf/snow_turf.effect",
        "fx/scenery_fx/weather/ash/ash_02.effect",
        "fx/scenery_fx/weather/rain/rain_heavy/rain_heavy.effect",
    ];
    for e in effects {
        let path = format!("{root}/{e}");
        let Ok(tag) = TagFile::read(&path) else { println!("{e}: read fail"); continue };
        let Ok(def) = EffectDefinition::from_tag(&tag) else { println!("{e}: decode fail"); continue };
        println!("\n#### {} ####", e.rsplit('/').next().unwrap());
        for (ei, ev) in def.events.iter().enumerate() {
            for (si, sys) in ev.particle_systems.iter().enumerate() {
                println!("  ev{ei} sys{si} '{}' sysflags(u16)={:#06x}",
                    sys.particle_tag_path.rsplit('/').next().unwrap_or(""),
                    flags_raw(&sys.flags));
                for (mi, em) in sys.emitters.iter().enumerate() {
                    use blam_tags::effect::EmitterDefinitionFlags as EF;
                    println!("    emitter{mi} flags={:?}  [can_be_gpu={}]",
                        em.flags, em.flags.contains(EF::CanBeGpu));
                }
            }
        }
    }
}

fn flags_raw(f: &blam_tags::typed_enums::Flags<blam_tags::effect::ParticleSystemFlags, u16>) -> u16 {
    use blam_tags::effect::ParticleSystemFlags as F;
    use strum::VariantArray;
    let mut r = 0u16;
    for v in F::VARIANTS { if f.contains(*v) { r |= 1 << (*v as u16); } }
    r
}
