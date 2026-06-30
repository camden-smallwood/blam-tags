use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;
use blam_tags::particle::ParticleDefinition;

fn argb(c: u32) -> String {
    format!("A{} R{} G{} B{}", (c>>24)&0xff, (c>>16)&0xff, (c>>8)&0xff, c&0xff)
}
fn dump(label: &str, f: Option<&blam_tags::tag_function::TagFunction>) {
    match f {
        None => println!("  {label}: NO function (constant)"),
        Some(f) => {
            let h = f.header();
            print!("  {label}: type={:?} graph={:?} stops=[", f.function_type(), h.color_graph_type);
            for i in 0..4 { print!("{} | ", argb(h.colors[i])); }
            println!("]");
        }
    }
}
fn main() {
    let root = "/Users/camden/Halo/halo3_mcc/tags";
    let tag = TagFile::read(&format!("{root}/fx/scenery_fx/weather/ash/ash_01.effect")).unwrap();
    let d = EffectDefinition::from_tag(&tag).unwrap();
    for (si, ps) in d.events[0].particle_systems.iter().enumerate() {
        println!("== sys{si} emitter ==");
        dump("particle_tint", ps.emitters[0].particle_tint.function.as_ref());
    }
    for p in ["ash_falling_01","ash_falling_02"] {
        let tag = TagFile::read(&format!("{root}/fx/particles/weather/{p}.particle")).unwrap();
        let prt = ParticleDefinition::from_tag(&tag).unwrap();
        println!("== {p} ==");
        dump("color", prt.color.function.as_ref());
        dump("intensity", prt.intensity.function.as_ref());
    }
}
