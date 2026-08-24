//! Parser for the `tgxc` pageable-resource xsync state.
//!
//! The xsync state is a small metadata blob inline in the tag that
//! describes where to find the resource's real payload in the
//! `cache_N` partition file. Layout for Halo 4 (v3):
//!
//! ```text
//! tgxc chunk payload:
//! ├── XSyncStateHeader (44 bytes for v3, BE)
//! │   unknown1, unknown2 (8 bytes — v3-only prefix)
//! │   cache_location_offset, cache_location_size,
//! │   optional_location_offset, optional_location_size,
//! │   control_alignment_bits, control_data_size,
//! │   control_fixup_count, interop_usage_count,
//! │   root_address (8 bytes: type+offset)
//! └── chunked body (xsrc / inrc / inus / ctrl / data / page / opti)
//! ```
//!
//! For just resolving the primary-data byte range in `cache_N`, we
//! only need the cache_location offset / size from the header — the
//! chunked body is required to materialize the full control data
//! with fixups applied (deferred to a later phase). The version-0
//! header (without the 8-byte prefix) is also defined here for
//! completeness, even though our corpus is exclusively v3.

use crate::error::TagReadError;

/// 8-byte fixup record from the `ctrl` chunk: patches a 4-byte
/// address in the control data so that what was a stub becomes a
/// valid offset (into the control data or primary/optional buffer).
///
/// On disk: `block_offset` (BE u32) followed by `address` (BE u32).
/// The high 2 bits of `address` carry the tier hint
/// ([`FixupAddress::tier`]); the low 30 bits are the byte offset
/// within that tier.
#[derive(Debug, Clone, Copy)]
pub struct ControlFixup {
    /// Byte offset within the control_data where the address word
    /// gets written.
    pub block_offset: u32,
    /// Packed `(tier << 30) | offset` value to write at that offset.
    pub address: FixupAddress,
}

/// A fixed-up address inside the resource control data. The top 3
/// bits select the buffer tier ([`FixupTier`]), the low 29 bits are
/// the offset within that tier. Mirrors TagTool's `CacheAddress`
/// (`TypeShift = 29`).
#[derive(Debug, Clone, Copy)]
pub struct FixupAddress(pub u32);

impl FixupAddress {
    /// Which tier this address points into. See [`FixupTier`].
    pub fn tier(self) -> FixupTier {
        match self.0 >> 29 {
            0 => FixupTier::Memory,
            1 => FixupTier::Control,
            2 => FixupTier::Primary,
            3 => FixupTier::Secondary,
            4 => FixupTier::Tertiary,
            _ => FixupTier::Unknown,
        }
    }

    /// Byte offset within the tier's buffer (low 29 bits).
    pub fn offset(self) -> u32 {
        self.0 & 0x1FFFFFFF
    }

    /// `true` for the null sentinel `(Memory, offset=0)` written when
    /// the resource emitter wants a stub.
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

/// Which buffer a fixed-up address points into. Matches TagTool's
/// `CacheAddressType` enum order; the top 3 bits of the packed
/// address word select among these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixupTier {
    /// `0` — runtime memory pointer. On serialized data this is the
    /// null sentinel (offset always `0`).
    Memory,
    /// `1` — offset within the control_data itself.
    Control,
    /// `2` — offset within the primary (always-resident) buffer.
    Primary,
    /// `3` — offset within the secondary (high-res / optional)
    /// buffer.
    Secondary,
    /// `4` — Gen4 tertiary buffer. Not observed in Halo 4 X360 but
    /// preserved for forward compatibility.
    Tertiary,
    /// Any other tier value — leave room for unobserved encodings.
    Unknown,
}

/// Fully parsed xsync state.
///
/// The chunked body is walked eagerly into [`Self::control_data`] +
/// [`Self::control_fixups`] + [`Self::interop_guids`] +
/// [`Self::pageable_fixups`] / [`Self::optional_fixups`]; the raw
/// [`Self::body`] is also kept for round-trip / debug purposes.
#[derive(Debug, Clone)]
pub struct XSyncState {
    pub header: XSyncStateHeader,
    /// Version flag — 3 for Halo 4 monolithic caches, 0 for the
    /// older shape. Drives header layout selection.
    pub version: u32,
    /// Bytes following the header — the chunked sub-section. Held
    /// verbatim for round-trip; parsed fields below come from it.
    pub body: Vec<u8>,
    /// Content of the body's `data` chunk — the in-memory dump of
    /// the resource's defining struct. Apply
    /// [`Self::control_fixups`] before walking. Empty when the
    /// resource has no control data (e.g. paged-only bitmaps).
    pub control_data: Vec<u8>,
    /// Address-fixup records from the `ctrl` chunk. Applied to
    /// `control_data` in [`Self::apply_control_fixups`].
    pub control_fixups: Vec<ControlFixup>,
    /// Address-fixup records from the `page` chunk — applied when
    /// resolving primary-buffer references at higher layers.
    pub pageable_fixups: Vec<ControlFixup>,
    /// Address-fixup records from the `opti` chunk — applied when
    /// resolving secondary-buffer references at higher layers.
    pub optional_fixups: Vec<ControlFixup>,
    /// 16-byte interop GUIDs from the `inus` chunk — one per
    /// runtime interop pointer the resource references. Order
    /// matches the appearance order of `tag_interop` fields in the
    /// resource's defining struct.
    pub interop_guids: Vec<[u8; 16]>,
}

/// 36 (v0) or 44 (v3) byte header at the start of every `tgxc`
/// payload. Layout = 9 × BE u32.
///
/// **The two regions.** BCS labels these four words as two `(offset, size)`
/// pairs (`cache_location_*` and `optional_location_*`), and each pair is one
/// region: its own offset with its own size.
///
/// - The **primary** (always-resident, e.g. a bitmap's mip chain) buffer starts
///   at `cache_location_offset` — `0` in observed corpora — and is
///   `cache_location_size` bytes long.
/// - The **secondary** (high-res, e.g. a bitmap's base level) buffer starts at
///   `optional_location_offset` and is `optional_location_size` bytes long.
///   Both are `0` on a resource that has no second region, which is every
///   geometry resource and every bitmap with no mips.
///
/// The cache_block's physical layout is
/// `[primary][alignment padding][secondary]`.
///
/// This was read the other way round until 2026-08-24 — each offset paired with
/// the other name's size. On a resource with only one region the two readings
/// agree, which is why it survived: it is exactly the mipless bitmaps and the
/// geometry that were being checked. A mipped bitmap read that way lands both
/// slices in the wrong region and every level of it comes out scrambled.
#[derive(Debug, Clone, Copy)]
pub struct XSyncStateHeader {
    /// Byte offset of the **primary** (always-resident) buffer inside the
    /// tag's cache block. Observed value is `0`. Pair with
    /// [`Self::cache_location_size`] — each named pair is one region, its own
    /// offset with its own size.
    pub cache_location_offset: u32,
    /// Byte length of the **primary** (always-resident) buffer. For a bitmap
    /// with mips this is the smaller levels; for geometry it is everything.
    pub cache_location_size: u32,
    /// Byte offset of the **secondary** (high-res) buffer inside the same
    /// cache block, which follows the primary one after alignment padding.
    /// `0` when there is no secondary buffer.
    pub optional_location_offset: u32,
    /// Byte length of the **secondary** (high-res) buffer — a bitmap's base
    /// level. `0` when there's no high-res streaming buffer for this resource.
    pub optional_location_size: u32,
    /// Alignment hint for control data — opaque, preserved for
    /// round-trip.
    pub control_alignment_bits: i32,
    /// Length of the inline `data`-chunk control data following in
    /// the chunked body.
    pub control_data_size: i32,
    /// Number of `ctrl`-chunk pointer-fixup records.
    pub control_fixup_count: i32,
    /// Number of `inus`-chunk 16-byte interop GUIDs.
    pub interop_usage_count: i32,
    /// Packed root address (type + offset). Used by the full
    /// control-data deserializer; opaque for primary-data extraction.
    pub root_address: u32,
}

impl XSyncState {
    /// Parse the xsync state from a `tgxc` chunk's content bytes.
    /// `version` is the `tgxc` chunk-header version field (0 or 3).
    pub fn parse(payload: &[u8], version: u32) -> Result<Self, TagReadError> {
        match version {
            3 => Self::parse_v3(payload),
            // v0 has the same header minus the 8-byte unknown prefix.
            // Our X360 corpus is exclusively v3, but we accept v0
            // defensively in case a different build uses it.
            _ => Self::parse_v0(payload),
        }
    }

    fn parse_v3(payload: &[u8]) -> Result<Self, TagReadError> {
        if payload.len() < 44 {
            return Err(TagReadError::ChunkSizeMismatch {
                chunk: "tgxc v3 header",
                started_at: 0,
                ended_at: payload.len() as u64,
                expected_end: 44,
            });
        }
        // Skip the 8-byte v3-only unknown prefix.
        let header = read_header_at(&payload[8..])?;
        let body = payload[44..].to_vec();
        let parsed = parse_body(&body, &header);
        Ok(Self {
            header,
            version: 3,
            body,
            control_data: parsed.control_data,
            control_fixups: parsed.control_fixups,
            pageable_fixups: parsed.pageable_fixups,
            optional_fixups: parsed.optional_fixups,
            interop_guids: parsed.interop_guids,
        })
    }

    fn parse_v0(payload: &[u8]) -> Result<Self, TagReadError> {
        if payload.len() < 36 {
            return Err(TagReadError::ChunkSizeMismatch {
                chunk: "tgxc v0 header",
                started_at: 0,
                ended_at: payload.len() as u64,
                expected_end: 36,
            });
        }
        let header = read_header_at(payload)?;
        let body = payload[36..].to_vec();
        let parsed = parse_body(&body, &header);
        Ok(Self {
            header,
            version: 0,
            body,
            control_data: parsed.control_data,
            control_fixups: parsed.control_fixups,
            pageable_fixups: parsed.pageable_fixups,
            optional_fixups: parsed.optional_fixups,
            interop_guids: parsed.interop_guids,
        })
    }

    /// Return a fixed-up copy of the control data — the `data`
    /// chunk's bytes with every `control_fixups[i]` patched in.
    ///
    /// After this, addresses inside the control data are valid as
    /// [`FixupAddress`]es: each was originally a stub (zero), and is
    /// now `(tier << 30) | offset`. Callers should pair the result
    /// with `header.root_address` and the cache-block primary /
    /// secondary buffers when walking the resource struct.
    pub fn apply_control_fixups(&self) -> Vec<u8> {
        let mut out = self.control_data.clone();
        for fixup in &self.control_fixups {
            let off = fixup.block_offset as usize;
            if off + 4 > out.len() {
                continue;
            }
            // Engine bytes are big-endian on Xenos.
            let bytes = fixup.address.0.to_be_bytes();
            out[off..off + 4].copy_from_slice(&bytes);
        }
        out
    }
}

#[derive(Default)]
struct ParsedBody {
    control_data: Vec<u8>,
    control_fixups: Vec<ControlFixup>,
    pageable_fixups: Vec<ControlFixup>,
    optional_fixups: Vec<ControlFixup>,
    interop_guids: Vec<[u8; 16]>,
}

/// Walk the chunked body and pluck out the few chunks we care about.
/// Mirrors TagTool's `TagResourceXSyncState.ReadChunks` — container
/// chunks (`xsrc`, `inrc`) recurse; leaves populate the output.
/// Unknown chunks are skipped silently to leave room for future
/// platforms.
fn parse_body(body: &[u8], header: &XSyncStateHeader) -> ParsedBody {
    let mut out = ParsedBody::default();
    walk_body(body, header, &mut out);
    out
}

fn walk_body(body: &[u8], header: &XSyncStateHeader, out: &mut ParsedBody) {
    let mut cursor = 0usize;
    while cursor + 12 <= body.len() {
        let sig = [body[cursor], body[cursor + 1], body[cursor + 2], body[cursor + 3]];
        let _version =
            u32::from_be_bytes([body[cursor + 4], body[cursor + 5], body[cursor + 6], body[cursor + 7]]);
        let size = u32::from_be_bytes([
            body[cursor + 8], body[cursor + 9], body[cursor + 10], body[cursor + 11],
        ]) as usize;
        let content_start = cursor + 12;
        let content_end = content_start.saturating_add(size).min(body.len());
        let content = &body[content_start..content_end];

        match &sig {
            // Container chunks — walk their inner content recursively.
            b"xsrc" | b"inrc" => walk_body(content, header, out),

            b"inus" => {
                let n = header.interop_usage_count.max(0) as usize;
                for i in 0..n {
                    let start = i * 16;
                    if start + 16 > content.len() {
                        break;
                    }
                    let mut guid = [0u8; 16];
                    guid.copy_from_slice(&content[start..start + 16]);
                    out.interop_guids.push(guid);
                }
            }

            b"ctrl" => {
                let n = header.control_fixup_count.max(0) as usize;
                out.control_fixups = read_fixups(content, n);
            }

            b"data" => {
                let n = header.control_data_size.max(0) as usize;
                out.control_data = content.get(..n).unwrap_or(content).to_vec();
            }

            b"page" => {
                out.pageable_fixups = read_fixups(content, content.len() / 8);
            }

            b"opti" => {
                out.optional_fixups = read_fixups(content, content.len() / 8);
            }

            _ => {} // Unknown chunk type — skip.
        }

        cursor = content_end;
    }
}

fn read_fixups(bytes: &[u8], count: usize) -> Vec<ControlFixup> {
    (0..count)
        .filter_map(|i| {
            let off = i * 8;
            if off + 8 > bytes.len() {
                return None;
            }
            let block_offset = u32::from_be_bytes([
                bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3],
            ]);
            let address = u32::from_be_bytes([
                bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7],
            ]);
            Some(ControlFixup { block_offset, address: FixupAddress(address) })
        })
        .collect()
}

fn read_header_at(bytes: &[u8]) -> Result<XSyncStateHeader, TagReadError> {
    if bytes.len() < 36 {
        return Err(TagReadError::ChunkSizeMismatch {
            chunk: "tgxc header body",
            started_at: 0,
            ended_at: bytes.len() as u64,
            expected_end: 36,
        });
    }
    Ok(XSyncStateHeader {
        cache_location_offset: be_u32(bytes, 0),
        cache_location_size: be_u32(bytes, 4),
        optional_location_offset: be_u32(bytes, 8),
        optional_location_size: be_u32(bytes, 12),
        control_alignment_bits: be_u32(bytes, 16) as i32,
        control_data_size: be_u32(bytes, 20) as i32,
        control_fixup_count: be_u32(bytes, 24) as i32,
        interop_usage_count: be_u32(bytes, 28) as i32,
        root_address: be_u32(bytes, 32),
    })
}

#[inline]
fn be_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
