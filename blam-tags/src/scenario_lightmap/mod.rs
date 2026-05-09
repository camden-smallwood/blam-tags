//! Halo `scenario_lightmap_bsp_data` walker — per-BSP baked SH probes,
//! lightprobe atlas refs, cluster / instance / scenery probe assignments.
//!
//! Reference: `Ares/source/scenario/scenario_lightmap_definitions.h:90`.

mod top;
mod types;

pub use top::ScenarioLightmap;
pub use types::{
    DequantizedLightmapProbe, DequantizedPerVertexProbe, LightmapAirprobe, LightmapBspData,
    LightmapClusterEntry, LightmapDeviceMachineProbe, LightmapDeviceMachineProbeData,
    LightmapError, LightmapInstanceEntry, LightmapPerVertexBlock, LightmapPerVertexProbe,
    LightmapPolicy, LightmapProbe, LightmapSceneryProbe, ScenarioObjectId,
};
