//! Display-oriented decoding for cooked Unreal `Texture2D` exports.
//!
//! The native texture serializer owns mip addressing and pixel-format names;
//! this layer turns one available mip into RGBA8 for tools that need a preview.
//! It never replaces the stored compressed bytes, so package round trips remain
//! lossless.

use anyhow::{Context, Result, bail};

use crate::iostore::object::tail_models::{
    TextureChainTail, VirtualTextureBuiltData, VirtualTextureTileOffsetData,
};

/// One decoded mip suitable for direct upload to a UI texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture2dPreview {
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub mip_level: usize,
    pub rgba8: Vec<u8>,
}

/// Decode the largest available, supported mip in a cooked texture.
///
/// Inline mips are read directly from the model. For a streaming mip,
/// `external_payload` receives its package bulk-data-map index and must return
/// that entry's exact payload bytes.
pub fn decode_texture2d_preview(
    texture: &TextureChainTail,
    mut external_payload: impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<Texture2dPreview> {
    let mut last_error = None;
    for format in &texture.cooked.formats {
        for (mip_level, mip) in format.mips.iter().enumerate() {
            let width = u32::try_from(mip.size_x).ok().filter(|value| *value != 0);
            let height = u32::try_from(mip.size_y).ok().filter(|value| *value != 0);
            let (Some(width), Some(height)) = (width, height) else {
                continue;
            };
            let payload = match &mip.payload {
                Some(payload) => payload.clone(),
                None => match external_payload(mip.bulk_index) {
                    Ok(payload) => payload,
                    Err(error) => {
                        last_error = Some(error.context(format!(
                            "mip {mip_level} bulk-data entry {}",
                            mip.bulk_index
                        )));
                        continue;
                    }
                },
            };
            match decode_pixel_format(format.pixel_format.as_str(), width, height, &payload) {
                Ok(rgba8) => {
                    return Ok(Texture2dPreview {
                        width,
                        height,
                        pixel_format: format.pixel_format.to_string(),
                        mip_level,
                        rgba8,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        if let Some(virtual_data) = &format.virtual_data {
            match decode_virtual_texture_preview(virtual_data, &mut external_payload) {
                Ok((width, height, pixel_format, mip_level, rgba8)) => {
                    return Ok(Texture2dPreview {
                        width,
                        height,
                        pixel_format,
                        mip_level,
                        rgba8,
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("texture has no displayable cooked mip")))
}

fn decode_virtual_texture_preview(
    texture: &VirtualTextureBuiltData,
    external_payload: &mut impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<(u32, u32, String, usize, Vec<u8>)> {
    if !texture.cooked || texture.tile_size == 0 {
        bail!("virtual texture is not initialized");
    }
    if texture.num_layers == 0 {
        bail!("virtual texture has no layers");
    }
    let pixel_format = texture
        .layer_types
        .first()
        .context("virtual texture is missing its first layer format")?
        .to_string();
    let packed_tile_stride = usize::try_from(
        *texture
            .tile_data_offset_per_layer
            .last()
            .context("virtual texture is missing its layer sizes")?,
    )?;
    let physical_tile_size = texture
        .tile_size
        .checked_add(
            texture
                .tile_border_size
                .checked_mul(2)
                .context("virtual texture tile border overflows")?,
        )
        .context("virtual texture physical tile size overflows")?;

    let mut last_error = None;
    for mip_level in 0..texture.num_mips as usize {
        let Some(offsets) = texture.tile_offset_data.get(mip_level) else {
            continue;
        };
        let width = texture
            .width
            .checked_shr(mip_level as u32)
            .unwrap_or(0)
            .max(1);
        let height = texture
            .height
            .checked_shr(mip_level as u32)
            .unwrap_or(0)
            .max(1);
        let result = (|| {
            let chunk_index = *texture
                .chunk_index_per_mip
                .get(mip_level)
                .context("virtual texture mip has no chunk index")?
                as usize;
            let chunk = texture
                .chunks
                .get(chunk_index)
                .context("virtual texture mip chunk index is out of range")?;
            let codec = chunk
                .codecs
                .first()
                .map(|(codec, _)| *codec)
                .context("virtual texture chunk has no first-layer codec")?;
            let chunk_data = match &chunk.payload {
                Some(payload) => payload.clone(),
                None => external_payload(chunk.bulk_index)
                    .with_context(|| format!("virtual texture chunk {chunk_index}"))?,
            };
            decode_virtual_texture_mip(
                texture,
                offsets,
                mip_level,
                width,
                height,
                physical_tile_size,
                packed_tile_stride,
                codec,
                &pixel_format,
                &chunk_data,
            )
        })();
        match result {
            Ok(rgba8) => return Ok((width, height, pixel_format, mip_level, rgba8)),
            Err(error) => {
                last_error = Some(error.context(format!("virtual texture mip {mip_level}")))
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("virtual texture has no displayable mip")))
}

#[allow(clippy::too_many_arguments)]
fn decode_virtual_texture_mip(
    texture: &VirtualTextureBuiltData,
    offsets: &VirtualTextureTileOffsetData,
    mip_level: usize,
    width: u32,
    height: u32,
    physical_tile_size: u32,
    packed_tile_size: usize,
    codec: u8,
    pixel_format: &str,
    chunk: &[u8],
) -> Result<Vec<u8>> {
    // EVirtualTextureCodec::RawGPU. The payload is already in the layer's GPU
    // pixel format; only the virtual tile layout and duplicated borders need
    // to be removed for a conventional image preview.
    if codec != 4 {
        bail!("Unreal virtual texture codec {codec} is not supported for preview");
    }
    let base_offset = usize::try_from(
        *texture
            .base_offset_per_mip
            .get(mip_level)
            .context("virtual texture mip has no base offset")?,
    )?;
    let tile_size = usize::try_from(texture.tile_size)?;
    let border = usize::try_from(texture.tile_border_size)?;
    let physical = usize::try_from(physical_tile_size)?;
    let width_usize = usize::try_from(width)?;
    let height_usize = usize::try_from(height)?;
    let mut out = vec![0; checked_surface_len(width, height, 4)?];
    let mut decoded_tiles = 0usize;

    for address in 0..offsets.max_address {
        let tile_x = reverse_morton_code_2(address);
        let tile_y = reverse_morton_code_2(address >> 1);
        if tile_x >= offsets.width || tile_y >= offsets.height {
            continue;
        }
        let Some(tile_offset) = virtual_tile_offset(offsets, address) else {
            continue;
        };
        let start = base_offset
            .checked_add(
                usize::try_from(tile_offset)?
                    .checked_mul(packed_tile_size)
                    .context("virtual texture tile offset overflows")?,
            )
            .context("virtual texture tile start overflows")?;
        let packed = chunk
            .get(start..start.saturating_add(packed_tile_size))
            .context("virtual texture tile lies outside its bulk chunk")?;
        let tile =
            decode_pixel_format(pixel_format, physical_tile_size, physical_tile_size, packed)?;
        let destination_x = usize::try_from(tile_x)? * tile_size;
        let destination_y = usize::try_from(tile_y)? * tile_size;
        let copy_width = tile_size.min(width_usize.saturating_sub(destination_x));
        let copy_height = tile_size.min(height_usize.saturating_sub(destination_y));
        for row in 0..copy_height {
            let source = ((row + border) * physical + border) * 4;
            let destination = ((destination_y + row) * width_usize + destination_x) * 4;
            out[destination..destination + copy_width * 4]
                .copy_from_slice(&tile[source..source + copy_width * 4]);
        }
        decoded_tiles += 1;
    }
    if decoded_tiles == 0 {
        bail!("virtual texture mip has no populated tiles");
    }
    Ok(out)
}

fn virtual_tile_offset(offsets: &VirtualTextureTileOffsetData, address: u32) -> Option<u32> {
    let block = offsets.addresses.partition_point(|start| *start <= address);
    let block = block.checked_sub(1)?;
    let base = *offsets.offsets.get(block)?;
    if base == u32::MAX {
        return None;
    }
    address
        .checked_sub(offsets.addresses[block])
        .and_then(|local| base.checked_add(local))
}

fn reverse_morton_code_2(mut value: u32) -> u32 {
    value &= 0x5555_5555;
    value = (value ^ (value >> 1)) & 0x3333_3333;
    value = (value ^ (value >> 2)) & 0x0f0f_0f0f;
    value = (value ^ (value >> 4)) & 0x00ff_00ff;
    value = (value ^ (value >> 8)) & 0x0000_ffff;
    value
}

fn decode_pixel_format(format: &str, width: u32, height: u32, input: &[u8]) -> Result<Vec<u8>> {
    let normalized = format.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "PF_DXT1" | "PF_BC1" => decode_blocks(width, height, input, 8, |block, out| {
            bcdec_rs::bc1(block, out, 16)
        }),
        "PF_DXT3" | "PF_BC2" => decode_blocks(width, height, input, 16, |block, out| {
            bcdec_rs::bc2(block, out, 16)
        }),
        "PF_DXT5" | "PF_BC3" => decode_blocks(width, height, input, 16, |block, out| {
            bcdec_rs::bc3(block, out, 16)
        }),
        "PF_BC4" | "PF_BC4_SNORM" => decode_single_channel_blocks(width, height, input, false),
        "PF_BC5" | "PF_BC5_SNORM" => decode_bc5(width, height, input),
        "PF_BC7" => decode_blocks(width, height, input, 16, |block, out| {
            bcdec_rs::bc7(block, out, 16)
        }),
        "PF_B8G8R8A8" => decode_uncompressed(width, height, input, 4, |pixel| {
            [pixel[2], pixel[1], pixel[0], pixel[3]]
        }),
        "PF_R8G8B8A8" => decode_uncompressed(width, height, input, 4, |pixel| {
            [pixel[0], pixel[1], pixel[2], pixel[3]]
        }),
        "PF_G8" | "PF_R8" => decode_uncompressed(width, height, input, 1, |pixel| {
            [pixel[0], pixel[0], pixel[0], 255]
        }),
        "PF_A8" => decode_uncompressed(width, height, input, 1, |pixel| [255, 255, 255, pixel[0]]),
        _ => bail!("Unreal pixel format {format} is not supported for preview"),
    }
}

fn checked_surface_len(width: u32, height: u32, channels: usize) -> Result<usize> {
    let pixels = usize::try_from(width)?
        .checked_mul(usize::try_from(height)?)
        .context("texture dimensions overflow")?;
    pixels
        .checked_mul(channels)
        .context("texture byte length overflows")
}

fn decode_uncompressed(
    width: u32,
    height: u32,
    input: &[u8],
    stride: usize,
    convert: impl Fn(&[u8]) -> [u8; 4],
) -> Result<Vec<u8>> {
    let needed = checked_surface_len(width, height, stride)?;
    if input.len() < needed {
        bail!(
            "texture mip is truncated: need {needed} bytes, found {}",
            input.len()
        );
    }
    let mut out = Vec::with_capacity(checked_surface_len(width, height, 4)?);
    for pixel in input[..needed].chunks_exact(stride) {
        out.extend_from_slice(&convert(pixel));
    }
    Ok(out)
}

fn decode_blocks(
    width: u32,
    height: u32,
    input: &[u8],
    block_bytes: usize,
    decode: impl Fn(&[u8], &mut [u8]),
) -> Result<Vec<u8>> {
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let needed = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(block_bytes))
        .context("compressed texture byte length overflows")?;
    if input.len() < needed {
        bail!(
            "compressed texture mip is truncated: need {needed} bytes, found {}",
            input.len()
        );
    }
    let mut out = vec![0; checked_surface_len(width, height, 4)?];
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut decoded = [0u8; 64];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let block = block_y * blocks_x + block_x;
            let start = block * block_bytes;
            decoded.fill(0);
            decode(&input[start..start + block_bytes], &mut decoded);
            for local_y in 0..4 {
                let y = block_y * 4 + local_y;
                if y >= height_usize {
                    continue;
                }
                for local_x in 0..4 {
                    let x = block_x * 4 + local_x;
                    if x >= width_usize {
                        continue;
                    }
                    let source = (local_y * 4 + local_x) * 4;
                    let target = (y * width_usize + x) * 4;
                    out[target..target + 4].copy_from_slice(&decoded[source..source + 4]);
                }
            }
        }
    }
    Ok(out)
}

fn decode_single_channel_blocks(
    width: u32,
    height: u32,
    input: &[u8],
    signed: bool,
) -> Result<Vec<u8>> {
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let needed = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(8))
        .context("BC4 texture byte length overflows")?;
    if input.len() < needed {
        bail!(
            "BC4 texture mip is truncated: need {needed} bytes, found {}",
            input.len()
        );
    }
    let mut out = vec![0; checked_surface_len(width, height, 4)?];
    let mut decoded = [0u8; 16];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let block = block_y * blocks_x + block_x;
            decoded.fill(0);
            bcdec_rs::bc4(&input[block * 8..block * 8 + 8], &mut decoded, 4, signed);
            copy_single_channel_block(width, height, block_x, block_y, &decoded, &mut out);
        }
    }
    Ok(out)
}

fn copy_single_channel_block(
    width: u32,
    height: u32,
    block_x: usize,
    block_y: usize,
    decoded: &[u8; 16],
    out: &mut [u8],
) {
    let width = width as usize;
    let height = height as usize;
    for local_y in 0..4 {
        let y = block_y * 4 + local_y;
        if y >= height {
            continue;
        }
        for local_x in 0..4 {
            let x = block_x * 4 + local_x;
            if x >= width {
                continue;
            }
            let value = decoded[local_y * 4 + local_x];
            let target = (y * width + x) * 4;
            out[target..target + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
}

fn decode_bc5(width: u32, height: u32, input: &[u8]) -> Result<Vec<u8>> {
    let blocks_x = width.div_ceil(4) as usize;
    let blocks_y = height.div_ceil(4) as usize;
    let needed = blocks_x
        .checked_mul(blocks_y)
        .and_then(|blocks| blocks.checked_mul(16))
        .context("BC5 texture byte length overflows")?;
    if input.len() < needed {
        bail!(
            "BC5 texture mip is truncated: need {needed} bytes, found {}",
            input.len()
        );
    }
    let mut out = vec![0; checked_surface_len(width, height, 4)?];
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut decoded = [0u8; 32];
    for block_y in 0..blocks_y {
        for block_x in 0..blocks_x {
            let block = block_y * blocks_x + block_x;
            decoded.fill(0);
            bcdec_rs::bc5(&input[block * 16..block * 16 + 16], &mut decoded, 8, false);
            for local_y in 0..4 {
                let y = block_y * 4 + local_y;
                if y >= height_usize {
                    continue;
                }
                for local_x in 0..4 {
                    let x = block_x * 4 + local_x;
                    if x >= width_usize {
                        continue;
                    }
                    let source = (local_y * 4 + local_x) * 2;
                    let red = decoded[source];
                    let green = decoded[source + 1];
                    let nx = red as f32 / 127.5 - 1.0;
                    let ny = green as f32 / 127.5 - 1.0;
                    let blue = ((1.0 - nx * nx - ny * ny).max(0.0).sqrt() * 0.5 + 0.5) * 255.0;
                    let target = (y * width_usize + x) * 4;
                    out[target..target + 4].copy_from_slice(&[red, green, blue.round() as u8, 255]);
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use crate::iostore::object::archive::ExportContext;
    use crate::iostore::object::export::read_export_in;
    use crate::iostore::object::tail_models::{
        TailContext, TextureCookedData, TextureMip, TexturePlatformData, parse_texture_chain_tail,
    };
    use crate::iostore::object::ue_struct::StripDataFlags;
    use crate::iostore::object::value::{FName, FStr};
    use crate::iostore::package::builder::read_payloads;
    use crate::iostore::package::zen::FZenPackageHeader;
    use crate::iostore::usmap::Usmap;
    use crate::iostore::world::{CE_HEADER_VERSION, CE_TOC_VERSION, World};

    fn texture(format: &str, width: i32, height: i32, payload: Vec<u8>) -> TextureChainTail {
        TextureChainTail {
            texture_strip_flags: StripDataFlags::default(),
            cooked: TextureCookedData {
                strip_flags: StripDataFlags::default(),
                cooked: true,
                serialize_mip_data: Some(true),
                formats: vec![TexturePlatformData {
                    format_name: FName::none(),
                    using_derived_data: false,
                    size_x: width,
                    size_y: height,
                    packed_data: 1,
                    pixel_format: FStr::new(format, false),
                    opt_data: None,
                    cpu_copy: None,
                    first_mip_to_serialize: 0,
                    mips: vec![TextureMip {
                        bulk_index: 0,
                        payload: Some(payload),
                        size_x: width,
                        size_y: height,
                        size_z: 1,
                    }],
                    virtual_data: None,
                }],
                terminator: FName::none(),
            },
        }
    }

    #[test]
    fn bgra_texture_decodes_to_rgba() {
        let preview =
            decode_texture2d_preview(&texture("PF_B8G8R8A8", 1, 1, vec![10, 20, 30, 40]), |_| {
                bail!("unexpected external mip")
            })
            .unwrap();
        assert_eq!(preview.rgba8, [30, 20, 10, 40]);
    }

    #[test]
    fn bc1_texture_decodes_and_crops_partial_blocks() {
        let preview = decode_texture2d_preview(
            &texture("PF_DXT1", 1, 1, vec![0, 248, 0, 248, 0, 0, 0, 0]),
            |_| bail!("unexpected external mip"),
        )
        .unwrap();
        assert_eq!(preview.rgba8, [255, 0, 0, 255]);
    }

    #[test]
    fn raw_gpu_virtual_texture_tiles_are_assembled_without_borders() {
        let texture = VirtualTextureBuiltData {
            cooked: true,
            num_layers: 1,
            width_in_blocks: 1,
            height_in_blocks: 1,
            tile_size: 4,
            tile_border_size: 0,
            tile_data_offset_per_layer: vec![8],
            num_mips: 1,
            width: 8,
            height: 4,
            chunk_index_per_mip: vec![0],
            base_offset_per_mip: vec![4],
            tile_offset_data: Vec::new(),
            tile_index_per_chunk: Vec::new(),
            tile_index_per_mip: Vec::new(),
            tile_offset_in_chunk: Vec::new(),
            layer_types: vec![FStr::new("PF_DXT1", false)],
            layer_fallback_colors: vec![[0.0, 0.0, 0.0, 1.0]],
            chunks: Vec::new(),
        };
        let offsets = VirtualTextureTileOffsetData {
            width: 2,
            height: 1,
            max_address: 4,
            addresses: vec![0, 2],
            offsets: vec![0, u32::MAX],
        };
        let mut chunk = vec![0; 4];
        chunk.extend_from_slice(&[0, 248, 0, 248, 0, 0, 0, 0]);
        chunk.extend_from_slice(&[224, 7, 224, 7, 0, 0, 0, 0]);

        let decoded =
            decode_virtual_texture_mip(&texture, &offsets, 0, 8, 4, 4, 8, 4, "PF_DXT1", &chunk)
                .unwrap();
        assert_eq!(&decoded[0..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded[4 * 4..5 * 4], &[0, 255, 0, 255]);
    }

    /// Opens real shipped packages, follows streaming bulk-data references and
    /// proves at least one cooked Texture2D reaches RGBA pixels.
    ///
    /// `CE_PAKS=D:\...\Meteorite\Content\Paks cargo test -p blam-tags
    /// --features iostore campaign_evolved_textures_decode -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn campaign_evolved_textures_decode() {
        let root = std::env::var("CE_PAKS").expect("set CE_PAKS");
        let target = std::env::var("CE_TEXTURE_PACKAGE").ok();
        let world = World::open(root, Usmap::meteorite().expect("bundled usmap"))
            .expect("open Campaign Evolved");
        let mut texture_exports = 0usize;
        let mut decoded = 0usize;
        let mut failures = Vec::new();

        'packages: for package in world.packages() {
            if target.as_deref().is_some_and(|target| {
                !package
                    .name
                    .to_ascii_lowercase()
                    .contains(&target.to_ascii_lowercase())
            }) {
                continue;
            }
            let Some(provider) = package.active_provider() else {
                continue;
            };
            let archive = &world.archives()[provider.container];
            let Ok(prefix) = archive.read_prefix(&provider.entry_path, 4 * 1024 * 1024) else {
                continue;
            };
            let Ok(prefix_header) = FZenPackageHeader::deserialize(
                &mut Cursor::new(&prefix),
                None,
                CE_TOC_VERSION,
                CE_HEADER_VERSION,
                None,
            ) else {
                continue;
            };
            if !prefix_header.export_map.iter().any(|export| {
                world
                    .class_key(&prefix_header, export.class_index)
                    .as_deref()
                    == Some("Texture2D")
            }) {
                continue;
            }

            let bytes = world.read_provider(provider).expect("read texture package");
            let header = FZenPackageHeader::deserialize(
                &mut Cursor::new(&bytes),
                None,
                CE_TOC_VERSION,
                CE_HEADER_VERSION,
                None,
            )
            .expect("parse texture package");
            let payloads = read_payloads(&header, &bytes).expect("split exports");
            let names = header.name_map.copy_raw_names();
            let resolver = world.resolver(&header, &bytes, &names);
            let bulk_map: Vec<(i64, i64)> = header
                .bulk_data
                .iter()
                .map(|entry| (entry.serial_offset, entry.serial_size))
                .collect();
            let export_context = ExportContext {
                bulk_data: &bulk_map,
                resolver: Some(&resolver),
            };
            let package_chunk = archive
                .chunk_index_for(&provider.entry_path)
                .expect("package chunk");

            for ((entry, payload), export_index) in
                header.export_map.iter().zip(&payloads).zip(0usize..)
            {
                if world.class_key(&header, entry.class_index).as_deref() != Some("Texture2D") {
                    continue;
                }
                texture_exports += 1;
                let export = read_export_in(
                    payload,
                    &names,
                    world.usmap(),
                    "Texture2D",
                    entry.object_flags,
                    &export_context,
                )
                .expect("decode Texture2D export");
                let tail_context = TailContext {
                    bulk_data: &bulk_map,
                    origin: payload.len() - export.tail.len(),
                    usmap: world.usmap(),
                    resolver: Some(&resolver),
                    object_flags: entry.object_flags,
                };
                let texture = parse_texture_chain_tail(&export.tail, &names, tail_context, true)
                    .expect("parse Texture2D tail");
                let result = decode_texture2d_preview(&texture, |bulk_index| {
                    let bulk_entry = header
                        .bulk_data
                        .get(bulk_index.max(0) as usize)
                        .context("bulk-data index out of range")?;
                    let chunk = archive
                        .read_bulk_for(package_chunk, bulk_entry.cooked_index as u16)
                        .context("read sibling bulk chunk")?;
                    let start = usize::try_from(bulk_entry.serial_offset)
                        .context("negative bulk-data offset")?;
                    let size = usize::try_from(bulk_entry.serial_size)
                        .context("negative bulk-data size")?;
                    chunk
                        .get(start..start.saturating_add(size))
                        .map(ToOwned::to_owned)
                        .context("bulk-data entry lies outside sibling chunk")
                });
                match result {
                    Ok(preview) => {
                        println!(
                            "{} export {}: {} {}x{} mip {}",
                            package.name,
                            export_index,
                            preview.pixel_format,
                            preview.width,
                            preview.height,
                            preview.mip_level
                        );
                        decoded += 1;
                    }
                    Err(error) => failures.push(format!("{}: {error:#}", package.name)),
                }
                if texture_exports >= 24 {
                    break 'packages;
                }
            }
        }

        for failure in failures.iter().take(10) {
            println!("SKIP {failure}");
        }
        assert!(texture_exports > 0, "no Texture2D exports found");
        assert!(
            decoded > 0,
            "none of {texture_exports} Texture2D exports decoded"
        );
    }
}
