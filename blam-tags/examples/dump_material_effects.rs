//! Dump a material_effects (foot) tag.

use blam_tags::file::TagFile;
use blam_tags::material_effects::MaterialEffects;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_material_effects <path>");
    let tag = TagFile::read(&path).expect("read");
    let me = MaterialEffects::from_tag(&tag).expect("walk");
    println!("=== {} ===", path);
    println!("effects[{}]:", me.effects.len());
    for (i, eb) in me.effects.iter().enumerate() {
        println!("  [{}]:", i);
        println!("    old_materials[{}]", eb.old_materials.len());
        for (j, om) in eb.old_materials.iter().take(3).enumerate() {
            println!("      [{j}] mat={:?} idx={} sweet={} effect={:?} sound={:?}",
                om.material_name, om.runtime_material_index, om.sweetener_mode,
                om.effect, om.sound);
        }
        println!("    sounds[{}]", eb.sounds.len());
        for (j, e) in eb.sounds.iter().take(3).enumerate() {
            let pg = e.primary.as_ref().map(|(g,_)| String::from_utf8_lossy(&g.to_be_bytes()).to_string()).unwrap_or_default();
            println!("      [{j}] mat={:?} idx={} sweet={} primary({})={:?}",
                e.material_name, e.runtime_material_index, e.sweetener_mode,
                pg, e.primary.as_ref().map(|(_,p)| p.as_str()));
        }
        println!("    effects[{}]", eb.effects.len());
        for (j, e) in eb.effects.iter().take(3).enumerate() {
            let pg = e.primary.as_ref().map(|(g,_)| String::from_utf8_lossy(&g.to_be_bytes()).to_string()).unwrap_or_default();
            println!("      [{j}] mat={:?} idx={} sweet={} primary({})={:?}",
                e.material_name, e.runtime_material_index, e.sweetener_mode,
                pg, e.primary.as_ref().map(|(_,p)| p.as_str()));
        }
    }
}
