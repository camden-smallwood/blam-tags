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
            let val = a.function.as_ref().map(|f| f.evaluate(0.0, 0.0));
            println!(
                "  '{}' anim[{i}] inner_type={:?} func_type={:?} eval(0,0)={:?} time_period_s={}",
                p.parameter_name, a.parameter_type, ft, val, a.time_period_in_seconds,
            );
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
