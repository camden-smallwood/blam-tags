//! `dxn` bump/zbump bitmaps decode to real unit normals.
//!
//! `tool.exe` converts a bump/zbump source into tangent-space normals and
//! stores them as `dxn` (BC5). On PC the two stored channels are SNORM, so
//! reading them as UNORM rotates every value by half the range: a flat
//! normal is `0x00`, not `0x80`. Decoded unsigned, only ~10% of a real
//! bump map's texels land inside the unit circle; decoded signed, ~98% do.
//! The schema spells both encodings `dxn`, so `BitmapImage::format` is what
//! separates the PC (`DxnSnorm`) and Xbox 360 (`Dxn`) cases.
//!
//! `x² + y² <= 1` is the check that discriminates them, because it is the
//! precondition for `z = sqrt(1 - x² - y²)` to mean anything. When it fails
//! the reconstruction clamps and blue flatlines at 128 — which is both the
//! old behavior and what a wrongly-signed decode degenerates into.
//!
//! Skips silently when no H3 editing kit is present.

use std::path::PathBuf;

use blam_tags::bitmap::decode::decode_to_rgba8;
use blam_tags::{Bitmap, BitmapFormat, TagFile};

/// An H3EK install, via `BLAM_TEST_H3EK` or the conventional Steam roots.
fn h3ek_tags() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("BLAM_TEST_H3EK") {
        let p = PathBuf::from(p);
        return p.is_dir().then_some(p);
    }
    [
        "C:/Program Files (x86)/Steam/steamapps/common/H3EK/tags",
        "D:/SteamLibrary/steamapps/common/H3EK/tags",
        "E:/SteamLibrary/steamapps/common/H3EK/tags",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_dir())
}

/// Fraction of texels whose stored X/Y form a vector no longer than the
/// unit normal they are meant to be two thirds of.
fn fraction_within_unit_circle(rgba: &[u8]) -> f32 {
    let mut inside = 0usize;
    for px in rgba.chunks_exact(4) {
        let x = px[0] as f32 / 127.5 - 1.0;
        let y = px[1] as f32 / 127.5 - 1.0;
        // The 1.02 slack absorbs the quantization of an exactly-flat
        // texel, which lands a hair outside after the byte round-trip.
        if x * x + y * y <= 1.02 {
            inside += 1;
        }
    }
    inside as f32 / (rgba.len() / 4) as f32
}

/// Two shipped bump maps — one terrain, one object — decode to normals
/// that are actually normalized, with a blue channel that varies.
#[test]
fn shipped_h3_bump_bitmaps_decode_to_unit_normals() {
    let Some(tags) = h3ek_tags() else {
        eprintln!("skipping: no H3 editing kit (set BLAM_TEST_H3EK to its `tags` directory)");
        return;
    };

    const BUMPS: [&str; 2] = [
        "levels/dlc/bunkerworld/bitmaps/nature/grassdirt_bump.bitmap",
        "objects/levels/solo/020_base/bitmaps/monitor_bump.bitmap",
    ];

    let mut checked = 0;
    for rel in BUMPS {
        let path = tags.join(rel);
        if !path.is_file() {
            continue;
        }
        let tag = TagFile::read(&path).expect("read bump bitmap tag");
        let bitmap = Bitmap::new(&tag).expect("bitmap tag exposes images");
        let image = bitmap.image(0).expect("bitmap has an image");

        // The tag says `dxn`; a PC build means the signed reading of it.
        assert_eq!(image.format_name().as_deref(), Some("dxn"), "{rel}");
        assert_eq!(
            image.format().expect("format resolves"),
            BitmapFormat::DxnSnorm,
            "{rel} is a PC tag, so its `dxn` must resolve to the signed variant",
        );

        let (w, h) = (image.width(), image.height());
        let pixels = image.pixel_bytes().expect("pixel bytes");
        let mip0_len = BitmapFormat::DxnSnorm.level_bytes(w, h) as usize;
        let rgba =
            decode_to_rgba8(BitmapFormat::DxnSnorm, w, h, &pixels[..mip0_len], bitmap.p8_palette())
                .expect("decode mip 0");

        let inside = fraction_within_unit_circle(&rgba);
        assert!(
            inside > 0.95,
            "{rel}: only {:.1}% of texels form a valid normal — \
             endpoints are being read with the wrong signedness",
            inside * 100.0,
        );

        // Z is derived, so a flat blue plane means it was never derived
        // (the old behavior) or was clamped away everywhere.
        let blues: Vec<u8> = rgba.chunks_exact(4).map(|px| px[2]).collect();
        assert!(
            blues.iter().any(|&b| b != blues[0]),
            "{rel}: blue channel is a constant {} — Z was not reconstructed",
            blues[0],
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "found an H3 kit at {} but none of the expected bump tags — kit incomplete?",
        tags.display(),
    );
}
