//! Bitmap → DDS extraction support for `.bitmap` tag files.
//!
//! Two layers:
//!
//! 1. [`BitmapFormat`] — the subset of the schema's `bitmap_formats`
//!    enum that maps directly to a DDS pixelformat without a pixel
//!    decoder pass. Mapping coverage matches TagTool's
//!    `BitmapDdsFormatDetection` table plus the single-channel and
//!    16-bit-per-channel formats observed in halo3_mcc /
//!    haloreach_mcc bitmap corpora. Formats outside this set
//!    (`dxn_mono_alpha`, `ctx1`, etc.) need a pixel decoder and are
//!    surfaced via [`BitmapError::FormatNotSupported`].
//!
//! 2. [`Bitmap`] / [`BitmapImage`] — high-level accessors over a
//!    parsed [`TagFile`], wrapping the `bitmaps[]` block + the
//!    top-level `processed pixel data` blob. Each image carries the
//!    metadata (format, dimensions, mipmap levels, type) plus a
//!    sliced view of its raw pixel bytes.
//!
//! Block-compressed math: BC formats store one block (8 or 16 bytes)
//! per 4×4 pixel region. Mips smaller than 4×4 still cost one block.
//! See [`BitmapFormat::level_bytes`].
//!
//! The on-disk pipeline for halo3_mcc / haloreach_mcc bitmaps is
//! "everything inline" — every observed `.bitmap` carries non-empty
//! `processed pixel data` and no populated `hardware textures`
//! resource block. This module is built around that case; cache-paged
//! bitmaps would need a different code path (out of scope here).

use std::io::{Read, Write};

use flate2::read::ZlibDecoder;

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::typed_enums::{Enum, Flags, SchemaEnum};
use xbox360::{EndianSwap, SurfaceDef, TextureType, level_data};

pub mod dds;
pub mod decode;
pub mod encode;
pub mod format;
pub mod layout;
mod p8;
pub use p8::P8Palette;
pub mod tiff;
pub mod xbox360;

pub use format::{
    BitmapCurve, BitmapCurveOverride, BitmapDataFlags, BitmapDicerFlags, BitmapDownsampleFilter,
    BitmapDownsampleFlags, BitmapForceFormat, BitmapFormat, BitmapGroupFlags, BitmapMoreFlags,
    BitmapPacker, BitmapSlicer, BitmapSwizzle, BitmapType, BitmapUsage, BitmapUsageFlags,
};

use dds::{needs_decode_for_dds, write_dds, write_dds_dx10};

/// Errors returned by the bitmap walkers and DDS writer.
#[derive(Debug)]
pub enum BitmapError {
    /// Root struct doesn't expose the fields we expect from a
    /// `.bitmap` tag (no `bitmaps` block or no `processed pixel data`).
    NotABitmapTag,
    /// `format` enum value resolved to a name that isn't in
    /// [`BitmapFormat::from_schema_name`]. Carries the name so callers
    /// can report which format failed.
    FormatNotSupported(String),
    /// `bitmaps[i].pixels offset/size` walks past the end of the
    /// `processed pixel data` blob.
    PixelSliceOutOfBounds { offset: u64, size: u64, available: u64 },
    /// Bitmap type isn't 2D / cube / array — 3D textures aren't
    /// modeled yet.
    UnsupportedTextureType(String),
    /// Phase 2 TIFF writer is 2D-only; cube / array / 3D layouts
    /// land in Phase 4.
    TiffLayoutDeferred(String),
    /// Underlying `tiff` crate failure (carries the error string —
    /// we don't expose the crate's error enum publicly to keep our
    /// API surface stable across `tiff` versions).
    Tiff(String),
    Io(std::io::Error),
}

impl std::fmt::Display for BitmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotABitmapTag => write!(f, "tag is not a recognizable bitmap (missing `bitmaps` or `processed pixel data`)"),
            Self::FormatNotSupported(name) => write!(f, "bitmap format `{name}` is not directly DDS-mappable (needs decoder)"),
            Self::PixelSliceOutOfBounds { offset, size, available } =>
                write!(f, "pixels slice [{offset}..{}] exceeds processed pixel data ({available} bytes)", offset + size),
            Self::UnsupportedTextureType(name) => write!(f, "bitmap type `{name}` is not supported (only `2D texture`, `cube map`, and `array`)"),
            Self::TiffLayoutDeferred(kind) => write!(f, "TIFF emission for bitmap type `{kind}` is deferred to Phase 4 (cube cross / array strip layouts)"),
            Self::Tiff(msg) => write!(f, "tiff error: {msg}"),
            Self::Io(e) => write!(f, "io error: {e}"),
        }
    }
}

impl std::error::Error for BitmapError {}

impl From<std::io::Error> for BitmapError {
    fn from(value: std::io::Error) -> Self { Self::Io(value) }
}

/// High-level view of a `.bitmap` tag — the `bitmaps[]` block plus
/// each image's resolved pixel bytes. Construct from a parsed
/// [`TagFile`] via [`Bitmap::new`].
///
/// Pixel storage varies by build:
///
/// - **MCC (and other PC tags):** all images share a top-level
///   `processed pixel data` blob; each image's `pixels offset`
///   indexes into it.
/// - **Xbox 360 monolithic builds:** each image's pixels live in
///   its own per-image `texture resource` (a pageable resource
///   whose contents are streamed from a `cache_N` partition and
///   pre-hydrated by [`crate::monolithic::MonolithicCache::read_tag`]).
///
/// `Bitmap` normalizes both cases by owning a `Vec<u8>` per image
/// — sliced from the shared blob for MCC, or copied from each
/// resource's payload for X360. Per-image consumers
/// ([`BitmapImage`]) get a borrowed `&[u8]` covering just their
/// own data.
pub struct Bitmap<'a> {
    bitmaps: TagBlock<'a>,
    /// The `sequences[]` block (sprite-sheet atlas layout), if present.
    /// Populated for sprite/animated bitmaps; empty for plain textures.
    sequences: Option<TagBlock<'a>>,
    per_image_pixels: Vec<Vec<u8>>,
    /// Per-image override of the mipmap level count. `None` means
    /// trust the tag's `mipmap count` field. `Some(n)` is used when
    /// we synthesize a different mip layout — currently only set
    /// for X360 bitmaps where we deliver just the base mip after
    /// detiling.
    per_image_mip_override: Vec<Option<u32>>,
    /// Which engine's p8 palette to decode palettized-normal images with
    /// (Halo 1 and Halo 2 use different tables). Derived from the tag.
    p8_palette: P8Palette,
    /// `true` when the pixel bytes came from the Xbox 360 side of the
    /// tag (`xenon bitmaps[]` + per-image texture resources) rather
    /// than the PC `processed pixel data` blob. Only `dxn` cares:
    /// the two builds disagree about whether its endpoints are signed.
    x360: bool,
}

impl<'a> Bitmap<'a> {
    /// Wrap a parsed `.bitmap` tag. Errors with [`BitmapError::NotABitmapTag`]
    /// if the tag doesn't expose a `bitmaps[]` (or `xenon bitmaps[]`)
    /// block with usable pixel data.
    ///
    /// Halo 4 bitmap tags carry **two parallel image-metadata blocks**
    /// — `bitmaps[]` (the PC-format mirror) and `xenon bitmaps[]`
    /// (the Xbox-360-format mirror with tiling / byte-order flags)
    /// — plus a third parallel `hardware textures[]` block that
    /// carries each image's pageable `texture resource`. MCC tags
    /// keep `bitmaps[]` populated and put pixels in
    /// `processed pixel data`; monolithic X360 builds use
    /// `xenon bitmaps[]` for metadata and per-image
    /// `hardware textures[i]/texture resource` for pixel bytes.
    pub fn new(tag: &'a TagFile) -> Result<Self, BitmapError> {
        let root = tag.root();

        let pc_bitmaps = root.field_path("bitmaps").and_then(|f| f.as_block());
        let xenon_bitmaps = root.field_path("xenon bitmaps").and_then(|f| f.as_block());
        let hardware_textures = root.field_path("hardware textures").and_then(|f| f.as_block());

        let shared_pc_pixels = root
            .field_path("processed pixel data")
            .and_then(|f| f.as_data())
            .unwrap_or(&[]);
        let shared_xenon_pixels = root
            .field_path("xenon processed pixel data")
            .and_then(|f| f.as_data())
            .unwrap_or(&[]);

        // Choose the metadata block (drives image count + format).
        // X360 mode kicks in when PC pixel data is empty and the
        // X360-specific blocks exist.
        let use_x360 = shared_pc_pixels.is_empty() && xenon_bitmaps.is_some();
        let (bitmaps, shared_pixels) = if use_x360 {
            (xenon_bitmaps.unwrap(), shared_xenon_pixels)
        } else if let Some(pb) = pc_bitmaps {
            (pb, shared_pc_pixels)
        } else if let Some(xb) = xenon_bitmaps {
            (xb, shared_xenon_pixels)
        } else {
            return Err(BitmapError::NotABitmapTag);
        };

        let mut per_image_pixels: Vec<Vec<u8>> = Vec::with_capacity(bitmaps.len());
        let mut per_image_mip_override: Vec<Option<u32>> = Vec::with_capacity(bitmaps.len());
        for (i, elem) in bitmaps.iter().enumerate() {
            let hw_elem = if use_x360 {
                hardware_textures.as_ref().and_then(|b| b.element(i))
            } else {
                None
            };
            let (pixels, mip_override) = resolve_image_pixels(elem, hw_elem, shared_pixels)?;
            per_image_pixels.push(pixels);
            per_image_mip_override.push(mip_override);
        }

        // If neither source produced data for any image, this isn't
        // a usable bitmap.
        if per_image_pixels.iter().all(|p| p.is_empty()) {
            return Err(BitmapError::NotABitmapTag);
        }

        // Sprite-sheet atlas layout. Prefer the processed `sequences[]`
        // (the runtime atlas tool.exe bakes with the normalized sprite
        // rects); fall back to `manual sequences[]` (the authored input).
        let sequences = root
            .field_path("sequences")
            .and_then(|f| f.as_block())
            .filter(|b| !b.is_empty())
            .or_else(|| {
                root.field_path("manual sequences").and_then(|f| f.as_block())
            });

        let p8_palette = if matches!(crate::game::Game::of(tag), crate::game::Game::Halo1) {
            P8Palette::Halo1
        } else {
            P8Palette::Halo2
        };
        Ok(Self {
            bitmaps,
            sequences,
            per_image_pixels,
            per_image_mip_override,
            p8_palette,
            x360: use_x360,
        })
    }

    /// Decode the sprite-sheet `sequences[]` block: each sequence is a
    /// run of frames (sprites), each sprite a normalized sub-rect of the
    /// atlas image. Empty for plain (non-sprite) bitmaps. Used by
    /// particle sprite-sheet animation to bake per-frame UVs.
    pub fn sequences(&self) -> Vec<BitmapSequence> {
        let Some(block) = &self.sequences else { return Vec::new() };
        let mut out = Vec::with_capacity(block.len());
        for i in 0..block.len() {
            let Some(seq) = block.element(i) else { continue };
            let first_bitmap_index = seq.read_int_any("first bitmap index").unwrap_or(0) as i16;
            let bitmap_count = seq.read_int_any("bitmap count").unwrap_or(0) as i16;
            let sprites = seq
                .field("sprites")
                .and_then(|f| f.as_block())
                .map(|sb| {
                    let mut sprites = Vec::with_capacity(sb.len());
                    for j in 0..sb.len() {
                        let Some(sp) = sb.element(j) else { continue };
                        let reg = sp.read_point2d("registration point");
                        sprites.push(BitmapSprite {
                            bitmap_index: sp.read_int_any("bitmap index").unwrap_or(0) as i16,
                            left: sp.read_real("left").unwrap_or(0.0),
                            right: sp.read_real("right").unwrap_or(1.0),
                            top: sp.read_real("top").unwrap_or(0.0),
                            bottom: sp.read_real("bottom").unwrap_or(1.0),
                            registration_point: [reg.x, reg.y],
                        });
                    }
                    sprites
                })
                .unwrap_or_default();
            out.push(BitmapSequence { first_bitmap_index, bitmap_count, sprites });
        }
        out
    }

    /// Number of images in the tag's `bitmaps[]` block.
    pub fn len(&self) -> usize { self.bitmaps.len() }

    /// `true` if this tag has no images.
    pub fn is_empty(&self) -> bool { self.bitmaps.is_empty() }

    /// Get the image at `index`, or `None` if out of range.
    pub fn image(&self, index: usize) -> Option<BitmapImage<'_>> {
        let elem = self.bitmaps.element(index)?;
        let pixels = self.per_image_pixels.get(index)?.as_slice();
        let mip_override = self.per_image_mip_override.get(index).copied().flatten();
        Some(BitmapImage {
            elem,
            pixels,
            mip_override,
            p8_palette: self.p8_palette,
            x360: self.x360,
        })
    }

    /// Iterate every image in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = BitmapImage<'_>> + '_ {
        let per_image = &self.per_image_pixels;
        let overrides = &self.per_image_mip_override;
        let p8_palette = self.p8_palette;
        let x360 = self.x360;
        self.bitmaps.iter().enumerate().map(move |(i, elem)| BitmapImage {
            elem,
            pixels: per_image[i].as_slice(),
            mip_override: overrides[i],
            p8_palette,
            x360,
        })
    }

    /// The engine p8 palette this tag's palettized-normal images decode with.
    pub fn p8_palette(&self) -> P8Palette {
        self.p8_palette
    }
}

/// One `sequences[]` entry — a run of sprite frames. For multi-image
/// (non-atlas) sprite bitmaps the frames are whole images
/// (`first_bitmap_index .. + bitmap_count`); for atlas bitmaps the
/// `sprites` carry the sub-rects.
#[derive(Debug, Clone)]
pub struct BitmapSequence {
    pub first_bitmap_index: i16,
    pub bitmap_count: i16,
    pub sprites: Vec<BitmapSprite>,
}

/// One sprite — a normalized sub-rect of an atlas image plus a
/// registration (pivot) point. `left/right/top/bottom` are in `[0,1]`
/// UV space.
#[derive(Debug, Clone)]
pub struct BitmapSprite {
    pub bitmap_index: i16,
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
    pub registration_point: [f32; 2],
}

// ===========================================================================
// Strongly-typed bitmap-group walker (Ares `bitmap_group_v2` / `bitmap_data`).
//
// An owned metadata tree in the `render_model::RenderModel` style, SEPARATE
// from the lazy pixel decoder [`Bitmap`] (which stays the path for extracting
// image data). Field order + names mirror Ares `bitmaps/bitmap_group.h`
// (`bitmap_group_v2`) and `bitmaps/bitmaps.h` (`bitmap_data`). Reads are
// by-schema-name (fold-matched), so cross-game field-order deltas are tolerated
// and the struct order is purely for readability.
// ===========================================================================

/// Ares `bitmap_group_v2` — the authored bitmap-group definition.
#[derive(Debug, Clone, Default)]
pub struct BitmapDefinition {
    /// `usage_index` — how the bitmap is used (drives the cache-build format,
    /// e.g. [`BitmapUsage::WarpMap`] = EMBM distortion). Engine stores it raw.
    pub usage: Enum<BitmapUsage, i32>,
    pub flags: Flags<BitmapGroupFlags, u16>,
    pub sprite_spacing: i16,
    /// `bump_height` — apparent bump height, in texture repeats.
    pub bump_height: f32,
    /// `detail_fade_factor` — detail/illum-map mip fade, `[0,1]`.
    pub detail_fade_factor: f32,
    pub curve_override: Enum<BitmapCurveOverride, i8>,
    /// `mipmap_limit` — max authored mip level (0 = usage default).
    pub mipmap_limit: i8,
    pub force_format: Enum<BitmapForceFormat, i16>,
    /// `usage_override` block — per-usage import overrides (authoring tooling).
    pub usage_overrides: Vec<BitmapUsageOverride>,
    pub sequences: Vec<BitmapSequence>,
    /// `bitmaps` block — the per-image metadata (pixels via [`Bitmap`]).
    pub images: Vec<BitmapImageDef>,
}

/// Ares `bitmap_data` — one image's authored metadata (a `bitmaps[]` element).
#[derive(Debug, Clone, Default)]
pub struct BitmapImageDef {
    pub width: u16,
    pub height: u16,
    pub depth: u8,
    pub more_flags: Flags<BitmapMoreFlags, u8>,
    pub bitmap_type: Enum<BitmapType, i16>,
    /// Pixel format (Ares `e_bitmap_format`). `None` if unrecognized.
    pub format: Option<BitmapFormat>,
    pub flags: Flags<BitmapDataFlags, u16>,
    /// Integer `point2d` — the bitmap "center" (e.g. for particles).
    pub registration_point: crate::math::Point2d,
    /// `mipmap_count_excluding_highest` — total mips is this `+ 1`.
    pub mipmap_count_excluding_highest: i8,
    pub curve: Enum<BitmapCurve, i8>,
    pub interleaved_interop_index: i8,
    pub interleaved_texture_index: i8,
    pub pixels_offset: i32,
    pub pixels_size: i32,
    pub high_res_pixels_offset: i32,
    pub high_res_pixels_size: i32,
}

/// Ares `bitmap_usage_block` — a per-usage import override.
#[derive(Debug, Clone, Default)]
pub struct BitmapUsageOverride {
    /// `source gamma` — 0.0 uses the Xenon-curve default.
    pub source_gamma: f32,
    pub bitmap_curve: Enum<BitmapCurve, i32>,
    pub flags: Flags<BitmapUsageFlags, u8>,
    pub slicer: Enum<BitmapSlicer, i8>,
    pub dicer_flags: Flags<BitmapDicerFlags, u16>,
    pub packer: Enum<BitmapPacker, i8>,
    pub bitmap_type: Enum<BitmapType, i8>,
    pub mipmap_limit: i16,
    pub downsample_filter: Enum<BitmapDownsampleFilter, i16>,
    pub downsample_flags: Flags<BitmapDownsampleFlags, u16>,
    pub sprite_background_color: crate::math::RealRgbColor,
    pub swizzle_red: Enum<BitmapSwizzle, i8>,
    pub swizzle_green: Enum<BitmapSwizzle, i8>,
    pub swizzle_blue: Enum<BitmapSwizzle, i8>,
    pub swizzle_alpha: Enum<BitmapSwizzle, i8>,
    pub bitmap_format: Enum<BitmapForceFormat, i32>,
}

impl BitmapDefinition {
    /// Walk a parsed `bitmap` (bitm) tag into the strongly-typed metadata tree.
    /// By-name reads tolerate cross-game field-order deltas; pixel data is
    /// delivered separately by [`Bitmap`].
    pub fn from_tag(tag: &TagFile) -> Self {
        let root = tag.root();
        Self {
            usage: root.try_read_enum("Usage").unwrap_or_default(),
            flags: root.try_read_flags("Flags").unwrap_or_default(),
            sprite_spacing: root.read_int_any("sprite spacing").unwrap_or(0) as i16,
            bump_height: root.read_real("bump map height").unwrap_or(0.0),
            detail_fade_factor: root.read_real("fade factor").unwrap_or(0.0),
            curve_override: root.try_read_enum("curve mode").unwrap_or_default(),
            mipmap_limit: root.read_int_any("max mipmap level").unwrap_or(0) as i8,
            force_format: root.try_read_enum("force bitmap format").unwrap_or_default(),
            usage_overrides: read_usage_overrides(&root),
            sequences: read_sequences(&root),
            images: read_image_defs(&root),
        }
    }
}

fn read_sequences(root: &TagStruct<'_>) -> Vec<BitmapSequence> {
    let Some(block) = root.field("sequences").and_then(|f| f.as_block()) else { return Vec::new() };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(seq) = block.element(i) else { continue };
        let sprites = seq
            .field("sprites")
            .and_then(|f| f.as_block())
            .map(|sb| {
                (0..sb.len())
                    .filter_map(|j| sb.element(j))
                    .map(|sp| {
                        let reg = sp.read_point2d("registration point");
                        BitmapSprite {
                            bitmap_index: sp.read_int_any("bitmap index").unwrap_or(0) as i16,
                            left: sp.read_real("left").unwrap_or(0.0),
                            right: sp.read_real("right").unwrap_or(1.0),
                            top: sp.read_real("top").unwrap_or(0.0),
                            bottom: sp.read_real("bottom").unwrap_or(1.0),
                            registration_point: [reg.x, reg.y],
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(BitmapSequence {
            first_bitmap_index: seq.read_int_any("first bitmap index").unwrap_or(0) as i16,
            bitmap_count: seq.read_int_any("bitmap count").unwrap_or(0) as i16,
            sprites,
        });
    }
    out
}

fn read_image_defs(root: &TagStruct<'_>) -> Vec<BitmapImageDef> {
    let Some(block) = root.field("bitmaps").and_then(|f| f.as_block()) else { return Vec::new() };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(BitmapImageDef {
            width: e.read_int_any("width").unwrap_or(0).max(0) as u16,
            height: e.read_int_any("height").unwrap_or(0).max(0) as u16,
            depth: e.read_int_any("depth").unwrap_or(1).max(0) as u8,
            more_flags: e.try_read_flags("more flags").unwrap_or_default(),
            bitmap_type: e.try_read_enum("type").unwrap_or_default(),
            format: e.read_enum_name("format").and_then(|n| BitmapFormat::from_schema_name(&n)),
            flags: e.try_read_flags("flags").unwrap_or_default(),
            registration_point: match e.field("registration point").and_then(|f| f.value()) {
                Some(crate::fields::TagFieldData::Point2d(p)) => p,
                _ => crate::math::Point2d::default(),
            },
            mipmap_count_excluding_highest: e.read_int_any("mipmap count").unwrap_or(0) as i8,
            curve: e.try_read_enum("curve").unwrap_or_default(),
            interleaved_interop_index: e.read_int_any("interleaved interop").unwrap_or(-1) as i8,
            interleaved_texture_index: e.read_int_any("interleaved texture index").unwrap_or(0) as i8,
            pixels_offset: e.read_int_any("pixels offset").unwrap_or(0) as i32,
            pixels_size: e.read_int_any("pixels size").unwrap_or(0) as i32,
            high_res_pixels_offset: e.read_int_any("high res pixels offset offset").unwrap_or(0) as i32,
            high_res_pixels_size: e.read_int_any("high res pixels size").unwrap_or(0) as i32,
        });
    }
    out
}

fn read_usage_overrides(root: &TagStruct<'_>) -> Vec<BitmapUsageOverride> {
    let Some(block) = root.field("usage override").and_then(|f| f.as_block()) else { return Vec::new() };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(e) = block.element(i) else { continue };
        out.push(BitmapUsageOverride {
            source_gamma: e.read_real("source gamma").unwrap_or(0.0),
            bitmap_curve: e.try_read_enum("bitmap curve").unwrap_or_default(),
            flags: e.try_read_flags("flags").unwrap_or_default(),
            slicer: e.try_read_enum("slicer").unwrap_or_default(),
            dicer_flags: e.try_read_flags("dicer flags").unwrap_or_default(),
            packer: e.try_read_enum("packer").unwrap_or_default(),
            bitmap_type: e.try_read_enum("type").unwrap_or_default(),
            mipmap_limit: e.read_int_any("mipmap limit").unwrap_or(0) as i16,
            downsample_filter: e.try_read_enum("downsample filter").unwrap_or_default(),
            downsample_flags: e.try_read_flags("downsample flags").unwrap_or_default(),
            sprite_background_color: e.read_rgb("sprite background color"),
            swizzle_red: e.try_read_enum("swizzle red").unwrap_or_default(),
            swizzle_green: e.try_read_enum("swizzle green").unwrap_or_default(),
            swizzle_blue: e.try_read_enum("swizzle blue").unwrap_or_default(),
            swizzle_alpha: e.try_read_enum("swizzle alpha").unwrap_or_default(),
            bitmap_format: e.try_read_enum("bitmap format").unwrap_or_default(),
        });
    }
    out
}

/// Resolve one image's pixel byte slice plus an optional mip-count
/// override. On X360 builds the bytes come from
/// `hardware textures[i]/texture resource`, get detiled +
/// byte-swapped, and the returned override pins the chain to a
/// single base mip. On PC/MCC they're sliced from the shared
/// `processed pixel data` blob at this image's `pixels offset` and
/// no override is set.
fn resolve_image_pixels<'a>(
    elem: TagStruct<'a>,
    hw_elem: Option<TagStruct<'a>>,
    shared_pixels: &'a [u8],
) -> Result<(Vec<u8>, Option<u32>), BitmapError> {
    if let Some(hw) = hw_elem
        && let Some(resource) = hw
            .field("texture resource")
            .and_then(|f| f.as_resource())
        && resource.exploded_payload().is_some()
    {
        let (primary, secondary) = x360_buffers(&resource);
        return convert_x360_image_full(elem, primary, secondary);
    }

    if !shared_pixels.is_empty() {
        let offset = elem.read_int_any("pixels offset").unwrap_or(0).max(0) as usize;
        if offset <= shared_pixels.len() {
            return Ok((shared_pixels[offset..].to_vec(), None));
        }
    }

    Ok((Vec::new(), None))
}


/// How the GPU swapped this format's bytes.
///
/// By component, not by pixel. Every block format is a run of 16-bit words
/// whatever its pixels are; an uncompressed format swaps by its own component
/// width. A format with no `block_dims_and_size` cannot be reasoned about and
/// is left alone.
pub fn endian_swap_for(format: BitmapFormat) -> EndianSwap {
    let Some((block_width, _, bytes_per_block)) = format.block_dims_and_size() else {
        return EndianSwap::None;
    };
    if block_width > 1 {
        return EndianSwap::In16;
    }
    match bytes_per_block {
        1 => EndianSwap::None,
        4 | 12 | 16 => EndianSwap::In32,
        _ => EndianSwap::In16,
    }
}

/// Read one `xenon bitmaps[i]` element into the shape the layout rules want.
///
/// Everything is taken from the tag rather than assumed: whether the surface was
/// tiled, whether its bytes were swapped, and whether the base level was moved
/// into the high-resolution buffer are all flags the build wrote, and a Reach
/// corpus disagrees with every guess about them.
fn surface_def(elem: TagStruct<'_>) -> Result<SurfaceDef, BitmapError> {
    let format_name = elem.read_enum_name("format").ok_or(BitmapError::NotABitmapTag)?;
    let format = BitmapFormat::from_schema_name(&format_name)
        .ok_or_else(|| BitmapError::FormatNotSupported(format_name.clone()))?;
    let (block_width, block_height, bytes_per_block) = format
        .block_dims_and_size()
        .ok_or(BitmapError::FormatNotSupported(format_name))?;

    let type_name = elem.read_enum_name("type").unwrap_or_default();
    let texture_type = if type_name.eq_ignore_ascii_case("cube map") {
        TextureType::CubeMap
    } else if type_name.eq_ignore_ascii_case("3D texture") {
        TextureType::Volume
    } else {
        TextureType::Flat
    };

    let more_flags: Flags<BitmapMoreFlags, u8> =
        elem.try_read_flags("more flags").unwrap_or_default();

    Ok(SurfaceDef {
        width: elem.read_int_any("width").unwrap_or(0).max(0) as u32,
        height: elem.read_int_any("height").unwrap_or(0).max(0) as u32,
        depth: (elem.read_int_any("depth").unwrap_or(1).max(1) as u32).max(1),
        texture_type,
        bits_per_pixel: bytes_per_block * 8 / (block_width * block_height),
        block_width,
        block_height,
        tiled: more_flags.contains(BitmapMoreFlags::X360Tiled),
        endian_swap: if more_flags.contains(BitmapMoreFlags::X360ByteOrder) {
            endian_swap_for(format)
        } else {
            EndianSwap::None
        },
        mipmap_count: elem.read_int_any("mipmap count").unwrap_or(0).max(0) as u32,
        // Filled in by the caller, which is the only side that can see whether
        // there is a secondary buffer at all.
        high_res_in_secondary: false,
    })
}

/// Convert an Xbox 360 image into the linear, PC-ordered pixel buffer a
/// `processed pixel data` blob holds.
///
/// Every layer, every level, laid out layer-major with each layer's full mip
/// chain — which is the order [`BitmapImage::pixel_bytes`] reads back and the
/// order a DDS writer expects.
///
/// The two buffers are the resource's own. A hydrated monolithic resource
/// concatenates them secondary-first; [`x360_buffers`] splits them apart again.
fn convert_x360_image_full(
    elem: TagStruct<'_>,
    primary: &[u8],
    secondary: &[u8],
) -> Result<(Vec<u8>, Option<u32>), BitmapError> {
    let mut def = surface_def(elem)?;
    if def.width == 0 || def.height == 0 {
        return Err(BitmapError::NotABitmapTag);
    }
    // Where the base level lives, asked of the data rather than of a flag. A
    // resource with a secondary buffer keeps its highest-resolution level there
    // and the rest of the chain in the primary one; a resource with only one
    // buffer keeps everything in it. The image's own
    // `xbox360 high resolution offset is valid` bit does not track this - it is
    // set on images with no secondary buffer at all, and trusting it reads the
    // mip chain as though it were the base.
    def.high_res_in_secondary = !secondary.is_empty();
    let mut out = Vec::new();
    for layer in 0..def.layer_count() {
        for level in 0..def.level_count() {
            let data = level_data(&def, primary, secondary, level, layer).ok_or(
                BitmapError::PixelSliceOutOfBounds {
                    offset: 0,
                    size: 0,
                    available: (primary.len() + secondary.len()) as u64,
                },
            )?;
            out.extend_from_slice(&data);
        }
    }
    if out.is_empty() {
        return Err(BitmapError::NotABitmapTag);
    }
    Ok((out, Some(def.level_count())))
}

/// Split a hydrated resource's payload back into its primary and secondary
/// buffers.
///
/// `MonolithicCache` concatenates them secondary-first so a consumer reading
/// mip 0 first sees the high-resolution buffer; the layout rules need them
/// apart, and the xsync header says where the seam is. A resource that was
/// already exploded on disk has no seam and is all primary.
fn x360_buffers<'a>(resource: &crate::api::TagResource<'a>) -> (&'a [u8], &'a [u8]) {
    let payload = resource.exploded_payload().unwrap_or(&[]);
    let Some(state) = resource.xsync_state() else {
        return (payload, &[]);
    };
    let secondary_len = (state.header.optional_location_size as usize).min(payload.len());
    (&payload[secondary_len..], &payload[..secondary_len])
}

/// A bitmap tag's **color plate** — the artist's original source sheet
/// (the input Tool.exe compiled the bitmap from), recovered as straight
/// RGBA8. Distinct from the per-image `processed pixel data` (the
/// compiled game texture): the color plate is lossless ARGB regardless
/// of the final compressed format, carries the full sprite-sheet layout,
/// and re-imports directly. Present on classic CE/H2 tags (gen3+ MCC
/// tags ship with the source stripped). See [`color_plate`].
pub struct ColorPlate {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 (`[R, G, B, A]` per pixel), `width*height*4` bytes.
    pub rgba: Vec<u8>,
}

impl ColorPlate {
    /// Write the color plate as a Tool-importable RGBA8 TIFF (the
    /// natural source format).
    pub fn write_tiff(&self, out: &mut impl Write) -> Result<(), BitmapError> {
        tiff::write_rgba8_tiff(out, self.width, self.height, &self.rgba)
    }

    /// Write the color plate as a single-mip A8R8G8B8 DDS.
    pub fn write_dds(&self, out: &mut impl Write) -> Result<(), BitmapError> {
        // Legacy A8R8G8B8 DDS stores little-endian BGRA bytes.
        let mut bgra = self.rgba.clone();
        for px in bgra.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        write_dds(out, BitmapFormat::A8r8g8b8, self.width, self.height, 1, false, &bgra)?;
        Ok(())
    }
}

/// Recover a bitmap tag's color plate, or `None` if it carries no source
/// (`compressed color plate data` empty / absent — common for tags
/// re-saved without source, and all gen3+ MCC tags).
///
/// On-disk shape (verified on CE + H2, cross-checked vs SnowyMouse's
/// `halo2-color-plate-extractor`): `compressed color plate data` is a
/// big-endian `u32` uncompressed-size prefix followed by a zlib stream
/// that inflates to `width*height` ARGB8888 pixels (little-endian
/// `0xAARRGGBB`, i.e. memory order `[B, G, R, A]`); we swap R↔B to get
/// RGBA. CE nests the fields under a `color plate` struct; H2 has them
/// at the tag root — handled transparently.
pub fn color_plate(tag: &TagFile) -> Result<Option<ColorPlate>, BitmapError> {
    let root = tag.root();
    let src = root
        .field_path("color plate")
        .and_then(|f| f.as_struct())
        .unwrap_or(root);

    let Some(blob) = src.field("compressed color plate data").and_then(|f| f.as_data()) else {
        return Ok(None);
    };
    let width = src.read_int_any("color plate width").unwrap_or(0).max(0) as u32;
    let height = src.read_int_any("color plate height").unwrap_or(0).max(0) as u32;
    if blob.len() < 4 || width == 0 || height == 0 {
        return Ok(None);
    }

    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(BitmapError::PixelSliceOutOfBounds { offset: 0, size: u64::MAX, available: 0 })?;

    // blob = [big-endian u32 uncompressed size][zlib stream].
    let mut rgba = Vec::with_capacity(expected);
    ZlibDecoder::new(&blob[4..])
        .read_to_end(&mut rgba)
        .map_err(BitmapError::Io)?;
    if rgba.len() != expected {
        return Err(BitmapError::PixelSliceOutOfBounds {
            offset: 0,
            size: expected as u64,
            available: rgba.len() as u64,
        });
    }

    // Stored ARGB8888 (memory `[B, G, R, A]`) → straight RGBA8.
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Ok(Some(ColorPlate { width, height, rgba }))
}

/// One element of `bitmaps[]` — metadata plus the slice of pixel
/// bytes this image owns. Pixel storage is normalized by
/// [`Bitmap::new`]: the slice always starts at this image's first
/// pixel byte, regardless of MCC vs X360 origin.
#[derive(Clone, Copy)]
pub struct BitmapImage<'a> {
    elem: TagStruct<'a>,
    pixels: &'a [u8],
    /// `Some(n)` overrides the schema-derived mip count. Set only
    /// when [`Bitmap::new`] synthesized a mip chain shorter than
    /// what the tag declares (X360 single-mip case).
    mip_override: Option<u32>,
    /// Engine p8 palette to use for palettized-normal decode (from the tag).
    p8_palette: P8Palette,
    /// `true` when these pixels came from the tag's Xbox 360 side.
    /// See [`Bitmap::x360`] — it decides how `dxn` endpoints are read.
    x360: bool,
}

impl<'a> BitmapImage<'a> {
    /// Base-mip width in pixels.
    pub fn width(&self) -> u32 { self.elem.read_int_any("width").unwrap_or(0).max(0) as u32 }

    /// Base-mip height in pixels.
    pub fn height(&self) -> u32 { self.elem.read_int_any("height").unwrap_or(0).max(0) as u32 }

    /// 3D-texture depth (or array layer count for arrays). Always `>= 1`.
    pub fn depth(&self) -> u32 { self.elem.read_int_any("depth").unwrap_or(1).max(1) as u32 }

    /// Total mipmap levels including the base level. The schema's
    /// `mipmap count` excludes the base, so this is `that + 1` —
    /// unless [`Bitmap::new`] overrode the count, which happens
    /// when we synthesize a shorter chain (e.g. X360 single-mip).
    pub fn mipmap_levels(&self) -> u32 {
        if let Some(n) = self.mip_override {
            return n;
        }
        self.elem.read_int_any("mipmap count").unwrap_or(0).max(0) as u32 + 1
    }

    /// The schema's per-image format name (e.g. `"dxt5"`, `"dxn"`).
    /// h3 uses `short_enum` while reach uses `char_enum`; both map the
    /// same way through the wider enum-name reader.
    pub fn format_name(&self) -> Option<String> {
        self.elem.read_enum_name("format")
    }

    /// The format mapped to a known [`BitmapFormat`], if supported.
    ///
    /// The schema spells both DXN encodings `dxn`, but PC builds store
    /// signed endpoints and Xbox 360 builds store unsigned ones, so
    /// this resolves the PC spelling to [`BitmapFormat::DxnSnorm`].
    /// [`Self::format_name`] still reports what the tag says.
    pub fn format(&self) -> Result<BitmapFormat, BitmapError> {
        let name = self.format_name().ok_or(BitmapError::NotABitmapTag)?;
        let format = BitmapFormat::from_schema_name(&name)
            .ok_or(BitmapError::FormatNotSupported(name))?;
        Ok(match format {
            BitmapFormat::Dxn if !self.x360 => BitmapFormat::DxnSnorm,
            other => other,
        })
    }

    /// The schema's per-image type name (e.g. `"2D texture"`, `"cube map"`).
    pub fn type_name(&self) -> Option<String> {
        self.elem.read_enum_name("type")
    }

    /// The bitmap's gamma curve. Defaults to [`BitmapCurve::Unknown`]
    /// when the field is missing or out of range — the engine treats
    /// "unknown" as "use default for this format/usage" anyway.
    pub fn curve(&self) -> BitmapCurve {
        // Resolve by embedded schema name (drift-immune); fall back to
        // Unknown when the field is absent or its name doesn't resolve.
        self.elem
            .read_enum_name("curve")
            .and_then(|n| BitmapCurve::from_schema_name(&n))
            .unwrap_or(BitmapCurve::Unknown)
    }

    /// `true` for cube map textures (six 2D surfaces under one image).
    /// Type-name match is case-insensitive (classic CE uses lowercase
    /// `2d texture`/`3d texture`; gen3+/H2 use `2D texture`).
    pub fn is_cube(&self) -> bool {
        self.type_name().is_some_and(|t| t.eq_ignore_ascii_case("cube map"))
    }

    /// `true` for texture arrays (`depth` layers of 2D surfaces).
    pub fn is_array(&self) -> bool {
        self.type_name().is_some_and(|t| t.eq_ignore_ascii_case("array"))
    }

    /// Number of stacked surfaces beyond the base mip chain:
    /// - 2D: 1
    /// - cube map: 6 (faces)
    /// - array: `depth` (layer count)
    pub fn layer_count(&self) -> u32 {
        if self.is_cube() {
            6
        } else if self.is_array() {
            self.depth()
        } else {
            1
        }
    }

    /// Byte slice covering this image's full mipmap chain across
    /// all faces / layers.
    ///
    /// The slice length is computed from format + dimensions +
    /// mipmap count + layer count rather than the tag's `pixels
    /// size` field, because the size field is sometimes stale on
    /// MCC tags (e.g. inflated by 2× from an original split-resource
    /// layout). The slice starts at offset 0 of this image's
    /// per-image pixel buffer; [`Bitmap::new`] has already resolved
    /// the right base (either the shared `processed pixel data` at
    /// the image's `pixels offset`, or the hydrated per-image
    /// `texture resource`).
    pub fn pixel_bytes(&self) -> Result<&'a [u8], BitmapError> {
        let format = self.format()?;
        let layers = self.layer_count() as u64;
        let expected = format.surface_bytes(self.width(), self.height(), self.mipmap_levels())
            .saturating_mul(layers) as usize;

        if expected > self.pixels.len() {
            return Err(BitmapError::PixelSliceOutOfBounds {
                offset: 0,
                size: expected as u64,
                available: self.pixels.len() as u64,
            });
        }
        Ok(&self.pixels[..expected])
    }

    /// Write a DDS file representing this image. Picks the legacy
    /// DDS pixelformat when possible; falls back to the DXT10
    /// extension when the format or type requires it (texture
    /// arrays, or formats with no legacy fourcc / pixelformat).
    /// Halo-specific decoder-only formats (`dxn_mono_alpha`, `ctx1`,
    /// the `dxt3a*` family, `dxt5nm`, …) are CPU-decompressed to
    /// A8R8G8B8 first, then routed through the normal DDS writer.
    pub fn write_dds(&self, out: &mut impl Write) -> Result<(), BitmapError> {
        let format = self.format()?;
        let type_name = self.type_name();
        let is_cube = self.is_cube();
        let is_array = self.is_array();
        // 3D textures aren't observed in either corpus and the
        // layer / depth semantics need a different writer path —
        // refuse for now.
        let is_2d = type_name.as_deref().is_none_or(|t| t.eq_ignore_ascii_case("2d texture"));
        if !is_cube && !is_array && !is_2d {
            return Err(BitmapError::UnsupportedTextureType(type_name.unwrap_or_default()));
        }
        let bytes = self.pixel_bytes()?;

        // Formats with no clean legacy DDS pixelformat get decoded
        // to RGBA8 per mip-and-face, then written as A8R8G8B8.
        if needs_decode_for_dds(format) {
            let decoded = self.decode_all_to_rgba8(format, bytes)?;
            return self.write_dds_with_format(
                out, BitmapFormat::A8r8g8b8, &decoded, is_cube, is_array,
            );
        }

        self.write_dds_with_format(out, format, bytes, is_cube, is_array)
    }

    /// Decode every mip of every face/layer to RGBA8, concatenated in
    /// the same `[face0_mips ... faceN_mips]` order the original
    /// bytes were laid out. Used to substitute A8R8G8B8 for formats
    /// that have no native DDS pixelformat.
    fn decode_all_to_rgba8(
        &self,
        format: BitmapFormat,
        bytes: &[u8],
    ) -> Result<Vec<u8>, BitmapError> {
        let width = self.width();
        let height = self.height();
        let levels = self.mipmap_levels();
        let layers = self.layer_count();
        // Each face's mip chain is the surface-bytes count for the
        // source format; the decoded chain (RGBA8) has the same mip
        // count and dimensions but a different per-pixel byte count.
        let face_src_bytes = format.surface_bytes(width, height, levels) as usize;
        let mut out: Vec<u8> = Vec::new();
        for face in 0..layers as usize {
            let face_start = face * face_src_bytes;
            let mut cursor = face_start;
            for level in 0..levels {
                let w = (width >> level).max(1);
                let h = (height >> level).max(1);
                let level_size = format.level_bytes(w, h) as usize;
                if cursor + level_size > bytes.len() {
                    return Err(BitmapError::PixelSliceOutOfBounds {
                        offset: cursor as u64,
                        size: level_size as u64,
                        available: bytes.len() as u64,
                    });
                }
                let decoded =
                    decode::decode_to_rgba8(format, w, h, &bytes[cursor..cursor + level_size], self.p8_palette)?;
                out.extend_from_slice(&decoded);
                cursor += level_size;
            }
        }
        Ok(out)
    }

    /// Write this image as a Tool-importable RGBA8 TIFF (Phase 2:
    /// 2D textures only, mip 0). Decodes the source format to
    /// straight-RGBA8 first; HDR formats are clamped to `[0, 1]`,
    /// signed-normalmap formats are biased into the unsigned range.
    /// Cube / array / 3D bitmaps and BC-compressed formats return
    /// the appropriate "deferred" / "unsupported" error.
    pub fn write_tiff(&self, out: &mut impl Write) -> Result<(), BitmapError> {
        tiff::write_image_tiff(self, out)
    }

    fn write_dds_with_format(
        &self,
        out: &mut impl Write,
        format: BitmapFormat,
        bytes: &[u8],
        is_cube: bool,
        is_array: bool,
    ) -> Result<(), BitmapError> {
        let needs_dx10 = is_array || format.requires_dxt10();
        if needs_dx10 {
            // Cube + DXT10 uses miscFlag bit 2 + arraySize=6 — no
            // observed corpus images need it, so we punt for now.
            if is_cube {
                return Err(BitmapError::UnsupportedTextureType(format!(
                    "cube map of `{:?}` requires DXT10 cube + array support", format
                )));
            }
            let layers = if is_array { self.layer_count() } else { 1 };
            write_dds_dx10(out, format, self.width(), self.height(), self.mipmap_levels(), layers, bytes)?;
        } else {
            write_dds(out, format, self.width(), self.height(), self.mipmap_levels(), is_cube, bytes)?;
        }
        Ok(())
    }
}
