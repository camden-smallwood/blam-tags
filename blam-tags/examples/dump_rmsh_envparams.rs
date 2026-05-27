//! Dump all env-mapping-related parameters baked into an rmsh's
//! ResolvedRenderMethod. Source of truth for what tool.exe baked.
use blam_tags::file::TagFile;
use blam_tags::render_method::ResolvedRenderMethod;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("usage: <rmsh path>")?);
    let tag = TagFile::read(&path)?;
    let rm = ResolvedRenderMethod::resolve(&tag)?;
    let env_names = [
        "env_tint_color",
        "env_bias",
        "env_topcoat_color",
        "env_topcoat_bias",
        "env_roughness_scale",
        "environment_map_specular_contribution",
        "specular_coefficient",
        "analytical_specular_contribution",
        "area_specular_contribution",
        "diffuse_coefficient",
        "specular_mask_texture",
        "normal_specular_power",
        "glancing_specular_power",
        "normal_specular_tint",
        "glancing_specular_tint",
        "albedo_specular_tint_blend",
        "fresnel_curve_steepness",
        "analytical_anti_shadow_control",
    ];
    println!("rmsh = {}", path.display());
    println!("group_tag = {:08x}", rm.group_tag);
    println!();
    for p in &rm.parameters {
        let lc = p.name.to_lowercase();
        if env_names.iter().any(|&n| lc == n) {
            println!("  {:42} source={:?}", p.name, p.source);
        }
    }
    Ok(())
}
