//! `structure_bsp` Havok structure physics (`global_structure_physics_struct`)
//! — the world MOPP collision-acceleration code, its bounds, the breakable-
//! surface MOPP, and the breakable-surface key table. Mirrors the MCC
//! schema 1:1.
//!
//! The remaining members of the `resource interface` (the small/large
//! collision BSP and the instanced-geometry definitions) are already
//! surfaced through [`super::Bsp3d::from_collision_block`] and
//! [`super::BspInstanceDefinition`]; only the render-geometry reuse and
//! structure physics were missing, and they live here + on the root.

use crate::api::TagStruct;
use crate::math::RealPoint3d;

use super::common::{read_block, MoppCode};

/// `breakable_surface_key_table_block` — maps a breakable surface (by
/// instanced-geometry + set + index) to its seed surface + AABB.
#[derive(Debug, Clone, Default)]
pub struct BreakableSurfaceKey {
    pub instanced_geometry_index: i16,
    pub breakable_surface_set_index: i8,
    pub breakable_surface_index: i8,
    pub seed_surface_index: i32,
    /// `x0/x1/y0/y1/z0/z1` — surface bounding box.
    pub bounds: [f32; 6],
}

impl BreakableSurfaceKey {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            instanced_geometry_index: s.read_int_any("instanced geometry index").unwrap_or(-1) as i16,
            breakable_surface_set_index: s.read_int_any("breakable surface set index").unwrap_or(-1) as i8,
            breakable_surface_index: s.read_int_any("breakable surface index").unwrap_or(-1) as i8,
            seed_surface_index: s.read_int_any("seed surface index").unwrap_or(-1) as i32,
            bounds: [
                s.read_real("x0").unwrap_or(0.0),
                s.read_real("x1").unwrap_or(0.0),
                s.read_real("y0").unwrap_or(0.0),
                s.read_real("y1").unwrap_or(0.0),
                s.read_real("z0").unwrap_or(0.0),
                s.read_real("z1").unwrap_or(0.0),
            ],
        }
    }
}

/// `global_structure_physics_struct` — the BSP's Havok collision MOPP
/// acceleration data + breakable-surface bookkeeping.
#[derive(Debug, Clone, Default)]
pub struct StructurePhysics {
    pub mopp_code: Vec<MoppCode>,
    pub mopp_bounds_min: RealPoint3d,
    pub mopp_bounds_max: RealPoint3d,
    pub breakable_surfaces_mopp_code: Vec<MoppCode>,
    pub breakable_surface_key_table: Vec<BreakableSurfaceKey>,
}

impl StructurePhysics {
    pub(crate) fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            mopp_code: read_block(s, "mopp code block", MoppCode::from_struct),
            mopp_bounds_min: s.read_point3d("mopp bounds min"),
            mopp_bounds_max: s.read_point3d("mopp bounds max"),
            breakable_surfaces_mopp_code: read_block(
                s,
                "breakable surfaces mopp code block",
                MoppCode::from_struct,
            ),
            breakable_surface_key_table: read_block(
                s,
                "breakable surfaace key table",
                BreakableSurfaceKey::from_struct,
            ),
        }
    }
}
