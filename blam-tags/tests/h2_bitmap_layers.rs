//! Every engine stores a cube's six top levels together, as a ring around the
//! Z-up world, and Halo CE and Halo 2 a volume's slices one after another.
//!
//! Needs the kits: set `BLAM_TEST_H2EK`, `BLAM_TEST_HCEEK`, `BLAM_TEST_H3EK` and
//! `BLAM_TEST_HREK` to their roots.

mod common;

/// `DDS ` and its legacy header.
const DDS_HEADER: usize = 128;

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

fn load_halo_ce(rel: &str) -> Option<TagFile> {
    let tags = common::kit("BLAM_TEST_HCEEK")?.tags();
    let bytes = std::fs::read(tags.join(rel)).ok()?;
    let layout = TagLayout::from_json(common::definitions("haloce_mcc").join("bitmap.json")).ok()?;
    read_classic_tag_file(&bytes, layout).ok()
}

fn load_gen3(env: &str, rel: &str) -> Option<TagFile> {
    TagFile::read(common::kit(env)?.tags().join(rel)).ok()
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
    assert_faces_join(&tag);
}

#[test]
fn halo_ce_cube_faces_come_back_in_directx_order_and_join() {
    let Some(tag) = load_halo_ce("characters/cyborg/bitmaps/cubemap mirror surface.bitmap") else {
        eprintln!("skipping: BLAM_TEST_HCEEK is not set");
        return;
    };
    assert_faces_join(&tag);
}

#[test]
fn halo_3_cube_faces_come_back_in_directx_order_and_join() {
    let Some(tag) = load_gen3("BLAM_TEST_H3EK", "levels/dlc/armory/armory_armory_cubemaps.bitmap") else {
        return;
    };
    assert_faces_join(&tag);
}

#[test]
fn halo_reach_cube_faces_come_back_in_directx_order_and_join() {
    let Some(tag) = load_gen3("BLAM_TEST_HREK", "levels/dlc/cex_beaver_creek/cex_beaver_creek_cex_beaver_creek_cubemaps.bitmap") else {
        return;
    };
    assert_faces_join(&tag);
}

fn assert_faces_join(tag: &TagFile) {
    let bitmap = Bitmap::new(tag).unwrap();
    assert_dds_holds_each_faces_chain(&bitmap);
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

/// A DDS cube is one whole mip chain per face, in DirectX order. The tags keep
/// a level's six faces together, so a face's chain has to be gathered up.
fn assert_dds_holds_each_faces_chain(bitmap: &Bitmap<'_>) {
    let image = bitmap.image(0).unwrap();
    let format = image.format().unwrap();
    let (size, levels) = (image.width(), image.mipmap_levels());
    let mut dds = Vec::new();
    image.write_dds(&mut dds).unwrap();
    let chain = format.surface_bytes(size, size, levels) as usize;
    let chains = &dds[DDS_HEADER..];
    eprintln!("cube of {format:?}, {size}, {levels} levels");
    // A format with no DDS of its own goes out decoded, in the same order.
    if chains.len() != chain * 6 {
        return;
    }
    let pixels = image.pixel_bytes().unwrap();
    let first_level = format.level_bytes(size, size) as usize;
    let second_level = format.level_bytes((size >> 1).max(1), (size >> 1).max(1)) as usize;
    for (face, slot) in [0, 2, 1, 3, 4, 5].into_iter().enumerate() {
        let held = &chains[face * chain..];
        assert_eq!(&held[..first_level], image.cube_face_bytes(face as u32).unwrap(), "face {face}");
        if levels > 1 {
            let stored = &pixels[first_level * 6 + slot * second_level..][..second_level];
            assert_eq!(&held[first_level..][..second_level], stored, "face {face}'s next level");
        }
    }
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
