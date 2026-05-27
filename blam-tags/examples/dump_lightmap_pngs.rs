//! Dump a .bitmap tag through blam-tags' loader to DDS, AND extract
//! each BC3 layer's raw RGBA into a `.rgba` file. Used to compare the
//! tag-side bytes against an externally-extracted MCC DDS.
//!
//! Run from blam-tags repo root:
//!     cargo run --release --example dump_lightmap_pngs -- \
//!         <bitmap_path> <out_dir> <label>

use blam_tags::file::TagFile;
use blam_tags::Bitmap;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let bitmap_path = PathBuf::from(args.get(1).ok_or("usage: <bitmap_path> <out_dir> <label>")?);
    let out_dir = PathBuf::from(args.get(2).ok_or("missing out_dir")?);
    let label = args.get(3).ok_or("missing label")?.clone();
    let tag = TagFile::read(&bitmap_path)?;
    let bitmap = Bitmap::new(&tag)?;
    eprintln!("loaded {} ({} images)", bitmap_path.display(), bitmap.len());
    for (i, img) in bitmap.iter().enumerate() {
        let w = img.width() as usize;
        let h = img.height() as usize;
        let layers = img.layer_count() as usize;
        let mips = img.mipmap_levels();
        let fmt = img.format()?;
        eprintln!("  image[{i}]: {w}x{h} layers={layers} mips={mips} format={fmt:?}");
        // Write the DDS this image would produce — for direct comparison
        // against MCC's extract.
        let dds_path = out_dir.join(format!("{label}_image{i}.dds"));
        let mut dds_file = std::fs::File::create(&dds_path)?;
        img.write_dds(&mut dds_file)?;
        eprintln!("    DDS → {}", dds_path.display());
        // Also dump raw BC3 (single mip0, all layers concatenated)
        let pixels = img.pixel_bytes()?;
        let level0_size_per_layer = fmt.level_bytes(w as u32, h as u32) as usize;
        for l in 0..layers {
            let off = l * (fmt.surface_bytes(w as u32, h as u32, mips) as usize);
            let bc3 = &pixels[off .. off + level0_size_per_layer];
            let path = out_dir.join(format!("{label}_image{i}_layer{l}.bc3"));
            std::fs::write(&path, bc3)?;
            eprintln!("    layer {l} BC3 ({} bytes) → {}", bc3.len(), path.display());
        }
    }
    Ok(())
}
