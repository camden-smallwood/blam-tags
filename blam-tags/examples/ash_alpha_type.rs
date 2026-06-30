use blam_tags::TagFile;
use blam_tags::effect::EffectDefinition;
fn main() {
    let tag = TagFile::read("/Users/camden/Halo/halo3_mcc/tags/fx/scenery_fx/weather/ash/ash_01.effect").unwrap();
    let d = EffectDefinition::from_tag(&tag).unwrap();
    for (si, ps) in d.events[0].particle_systems.iter().enumerate() {
        let em = &ps.emitters[0];
        if let Some(f) = &em.particle_alpha.function {
            let h = f.header();
            println!("sys{si} '{}' particle_alpha: type={:?} graph={:?} colors={:08X?}",
                ps.particle_tag_path, f.function_type(), h.color_graph_type, &h.colors);
            // sample the curve densely
            let s: Vec<String> = (0..=10).map(|i| format!("{:.2}", f.evaluate(i as f32/10.0, 0.0))).collect();
            println!("    curve: {}", s.join(" "));
        } else {
            println!("sys{si} particle_alpha CONST={}", em.particle_alpha.constant_value);
        }
    }
}
