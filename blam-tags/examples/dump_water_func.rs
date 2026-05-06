//! Dump the function-data first byte (= function_type enum) for every
//! animated_parameter in a shader_water tag. Used to drive
//! TagFunction implementation priority.

use blam_tags::file::TagFile;
use blam_tags::render_method::RenderMethod;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Halo/halo3_mcc/tags/levels/multi/riverworld/shaders/riverworld_water_rough.shader_water".to_string()
    });
    let tag = TagFile::read(&path)?;
    let rm = RenderMethod::from_tag(&tag)?;
    println!("{} parameters", rm.parameters.len());
    for p in &rm.parameters {
        if p.animated_parameters.is_empty() { continue; }
        for (i, a) in p.animated_parameters.iter().enumerate() {
            let ft = a.function.as_ref().map(|f| f.function_type());
            let samples: Vec<f32> = if let Some(f) = a.function.as_ref() {
                [0.0, 0.25, 0.5, 0.75, 1.0].iter()
                    .map(|&x| f.evaluate(x, 0.0)).collect()
            } else { vec![] };
            println!(
                "  '{}' anim[{i}] inner_type={:?} func_type={:?} time_period_s={}",
                p.parameter_name, a.parameter_type, ft, a.time_period_in_seconds,
            );
            if !samples.is_empty() {
                println!("    samples @ x=[0, .25, .5, .75, 1]: {samples:?}");
            }
        }
    }
    let mut types: Vec<_> = rm.parameters.iter()
        .flat_map(|p| p.animated_parameters.iter())
        .filter_map(|a| a.function.as_ref().map(|f| format!("{:?}", f.function_type())))
        .collect();
    types.sort();
    types.dedup();
    println!("\nFunction types used: {types:?}");
    Ok(())
}
