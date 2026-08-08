//! Classic (Halo CE / Halo 2) loose-tag decoder + encoder.
//!
//! Gen-1/2 tags are *not* MCC self-describing containers: there is no
//! embedded `blay`/`tgly` layout stream and no `tgbl`/`tgst` chunking.
//! The body is flat data laid out depth-first, interpreted entirely by
//! an *external* field definition (synthesized here from a JSON schema
//! via [`crate::layout::TagLayout::from_json`], ported from the HABT
//! XML layouts).
//!
//! This module owns the classic *encoding*; it borrows the synthesized
//! layout only for structure + field types + nesting. The decoded form
//! is the same `TagBlockData`/`TagStructData` model the MCC reader
//! produces, so the entire downstream API + extractors work unchanged.
//!
//! ## On-disk shape (verified against real MCC CE/H2 bytes + HABT)
//! - 64-byte file header: `group_tag` at offset 36, `engine` word at
//!   offset 60. Every signature is read as a **u32**: CE stores them
//!   big-endian (`blam`), H2 little-endian (`!MLB` == `BLM!` reversed).
//! - The body (everything after the 64-byte header) begins with the
//!   root struct's fixed bytes (`sizeofValue`), followed by all variable
//!   data depth-first.
//! - **tag block** (Bungie's term — never "reflexive"): 12 inline bytes
//!   `count(i32) + pointer(i32) + pointer(i32)` (pointers are runtime
//!   garbage in loose tags). When `count>0`, child elements follow as
//!   `count * element_size` contiguous fixed bytes, *then* each element's
//!   own nested variable data in element order. Halo CE has **no**
//!   per-block tag-block header (that is H2+).
//! - **data reference**: 20 inline bytes (`size` + 4 runtime words),
//!   then `size` blob bytes trailing.
//! - **tag reference**: 16 inline bytes (`group` + ptr + `length` +
//!   tag_id); `group == -1` ⇒ null (no path), else `length+1` trailing
//!   bytes (path + NUL).
//! - **string**: 32 inline bytes, in place (no trailing).
//! - **string id** (H2 only): 4 inline bytes (`pad:u16` + `length:u16`,
//!   big-endian) then `length` trailing bytes.
//!
//! Decoder and encoder share one depth-first traversal and preserve
//! every byte verbatim (fixed bytes in `raw_data`, blobs/paths/children
//! in `sub_chunks`), so read→write is byte-exact by construction.

use crate::data::{TagBlockData, TagStructData, TagSubChunkContent, TagSubChunkEntry};
use crate::fields::TagFieldType;
use crate::file::{TagContainer, TagFile, TagFileHeader};
use crate::io::Endian;
use crate::layout::{TagFieldLayout, TagLayout};
use crate::stream::TagStream;

/// Which classic engine a tag belongs to. Selects signature byte order
/// and a family of per-field encoding quirks. The four Halo 2 variants
/// are distinguished by the offset-60 engine word (`ambl`/`LAMB`/`MLAB`/
/// `BLM!`, stored little-endian / reversed on disk) — each turns on a
/// different set of "legacy" read rules, matching HABT's
/// `HAS_LEGACY_{HEADER,STRINGS,PADDING}` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassicEngine {
    /// Halo 1 / Halo: Combat Evolved (Anniversary). Big-endian signature
    /// words, inline 32-byte strings, no string_id table.
    HaloCe,
    /// Halo 2 V1 (`ambl`). 12-byte block/struct headers, 32-byte inline
    /// `old_string_id`, `useless_pad` occupies its real length.
    Halo2V1,
    /// Halo 2 V2 (`LAMB`). 16-byte headers, 32-byte inline
    /// `old_string_id`, `useless_pad` occupies its real length.
    Halo2V2,
    /// Halo 2 V3 (`MLAB`). 16-byte headers, modern (4-byte + trailing)
    /// `old_string_id`, `useless_pad` occupies its real length.
    Halo2V3,
    /// Halo 2 V4 / latest (`BLM!`). The modern MCC form: 16-byte headers,
    /// 4-byte + trailing `old_string_id`, `useless_pad` is 0 bytes.
    Halo2V4,
}

impl ClassicEngine {
    /// Numeric body fields (ints/floats/counts/lengths) byte order.
    /// **CE is big-endian** (MCC's CE tag set is the CEA / Xbox-360
    /// PowerPC-derived form — HABT reads H1 with `file_endian=">"`).
    /// **H2 is little-endian** (x86).
    pub fn body_endian(self) -> Endian {
        match self {
            ClassicEngine::HaloCe => Endian::Be,
            _ => Endian::Le,
        }
    }

    /// Any of the four Halo 2 variants (i.e. not Halo CE).
    fn is_halo2(self) -> bool {
        !matches!(self, ClassicEngine::HaloCe)
    }

    /// Block/struct headers are the 12-byte legacy form (`4s2hi`:
    /// version + count are `i16`) instead of the 16-byte `4s3i` form.
    /// Only Halo 2 V1 (`ambl`).
    fn legacy_header(self) -> bool {
        matches!(self, ClassicEngine::Halo2V1)
    }

    /// `old_string_id` is a 32-byte inline null-terminated string (like a
    /// CE `string`) rather than a 4-byte length + trailing bytes. Halo 2
    /// V1/V2 (`ambl`/`LAMB`).
    fn legacy_strings(self) -> bool {
        matches!(self, ClassicEngine::Halo2V1 | ClassicEngine::Halo2V2)
    }

    /// `useless_pad` fields occupy their real `length` on disk rather than
    /// 0 bytes. Halo 2 V1/V2/V3 (`ambl`/`LAMB`/`MLAB`).
    fn legacy_padding(self) -> bool {
        matches!(
            self,
            ClassicEngine::Halo2V1 | ClassicEngine::Halo2V2 | ClassicEngine::Halo2V3
        )
    }
}

/// Errors raised while decoding a classic tag body.
#[derive(Debug)]
pub enum ClassicError {
    /// The file is shorter than the 64-byte classic header.
    ShortHeader,
    /// The offset-60 engine word isn't a recognized classic engine —
    /// the caller should route to the MCC reader instead.
    NotClassic,
    /// Ran off the end of the body while reading a fixed/variable region.
    UnexpectedEof {
        context: &'static str,
        need: usize,
        have: usize,
    },
    /// Body had trailing bytes the layout-driven walk never consumed.
    TrailingBytes { consumed: usize, total: usize },
    /// A block declared more element bytes than the body had left. Carries the
    /// block's name and the header's own `count`/`element_size`, because the
    /// product alone cannot say which of the two is wrong.
    ShortBlock {
        block: String,
        count: usize,
        elem_size: usize,
        need: usize,
        have: usize,
        /// Body offset the elements were to be read from. The value that makes
        /// this diagnosable: a plausible `count`/`elem_size` pair read a byte or
        /// two off yields an implausible one, so the offset says where to look.
        at: usize,
    },
    /// A block/struct header's `count`/`element_size` pair is implausible
    /// (count*size overflows, or a nonzero count with a zero element
    /// size). Almost always a cursor desync that read garbage as a
    /// header — caught here so a single tag fails cleanly instead of
    /// driving `Vec::with_capacity(count)` into a multi-gigabyte alloc.
    CorruptBlockHeader { count: usize, elem_size: usize },
}

impl std::fmt::Display for ClassicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClassicError::ShortHeader => write!(f, "file shorter than 64-byte classic header"),
            ClassicError::NotClassic => write!(f, "not a classic (CE/H2) tag header"),
            ClassicError::UnexpectedEof { context, need, have } => {
                write!(f, "unexpected EOF reading {context}: need {need} bytes, have {have}")
            }
            ClassicError::TrailingBytes { consumed, total } => {
                write!(f, "layout walk consumed {consumed} of {total} body bytes")
            }
            ClassicError::ShortBlock { block, count, elem_size, need, have, at } => write!(
                f,
                "block {block:?} at body offset {at} wants {count} x {elem_size} = {need} bytes but only {have} remain",
            ),
            ClassicError::CorruptBlockHeader { count, elem_size } => {
                write!(f, "corrupt block header: count={count} element_size={elem_size}")
            }
        }
    }
}

impl std::error::Error for ClassicError {}

/// The 64-byte classic tag-file header.
#[derive(Debug, Clone)]
pub struct ClassicHeader {
    /// Logical group tag (e.g. `b"bitm"`), un-reversed.
    pub group_tag: [u8; 4],
    /// Logical engine signature (e.g. `b"blam"`, `b"BLM!"`), un-reversed.
    pub engine: [u8; 4],
    /// Tag-file format version word at offset 56.
    pub version: u16,
    /// Stored body checksum (offset 40). `0xFFFFFFFF` is the
    /// "unchecksummed" sentinel some HEK tags ship with.
    pub checksum: u32,
}

impl ClassicHeader {
    /// Parse the 64-byte header and classify the engine from the offset-60
    /// signature word. Returns `None` if the signature isn't a known
    /// classic engine (so the caller can fall through to the MCC reader).
    pub fn parse(bytes: &[u8]) -> Option<(ClassicHeader, ClassicEngine)> {
        if bytes.len() < 64 {
            return None;
        }
        let raw_engine: [u8; 4] = bytes[60..64].try_into().ok()?;
        let raw_group: [u8; 4] = bytes[36..40].try_into().ok()?;

        // Read the engine word as a u32 in both orders and match against
        // the known classic engines. The matching order tells us how the
        // engine stores all its signature words.
        let be = u32::from_be_bytes(raw_engine);
        let le = u32::from_le_bytes(raw_engine);

        // CE: big-endian "blam". H2: little-endian — the on-disk word
        // reversed (== `le.to_be_bytes()`) gives the logical engine tag.
        // Each H2 logical tag selects a sub-version with its own legacy
        // read rules: ambl=V1, LAMB=V2, MLAB=V3, BLM!=V4 (latest/MCC).
        let (engine, group_tag, eng_le) = if be == u32::from_be_bytes(*b"blam") {
            (ClassicEngine::HaloCe, raw_group, false)
        } else if let Some(h2) = match &le.to_be_bytes() {
            b"ambl" => Some(ClassicEngine::Halo2V1),
            b"LAMB" => Some(ClassicEngine::Halo2V2),
            b"MLAB" => Some(ClassicEngine::Halo2V3),
            b"BLM!" => Some(ClassicEngine::Halo2V4),
            _ => None,
        } {
            // Reverse the on-disk (little-endian) bytes to get logical order.
            let mut g = raw_group;
            g.reverse();
            (h2, g, true)
        } else {
            return None;
        };

        let engine_logical = if eng_le {
            let mut e = raw_engine;
            e.reverse();
            e
        } else {
            raw_engine
        };

        // Version + checksum word byte order matches the engine's
        // signature order: CE big-endian, H2 little-endian.
        let vb = [bytes[56], bytes[57]];
        let cb: [u8; 4] = bytes[40..44].try_into().ok()?;
        let (version, checksum) = match engine {
            ClassicEngine::HaloCe => (u16::from_be_bytes(vb), u32::from_be_bytes(cb)),
            _ => (u16::from_le_bytes(vb), u32::from_le_bytes(cb)),
        };
        Some((
            ClassicHeader { group_tag, engine: engine_logical, version, checksum },
            engine,
        ))
    }
}

/// A forward-only cursor over the classic tag body.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Cursor { data, pos: 0 }
    }

    fn take(&mut self, n: usize, context: &'static str) -> Result<&'a [u8], ClassicError> {
        let end = self.pos.checked_add(n).ok_or(ClassicError::UnexpectedEof {
            context,
            need: n,
            have: self.data.len().saturating_sub(self.pos),
        })?;
        if end > self.data.len() {
            return Err(ClassicError::UnexpectedEof {
                context,
                need: n,
                have: self.data.len() - self.pos,
            });
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
}

fn rd_u32(raw: &[u8], off: usize, endian: Endian) -> u32 {
    let b: [u8; 4] = raw[off..off + 4].try_into().unwrap();
    match endian {
        Endian::Le => u32::from_le_bytes(b),
        Endian::Be => u32::from_be_bytes(b),
    }
}

fn rd_i32(raw: &[u8], off: usize, endian: Endian) -> i32 {
    rd_u32(raw, off, endian) as i32
}

/// Resolve a base (latest) struct index to the variant for on-disk
/// `version`, using the layout's per-base version table. Returns `base`
/// for single-version structs, an out-of-range version, or any engine
/// with an empty table (MCC / Halo CE). See
/// [`TagLayout::struct_version_table`].
fn resolve_version_variant(layout: &TagLayout, base: u32, version: u32) -> u32 {
    match layout.struct_version_table.get(base as usize) {
        Some(Some(v)) => v.get(version as usize).copied().unwrap_or(base),
        _ => base,
    }
}

/// Extract the `version` field from a 12/16-byte Halo 2 block/struct
/// header (little-endian): an `i32` at +4 for the modern 16-byte form, an
/// `i16` at +4 for the legacy 12-byte (`4s2hi`) form.
fn header_version(h: &[u8], engine: ClassicEngine) -> u32 {
    if engine.legacy_header() {
        i16::from_le_bytes([h[4], h[5]]) as u32
    } else {
        u32::from_le_bytes([h[4], h[5], h[6], h[7]])
    }
}

/// Inverse of [`resolve_version_variant`]: given a block's base struct and
/// the resolved variant its live elements use, find the on-disk version
/// index to stamp into a rebuilt H2 block header. Falls back to 0 (the
/// latest/base variant convention) when there's no version table or the
/// variant isn't found.
fn infer_struct_version(layout: &TagLayout, base: u32, resolved: u32) -> u32 {
    match layout.struct_version_table.get(base as usize) {
        Some(Some(versions)) => versions
            .iter()
            .position(|candidate| *candidate == resolved)
            .map(|index| index as u32)
            .unwrap_or(0),
        _ => 0,
    }
}

#[inline]
fn h2_classic_block_signature() -> [u8; 4] {
    *b"dfbt"
}

/// Build the Halo 2 inline-struct header for a freshly-created tagged
/// struct (one with no preserved `classic_struct_header`). Returns `None`
/// when no header should be written:
///   * non-Halo-2 engines (CE has no inline struct headers),
///   * untagged structs (`struct_tags == 0` — never headered on disk),
///   * a struct whose resolved variant equals the *headerless* default
///     (version 0 → `resolve_version_variant(base, 0)`): omitting the
///     header is exactly how that variant reads back, and writing one
///     would diverge from a byte-exact headerless round-trip.
/// Otherwise it stamps the `4cc + version + count(1) + size` header (16
/// bytes, or the legacy 12-byte `4s2hi` form) whose `version` selects
/// `child`'s variant via [`infer_struct_version`]. The 4cc is the base
/// struct's tag (the read path looks it up by base index too), written
/// little-endian to match on-disk byte order.
fn synthesize_h2_struct_header(
    layout: &TagLayout,
    base_si: u32,
    child: &TagStructData,
    engine: ClassicEngine,
) -> Option<Vec<u8>> {
    if !engine.is_halo2() {
        return None;
    }
    let tag = layout.struct_tags.get(base_si as usize).copied().unwrap_or(0);
    if tag == 0 {
        return None;
    }
    let resolved_si = child.struct_index;
    if resolved_si == resolve_version_variant(layout, base_si, 0) {
        return None;
    }
    let version = infer_struct_version(layout, base_si, resolved_si);
    let size = layout
        .struct_layouts
        .get(resolved_si as usize)
        .map(|s| s.size)
        .unwrap_or(0);
    let mut hdr = Vec::with_capacity(16);
    hdr.extend_from_slice(&tag.to_le_bytes());
    if engine.legacy_header() {
        hdr.extend_from_slice(&(version as i16).to_le_bytes());
        hdr.extend_from_slice(&1i16.to_le_bytes());
        hdr.extend_from_slice(&(size as u32).to_le_bytes());
    } else {
        hdr.extend_from_slice(&version.to_le_bytes());
        hdr.extend_from_slice(&1u32.to_le_bytes());
        hdr.extend_from_slice(&(size as u32).to_le_bytes());
    }
    Some(hdr)
}

/// The struct index to use when statically sizing an inline `<Struct>`
/// field. An UNTAGGED inline struct is always on-disk version 0 (HABT
/// picks `version==0`), so it resolves to its v0 variant. A TAGGED inline
/// struct (one that carries a block-style header, e.g. `MAPP`) selects
/// its version from that header at decode time — statically we use the
/// base (latest); the decoder/encoder override the size from the live
/// element's resolved `struct_index`.
fn inline_struct_static_index(layout: &TagLayout, base: u32) -> u32 {
    let tagged = layout.struct_tags.get(base as usize).copied().unwrap_or(0) != 0;
    if tagged {
        base
    } else {
        resolve_version_variant(layout, base, 0)
    }
}

/// On-disk size (bytes) of a single struct field for `engine`, using
/// classic packed layout (no alignment). Two field kinds change width
/// across the Halo 2 legacy variants, so the running offset has to be
/// computed per engine rather than read from the precomputed
/// `field.offset` (which always reflects the non-legacy form):
/// - `useless_pad`: its real `length` when legacy-padding, else 0.
/// - `old_string_id`: a 32-byte inline string when legacy-strings, else
///   the modern 4-byte length slot.
fn classic_field_size(layout: &TagLayout, field: &TagFieldLayout, engine: ClassicEngine) -> usize {
    match field.field_type {
        TagFieldType::Terminator => 0,
        TagFieldType::Struct => {
            classic_struct_size(layout, inline_struct_static_index(layout, field.definition), engine)
        }
        TagFieldType::Array => {
            // Array element structs are versionless on disk (always v0,
            // like an untagged inline struct).
            let a = &layout.array_layouts[field.definition as usize];
            let esi = resolve_version_variant(layout, a.struct_index, 0);
            classic_struct_size(layout, esi, engine) * a.count as usize
        }
        TagFieldType::Pad | TagFieldType::Skip => field.definition as usize,
        TagFieldType::UselessPad => {
            if engine.legacy_padding() {
                field.definition as usize
            } else {
                0
            }
        }
        TagFieldType::OldStringId => {
            if engine.legacy_strings() {
                32
            } else {
                4
            }
        }
        TagFieldType::Custom => field.definition as usize,
        _ => layout.field_types[field.type_index as usize].size as usize,
    }
}

/// On-disk size of a struct's fixed region for `engine` (sum of its
/// fields). For non-legacy engines this equals `struct_layouts[i].size`;
/// for legacy variants it grows by the `useless_pad` / `old_string_id`
/// deltas above. Field offsets within a struct are the running prefix
/// sums of [`classic_field_size`].
fn classic_struct_size(layout: &TagLayout, struct_index: u32, engine: ClassicEngine) -> usize {
    let mut size = 0;
    let mut fi = layout.struct_layouts[struct_index as usize].first_field_index as usize;
    loop {
        let f = &layout.fields[fi];
        if f.field_type == TagFieldType::Terminator {
            break;
        }
        size += classic_field_size(layout, f, engine);
        fi += 1;
    }
    size
}

/// Decode then re-encode a classic tag body, returning the re-encoded
/// bytes. The byte-exact roundtrip gate: `classic_roundtrip(body, ..) ==
/// body` for a well-formed tag. Keeps the internal
/// `TagBlockData` model crate-private.
pub fn classic_roundtrip(
    body: &[u8],
    layout: &TagLayout,
    engine: ClassicEngine,
) -> Result<Vec<u8>, ClassicError> {
    let root = read_classic_body(body, layout, engine)?;
    Ok(write_classic_body(layout, &root, engine))
}

/// Read a complete classic (Halo CE / H2) tag into a [`TagFile`], using
/// `layout` (synthesized from the group's JSON def) for structure. The
/// resulting `TagFile` behaves like any MCC-loaded tag — the same
/// navigation/inspect API, field readers, and extractors apply (field
/// byte order comes from the engine: CE big-endian, H2 little-endian).
///
/// Returns [`ClassicError::NotClassic`] if the offset-60 engine word
/// isn't a known classic engine (caller should route to the MCC reader).
pub fn read_classic_tag_file(bytes: &[u8], layout: TagLayout) -> Result<TagFile, ClassicError> {
    let (header, engine) = ClassicHeader::parse(bytes).ok_or(ClassicError::NotClassic)?;
    let body = &bytes[64..];
    let root = read_classic_body(body, &layout, engine)?;

    let file_header = TagFileHeader {
        pad: [0u8; 36],
        build_version: 0,
        build_number: 0,
        version: header.version as u32,
        group_tag: u32::from_be_bytes(header.group_tag),
        group_version: 0,
        // Preserve the original stored checksum; the writer recomputes
        // it unless it's the "unchecksummed" 0xFFFFFFFF sentinel.
        checksum: header.checksum,
        signature: u32::from_be_bytes(*b"BLAM"),
    };

    Ok(TagFile::from_parts(
        file_header,
        TagContainer::Classic { engine, header: bytes[0..64].to_vec() },
        engine.body_endian(),
        TagStream { layout, data: root },
    ))
}

/// CRC32 table (poly `0xEDB88320`). Built at compile time.
const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut r = i as u32;
        let mut j = 0;
        while j < 8 {
            r = if r & 1 == 1 { (r >> 1) ^ 0xEDB8_8320 } else { r >> 1 };
            j += 1;
        }
        t[i] = r;
        i += 1;
    }
    t
};

/// Classic tag checksum: CRC32 (poly `0xEDB88320`, init `0xFFFFFFFF`)
/// over the body, with **no final XOR inversion** (matches HABT
/// `checksum_calculate`). Verified against real CE tags.
pub fn classic_checksum(body: &[u8]) -> u32 {
    let mut c: u32 = 0xFFFF_FFFF;
    for &b in body {
        let idx = ((c ^ b as u32) & 0xFF) as usize;
        c = CRC_TABLE[idx] ^ (c >> 8);
    }
    c
}

/// Serialize a complete classic tag: the original 64-byte header
/// preserved verbatim (with only the checksum patched), followed by the
/// re-encoded flat body. The first 36 header bytes can carry per-tag
/// build strings, so reconstructing from scratch would lose them —
/// preserving keeps every byte while still recomputing the checksum.
///
/// The checksum is recomputed from the body, except the "unchecksummed"
/// `0xFFFFFFFF` sentinel (some HEK tags) is preserved so an unmodified
/// tag round-trips byte-exact.
pub(crate) fn write_classic_tag(file: &TagFile, engine: ClassicEngine, header: &[u8]) -> Vec<u8> {
    let body = write_classic_body(&file.tag_stream.layout, &file.tag_stream.data, engine);
    let checksum = if file.header.checksum == 0xFFFF_FFFF {
        0xFFFF_FFFF
    } else {
        classic_checksum(&body)
    };
    let checksum_bytes = match engine {
        ClassicEngine::HaloCe => checksum.to_be_bytes(),
        _ => checksum.to_le_bytes(),
    };

    let mut out = Vec::with_capacity(64 + body.len());
    out.extend_from_slice(header);
    out[40..44].copy_from_slice(&checksum_bytes);
    out.extend_from_slice(&body);
    out
}

/// Read a Halo 2 block header, little-endian. The modern (V2/V3/V4) form
/// is 16 bytes — `4cc name + version(i32) + count(i32) + size(i32)`. The
/// legacy V1 (`ambl`) form is 12 bytes — `4cc + version(i16) + count(i16)
/// + size(i32)`. The `size` is the element struct size (H2 blocks are
/// self-describing). Returns the raw header bytes (preserved for write)
/// plus the count + element size.
fn read_h2_block_header(
    cur: &mut Cursor,
    engine: ClassicEngine,
) -> Result<(Vec<u8>, usize, usize), ClassicError> {
    if engine.legacy_header() {
        let h = cur.take(12, "h2 block header (legacy)")?.to_vec();
        let count = i16::from_le_bytes([h[6], h[7]]) as usize;
        let size = u32::from_le_bytes([h[8], h[9], h[10], h[11]]) as usize;
        Ok((h, count, size))
    } else {
        let h = cur.take(16, "h2 block header")?.to_vec();
        let count = u32::from_le_bytes([h[8], h[9], h[10], h[11]]) as usize;
        let size = u32::from_le_bytes([h[12], h[13], h[14], h[15]]) as usize;
        Ok((h, count, size))
    }
}

/// Validate a block/struct header's `(count, element_size)` and return
/// the total fixed-region byte length, guarding against the allocation
/// blow-ups a desynced cursor can trigger. A garbage header may carry an
/// enormous `count` (the legacy 12-byte form reads it as an `i16`, so a
/// negative value sign-extends to ~1.8e19) and/or a zero `element_size`;
/// either lets the subsequent `cur.take` pass while
/// `Vec::with_capacity(count)` allocates gigabytes from one tag. We reject
/// a nonzero count with a zero element size and use a checked multiply;
/// the `take` that follows then bounds `count` by the remaining bytes.
fn checked_block_extent(count: usize, elem_size: usize) -> Result<usize, ClassicError> {
    if elem_size == 0 && count != 0 {
        return Err(ClassicError::CorruptBlockHeader { count, elem_size });
    }
    count
        .checked_mul(elem_size)
        .ok_or(ClassicError::CorruptBlockHeader { count, elem_size })
}

/// Halo 2 only: if struct `child_si` is tagged (e.g. `MAPP`) and the
/// next 4 trailing bytes match its tag (the 4cc is stored little-endian,
/// so an LE read equals the BE-packed tag), consume + return its 16-byte
/// block-style header. Otherwise no header is present.
fn read_h2_struct_header(
    layout: &TagLayout,
    child_si: u32,
    cur: &mut Cursor,
    engine: ClassicEngine,
) -> Result<Option<Vec<u8>>, ClassicError> {
    if !engine.is_halo2() {
        return Ok(None);
    }
    let tag = layout.struct_tags.get(child_si as usize).copied().unwrap_or(0);
    if tag == 0 {
        return Ok(None);
    }
    let rem = &cur.data[cur.pos..];
    if rem.len() >= 4 && u32::from_le_bytes([rem[0], rem[1], rem[2], rem[3]]) == tag {
        let n = if engine.legacy_header() { 12 } else { 16 };
        Ok(Some(cur.take(n, "h2 struct header")?.to_vec()))
    } else {
        Ok(None)
    }
}

/// Decode a classic tag body (everything after the 64-byte header) into
/// the root [`TagBlockData`] using `layout` for structure.
pub(crate) fn read_classic_body(
    body: &[u8],
    layout: &TagLayout,
    engine: ClassicEngine,
) -> Result<TagBlockData, ClassicError> {
    let endian = engine.body_endian();
    let mut cur = Cursor::new(body);

    let root_block_index = layout.header.tag_group_block_index;
    let struct_index = layout.block_layouts[root_block_index as usize].struct_index;

    // Halo 2: a block header (count + element size) leads the body.
    // Halo CE: headerless — the single root element leads directly.
    let (header, count, elem_size) = if engine.is_halo2() {
        let (h, c, s) = read_h2_block_header(&mut cur, engine)?;
        (Some(h), c, s)
    } else {
        (None, 1usize, layout.struct_layouts[struct_index as usize].size)
    };

    // The root block header's version selects the root struct FieldSet.
    let version = header.as_ref().map_or(0, |h| header_version(h, engine));
    let struct_index = resolve_version_variant(layout, struct_index, version);

    let total = checked_block_extent(count, elem_size)?;
    let raw_data = cur.take(total, "root struct").map(<[u8]>::to_vec)?;
    let mut elements = Vec::with_capacity(count);
    for i in 0..count {
        let elem_raw = raw_data[i * elem_size..(i + 1) * elem_size].to_vec();
        let sub = decode_struct_element(layout, struct_index, &elem_raw, &mut cur, engine, endian)?;
        elements.push(TagStructData { struct_index, sub_chunks: sub, classic_struct_header: None });
    }

    // Bytes after the structured body are appended sample/cache data that
    // no layout field references (e.g. multi-MB ambience-sound audio).
    // Both we and HABT read only the fixed structure (the trailing is
    // loaded by offset/cache, not via a tag field); preserve it verbatim
    // for a byte-exact round-trip. Halo CE has no such appendage, so any
    // CE trailing is a real desync and still errors.
    let classic_trailing = if cur.pos != body.len() {
        if !engine.is_halo2() {
            return Err(ClassicError::TrailingBytes { consumed: cur.pos, total: body.len() });
        }
        Some(body[cur.pos..].to_vec())
    } else {
        None
    };

    Ok(TagBlockData {
        block_index: root_block_index,
        flags: 0,
        raw_data,
        endian,
        elements,
        classic_block_header: header,
        classic_structural_dirty: false,
        classic_trailing,
    })
}

/// Decode one struct element: walk its fields from offset 0 of `raw`.
/// Thin wrapper over [`decode_struct_trailing`] for the block/root
/// callers (which pass a whole element's fixed bytes).
fn decode_struct_element(
    layout: &TagLayout,
    struct_index: u32,
    raw: &[u8],
    cur: &mut Cursor,
    engine: ClassicEngine,
    endian: Endian,
) -> Result<Vec<TagSubChunkEntry>, ClassicError> {
    let mut off = 0usize;
    decode_struct_trailing(layout, struct_index, raw, &mut off, cur, engine, endian)
}

/// Walk a struct's fields in order, pulling each field's trailing/variable
/// data from the cursor and building the `sub_chunks` list. `raw` is the
/// enclosing element's fixed bytes and `off` the running position into it,
/// shared with the parent: an inline `<Struct>` (and array elements) keep
/// reading from the SAME `raw`/`off`, so a tag'd struct whose on-disk
/// version makes it wider/narrower advances the parent's cursor by exactly
/// what it consumes — mirroring HABT's sequential read of the fixed-data
/// stream. The block header's element size stays authoritative for `raw`.
fn decode_struct_trailing(
    layout: &TagLayout,
    struct_index: u32,
    raw: &[u8],
    off: &mut usize,
    cur: &mut Cursor,
    engine: ClassicEngine,
    endian: Endian,
) -> Result<Vec<TagSubChunkEntry>, ClassicError> {
    let mut entries = Vec::new();
    let first = layout.struct_layouts[struct_index as usize].first_field_index as usize;
    let mut fi = first;
    loop {
        let field = &layout.fields[fi];
        if field.field_type == TagFieldType::Terminator {
            break;
        }
        // Truncated element: when an on-disk element is shorter than its
        // (newer/larger) layout, the trailing fields simply aren't present
        // — HABT clamps reads to the available bytes and treats the rest as
        // absent defaults (tag_interface.py:1487). Offsets only grow, so
        // once `*off` reaches the element end every remaining field is
        // absent: no fixed bytes (they aren't in `raw`/`raw_data` either,
        // so re-emitting `raw_data` stays byte-exact), no trailing data, no
        // sub-chunk. Stop here. (E.g. a v1 `model` element stored at the
        // older 208-byte size: fields past 208 carry no `default dialogue`
        // tag_reference, etc.)
        if *off >= raw.len() {
            break;
        }
        // Inline structs/arrays recurse on the shared `raw`/`off` and
        // advance it themselves; every other field reads `fsize` fixed
        // bytes at `*off` then advances past them.
        if field.field_type == TagFieldType::Struct {
            // A TAGGED inline struct carries a block-style header (4cc +
            // version + count + size) at the front of the trailing stream;
            // its version selects the FieldSet variant. Consume + preserve
            // it. An untagged inline struct is always version 0.
            let base_si = field.definition;
            let struct_header = read_h2_struct_header(layout, base_si, cur, engine)?;
            let version = struct_header.as_ref().map_or(0, |h| header_version(h, engine));
            let child_si = resolve_version_variant(layout, base_si, version);
            let child_sub =
                decode_struct_trailing(layout, child_si, raw, off, cur, engine, endian)?;
            entries.push(TagSubChunkEntry {
                field_index: Some(fi as u32),
                content: TagSubChunkContent::Struct(TagStructData {
                    struct_index: child_si,
                    sub_chunks: child_sub,
                    classic_struct_header: struct_header,
                }),
            });
            fi += 1;
            continue;
        }
        if field.field_type == TagFieldType::Array {
            let array = &layout.array_layouts[field.definition as usize];
            let count = array.count as usize;
            // Array element structs are versionless (always v0).
            let asi = resolve_version_variant(layout, array.struct_index, 0);
            let mut elems = Vec::with_capacity(count);
            for _ in 0..count {
                let sub = decode_struct_trailing(layout, asi, raw, off, cur, engine, endian)?;
                elems.push(TagStructData {
                    struct_index: asi,
                    sub_chunks: sub,
                    classic_struct_header: None,
                });
            }
            entries.push(TagSubChunkEntry {
                field_index: Some(fi as u32),
                content: TagSubChunkContent::Array(elems),
            });
            fi += 1;
            continue;
        }

        let fsize = classic_field_size(layout, field, engine);
        // A sub-chunk field needs its 4-byte inline slot (count / size /
        // length) fully present to know whether — and how much — trailing
        // data to read. At a truncated element's tail that slot can fall
        // partly past the end; like HABT's clamp, treat such a field as
        // absent (no sub-chunk, no trailing) and advance past its nominal
        // fixed bytes. Plain fields carry no sub-chunk and need no guard.
        if matches!(
            field.field_type,
            TagFieldType::Block | TagFieldType::Data | TagFieldType::TagReference | TagFieldType::StringId
        ) || (field.field_type == TagFieldType::OldStringId && !engine.legacy_strings())
        {
            if *off + 4 > raw.len() {
                *off += fsize;
                fi += 1;
                continue;
            }
        }
        match field.field_type {
            TagFieldType::Block => {
                let count = rd_u32(raw, *off, endian) as usize;
                let block = decode_block(layout, field.definition, count, cur, engine, endian)?;
                entries.push(TagSubChunkEntry {
                    field_index: Some(fi as u32),
                    content: TagSubChunkContent::Block(block),
                });
            }
            TagFieldType::Data => {
                let len = rd_u32(raw, *off, endian) as usize;
                let blob = cur.take(len, "data blob")?.to_vec();
                entries.push(TagSubChunkEntry {
                    field_index: Some(fi as u32),
                    content: TagSubChunkContent::Data(blob),
                });
            }
            TagFieldType::TagReference => {
                let group = rd_i32(raw, *off, endian);
                // The length is signed: H2 null references carry a valid
                // group but length -1 (CE used group == -1). Treat any
                // non-positive length as "no path". At a truncated tail the
                // group (off..off+4) can be present while the length word
                // (off+8..off+12) is past the end — then there's no path.
                let len_i = if *off + 12 <= raw.len() { rd_i32(raw, *off + 8, endian) } else { 0 };
                let len = if len_i > 0 { len_i as usize } else { 0 };
                // The MCC `TagReference` payload is `group_tag(4) +
                // null-terminated path`. The classic inline header keeps
                // the group (raw[off..off+4]); only `path + NUL` is
                // trailing. Prepend the inline group bytes so the payload
                // matches the MCC convention; the encoder strips them back.
                let mut payload = raw[*off..*off + 4].to_vec();
                if group != -1 && len != 0 {
                    payload.extend_from_slice(cur.take(len + 1, "tag_reference path")?);
                }
                entries.push(TagSubChunkEntry {
                    field_index: Some(fi as u32),
                    content: TagSubChunkContent::TagReference(payload),
                });
            }
            TagFieldType::StringId => {
                // Inline (pad:u16, length:u16) big-endian, then `length`
                // trailing string bytes.
                let len = u16::from_be_bytes([raw[*off + 2], raw[*off + 3]]) as usize;
                let s = cur.take(len, "string_id value")?.to_vec();
                entries.push(TagSubChunkEntry {
                    field_index: Some(fi as u32),
                    content: TagSubChunkContent::StringId(s),
                });
            }
            TagFieldType::OldStringId => {
                if engine.legacy_strings() {
                    // Legacy (ambl/LAMB): a 32-byte inline null-terminated
                    // string living entirely in the fixed bytes — no
                    // trailing data, no sub-chunk for a byte-exact round.
                } else {
                    let len = u16::from_be_bytes([raw[*off + 2], raw[*off + 3]]) as usize;
                    let s = cur.take(len, "old_string_id value")?.to_vec();
                    entries.push(TagSubChunkEntry {
                        field_index: Some(fi as u32),
                        content: TagSubChunkContent::OldStringId(s),
                    });
                }
            }
            // Everything else lives entirely in the fixed bytes (already
            // captured in `raw`) — nothing trailing to read.
            _ => {}
        }
        *off += fsize;
        fi += 1;
    }
    Ok(entries)
}

/// Decode a tag block's `count` elements: all element fixed bytes are
/// contiguous, then each element's nested variable data in element order.
fn decode_block(
    layout: &TagLayout,
    block_index: u32,
    inline_count: usize,
    cur: &mut Cursor,
    engine: ClassicEngine,
    endian: Endian,
) -> Result<TagBlockData, ClassicError> {
    let struct_index = layout.block_layouts[block_index as usize].struct_index;

    // An empty block has no on-disk presence (no H2 header, no elements).
    if inline_count == 0 {
        return Ok(TagBlockData {
            block_index,
            flags: 0,
            raw_data: Vec::new(),
            endian,
            elements: Vec::new(),
            classic_block_header: None,
            classic_structural_dirty: false,
            classic_trailing: None,
        });
    }

    // Halo 2: a block header (authoritative count + element size)
    // precedes the elements. Halo CE: headerless, size from layout.
    let (header, count, elem_size) = if engine.is_halo2() {
        let (h, c, s) = read_h2_block_header(cur, engine)?;
        (Some(h), c, s)
    } else {
        (None, inline_count, layout.struct_layouts[struct_index as usize].size)
    };

    // The block header's version selects which FieldSet variant the
    // elements use (older versions have a different on-disk layout).
    let version = header.as_ref().map_or(0, |h| header_version(h, engine));
    let struct_index = resolve_version_variant(layout, struct_index, version);

    let total = checked_block_extent(count, elem_size)?;
    // Name the block and quote the header's own numbers. A bare "need N bytes,
    // have M" cannot be acted on: N is `count * element_size`, so the interesting
    // question is always which of the two is wrong and for which block, and
    // recovering that from the product alone is guesswork.
    let elements_at = cur.pos;
    let raw_data = cur
        .take(total, "block elements")
        .map_err(|error| match error {
            ClassicError::UnexpectedEof { need, have, .. } => ClassicError::ShortBlock {
                block: layout
                    .get_string(layout.block_layouts[block_index as usize].name_offset)
                    .unwrap_or("?")
                    .to_owned(),
                count,
                elem_size,
                need,
                have,
                at: elements_at,
            },
            other => other,
        })?
        .to_vec();

    let mut elements = Vec::with_capacity(count);
    for i in 0..count {
        let elem_raw = raw_data[i * elem_size..(i + 1) * elem_size].to_vec();
        let sub = decode_struct_element(layout, struct_index, &elem_raw, cur, engine, endian)?;
        elements.push(TagStructData { struct_index, sub_chunks: sub, classic_struct_header: None });
    }

    Ok(TagBlockData {
        block_index,
        flags: 0,
        raw_data,
        endian,
        elements,
        classic_block_header: header,
        classic_structural_dirty: false,
        classic_trailing: None,
    })
}

/// Re-encode a classic tag body from the root [`TagBlockData`].
///
/// Inline counts/lengths (tag-block element count, data size, tag-ref
/// path length + group, string_id length) are **derived from the live
/// model** before each struct's fixed bytes are emitted — so structural
/// edits (add/remove block elements, resize data) serialize correctly.
/// The adjacent runtime-pointer bytes are left untouched. For an
/// unmodified tree this is a no-op, so read→write stays byte-exact.
pub(crate) fn write_classic_body(
    layout: &TagLayout,
    root: &TagBlockData,
    engine: ClassicEngine,
) -> Vec<u8> {
    let mut out = Vec::new();
    // The root is just a block (1 element) — emit it the same way,
    // including the Halo 2 block header if present.
    encode_block(layout, root, engine, &mut out);
    // Re-emit any appended sample/cache data captured past the structured
    // body (Halo 2 root only — preserved verbatim for byte-exactness).
    if let Some(trailing) = &root.classic_trailing {
        out.extend_from_slice(trailing);
    }
    out
}

#[inline]
fn wr_u32(raw: &mut [u8], off: usize, v: u32, endian: Endian) {
    let b = match endian {
        Endian::Le => v.to_le_bytes(),
        Endian::Be => v.to_be_bytes(),
    };
    raw[off..off + 4].copy_from_slice(&b);
}

/// Rewrite one element's inline count/length slots from the live model.
/// Thin wrapper over [`sync_fixed_counts`] for the block encoder.
fn sync_fixed_counts_element(
    layout: &TagLayout,
    raw: &mut [u8],
    elem: &TagStructData,
    engine: ClassicEngine,
    endian: Endian,
) {
    let mut off = 0usize;
    sync_fixed_counts(layout, raw, &mut off, elem, engine, endian);
}

/// Rewrite a struct's inline count/length slots from the live model,
/// recursing through inline structs/arrays on the shared `raw`/`off` (so a
/// version-resized inline struct advances the parent's cursor exactly as
/// the decoder did). Leaves runtime-pointer bytes intact.
fn sync_fixed_counts(
    layout: &TagLayout,
    raw: &mut [u8],
    off: &mut usize,
    elem: &TagStructData,
    engine: ClassicEngine,
    endian: Endian,
) {
    let first = layout.struct_layouts[elem.struct_index as usize].first_field_index as usize;
    let mut fi = first;
    loop {
        let field = &layout.fields[fi];
        if field.field_type == TagFieldType::Terminator {
            break;
        }
        // Mirror the decoder's truncation rule: once past the on-disk
        // element end, every remaining field is absent (no sub-chunk to
        // sync, no inline slot to write).
        if *off >= raw.len() {
            break;
        }
        let content = elem
            .sub_chunks
            .iter()
            .find(|e| e.field_index == Some(fi as u32))
            .map(|e| &e.content);
        // Inline structs/arrays recurse on the shared `raw`/`off`,
        // advancing it through the child's fields (its resolved
        // `struct_index` gives the right widths). Other fields write at
        // `*off` then step past their `fsize` fixed bytes.
        match (field.field_type, content) {
            (TagFieldType::Struct, Some(TagSubChunkContent::Struct(child))) => {
                sync_fixed_counts(layout, raw, off, child, engine, endian);
                fi += 1;
                continue;
            }
            (TagFieldType::Array, Some(TagSubChunkContent::Array(elems))) => {
                for child in elems.iter() {
                    sync_fixed_counts(layout, raw, off, child, engine, endian);
                }
                fi += 1;
                continue;
            }
            (TagFieldType::Block, Some(TagSubChunkContent::Block(b))) => {
                // count(i32) + 2 runtime pointers. On CE the inline count
                // is authoritative, so sync it. On unmodified H2 the element
                // count lives in the block's own trailing header (synced in
                // `encode_block`) and this inline word is runtime garbage —
                // preserve it verbatim (some tags, e.g. speedtree foliage,
                // store a non-count value here that must round-trip). Once a
                // block is structurally edited, the inline word must match
                // the trailing block we emit (in particular its zero/
                // non-zero state gates the empty-block decode), so sync it.
                if !engine.is_halo2() || b.classic_structural_dirty {
                    wr_u32(raw, *off, b.elements.len() as u32, endian);
                }
            }
            (TagFieldType::Data, Some(TagSubChunkContent::Data(blob))) => {
                // size(i32) + 4 runtime words; rewrite only the size.
                wr_u32(raw, *off, blob.len() as u32, endian);
            }
            (TagFieldType::TagReference, Some(TagSubChunkContent::TagReference(p))) => {
                // 16-byte inline header: group(4) + ptr(4) + length(4) +
                // tag_id(4). Payload is `group(4) + path + NUL`. Sync the
                // group + the path length (excluding the NUL).
                if p.len() >= 4 {
                    raw[*off..*off + 4].copy_from_slice(&p[0..4]);
                    // Only rewrite the length when a path is present; a null
                    // reference read from disk keeps its 4-byte (group-only)
                    // payload and its original length field (H2 stores -1
                    // there, not 0), so leave the inline word alone.
                    if p.len() > 4 {
                        let path_len = p.len() - 5; // minus 4 group + 1 NUL
                        wr_u32(raw, *off + 8, path_len as u32, endian);
                    }
                } else if *off + 12 <= raw.len() {
                    // Empty payload: the reference was *edited* to null
                    // (`TagReferenceData::to_bytes(None)` yields no bytes),
                    // unlike an originally-null ref whose decoded payload is
                    // the 4 group bytes. Neither the group nor the length was
                    // touched above, so the stale on-disk group + path length
                    // would survive while no trailing path is emitted — the
                    // decoder then tries to read a phantom path and hits EOF
                    // ("need N bytes, have M"), corrupting the saved tag.
                    // Write a canonical null: group = -1 and length = 0, which
                    // the decoder treats as NONE for both CE and H2.
                    raw[*off..*off + 4].copy_from_slice(&[0xFF; 4]);
                    wr_u32(raw, *off + 8, 0, endian);
                }
            }
            (
                TagFieldType::StringId | TagFieldType::OldStringId,
                Some(TagSubChunkContent::StringId(s) | TagSubChunkContent::OldStringId(s)),
            ) => {
                // Inline (pad:u16, length:u16) big-endian. Sync length.
                // (Legacy 32-byte inline old_string_id has no sub-chunk and
                // is preserved verbatim, so it never reaches this arm.)
                //
                // Both field types accept either content variant on purpose.
                // `encode_struct_trailing` already emits the bytes for either,
                // so requiring the variant to match the field type left the
                // inline length stale whenever a writer set a `string_id` value
                // on an `old_string_id` field — the trailing bytes were the new
                // string's while the length word was still the old string's, and
                // the decoder then read the following block header off by the
                // difference.
                raw[*off + 2..*off + 4].copy_from_slice(&(s.len() as u16).to_be_bytes());
            }
            _ => {}
        }
        *off += classic_field_size(layout, field, engine);
        fi += 1;
    }
}

fn encode_struct_trailing(
    layout: &TagLayout,
    elem: &TagStructData,
    engine: ClassicEngine,
    out: &mut Vec<u8>,
) {
    let first = layout.struct_layouts[elem.struct_index as usize].first_field_index as usize;
    let mut fi = first;
    loop {
        let field = &layout.fields[fi];
        if field.field_type == TagFieldType::Terminator {
            break;
        }
        if matches!(
            field.field_type,
            TagFieldType::Block
                | TagFieldType::Data
                | TagFieldType::TagReference
                | TagFieldType::StringId
                | TagFieldType::OldStringId
                | TagFieldType::Struct
                | TagFieldType::Array
        ) {
            let content = elem
                .sub_chunks
                .iter()
                .find(|e| e.field_index == Some(fi as u32))
                .map(|e| &e.content);
            if let Some(content) = content {
                match content {
                    TagSubChunkContent::Block(block) => encode_block(layout, block, engine, out),
                    TagSubChunkContent::Data(blob) => out.extend_from_slice(blob),
                    // Payload is `group_tag(4) + path + NUL`; the group
                    // already lives in raw_data, so only `path + NUL`
                    // (payload[4..]) is trailing on disk.
                    TagSubChunkContent::TagReference(p) => {
                        if p.len() > 4 {
                            out.extend_from_slice(&p[4..]);
                        }
                    }
                    TagSubChunkContent::StringId(s)
                    | TagSubChunkContent::OldStringId(s) => out.extend_from_slice(s),
                    TagSubChunkContent::Struct(child) => {
                        // H2 tag'd-struct header precedes the struct's
                        // trailing data. A struct read from disk carries its
                        // original header verbatim; a freshly-created one has
                        // none, so synthesize the header that selects its
                        // on-disk version variant (else the reader defaults to
                        // version 0 and resolves the wrong FieldSet — e.g. a
                        // new `mapping_function` decodes as `__v0`, corrupting
                        // the tag on the next open).
                        if let Some(hdr) = &child.classic_struct_header {
                            out.extend_from_slice(hdr);
                        } else if let Some(hdr) =
                            synthesize_h2_struct_header(layout, field.definition, child, engine)
                        {
                            out.extend_from_slice(&hdr);
                        }
                        encode_struct_trailing(layout, child, engine, out);
                    }
                    TagSubChunkContent::Array(elems) => {
                        for child in elems {
                            encode_struct_trailing(layout, child, engine, out);
                        }
                    }
                    _ => {}
                }
            }
        }
        fi += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::classic_checksum;

    #[test]
    fn checksum_matches_crc32_without_final_xor() {
        // Standard CRC32("123456789") = 0xCBF43926 (with final inversion).
        // The classic algorithm omits the final XOR, so the result is the
        // standard value XOR 0xFFFFFFFF.
        assert_eq!(classic_checksum(b"123456789"), 0xCBF4_3926 ^ 0xFFFF_FFFF);
        // Empty input returns the init value (no bytes processed).
        assert_eq!(classic_checksum(b""), 0xFFFF_FFFF);
    }
}

fn encode_block(layout: &TagLayout, block: &TagBlockData, engine: ClassicEngine, out: &mut Vec<u8>) {
    let elem_count = block.elements.len();
    // Use the on-disk element size (raw_data is preserved verbatim) so
    // the Halo 2 header's authoritative size is honoured even when the
    // synthesized layout size disagrees.
    let sz = if elem_count > 0 { block.raw_data.len() / elem_count } else { 0 };
    let mut raw = block.raw_data.clone();
    if sz > 0 {
        for (i, elem) in block.elements.iter().enumerate() {
            sync_fixed_counts_element(layout, &mut raw[i * sz..(i + 1) * sz], elem, engine, block.endian);
        }
    }
    // Halo 2: re-emit the block header, re-syncing the count from the live
    // element count. The modern 16-byte header stores count as an LE i32
    // at offset 8; the legacy 12-byte (`ambl`) header stores it as an LE
    // i16 at offset 6. An empty H2 block carries no trailing header/data,
    // even if a now-deleted edit left a synthesized header behind.
    if engine.is_halo2() && elem_count == 0 {
        // Empty H2 blocks carry no trailing header/data.
    } else if let Some(hdr) = canonical_h2_block_header(layout, block, elem_count, sz, engine) {
        out.extend_from_slice(&hdr);
    }
    out.extend_from_slice(&raw);
    for elem in &block.elements {
        encode_struct_trailing(layout, elem, engine, out);
    }
}

/// Produce the on-disk block header bytes for `block`. For unmodified
/// blocks (or any non-H2 engine) the original header is preserved and only
/// the count word re-synced. For a structurally edited H2 block the 16-byte
/// header is rebuilt from scratch (`dfbt` + inferred version + count +
/// on-disk element size) so it matches the body we emit; the legacy 12-byte
/// form only carries a count, so it is just re-synced. Returns `None` when
/// the block has no header (e.g. MCC/CE, or an empty H2 block).
fn canonical_h2_block_header(
    layout: &TagLayout,
    block: &TagBlockData,
    elem_count: usize,
    sz: usize,
    engine: ClassicEngine,
) -> Option<Vec<u8>> {
    let hdr = block.classic_block_header.as_ref()?;
    let mut out = hdr.clone();
    if !engine.is_halo2() || !block.classic_structural_dirty {
        if out.len() == 12 {
            out[6..8].copy_from_slice(&(elem_count as u16).to_le_bytes());
        } else {
            out[8..12].copy_from_slice(&(elem_count as u32).to_le_bytes());
        }
        return Some(out);
    }

    if out.len() == 12 {
        out[6..8].copy_from_slice(&(elem_count as u16).to_le_bytes());
        return Some(out);
    }

    let base_struct_index = layout.block_layouts[block.block_index as usize].struct_index;
    let resolved_struct_index = block
        .elements
        .first()
        .map(|element| element.struct_index)
        .unwrap_or(base_struct_index);
    let version = infer_struct_version(layout, base_struct_index, resolved_struct_index);
    out[0..4].copy_from_slice(&h2_classic_block_signature());
    out[4..8].copy_from_slice(&version.to_le_bytes());
    out[8..12].copy_from_slice(&(elem_count as u32).to_le_bytes());
    out[12..16].copy_from_slice(&(sz as u32).to_le_bytes());
    Some(out)
}
