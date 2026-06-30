//! Resolves the pixel user cbuffer for an arbitrary shader/halogram and
//! prints the resolved slot values at several times (to catch animated
//! parameters like a scrolling detail xform or an animated albedo alpha).
//!
//! Usage:
//!   cargo run --example probe_smoke_cbuffer -- <path/to/shader_or_halogram>
//!
//! Mirrors protomorph's runtime resolution exactly: Stage-1 rmop defaults
//! (`build_rmop_param_list`) then Stage-2 rmsh overrides, evaluated via
//! `resolve_pixel_user_cbuffer_at_time`.

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::render_method::{
    build_rmop_param_list, resolve_pixel_user_cbuffer_at_time, DefaultEvalContext, RenderMethod,
    RenderMethodDefinition, RenderMethodOption, RenderMethodTemplate,
};

fn main() {
    let tags_root = std::env::var("HALO3_TAGS")
        .unwrap_or_else(|_| "/Users/camden/Halo/halo3_mcc/tags".into());

    let rmsh_rel = std::env::args().nth(1).unwrap_or_else(|| {
        "levels/multi/s3d_lockout/sky/shaders/smoke_moving_lock_01.shader_halogram".into()
    });
    let rmsh_path: PathBuf = [&tags_root, &rmsh_rel].iter().collect();

    let rmsh_tag = TagFile::read(&rmsh_path).expect("read rmsh");
    let rmsh = RenderMethod::from_tag(&rmsh_tag).expect("parse rmsh");

    let template_path = rmsh
        .postprocess_definition
        .as_ref()
        .map(|pp| pp.template_path.as_str())
        .filter(|s| !s.is_empty())
        .expect("postprocess.template missing");

    let normalized = template_path.replace('\\', "/");
    let exact: PathBuf =
        [&tags_root, &format!("{}.render_method_template", normalized)].iter().collect();
    let rmt2_full: PathBuf = if exact.exists() {
        exact
    } else {
        let dir = exact.parent().unwrap().to_path_buf();
        let stem_prefix = exact
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches("_0")
            .to_string();
        let mut found = None;
        for entry in std::fs::read_dir(&dir).expect("read templates dir") {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("render_method_template") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
            if stem.starts_with(&stem_prefix)
                && stem.trim_start_matches(&stem_prefix).chars().all(|c| c == '_' || c == '0')
            {
                found = Some(path);
                break;
            }
        }
        found.expect("no matching rmt2")
    };
    let rmt2_tag = TagFile::read(&rmt2_full).expect("read rmt2");
    let rmt2 = RenderMethodTemplate::from_tag(&rmt2_tag).expect("parse rmt2");

    let rmdf_normalized = rmsh.definition_path.replace('\\', "/");
    let rmdf_path: PathBuf =
        [&tags_root, &format!("{}.render_method_definition", rmdf_normalized)].iter().collect();
    let rmdf_tag = TagFile::read(&rmdf_path).expect("read rmdf");
    let rmdf = RenderMethodDefinition::from_tag(&rmdf_tag).expect("parse rmdf");

    let tags_root_for_load = tags_root.clone();
    let rmop_params = build_rmop_param_list(&rmsh, &rmdf, |path| {
        let normalized = path.replace('\\', "/");
        let rmop_path: PathBuf =
            [&tags_root_for_load, &format!("{}.render_method_option", normalized)].iter().collect();
        let tag = TagFile::read(&rmop_path).ok()?;
        RenderMethodOption::from_tag(&tag).ok()
    });

    println!("== {} ==", rmsh_rel);
    println!("rmt2: {}", template_path);

    // Dump every animated parameter's function (where values like the
    // 6.354 alpha actually originate).
    println!("\n-- animated functions (source of resolved values) --");
    for p in &rmsh.parameters {
        for anim in &p.animated_parameters {
            let ty = anim.parameter_type.map(|e| e.get());
            if let Some(f) = anim.function.as_ref() {
                let h = f.header();
                println!(
                    "  param '{}' anim_type={:?} period={} fn_type={:?} flags={:?} clamp_range=[{}, {}] as_constant={:?} eval(0)={} eval(.25)={} eval(.5)={}",
                    p.parameter_name,
                    ty,
                    anim.time_period_in_seconds,
                    f.function_type(),
                    h.flags,
                    h.clamp_range_min,
                    h.clamp_range_max,
                    f.as_constant(),
                    f.evaluate(0.0, 0.0),
                    f.evaluate(0.25, 0.0),
                    f.evaluate(0.5, 0.0),
                );
            }
        }
    }

    for t in [0.0f32, 2.5, 5.0] {
        let ctx = DefaultEvalContext { eval_time: t };
        let cb = resolve_pixel_user_cbuffer_at_time(&rmsh, &rmt2, &rmop_params, &ctx);
        println!("\n-- t = {t} --");
        for slot in &cb.slots {
            println!(
                "  [{:>2}] {:<28} xform={} value={:?}",
                slot.byte_offset / 16,
                slot.source_name,
                slot.is_xform,
                slot.value,
            );
        }
    }
}
