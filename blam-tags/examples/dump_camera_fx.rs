//! Dump exposure fields from a .camera_fx_settings tag for diagnosis.

use blam_tags::camera_fx_settings::CameraFxSettings;
use blam_tags::file::TagFile;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_camera_fx <path>");
    let tag = TagFile::read(&path).expect("failed to read tag");
    let cfx = CameraFxSettings::from_tag(&tag).expect("failed to parse cfxs");

    println!("=== {} ===", path);
    println!("exposure:");
    println!("  flags:                            0x{:04x}", cfx.exposure.flags);
    let f = cfx.exposure.flags;
    println!("    bit0 USE_DEFAULT:               {}", f & 0x01 != 0);
    println!("    bit1 BLEND_LIMIT_RELATIVE:      {}", f & 0x02 != 0);
    println!("    bit2 AUTO:                      {}", f & 0x04 != 0);
    println!("    bit3 DOUBLE_SIDED_STAR:         {}", f & 0x08 != 0);
    println!("    bit4 FIXED:                     {}", f & 0x10 != 0);
    println!("  exposure (static target stops):   {:.4}", cfx.exposure.exposure);
    println!("  maximum_change:                   {:.4}", cfx.exposure.maximum_change);
    println!("  blend_speed:                      {:.4}", cfx.exposure.blend_speed);
    println!("  minimum (stops clamp):            {:.4}", cfx.exposure.minimum);
    println!("  maximum (stops clamp):            {:.4}", cfx.exposure.maximum);
    println!("  auto_exposure_screen_brightness:  {:.4}", cfx.exposure.auto_exposure_screen_brightness);
    println!("  auto_exposure_delay:              {:.4}", cfx.exposure.auto_exposure_delay);
    println!();
    println!("bloom_point:                        {} flags=0x{:04x} value={:.4}", "", cfx.bloom_point.flags, cfx.bloom_point.value);
    println!("bloom_inherent:                     flags=0x{:04x} value={:.4}", cfx.bloom_inherent.flags, cfx.bloom_inherent.value);
    println!("bloom_intensity:                    flags=0x{:04x} value={:.4}", cfx.bloom_intensity.flags, cfx.bloom_intensity.value);
    println!("auto_exposure_anti_bloom:           flags=0x{:04x} value={:.4}", cfx.auto_exposure_anti_bloom.flags, cfx.auto_exposure_anti_bloom.value);
    println!("auto_exposure_sensitivity:          flags=0x{:04x} value={:.4}", cfx.auto_exposure_sensitivity.flags, cfx.auto_exposure_sensitivity.value);
    println!("self_illum_preferred:               flags=0x{:04x} value={:.4}", cfx.self_illum_preferred.flags, cfx.self_illum_preferred.value);
    println!("self_illum_scale:                   flags=0x{:04x} value={:.4}", cfx.self_illum_scale.flags, cfx.self_illum_scale.value);
}
