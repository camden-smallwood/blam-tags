//! Print a decal_system's definition radius, runtime bitmap_aspect,
//! base_map path, and the base_map bitmap's real dimensions/aspect.
//!
//! Usage: cargo run --example decal_aspect -- <decal_system tag>

use std::path::PathBuf;

use blam_tags::bitmap::Bitmap;
use blam_tags::decal_system::DecalSystem;
use blam_tags::paths::{derive_tags_root, resolve_tag_path};
use blam_tags::TagFile;

fn main() {
    let p = PathBuf::from(std::env::args().nth(1).expect("usage: <decal_system>"));
    let root = derive_tags_root(&p).unwrap();
    let ds = DecalSystem::from_tag(&TagFile::read(&p).unwrap()).unwrap();
    println!("runtime_max_radius={:.3} defs={}", ds.runtime_max_radius, ds.definitions.len());
    for (i, def) in ds.definitions.iter().enumerate() {
        println!(
            "def[{i}] radius=({:.3},{:.3}) baked_bitmap_aspect={:.4}",
            def.radius.0, def.radius.1, def.bitmap_aspect
        );
        let Some(sh) = def.shader.as_ref() else { println!("  no shader"); continue };
        for param in sh.parameters.iter().filter(|p| p.parameter_name == "base_map") {
            println!("  base_map -> {}", param.bitmap_path);
            if param.bitmap_path.is_empty() { continue; }
            let bp = resolve_tag_path(&root, &param.bitmap_path, "bitmap");
            let Ok(t) = TagFile::read(&bp) else { println!("    (failed to read tag)"); continue };
            match Bitmap::new(&t) {
                Ok(bm) => {
                    for k in 0..bm.len() {
                        if let Some(img) = bm.image(k) {
                            let (w, h) = (img.width(), img.height());
                            println!("    image[{k}] {w}x{h} aspect={:.4}", w as f32 / h.max(1) as f32);
                        }
                    }
                }
                Err(_) => println!("    (failed to parse bitmap)"),
            }
        }
    }
}
