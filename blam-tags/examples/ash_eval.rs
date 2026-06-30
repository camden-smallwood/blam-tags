//! Evaluate the key particle-emitter property curves for the ash weather
//! effects, since blam-tag-shell renders the mapping functions opaquely.
use blam_tags::effect::EffectDefinition;
use blam_tags::effects_properties::EditableProperty;
use blam_tags::TagFile;

fn ev(p: &EditableProperty, input: f32, range: f32) -> f32 {
    match &p.function {
        Some(f) => f.evaluate(input, range),
        None => f32::NAN,
    }
}

fn dump(name: &str, p: &EditableProperty) {
    // range 0 = low correlate, range 1 = high correlate; input 0 = age/start.
    let lo = ev(p, 0.0, 0.0);
    let hi = ev(p, 0.0, 1.0);
    let ftype = p.function.as_ref().map(|f| format!("{:?}", f.header().function_type));
    println!(
        "    {name:<26} in={} range={} mod={} -> [{lo:.4} .. {hi:.4}]  fn={:?} ranged_flags=runtime_flags:{:#x}",
        p.input_index, p.range_input_index, p.output_modifier_type, ftype, p.runtime_flags
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths: Vec<String> = if args.is_empty() {
        vec![format!("{root}/fx/scenery_fx/weather/ash/ash_02.effect"),
             format!("{root}/fx/scenery_fx/weather/ash/ash_01.effect")]
    } else { args.iter().map(|a| format!("{root}/{a}")).collect() };
    for path in &paths {
        let eff = path.rsplit('/').next().unwrap();
        let tag = TagFile::read(&path)?;
        let def = EffectDefinition::from_tag(&tag)?;
        println!("\n################ {eff}.effect  events={} ################", def.events.len());
        for (ei, event) in def.events.iter().enumerate() {
            for (si, sys) in event.particle_systems.iter().enumerate() {
                println!(
                    "== event[{ei}] system[{si}] particle='{}' coord={:?} env={:?} lod_in={} lod_out={} ==",
                    sys.particle_tag_path, sys.coordinate_system, sys.environment,
                    sys.lod_in_distance, sys.lod_out_distance
                );
                for (mi, em) in sys.emitters.iter().enumerate() {
                    println!(
                        "  emitter[{mi}] shape={:?} bounding_radius={}/{}",
                        em.emission_shape, em.bounding_radius_estimate, em.bounding_radius_override
                    );
                    dump("emission_radius", &em.emission_radius);
                    dump("emission_angle", &em.emission_angle);
                    dump("particle_starting_count", &em.particle_starting_count);
                    dump("particle_max_count", &em.particle_max_count);
                    dump("particle_emission_rate", &em.particle_emission_rate);
                    dump("particle_lifespan", &em.particle_lifespan);
                    dump("particle_initial_velocity", &em.particle_initial_velocity);
                    dump("particle_size", &em.particle_size);
                    dump("particle_scale", &em.particle_scale);
                }
            }
        }
    }
    Ok(())
}
