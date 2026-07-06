// Throwaway: print the evaluated emission/size/velocity properties for the
// s3d_turf sparks_medium effect's emitters, to settle continuous-vs-intermittent
// (emission_rate) and tiny-dots (size/scale/velocity).
use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;
use blam_tags::effects_properties::EditableProperty;

fn v(p: &EditableProperty) -> String {
    match &p.function {
        Some(f) => format!(
            "fn @0={:.4} @.5={:.4} @1={:.4} const={:.4} in_idx={}",
            f.evaluate(0.0, 0.0),
            f.evaluate(0.5, 0.0),
            f.evaluate(1.0, 0.0),
            p.constant_value,
            p.input_index,
        ),
        None => format!("CONST={:.4} in_idx={}", p.constant_value, p.input_index),
    }
}

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    let eff = "levels/multi/s3d_turf/fx/sparks_medium/sparks_medium";
    let tag = TagFile::read(&format!("{root}/{eff}.effect")).unwrap();
    let d = EffectDefinition::from_tag(&tag).unwrap();
    println!("==== {eff} ====");
    for (evi, ev) in d.events.iter().enumerate() {
        println!("event[{evi}] delay={:?} duration={:?}", ev.delay_bounds, ev.duration_bounds);
        for (si, ps) in ev.particle_systems.iter().enumerate() {
            println!("-- sys{si} particle='{}' ({} emitters)", ps.particle_tag_path, ps.emitters.len());
            for (ei, em) in ps.emitters.iter().enumerate() {
                println!("   emitter{ei} shape={:?}", em.emission_shape.get());
                println!("     starting_count   {}", v(&em.particle_starting_count));
                println!("     emission_rate    {}", v(&em.particle_emission_rate));
                println!("     max_count        {}", v(&em.particle_max_count));
                println!("     lifespan         {}", v(&em.particle_lifespan));
                println!("     size             {}", v(&em.particle_size));
                println!("     scale            {}", v(&em.particle_scale));
                println!("     initial_velocity {}", v(&em.particle_initial_velocity));
                println!("     self_accel       {}", v(&em.particle_self_acceleration));
                println!("     emission_radius  {}", v(&em.emission_radius));
                println!("     emission_angle   {}", v(&em.emission_angle));
                println!("     tint             {}", v(&em.particle_tint));
                println!("     alpha            {}", v(&em.particle_alpha));
            }
        }
    }
}
