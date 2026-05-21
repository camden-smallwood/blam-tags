//! Dump a particle_physics (pmov) tag.

use blam_tags::file::TagFile;
use blam_tags::particle_physics::ParticlePhysics;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_particle_physics <path>");
    let tag = TagFile::read(&path).expect("read");
    let pp = ParticlePhysics::from_tag(&tag).expect("walk");
    println!("=== {} ===", path);
    println!("template:    {:?}", pp.template);
    println!("flags:       0x{:02x}", pp.flags);
    println!("movements[{}]:", pp.movements.len());
    for (i, m) in pp.movements.iter().enumerate() {
        println!("  [{}] type={:?}  const_params={} used_states={}",
            i, m.controller_type, m.runtime_constant_parameters, m.runtime_used_particle_states);
        println!("      parameters[{}]:", m.parameters.len());
        for (j, p) in m.parameters.iter().enumerate() {
            let pr = &p.property;
            println!("        [{}] id={}  input={} range={} omod={} omod_in={}  const={:.4}  flags=0x{:02x}  fn={}",
                j, p.parameter_id,
                pr.input_index, pr.range_input_index,
                pr.output_modifier_type, pr.output_modifier_input_index,
                pr.constant_value, pr.runtime_flags,
                if pr.function.is_some() {"<fn>"} else {"none"});
        }
    }
}
