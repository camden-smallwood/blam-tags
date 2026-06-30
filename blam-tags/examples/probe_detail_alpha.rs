//! Decodes a bitmap (mip 0) and measures the alpha-vs-luminance
//! correlation — specifically the "void-causing" combination of HIGH
//! alpha + LOW color that, multiplied by an HDR albedo_color alpha,
//! produces black voids under alpha_blend.
//!
//!   cargo run --example probe_detail_alpha -- <path/to/bitmap>

use blam_tags::TagFile;
use blam_tags::bitmap::Bitmap;
use blam_tags::bitmap::decode::decode_to_rgba8;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Halo/halo3_mcc/tags/levels/multi/s3d_sky_bridgenew/sky/bitmaps/clouds_moving_tail_sb_01.bitmap".into()
    });
    let tag = TagFile::read(&path).expect("read bitmap tag");
    let bitmap = Bitmap::new(&tag).expect("parse bitmap");
    let image = bitmap.iter().next().expect("no image");
    let (w, h) = (image.width(), image.height());
    let format = image.format().expect("format");
    let all = image.pixel_bytes().expect("pixels");
    let need = format.level_bytes(w, h) as usize;
    let rgba = decode_to_rgba8(format, w, h, &all[..need], bitmap.p8_palette()).expect("decode");

    println!("{path}");
    println!("  {w}x{h}  format={format:?}");

    let n = (w * h) as usize;
    let mut a_hist = [0u64; 8]; // alpha buckets
    let mut void_px = 0u64; // high alpha (>0.5) AND low luma (<0.2)
    let mut a_eq_255 = 0u64; // fully-opaque alpha
    let (mut sum_a, mut sum_l) = (0u64, 0u64);
    let mut corr_hi_a_lo_l = 0u64;
    for px in rgba.chunks_exact(4) {
        let (r, g, b, a) = (px[0] as f32, px[1] as f32, px[2] as f32, px[3]);
        let luma = 0.299 * r + 0.587 * g + 0.114 * b; // 0..255
        a_hist[(a as usize) * 8 / 256] += 1;
        if a == 255 { a_eq_255 += 1; }
        sum_a += a as u64;
        sum_l += luma as u64;
        if a as f32 > 128.0 && luma < 51.0 { void_px += 1; corr_hi_a_lo_l += 1; }
    }
    println!("  mean alpha = {:.1}/255   mean luma = {:.1}/255", sum_a as f64 / n as f64, sum_l as f64 / n as f64);
    println!("  pixels with alpha==255 (fully opaque): {} ({:.1}%)", a_eq_255, 100.0 * a_eq_255 as f64 / n as f64);
    println!("  VOID-causing pixels (alpha>0.5 AND luma<0.2): {} ({:.1}%)", void_px, 100.0 * void_px as f64 / n as f64);
    print!("  alpha histogram [0..1 in 8 buckets]: ");
    for (i, c) in a_hist.iter().enumerate() {
        print!("{}:{:.0}% ", i, 100.0 * *c as f64 / n as f64);
    }
    println!();
    let _ = corr_hi_a_lo_l;
}
