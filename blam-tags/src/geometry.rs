//! Geometry primitives shared across format-specific exporters
//! ([`crate::jms`], [`crate::ass`], future ones). All items are
//! `pub(crate)` — these are extraction-pipeline internals, not the
//! crate's public API.
//!
//! Categories:
//! - **Compression bounds**: dequantize bounds-compressed positions
//!   and texcoords from `render geometry/compression info[i]`. The
//!   six bounds floats are packed across two `real_point_3d` fields
//!   as the sequential tuple `[xmin, xmax, ymin, ymax, zmin, zmax]`
//!   (NOT min/max corners as the field type suggests).
//! - **Strip → list conversion**: restart-aware (`0xFFFF` sentinel)
//!   triangle-strip decoder with parity-flip windings + degenerate
//!   filtering. Matches TagTool's `ReadTriangleStrip` exactly.
//! - **BSP edge-ring walker**: shared between `collision_model` and
//!   `scenario_structure_bsp` (both have the same Halo BSP shape —
//!   surfaces walk an edge ring, each edge belongs to two surfaces,
//!   matching side decides start-vs-end vertex emission).
//!
//! Vector / quaternion / point math is now expressed via inherent
//! methods + `Ops` impls on the [`crate::math`] types
//! ([`crate::math::RealVector3d`], [`crate::math::RealPoint3d`],
//! [`crate::math::RealQuaternion`], [`crate::math::RealPlane3d`]).
//! Use those.
//!
//! World-units → JMS/ASS centimeter scale factor `SCALE` also
//! lives here so both format modules use the same value.

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::math::{RealPoint2d, RealPoint3d};

/// World-units → centimeter scale factor used by JMS / ASS export
/// (`position * SCALE` everywhere positions cross into the artist
/// source format).
pub(crate) const SCALE: f32 = 100.0;

/// The inverse: JMS / ASS centimeters → world units, for the importers.
pub const JMS_TO_WORLD: f32 = 1.0 / SCALE;

//================================================================================
// CompressionBounds
//================================================================================

/// Per-axis dequantization bounds for a `compression info[i]` entry.
/// Position and texcoord components are stored as 0..1 normalized
/// values; multiplying by `(max - min)` and adding `min` recovers
/// the original world / uv coordinate.
#[derive(Debug, Clone, Copy)]
pub struct CompressionBounds {
    // Default impl below delegates to `identity()` so callers (like
    // `RenderModel` which has `#[derive(Default)]`) compile.
    /// `true` if positions in this group were quantized — when `false`,
    /// [`Self::decompress_position`] is a passthrough.
    pub pos_compressed: bool,
    /// `true` if texcoords were quantized.
    pub uv_compressed: bool,
    pub px_min: f32, pub px_max: f32,
    pub py_min: f32, pub py_max: f32,
    pub pz_min: f32, pub pz_max: f32,
    pub u_min: f32, pub u_max: f32,
    pub v_min: f32, pub v_max: f32,
}

impl Default for CompressionBounds {
    fn default() -> Self { Self::identity() }
}

impl CompressionBounds {
    /// Identity bounds — `decompress_*` returns its input unchanged.
    /// Used for cluster meshes (already in world units) and as a
    /// fallback when the `compression info` index is out of range.
    pub fn identity() -> Self {
        Self {
            pos_compressed: false, uv_compressed: false,
            px_min: 0.0, px_max: 1.0, py_min: 0.0, py_max: 1.0, pz_min: 0.0, pz_max: 1.0,
            u_min: 0.0, u_max: 1.0, v_min: 0.0, v_max: 1.0,
        }
    }

    /// Map a 0..1 quantized position back into world units.
    /// Passthrough when [`Self::pos_compressed`] is `false`.
    pub fn decompress_position(&self, p: RealPoint3d) -> RealPoint3d {
        if !self.pos_compressed { return p; }
        RealPoint3d {
            x: self.px_min + p.x * (self.px_max - self.px_min),
            y: self.py_min + p.y * (self.py_max - self.py_min),
            z: self.pz_min + p.z * (self.pz_max - self.pz_min),
        }
    }

    /// Map a 0..1 quantized texcoord back into uv units.
    /// Passthrough when [`Self::uv_compressed`] is `false`.
    pub fn decompress_texcoord(&self, uv: RealPoint2d) -> RealPoint2d {
        if !self.uv_compressed { return uv; }
        RealPoint2d {
            x: self.u_min + uv.x * (self.u_max - self.u_min),
            y: self.v_min + uv.y * (self.v_max - self.v_min),
        }
    }
}

/// Read `render geometry/compression info[0]`. For `render_model`
/// and sbsp clusters which share the global bounds.
pub fn read_compression_bounds(root: &TagStruct<'_>) -> CompressionBounds {
    read_compression_bounds_at(root, 0)
}

/// Read `render geometry/compression info[index]`. sbsp's instance
/// definitions carry per-definition `compression index` since each
/// instanced geometry has its own bounds. Falls back to identity if
/// the index is out of range.
pub fn read_compression_bounds_at(root: &TagStruct<'_>, index: usize) -> CompressionBounds {
    let Some(ci_block) = root.field_path("render geometry/compression info").and_then(|f| f.as_block())
        else { return CompressionBounds::identity(); };
    if index >= ci_block.len() { return CompressionBounds::identity(); }
    let ci = ci_block.element(index).unwrap();
    let mut pos_compressed = true;
    let mut uv_compressed = true;
    if let Some(TagFieldData::WordFlags { value, .. }) = ci.field("compression flags").and_then(|f| f.value()) {
        pos_compressed = (value & 0x0001) != 0;
        uv_compressed = (value & 0x0002) != 0;
    }
    // Six floats packed as the sequential tuple
    // `[xmin, xmax, ymin, ymax, zmin, zmax]` across two
    // `real_point_3d` fields. Despite the field type, this is NOT a
    // min/max corner pair.
    let pb0 = ci.read_point3d("position bounds 0");
    let pb1 = ci.read_point3d("position bounds 1");
    let tb0 = match ci.field("texcoord bounds 0").and_then(|f| f.value()) {
        Some(TagFieldData::RealPoint2d(p)) => p, _ => RealPoint2d { x: 0.0, y: 1.0 },
    };
    let tb1 = match ci.field("texcoord bounds 1").and_then(|f| f.value()) {
        Some(TagFieldData::RealPoint2d(p)) => p, _ => RealPoint2d { x: 0.0, y: 1.0 },
    };
    CompressionBounds {
        pos_compressed, uv_compressed,
        px_min: pb0.x, px_max: pb0.y,
        py_min: pb0.z, py_max: pb1.x,
        pz_min: pb1.y, pz_max: pb1.z,
        u_min: tb0.x, u_max: tb0.y,
        v_min: tb1.x, v_max: tb1.y,
    }
}

//================================================================================
// Triangle-strip → list
//================================================================================

/// Restart-aware (`0xFFFF` sentinel) triangle-strip decoder. Splits
/// the strip on restart sentinels, then within each sub-strip flips
/// winding parity per local position and drops degenerate windows
/// (any two indices equal — these are splice triangles used to stitch
/// strip pieces together).
pub fn strip_to_list(strip: &[u16]) -> Vec<(u16, u16, u16)> {
    let mut out = Vec::with_capacity(strip.len().saturating_sub(2));
    for segment in strip.split(|&x| x == 0xFFFF) {
        for i in 0..segment.len().saturating_sub(2) {
            let (a, b, c) = (segment[i], segment[i + 1], segment[i + 2]);
            if a == b || b == c || a == c { continue; }
            if i % 2 == 0 { out.push((a, b, c)); }
            else          { out.push((a, c, b)); }
        }
    }
    out
}

/// u32 sibling of [`strip_to_list`] — restart sentinel is `0xFFFFFFFF`
/// instead of `0xFFFF`. Needed for meshes that index >65k vertices via
/// the `raw indices32` block (e.g. H4 X360 monolithic placeholders
/// like `bigmuthafucka`). Same semantics: restart-aware, drops
/// degenerate windows, flips winding parity per local position.
pub fn strip_to_list_u32(strip: &[u32]) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::with_capacity(strip.len().saturating_sub(2));
    for segment in strip.split(|&x| x == 0xFFFFFFFF) {
        for i in 0..segment.len().saturating_sub(2) {
            let (a, b, c) = (segment[i], segment[i + 1], segment[i + 2]);
            if a == b || b == c || a == c { continue; }
            if i % 2 == 0 { out.push((a, b, c)); }
            else          { out.push((a, c, b)); }
        }
    }
    out
}

/// A tag reference's name: the last segment of its path (either separator).
/// `None` when that is empty — a null reference, or a path ending in a
/// separator.
///
/// Not `Path::file_stem`: a reference path carries no extension, so a stem
/// would cut a tag named `probe. multipurpose` down to `probe`.
pub(crate) fn tag_path_basename(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/']).next().filter(|name| !name.is_empty())
}

//================================================================================
// Mesh indices
//================================================================================

/// A mesh's index buffer from its `per mesh temporary` element, widened to
/// `u32`: the 16-bit `raw indices` when it has any, else `raw indices32`
/// (meshes with more vertices than 16 bits can address). `None` when neither
/// has any.
///
/// `raw indices`' `word` is declared `short_integer` in every schema, so an
/// index of 32,768 or more reads back negative; it is masked to its 16 bits.
pub(crate) fn read_mesh_indices(pmt: &TagStruct<'_>) -> Option<Vec<u32>> {
    let read = |block: &str, field: &str, mask: u32| {
        let block = pmt.field(block).and_then(|f| f.as_block()).filter(|b| !b.is_empty())?;
        Some(
            (0..block.len())
                .filter_map(|k| block.element(k))
                .map(|e| e.read_int_any(field).unwrap_or(0) as u32 & mask)
                .collect(),
        )
    };
    read("raw indices", "word", 0xFFFF).or_else(|| read("raw indices32", "dword", u32::MAX))
}

/// A part's or subpart's `index start` as a position in the index buffer.
/// Halo 3 declares it a signed `short_integer`, so a start past 32,767 reads
/// back negative; Halo 4 widened it to `long_integer`. Negative can only be
/// the Halo 3 wrap.
pub(crate) fn index_start(start: i128) -> usize {
    if start < 0 { (start as i16 as u16) as usize } else { start as usize }
}

/// Whether a render mesh's indices are a triangle strip, per its `index
/// buffer type` — defaulting to a strip, which is what every MCC render mesh
/// uses.
pub(crate) fn mesh_is_triangle_strip(mesh: &TagStruct<'_>) -> bool {
    mesh.field("index buffer type")
        .and_then(|f| f.value())
        .map(|v| matches!(v, TagFieldData::CharEnum { name: Some(n), .. } if n == "triangle strip"))
        .unwrap_or(true)
}

//================================================================================
// BSP edge-ring walker
//================================================================================

/// Cached row of a Halo BSP `edges[]` block. Walking a surface's
/// polygon ring hammers these fields tens of thousands of times in
/// hot loops, so callers pre-cache once into a `Vec<EdgeRow>` rather
/// than re-resolving via `as_struct()` per step.
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeRow {
    pub(crate) start_vertex: i32,
    pub(crate) end_vertex: i32,
    pub(crate) forward_edge: i32,
    pub(crate) reverse_edge: i32,
    pub(crate) left_surface: i32,
    pub(crate) right_surface: i32,
}

/// Walk a single surface's edge ring and return the ordered list of
/// vertex indices that bound it. Each edge belongs to two surfaces;
/// the matching side decides which vertex (start vs end) to emit
/// AND which neighbour edge to follow next. Returns an empty vec on
/// malformed rings (cycles that don't return to `first_edge` within
/// a reasonable bound).
///
/// Used by both `collision_model` (object collision) and
/// `scenario_structure_bsp` (level collision).
pub(crate) fn walk_surface_ring(
    surface_index: i32,
    first_edge: i32,
    edges: &[EdgeRow],
) -> Vec<i32> {
    let mut out = Vec::new();
    let mut current = first_edge;
    let mut steps = 0;
    let max_steps = edges.len() * 2 + 8;
    loop {
        if current < 0 || (current as usize) >= edges.len() { return Vec::new(); }
        let e = edges[current as usize];
        let next = if e.left_surface == surface_index {
            out.push(e.start_vertex);
            e.forward_edge
        } else if e.right_surface == surface_index {
            out.push(e.end_vertex);
            e.reverse_edge
        } else {
            return Vec::new();
        };
        if next == first_edge { break; }
        current = next;
        steps += 1;
        if steps > max_steps { return Vec::new(); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TagFieldData, TagFile};

    /// `raw indices` stores `word` as a signed short, so the upper half of the
    /// 16-bit range reads back negative and has to come out as the index it is.
    #[test]
    fn a_16_bit_index_past_32767_is_not_sign_extended() {
        let schema = "../definitions/halo3_mcc/render_model.json";
        if !std::path::Path::new(schema).exists() {
            return;
        }
        let mut tag = TagFile::new(schema).unwrap();
        let mut root = tag.root_mut();
        let mut pmt = root.field_path_mut("render geometry/per mesh temporary").unwrap();
        let mut pmt = pmt.as_block_mut().unwrap();
        let element = pmt.add_element();
        let mut element = pmt.element_mut(element).unwrap();
        let mut field = element.field_path_mut("raw indices").unwrap();
        let mut indices = field.as_block_mut().unwrap();
        for value in [0i16, 32767, -32768, -1] {
            let index = indices.add_element();
            let mut word = indices.element_mut(index).unwrap();
            word.field_path_mut("word").unwrap().set(TagFieldData::ShortInteger(value)).unwrap();
        }

        let pmt = tag.root().field_path("render geometry/per mesh temporary").unwrap();
        let pmt = pmt.as_block().unwrap().element(0).unwrap();
        assert_eq!(read_mesh_indices(&pmt), Some(vec![0, 32767, 32768, 65535]));
    }

    #[test]
    fn a_tag_name_keeps_its_dots() {
        assert_eq!(tag_path_basename("shaders\\metal\\plate"), Some("plate"));
        assert_eq!(tag_path_basename("shaders/metal/plate"), Some("plate"));
        // Shipped CE tags: the dot is part of the name, not an extension.
        assert_eq!(tag_path_basename("bitmaps\\probe. multipurpose"), Some("probe. multipurpose"));
        assert_eq!(tag_path_basename("sound\\stand_sword_melee.mov"), Some("stand_sword_melee.mov"));
        assert_eq!(tag_path_basename("plate"), Some("plate"));
        assert_eq!(tag_path_basename(""), None);
        assert_eq!(tag_path_basename("shaders\\"), None);
    }

    #[test]
    fn a_negative_index_start_is_the_halo_3_short_wrap() {
        assert_eq!(index_start(0), 0);
        assert_eq!(index_start(40_000), 40_000);
        assert_eq!(index_start(-32768), 32768);
        assert_eq!(index_start(-1), 65535);
    }
}
