//! Smoke test for the particle tag walker (P3.T1).
//!
//! Default: dump the 3 riverworld waterfall particles
//! (rolling_mist, mist, water_spray). Pass paths as args to dump
//! specific tags instead.

use std::path::PathBuf;

use blam_tags::particle::ParticleDefinition;
use blam_tags::TagFile;

fn main() {
    let paths: Vec<PathBuf> = if std::env::args().len() > 1 {
        std::env::args().skip(1).map(PathBuf::from).collect()
    } else {
        let base = "/Users/camden/Halo/halo3_mcc/tags/levels/multi/riverworld/fx/waterfall/particles";
        vec![
            format!("{base}/rolling_mist.particle"),
            format!("{base}/mist.particle"),
            format!("{base}/water_spray.particle"),
        ]
        .into_iter()
        .map(PathBuf::from)
        .collect()
    };

    for path in paths {
        match dump_one(&path) {
            Ok(()) => {}
            Err(e) => eprintln!("{}: ERROR {e}", path.display()),
        }
        println!();
    }
}

fn dump_one(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let tag = TagFile::read(path)?;
    let prt = ParticleDefinition::from_tag(&tag)?;
    println!("=== {} ===", path.display());
    println!(
        "main_flags=0x{:08x} appearance_flags=0x{:08x} billboard={:?}",
        prt.main_flags, prt.appearance_flags, prt.billboard_style,
    );
    println!(
        "sequence=[{}..{}] center_offset=({}, {}) curvature={}",
        prt.first_sequence_index,
        prt.first_sequence_index + prt.sequence_count,
        prt.center_offset.x, prt.center_offset.y, prt.curvature,
    );
    println!(
        "angle_fade range={}° cutoff={}° motion_blur trans={} rot={} aspect={}",
        prt.angle_fade_range_degrees,
        prt.angle_fade_cutoff_degrees,
        prt.motion_blur_translation_scale,
        prt.motion_blur_rotation_scale,
        prt.motion_blur_aspect_scale,
    );
    println!("animation_flags=0x{:08x}", prt.animation_flags);
    println!(
        "runtime: used_states=0x{:08x} const_per_particle=0x{:08x} const_over_time=0x{:08x}",
        prt.runtime_used_particle_states,
        prt.runtime_constant_per_particle_properties,
        prt.runtime_constant_over_time_properties,
    );

    if !prt.model.is_empty() {
        println!("model = pmdf:{}", prt.model);
    }

    if let Some(shader) = &prt.shader {
        println!(
            "shader: rmdf={} options={:?} parameters={} sort_layer={}",
            shader.definition_path,
            shader.options,
            shader.parameters.len(),
            shader.sort_layer,
        );
        for (i, p) in shader.parameters.iter().enumerate().take(8) {
            println!(
                "  param[{i}] '{}' type={:?} bitmap='{}' real={} int={}",
                p.parameter_name, p.parameter_type, p.bitmap_path, p.real_parameter, p.int_parameter
            );
        }
    } else {
        println!("shader: <missing>");
    }

    let p = &prt.alpha;
    println!(
        "alpha: input_var={} range_var={} mod={:?} mod_input={} constant={} flags=0x{:02x}",
        p.input_variable, p.range_variable, p.output_modifier,
        p.output_modifier_input, p.constant_value, p.runtime_flags,
    );
    let p = &prt.aspect_ratio;
    println!("aspect_ratio: input={} constant={}", p.input_variable, p.constant_value);
    let p = &prt.intensity;
    println!("intensity: input={} constant={}", p.input_variable, p.constant_value);
    let p = &prt.frame_index;
    println!("frame_index: input={} constant={}", p.input_variable, p.constant_value);
    let p = &prt.animation_rate;
    println!("animation_rate: input={} constant={}", p.input_variable, p.constant_value);
    let p = &prt.palette_animation;
    println!("palette_animation: input={} constant={}", p.input_variable, p.constant_value);

    println!("attachments[{}]:", prt.attachments.len());
    for (i, a) in prt.attachments.iter().enumerate() {
        let gs: String = a.type_group.iter().map(|&b| b as char).collect();
        println!(
            "  [{i}] {gs}:{} trigger={:?} flags=0x{:02x} scales=[{}, {}]",
            a.type_ref, a.trigger, a.flags, a.primary_scale, a.secondary_scale,
        );
    }

    if let Some(sprite) = prt.gpu_data.sprite {
        println!(
            "gpu_sprite: corner=[{}, {}, {}, {}]",
            sprite.corner[0], sprite.corner[1], sprite.corner[2], sprite.corner[3],
        );
    }
    if let Some(frames) = &prt.gpu_data.frames {
        println!("gpu_frames: count={} entries={}", frames.count, frames.frames.len());
    }

    Ok(())
}
