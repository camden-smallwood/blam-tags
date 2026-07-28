//! Decode the u_tiles/v_tiles (and any real-typed) animated-parameter
//! function values on a decal_system's shader.

use std::path::PathBuf;
use blam_tags::decal_system::DecalSystem;
use blam_tags::TagFile;

fn main() {
    let p = PathBuf::from(std::env::args().nth(1).expect("usage: <decal_system>"));
    let ds = DecalSystem::from_tag(&TagFile::read(&p).unwrap()).unwrap();
    for (di, def) in ds.definitions.iter().enumerate() {
        println!("def[{di}] {} radius=({:.2},{:.2}) clamp={:.1} cull={:.1}",
            def.name, def.radius.0, def.radius.1, def.clamp_angle_degrees, def.cull_angle_degrees);
        let Some(sh) = def.shader.as_ref() else { continue };
        for param in &sh.parameters {
            print!("  param '{}' type={:?}", param.parameter_name, param.parameter_type.map(|e| e.get()));
            if !param.bitmap_path.is_empty() { print!(" bitmap={}", param.bitmap_path); }
            print!(" real_field={:.4}", param.real_parameter);
            for ap in &param.animated_parameters {
                if let Some(f) = ap.function.as_ref() {
                    // Evaluate at input 0 and 1 to reveal constant vs ranged.
                    let v0 = f.evaluate(0.0, 0.0);
                    let v1 = f.evaluate(1.0, 0.0);
                    print!(" | fn[{:?}] eval(0)={:.4} eval(1)={:.4}", ap.parameter_type.map(|e| e.get()), v0, v1);
                }
            }
            println!();
        }
    }
}
