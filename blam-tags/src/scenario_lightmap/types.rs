//! `scenario_lightmap_bsp_data` (.scenario_lightmap_bsp_data) — per-BSP
//! baked lightmap data: compression vectors, lightprobe atlas refs,
//! per-cluster + per-instance probe assignments, plus inline SH probes.
//!
//! ## Lighting paths supported (MCC)
//!
//! Each cluster / instance / scenery placement carries a policy via
//! `lightprobe_texture_array_index` + `pervertex_block_index` +
//! `probe_block_index` (mutually exclusive — only one is non-(-1) per
//! entry, except texture+anything which combines):
//!
//! | What | Resolves via |
//! |---|---|
//! | Per-pixel | `lightprobe texture reference` (atlas) sampled with mesh's lightmap UVs at the assigned `lightprobe_texture_array_index` slice. |
//! | Per-vertex | `bsp_per_vertex_data[pervertex_block_index]` — vertex-buffer-bound SH stream. |
//! | Single-probe | `probes[probe_block_index]` — order-3 SH (9 i16 coefs per channel) + dominant_light_direction + intensity. Used for instances + scenery placements. |
//!
//! Reference: `Ares/source/scenario/scenario_lightmap_definitions.h:90-104`.
//! Note that MCC stores **order-3** SH (9 coeffs / channel) for single
//! probes, while Ares older versions used order-2 (4 coefs / channel).

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::math::{RealPoint3d, RealVector3d};

const SCNL_BSP_GROUP: [u8; 4] = *b"Lbsp";

#[derive(Debug)]
pub enum LightmapError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for LightmapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "scenario_lightmap_bsp_data: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for LightmapError {}

// =============================================================================
// Top-level
// =============================================================================

/// Per-BSP lightmap data — references the lightprobe atlas + per-cluster /
/// instance / scenery probes.
#[derive(Debug, Clone, Default)]
pub struct LightmapBspData {
    /// Bitmask: 0x1 compressed, 0x4 relightmapped, etc.
    pub flags: u16,
    /// Index of the BSP this lightmap applies to (matches scenario's
    /// structure_bsps[i]).
    pub bsp_reference_index: i16,
    /// Checksum from when the lightmap was baked — should match the
    /// BSP's `import info checksum` for valid pairing.
    pub structure_bsp_import_checksum: i32,

    /// 18 compression vectors used to dequantize SH coefficients in
    /// `probes[]` and per-vertex blocks. The runtime decoder multiplies
    /// each i16 coefficient by the matching compression vector.
    pub compression_vectors: [RealVector3d; 18],

    /// `.bitmap` of the lightprobe RGB SH atlas. Each texel holds 18
    /// half-float values (9 SH × 3 channels OR 4 SH × 3 channels at
    /// lower order; format documented in MCC tooling).
    pub lightprobe_texture: String,
    /// `.bitmap` of the dominant-light direction + intensity atlas.
    pub dominant_light_intensity_texture: String,

    /// Per-cluster lightmap policy. `clusters[i]` corresponds to
    /// `structure_bsp.clusters[i]`.
    pub clusters: Vec<LightmapClusterEntry>,

    /// Per-instance lightmap policy. `instances[i]` corresponds to
    /// `structure_bsp.instanced_geometry_instances[i]`.
    pub instances: Vec<LightmapInstanceEntry>,

    /// Single-probe SH coefficients — referenced by
    /// `instances[].probe_block_index`. Index = block index into here.
    pub probes: Vec<LightmapProbe>,

    /// Per-vertex SH blocks — referenced by
    /// `clusters[].pervertex_block_index` and
    /// `instances[].pervertex_block_index`. Each block is a flat list of
    /// per-vertex SH samples for the matching mesh's vertex buffer.
    pub bsp_per_vertex_data: Vec<LightmapPerVertexBlock>,

    /// Per-scenery-placement probes — each carries a
    /// [`ScenarioObjectId`] header (placement reference) plus the
    /// SH coefficients. One per `scenario.scenery[i]`.
    pub scenery_probes: Vec<LightmapSceneryProbe>,

    /// Per-airprobe single probes (manually-placed lighting samples).
    /// Each carries position + name + `manual_bsp_flags` plus the
    /// SH coefficients.
    pub airprobes: Vec<LightmapAirprobe>,

    /// Per-machine-placement probe DATA: [`ScenarioObjectId`] +
    /// world-space bounding box + a list of nested per-position
    /// probes. NOT a single probe like the schema name might
    /// suggest — `device_machine_probe_data` is a CONTAINER.
    pub device_machine_probes: Vec<LightmapDeviceMachineProbeData>,
}

impl LightmapBspData {
    pub fn from_tag(tag: &TagFile) -> Result<Self, LightmapError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != SCNL_BSP_GROUP {
            return Err(LightmapError::WrongGroup {
                expected: SCNL_BSP_GROUP,
                actual,
            });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let mut compression_vectors = [RealVector3d::default(); 18];
        if let Some(arr) = s.field("compression vectors").and_then(|f| f.as_array()) {
            for i in 0..arr.len().min(18) {
                if let Some(elem) = arr.element(i) {
                    compression_vectors[i] = elem.read_vec3("vector");
                }
            }
        } else if let Some(b) = s.field("compression vectors").and_then(|f| f.as_block()) {
            for i in 0..b.len().min(18) {
                if let Some(e) = b.element(i) {
                    compression_vectors[i] = e.read_vec3("vector");
                }
            }
        }

        Self {
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            bsp_reference_index: s.read_int_any("bsp reference index").unwrap_or(-1) as i16,
            structure_bsp_import_checksum: s
                .read_int_any("structure BSP import checksum")
                .unwrap_or(0) as i32,
            compression_vectors,

            lightprobe_texture: s.read_tag_ref_path("lightprobe texture reference").unwrap_or_default(),
            dominant_light_intensity_texture: s
                .read_tag_ref_path("dominant light intensity texture reference")
                .unwrap_or_default(),

            clusters: read_block(s, "clusters", LightmapClusterEntry::from_struct),
            instances: read_block(s, "instances", LightmapInstanceEntry::from_struct),
            probes: read_block(s, "probes", LightmapProbe::from_struct),
            bsp_per_vertex_data: read_block(
                s,
                "bsp per-vertex data",
                LightmapPerVertexBlock::from_struct,
            ),
            scenery_probes: read_block(s, "scenery probes", LightmapSceneryProbe::from_struct),
            airprobes: read_block(s, "airprobes", LightmapAirprobe::from_struct),
            device_machine_probes: read_block(
                s,
                "device machine probes",
                LightmapDeviceMachineProbeData::from_struct,
            ),
        }
    }
}

// =============================================================================
// Sub-blocks
// =============================================================================

/// One cluster's lightmap policy. Texture-array mode and per-vertex
/// mode are mutually exclusive: only one of the two indices is non-(-1).
#[derive(Debug, Clone, Copy, Default)]
pub struct LightmapClusterEntry {
    /// Slice index into `lightprobe_texture` (-1 = no per-pixel lightmap).
    pub lightprobe_texture_array_index: i16,
    /// Block index into `bsp_per_vertex_data` (-1 = no per-vertex SH).
    pub pervertex_block_index: i16,
}

impl LightmapClusterEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            lightprobe_texture_array_index: s
                .read_int_any("lightprobe texture array index")
                .unwrap_or(-1) as i16,
            pervertex_block_index: s.read_int_any("pervertex block index").unwrap_or(-1) as i16,
        }
    }

    /// Selected lighting policy for this entry (in order of precedence).
    pub fn policy(&self) -> LightmapPolicy {
        if self.lightprobe_texture_array_index >= 0 {
            LightmapPolicy::PerPixel
        } else if self.pervertex_block_index >= 0 {
            LightmapPolicy::PerVertex
        } else {
            LightmapPolicy::Fallback
        }
    }
}

/// One instance's lightmap policy. Like cluster, but instances also
/// support a single-probe path via `probe_block_index`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LightmapInstanceEntry {
    pub lightprobe_texture_array_index: i16,
    pub pervertex_block_index: i16,
    /// Block index into `probes[]` (-1 = no single probe assignment).
    pub probe_block_index: i16,
}

impl LightmapInstanceEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            lightprobe_texture_array_index: s
                .read_int_any("lightprobe texture array index")
                .unwrap_or(-1) as i16,
            pervertex_block_index: s.read_int_any("pervertex block index").unwrap_or(-1) as i16,
            probe_block_index: s.read_int_any("probe block index").unwrap_or(-1) as i16,
        }
    }

    pub fn policy(&self) -> LightmapPolicy {
        if self.lightprobe_texture_array_index >= 0 {
            LightmapPolicy::PerPixel
        } else if self.pervertex_block_index >= 0 {
            LightmapPolicy::PerVertex
        } else if self.probe_block_index >= 0 {
            LightmapPolicy::SingleProbe
        } else {
            LightmapPolicy::Fallback
        }
    }
}

/// Lighting evaluation path, selected per cluster / instance / object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LightmapPolicy {
    /// Sample from lightprobe atlas with per-pixel UVs.
    PerPixel,
    /// Per-vertex SH stream interpolated to fragment.
    PerVertex,
    /// One pre-baked SH probe (instance / scenery / airprobe).
    SingleProbe,
    /// No lightmap data — engine uses sky-default lightprobe fallback.
    Fallback,
}

/// One single-probe SH sample: order-3 RGB SH (9 coefs / channel) +
/// dominant-light direction + intensity. Quantized as i16 — multiply
/// by the appropriate `compression_vectors[i]` to dequantize.
#[derive(Debug, Clone, Copy, Default)]
pub struct LightmapProbe {
    /// `dominant light direction i/j/k` — quantized direction.
    pub dominant_light_direction: [i16; 3],
    /// `dominant light intensity r/g/b` — quantized RGB intensity.
    pub dominant_light_intensity: [i16; 3],
    /// `red/green/blue lightprobe terms[0..9]` — quantized SH order-3
    /// coefficients per channel.
    pub red_terms: [i16; 9],
    pub green_terms: [i16; 9],
    pub blue_terms: [i16; 9],
}

impl LightmapProbe {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let mut probe = Self::default();
        probe.dominant_light_direction = [
            s.read_int_any("dominant light direction i").unwrap_or(0) as i16,
            s.read_int_any("dominant light direction j").unwrap_or(0) as i16,
            s.read_int_any("dominant light direction k").unwrap_or(0) as i16,
        ];
        probe.dominant_light_intensity = [
            s.read_int_any("dominant light intensity r").unwrap_or(0) as i16,
            s.read_int_any("dominant light intensity g").unwrap_or(0) as i16,
            s.read_int_any("dominant light intensity b").unwrap_or(0) as i16,
        ];
        read_short_array(s, "red lightprobe terms", &mut probe.red_terms);
        read_short_array(s, "green lightprobe terms", &mut probe.green_terms);
        read_short_array(s, "blue lightprobe terms", &mut probe.blue_terms);
        probe
    }

    /// Dequantize from the on-disk half-float bit pattern.
    /// `LightmapProbe` reads the values as `i16` (the schema says
    /// "short"), but the bytes are actually IEEE half floats — see
    /// dllcache `real_rgb_lightprobe_from_half @ 0x180519A20` and
    /// `half_to_real`. Reinterpret as `half::f16` and widen.
    pub fn dequantize(&self) -> DequantizedLightmapProbe {
        let half_to_f32 = |x: i16| -> f32 {
            half::f16::from_bits(x as u16).to_f32()
        };
        DequantizedLightmapProbe {
            dominant_light_direction: [
                half_to_f32(self.dominant_light_direction[0]),
                half_to_f32(self.dominant_light_direction[1]),
                half_to_f32(self.dominant_light_direction[2]),
            ],
            dominant_light_intensity: [
                half_to_f32(self.dominant_light_intensity[0]),
                half_to_f32(self.dominant_light_intensity[1]),
                half_to_f32(self.dominant_light_intensity[2]),
            ],
            red_terms: std::array::from_fn(|i| half_to_f32(self.red_terms[i])),
            green_terms: std::array::from_fn(|i| half_to_f32(self.green_terms[i])),
            blue_terms: std::array::from_fn(|i| half_to_f32(self.blue_terms[i])),
        }
    }
}

/// `LightmapProbe` with all i16 half bit-patterns expanded to f32 —
/// the form the runtime reads after `real_rgb_lightprobe_from_half`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DequantizedLightmapProbe {
    pub dominant_light_direction: [f32; 3],
    pub dominant_light_intensity: [f32; 3],
    pub red_terms: [f32; 9],
    pub green_terms: [f32; 9],
    pub blue_terms: [f32; 9],
}

/// `scenario_object_id_struct` (8B) — a placement reference inside
/// the scenario tag. Identifies which placement a probe belongs to.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScenarioObjectId {
    /// Globally-unique placement id.
    pub unique_id: i32,
    /// Block index of the BSP this placement originates in (-1 if
    /// not bound to a specific BSP).
    pub origin_bsp_index: i16,
    /// `e_object_type` value (scenery, biped, vehicle, …).
    pub object_type: i8,
    /// Authoring source enum (auto-placed vs manual vs script-spawned).
    pub source: i8,
}

impl ScenarioObjectId {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            unique_id: s.read_int_any("unique id").unwrap_or(0) as i32,
            origin_bsp_index: s.read_block_index("origin bsp index"),
            object_type: s.read_int_any("type").unwrap_or(-1) as i8,
            source: s.read_int_any("source").unwrap_or(-1) as i8,
        }
    }
}

/// One airprobe entry — manually-placed in the editor for spotty
/// fill light. Has world-space position + name + manual-bsp flags
/// plus the standard SH probe payload.
#[derive(Debug, Clone, Default)]
pub struct LightmapAirprobe {
    pub position: RealPoint3d,
    pub name: String,
    pub manual_bsp_flags: i16,
    pub probe: LightmapProbe,
}

impl LightmapAirprobe {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            position: s.read_point3d("airprobe position"),
            name: s.read_string_id("airprobe name").unwrap_or_default(),
            manual_bsp_flags: s.read_int_any("manual bsp flags").unwrap_or(0) as i16,
            probe: LightmapProbe::from_struct(s),
        }
    }
}

/// One scenery-placement probe — bound to a specific scenery
/// placement via [`ScenarioObjectId`] + the SH probe payload.
#[derive(Debug, Clone, Default)]
pub struct LightmapSceneryProbe {
    pub object_id: ScenarioObjectId,
    pub probe: LightmapProbe,
}

impl LightmapSceneryProbe {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let object_id = s
            .field("object id")
            .and_then(|f| f.as_struct())
            .map(|sub| ScenarioObjectId::from_struct(&sub))
            .unwrap_or_default();
        Self {
            object_id,
            probe: LightmapProbe::from_struct(s),
        }
    }
}

/// One device-machine probe data block — bound to a device_machine
/// placement, defines a world-space bounding box, and carries a
/// list of per-position probes inside that box. The schema name
/// `_value` is misleading: this is a CONTAINER, not a single probe.
#[derive(Debug, Clone, Default)]
pub struct LightmapDeviceMachineProbeData {
    pub object_id: ScenarioObjectId,
    /// World-space bounding box `[x0, x1, y0, y1, z0, z1]`. Schema
    /// stores 6 reals; we keep them in axis-pair order.
    pub bbox: [f32; 6],
    /// Per-position probes within the bbox.
    pub probes: Vec<LightmapDeviceMachineProbe>,
}

impl LightmapDeviceMachineProbeData {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let object_id = s
            .field("object id")
            .and_then(|f| f.as_struct())
            .map(|sub| ScenarioObjectId::from_struct(&sub))
            .unwrap_or_default();
        let bbox = [
            s.read_real("x0").unwrap_or(0.0),
            s.read_real("x1").unwrap_or(0.0),
            s.read_real("y0").unwrap_or(0.0),
            s.read_real("y1").unwrap_or(0.0),
            s.read_real("z0").unwrap_or(0.0),
            s.read_real("z1").unwrap_or(0.0),
        ];
        Self {
            object_id,
            bbox,
            probes: read_block(s, "probes", LightmapDeviceMachineProbe::from_struct),
        }
    }
}

/// One device-machine probe — world-space position + SH probe
/// payload. Nested inside [`LightmapDeviceMachineProbeData::probes`].
#[derive(Debug, Clone, Default)]
pub struct LightmapDeviceMachineProbe {
    pub position: RealPoint3d,
    pub probe: LightmapProbe,
}

impl LightmapDeviceMachineProbe {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            position: s.read_point3d("position"),
            probe: LightmapProbe::from_struct(s),
        }
    }
}

/// One per-vertex SH block — a flat list of vertex-aligned probes.
/// Index in [`LightmapBspData::bsp_per_vertex_data`] is what
/// cluster/instance entries reference.
///
/// Each entry in `lightprobe_data[]` is the per-vertex equivalent of
/// [`LightmapProbe`] but typically with order-2 SH (4 coefs / channel)
/// for memory efficiency.
#[derive(Debug, Clone, Default)]
pub struct LightmapPerVertexBlock {
    pub lightprobe_data: Vec<LightmapPerVertexProbe>,
    /// Runtime hint — index of the vertex buffer the per-vertex SH
    /// stream is bound to. Engine uses this to pick the right
    /// cluster mesh's vertex buffer when sampling per-vertex SH.
    pub vertex_buffer_index: i32,
}

impl LightmapPerVertexBlock {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            lightprobe_data: read_block(
                s,
                "lightprobe data",
                LightmapPerVertexProbe::from_struct,
            ),
            vertex_buffer_index: s
                .read_int_any("light probe vertex buffer index")
                .unwrap_or(-1) as i32,
        }
    }
}

/// One per-vertex SH probe entry. Order-2 SH: 4 coefs per channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct LightmapPerVertexProbe {
    pub dominant_light_intensity: [i16; 3],
    pub red_terms: [i16; 4],
    pub green_terms: [i16; 4],
    pub blue_terms: [i16; 4],
}

impl LightmapPerVertexProbe {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let mut p = Self::default();
        p.dominant_light_intensity = [
            s.read_int_any("dominant light intensity r").unwrap_or(0) as i16,
            s.read_int_any("dominant light intensity g").unwrap_or(0) as i16,
            s.read_int_any("dominant light intensity b").unwrap_or(0) as i16,
        ];
        read_short_array(s, "red lightprobe terms", &mut p.red_terms);
        read_short_array(s, "green lightprobe terms", &mut p.green_terms);
        read_short_array(s, "blue lightprobe terms", &mut p.blue_terms);
        p
    }

    /// Dequantize from on-disk half-float bit patterns. Order-2 SH (4
    /// coefs / channel). Same encoding rule as [`LightmapProbe`].
    pub fn dequantize(&self) -> DequantizedPerVertexProbe {
        let h2f = |x: i16| half::f16::from_bits(x as u16).to_f32();
        DequantizedPerVertexProbe {
            dominant_light_intensity: [
                h2f(self.dominant_light_intensity[0]),
                h2f(self.dominant_light_intensity[1]),
                h2f(self.dominant_light_intensity[2]),
            ],
            red_terms: std::array::from_fn(|i| h2f(self.red_terms[i])),
            green_terms: std::array::from_fn(|i| h2f(self.green_terms[i])),
            blue_terms: std::array::from_fn(|i| h2f(self.blue_terms[i])),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DequantizedPerVertexProbe {
    pub dominant_light_intensity: [f32; 3],
    pub red_terms: [f32; 4],
    pub green_terms: [f32; 4],
    pub blue_terms: [f32; 4],
}

// =============================================================================
// Helpers
// =============================================================================

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|b| read_block_vec(&b, f))
        .unwrap_or_default()
}

fn read_block_vec<T, F>(block: &TagBlock<'_>, f: F) -> Vec<T>
where
    F: Fn(&TagStruct<'_>) -> T,
{
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}

/// Read a fixed-size array-of-i16-coefficient field (the per-channel
/// SH "lightprobe terms" arrays). Each element is a struct with a
/// single `coefficient: short integer` field.
fn read_short_array<const N: usize>(s: &TagStruct<'_>, name: &str, out: &mut [i16; N]) {
    if let Some(arr) = s.field(name).and_then(|f| f.as_array()) {
        for i in 0..arr.len().min(N) {
            if let Some(elem) = arr.element(i) {
                out[i] = elem.read_int_any("coefficient").unwrap_or(0) as i16;
            }
        }
    } else if let Some(b) = s.field(name).and_then(|f| f.as_block()) {
        for i in 0..b.len().min(N) {
            if let Some(e) = b.element(i) {
                out[i] = e.read_int_any("coefficient").unwrap_or(0) as i16;
            }
        }
    }
}
