use blam_tags::TagFile;
use blam_tags::particle::ParticleDefinition;
fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    for p in ["ash_falling_01","ash_falling_02"] {
        let tag = TagFile::read(&format!("{root}/fx/particles/weather/{p}.particle")).unwrap();
        let prt = ParticleDefinition::from_tag(&tag).unwrap();
        let i = &prt.intensity;
        print!("{p} intensity: const_val={:.3} ", i.constant_value);
        match i.function.as_ref() {
            None => println!("NO function"),
            Some(f) => {
                let h = f.header();
                println!("type={:?} ranged={} clamp=[{:.3},{:.3}] eval(0)={:.3} eval(.5)={:.3} eval(1)={:.3}",
                    f.function_type(), h.flags.is_ranged(), h.clamp_range_min, h.clamp_range_max,
                    f.evaluate(0.0,0.0), f.evaluate(0.5,0.5), f.evaluate(1.0,1.0));
            }
        }
    }
}
