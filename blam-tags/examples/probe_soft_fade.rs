//! Dump soft_fade-related bool params (use_soft_z / use_soft_fresnel) and
//! their resolution source (rmop default vs rmsh override) for a shader.
//!   cargo run --example probe_soft_fade -- <shader_path>

use std::path::PathBuf;
use blam_tags::TagFile;
use blam_tags::render_method::{build_rmop_param_list, RenderMethod, RenderMethodDefinition, RenderMethodOption};

fn main() {
    let tags_root = std::env::var("HALO3_TAGS")
        .unwrap_or_else(|_| "/Users/camden/Halo/halo3_mcc/tags".into());
    let rel = std::env::args().nth(1).expect("usage: probe_soft_fade <shader_path>");
    let path: PathBuf = [&tags_root, &rel].iter().collect();
    let tag = TagFile::read(&path).expect("read");
    let rm = RenderMethod::from_tag(&tag).expect("parse");

    let rmdf_norm = rm.definition_path.replace('\\', "/");
    let rmdf_path: PathBuf = [&tags_root, &format!("{}.render_method_definition", rmdf_norm)].iter().collect();
    let rmdf = RenderMethodDefinition::from_tag(&TagFile::read(&rmdf_path).expect("rmdf")).expect("parse rmdf");

    let tr = tags_root.clone();
    let rmop_params = build_rmop_param_list(&rm, &rmdf, |p| {
        let n = p.replace('\\', "/");
        let pp: PathBuf = [&tr, &format!("{}.render_method_option", n)].iter().collect();
        RenderMethodOption::from_tag(&TagFile::read(&pp).ok()?).ok()
    });

    println!("=== {rel} ===");
    for name in ["use_soft_z", "use_soft_fresnel", "soft_z_range", "soft_fresnel_power"] {
        let rmsh = rm.parameters.iter().find(|p| p.parameter_name == name);
        let rmop = rmop_params.iter().find(|p| p.parameter_name == name);
        println!(
            "  {name:<20} rmsh_override={:?}  rmop_default_int_bool={:?}",
            rmsh.map(|p| (p.parameter_type.map(|e| e.get()), p.int_parameter, p.real_parameter)),
            rmop.map(|p| p.default_int_bool_value),
        );
    }
}
