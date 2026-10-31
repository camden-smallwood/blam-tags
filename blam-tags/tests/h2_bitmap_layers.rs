//! Halo 2 stores a cube's six top levels together, as a ring around the Z-up
//! world, and a volume's slices one after another.
//!
//! Needs the Halo 2 kit: set `BLAM_TEST_H2EK` to its root.

mod common;

use blam_tags::bitmap::decode::decode_to_rgba8;
use blam_tags::bitmap::Bitmap;
use blam_tags::classic::read_classic_tag_file;
use blam_tags::file::TagFile;
use blam_tags::layout::TagLayout;

fn load(rel: &str) -> Option<TagFile> {
    let tags = common::kit("BLAM_TEST_H2EK")?.tags();
    let bytes = std::fs::read(tags.join(rel)).ok()?;
    let layout = TagLayout::from_json(common::definitions("halo2_mcc").join("bitmap.json")).ok()?;
    read_classic_tag_file(&bytes, layout).ok()
}

/// `+X`'s top row is `+Y`'s right column reversed, so faces in the right
/// order join there. In storage order this pair is `+X` and `-X`, which share
/// no edge at all, and a mip-chain stride reads noise.
#[test]
fn cube_faces_come_back_in_directx_order_and_join() {
    let Some(tag) = load("objects/bitmaps/reflection_maps/vehicle_glass.bitmap") else {
        eprintln!("skipping: BLAM_TEST_H2EK is not set");
        return;
    };
    let bitmap = Bitmap::new(&tag).unwrap();
    let image = bitmap.image(0).unwrap();
    assert!(image.is_cube());
    let (size, format) = (image.width(), image.format().unwrap());
    let face = |index| {
        let bytes = image.cube_face_bytes(index).unwrap();
        decode_to_rgba8(format, size, size, bytes, bitmap.p8_palette()).unwrap()
    };
    let (positive_x, negative_x, positive_y) = (face(0), face(1), face(2));
    let texel = |rgba: &[u8], x: usize, y: usize| rgba[(y * size as usize + x) * 4];
    let seam = |other: &[u8]| {
        (0..size as usize)
            .map(|x| {
                let end = size as usize - 1;
                texel(&positive_x, x, 0).abs_diff(texel(other, end, end - x)) as u32
            })
            .sum::<u32>() as f32
            / size as f32
    };
    assert!(seam(&positive_y) < 10.0, "+X and +Y should join: {}", seam(&positive_y));
    assert!(seam(&negative_x) > seam(&positive_y) * 2.0);
}

#[test]
fn every_volume_slice_is_reachable() {
    let Some(tag) = load("rasterizer/plasma_shield_noise.bitmap") else {
        eprintln!("skipping: BLAM_TEST_H2EK is not set");
        return;
    };
    let bitmap = Bitmap::new(&tag).unwrap();
    let image = bitmap.image(0).unwrap();
    assert!(image.depth() > 1);
    let first = image.volume_slice_bytes(0).unwrap();
    let last = image.volume_slice_bytes(image.depth() - 1).unwrap();
    assert_eq!(first.len(), last.len());
    assert_ne!(first, last, "slices differ");
}
