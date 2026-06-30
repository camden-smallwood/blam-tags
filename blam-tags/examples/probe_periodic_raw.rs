//! Dump the RAW periodic function internals for a shader's animated param,
//! so we can verify each step that produces e.g. roter_rauch alpha 6.354.
use std::path::PathBuf;
use blam_tags::TagFile;
use blam_tags::render_method::RenderMethod;
use blam_tags::tag_function::{FunctionKind, periodic_function_evaluate};

fn main() {
    let root = std::env::var("HALO3_TAGS").unwrap_or_else(|_| "/Users/camden/Halo/halo3_mcc/tags".into());
    let rel = std::env::args().nth(1).unwrap_or_else(|| "levels/multi/s3d_lockout/sky/shaders/roter_rauch.shader_halogram".into());
    let want_param = std::env::args().nth(2).unwrap_or_else(|| "albedo_color".into());
    let p: PathBuf = [&root, &rel].iter().collect();
    let tag = TagFile::read(&p).expect("read");
    let rm = RenderMethod::from_tag(&tag).expect("parse");
    for param in &rm.parameters {
        if param.parameter_name != want_param { continue; }
        for anim in &param.animated_parameters {
            let Some(f) = anim.function.as_ref() else { continue };
            let h = f.header();
            println!("param '{}' anim_type={:?} period={}", param.parameter_name, anim.parameter_type.map(|e| e.get()), anim.time_period_in_seconds);
            println!("  header: type={:?} flags={:?} clamp_range=[{}, {}] is_clamped={}",
                f.function_type(), h.flags, h.clamp_range_min, h.clamp_range_max, h.flags.is_clamped());
            if let FunctionKind::Periodic { compact, .. } = f.kind() {
                println!("  PeriodicCompact: fn_index={} frequency={} phase={} amp_min={} amp_max={}",
                    compact.function_index, compact.frequency, compact.phase, compact.amplitude_min, compact.amplitude_max);
                for t in [0.0f32, 0.25, 0.5] {
                    let adj = t * compact.frequency + compact.phase;
                    let pv = periodic_function_evaluate(compact.function_index, adj);
                    let legacy = (compact.amplitude_max - compact.amplitude_min) * pv + compact.amplitude_min;
                    let clamped = legacy.clamp(0.0, 1.0);
                    let mapped = h.clamp_range_min + clamped * (h.clamp_range_max - h.clamp_range_min);
                    println!("  t={t}: adj={adj:.4} periodic_eval={pv:.5} legacy(amp)={legacy:.5} clamped={clamped:.5} -> final={mapped:.5}");
                }
            }
        }
    }
}
