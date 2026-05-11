//! Halo structure_bsp tag (`sbsp`) walker.
//!
//! Walks the rendering-relevant subset of a BSP tag — clusters,
//! instances, materials, mesh parts. Mesh vertex / index data is
//! decoded separately via [`crate::render_model`]'s mesh reader on the
//! BSP's `render geometry` sub-struct (it shares the s_render_geometry
//! schema).
//!
//! Reference: `Ares/source/structures/structure_bsp_definitions.h:102`.

mod types;

pub use types::{
    Bsp3d, Bsp3dNode, BspAtmospherePaletteEntry, BspCluster, BspClusterPortal,
    BspCollisionMaterial, BspInstance, BspInstanceDefinition, BspLeaf, BspMarker, BspMaterial,
    BspMeshMetadata, BspMeshPart, BspWeatherPaletteEntry, CollisionBsp2dNode,
    CollisionBsp2dReference, CollisionEdge, CollisionLeaf, CollisionSurface, CollisionVertex,
    StructureBsp, StructureBspError,
};
