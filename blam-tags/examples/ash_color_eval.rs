//! Compare the prt3 appearance curves (color/intensity/alpha) of the two ash
//! particle types to find why the "black ash" (ash_falling_02) renders invisible.
use blam_tags::TagFile;
use blam_tags::particle::ParticleDefinition;
use blam_tags::tag_function::TagFunction;

fn dump(name: &str, f: &Option<TagFunction>, cv: f32, fl: u8) {
    match f {
        Some(f) => {
            let h = f.header();
            println!(
                "  {name:<10} fn={:?} graph={:?} colors_argb={:08X?} eval[0]={:.3} eval[.5]={:.3} eval[1]={:.3} const={cv} flags={fl:#04x}",
                f.function_type(), h.color_graph_type, &h.colors,
                f.evaluate(0.0, 0.0), f.evaluate(0.5, 0.0), f.evaluate(1.0, 0.0),
            );
        }
        None => println!("  {name:<10} CONSTANT const={cv} flags={fl:#04x}"),
    }
}

fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    for p in ["ash_falling_01", "ash_falling_02"] {
        let path = format!("{root}/fx/particles/weather/{p}.particle");
        let tag = TagFile::read(&path).unwrap();
        let d = ParticleDefinition::from_tag(&tag).unwrap();
        println!(
            "\n==== {p}  used_states={:#010x} const_per_particle={:#010x} const_over_time={:#010x} ====",
            d.runtime_used_particle_states,
            d.runtime_constant_per_particle_properties,
            d.runtime_constant_over_time_properties,
        );
        dump("color", &d.color.function, d.color.constant_value, d.color.runtime_flags);
        dump("intensity", &d.intensity.function, d.intensity.constant_value, d.intensity.runtime_flags);
        dump("alpha", &d.alpha.function, d.alpha.constant_value, d.alpha.runtime_flags);
    }
}
