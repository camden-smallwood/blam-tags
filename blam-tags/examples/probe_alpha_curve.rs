//! Sweep an animated param's function across a full period to see its
//! true peak/shape (does it smoothly peak below the clamp_max, or slam
//! flat into the clamp?). Usage:
//!   cargo run --example probe_alpha_curve -- <shader_path> [param_name]
//! param_name defaults to "albedo_color".

use std::path::PathBuf;
use blam_tags::TagFile;
use blam_tags::render_method::RenderMethod;

fn main() {
    let tags_root = std::env::var("HALO3_TAGS")
        .unwrap_or_else(|_| "/Users/camden/Halo/halo3_mcc/tags".into());
    let rel = std::env::args().nth(1).expect("usage: probe_alpha_curve <shader_path> [param]");
    let want = std::env::args().nth(2).unwrap_or_else(|| "albedo_color".into());
    let path: PathBuf = [&tags_root, &rel].iter().collect();
    let tag = TagFile::read(&path).expect("read");
    let rm = RenderMethod::from_tag(&tag).expect("parse");

    for p in &rm.parameters {
        if p.parameter_name != want { continue; }
        for anim in &p.animated_parameters {
            let Some(f) = anim.function.as_ref() else { continue };
            let h = f.header();
            println!(
                "param '{}' anim={:?} period={}s fn={:?} flags={:?} clamp=[{},{}]",
                p.parameter_name, anim.parameter_type.map(|e| e.get()),
                anim.time_period_in_seconds, f.function_type(), h.flags,
                h.clamp_range_min, h.clamp_range_max,
            );
            // sweep phase 0..1 in 0.05 steps
            let mut peak = f32::MIN;
            let mut trough = f32::MAX;
            let mut line = String::new();
            for i in 0..=20 {
                let phase = i as f32 / 20.0;
                let v = f.evaluate(phase, 0.0);
                peak = peak.max(v);
                trough = trough.min(v);
                line.push_str(&format!("{:.2} ", v));
            }
            println!("  curve: {line}");
            println!("  trough={trough:.3} peak={peak:.3}");
        }
    }
}
