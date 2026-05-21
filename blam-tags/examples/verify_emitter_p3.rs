//! Verify Tier 1.12 effect.rs P3 expansion — walk emitter property
//! curves + inline particle_physics struct via the typed walker.

use blam_tags::effect::EffectDefinition;
use blam_tags::file::TagFile;

fn main() {
    let path = std::env::args().nth(1).expect("usage: verify_emitter_p3 <effect>");
    let tag = TagFile::read(&path).expect("read");
    let eff = EffectDefinition::from_tag(&tag).expect("walk");
    println!("=== {} ===", path);
    for (ei, ev) in eff.events.iter().enumerate() {
        for (pi, ps) in ev.particle_systems.iter().enumerate() {
            for (mi, em) in ps.emitters.iter().enumerate() {
                println!("event[{ei}].ps[{pi}].emitter[{mi}] = {:?}", em.name);
                println!("  bounding: est={:.4} override={:.4}", em.bounding_radius_estimate, em.bounding_radius_override);
                println!("  particle_movement: template={:?} flags=0x{:02x} movements={}",
                    em.particle_movement.template,
                    em.particle_movement.flags,
                    em.particle_movement.movements.len());
                let props = [
                    ("translational_offset", &em.translational_offset),
                    ("relative_direction", &em.relative_direction),
                    ("emission_radius", &em.emission_radius),
                    ("emission_angle", &em.emission_angle),
                    ("emission_axis_angle", &em.emission_axis_angle),
                    ("particle_starting_count", &em.particle_starting_count),
                    ("particle_max_count", &em.particle_max_count),
                    ("particle_emission_rate", &em.particle_emission_rate),
                    ("particle_lifespan", &em.particle_lifespan),
                    ("particle_self_acceleration", &em.particle_self_acceleration),
                    ("particle_initial_velocity", &em.particle_initial_velocity),
                    ("particle_rotation", &em.particle_rotation),
                    ("particle_initial_rotation_rate", &em.particle_initial_rotation_rate),
                    ("particle_size", &em.particle_size),
                    ("particle_scale", &em.particle_scale),
                    ("particle_tint", &em.particle_tint),
                    ("particle_alpha", &em.particle_alpha),
                    ("particle_alpha_black_point", &em.particle_alpha_black_point),
                ];
                let n_with_fn = props.iter().filter(|(_, p)| p.function.is_some()).count();
                println!("  properties: {}/18 carry mapping functions", n_with_fn);
                println!("  runtime: const_per_particle=0x{:x} const_over_time=0x{:x} used_states=0x{:x}",
                    em.runtime_constant_per_particle_properties,
                    em.runtime_constant_over_time_properties,
                    em.runtime_used_particle_states);
            }
        }
    }
}
