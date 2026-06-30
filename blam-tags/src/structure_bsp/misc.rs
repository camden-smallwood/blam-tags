//! Smaller standalone `structure_bsp` root blocks: structure seams,
//! large surfaces, weather polyhedra, detail objects, conveyor surfaces,
//! marker lights, runtime decals, and transparent planes. All mirror the
//! MCC schema 1:1.

use crate::api::TagStruct;
use crate::math::{RealPlane3d, RealPoint3d, RealQuaternion, RealVector3d};

use super::common::{read_block, read_plane3d_block, read_struct};

// --- structure seams --------------------------------------------------------

/// `structure_seam_identifier_struct` — the 4-word identifier shared by a
/// seam across the two BSPs it stitches.
#[derive(Debug, Clone, Default)]
pub struct StructureSeamIdentifier {
    pub seam_id: [i32; 4],
}

/// `structure_seam_mapping_block` — maps a structure seam to its
/// per-BSP edge + cluster sets.
#[derive(Debug, Clone, Default)]
pub struct StructureSeamMapping {
    pub identifier: StructureSeamIdentifier,
    /// `edge mapping` — structure-edge indices on this side of the seam.
    pub edge_mapping: Vec<i32>,
    /// `cluster mapping` — (cluster index, centroid) pairs.
    pub cluster_mapping: Vec<SeamClusterMapping>,
}

/// `structure_seam_cluster_mapping_block`.
#[derive(Debug, Clone, Default)]
pub struct SeamClusterMapping {
    pub cluster_index: i32,
    pub cluster_center: RealPoint3d,
}

impl StructureSeamMapping {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            identifier: read_struct(s, "seams identifier", |i| StructureSeamIdentifier {
                seam_id: [
                    i.read_int_any("seam_id0").unwrap_or(0) as i32,
                    i.read_int_any("seam_id1").unwrap_or(0) as i32,
                    i.read_int_any("seam_id2").unwrap_or(0) as i32,
                    i.read_int_any("seam_id3").unwrap_or(0) as i32,
                ],
            }),
            edge_mapping: read_block(s, "edge mapping", |e| {
                e.read_int_any("structure edge index").unwrap_or(0) as i32
            }),
            cluster_mapping: read_block(s, "cluster mapping", |e| SeamClusterMapping {
                cluster_index: e.read_int_any("cluster_index").unwrap_or(0) as i32,
                cluster_center: e.read_point3d("cluster center"),
            }),
        }
    }
}

/// `structure_edge_to_seam_edge_mapping_block`.
#[derive(Debug, Clone, Default)]
pub struct EdgeToSeamEdge {
    pub seam_index: i16,
    pub seam_edge_index: i16,
}

impl EdgeToSeamEdge {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            seam_index: s.read_int_any("seam_index").unwrap_or(-1) as i16,
            seam_edge_index: s.read_int_any("seam_edge_index").unwrap_or(-1) as i16,
        }
    }
}

// --- large structure surfaces ----------------------------------------------

/// `structure_surface_block` — the `long`-indexed (large) variant of the
/// surface→triangle-mapping range. (The `short` variant is
/// `StructureSurface` in `types.rs`.)
#[derive(Debug, Clone, Default)]
pub struct StructureSurfaceLarge {
    pub first_mapping_index: i32,
    pub mapping_count: i32,
}

impl StructureSurfaceLarge {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            first_mapping_index: s
                .read_int_any("first_structure_surface_to_triangle_mapping_index")
                .unwrap_or(0) as i32,
            mapping_count: s
                .read_int_any("structure_surface_to_triangle_mapping_count")
                .unwrap_or(0) as i32,
        }
    }
}

// --- weather polyhedra ------------------------------------------------------

/// `structure_bsp_weather_polyhedron_block` — a convex weather volume
/// (bounding sphere + bounding planes) the engine tests the camera
/// against for per-region weather activation.
#[derive(Debug, Clone, Default)]
pub struct WeatherPolyhedron {
    pub bounding_sphere_center: RealPoint3d,
    pub bounding_sphere_radius: f32,
    pub planes: Vec<RealPlane3d>,
}

impl WeatherPolyhedron {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            bounding_sphere_center: s.read_point3d("bounding sphere center"),
            bounding_sphere_radius: s.read_real("bounding sphere radius").unwrap_or(0.0),
            planes: read_plane3d_block(s, "planes", "plane"),
        }
    }
}

// --- detail objects ---------------------------------------------------------

/// `structure_bsp_detail_object_data_block` — the cell-grid + packed
/// instance/count/z-reference streams driving detail-object (grass)
/// rendering.
#[derive(Debug, Clone, Default)]
pub struct DetailObjectData {
    pub cells: Vec<DetailObjectCell>,
    pub instances: Vec<DetailObjectInstance>,
    pub counts: Vec<i16>,
    pub z_reference_vectors: Vec<[f32; 4]>,
}

/// `global_detail_object_cells_block`.
#[derive(Debug, Clone, Default)]
pub struct DetailObjectCell {
    pub cell_x: i16,
    pub cell_y: i16,
    pub cell_z: i16,
    pub offset_z: i16,
    pub valid_layers_flags: i32,
    pub start_index: i32,
    pub count_index: i32,
}

/// `global_detail_object_block` — one packed detail-object instance.
#[derive(Debug, Clone, Default)]
pub struct DetailObjectInstance {
    pub position: [i8; 3],
    pub data: i8,
    pub color: i16,
}

impl DetailObjectData {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cells: read_block(s, "cells", |e| DetailObjectCell {
                cell_x: e.read_int_any("cell x").unwrap_or(0) as i16,
                cell_y: e.read_int_any("cell y").unwrap_or(0) as i16,
                cell_z: e.read_int_any("cell z").unwrap_or(0) as i16,
                offset_z: e.read_int_any("offset z").unwrap_or(0) as i16,
                valid_layers_flags: e.read_int_any("valid layers flags").unwrap_or(0) as i32,
                start_index: e.read_int_any("start index").unwrap_or(0) as i32,
                count_index: e.read_int_any("count index").unwrap_or(0) as i32,
            }),
            instances: read_block(s, "instances", |e| DetailObjectInstance {
                position: [
                    e.read_int_any("position x").unwrap_or(0) as i8,
                    e.read_int_any("position y").unwrap_or(0) as i8,
                    e.read_int_any("position z").unwrap_or(0) as i8,
                ],
                data: e.read_int_any("data").unwrap_or(0) as i8,
                color: e.read_int_any("color").unwrap_or(0) as i16,
            }),
            counts: read_block(s, "counts", |e| e.read_int_any("count").unwrap_or(0) as i16),
            z_reference_vectors: read_block(s, "z reference vectors", |e| {
                [
                    e.read_real("z reference i").unwrap_or(0.0),
                    e.read_real("z reference j").unwrap_or(0.0),
                    e.read_real("z reference k").unwrap_or(0.0),
                    e.read_real("z reference l").unwrap_or(0.0),
                ]
            }),
        }
    }
}

// --- conveyor surfaces ------------------------------------------------------

/// `structure_bsp_conveyor_surface_block` — a (u, v) surface-flow basis.
#[derive(Debug, Clone, Default)]
pub struct ConveyorSurface {
    pub u: RealVector3d,
    pub v: RealVector3d,
}

impl ConveyorSurface {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self { u: s.read_vec3("u"), v: s.read_vec3("v") }
    }
}

// --- marker lights ----------------------------------------------------------

/// `structure_bsp_marker_light_palette` — a referenced `light` tag.
#[derive(Debug, Clone, Default)]
pub struct MarkerLightPalette {
    pub light_tag: String,
}

impl MarkerLightPalette {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self { light_tag: s.read_tag_ref_path("light tag").unwrap_or_default() }
    }
}

// --- runtime decals ---------------------------------------------------------

/// `structure_bsp_runtime_decal_block` — a baked-in scenario decal
/// placement.
#[derive(Debug, Clone, Default)]
pub struct RuntimeDecal {
    pub decal_palette_index: i16,
    pub rotation: RealQuaternion,
    pub position: RealPoint3d,
    pub scale: f32,
}

impl RuntimeDecal {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            decal_palette_index: s.read_int_any("decal palette index").unwrap_or(-1) as i16,
            rotation: s.read_quat("rotation"),
            position: s.read_point3d("position"),
            scale: s.read_real("scale").unwrap_or(0.0),
        }
    }
}

// --- transparent planes -----------------------------------------------------

/// `transparent_planes_block` — per-part transparent sorting planes.
#[derive(Debug, Clone, Default)]
pub struct TransparentPlane {
    pub section_index: i16,
    pub part_index: i16,
    pub plane: RealPlane3d,
}

impl TransparentPlane {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            section_index: s.read_int_any("section index").unwrap_or(-1) as i16,
            part_index: s.read_int_any("part index").unwrap_or(-1) as i16,
            plane: s.read_plane3d("plane"),
        }
    }
}
