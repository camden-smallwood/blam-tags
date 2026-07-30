//! Bitmap format identifiers and associated metadata.
//!
//! [`BitmapFormat`] covers every variant in the schema's
//! `bitmap_formats` enum (49 entries across halo3_mcc, halo3odst_mcc,
//! haloreach_mcc, halo4_mcc / halo4_xbox360 schemas). Indices on disk
//! differ between games, but the schema enum option *names* are
//! stable and that's what [`crate::bitmap::BitmapFormat::from_schema_name`] keys on.
//!
//! [`BitmapCurve`] is the canonical 6-value `e_bitmap_curve` enum
//! verified against H3 source `bitmap_curve.h`. Maps to the schema's
//! `bitmap_curve_enum` by index — the schema strings are display
//! names with awkward curly-brace alts, so we read the enum by index
//! rather than by string.

/// A bitmap pixel format. Variants correspond 1:1 to the schema's
/// `bitmap_formats` enum members — see [`Self::from_schema_name`].
///
/// Many of these are Halo-specific block-compressed formats with no
/// direct DDS pixelformat. The general decode-time strategy mirrors
/// TagTool's:
///
/// - Formats with a clean legacy DDS pixelformat get written as DDS
///   bit-exact (`Dxt1`, `Dxt3`, `Dxt5`, `Dxn`, the 8/16/32-bit
///   uncompressed types, `Abgrfp16/32`).
/// - Halo-only formats (`Ctx1`, `Dxt5nm`, the `Dxt3a*` and `Dxt5a*`
///   families, `DxnMonoAlpha`, etc.) are CPU-decoded to RGBA8 first
///   and then output as `A8r8g8b8` — TIFF is RGBA8 by default, DDS
///   writes the decoded RGBA8 with the `A8r8g8b8` pixelformat.
/// - Reach/H4 single-channel `Dxt5_red/green/blue` route the BC3
///   alpha output to the named color channel.
/// - `SoftwareRgbfp32`, `Depth24`, `Unused*` are schema-reserved
///   slots not observed in any corpus; they decode as a stub
///   "unsupported".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitmapFormat {
    // 8-bit single / dual channel uncompressed.
    A8,
    Y8,
    Ay8,
    A8y8,
    R8,
    /// Schema reserved slot. Not observed in any corpus.
    Unused2,
    // 16-bit packed RGB(A).
    R5g6b5,
    /// Schema reserved slot. Not observed in any corpus.
    Unused3,
    A1r5g5b5,
    A4r4g4b4,
    // 32-bit RGB(A).
    X8r8g8b8,
    A8r8g8b8,
    /// Schema reserved slot. Not observed in any corpus.
    Unused4,
    /// DXT5 normal-map variant. BC3-shaped (16 bytes/block) but the
    /// alpha channel carries the normal's X component and the color
    /// block's green carries Y; Z is reconstructed from X²+Y²+Z²=1.
    Dxt5nm,
    Dxt1,
    Dxt3,
    Dxt5,
    /// Bungie's "A4R4G4B4 font" — same 16-bit storage as
    /// [`Self::A4r4g4b4`] but treated as a P8-style palette index
    /// that decodes to grayscale-with-alpha. TagTool routes it
    /// through `DecodeP8`.
    A4r4g4b4Font,
    /// Schema reserved slot.
    Unused7,
    /// Schema reserved slot.
    Unused8,
    /// 96-bit `software_rgbfp32` — debug-only render target format,
    /// not observed in shipped corpora.
    SoftwareRgbfp32,
    /// Schema reserved slot.
    Unused9,
    /// Signed 16-bit two-channel (V/U); +128 bias for unsigned view.
    V8u8,
    /// Unsigned 16-bit two-channel.
    G8b8,
    /// 128-bit four-channel float (single-precision).
    Abgrfp32,
    /// 64-bit four-channel float (half-precision).
    Abgrfp16,
    /// 16-bit float mono (Reach+).
    F16Mono,
    /// 16-bit float red (Reach+).
    F16Red,
    /// 32-bit four-channel signed (Q/W/V/U).
    Q8w8v8u8,
    A2r10g10b10,
    A16b16g16r16,
    /// 32-bit signed two-channel normal map.
    V16u16,
    /// 16-bit unsigned luminance (Reach+).
    L16,
    /// 32-bit two-channel (Reach+).
    R16g16,
    /// 64-bit four-channel signed (Reach+). Has no legacy DDS
    /// fourcc; we write it via DXT10 extension as
    /// `DXGI_FORMAT_R16G16B16A16_SNORM`.
    Signedr16g16b16a16,
    /// BC4-shaped (8 bytes/block) 4-bit alpha-only encoding. Each
    /// 4-bit value expands to `alpha * 17` and replicates into all
    /// channels.
    Dxt3a,
    /// Halo's BC4 (8 bytes/block) interpreted as a single-channel
    /// alpha. Decodes by replicating the alpha through RGBA.
    Dxt5a,
    /// BC4-shaped 4-bit per pixel where the 4 bits are unpacked as
    /// 4 binary channels (R/G/B/A).
    Dxt3a1111,
    /// Two-channel BC5/ATI2. 16 bytes/block.
    Dxn,
    /// BC1-shaped (8 bytes/block) two-channel normal map. Color
    /// endpoints are 8-8 rather than 5-6-5 RGB; Z is reconstructed.
    Ctx1,
    /// `Dxt3a` with `R=G=B=alpha, A=255`.
    Dxt3aAlpha,
    /// `Dxt3a` with `R=G=B=alpha, A=0` (alpha mask zeroed).
    Dxt3aMono,
    /// `Dxt5a` with `R=G=B=0, A=alpha` (alpha-only).
    Dxt5aAlpha,
    /// `Dxt5a` with `R=G=B=alpha, A=0`.
    Dxt5aMono,
    /// Halo-specific: BC5-shaped (16 bytes/4×4 block) but the two
    /// sub-blocks are *luminance* (red half) and *alpha* (green
    /// half). Decodes to A8R8G8B8 with `(R, R, R, A)` semantics —
    /// no DDS reader does this swizzle automatically, so we run the
    /// decoder before writing.
    DxnMonoAlpha,
    /// `Dxt5` red-only (Reach+) — BC3 alpha block routed to R.
    Dxt5Red,
    /// `Dxt5` green-only (Reach+) — BC3 alpha block routed to G.
    Dxt5Green,
    /// `Dxt5` blue-only (Reach+) — BC3 alpha block routed to B.
    Dxt5Blue,
    /// 24-bit depth (Reach+). Render-only; not observed in shipped tags.
    Depth24,
    /// Classic Halo CE / Halo 2 palettized 8-bit. Each pixel is an
    /// index into a fixed 256-entry palette (`super::p8::P8_PALETTE`).
    /// Used for height / vector (bump) maps. Decodes to A8R8G8B8 via
    /// the palette — no native DDS pixelformat.
    P8,
    /// Classic Halo CE / Halo 2 palettized 8-bit *bump* — same on-disk
    /// storage and palette as [`Self::P8`] (the palette entries encode
    /// normal vectors). Kept distinct so the schema name round-trips.
    P8Bump,
}

impl BitmapFormat {
    /// Ares `bitmap_format_is_default_srgb` (dllcache 0x1804E9100) — whether a
    /// texture of this format is sRGB-decoded by the hardware when the bitmap's
    /// `curve` is `Unknown`. TRUE for all color/alpha/mono LDR formats; FALSE
    /// for float, signed (`u`/`v`), high-precision (10/16-bit), and normal-map
    /// (DXN/CTX1/DXT5nm) formats — those are always linear. The H3 engine keys
    /// on the discriminants {20,22,24,25,26,27,28,29,33,34}; mapped here by
    /// meaning so the multi-game superset (float16/L16/R16g16/signed16/Dxt5nm)
    /// stays consistent. Used together with `curve` by `get_pc_d3d_format`:
    /// `is_srgb = curve!=Unknown ? curve∈{xRGB,sRGB} : format.is_default_srgb()`.
    pub fn is_default_srgb(self) -> bool {
        use BitmapFormat::*;
        !matches!(
            self,
            SoftwareRgbfp32
                | V8u8
                | Abgrfp32
                | Abgrfp16
                | F16Mono
                | F16Red
                | Q8w8v8u8
                | A2r10g10b10
                | A16b16g16r16
                | V16u16
                | L16
                | R16g16
                | Signedr16g16b16a16
                | Dxn
                | Ctx1
                | Dxt5nm
        )
    }

    /// Resolve the schema's `bitmap_formats` enum option name. Names
    /// match those in `definitions/<game>/bitmap.json` and are stable
    /// across halo3_mcc and haloreach_mcc.
    pub fn from_schema_name(name: &str) -> Option<Self> {
        // Match case-insensitively: gen3/gen4 schemas use lowercase
        // (`dxt3`), but the classic Halo CE schema uses uppercase
        // (`DXT3`, `A8R8G8B8`) for the same formats.
        Some(match name.to_ascii_lowercase().as_str() {
            "a8" => Self::A8,
            "y8" => Self::Y8,
            "ay8" => Self::Ay8,
            "a8y8" => Self::A8y8,
            "r8" => Self::R8,
            "unused2" => Self::Unused2,
            "r5g6b5" => Self::R5g6b5,
            "unused3" => Self::Unused3,
            "a1r5g5b5" => Self::A1r5g5b5,
            "a4r4g4b4" => Self::A4r4g4b4,
            "x8r8g8b8" => Self::X8r8g8b8,
            "a8r8g8b8" => Self::A8r8g8b8,
            "unused4" => Self::Unused4,
            "dxt5_bias_alpha" | "dxt5nm" => Self::Dxt5nm,
            "dxt1" => Self::Dxt1,
            "dxt3" => Self::Dxt3,
            "dxt5" => Self::Dxt5,
            "a4r4g4b4 font" => Self::A4r4g4b4Font,
            "unused7" => Self::Unused7,
            "unused8" => Self::Unused8,
            "software rgbfp32" => Self::SoftwareRgbfp32,
            "unused9" => Self::Unused9,
            "v8u8" => Self::V8u8,
            "g8b8" => Self::G8b8,
            "abgrfp32" => Self::Abgrfp32,
            "abgrfp16" => Self::Abgrfp16,
            "16f_mono" => Self::F16Mono,
            "16f_red" => Self::F16Red,
            "q8w8v8u8" => Self::Q8w8v8u8,
            "a2r10g10b10" => Self::A2r10g10b10,
            "a16b16g16r16" => Self::A16b16g16r16,
            "v16u16" => Self::V16u16,
            "l16" => Self::L16,
            "r16g16" => Self::R16g16,
            "signedr16g16b16a16" => Self::Signedr16g16b16a16,
            "dxt3a" => Self::Dxt3a,
            "dxt5a" => Self::Dxt5a,
            "dxt3a_1111" => Self::Dxt3a1111,
            "dxn" => Self::Dxn,
            "ctx1" => Self::Ctx1,
            "dxt3a_alpha" => Self::Dxt3aAlpha,
            "dxt3a_mono" => Self::Dxt3aMono,
            "dxt5a_alpha" => Self::Dxt5aAlpha,
            "dxt5a_mono" => Self::Dxt5aMono,
            "dxn_mono_alpha" => Self::DxnMonoAlpha,
            "dxt5_red" => Self::Dxt5Red,
            "dxt5_green" => Self::Dxt5Green,
            "dxt5_blue" => Self::Dxt5Blue,
            "depth 24" | "depth24" => Self::Depth24,
            "p8" => Self::P8,
            "p8-bump" | "p8_bump" => Self::P8Bump,
            _ => return None,
        })
    }

    /// Whether this format stores pixels as 4×4 blocks rather than
    /// per-pixel.
    pub fn is_compressed(self) -> bool {
        matches!(
            self,
            Self::Dxt1
                | Self::Dxt3
                | Self::Dxt5
                | Self::Dxt5nm
                | Self::Dxt5a
                | Self::Dxn
                | Self::DxnMonoAlpha
                | Self::Dxt3a
                | Self::Dxt3a1111
                | Self::Dxt3aAlpha
                | Self::Dxt3aMono
                | Self::Dxt5aAlpha
                | Self::Dxt5aMono
                | Self::Ctx1
                | Self::Dxt5Red
                | Self::Dxt5Green
                | Self::Dxt5Blue
        )
    }

    /// Whether the format only has a clean DDS expression via the
    /// DXT10 extension header. Currently just `signedr16g16b16a16`.
    pub fn requires_dxt10(self) -> bool {
        matches!(self, Self::Signedr16g16b16a16)
    }

    /// Whether channel values are stored as signed integers and need
    /// a `+128` (or `+32768` for 16-bit) bias to display as unsigned.
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            Self::V8u8
                | Self::Q8w8v8u8
                | Self::V16u16
                | Self::Signedr16g16b16a16
        )
    }

    /// Whether the format stores HDR float values that exceed the
    /// `[0, 1]` range a typical 8-bit display can carry.
    pub fn is_hdr(self) -> bool {
        matches!(
            self,
            Self::Abgrfp16 | Self::Abgrfp32 | Self::F16Mono | Self::F16Red | Self::SoftwareRgbfp32
        )
    }

    /// Bytes per stored block for compressed formats. 8 for BC1 /
    /// BC4-shaped, 16 for BC2 / BC3 / BC5-shaped. 0 for
    /// uncompressed formats.
    pub fn block_bytes(self) -> u32 {
        match self {
            Self::Dxt1
            | Self::Dxt5a
            | Self::Dxt3a
            | Self::Dxt3a1111
            | Self::Dxt3aAlpha
            | Self::Dxt3aMono
            | Self::Dxt5aAlpha
            | Self::Dxt5aMono
            | Self::Ctx1 => 8,
            Self::Dxt3
            | Self::Dxt5
            | Self::Dxt5nm
            | Self::Dxn
            | Self::DxnMonoAlpha
            | Self::Dxt5Red
            | Self::Dxt5Green
            | Self::Dxt5Blue => 16,
            _ => 0,
        }
    }

    /// `(block_width, block_height, bytes_per_block)` for any format
    /// the X360 detiler can handle. Returns `None` for unsupported /
    /// schema-reserved variants. Compressed formats use 4×4 blocks;
    /// uncompressed use 1×1.
    pub fn block_dims_and_size(self) -> Option<(u32, u32, u32)> {
        if self.is_compressed() {
            Some((4, 4, self.block_bytes()))
        } else if self.bytes_per_pixel() > 0 {
            Some((1, 1, self.bytes_per_pixel()))
        } else {
            None
        }
    }

    /// Bytes per pixel for uncompressed formats. 0 for compressed and
    /// unsupported/reserved slots. Matches the canonical bits-per-pixel
    /// table read from MCC `ManagedBlam.dll` at `0x180C99010` and
    /// TagTool's `BitmapFormat.BitsPerPixelTable`.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::A8 | Self::Y8 | Self::Ay8 | Self::R8 | Self::P8 | Self::P8Bump => 1,
            Self::A8y8
            | Self::R5g6b5
            | Self::A1r5g5b5
            | Self::A4r4g4b4
            | Self::A4r4g4b4Font
            | Self::V8u8
            | Self::G8b8
            | Self::F16Mono
            | Self::F16Red
            | Self::L16 => 2,
            Self::X8r8g8b8
            | Self::A8r8g8b8
            | Self::Q8w8v8u8
            | Self::A2r10g10b10
            | Self::V16u16
            | Self::R16g16
            | Self::Depth24 => 4,
            Self::A16b16g16r16 | Self::Signedr16g16b16a16 | Self::Abgrfp16 => 8,
            Self::SoftwareRgbfp32 => 12,
            Self::Abgrfp32 => 16,
            _ => 0,
        }
    }

    /// Bytes consumed by one mip level at `(width, height)`. For
    /// compressed formats, dimensions round up to the 4×4 block grid
    /// with a 1-block minimum so 1×1 / 2×2 mips cost one block.
    pub fn level_bytes(self, width: u32, height: u32) -> u64 {
        if self.is_compressed() {
            let blocks_w = ((width + 3) / 4).max(1);
            let blocks_h = ((height + 3) / 4).max(1);
            blocks_w as u64 * blocks_h as u64 * self.block_bytes() as u64
        } else {
            width as u64 * height as u64 * self.bytes_per_pixel() as u64
        }
    }

    /// Total pixel bytes for one full mipmap chain at the given base
    /// dimensions. `mipmap_levels` is the total level count (= the
    /// schema's `mipmap count + 1` since that field excludes the base
    /// level).
    pub fn surface_bytes(self, width: u32, height: u32, mipmap_levels: u32) -> u64 {
        (0..mipmap_levels)
            .map(|i| self.level_bytes((width >> i).max(1), (height >> i).max(1)))
            .sum()
    }

    /// Whether this is a schema-reserved or render-only slot we
    /// don't model as a real bitmap format.
    pub fn is_unsupported_stub(self) -> bool {
        matches!(
            self,
            Self::Unused2
                | Self::Unused3
                | Self::Unused4
                | Self::Unused7
                | Self::Unused8
                | Self::Unused9
                | Self::SoftwareRgbfp32
                | Self::Depth24
        )
    }
}

/// Per-image gamma curve. Mirrors `e_bitmap_curve` from H3 source
/// (`bitmap_curve.h`) — same enumeration order as the schema's
/// `bitmap_curve_enum`. The schema's display strings are noisy
/// (e.g. `"xRGB (gamma about 2.0){SRGB (gamma 2.2)}"` for `XrgbGamma2`),
/// so callers should read the underlying integer rather than the
/// resolved string.
/// `bitmap_curve_enum` (char_enum). Resolved by embedded schema name
/// (drift-immune) via the `SchemaEnum` blanket `from_schema_name`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BitmapCurve {
    #[default]
    #[strum(serialize = "unknown")] Unknown = 0,
    /// xRGB on Xenon, sRGB-equivalent (~γ 2.2) on PC.
    #[strum(serialize = "xRGB (gamma about 2.0)")] XrgbGamma2 = 1,
    #[strum(serialize = "gamma 2.0")] Gamma2 = 2,
    #[strum(serialize = "linear")] Linear = 3,
    #[strum(serialize = "offset log")] OffsetLog = 4,
    #[strum(serialize = "sRGB")] Srgb = 5,
}

// ===========================================================================
// Tag-metadata enums — the authored bitmap-group fields.
//
// Field/enum names mirror Ares `bitmaps/bitmap_group.h` + `bitmap_types.h`
// (`e_bitmap_type`, `bitmap_group_v2::e_flags` / `e_curve_override`, tag
// `e_bitmap_flags`). `Usage` has NO runtime enum in Ares (`usage_index` is a
// raw `unsigned long`); the 25-value list is authoring-only (guerilla schema),
// surfaced here typed. `#[strum(serialize)]` strings are the canonical schema
// option names — the `SchemaEnum` fold matches them drift-proof by name.
// ===========================================================================

/// `bitmap_usage_global_enum` — the authoring "how are you using this bitmap"
/// selector. Engine stores it as the raw `usage_index` (`unsigned long`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i32)]
pub enum BitmapUsage {
    #[default]
    #[strum(serialize = "Diffuse Map")] DiffuseMap = 0,
    #[strum(serialize = "Specular Map")] SpecularMap = 1,
    #[strum(serialize = "Bump Map (from Height Map)")] BumpMap = 2,
    #[strum(serialize = "Detail Bump Map (from Height Map - fades out)")] DetailBumpMap = 3,
    #[strum(serialize = "Detail Map")] DetailMap = 4,
    #[strum(serialize = "Self-Illum Map")] SelfIllumMap = 5,
    #[strum(serialize = "Change Color Map")] ChangeColorMap = 6,
    #[strum(serialize = "Cube Map (Reflection Map)")] CubeMap = 7,
    #[strum(serialize = "Sprite (Additive, Black Background)")] SpriteAdditive = 8,
    #[strum(serialize = "Sprite (Blend, White Background)")] SpriteBlend = 9,
    #[strum(serialize = "Sprite (Double Multiply, Gray Background)")] SpriteDoubleMultiply = 10,
    #[strum(serialize = "Interface Bitmap")] InterfaceBitmap = 11,
    /// EMBM warp/displacement map (distortion particles, e.g. `distortion_shimmer`).
    #[strum(serialize = "Warp Map (EMBM)")] WarpMap = 12,
    #[strum(serialize = "Vector Map")] VectorMap = 13,
    #[strum(serialize = "3D Texture")] Texture3d = 14,
    #[strum(serialize = "Float Map (WARNING: HUGE)")] FloatMap = 15,
    #[strum(serialize = "Height Map (for Parallax)")] HeightMap = 16,
    #[strum(serialize = "ZBrush Bump Map (from Bump Map)")] ZBrushBumpMap = 17,
    #[strum(serialize = "Blend Map (linear for terrains)")] BlendMap = 18,
    #[strum(serialize = "Palettized --- effects only")] Palettized = 19,
    #[strum(serialize = "CHUD related bitmap")] Chud = 20,
    #[strum(serialize = "Lightmap Array")] LightmapArray = 21,
    #[strum(serialize = "Water Array")] WaterArray = 22,
    #[strum(serialize = "Interface Sprite")] InterfaceSprite = 23,
    #[strum(serialize = "Interface Gradient")] InterfaceGradient = 24,
}

/// Ares `e_bitmap_type` (`bitmap_types.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum BitmapType {
    #[default]
    #[strum(serialize = "2D texture")] TwoDimensional = 0,
    #[strum(serialize = "3D texture")] ThreeDimensional = 1,
    #[strum(serialize = "cube map")] CubeMap = 2,
    #[strum(serialize = "array")] Array = 3,
}

/// Ares `bitmap_group_v2::e_curve_override` — the "curve mode" import selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BitmapCurveOverride {
    #[default]
    #[strum(serialize = "choose best")] ChooseBest = 0,
    #[strum(serialize = "force FAST")] ForceFast = 1,
    #[strum(serialize = "force PRETTY")] ForcePretty = 2,
}

/// `bitmap_group_v2::e_flags` — the authored bitmap-group flag word.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum BitmapGroupFlags {
    #[strum(serialize = "bitmap is TILED")] Tiled = 0,
    #[strum(serialize = "use less blurry bump map")] UseSharpBumpFilter = 1,
    #[strum(serialize = "dither when compressing")] DitherWhenCompressing = 2,
    #[strum(serialize = "generate random sprites")] GenerateRandomSprites = 3,
    #[strum(serialize = "using tag_interop and tag_resource")] UsingInteropResource = 4,
    #[strum(serialize = "alpha channel stores TRANSPARENCY")] AlphaIsTransparency = 5,
    #[strum(serialize = "can be sampled")] CanBeSampled = 6,
}

/// `bitmap_data` tag flags — Ares `e_bitmap_flags` bits 0-2
/// (`k_tag_bitmap_flags_count`; bits 3+ are runtime-only, not authored).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum BitmapDataFlags {
    #[strum(serialize = "power of two dimensions")] PowerOfTwo = 0,
    #[strum(serialize = "compressed")] Compressed = 1,
    #[strum(serialize = "swap axes")] SwapAxes = 2,
}

/// `bitmap_data::more_flags` — Xbox-360 layout markers (`bitmap_more_flags_definition`).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum BitmapMoreFlags {
    #[strum(serialize = "delete from cache file")] DeleteFromCache = 0,
    #[strum(serialize = "xbox360 pitch (memory spacing)")] X360Pitch = 1,
    #[strum(serialize = "xbox360 byte order")] X360ByteOrder = 2,
    #[strum(serialize = "xbox360 tiled texture")] X360Tiled = 3,
    #[strum(serialize = "xbox360 created correctly (hack for bumpmaps)")] X360CreatedCorrectly = 4,
    #[strum(serialize = "xbox360 high resolution offset is valid")] X360HighResOffsetValid = 5,
    #[strum(serialize = "xbox360 use interleaved textures")] X360Interleaved = 6,
}

/// `bitmap_usage_format_def` — the "force bitmap format" / usage-override
/// format override. Reserved/unused/separator slots kept so `FromPrimitive`
/// is total and by-name decode is exact; `Enum<_,i16>` keeps any raw value it
/// can't name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum BitmapForceFormat {
    #[default]
    #[strum(serialize = "Use Default (defined by usage)")] UseDefault = 0,
    #[strum(serialize = "Best Compressed Color Format")] BestCompressedColor = 1,
    #[strum(serialize = "Best Uncompressed Color Format")] BestUncompressedColor = 2,
    #[strum(serialize = "Best Compressed Bump Format")] BestCompressedBump = 3,
    #[strum(serialize = "Best Uncompressed Bump Format")] BestUncompressedBump = 4,
    #[strum(serialize = "Best Compressed Monochrome Format")] BestCompressedMono = 5,
    #[strum(serialize = "Best Uncompressed Monochrome Format")] BestUncompressedMono = 6,
    #[strum(serialize = "unused2")] Unused2 = 7,
    #[strum(serialize = "unused3")] Unused3 = 8,
    #[strum(serialize = "unused4")] Unused4 = 9,
    #[strum(serialize = "unused5")] Unused5 = 10,
    #[strum(serialize = "unused6")] Unused6 = 11,
    #[strum(serialize = "--- Color and Alpha formats ---")] SepColorAlpha = 12,
    #[strum(serialize = "DXT1 (Compressed Color + Color-Key Alpha)")] Dxt1 = 13,
    #[strum(serialize = "DXT3 (Compressed Color + 4-bit Alpha)")] Dxt3 = 14,
    #[strum(serialize = "DXT5 (Compressed Color + Compressed 8-bit Alpha)")] Dxt5 = 15,
    #[strum(serialize = "24-bit Color + 8-bit Alpha")] Color24Alpha8 = 16,
    #[strum(serialize = "8-bit Monochrome + 8-bit Alpha")] Mono8Alpha8 = 17,
    #[strum(serialize = "Channel Mask (3-bit Color + 1-bit Alpha)")] ChannelMask = 18,
    #[strum(serialize = "30-bit Color + 2-bit Alpha")] Color30Alpha2 = 19,
    #[strum(serialize = "48-bit Color + 16-bit Alpha")] Color48Alpha16 = 20,
    #[strum(serialize = "HALF Color + Alpha")] HalfColorAlpha = 21,
    #[strum(serialize = "FLOAT Color + Alpha")] FloatColorAlpha = 22,
    #[strum(serialize = "AY8 (8-bit Intensity replicated to ARGB)")] Ay8 = 23,
    #[strum(serialize = "DXT3A (4-bit Intensity replicated to ARGB)")] Dxt3aIntensity = 24,
    #[strum(serialize = "DXT5A (DXT-compressed Intensity replicated to ARGB)")] Dxt5aIntensity = 25,
    #[strum(serialize = "Compressed Monochrome + Alpha")] CompressedMonoAlpha = 26,
    #[strum(serialize = "A4R4G4B4 (12-bit color + 4-bit alpha)")] A4r4g4b4 = 27,
    #[strum(serialize = "--- Color only formats ---")] SepColorOnly = 28,
    #[strum(serialize = "8-bit Monochrome")] Mono8 = 29,
    #[strum(serialize = "Compressed 24-bit Color")] Compressed24 = 30,
    #[strum(serialize = "32-bit Color (R11G11B10)")] R11g11b10 = 31,
    #[strum(serialize = "16-bit Monochrome")] Mono16 = 32,
    #[strum(serialize = "16-bit Red + Green Only")] Rg16 = 33,
    #[strum(serialize = "HALF Red Only")] HalfR = 34,
    #[strum(serialize = "FLOAT Red Only")] FloatR = 35,
    #[strum(serialize = "HALF Red + Green Only")] HalfRg = 36,
    #[strum(serialize = "FLOAT Red + Green Only")] FloatRg = 37,
    #[strum(serialize = "Compressed 4-bit Monochrome")] Compressed4Mono = 38,
    #[strum(serialize = "Compressed Interpolated Monochrome")] CompressedInterpMono = 39,
    #[strum(serialize = "unused12")] Unused12 = 40,
    #[strum(serialize = "--- Alpha only formats ---")] SepAlphaOnly = 41,
    #[strum(serialize = "DXT3A (4-bit Alpha)")] Dxt3aAlpha = 42,
    #[strum(serialize = "DXT5A (8-bit Compressed Alpha)")] Dxt5aAlpha = 43,
    #[strum(serialize = "8-bit Alpha")] Alpha8 = 44,
    #[strum(serialize = "unused13")] Unused13 = 45,
    #[strum(serialize = "unused14")] Unused14 = 46,
    #[strum(serialize = "unused15")] Unused15 = 47,
    #[strum(serialize = "--- Normal map formats ---")] SepNormal = 48,
    #[strum(serialize = "DXN Compressed Normals (better)")] Dxn = 49,
    #[strum(serialize = "CTX1 Compressed Normals (smaller)")] Ctx1 = 50,
    #[strum(serialize = "16-bit Normals")] Normals16 = 51,
    #[strum(serialize = "32-bit Normals")] Normals32 = 52,
}

// --- usage-override sub-block enums (import-tooling; `bitmap_usage_block`) ---

/// `bitmap_usage_flags_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum BitmapUsageFlags {
    #[strum(serialize = "Ignore Curve Override")] IgnoreCurveOverride = 0,
    #[strum(serialize = "Dont Allow Size Optimization")] DontAllowSizeOptimization = 1,
    #[strum(serialize = "Swap Axes")] SwapAxes = 2,
}

/// `bitmap_usage_slicer_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BitmapSlicer {
    #[default]
    #[strum(serialize = "Automatically Determine Slicer")] Automatic = 0,
    #[strum(serialize = "No Slicing (each source bitmap generates one element)")] NoSlicing = 1,
    #[strum(serialize = "Color Plate Slicer")] ColorPlate = 2,
    #[strum(serialize = "Cube Map Slicer")] CubeMap = 3,
}

/// `bitmap_usage_dicer_flags_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum BitmapDicerFlags {
    #[strum(serialize = "Convert Plate Color Key to Alpha Channel")] ColorKeyToAlpha = 0,
    #[strum(serialize = "Rotate Cube Map to Match DirectX Format")] RotateCubeMap = 1,
    #[strum(serialize = "Sprites- Shrink Elements to Smallest Non-Zero Alpha Region")] ShrinkToAlpha = 2,
    #[strum(serialize = "Sprites- Shrink Elements to Smallest Non-Zero Color And Alpha Region")] ShrinkToColorAlpha = 3,
    #[strum(serialize = "Unsigned -> Signed Scale and Bias")] UnsignedToSigned = 4,
}

/// `bitmap_usage_packer_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BitmapPacker {
    #[default]
    #[strum(serialize = "No packing")] None = 0,
    #[strum(serialize = "Sprite Pack (packs elements into as few bitmaps as possible)")] SpritePack = 1,
    #[strum(serialize = "Sprite Pack if needed (packs elements into as few bitmaps as possible)")] SpritePackIfNeeded = 2,
    #[strum(serialize = "3D Pack (packs elements into a 3D bitmap)")] Pack3d = 3,
}

/// `bitmap_usage_downsample_filter_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum BitmapDownsampleFilter {
    #[default]
    #[strum(serialize = "Point Sampled")] Point = 0,
    #[strum(serialize = "Box Filter")] Box = 1,
    #[strum(serialize = "Gaussian Filter")] Gaussian = 2,
}

/// `bitmap_usage_downsample_flags_def`.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum BitmapDownsampleFlags {
    #[strum(serialize = "Sprites - Color Bleed in Zero Alpha Regions")] SpriteColorBleed = 0,
    #[strum(serialize = "Pre-Multiply Alpha (before downsampling)")] PremultiplyAlpha = 1,
    #[strum(serialize = "Post-Divide Alpha (after downsampling)")] PostDivideAlpha = 2,
    #[strum(serialize = "Height Map - Convert to Bump Map")] HeightToBump = 3,
    #[strum(serialize = "Detail Map - Fade to Gray")] DetailFadeGray = 4,
    #[strum(serialize = "Signed -> Unsigned Scale and Bias")] SignedToUnsigned = 5,
    #[strum(serialize = "Illum Map - Fade to Black")] IllumFadeBlack = 6,
    #[strum(serialize = "ZBump - Scale by height and renormalize")] ZBumpScale = 7,
}

/// `bitmap_usage_swizzle_def` — per-channel source selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BitmapSwizzle {
    #[default]
    #[strum(serialize = "Default")] Default = 0,
    #[strum(serialize = "Source Red Channel")] SourceRed = 1,
    #[strum(serialize = "Source Green Channel")] SourceGreen = 2,
    #[strum(serialize = "Source Blue Channel")] SourceBlue = 3,
    #[strum(serialize = "Source Alpha Channel")] SourceAlpha = 4,
    #[strum(serialize = "Set to 1.0")] SetOne = 5,
    #[strum(serialize = "Set to 0.0")] SetZero = 6,
    #[strum(serialize = "Set to 0.5")] SetHalf = 7,
}
