//! Opt-in integration check: walk one real tag per classic-shader / H4-material
//! group and confirm the walker reads the load-bearing fields (not just that it
//! compiles). Skips silently when a tag isn't present on this machine.

use blam_tags::file::TagFile;

const ROOT: &str = "/Users/camden/Halo";

fn load(rel: &str) -> Option<TagFile> {
    TagFile::read(format!("{ROOT}/{rel}")).ok()
}

#[test]
fn walks_classic_and_h4_shaders() {
    use blam_tags::shader::*;

    // --- CE shader_environment (senv) ---
    if let Some(t) = load("haloce_mcc/tags/scenery/teleporter_base/shaders/teleporter_base.shader_environment") {
        let s = ce_environment::ShaderEnvironment::from_tag(&t).expect("senv parse");
        eprintln!("senv: base={:?} bump={:?}", s.diffuse.base_map.path, s.bump.bump_map.path);
        assert!(!s.diffuse.base_map.is_null(), "senv base_map should be populated");
    }
    // --- CE transparent subtypes: confirm parse on a real tag (+ eyeball) ---
    if let Some(t) = load("haloce_mcc/tags/powerups/active camoflage/shaders/powerup glass.shader_transparent_glass") {
        let s = ce_transparent_glass::ShaderTransparentGlass::from_tag(&t).expect("sgla parse");
        eprintln!("sgla: reflection_map={:?}", s.reflection.reflection_map.path);
    }
    if let Some(t) = load("haloce_mcc/tags/scenery/halo_destroyed/shaders/shockwave_surface.shader_transparent_chicago") {
        let s = ce_transparent_chicago::ShaderTransparentChicago::from_tag(&t).expect("schi parse");
        eprintln!("schi: maps={}", s.maps.len());
        assert!(!s.maps.is_empty(), "schi maps block");
    }
    if let Some(t) = load("haloce_mcc/tags/sky/sky_timberland/shaders/timberland_sky_dusk.shader_transparent_chicago_extended") {
        ce_transparent_chicago_extended::ShaderTransparentChicagoExtended::from_tag(&t).expect("scex parse");
        eprintln!("scex: parsed");
    }
    if let Some(t) = load("haloce_mcc/tags/powerups/active camoflage/shaders/active camoflage.shader_transparent_generic") {
        let s = ce_transparent_generic::ShaderTransparentGeneric::from_tag(&t).expect("sotr parse");
        eprintln!("sotr: maps={} stages={}", s.maps.len(), s.stages.len());
    }
    if let Some(t) = load("haloce_mcc/tags/weapons/plasma pistol/shaders/pp gauge.shader_transparent_meter") {
        ce_transparent_meter::ShaderTransparentMeter::from_tag(&t).expect("smet parse");
        eprintln!("smet: parsed");
    }
    if let Some(t) = load("haloce_mcc/tags/characters/elite/shaders/shield plasma.shader_transparent_plasma") {
        ce_transparent_plasma::ShaderTransparentPlasma::from_tag(&t).expect("spla parse");
        eprintln!("spla: parsed");
    }
    if let Some(t) = load("haloce_mcc/tags/sky/dusk/shaders/water dusk.shader_transparent_water") {
        ce_transparent_water::ShaderTransparentWater::from_tag(&t).expect("swat parse");
        eprintln!("swat: parsed");
    }

    // --- H2 shader (shad): the parameters block is the load-bearing part ---
    if let Some(t) = load("halo2_mcc/tags/objects/characters/masterchief/shaders/masterchief.shader") {
        let s = h2_shader::H2Shader::from_tag(&t).expect("h2 parse");
        eprintln!("h2: template={:?} params={}", s.template.path, s.parameters.len());
        assert!(!s.parameters.is_empty(), "h2 parameters block");
        assert!(s.parameters.iter().any(|p| p.name == "environment_map"), "h2 env param by name");
    }

    // --- H4 material (mat ) ---
    if let Some(t) = load("halo4_mcc/tags/objects/characters/storm_pawn/materials/storm_pawn_leader/storm_pawn_leader.material") {
        let s = h4_material::H4Material::from_tag(&t).expect("h4 material parse");
        eprintln!("h4mat: shader={:?} params={}", s.material_shader.path, s.material_parameters.len());
        assert!(!s.material_parameters.is_empty(), "h4 material_parameters block");
    }

    // --- H4 material_shader ---
    if let Some(t) = load("halo4_mcc/tags/shaders/material_shaders/materials/srf_blinn.material_shader") {
        let s = h4_material_shader::MaterialShader::from_tag(&t).expect("h4 material_shader parse");
        eprintln!("h4ms: params={} sources={}", s.material_parameters.len(), s.source_shader_files.len());
        assert!(!s.source_shader_files.is_empty(), "h4 material_shader source files");
    }
}
