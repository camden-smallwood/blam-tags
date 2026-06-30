//! Shared leaf structs + read helpers reused across the maximal
//! `structure_bsp` walker — Havok/MOPP collision-shape headers, scenario
//! object identifiers, and the error-report geometry point. These mirror
//! the schema 1:1; runtime-only pointer fields (`field pointer skip`,
//! `*_ptr`, `user data`) are zero in author-time tags and are omitted —
//! only the meaningful authored fields are surfaced.

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::math::{RealArgbColor, RealPlane2d, RealPoint3d};

// ---------------------------------------------------------------------------
// read helpers for field types without a dedicated TagStruct accessor
// ---------------------------------------------------------------------------

/// Read a `real_plane_2d` field (default zero plane when absent/mismatched).
pub(crate) fn read_plane2d(s: &TagStruct<'_>, name: &str) -> RealPlane2d {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealPlane2d(p)) => p,
        _ => RealPlane2d::default(),
    }
}

/// Read a `real_argb_color` field (default transparent black when absent).
pub(crate) fn read_argb(s: &TagStruct<'_>, name: &str) -> RealArgbColor {
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealArgbColor(c)) => c,
        _ => RealArgbColor::default(),
    }
}

/// Read a `data` field as owned bytes (empty when absent).
pub(crate) fn read_data(s: &TagStruct<'_>, name: &str) -> Vec<u8> {
    s.field(name).and_then(|f| f.as_data()).map(|b| b.to_vec()).unwrap_or_default()
}

/// Read a fixed inline `array` of single-field `i32`-valued structs.
pub(crate) fn read_int_array(s: &TagStruct<'_>, name: &str, field: &str) -> Vec<i32> {
    s.field(name)
        .and_then(|f| f.as_array())
        .map(|a| a.iter().map(|e| e.read_int_any(field).unwrap_or(0) as i32).collect())
        .unwrap_or_default()
}

/// Read a fixed inline `array` of single-field `f32`-valued structs.
pub(crate) fn read_real_array(s: &TagStruct<'_>, name: &str, field: &str) -> Vec<f32> {
    s.field(name)
        .and_then(|f| f.as_array())
        .map(|a| a.iter().map(|e| e.read_real(field).unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

/// Read a `block` field into a `Vec<T>` via a per-element constructor.
pub(crate) fn read_block<T, F: Fn(&TagStruct<'_>) -> T>(
    s: &TagStruct<'_>,
    name: &str,
    f: F,
) -> Vec<T> {
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|b| {
            let mut v = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    v.push(f(&e));
                }
            }
            v
        })
        .unwrap_or_default()
}

/// Read an inline `struct` field via a constructor (default when absent).
pub(crate) fn read_struct<T: Default, F: Fn(&TagStruct<'_>) -> T>(
    s: &TagStruct<'_>,
    name: &str,
    f: F,
) -> T {
    s.field(name).and_then(|x| x.as_struct()).map(|x| f(&x)).unwrap_or_default()
}

/// Read a `real_plane_3d` block field into a `Vec`.
pub(crate) fn read_plane3d_block(s: &TagStruct<'_>, name: &str, field: &str) -> Vec<crate::math::RealPlane3d> {
    read_block(s, name, |e| e.read_plane3d(field))
}

// ---------------------------------------------------------------------------
// Havok / MOPP collision-shape headers
// ---------------------------------------------------------------------------

/// `havok_shape_struct` — the common Havok shape header. All four
/// runtime pointers/`type` are tool/runtime values (zero on disk).
#[derive(Debug, Clone, Default)]
pub struct HavokShape {
    pub size: i16,
    pub count: i16,
    pub shape_type: i32,
}

impl HavokShape {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            size: s.read_int_any("size").unwrap_or(0) as i16,
            count: s.read_int_any("count").unwrap_or(0) as i16,
            shape_type: s.read_int_any("type").unwrap_or(0) as i32,
        }
    }
}

/// `havok_shape_collection_struct` — a [`HavokShape`] plus a welding flag.
#[derive(Debug, Clone, Default)]
pub struct HavokShapeCollection {
    pub base: HavokShape,
    pub disable_welding: i8,
}

impl HavokShapeCollection {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            base: s
                .field("base")
                .and_then(|f| f.as_struct())
                .map(|b| HavokShape::from_struct(&b))
                .unwrap_or_default(),
            disable_welding: s.read_int_any("disable welding").unwrap_or(0) as i8,
        }
    }
}

/// `mopp_bv_tree_shape_struct` — Havok bounding-volume MOPP tree header.
#[derive(Debug, Clone, Default)]
pub struct MoppBvTreeShape {
    pub base: HavokShape,
}

impl MoppBvTreeShape {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            base: s
                .field("base")
                .and_then(|f| f.as_struct())
                .map(|b| HavokShape::from_struct(&b))
                .unwrap_or_default(),
        }
    }
}

/// `mopp_code_definition_block` — a Havok MOPP code blob (compiled
/// collision acceleration structure) with its bounding offset/scale and
/// the raw code bytes.
#[derive(Debug, Clone, Default)]
pub struct MoppCode {
    pub size: i16,
    pub count: i16,
    /// `v.i/j/k/w` — MOPP code offset/scale vector4.
    pub offset_scale: [f32; 4],
    pub m_size: i32,
    pub m_capacity_and_flags: i32,
    /// `mopp data block` — the compiled MOPP byte stream.
    pub data: Vec<u8>,
}

impl MoppCode {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            size: s.read_int_any("size").unwrap_or(0) as i16,
            count: s.read_int_any("count").unwrap_or(0) as i16,
            offset_scale: [
                s.read_real("v.i").unwrap_or(0.0),
                s.read_real("v.j").unwrap_or(0.0),
                s.read_real("v.k").unwrap_or(0.0),
                s.read_real("v.w").unwrap_or(0.0),
            ],
            m_size: s.read_int_any("int m_size").unwrap_or(0) as i32,
            m_capacity_and_flags: s.read_int_any("int m_capacityAndFlags").unwrap_or(0) as i32,
            data: s
                .field("mopp data block")
                .and_then(|f| f.as_block())
                .map(|b| {
                    let mut v = Vec::with_capacity(b.len());
                    for i in 0..b.len() {
                        if let Some(e) = b.element(i) {
                            v.push(e.read_int_any("mopp data").unwrap_or(0) as u8);
                        }
                    }
                    v
                })
                .unwrap_or_default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Scenario object identifiers
// ---------------------------------------------------------------------------

/// `scenario_object_id_struct` — uniquely identifies a placed scenario
/// object (PVS-bound objects, fake lightprobes).
#[derive(Debug, Clone, Default)]
pub struct ScenarioObjectId {
    pub unique_id: i32,
    pub origin_bsp_index: i16,
    /// `type` (char_enum) — `e_object_type`, raw.
    pub object_type: i8,
    /// `source` (char_enum) — placement source, raw.
    pub source: i8,
}

impl ScenarioObjectId {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            unique_id: s.read_int_any("unique id").unwrap_or(0) as i32,
            origin_bsp_index: s.read_block_index("origin bsp index"),
            object_type: s.read_int_any("type").unwrap_or(-1) as i8,
            source: s.read_int_any("source").unwrap_or(-1) as i8,
        }
    }
}

/// `scenario_object_reference_struct` — a back-reference into the
/// scenario's object/palette tables.
#[derive(Debug, Clone, Default)]
pub struct ScenarioObjectReference {
    pub object_index: i16,
    pub scenario_object_index: i16,
}

impl ScenarioObjectReference {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            object_index: s.read_int_any("object index").unwrap_or(-1) as i16,
            scenario_object_index: s.read_int_any("scenario object index").unwrap_or(-1) as i16,
        }
    }
}

// ---------------------------------------------------------------------------
// Error-report geometry point
// ---------------------------------------------------------------------------

/// `error_report_point_definition` — a debug point with node skinning,
/// reused across the error-report vertices/vectors/lines/etc.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportPoint {
    pub position: RealPoint3d,
    /// `node indices`[4].
    pub node_indices: Vec<i32>,
    /// `node weights`[4].
    pub node_weights: Vec<f32>,
}

impl ErrorReportPoint {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            position: s.read_point3d("position"),
            node_indices: read_int_array(s, "node indices", "node index"),
            node_weights: read_real_array(s, "node weights", "node weight"),
        }
    }
}
