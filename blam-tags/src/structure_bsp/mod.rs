//! Halo structure_bsp tag (`sbsp`) walker.
//!
//! Walks the rendering-relevant subset of a BSP tag — clusters,
//! instances, materials, mesh parts. Mesh vertex / index data is
//! decoded separately via [`crate::render_model`]'s mesh reader on the
//! BSP's `render geometry` sub-struct (it shares the s_render_geometry
//! schema).
//!
//! Reference: `Ares/source/structures/structure_bsp_definitions.h:102`.

mod acoustics;
mod cluster;
mod common;
mod environment;
mod misc;
mod pathfinding;
mod resource;
mod types;

pub use resource::{BreakableSurfaceKey, StructurePhysics};

pub use cluster::{
    ClusterCollisionInstancedGeometry, ClusterCubemap, ClusterInstancedGeometryShape,
    DecoratorRuntimeCluster,
};

pub use environment::{
    BreakableSurfaceSet, ClusterDebugInfo, DebugInfo, DebugRenderLine, EnvironmentObject,
    EnvironmentObjectPalette, ErrorReport, ErrorReportCategory, ErrorReportComment,
    ErrorReportLine, ErrorReportPolygon, ErrorReportVector, ErrorReportVertex, FogPlaneDebugInfo,
    FogZoneDebugInfo, LeafConnection, MapLeaf, MapLeafFace,
};

pub use acoustics::{
    AcousticsAmbience, AcousticsEnvironment, AcousticsPalette, Audibility, BackgroundSoundPalette,
    SoundCluster, SoundEnvironmentPalette,
};

pub use pathfinding::{
    EnvironmentObjectBspRef, EnvironmentObjectRef, PathfindingData, PathfindingHint,
    PathfindingJumpSeam, PathfindingSeam, PathfindingSector, SectorBsp2dNode, SectorLink,
};

pub use common::{
    ErrorReportPoint, HavokShape, HavokShapeCollection, MoppBvTreeShape, MoppCode,
    ScenarioObjectId, ScenarioObjectReference,
};
pub use misc::{
    ConveyorSurface, DetailObjectCell, DetailObjectData, DetailObjectInstance, EdgeToSeamEdge,
    MarkerLightPalette, RuntimeDecal, SeamClusterMapping, StructureSeamIdentifier,
    StructureSeamMapping, StructureSurfaceLarge, TransparentPlane, WeatherPolyhedron,
};
pub use types::{
    Bsp3d, Bsp3dNode, BspAtmospherePaletteEntry, BspCameraFxPaletteEntry, BspCluster,
    BspClusterPortal, BspCollisionMaterial, BspInstance, BspInstanceDefinition, BspLeaf,
    CameraFxPaletteFlags, CollisionLeafFlags, CollisionSurfaceFlags, StructureBspClusterPortalFlags,
    BspMarker, BspMaterial, BspMeshMetadata, BspMeshPart, BspWeatherPaletteEntry,
    CollisionBsp2dNode, CollisionBsp2dReference, CollisionEdge, CollisionLeaf, CollisionSurface,
    CollisionVertex, InstancedGeometryFlags, InstancedGeometryLightmappingPolicy,
    InstancedGeometryPathfindingPolicy, StructureBsp, StructureBspError, StructureBspFlags,
    StructureClusterFlags, StructureSurface, StructureSurfaceTriangleMapping,
};
