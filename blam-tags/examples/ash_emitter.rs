//! Dump the ash_01 effect emitters' size/scale/alpha/tint + emission so we can
//! compute m_size and m_color.w and find why the black ash is faint/tiny.
use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;
use blam_tags::effects_properties::EditableProperty;

fn v(p: &EditableProperty) -> String {
    match &p.function {
        Some(f) => format!(
            "fn eval[0]={:.4} eval[.5]={:.4} eval[1]={:.4} const={:.4} in_idx={}",
            f.evaluate(0.0, 0.0), f.evaluate(0.5, 0.0), f.evaluate(1.0, 0.0),
            p.constant_value, p.input_index,
        ),
        None => format!("CONST={:.4} in_idx={}", p.constant_value, p.input_index),
    }
}

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    for eff in ["fx/scenery_fx/weather/ash/ash_01"] {
        let tag = TagFile::read(&format!("{root}/{eff}.effect")).unwrap();
        let d = EffectDefinition::from_tag(&tag).unwrap();
        println!("==== {eff} ====");
        for (si, ps) in d.events[0].particle_systems.iter().enumerate() {
            println!("-- sys{si} particle='{}' ({} emitters)", ps.particle_tag_path, ps.emitters.len());
            for (ei, em) in ps.emitters.iter().enumerate() {
                println!("   emitter{ei}:");
                println!("     particle_size      {}", v(&em.particle_size));
                println!("     particle_scale     {}", v(&em.particle_scale));
                println!("     particle_alpha     {}", v(&em.particle_alpha));
                println!("     particle_tint      {}", v(&em.particle_tint));
                println!("     emission_rate      {}", v(&em.particle_emission_rate));
                println!("     max_count          {}", v(&em.particle_max_count));
                println!("     lifespan           {}", v(&em.particle_lifespan));
            }
        }
    }
}
// (type probe appended below is via a second pass)
