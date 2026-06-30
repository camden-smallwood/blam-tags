//! Sub-structs nested inside `structure_bsp_cluster_block`: the cluster
//! collision-instanced-geometry (Havok/MOPP) shape, decorator runtime
//! groups, and cluster cubemaps. Mirrors the MCC schema 1:1.

use crate::api::TagStruct;
use crate::math::{RealPoint3d, RealVector3d};

use super::common::{read_block, read_struct, HavokShapeCollection, MoppBvTreeShape, MoppCode};

/// `cluster_instanced_geometry_shape_struct` — the cluster's Havok
/// collision-shape collection plus its source BSP/cluster reference.
#[derive(Debug, Clone, Default)]
pub struct ClusterInstancedGeometryShape {
    pub cluster_collision_shape: HavokShapeCollection,
    pub structure_bsp_reference: String,
    pub cluster_index: i32,
}

impl ClusterInstancedGeometryShape {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cluster_collision_shape: read_struct(
                s,
                "cluster collsion shape",
                HavokShapeCollection::from_struct,
            ),
            structure_bsp_reference: s
                .read_tag_ref_path("structure bsp reference")
                .unwrap_or_default(),
            cluster_index: s.read_block_index("cluster index") as i32,
        }
    }
}

/// `collision_instanced_geometry_struct` — the cluster collision shape:
/// the shape collection, its MOPP bounding-volume tree, and the compiled
/// MOPP code blocks.
#[derive(Debug, Clone, Default)]
pub struct ClusterCollisionInstancedGeometry {
    pub cluster_shape: ClusterInstancedGeometryShape,
    pub mopp_bv_tree_shape: MoppBvTreeShape,
    pub mopp_code: Vec<MoppCode>,
}

impl ClusterCollisionInstancedGeometry {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cluster_shape: read_struct(s, "cluster shape", ClusterInstancedGeometryShape::from_struct),
            mopp_bv_tree_shape: read_struct(s, "mopp bv tree shape", MoppBvTreeShape::from_struct),
            mopp_code: read_block(s, "mopp code block", MoppCode::from_struct),
        }
    }
}

/// `decorator_runtime_cluster_block` — a runtime decorator (grass/clutter)
/// placement group within a cluster.
#[derive(Debug, Clone, Default)]
pub struct DecoratorRuntimeCluster {
    pub decorator_placement_count: i16,
    pub decorator_set_index: i8,
    pub decorator_instance_buffer_index: i8,
    pub decorator_instance_buffer_offset: i32,
    pub position_bounds_min: RealVector3d,
    pub bounding_sphere_radius: f32,
    pub position_bounds_size: RealVector3d,
    pub bounding_sphere_center: RealVector3d,
    /// `model start index` — per-model start offsets.
    pub model_start_indices: Vec<i16>,
}

impl DecoratorRuntimeCluster {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            decorator_placement_count: s.read_int_any("decorator placement count").unwrap_or(0) as i16,
            decorator_set_index: s.read_int_any("decorator set index").unwrap_or(-1) as i8,
            decorator_instance_buffer_index: s
                .read_int_any("decorator instance buffer index")
                .unwrap_or(-1) as i8,
            decorator_instance_buffer_offset: s
                .read_int_any("decorator instance buffer offset")
                .unwrap_or(0) as i32,
            position_bounds_min: s.read_vec3("position bounds min"),
            bounding_sphere_radius: s.read_real("bounding sphere radius").unwrap_or(0.0),
            position_bounds_size: s.read_vec3("position bounds size"),
            bounding_sphere_center: s.read_vec3("bounding sphere center"),
            model_start_indices: read_block(s, "model start index", |e| {
                e.read_int_any("index").unwrap_or(0) as i16
            }),
        }
    }
}

/// `structure_cluster_cubemap` — a baked reflection-probe cubemap sample
/// position within the cluster.
#[derive(Debug, Clone, Default)]
pub struct ClusterCubemap {
    pub cubemap_position: RealPoint3d,
    pub scenario_cubemap_index: i16,
    pub cubemap_bitmap_index: i16,
}

impl ClusterCubemap {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cubemap_position: s.read_point3d("cubemap position"),
            scenario_cubemap_index: s.read_int_any("scenario cubemap index").unwrap_or(-1) as i16,
            cubemap_bitmap_index: s.read_int_any("cubemap bitmap index").unwrap_or(-1) as i16,
        }
    }
}
