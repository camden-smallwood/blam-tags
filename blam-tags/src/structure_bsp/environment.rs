//! `structure_bsp` environment objects, leaf-map graph, breakable
//! surface sets, the error-report tree, and the debug-info tree. Mirrors
//! the MCC schema 1:1.

use crate::api::TagStruct;
use crate::math::{RealArgbColor, RealBounds, RealPoint3d, RealQuaternion, RealVector3d};

use super::common::{read_argb, read_block, read_data, read_struct, ErrorReportPoint};

// --- environment objects ----------------------------------------------------

/// `structure_bsp_environment_object_palette_block`.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentObjectPalette {
    pub definition: String,
    pub model: String,
}

impl EnvironmentObjectPalette {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            definition: s.read_tag_ref_path("definition").unwrap_or_default(),
            model: s.read_tag_ref_path("model").unwrap_or_default(),
        }
    }
}

/// `structure_bsp_environment_object_block` — a baked scenery placement
/// owned by this BSP.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentObject {
    pub name: String,
    pub rotation: RealQuaternion,
    pub translation: RealPoint3d,
    pub scale: f32,
    pub palette_index: i16,
    pub unique_id: i32,
    /// `exported object type` (a 4-char group tag).
    pub exported_object_type: u32,
    pub scenario_object_name: String,
}

impl EnvironmentObject {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string("name").unwrap_or_default(),
            rotation: s.read_quat("rotation"),
            translation: s.read_point3d("translation"),
            scale: s.read_real("scale").unwrap_or(0.0),
            palette_index: s.read_block_index("palette_index"),
            unique_id: s.read_int_any("unique id").unwrap_or(0) as i32,
            exported_object_type: s.read_int_any("exported object type").unwrap_or(0) as u32,
            scenario_object_name: s.read_string("scenario object name").unwrap_or_default(),
        }
    }
}

// --- leaf map ---------------------------------------------------------------

/// `global_map_leaf_block` — one leaf of the global (whole-map) BSP leaf
/// graph: bounded faces + connection refs.
#[derive(Debug, Clone, Default)]
pub struct MapLeaf {
    pub faces: Vec<MapLeafFace>,
    /// `connection indices`.
    pub connection_indices: Vec<i32>,
}

/// `map_leaf_face_block`.
#[derive(Debug, Clone, Default)]
pub struct MapLeafFace {
    pub node_index: i32,
    pub vertices: Vec<RealPoint3d>,
}

impl MapLeaf {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            faces: read_block(s, "faces", |e| MapLeafFace {
                node_index: e.read_int_any("node index").unwrap_or(0) as i32,
                vertices: read_block(e, "vertices", |v| v.read_point3d("vertex")),
            }),
            connection_indices: read_block(s, "connection indices", |e| {
                e.read_int_any("connection index").unwrap_or(0) as i32
            }),
        }
    }
}

/// `global_leaf_connection_block` — a planar connection (portal) between
/// two leaf-map leaves.
#[derive(Debug, Clone, Default)]
pub struct LeafConnection {
    pub plane_index: i32,
    pub back_leaf_index: i32,
    pub front_leaf_index: i32,
    pub vertices: Vec<RealPoint3d>,
    pub area: f32,
}

impl LeafConnection {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            plane_index: s.read_int_any("plane index").unwrap_or(0) as i32,
            back_leaf_index: s.read_int_any("back leaf index").unwrap_or(0) as i32,
            front_leaf_index: s.read_int_any("front leaf index").unwrap_or(0) as i32,
            vertices: read_block(s, "vertices", |v| v.read_point3d("vertex")),
            area: s.read_real("area").unwrap_or(0.0),
        }
    }
}

// --- breakable surface sets -------------------------------------------------

/// `breakable_surface_set_block` — an 8-`long` bitfield tracking which
/// breakable surfaces in the set are still intact.
#[derive(Debug, Clone, Default)]
pub struct BreakableSurfaceSet {
    pub supported_bitfield: Vec<i32>,
}

impl BreakableSurfaceSet {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            supported_bitfield: s
                .field("supported bitfield")
                .and_then(|f| f.as_array())
                .map(|a| a.iter().map(|e| e.read_int_any("bitvector data").unwrap_or(0) as i32).collect())
                .unwrap_or_default(),
        }
    }
}

// --- error reports ----------------------------------------------------------

/// `global_error_report_categories_block` — a named category of debug
/// error reports.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportCategory {
    pub name: String,
    /// `report type` (short_enum), raw.
    pub report_type: i16,
    pub flags: u16,
    pub reports: Vec<ErrorReport>,
}

/// `error_reports_block` — one debug report with geometry.
#[derive(Debug, Clone, Default)]
pub struct ErrorReport {
    /// `type` (char_enum), raw.
    pub report_type: i8,
    /// `source` (char_enum), raw.
    pub source: i8,
    pub flags: u16,
    pub text: Vec<u8>,
    pub source_filename: String,
    pub source_line_number: i32,
    pub vertices: Vec<ErrorReportVertex>,
    pub vectors: Vec<ErrorReportVector>,
    pub lines: Vec<ErrorReportLine>,
    pub triangles: Vec<ErrorReportPolygon>,
    pub quads: Vec<ErrorReportPolygon>,
    pub comments: Vec<ErrorReportComment>,
    pub report_key: i32,
    pub node_index: i32,
    pub bounds_x: RealBounds,
    pub bounds_y: RealBounds,
    pub bounds_z: RealBounds,
    pub color: RealArgbColor,
}

/// `error_report_vertices_block`.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportVertex {
    pub point: ErrorReportPoint,
    pub color: RealArgbColor,
    pub screen_size: f32,
}

/// `error_report_vectors_block`.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportVector {
    pub point: ErrorReportPoint,
    pub color: RealArgbColor,
    pub normal: RealVector3d,
    pub screen_length: f32,
}

/// `error_report_lines_block` / triangles / quads share a points-array +
/// color shape; `points` holds 2/3/4 points respectively.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportLine {
    pub points: Vec<ErrorReportPoint>,
    pub color: RealArgbColor,
}

/// Polygon report (triangle = 3 points, quad = 4 points).
#[derive(Debug, Clone, Default)]
pub struct ErrorReportPolygon {
    pub points: Vec<ErrorReportPoint>,
    pub color: RealArgbColor,
}

/// `error_report_comments_block`.
#[derive(Debug, Clone, Default)]
pub struct ErrorReportComment {
    pub text: Vec<u8>,
    pub point: ErrorReportPoint,
    pub color: RealArgbColor,
}

fn read_point_array(s: &TagStruct<'_>) -> Vec<ErrorReportPoint> {
    s.field("points")
        .and_then(|f| f.as_array())
        .map(|a| {
            a.iter()
                .map(|e| read_struct(&e, "point", ErrorReportPoint::from_struct))
                .collect()
        })
        .unwrap_or_default()
}

impl ErrorReportCategory {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_long_string("name").unwrap_or_default(),
            report_type: s.read_int_any("report type").unwrap_or(0) as i16,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            reports: read_block(s, "reports", ErrorReport::from_struct),
        }
    }
}

impl ErrorReport {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            report_type: s.read_int_any("type").unwrap_or(0) as i8,
            source: s.read_int_any("source").unwrap_or(0) as i8,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            text: read_data(s, "text"),
            source_filename: s.read_string("source filename").unwrap_or_default(),
            source_line_number: s.read_int_any("source line number").unwrap_or(0) as i32,
            vertices: read_block(s, "vertices", |e| ErrorReportVertex {
                point: read_struct(e, "point", ErrorReportPoint::from_struct),
                color: read_argb(e, "color"),
                screen_size: e.read_real("screen size").unwrap_or(0.0),
            }),
            vectors: read_block(s, "vectors", |e| ErrorReportVector {
                point: read_struct(e, "point", ErrorReportPoint::from_struct),
                color: read_argb(e, "color"),
                normal: e.read_vec3("normal"),
                screen_length: e.read_real("screen length").unwrap_or(0.0),
            }),
            lines: read_block(s, "lines", |e| ErrorReportLine {
                points: read_point_array(e),
                color: read_argb(e, "color"),
            }),
            triangles: read_block(s, "triangles", |e| ErrorReportPolygon {
                points: read_point_array(e),
                color: read_argb(e, "color"),
            }),
            quads: read_block(s, "quads", |e| ErrorReportPolygon {
                points: read_point_array(e),
                color: read_argb(e, "color"),
            }),
            comments: read_block(s, "comments", |e| ErrorReportComment {
                text: read_data(e, "text"),
                point: read_struct(e, "point", ErrorReportPoint::from_struct),
                color: read_argb(e, "color"),
            }),
            report_key: s.read_int_any("report key").unwrap_or(0) as i32,
            node_index: s.read_int_any("node index").unwrap_or(0) as i32,
            bounds_x: s.read_real_bounds("bounds x"),
            bounds_y: s.read_real_bounds("bounds y"),
            bounds_z: s.read_real_bounds("bounds z"),
            color: read_argb(s, "color"),
        }
    }
}

// --- debug info -------------------------------------------------------------

/// `structure_bsp_debug_info_block` — per-cluster + fog-plane/zone debug
/// render lines and index lists.
#[derive(Debug, Clone, Default)]
pub struct DebugInfo {
    pub clusters: Vec<ClusterDebugInfo>,
    pub fog_planes: Vec<FogPlaneDebugInfo>,
    pub fog_zones: Vec<FogZoneDebugInfo>,
}

/// `structure_bsp_debug_info_render_line_block`.
#[derive(Debug, Clone, Default)]
pub struct DebugRenderLine {
    /// `type` (short_enum), raw.
    pub line_type: i16,
    pub code: i16,
    pub point_0: RealPoint3d,
    pub point_1: RealPoint3d,
}

fn read_lines(s: &TagStruct<'_>) -> Vec<DebugRenderLine> {
    read_block(s, "lines", |e| DebugRenderLine {
        line_type: e.read_int_any("type").unwrap_or(0) as i16,
        code: e.read_int_any("code").unwrap_or(0) as i16,
        point_0: e.read_point3d("point 0"),
        point_1: e.read_point3d("point 1"),
    })
}

fn read_indices(s: &TagStruct<'_>, name: &str) -> Vec<i32> {
    read_block(s, name, |e| e.read_int_any("index").unwrap_or(0) as i32)
}

/// `structure_bsp_cluster_debug_info_block`.
#[derive(Debug, Clone, Default)]
pub struct ClusterDebugInfo {
    pub errors: u16,
    pub warnings: u16,
    pub lines: Vec<DebugRenderLine>,
    pub fog_plane_indices: Vec<i32>,
    pub visible_fog_plane_indices: Vec<i32>,
    pub vis_fog_omission_cluster_indices: Vec<i32>,
    pub containing_fog_zone_indices: Vec<i32>,
}

/// `structure_bsp_fog_plane_debug_info_block`.
#[derive(Debug, Clone, Default)]
pub struct FogPlaneDebugInfo {
    pub fog_zone_index: i32,
    pub connected_plane_designator: i32,
    pub lines: Vec<DebugRenderLine>,
    pub intersected_cluster_indices: Vec<i32>,
    pub inf_extent_cluster_indices: Vec<i32>,
}

/// `structure_bsp_fog_zone_debug_info_block`.
#[derive(Debug, Clone, Default)]
pub struct FogZoneDebugInfo {
    pub media_index: i32,
    pub base_fog_plane_index: i32,
    pub lines: Vec<DebugRenderLine>,
    pub immersed_cluster_indices: Vec<i32>,
    pub bounding_fog_plane_indices: Vec<i32>,
    pub collision_fog_plane_indices: Vec<i32>,
}

impl DebugInfo {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            clusters: read_block(s, "clusters", |e| ClusterDebugInfo {
                errors: e.read_int_any("errors").unwrap_or(0) as u16,
                warnings: e.read_int_any("warnings").unwrap_or(0) as u16,
                lines: read_lines(e),
                fog_plane_indices: read_indices(e, "fog plane indices"),
                visible_fog_plane_indices: read_indices(e, "visible fog plane indices"),
                vis_fog_omission_cluster_indices: read_indices(
                    e,
                    "vis-fog omission cluster indices",
                ),
                containing_fog_zone_indices: read_indices(e, "containing fog zone indices"),
            }),
            fog_planes: read_block(s, "fog planes", |e| FogPlaneDebugInfo {
                fog_zone_index: e.read_int_any("fog zone index").unwrap_or(0) as i32,
                connected_plane_designator: e
                    .read_int_any("connected plane designator")
                    .unwrap_or(0) as i32,
                lines: read_lines(e),
                intersected_cluster_indices: read_indices(e, "intersected cluster indices"),
                inf_extent_cluster_indices: read_indices(e, "inf. extent cluster indices"),
            }),
            fog_zones: read_block(s, "fog zones", |e| FogZoneDebugInfo {
                media_index: e.read_int_any("media index").unwrap_or(0) as i32,
                base_fog_plane_index: e.read_int_any("base fog plane index").unwrap_or(0) as i32,
                lines: read_lines(e),
                immersed_cluster_indices: read_indices(e, "immersed cluster indices"),
                bounding_fog_plane_indices: read_indices(e, "bounding fog plane indices"),
                collision_fog_plane_indices: read_indices(e, "collision fog plane indices"),
            }),
        }
    }
}
