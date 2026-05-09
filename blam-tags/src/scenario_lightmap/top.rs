//! Top-level `scenario_lightmap` (`sLdT`) tag walker.
//!
//! `sLdT` is the per-scenario master lightmap that bundles:
//!  - per-pixel and per-vertex `scenario_lightmap_bsp_data` (`Lbsp`)
//!    tag refs (one per active BSP)
//!  - global airprobes / scenery probes / device-machine-probe data
//!    (some scenarios author these here rather than per-BSP)
//!
//! Engine: `c_player_view::resolve_lightmap` walks the active
//! scenario's `new lightmaps` tag-ref to this `sLdT`, then resolves
//! the per-bsp `Lbsp` references for the active zone set.
//!
//! Schema (H3 MCC):
//! ```text
//! scenario_lightmap_block_struct  size=76B
//!   long_integer  job guid
//!   block  per-pxiel lightmap BSPs   → tag_ref(Lbsp)
//!   block  per-vertex lightmap BSPs  → tag_ref(Lbsp)
//!   block  airprobes                 → scenario_lightmap_airprobe_value
//!   block  scenery probes            → scenario_lightmap_scenery_probe_value
//!   block  device machine probes     → scenario_lightmap_device_machine_probe_data_value
//!   block  lightmap BSP's            → obsolete_scenario_lightmap_bsp_data  // skipped
//! ```

use crate::api::TagStruct;
use crate::file::TagFile;

use super::types::{
    LightmapAirprobe, LightmapDeviceMachineProbeData, LightmapError, LightmapSceneryProbe,
};

const SLDT_GROUP: [u8; 4] = *b"sLdT";

/// Top-level `.scenario_lightmap` (`sLdT`).
#[derive(Debug, Clone, Default)]
pub struct ScenarioLightmap {
    /// `job guid` — bake-job identifier (informational).
    pub job_guid: i32,
    /// Per-pixel-mode BSP-data tag references. Each entry is a path
    /// to a `.scenario_lightmap_bsp_data` tag.
    pub per_pixel_bsp_data: Vec<String>,
    /// Per-vertex-mode BSP-data tag references.
    pub per_vertex_bsp_data: Vec<String>,
    /// Global airprobes — manually-placed lighting samples that
    /// apply across the whole scenario rather than a single BSP.
    pub airprobes: Vec<LightmapAirprobe>,
    /// Global scenery probes — bound to scenery placements via
    /// [`super::types::ScenarioObjectId`].
    pub scenery_probes: Vec<LightmapSceneryProbe>,
    /// Global device-machine probe data containers. Each carries a
    /// bbox + nested per-position probes.
    pub device_machine_probes: Vec<LightmapDeviceMachineProbeData>,
}

impl ScenarioLightmap {
    pub fn from_tag(tag: &TagFile) -> Result<Self, LightmapError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != SLDT_GROUP {
            return Err(LightmapError::WrongGroup {
                expected: SLDT_GROUP,
                actual,
            });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        // Schema mis-spells "per-pxiel" — try both forms in case
        // different builds correct it.
        let per_pixel_bsp_data = read_bsp_refs(
            s,
            &["per-pxiel lightmap BSPs", "per-pixel lightmap BSPs"],
        );
        let per_vertex_bsp_data = read_bsp_refs(s, &["per-vertex lightmap BSPs"]);

        let airprobes = read_block(s, "airprobes", LightmapAirprobe::from_struct);
        let scenery_probes = read_block(s, "scenery probes", LightmapSceneryProbe::from_struct);
        let device_machine_probes = read_block(
            s,
            "device machine probes",
            LightmapDeviceMachineProbeData::from_struct,
        );

        Self {
            job_guid: s.read_int_any("job guid").unwrap_or(0) as i32,
            per_pixel_bsp_data,
            per_vertex_bsp_data,
            airprobes,
            scenery_probes,
            device_machine_probes,
        }
    }
}

fn read_bsp_refs(s: &TagStruct<'_>, names: &[&str]) -> Vec<String> {
    for name in names {
        if let Some(block) = s.field(name).and_then(|f| f.as_block()) {
            let mut out = Vec::with_capacity(block.len());
            for i in 0..block.len() {
                if let Some(elem) = block.element(i) {
                    if let Some(p) = elem
                        .read_tag_ref_path("lightmap bsp data reference")
                        .or_else(|| elem.read_tag_ref_path("lightmap bsp data"))
                    {
                        out.push(p);
                    }
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
    }
    Vec::new()
}

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|block| {
            let mut out = Vec::with_capacity(block.len());
            for i in 0..block.len() {
                if let Some(elem) = block.element(i) {
                    out.push(f(&elem));
                }
            }
            out
        })
        .unwrap_or_default()
}
