//! Dump a .shield_impact tag — the global shield-rendering params
//! consumed by `c_object_renderer::render_shield_impact_mesh_part`.

use blam_tags::file::TagFile;
use blam_tags::shield_impact::ShieldImpact;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_shield_impact <path>");
    let tag = TagFile::read(&path).expect("failed to read tag");
    let si = ShieldImpact::from_tag(&tag).expect("failed to parse shit");

    println!("=== {} ===", path);
    println!("noise textures:");
    println!("  1: {:?}", si.noise_texture_1);
    println!("  2: {:?}", si.noise_texture_2);
    println!("uv:");
    println!("  extrusion_distance: {:.4}", si.extrusion_distance);
    println!("  texture_scale:      {:.4}", si.texture_scale);
    println!("  scroll_speed:       {:.4}", si.scroll_speed);
    println!("plasma layer 1:        sharp={:.4} scale={:.4} threshold={:.4}",
        si.plasma_layer_1.sharpness, si.plasma_layer_1.scale, si.plasma_layer_1.threshold);
    println!("plasma layer 2:        sharp={:.4} scale={:.4} threshold={:.4}",
        si.plasma_layer_2.sharpness, si.plasma_layer_2.scale, si.plasma_layer_2.threshold);
    println!("overshield primary:    rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.overshield_1.color.red, si.overshield_1.color.green, si.overshield_1.color.blue,
        si.overshield_1.intensity);
    println!("overshield secondary:  rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.overshield_2.color.red, si.overshield_2.color.green, si.overshield_2.color.blue,
        si.overshield_2.intensity);
    println!("overshield ambient:    rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.overshield_ambient.color.red, si.overshield_ambient.color.green, si.overshield_ambient.color.blue,
        si.overshield_ambient.intensity);
    println!("impact primary:        rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.impact_1.color.red, si.impact_1.color.green, si.impact_1.color.blue,
        si.impact_1.intensity);
    println!("impact secondary:      rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.impact_2.color.red, si.impact_2.color.green, si.impact_2.color.blue,
        si.impact_2.intensity);
    println!("impact ambient:        rgb=({:.3},{:.3},{:.3}) intensity={:.4}",
        si.impact_ambient.color.red, si.impact_ambient.color.green, si.impact_ambient.color.blue,
        si.impact_ambient.intensity);
}
