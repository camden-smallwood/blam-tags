//! The `structure_bsp` AI pathfinding subsystem
//! (`pathfinding_data_block` + `structure_bsp_pathfinding_edges_block`):
//! sectors, sector links, the bsp2d acceleration tree, per-object
//! pathfinding refs, hints, seams, jump seams, and doors. Mirrors the
//! MCC schema 1:1.

use crate::api::TagStruct;
use crate::math::{RealPlane2d, RealPoint3d};

use super::common::{read_block, read_plane2d, read_struct, ScenarioObjectId};

/// `pathfinding_data_block` — one BSP's complete pathfinding graph.
#[derive(Debug, Clone, Default)]
pub struct PathfindingData {
    pub sectors: Vec<PathfindingSector>,
    pub links: Vec<SectorLink>,
    /// `bsp2d refs` — node-or-sector refs (packed `long`).
    pub bsp2d_refs: Vec<i32>,
    pub bsp2d_nodes: Vec<SectorBsp2dNode>,
    pub vertices: Vec<RealPoint3d>,
    pub object_refs: Vec<EnvironmentObjectRef>,
    pub hints: Vec<PathfindingHint>,
    /// `instanced geometry refs` — pathfinding object indices.
    pub instanced_geometry_refs: Vec<i16>,
    pub structure_checksum: i32,
    /// `giant pathfinding data` — bsp2d root per giant object.
    pub giant_pathfinding_data: Vec<i32>,
    pub seams: Vec<PathfindingSeam>,
    pub jump_seams: Vec<PathfindingJumpSeam>,
    /// `doors` — scenario machine indices.
    pub doors: Vec<i16>,
}

/// `sector_block`.
#[derive(Debug, Clone, Default)]
pub struct PathfindingSector {
    pub flags: u16,
    pub hint_index: i16,
    pub first_link: i32,
}

/// `sector_link_block`.
#[derive(Debug, Clone, Default)]
pub struct SectorLink {
    pub vertex_1: i16,
    pub vertex_2: i16,
    pub flags: u16,
    pub hint_index: i16,
    pub forward_link: i16,
    pub reverse_link: i16,
    pub left_sector: i16,
    pub right_sector: i16,
}

/// `sector_bsp2d_nodes_block`.
#[derive(Debug, Clone, Default)]
pub struct SectorBsp2dNode {
    pub plane: RealPlane2d,
    pub left_child: i32,
    pub right_child: i32,
}

/// `environment_object_refs` — a pathfinding-relevant placed object plus
/// its per-BSP refs.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentObjectRef {
    pub flags: u16,
    pub bsps: Vec<EnvironmentObjectBspRef>,
    pub object_id: ScenarioObjectId,
}

/// `environment_object_bsp_refs`.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentObjectBspRef {
    pub bsp_reference: i32,
    pub node_index: i16,
    /// `bsp2d refs` — packed node-or-sector refs.
    pub bsp2d_refs: Vec<i32>,
    pub vertex_offset: i32,
}

/// `pathfinding_hints_block`.
#[derive(Debug, Clone, Default)]
pub struct PathfindingHint {
    /// `hint type` (short_enum), raw.
    pub hint_type: i16,
    pub next_hint_index: i16,
    pub hint_data: [i32; 4],
}

/// `pf_seam_block` — link-index mappings stitching pathfinding across a seam.
#[derive(Debug, Clone, Default)]
pub struct PathfindingSeam {
    /// `link mappings` — link indices.
    pub link_mappings: Vec<i32>,
}

/// `pf_jump_seam_block`.
#[derive(Debug, Clone, Default)]
pub struct PathfindingJumpSeam {
    pub user_jump_index: i16,
    pub rail_length: f32,
    /// `jump hints` — jump indices.
    pub jump_hints: Vec<i16>,
}

impl PathfindingData {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sectors: read_block(s, "sectors", |e| PathfindingSector {
                flags: e.read_int_any("Path-finding sector flags").unwrap_or(0) as u16,
                hint_index: e.read_int_any("hint index").unwrap_or(-1) as i16,
                first_link: e.read_int_any("first link (do not set manually)").unwrap_or(0) as i32,
            }),
            links: read_block(s, "links", |e| SectorLink {
                vertex_1: e.read_int_any("vertex 1").unwrap_or(-1) as i16,
                vertex_2: e.read_int_any("vertex 2").unwrap_or(-1) as i16,
                flags: e.read_int_any("link flags").unwrap_or(0) as u16,
                hint_index: e.read_int_any("hint index").unwrap_or(-1) as i16,
                forward_link: e.read_int_any("forward link").unwrap_or(-1) as i16,
                reverse_link: e.read_int_any("reverse link").unwrap_or(-1) as i16,
                left_sector: e.read_int_any("left sector").unwrap_or(-1) as i16,
                right_sector: e.read_int_any("right sector").unwrap_or(-1) as i16,
            }),
            bsp2d_refs: read_block(s, "bsp2d refs", |e| {
                e.read_int_any("node ref or sector ref").unwrap_or(0) as i32
            }),
            bsp2d_nodes: read_block(s, "bsp2d nodes", |e| SectorBsp2dNode {
                plane: read_plane2d(e, "plane"),
                left_child: e.read_int_any("left child").unwrap_or(0) as i32,
                right_child: e.read_int_any("right child").unwrap_or(0) as i32,
            }),
            vertices: read_block(s, "vertices", |e| e.read_point3d("point")),
            object_refs: read_block(s, "object refs", EnvironmentObjectRef::from_struct),
            hints: read_block(s, "pathfinding hints", |e| PathfindingHint {
                hint_type: e.read_int_any("hint type").unwrap_or(0) as i16,
                next_hint_index: e.read_int_any("Next hint index").unwrap_or(-1) as i16,
                hint_data: [
                    e.read_int_any("hint data 0").unwrap_or(0) as i32,
                    e.read_int_any("hint data 1").unwrap_or(0) as i32,
                    e.read_int_any("hint data 2").unwrap_or(0) as i32,
                    e.read_int_any("hint data 3").unwrap_or(0) as i32,
                ],
            }),
            instanced_geometry_refs: read_block(s, "instanced geometry refs", |e| {
                e.read_int_any("pathfinding object_index").unwrap_or(-1) as i16
            }),
            structure_checksum: s.read_int_any("structure checksum").unwrap_or(0) as i32,
            giant_pathfinding_data: read_block(s, "giant pathfinding data", |e| {
                e.read_block_index("bsp2d root") as i32
            }),
            seams: read_block(s, "seams", |e| PathfindingSeam {
                link_mappings: read_block(e, "link mappings", |m| {
                    m.read_int_any("link index").unwrap_or(0) as i32
                }),
            }),
            jump_seams: read_block(s, "jump seams", |e| PathfindingJumpSeam {
                user_jump_index: e.read_int_any("user jump index").unwrap_or(-1) as i16,
                rail_length: e.read_real("rail length").unwrap_or(0.0),
                jump_hints: read_block(e, "jump hints", |j| {
                    j.read_int_any("jump index").unwrap_or(-1) as i16
                }),
            }),
            doors: read_block(s, "doors", |e| {
                e.read_block_index("scenario machine index")
            }),
        }
    }
}

impl EnvironmentObjectRef {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            bsps: read_block(s, "bsps", |e| EnvironmentObjectBspRef {
                bsp_reference: e.read_int_any("bsp reference").unwrap_or(0) as i32,
                node_index: e.read_int_any("node_index").unwrap_or(-1) as i16,
                bsp2d_refs: read_block(e, "bsp2d refs", |r| {
                    r.read_int_any("node ref or sector ref").unwrap_or(0) as i32
                }),
                vertex_offset: e.read_int_any("vertex offset").unwrap_or(0) as i32,
            }),
            object_id: read_struct(s, "object id", ScenarioObjectId::from_struct),
        }
    }
}
