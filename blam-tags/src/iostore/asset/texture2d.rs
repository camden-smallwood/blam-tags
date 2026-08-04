//! Display-oriented decoding for cooked Unreal `Texture2D` exports.
//!
//! The native texture serializer owns mip addressing and pixel-format names;
//! this layer turns a cooked texture into per-layer, per-mip surfaces that stay
//! in their stored pixel format, plus an RGBA8 conversion for previews. It never
//! replaces the stored compressed bytes, so package round trips remain lossless.

use anyhow::{Context, Result, bail};

use crate::iostore::object::tail_models::{
    TextureChainTail, VirtualTextureBuiltData, VirtualTextureTileOffsetData,
};

/// `EVirtualTextureCodec::RawGPU` — tile payloads are already in the layer's GPU
/// pixel format and only need the virtual tile layout and borders removed.
const VT_CODEC_RAW_GPU: u8 = 4;
/// The "no data here" sentinel UE writes into every virtual-texture offset array.
const VT_NO_OFFSET: u32 = u32::MAX;

/// One mip of one layer, in that layer's stored pixel format.
///
/// `data` carries the failure instead of the mip being dropped: a texture whose
/// mip 0 cannot be read must say so, because silently presenting mip 1 is
/// indistinguishable from a texture that simply has no larger mip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureSurface {
    pub width: u32,
    pub height: u32,
    /// The format of `data`, which is normally the layer's format but falls back
    /// to `PF_B8G8R8A8`-style RGBA8 when a virtual tile layout cannot be
    /// reassembled while still compressed.
    pub pixel_format: String,
    pub data: Result<Vec<u8>, String>,
}

impl TextureSurface {
    /// Expand this surface to straight RGBA8 for display.
    pub fn to_rgba8(&self) -> Result<Vec<u8>> {
        let data = match &self.data {
            Ok(data) => data,
            Err(error) => bail!("{error}"),
        };
        decode_pixel_format(&self.pixel_format, self.width, self.height, data)
    }
}

/// One virtual-texture layer, or the single layer a conventional texture has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextureLayerSurfaces {
    /// The layer's declared pixel format. Individual mips may differ when a
    /// fallback was needed — always read [`TextureSurface::pixel_format`].
    pub pixel_format: String,
    /// Mip 0 first, largest to smallest.
    pub mips: Vec<TextureSurface>,
}

/// Every surface a cooked `Texture2D` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture2dSurfaces {
    pub width: u32,
    pub height: u32,
    /// UDIM block grid. `1x1` for a conventional texture.
    pub width_in_blocks: u32,
    pub height_in_blocks: u32,
    pub is_virtual: bool,
    pub layers: Vec<TextureLayerSurfaces>,
}

impl Texture2dSurfaces {
    /// True when this texture is a UDIM set rather than one image.
    pub fn is_udim(&self) -> bool {
        self.width_in_blocks.saturating_mul(self.height_in_blocks) > 1
    }
}

/// One decoded mip suitable for direct upload to a UI texture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture2dPreview {
    pub width: u32,
    pub height: u32,
    pub pixel_format: String,
    pub mip_level: usize,
    pub rgba8: Vec<u8>,
}

/// Decode every layer and mip of a cooked texture, keeping stored pixel formats.
///
/// Inline mips are read directly from the model. For a streaming mip or virtual
/// chunk, `external_payload` receives its package bulk-data-map index and must
/// return that entry's exact payload bytes.
pub fn decode_texture2d_surfaces(
    texture: &TextureChainTail,
    mut external_payload: impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<Texture2dSurfaces> {
    let mut last_error = None;
    for format in &texture.cooked.formats {
        if let Some(virtual_data) = &format.virtual_data {
            match virtual_texture_surfaces(virtual_data, &mut external_payload) {
                Ok(surfaces) => return Ok(surfaces),
                Err(error) => last_error = Some(error),
            }
            continue;
        }
        if format.mips.is_empty() {
            continue;
        }
        let pixel_format = format.pixel_format.to_string();
        let mips = format
            .mips
            .iter()
            .map(|mip| {
                let width = mip.size_x.max(0) as u32;
                let height = mip.size_y.max(0) as u32;
                let data = read_stored_mip(mip, &pixel_format, width, height, &mut external_payload)
                    .map_err(|error| format!("{error:#}"));
                TextureSurface {
                    width,
                    height,
                    pixel_format: pixel_format.clone(),
                    data,
                }
            })
            .collect();
        return Ok(Texture2dSurfaces {
            width: format.size_x.max(0) as u32,
            height: format.size_y.max(0) as u32,
            width_in_blocks: 1,
            height_in_blocks: 1,
            is_virtual: false,
            layers: vec![TextureLayerSurfaces { pixel_format, mips }],
        });
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("texture has no cooked mips")))
}

fn read_stored_mip(
    mip: &crate::iostore::object::tail_models::TextureMip,
    pixel_format: &str,
    width: u32,
    height: u32,
    external_payload: &mut impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("mip has empty dimensions");
    }
    let payload = match &mip.payload {
        Some(payload) => payload.clone(),
        None => external_payload(mip.bulk_index)
            .with_context(|| format!("bulk-data entry {}", mip.bulk_index))?,
    };
    // Length is checked exactly, not as a lower bound. An over-long payload
    // means the wrong bytes were fetched, and block decoders happily turn those
    // into noise that looks like a decoding bug rather than a read bug.
    let expected = ue_surface_len(pixel_format, width, height)
        .with_context(|| format!("Unreal pixel format {pixel_format} has no known block size"))?;
    if payload.len() != expected {
        bail!(
            "mip is {} bytes but {pixel_format} at {width}x{height} needs {expected}",
            payload.len()
        );
    }
    Ok(payload)
}

/// Decode the largest available, supported mip in a cooked texture.
///
/// A thin view over [`decode_texture2d_surfaces`] for callers that only want one
/// image. Unlike the surface model this *does* fall back to a smaller mip, so
/// prefer the surface model when the caller can report the failure.
pub fn decode_texture2d_preview(
    texture: &TextureChainTail,
    external_payload: impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<Texture2dPreview> {
    let surfaces = decode_texture2d_surfaces(texture, external_payload)?;
    let layer = surfaces
        .layers
        .first()
        .context("texture has no layers")?;
    let mut last_error = None;
    for (mip_level, surface) in layer.mips.iter().enumerate() {
        match surface.to_rgba8() {
            Ok(rgba8) => {
                return Ok(Texture2dPreview {
                    width: surface.width,
                    height: surface.height,
                    pixel_format: surface.pixel_format.clone(),
                    mip_level,
                    rgba8,
                });
            }
            Err(error) => last_error = Some(error.context(format!("mip {mip_level}"))),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("texture has no displayable cooked mip")))
}

/// Reassemble every layer and mip of a cooked virtual texture.
fn virtual_texture_surfaces(
    texture: &VirtualTextureBuiltData,
    external_payload: &mut impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<Texture2dSurfaces> {
    if !texture.cooked || texture.tile_size == 0 {
        bail!("virtual texture is not initialized");
    }
    if texture.num_layers == 0 {
        bail!("virtual texture has no layers");
    }

    // Chunk payloads are fetched once and shared by every mip and layer that
    // lands in them. Mip 0 of a large texture is tens of megabytes, and the
    // per-tile loop would otherwise re-decompress the same chunk thousands of
    // times.
    let mut chunk_cache: Vec<Option<std::rc::Rc<Result<Vec<u8>, String>>>> =
        vec![None; texture.chunks.len()];

    let layers = (0..texture.num_layers as usize)
        .map(|layer| {
            let pixel_format = texture
                .layer_types
                .get(layer)
                .map(ToString::to_string)
                .unwrap_or_default();
            let mips = (0..texture.num_mips as usize)
                .map(|level| {
                    let width = (texture.width >> level.min(31)).max(1);
                    let height = (texture.height >> level.min(31)).max(1);
                    let data = virtual_texture_mip(
                        texture,
                        layer,
                        level,
                        width,
                        height,
                        &pixel_format,
                        &mut chunk_cache,
                        external_payload,
                    );
                    match data {
                        Ok((format, bytes)) => TextureSurface {
                            width,
                            height,
                            pixel_format: format,
                            data: Ok(bytes),
                        },
                        Err(error) => TextureSurface {
                            width,
                            height,
                            pixel_format: pixel_format.clone(),
                            data: Err(format!("{error:#}")),
                        },
                    }
                })
                .collect();
            TextureLayerSurfaces {
                pixel_format,
                mips,
            }
        })
        .collect();

    Ok(Texture2dSurfaces {
        width: texture.width,
        height: texture.height,
        width_in_blocks: texture.width_in_blocks.max(1),
        height_in_blocks: texture.height_in_blocks.max(1),
        is_virtual: true,
        layers,
    })
}

/// The tile grid and address bound for one virtual-texture mip.
struct VirtualMipGrid {
    width: u32,
    height: u32,
    max_address: u32,
}

fn virtual_mip_grid(texture: &VirtualTextureBuiltData, level: usize) -> Result<VirtualMipGrid> {
    if let Some(offsets) = texture.tile_offset_data.get(level) {
        return Ok(VirtualMipGrid {
            width: offsets.width,
            height: offsets.height,
            max_address: offsets.max_address,
        });
    }
    // Legacy builds store no per-mip grid, so derive it from the logical size.
    // The address bound comes from the mip's slot span, which counts one slot
    // per layer and so must be divided back down to addresses.
    let width = (texture.width >> level.min(31)).max(1).div_ceil(texture.tile_size);
    let height = (texture.height >> level.min(31)).max(1).div_ceil(texture.tile_size);
    let start = *texture
        .tile_index_per_mip
        .get(level)
        .context("virtual texture mip has no tile index")?;
    let end = *texture
        .tile_index_per_mip
        .get(level + 1)
        .context("virtual texture mip has no end tile index")?;
    let slots = end.saturating_sub(start);
    let max_address = slots / texture.num_layers.max(1);
    Ok(VirtualMipGrid {
        width,
        height,
        max_address,
    })
}

/// Resolve one `(level, address, layer)` to the chunk and byte offset holding it.
///
/// Returns `None` when the tile is absent — Morton addressing allocates a square
/// address space, so a non-square texture always has holes, and UE marks empty
/// runs with `~0u` in the modern layout and a zero-length span in the legacy one.
fn virtual_tile_location(
    texture: &VirtualTextureBuiltData,
    level: usize,
    address: u32,
    layer: usize,
) -> Result<Option<(usize, usize)>> {
    let num_layers = texture.num_layers.max(1) as usize;
    if texture.tile_offset_in_chunk.is_empty() {
        // Modern layout: one chunk per mip, tiles addressed by run lookup.
        let offsets = texture
            .tile_offset_data
            .get(level)
            .context("virtual texture mip has no tile offset data")?;
        let chunk_index = *texture
            .chunk_index_per_mip
            .get(level)
            .context("virtual texture mip has no chunk index")? as usize;
        let base_offset = *texture
            .base_offset_per_mip
            .get(level)
            .context("virtual texture mip has no base offset")?;
        if base_offset == VT_NO_OFFSET {
            return Ok(None);
        }
        let Some(tile_offset) = virtual_tile_offset(offsets, address) else {
            return Ok(None);
        };
        let stride = *texture
            .tile_data_offset_per_layer
            .last()
            .context("virtual texture is missing its layer sizes")?;
        let layer_offset = if layer == 0 {
            0
        } else {
            *texture
                .tile_data_offset_per_layer
                .get(layer - 1)
                .context("virtual texture layer has no tile offset")?
        };
        let offset = (tile_offset as u64)
            .checked_mul(stride as u64)
            .and_then(|value| value.checked_add(base_offset as u64))
            .and_then(|value| value.checked_add(layer_offset as u64))
            .context("virtual texture tile offset overflows")?;
        return Ok(Some((chunk_index, usize::try_from(offset)?)));
    }

    // Legacy layout: fencepost arrays of per-(address, layer) slots.
    let start = *texture
        .tile_index_per_mip
        .get(level)
        .context("virtual texture mip has no tile index")?;
    let end = *texture
        .tile_index_per_mip
        .get(level + 1)
        .context("virtual texture mip has no end tile index")?;
    let tile_index = (start as u64)
        .checked_add((address as u64).checked_mul(num_layers as u64).unwrap_or(u64::MAX))
        .context("virtual texture tile index overflows")?;
    if tile_index >= end as u64 {
        return Ok(None);
    }
    let tile_index = usize::try_from(tile_index)?;
    let chunk_index = texture
        .tile_index_per_chunk
        .partition_point(|first| (*first as usize) <= tile_index)
        .checked_sub(1)
        .context("virtual texture tile belongs to no chunk")?;
    let tile_offset = |index: usize| -> Option<u32> {
        let chunk_end = *texture.tile_index_per_chunk.get(chunk_index + 1)? as usize;
        if index >= chunk_end {
            return texture.chunks.get(chunk_index).map(|chunk| chunk.size_in_bytes);
        }
        texture.tile_offset_in_chunk.get(index).copied()
    };
    let tile_start = tile_offset(tile_index).context("virtual texture tile has no offset")?;
    let tile_end =
        tile_offset(tile_index + num_layers).context("virtual texture tile has no end offset")?;
    // A zero-length span is Morton padding, not a tile.
    if tile_start == tile_end {
        return Ok(None);
    }
    let offset =
        tile_offset(tile_index + layer).context("virtual texture layer has no tile offset")?;
    Ok(Some((chunk_index, usize::try_from(offset)?)))
}

/// Reassemble one mip of one layer, keeping the stored pixel format when the
/// tile geometry allows it.
#[allow(clippy::too_many_arguments)]
fn virtual_texture_mip(
    texture: &VirtualTextureBuiltData,
    layer: usize,
    level: usize,
    width: u32,
    height: u32,
    pixel_format: &str,
    chunk_cache: &mut [Option<std::rc::Rc<Result<Vec<u8>, String>>>],
    external_payload: &mut impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<(String, Vec<u8>)> {
    let grid = virtual_mip_grid(texture, level)?;
    let physical = texture
        .tile_size
        .checked_add(
            texture
                .tile_border_size
                .checked_mul(2)
                .context("virtual texture tile border overflows")?,
        )
        .context("virtual texture physical tile size overflows")?;

    let (block_x, block_y, block_bytes) = ue_format_info(pixel_format)
        .with_context(|| format!("Unreal pixel format {pixel_format} has no known block size"))?;
    let tile_payload_len = ue_surface_len(pixel_format, physical, physical)
        .context("virtual texture tile size overflows")?;

    // Cropping the duplicated border in block space keeps the surface
    // compressed, which is what a DDS export needs. It is only valid when the
    // tile and its border land on block boundaries, and when no tile is a
    // constant colour — a constant has no compressed expression. Otherwise fall
    // back to RGBA8 and say so through the returned format.
    let constant_codec_present = texture.chunks.iter().any(|chunk| {
        chunk
            .codecs
            .get(layer)
            .is_some_and(|(codec, _)| constant_codec_colour(*codec).is_some())
    });
    let block_aligned = !constant_codec_present
        && texture.tile_border_size % block_x == 0
        && texture.tile_border_size % block_y == 0
        && texture.tile_size % block_x == 0
        && texture.tile_size % block_y == 0;

    // The surface grid: blocks when compressed, pixels when not. Both paths
    // scatter tiles with the same arithmetic once the units agree.
    let (surface_w, surface_h, tile_step_x, tile_step_y, border_x, border_y, tile_row_stride) =
        if block_aligned {
            (
                width.div_ceil(block_x) as usize,
                height.div_ceil(block_y) as usize,
                (texture.tile_size / block_x) as usize,
                (texture.tile_size / block_y) as usize,
                (texture.tile_border_size / block_x) as usize,
                (texture.tile_border_size / block_y) as usize,
                physical.div_ceil(block_x) as usize,
            )
        } else {
            (
                width as usize,
                height as usize,
                texture.tile_size as usize,
                texture.tile_size as usize,
                texture.tile_border_size as usize,
                texture.tile_border_size as usize,
                physical as usize,
            )
        };
    let (out_format, out_stride, out_len) = if block_aligned {
        (
            pixel_format.to_owned(),
            block_bytes as usize,
            surface_w
                .checked_mul(surface_h)
                .and_then(|blocks| blocks.checked_mul(block_bytes as usize))
                .context("virtual texture surface overflows")?,
        )
    } else {
        (
            "PF_R8G8B8A8".to_owned(),
            4,
            checked_surface_len(width, height, 4)?,
        )
    };
    let mut out = vec![0u8; out_len];
    let mut placed = 0usize;
    let mut last_error = None;

    for address in 0..grid.max_address {
        let tile_x = reverse_morton_code_2(address);
        let tile_y = reverse_morton_code_2(address >> 1);
        if tile_x >= grid.width || tile_y >= grid.height {
            continue;
        }
        let Some((chunk_index, offset)) = virtual_tile_location(texture, level, address, layer)?
        else {
            continue;
        };
        let chunk = texture
            .chunks
            .get(chunk_index)
            .context("virtual texture mip chunk index is out of range")?;
        let codec = chunk
            .codecs
            .get(layer)
            .map(|(codec, _)| *codec)
            .context("virtual texture chunk has no codec for this layer")?;

        let destination_x = tile_x as usize * tile_step_x;
        let destination_y = tile_y as usize * tile_step_y;
        let copy_width = tile_step_x.min(surface_w.saturating_sub(destination_x));
        let copy_height = tile_step_y.min(surface_h.saturating_sub(destination_y));
        if copy_width == 0 || copy_height == 0 {
            continue;
        }

        // Constant codecs carry no payload at all; they describe the tile.
        // `block_aligned` is already false whenever one is present, so the
        // surface here is always RGBA8.
        if let Some(colour) = constant_codec_colour(codec) {
            for row in 0..copy_height {
                let start = ((destination_y + row) * surface_w + destination_x) * out_stride;
                for pixel in 0..copy_width {
                    let at = start + pixel * out_stride;
                    out[at..at + out_stride].copy_from_slice(&colour);
                }
            }
            placed += 1;
            continue;
        }
        if codec != VT_CODEC_RAW_GPU {
            bail!("Unreal virtual texture codec {codec} is not supported");
        }

        let chunk_payload =
            virtual_chunk_payload(texture, chunk_index, chunk_cache, external_payload)?;
        let payload = match chunk_payload.as_ref() {
            Ok(payload) => payload,
            Err(error) => {
                last_error = Some(anyhow::anyhow!("{error}"));
                continue;
            }
        };
        let Some(packed) = payload.get(offset..offset.saturating_add(tile_payload_len)) else {
            last_error = Some(anyhow::anyhow!(
                "tile at {offset} lies outside chunk {chunk_index} ({} bytes)",
                payload.len()
            ));
            continue;
        };

        // In the compressed path the tile is copied verbatim, block row by block
        // row. In the fallback it is expanded first and copied pixel row by
        // pixel row; the indices are identical once the stride agrees.
        let tile;
        let source_rows: &[u8] = if block_aligned {
            packed
        } else {
            tile = decode_pixel_format(pixel_format, physical, physical, packed)?;
            &tile
        };
        for row in 0..copy_height {
            let source = ((row + border_y) * tile_row_stride + border_x) * out_stride;
            let destination = ((destination_y + row) * surface_w + destination_x) * out_stride;
            out[destination..destination + copy_width * out_stride]
                .copy_from_slice(&source_rows[source..source + copy_width * out_stride]);
        }
        placed += 1;
    }

    if placed == 0 {
        return Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("virtual texture mip has no populated tiles")));
    }
    Ok((out_format, out))
}

/// The colour a constant `EVirtualTextureCodec` stands for, if it is one.
fn constant_codec_colour(codec: u8) -> Option<[u8; 4]> {
    Some(match codec {
        0 => [0, 0, 0, 0],       // Black
        1 => [0, 0, 0, 255],     // OpaqueBlack
        2 => [255, 255, 255, 255], // White
        3 => [128, 128, 255, 255], // Flat (a flat tangent-space normal)
        _ => return None,
    })
}

fn virtual_chunk_payload(
    texture: &VirtualTextureBuiltData,
    chunk_index: usize,
    chunk_cache: &mut [Option<std::rc::Rc<Result<Vec<u8>, String>>>],
    external_payload: &mut impl FnMut(i32) -> Result<Vec<u8>>,
) -> Result<std::rc::Rc<Result<Vec<u8>, String>>> {
    if let Some(cached) = chunk_cache.get(chunk_index).and_then(Clone::clone) {
        return Ok(cached);
    }
    let chunk = texture
        .chunks
        .get(chunk_index)
        .context("virtual texture chunk index is out of range")?;
    let payload = match &chunk.payload {
        Some(payload) => Ok(payload.clone()),
        None => external_payload(chunk.bulk_index)
            .with_context(|| format!("virtual texture chunk {chunk_index}"))
            .map_err(|error| format!("{error:#}")),
    };
    let payload = std::rc::Rc::new(payload);
    if let Some(slot) = chunk_cache.get_mut(chunk_index) {
        *slot = Some(payload.clone());
    }
    Ok(payload)
}

fn virtual_tile_offset(offsets: &VirtualTextureTileOffsetData, address: u32) -> Option<u32> {
    let block = offsets.addresses.partition_point(|start| *start <= address);
    let block = block.checked_sub(1)?;
    let base = *offsets.offsets.get(block)?;
    if base == VT_NO_OFFSET {
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
        // BC6H is HDR. Tone mapping is a viewer decision, so clamp to [0,1] the
        // same way the classic bitmap decoders do rather than invent a curve.
        "PF_BC6H" => decode_blocks(width, height, input, 16, |block, out| {
            let mut rgb = [0.0f32; 4 * 4 * 3];
            bcdec_rs::bc6h_float(block, &mut rgb, 4 * 3, false);
            for pixel in 0..16 {
                out[pixel * 4] = clamp_unit_to_u8(rgb[pixel * 3]);
                out[pixel * 4 + 1] = clamp_unit_to_u8(rgb[pixel * 3 + 1]);
                out[pixel * 4 + 2] = clamp_unit_to_u8(rgb[pixel * 3 + 2]);
                out[pixel * 4 + 3] = 255;
            }
        }),
        "PF_FLOATRGBA" => decode_uncompressed(width, height, input, 8, |pixel| {
            [
                clamp_unit_to_u8(half_to_f32(u16::from_le_bytes([pixel[0], pixel[1]]))),
                clamp_unit_to_u8(half_to_f32(u16::from_le_bytes([pixel[2], pixel[3]]))),
                clamp_unit_to_u8(half_to_f32(u16::from_le_bytes([pixel[4], pixel[5]]))),
                clamp_unit_to_u8(half_to_f32(u16::from_le_bytes([pixel[6], pixel[7]]))),
            ]
        }),
        "PF_A32B32G32R32F" => decode_uncompressed(width, height, input, 16, |pixel| {
            let channel = |at: usize| {
                clamp_unit_to_u8(f32::from_le_bytes([
                    pixel[at],
                    pixel[at + 1],
                    pixel[at + 2],
                    pixel[at + 3],
                ]))
            };
            [channel(0), channel(4), channel(8), channel(12)]
        }),
        "PF_R16F" | "PF_R16F_FILTER" => decode_uncompressed(width, height, input, 2, |pixel| {
            let value = clamp_unit_to_u8(half_to_f32(u16::from_le_bytes([pixel[0], pixel[1]])));
            [value, value, value, 255]
        }),
        "PF_G16" => decode_uncompressed(width, height, input, 2, |pixel| {
            let value = pixel[1];
            [value, value, value, 255]
        }),
        "PF_A16B16G16R16" | "PF_R16G16B16A16_UNORM" => {
            decode_uncompressed(width, height, input, 8, |pixel| {
                [pixel[1], pixel[3], pixel[5], pixel[7]]
            })
        }
        "PF_G16R16" => decode_uncompressed(width, height, input, 4, |pixel| {
            [pixel[1], pixel[3], 0, 255]
        }),
        "PF_A2B10G10R10" => decode_uncompressed(width, height, input, 4, |pixel| {
            let packed = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
            let ten = |shift: u32| (((packed >> shift) & 0x3ff) >> 2) as u8;
            let alpha = ((packed >> 30) & 0x3) as u8;
            [ten(0), ten(10), ten(20), alpha * 85]
        }),
        "PF_R5G6B5_UNORM" => decode_uncompressed(width, height, input, 2, |pixel| {
            let packed = u16::from_le_bytes([pixel[0], pixel[1]]);
            let red = ((packed >> 11) & 0x1f) as u8;
            let green = ((packed >> 5) & 0x3f) as u8;
            let blue = (packed & 0x1f) as u8;
            [red << 3 | red >> 2, green << 2 | green >> 4, blue << 3 | blue >> 2, 255]
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

/// Block geometry for an Unreal pixel format: `(block width, block height,
/// bytes per block)`. Uncompressed formats report a 1x1 block whose size is the
/// per-pixel stride, so callers can size every surface with one formula.
///
/// Returns `None` for a format this module cannot measure — which is also every
/// format it cannot decode, so a `None` here is the honest "unsupported" answer
/// rather than a guess that silently mis-sizes a mip.
pub fn ue_format_info(format: &str) -> Option<(u32, u32, u32)> {
    let normalized = format.trim().to_ascii_uppercase();
    Some(match normalized.as_str() {
        "PF_DXT1" | "PF_BC1" | "PF_BC4" | "PF_BC4_SNORM" => (4, 4, 8),
        "PF_DXT3" | "PF_BC2" | "PF_DXT5" | "PF_BC3" | "PF_BC5" | "PF_BC5_SNORM" | "PF_BC6H"
        | "PF_BC7" => (4, 4, 16),
        "PF_G8" | "PF_R8" | "PF_A8" | "PF_L8" => (1, 1, 1),
        "PF_G16" | "PF_R16F" | "PF_R16F_FILTER" | "PF_R16_UINT" | "PF_R16_SINT"
        | "PF_R5G6B5_UNORM" | "PF_B5G5R5A1_UNORM" => (1, 1, 2),
        "PF_B8G8R8A8" | "PF_R8G8B8A8" | "PF_A8R8G8B8" | "PF_R8G8B8A8_SNORM"
        | "PF_R8G8B8A8_UINT" | "PF_A2B10G10R10" | "PF_G16R16" | "PF_G16R16_SNORM"
        | "PF_R16G16_UINT" | "PF_R32_FLOAT" | "PF_R32_UINT" | "PF_R32_SINT" | "PF_FLOATR11G11B10"
        | "PF_FLOATRGB" => (1, 1, 4),
        "PF_FLOATRGBA" | "PF_A16B16G16R16" | "PF_R16G16B16A16_UNORM"
        | "PF_R16G16B16A16_SNORM" | "PF_R16G16B16A16_UINT" | "PF_G32R32F" => (1, 1, 8),
        "PF_A32B32G32R32F" => (1, 1, 16),
        _ => return None,
    })
}

/// Exact byte length of one `width` x `height` surface in `format`.
///
/// This is the length a stored mip must have. Accepting anything longer is how
/// a wrong-chunk read reaches a block decoder and comes out as noise, so every
/// caller compares against this rather than against a lower bound.
pub fn ue_surface_len(format: &str, width: u32, height: u32) -> Option<usize> {
    let (block_x, block_y, block_bytes) = ue_format_info(format)?;
    let blocks_x = width.div_ceil(block_x) as usize;
    let blocks_y = height.div_ceil(block_y) as usize;
    blocks_x
        .checked_mul(blocks_y)?
        .checked_mul(block_bytes as usize)
}

/// Clamp a linear float channel to `[0, 1]` and scale to 8 bits.
///
/// HDR formats have no single correct 8-bit answer; this matches what the
/// classic bitmap decoders do so the two viewers agree.
fn clamp_unit_to_u8(value: f32) -> u8 {
    let clamped = if value.is_nan() { 0.0 } else { value.clamp(0.0, 1.0) };
    (clamped * 255.0 + 0.5) as u8
}

/// IEEE 754 half → f32, including subnormals, infinity and NaN.
fn half_to_f32(half: u16) -> f32 {
    let sign = if (half >> 15) & 1 == 1 { -1.0f32 } else { 1.0 };
    let exponent = (half >> 10) & 0x1f;
    let mantissa = half & 0x3ff;
    match exponent {
        0 if mantissa == 0 => sign * 0.0,
        0 => sign * (mantissa as f32) * 2.0f32.powi(-24),
        0x1f if mantissa == 0 => sign * f32::INFINITY,
        0x1f => f32::NAN,
        _ => sign * (1.0 + (mantissa as f32) / 1024.0) * 2.0f32.powi(exponent as i32 - 15),
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
        TailContext, TextureCookedData, TextureMip, TexturePlatformData, VirtualTextureDataChunk,
        parse_texture_chain_tail,
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

        let mut texture = texture;
        texture.tile_offset_data = vec![offsets];
        texture.chunks = vec![VirtualTextureDataChunk {
            bulk_data_hash: Default::default(),
            size_in_bytes: chunk.len() as u32,
            codec_payload_size: 4,
            codecs: vec![(4, 4)],
            bulk_index: 0,
            payload: Some(chunk),
        }];

        let surfaces = virtual_texture_surfaces(&texture, &mut |_| bail!("no external chunk"))
            .expect("reassemble");
        assert!(!surfaces.is_udim());
        let surface = &surfaces.layers[0].mips[0];
        // The tiles stay in PF_DXT1, so the border crop happened in block space.
        assert_eq!(surface.pixel_format, "PF_DXT1");
        let decoded = surface.to_rgba8().expect("expand tiles");
        assert_eq!(&decoded[0..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded[4 * 4..5 * 4], &[0, 255, 0, 255]);
    }

    /// A virtual texture with 1x2 tiles allocates a 2x2 Morton address space, so
    /// two of the four addresses have no tile. Reading them would walk off the
    /// end of the chunk; the padding must be skipped by address, not by luck.
    #[test]
    fn morton_padding_addresses_are_skipped_on_non_square_tile_grids() {
        let mut texture = virtual_texture(1, 4, 4, 8, vec![8]);
        texture.tile_offset_data = vec![VirtualTextureTileOffsetData {
            width: 1,
            height: 2,
            max_address: 4,
            addresses: vec![0, 1, 2, 3],
            // Addresses 1 and 3 are the Morton holes of a 1-wide grid.
            offsets: vec![0, u32::MAX, 1, u32::MAX],
        }];
        let mut chunk = vec![0; 4];
        chunk.extend_from_slice(&[0, 248, 0, 248, 0, 0, 0, 0]);
        chunk.extend_from_slice(&[224, 7, 224, 7, 0, 0, 0, 0]);
        texture.chunks = vec![chunk_of(chunk)];

        let surfaces =
            virtual_texture_surfaces(&texture, &mut |_| bail!("no external chunk")).unwrap();
        let rgba = surfaces.layers[0].mips[0].to_rgba8().unwrap();
        // Tile (0,0) is red at the top, tile (0,1) green four rows down.
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&rgba[4 * 4 * 4..4 * 4 * 4 + 4], &[0, 255, 0, 255]);
    }

    /// Each layer must use its own format, its own codec, and its own slice of
    /// the shared per-tile stride. Decoding every layer as layer 0 was the old
    /// behaviour and silently dropped the second layer entirely.
    #[test]
    fn virtual_texture_layers_use_their_own_format_and_tile_slice() {
        // Layer 0 is one 8-byte DXT1 block, layer 1 one 16-byte DXT5 block.
        let mut texture = virtual_texture(2, 4, 4, 4, vec![8, 24]);
        texture.layer_types = vec![FStr::new("PF_DXT1", false), FStr::new("PF_DXT5", false)];
        texture.tile_offset_data = vec![VirtualTextureTileOffsetData {
            width: 1,
            height: 1,
            max_address: 1,
            addresses: vec![0],
            offsets: vec![0],
        }];
        let mut chunk = vec![0; 4];
        chunk.extend_from_slice(&[0, 248, 0, 248, 0, 0, 0, 0]); // layer 0: red
        chunk.extend_from_slice(&[255; 8]); // layer 1 alpha block: opaque
        chunk.extend_from_slice(&[224, 7, 224, 7, 0, 0, 0, 0]); // layer 1 colour: green
        let mut chunk = chunk_of(chunk);
        chunk.codecs = vec![(4, 4), (4, 4)];
        texture.chunks = vec![chunk];

        let surfaces =
            virtual_texture_surfaces(&texture, &mut |_| bail!("no external chunk")).unwrap();
        assert_eq!(surfaces.layers.len(), 2);
        assert_eq!(surfaces.layers[0].pixel_format, "PF_DXT1");
        assert_eq!(surfaces.layers[1].pixel_format, "PF_DXT5");
        assert_eq!(&surfaces.layers[0].mips[0].to_rgba8().unwrap()[0..4], &[255, 0, 0, 255]);
        assert_eq!(&surfaces.layers[1].mips[0].to_rgba8().unwrap()[0..4], &[0, 255, 0, 255]);
    }

    /// Codecs 0-3 describe a colour rather than carrying a payload. They used to
    /// abort the whole mip.
    #[test]
    fn constant_codec_tiles_fill_their_colour() {
        let mut texture = virtual_texture(1, 4, 4, 4, vec![8]);
        texture.tile_offset_data = vec![VirtualTextureTileOffsetData {
            width: 1,
            height: 1,
            max_address: 1,
            addresses: vec![0],
            offsets: vec![0],
        }];
        let mut chunk = chunk_of(vec![0; 64]);
        chunk.codecs = vec![(3, 0)]; // Flat
        texture.chunks = vec![chunk];

        let surfaces =
            virtual_texture_surfaces(&texture, &mut |_| bail!("no external chunk")).unwrap();
        let surface = &surfaces.layers[0].mips[0];
        // A constant has no compressed expression, so the surface drops to RGBA8.
        assert_eq!(surface.pixel_format, "PF_R8G8B8A8");
        assert_eq!(&surface.data.as_ref().unwrap()[0..4], &[128, 128, 255, 255]);
    }

    /// A mip whose stored payload is the wrong length must report that rather
    /// than decode whatever bytes it was handed.
    #[test]
    fn stored_mip_with_wrong_length_is_reported_not_decoded() {
        let surfaces = decode_texture2d_surfaces(
            &texture("PF_DXT1", 4, 4, vec![0; 32]),
            |_| bail!("unexpected external mip"),
        )
        .unwrap();
        let error = surfaces.layers[0].mips[0]
            .data
            .as_ref()
            .expect_err("32 bytes is not one DXT1 block");
        assert!(error.contains("needs 8"), "{error}");
    }

    #[test]
    fn udim_block_grid_is_reported() {
        let mut texture = virtual_texture(1, 4, 4, 4, vec![8]);
        texture.width_in_blocks = 7;
        texture.height_in_blocks = 2;
        texture.tile_offset_data = vec![VirtualTextureTileOffsetData {
            width: 1,
            height: 1,
            max_address: 1,
            addresses: vec![0],
            offsets: vec![0],
        }];
        let mut chunk = vec![0; 4];
        chunk.extend_from_slice(&[0, 248, 0, 248, 0, 0, 0, 0]);
        texture.chunks = vec![chunk_of(chunk)];

        let surfaces =
            virtual_texture_surfaces(&texture, &mut |_| bail!("no external chunk")).unwrap();
        assert!(surfaces.is_udim());
        assert_eq!((surfaces.width_in_blocks, surfaces.height_in_blocks), (7, 2));
    }

    fn virtual_texture(
        layers: u32,
        tile_size: u32,
        width: u32,
        height: u32,
        offsets_per_layer: Vec<u32>,
    ) -> VirtualTextureBuiltData {
        VirtualTextureBuiltData {
            cooked: true,
            num_layers: layers,
            width_in_blocks: 1,
            height_in_blocks: 1,
            tile_size,
            tile_border_size: 0,
            tile_data_offset_per_layer: offsets_per_layer,
            num_mips: 1,
            width,
            height,
            chunk_index_per_mip: vec![0],
            base_offset_per_mip: vec![4],
            tile_offset_data: Vec::new(),
            tile_index_per_chunk: Vec::new(),
            tile_index_per_mip: Vec::new(),
            tile_offset_in_chunk: Vec::new(),
            layer_types: vec![FStr::new("PF_DXT1", false); layers as usize],
            layer_fallback_colors: vec![[0.0, 0.0, 0.0, 1.0]; layers as usize],
            chunks: Vec::new(),
        }
    }

    fn chunk_of(payload: Vec<u8>) -> VirtualTextureDataChunk {
        VirtualTextureDataChunk {
            bulk_data_hash: Default::default(),
            size_in_bytes: payload.len() as u32,
            codec_payload_size: 4,
            codecs: vec![(4, 4)],
            bulk_index: 0,
            payload: Some(payload),
        }
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

    /// Diagnostic sweep over every shipped `Texture2D`.
    ///
    /// This reports rather than asserts: it exists to say *why* a texture does
    /// not reach mip 0, which the decoder itself cannot say because it silently
    /// falls through to a smaller mip. For each export it prints the platform
    /// data, every mip's stored length against the length its format requires,
    /// and the bulk-data flags behind any streaming mip.
    ///
    /// `CE_PAKS=D:\...\Meteorite\Content\Paks cargo test -p blam-tags
    /// --features iostore campaign_evolved_texture_survey -- --ignored --nocapture`
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn campaign_evolved_texture_survey() {
        let Ok(root) = std::env::var("CE_PAKS") else {
            eprintln!("skipping: set CE_PAKS to a Campaign Evolved Content/Paks folder");
            return;
        };
        let target = std::env::var("CE_TEXTURE_PACKAGE").ok();
        let limit = std::env::var("CE_TEXTURE_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(usize::MAX);
        let world = World::open(root, Usmap::meteorite().expect("bundled usmap"))
            .expect("open Campaign Evolved");

        let mut exports_seen = 0usize;
        // Tallies keyed by the short reason a mip could not be measured or read,
        // so the failure modes rank themselves instead of needing to be guessed.
        let mut mip0_ok = 0usize;
        let mut mip0_bad = 0usize;
        let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
        let mut formats: std::collections::BTreeMap<String, usize> = Default::default();
        let mut virtual_textures = 0usize;
        let mut codec_counts: std::collections::BTreeMap<u8, usize> = Default::default();
        let mut legacy_virtual = 0usize;
        let mut multi_layer = 0usize;
        let mut udim = 0usize;
        let note = |map: &mut std::collections::BTreeMap<String, usize>, key: String| {
            *map.entry(key).or_default() += 1;
        };
        fn root_cause(error: &anyhow::Error) -> String {
            error.chain().last().map(ToString::to_string).unwrap_or_default()
        }

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

            let Ok(bytes) = world.read_provider(provider) else {
                continue;
            };
            let Ok(header) = FZenPackageHeader::deserialize(
                &mut Cursor::new(&bytes),
                None,
                CE_TOC_VERSION,
                CE_HEADER_VERSION,
                None,
            ) else {
                continue;
            };
            let Ok(payloads) = read_payloads(&header, &bytes) else {
                continue;
            };
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
            let Ok(package_chunk) = archive.chunk_index_for(&provider.entry_path) else {
                continue;
            };

            for ((entry, payload), export_index) in
                header.export_map.iter().zip(&payloads).zip(0usize..)
            {
                if world.class_key(&header, entry.class_index).as_deref() != Some("Texture2D") {
                    continue;
                }
                let Ok(export) = read_export_in(
                    payload,
                    &names,
                    world.usmap(),
                    "Texture2D",
                    entry.object_flags,
                    &export_context,
                ) else {
                    note(&mut reasons, "export decode failed".to_owned());
                    continue;
                };
                let tail_context = TailContext {
                    bulk_data: &bulk_map,
                    origin: payload.len() - export.tail.len(),
                    usmap: world.usmap(),
                    resolver: Some(&resolver),
                    object_flags: entry.object_flags,
                };
                let texture =
                    match parse_texture_chain_tail(&export.tail, &names, tail_context, true) {
                        Ok(texture) => texture,
                        Err(error) => {
                            note(&mut reasons, format!("tail parse failed: {error}"));
                            continue;
                        }
                    };
                exports_seen += 1;

                for format in &texture.cooked.formats {
                    note(&mut formats, format.pixel_format.to_string());
                    println!(
                        "\n{} export {export_index}: {} {}x{} packed={:#x} first_mip={} mips={} opt={:?}",
                        package.name,
                        format.pixel_format,
                        format.size_x,
                        format.size_y,
                        format.packed_data,
                        format.first_mip_to_serialize,
                        format.mips.len(),
                        format.opt_data.as_ref().map(|opt| opt.num_mips_in_tail),
                    );

                    for (level, mip) in format.mips.iter().enumerate() {
                        let width = mip.size_x.max(0) as u32;
                        let height = mip.size_y.max(0) as u32;
                        let expected = ue_surface_len(format.pixel_format.as_str(), width, height);
                        let (source, actual, detail) = match &mip.payload {
                            Some(payload) => ("inline".to_owned(), Some(payload.len()), String::new()),
                            None => {
                                let bulk = header.bulk_data.get(mip.bulk_index.max(0) as usize);
                                let detail = bulk
                                    .map(|bulk| {
                                        format!(
                                            "flags={:#x} cooked_index={} offset={} size={}",
                                            bulk.flags,
                                            bulk.cooked_index,
                                            bulk.serial_offset,
                                            bulk.serial_size
                                        )
                                    })
                                    .unwrap_or_else(|| "no bulk entry".to_owned());
                                let actual = bulk.and_then(|bulk| {
                                    let chunk = archive
                                        .read_bulk_for(package_chunk, bulk.cooked_index as u16)
                                        .ok()?;
                                    let start = usize::try_from(bulk.serial_offset).ok()?;
                                    let size = usize::try_from(bulk.serial_size).ok()?;
                                    chunk.get(start..start.checked_add(size)?).map(<[u8]>::len)
                                });
                                (format!("bulk {}", mip.bulk_index), actual, detail)
                            }
                        };
                        let verdict = match (expected, actual) {
                            (Some(expected), Some(actual)) if expected == actual => "ok",
                            (Some(_), Some(_)) => "LENGTH MISMATCH",
                            (None, _) => "UNMEASURABLE FORMAT",
                            (_, None) => "PAYLOAD UNREADABLE",
                        };
                        println!(
                            "  mip {level}: {width}x{height} {source} actual={actual:?} expected={expected:?} {verdict} {detail}"
                        );
                        if level == 0 {
                            if verdict == "ok" {
                                mip0_ok += 1;
                            } else {
                                mip0_bad += 1;
                                note(
                                    &mut reasons,
                                    format!("{verdict} ({})", format.pixel_format),
                                );
                            }
                        }
                    }

                    let Some(vt) = &format.virtual_data else {
                        continue;
                    };
                    virtual_textures += 1;
                    if !vt.tile_offset_in_chunk.is_empty() {
                        legacy_virtual += 1;
                    }
                    if vt.num_layers > 1 {
                        multi_layer += 1;
                    }
                    if vt.width_in_blocks * vt.height_in_blocks > 1 {
                        udim += 1;
                    }
                    for chunk in &vt.chunks {
                        for (codec, _) in &chunk.codecs {
                            *codec_counts.entry(*codec).or_default() += 1;
                        }
                    }
                    println!(
                        "  VT: {}x{} blocks={}x{} tile={}+{} layers={} [{}] mips={} chunks={} legacy={} tile_offset_data={}",
                        vt.width,
                        vt.height,
                        vt.width_in_blocks,
                        vt.height_in_blocks,
                        vt.tile_size,
                        vt.tile_border_size,
                        vt.num_layers,
                        vt.layer_types
                            .iter()
                            .map(FStr::to_string)
                            .collect::<Vec<_>>()
                            .join(", "),
                        vt.num_mips,
                        vt.chunks.len(),
                        !vt.tile_offset_in_chunk.is_empty(),
                        vt.tile_offset_data.len(),
                    );
                    println!(
                        "    chunk_index_per_mip={:?} base_offset_per_mip={:?} tile_data_offset_per_layer={:?}",
                        vt.chunk_index_per_mip,
                        vt.base_offset_per_mip,
                        vt.tile_data_offset_per_layer,
                    );
                    for (index, chunk) in vt.chunks.iter().enumerate() {
                        println!(
                            "    chunk {index}: size_in_bytes={} codec_payload_size={} codecs={:?} bulk_index={} inline={}",
                            chunk.size_in_bytes,
                            chunk.codec_payload_size,
                            chunk.codecs,
                            chunk.bulk_index,
                            chunk.payload.is_some(),
                        );
                    }

                    // Reassemble every VT mip. The preview API stops at the first
                    // that works, which hides exactly the mip-0 failures this
                    // survey exists to find.
                    let surfaces = decode_texture2d_surfaces(&texture, |bulk_index| {
                        let bulk = header
                            .bulk_data
                            .get(bulk_index.max(0) as usize)
                            .context("bulk entry out of range")?;
                        let bytes =
                            archive.read_bulk_for(package_chunk, bulk.cooked_index as u16)?;
                        let start = usize::try_from(bulk.serial_offset)?;
                        let size = usize::try_from(bulk.serial_size)?;
                        bytes
                            .get(start..start.saturating_add(size))
                            .map(ToOwned::to_owned)
                            .context("bulk entry lies outside its chunk")
                    });
                    match surfaces {
                        Ok(surfaces) => {
                            for (layer_index, layer) in surfaces.layers.iter().enumerate() {
                                for (level, surface) in layer.mips.iter().enumerate() {
                                    let grid = vt
                                        .tile_offset_data
                                        .get(level)
                                        .map(|offsets| {
                                            format!(
                                                "grid={}x{} max_address={}",
                                                offsets.width, offsets.height, offsets.max_address
                                            )
                                        })
                                        .unwrap_or_else(|| "no grid".to_owned());
                                    match &surface.data {
                                        Ok(bytes) => println!(
                                            "    layer {layer_index} mip {level}: {}x{} {grid} ok ({} bytes {})",
                                            surface.width,
                                            surface.height,
                                            bytes.len(),
                                            surface.pixel_format,
                                        ),
                                        Err(error) => {
                                            println!(
                                                "    layer {layer_index} mip {level}: {}x{} {grid} FAILED {error}",
                                                surface.width, surface.height
                                            );
                                            if level == 0 {
                                                note(
                                                    &mut reasons,
                                                    format!("VT mip 0 failed: {error}"),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            println!("    VT reassembly FAILED {error:#}");
                            note(&mut reasons, format!("VT failed: {}", root_cause(&error)));
                        }
                    }
                }

                if exports_seen >= limit {
                    break 'packages;
                }
            }
        }

        println!("\n=== survey ===");
        println!("Texture2D exports: {exports_seen}");
        println!("mip 0 exact length: {mip0_ok}, not usable: {mip0_bad}");
        println!(
            "virtual textures: {virtual_textures} (legacy layout {legacy_virtual}, multi-layer {multi_layer}, UDIM {udim})"
        );
        println!("VT codecs: {codec_counts:?}");
        println!("pixel formats: {formats:?}");
        println!("reasons: {reasons:?}");
        assert!(exports_seen > 0, "no Texture2D exports found");
    }
}
