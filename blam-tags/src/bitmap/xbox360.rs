//! Xbox 360 (Xenos GPU) texture format handling.
//!
//! Halo 4's X360 monolithic builds store bitmap pixels in two
//! GPU-friendly transforms relative to PC:
//!
//! 1. **2D-tiled layout.** Texture data is stored in a 32×32-tile
//!    swizzled order so the GPU can fetch any (x, y) with locality.
//!    See [`xg_address_2d_tiled_offset`] for the formula and
//!    [`detile_blocks`] for the bulk conversion to linear order.
//! 2. **Big-endian byte order within compressed blocks.** Each
//!    16-bit half of a DXT/BC block lands in memory with its bytes
//!    reversed relative to PC. The fix-up is a pairwise byte swap
//!    over the entire detiled buffer — see [`swap_byte_pairs`].
//!
//! Ported from TagTool's `XboxGraphics.XGAddress2DTiledOffset` and
//! `XGEndianSwapSurface` (Xbox-360 SDK reference implementations).

/// Compute the BLOCK offset of `(x, y)` inside a tiled buffer.
/// `width_in_blocks` is the texture's per-row block count rounded
/// up to the nearest 32 (the tile width). `texel_pitch` is the byte
/// count of one block: 8 for BC1/BC4, 16 for BC2/BC3/BC5.
///
/// Returns the block index in the tiled source buffer. Multiply by
/// `texel_pitch` to get the byte offset.
pub fn xg_address_2d_tiled_offset(
    x: u32,
    y: u32,
    width_in_blocks: u32,
    texel_pitch: u32,
) -> u32 {
    let aligned_width = (width_in_blocks + 31) & !31;
    let log_bpp = xg_log2_le16(texel_pitch);

    let macro_part: u32 = ((x >> 5) + (y >> 5) * (aligned_width >> 5)) << (log_bpp + 7);
    let micro_part: u32 = ((x & 7) + ((y & 6) << 2)) << log_bpp;

    let offset = macro_part
        + ((micro_part & !15) << 1)
        + (micro_part & 15)
        + ((y & 8) << (3 + log_bpp))
        + ((y & 1) << 4);

    let tiled_byte_offset = ((offset & !511) << 3)
        + ((offset & 448) << 2)
        + (offset & 63)
        + ((y & 16) << 7)
        + (((((y & 8) >> 2) + (x >> 3)) & 3) << 6);

    tiled_byte_offset >> log_bpp
}

/// `log2` for inputs in `1..=16` — the texel pitch (bytes per
/// block) is always 1, 2, 4, 8, or 16 for the formats we handle.
fn xg_log2_le16(value: u32) -> u32 {
    debug_assert!(value > 0 && value <= 16, "xg_log2_le16: value out of range: {value}");
    value.trailing_zeros()
}

/// Convert a tiled compressed-block buffer into a linear one. Both
/// buffers hold `width_in_blocks * height_in_blocks` block records
/// of `texel_pitch` bytes each; the tiled buffer's bytes are laid
/// out in Xenos 32-block-tile swizzled order, the linear buffer in
/// plain row-major order.
///
/// The source buffer must be at least the size of one tile-aligned
/// surface — Xenos rounds each surface up to a 32×32-block multiple.
pub fn detile_blocks(
    tiled: &[u8],
    width_in_blocks: u32,
    height_in_blocks: u32,
    texel_pitch: u32,
) -> Vec<u8> {
    let pitch = texel_pitch as usize;
    let mut linear =
        vec![0u8; (width_in_blocks as usize) * (height_in_blocks as usize) * pitch];
    for y in 0..height_in_blocks {
        for x in 0..width_in_blocks {
            let src_block = xg_address_2d_tiled_offset(x, y, width_in_blocks, texel_pitch);
            let src_off = src_block as usize * pitch;
            let dst_off = ((y * width_in_blocks + x) as usize) * pitch;
            if src_off + pitch <= tiled.len() {
                linear[dst_off..dst_off + pitch]
                    .copy_from_slice(&tiled[src_off..src_off + pitch]);
            }
        }
    }
    linear
}

/// Swap every pair of bytes in `buf`. Halo 4 X360 stores DXT5 / BC3
/// blocks (and many other 16-bit-aligned formats) with each `u16`
/// field's bytes reversed compared to PC LE. The PC decoders
/// already in [`super::decode`] expect LE; pairwise swap fixes
/// every affected field at once.
///
/// This matches TagTool's `XGEndianSwapSurface` for the
/// `GPUENDIAN_8IN16` case, which is what DXT-family formats use.
pub fn swap_byte_pairs(buf: &mut [u8]) {
    for pair in buf.chunks_exact_mut(2) {
        pair.swap(0, 1);
    }
}

//================================================================================
// Surface layout
//
// Ported from TagTool's `XboxBitmapUtils` and `Direct3D/Xbox360/D3D`, which are
// themselves a reading of the Xbox 360 SDK's `XGSetTextureHeader` family. The
// arithmetic is reproduced rather than re-derived: it decides where a mip level
// starts inside a resource, and being one 4KB page out gives a texture that
// decodes to noise rather than to an error.
//================================================================================

/// How the GPU swapped a surface's bytes on its way to memory.
///
/// The 360 stores a texture in the byte order its sampler wants, which is the
/// reverse of the PC's within each component. The unit is the *component*, not
/// the pixel: a DXT block is a run of 16-bit words however wide its pixels are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndianSwap {
    /// Single-byte components. Nothing to do.
    None,
    /// Swap each pair of bytes. 16-bit components, and every block format.
    In16,
    /// Reverse each group of four. 32-bit components.
    In32,
}

impl EndianSwap {
    /// Apply in place.
    pub fn apply(self, buffer: &mut [u8]) {
        match self {
            Self::None => {}
            Self::In16 => {
                for pair in buffer.chunks_exact_mut(2) {
                    pair.swap(0, 1);
                }
            }
            Self::In32 => {
                for word in buffer.chunks_exact_mut(4) {
                    word.reverse();
                }
            }
        }
    }
}

/// Round `value` up to a multiple of `multiple`, which must be a power of two.
///
/// TagTool's `NextMultipleOf`, kept in its bit form because the callers below
/// lean on it wrapping the same way.
#[inline]
pub fn next_multiple_of(value: u32, multiple: u32) -> u32 {
    debug_assert!(multiple.is_power_of_two(), "multiple must be a power of two");
    !(multiple - 1) & value.wrapping_add(multiple - 1)
}

/// TagTool's `Log2Ceiling`, which is really a bit length.
///
/// It shifts left until the sign bit lands and returns `32 - shifts`, so 4 gives
/// 3, not 2. That is why its callers pass `x - 1`: `bit_length(x - 1)` is the
/// true ceiling of log2 for `x > 1`. Reproduced with the quirk intact, because
/// the offset arithmetic is built on it.
#[inline]
pub fn log2_ceiling(input: i32) -> u32 {
    if input < 0 {
        return 32;
    }
    32 - (input as u32).leading_zeros()
}

/// TagTool's `IsPowerOfTwo`, which answers `true` for zero.
///
/// Kept rather than corrected: it guards the "round a mip up to a power of two"
/// step, and a zero dimension there must not be rounded to one.
#[inline]
pub fn is_power_of_two(x: u32) -> bool {
    x & x.wrapping_sub(1) == 0
}

/// Which shape of texture a surface is, in the terms the alignment rules use.
///
/// The 360 packs a 1D texture, a 2D or array texture and a volume differently.
/// Numbered as TagTool's `GetXboxBitmapD3DTextureType` numbers them, because the
/// alignment code branches on the number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureType {
    /// 2D, and an array of 2D surfaces.
    Flat = 1,
    /// A volume texture.
    Volume = 2,
    /// Six faces.
    CubeMap = 3,
}

/// Round a surface up to the tile grid the GPU stores it on.
///
/// Returns the aligned dimensions. TagTool's `AlignTextureDimensions`, minus the
/// size it also returns and nothing here uses.
pub fn align_texture_dimensions(
    width: u32,
    height: u32,
    depth: u32,
    bits_per_pixel: u32,
    block_width: u32,
    block_height: u32,
    texture_type: TextureType,
    tiled: bool,
) -> (u32, u32, u32) {
    let mut tile_width = 32;
    let tile_height = 32;
    let tile_depth = if texture_type == TextureType::Volume { 4 } else { 1 };

    // An untiled surface is packed to a 256-byte row instead, so a narrow
    // format aligns to more texels rather than fewer.
    if !tiled {
        let texel_pitch = block_height * block_width * bits_per_pixel >> 3;
        if texel_pitch > 0 && tile_width <= 0x100 / texel_pitch {
            tile_width = 0x100 / texel_pitch;
        }
    }

    (
        next_multiple_of(width, tile_width * block_width),
        next_multiple_of(height, tile_height * block_height),
        next_multiple_of(depth, tile_depth),
    )
}

/// Round a mip's aligned dimension up to a power of two.
///
/// Every level past the base is stored at a power-of-two size whatever the base
/// was, which is what makes a 640x480 texture's mip chain reachable at all.
#[inline]
fn round_up_to_power_of_two(value: u32) -> u32 {
    if is_power_of_two(value) {
        return value;
    }
    1u32 << log2_ceiling(value as i32)
}

/// One Xbox 360 surface, as the layout rules need to see it.
///
/// Everything here comes off the tag's own `xenon bitmaps[i]` element. The
/// resource header carries the same facts as a packed D3D format word, but a
/// hydrated resource does not yet resolve its own struct definition, so the
/// image block is the source that can be trusted.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceDef {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub texture_type: TextureType,
    /// Bits per *pixel*, so 4 for DXT1 and 32 for a8r8g8b8.
    pub bits_per_pixel: u32,
    pub block_width: u32,
    pub block_height: u32,
    /// `xbox360 tiled texture`. A Reach build ships plenty of small textures
    /// stored linear, and un-tiling one of those scrambles it.
    pub tiled: bool,
    pub endian_swap: EndianSwap,
    /// Levels *beyond* the base, matching the tag's own `mipmap count`.
    pub mipmap_count: u32,
    /// The base level lives in the resource's secondary buffer rather than at
    /// the head of the primary one.
    pub high_res_in_secondary: bool,
}

impl SurfaceDef {
    /// Bytes for one block of this format.
    #[inline]
    fn texel_pitch(&self) -> u32 {
        self.block_width * self.block_height * self.bits_per_pixel / 8
    }

    /// How many surfaces are stacked under one image: faces, or array layers.
    pub fn layer_count(&self) -> u32 {
        match self.texture_type {
            TextureType::CubeMap => 6,
            TextureType::Volume => 1,
            TextureType::Flat => self.depth.max(1),
        }
    }

    /// How many layers one level's offset advances by.
    ///
    /// A cube map's six faces are stored as six, an array's layers round up to
    /// an even count, and a plain 2D texture is one.
    fn array_stride(&self) -> u32 {
        match self.texture_type {
            TextureType::CubeMap => next_multiple_of(6, 1),
            TextureType::Flat if self.depth > 1 => next_multiple_of(self.depth, 4),
            _ => 1,
        }
    }

    /// Total levels including the base.
    pub fn level_count(&self) -> u32 {
        self.mipmap_count.max(0) + 1
    }

    /// Whether the small end of the chain is packed into one shared tile.
    fn is_packed(&self) -> bool {
        self.level_count() > 1
    }

    /// The level at which the chain becomes packed, i.e. the first whose
    /// smaller dimension is 16 texels or under.
    ///
    /// TagTool's `GetMipLevelRequiresOffset`. Zero means the base itself is
    /// already that small, and the whole chain shares one tile.
    fn packed_from_level(&self) -> u32 {
        let log_width = log2_ceiling(self.width as i32 - 1);
        let log_height = log2_ceiling(self.height as i32 - 1);
        log_width.min(log_height).saturating_sub(4)
    }

    /// The aligned, power-of-two-rounded size of one level, in texels.
    fn aligned_level(&self, level: u32) -> (u32, u32, u32) {
        let level_width = (self.width >> level).max(1);
        let level_height = (self.height >> level).max(1);
        let (mut aligned_width, mut aligned_height, aligned_depth) = align_texture_dimensions(
            level_width,
            level_height,
            self.depth,
            self.bits_per_pixel,
            self.block_width,
            self.block_height,
            self.texture_type,
            self.tiled,
        );
        if level > 0 {
            aligned_width = round_up_to_power_of_two(aligned_width);
            aligned_height = round_up_to_power_of_two(aligned_height);
        }
        (aligned_width, aligned_height, aligned_depth)
    }
}

/// Where a level's bytes start inside the buffer that holds them.
///
/// TagTool's `GetXboxBitmapLevelOffset`. The shape is a running sum over the
/// levels before this one, each rounded up to a 4KB page, multiplied by however
/// many layers the image stacks. Two details carry the weight:
///
/// - The sum stops as soon as a level's smaller side reaches 16 texels, because
///   from there down the chain shares one tile and is addressed by
///   [`mip_tail_offset`] instead.
/// - When the base level lives in the secondary buffer it contributes nothing to
///   an offset into the primary one, so it is skipped rather than added.
pub fn level_offset(def: &SurfaceDef, array_index: u32, level: u32, has_high_res: bool) -> u32 {
    let packed = def.is_packed();
    if level == 0 && !(packed && (def.width <= 16 || def.height <= 16)) {
        // The base level of an unpacked image: nothing before it but whichever
        // layers of itself come first.
        let (aligned_width, aligned_height, _) = {
            let (w, h, d) = align_texture_dimensions(
                def.width,
                def.height,
                def.depth,
                def.bits_per_pixel,
                def.block_width,
                def.block_height,
                def.texture_type,
                def.tiled,
            );
            (w, h, d)
        };
        let layer_size = def.bits_per_pixel * aligned_width * aligned_height >> 3;
        return next_multiple_of(layer_size, 0x1000) * array_index;
    }

    let array_stride = def.array_stride();
    let mut offset = 0u32;
    for i in 0..level {
        let level_width = (def.width >> i).max(1);
        let level_height = (def.height >> i).max(1);
        let (aligned_width, aligned_height, aligned_depth) = def.aligned_level(i);

        // The base sits in the other buffer, so it takes up none of this one.
        if has_high_res && i == 0 {
            continue;
        }
        // From here down the chain is one shared tile.
        if (level_width <= 16 || level_height <= 16) && packed {
            break;
        }
        let layer_size = def.bits_per_pixel * aligned_width * aligned_height >> 3;
        let level_size = if def.texture_type == TextureType::Volume {
            next_multiple_of(aligned_depth * layer_size, 0x1000)
        } else {
            aligned_depth * next_multiple_of(layer_size, 0x1000)
        };
        offset += array_stride * level_size;
    }

    // The loop above covers every layer of every earlier level; this steps into
    // the right layer of *this* one.
    if array_index > 0 {
        let (aligned_width, aligned_height, _) = def.aligned_level(level);
        let size = aligned_width * aligned_height * def.bits_per_pixel / 8;
        offset += array_index * next_multiple_of(size, 0x1000);
    }
    offset
}

/// Where a packed level sits inside the shared tail tile, in texels.
///
/// Once a level's smaller side is 16 texels or less the 360 stops giving it a
/// page of its own and tucks it into a corner of the last full tile. This is
/// TagTool's `GetMipTailLevelOffsetCoords` reduced to the 2D case, which is the
/// only one a bitmap tag uses.
pub fn mip_tail_offset(def: &SurfaceDef, level: u32) -> (u32, u32) {
    let packed_from = def.packed_from_level();
    if level < packed_from {
        return (0, 0);
    }
    let log_width = log2_ceiling(def.width as i32 - 1);
    let log_height = log2_ceiling(def.height as i32 - 1);
    let tail_width = 1u32 << log_width.saturating_sub(packed_from);
    let tail_height = 1u32 << log_height.saturating_sub(packed_from);
    let tail_level = level - packed_from;

    let tail_log_width = log2_ceiling(tail_width as i32 - 1);
    let tail_log_height = log2_ceiling(tail_height as i32 - 1);

    let (offset_x, offset_y) = if tail_level < 3 {
        // The first few sit beside the level above, on whichever axis is longer.
        if tail_log_height < tail_log_width {
            (0, 16 >> tail_level)
        } else {
            (16 >> tail_level, 0)
        }
    } else if tail_log_width > tail_log_height {
        ((1u32 << tail_log_width) >> (tail_level - 2), 0)
    } else {
        (0, (1u32 << tail_log_height) >> (tail_level - 2))
    };

    // Reported in blocks, because everything downstream indexes in blocks.
    (offset_x / def.block_width, offset_y / def.block_height)
}

/// One level of one layer, in the shape a PC tag stores it.
///
/// TagTool's `GetXboxBitmapLevelData`, in order: pick the buffer, slice at the
/// level's offset, un-tile, crop out of the tail tile and off the alignment
/// padding, then swap the bytes.
///
/// `primary` and `secondary` are the resource's two buffers. A build truncates
/// a buffer it did not need all of, so a short slice is padded rather than
/// refused — the padding lands in alignment rows the crop then drops.
pub fn level_data(
    def: &SurfaceDef,
    primary: &[u8],
    secondary: &[u8],
    level: u32,
    array_index: u32,
) -> Option<Vec<u8>> {
    let (aligned_width, aligned_height, _) = def.aligned_level(level);
    let (point_x, point_y) = if def.level_count() > 1 {
        mip_tail_offset(def, level)
    } else {
        (0, 0)
    };

    // A packed level whose corner falls outside the first tile needs the tiles
    // it spills into read as well.
    let mut aligned_width = aligned_width;
    let mut aligned_height = aligned_height;
    if point_x >= 32 {
        aligned_width *= 1 + point_x / 32;
    }
    if point_y >= 32 {
        aligned_height *= 1 + point_y / 32;
    }

    let texel_pitch = def.texel_pitch();
    let size = (aligned_width * aligned_height * def.bits_per_pixel / 8) as usize;
    // Each packed level is page-aligned, which is what lets the un-tiler read a
    // whole number of tiles.
    let size = (size + 0xFFF) & !0xFFF;

    let use_secondary = level == 0 && def.high_res_in_secondary;
    let (buffer, offset) = if use_secondary {
        (secondary, level_offset(def, array_index, level, false))
    } else {
        (
            primary,
            level_offset(def, array_index, level, def.high_res_in_secondary),
        )
    };
    let offset = offset as usize;
    if offset >= buffer.len() {
        return None;
    }

    let mut data = vec![0u8; size];
    let available = (buffer.len() - offset).min(size);
    data[..available].copy_from_slice(&buffer[offset..offset + available]);

    if def.tiled {
        let block_columns = aligned_width / def.block_width;
        let block_rows = aligned_height / def.block_height;
        data = detile_blocks(&data, block_columns, block_rows, texel_pitch);
    }

    // Crop to the level's own block grid, out of whatever tile it shares.
    let level_width = (def.width >> level).max(1);
    let level_height = (def.height >> level).max(1);
    let level_width = next_multiple_of_any(level_width, def.block_width);
    let level_height = next_multiple_of_any(level_height, def.block_height);
    let out_columns = level_width / def.block_width;
    let out_rows = level_height / def.block_height;
    let slice_columns = aligned_width / def.block_width;

    let mut out = vec![0u8; (out_columns * out_rows * texel_pitch) as usize];
    if point_x == 0 && point_y == 0 && out.len() == data.len() {
        out.copy_from_slice(&data);
    } else {
        for row in 0..out_rows {
            for column in 0..out_columns {
                let from =
                    (((row + point_y) * slice_columns) + column + point_x) * texel_pitch;
                let to = ((row * out_columns) + column) * texel_pitch;
                let (from, to) = (from as usize, to as usize);
                if from + texel_pitch as usize > data.len() {
                    continue;
                }
                out[to..to + texel_pitch as usize]
                    .copy_from_slice(&data[from..from + texel_pitch as usize]);
            }
        }
    }

    def.endian_swap.apply(&mut out);
    Some(out)
}

/// Round up to a multiple that is not necessarily a power of two.
///
/// Block dimensions are 1 or 4 in practice, but the crop above must not depend
/// on that.
#[inline]
fn next_multiple_of_any(value: u32, multiple: u32) -> u32 {
    if multiple == 0 || value % multiple == 0 {
        value
    } else {
        value + multiple - value % multiple
    }
}
