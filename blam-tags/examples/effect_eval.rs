// Throwaway: evaluate emission/size/velocity properties for the riverworld
// waterfall effects (top → mid → base). Complements blam-tag-shell (which shows
// raw curve data) with evaluated values.
use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;
use blam_tags::effects_properties::EditableProperty;

fn v(p: &EditableProperty) -> String {
    match &p.function {
        Some(f) => format!("[{:.3} .. {:.3}]", f.evaluate(0.0, 0.0), f.evaluate(1.0, 0.0)),
        None => format!("{:.3}", p.constant_value),
    }
}

fn main() {
    let path = std::env::args().nth(1).unwrap(); {
        let tag = TagFile::read(&path).unwrap();
        let d = EffectDefinition::from_tag(&tag).unwrap();
        println!("==== {path} ====");
        for ev in &d.events {
            for (si, ps) in ev.particle_systems.iter().enumerate() {
                let particle = ps.particle_tag_path.rsplit(['\\', '/']).next().unwrap_or("");
                for em in &ps.emitters {
                    println!(
                        "sys{si} {particle:<14} shape={:?}  start={} rate={} max={} life={} size={} scale={} vel={} accel={} radius={} angle={} alpha={}",
                        em.emission_shape.get(),
                        v(&em.particle_starting_count),
                        v(&em.particle_emission_rate),
                        v(&em.particle_max_count),
                        v(&em.particle_lifespan),
                        v(&em.particle_size),
                        v(&em.particle_scale),
                        v(&em.particle_initial_velocity),
                        v(&em.particle_self_acceleration),
                        v(&em.emission_radius),
                        v(&em.emission_angle),
                        v(&em.particle_alpha),
                    );
                }
            }
        }
    }
}
