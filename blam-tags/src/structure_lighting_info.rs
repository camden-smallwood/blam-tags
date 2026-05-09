//! `scenario_structure_lighting_info` (`stli`) tag walker — the per-BSP
//! authored dynamic-light table. Pointed at by the sbsp's
//! `structure lighting_info^` tag-ref.
//!
//! Two parallel blocks:
//!   - `generic light definitions[]` — light "type" records (color,
//!     intensity, shape, falloff bounds). Reusable.
//!   - `generic light instances[]` — placements that point back into
//!     a definition + carry origin/forward/up.
//!
//! Schema reference: `definitions/halo3_mcc/scenario_structure_lighting_info.json`.
//! Engine source file (per JSON `source_file`):
//! `c:\mcc\release\h3\source\structures\structure_lighting_definitions.cpp`.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealBounds, RealPoint3d, RealRgbColor, RealVector3d};

#[derive(Debug)]
pub enum StructureLightingInfoError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for StructureLightingInfoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "expected group '{}', got '{}'",
                std::str::from_utf8(expected).unwrap_or("?"),
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
        }
    }
}

impl std::error::Error for StructureLightingInfoError {}

const STRUCTURE_LIGHTING_INFO_GROUP: [u8; 4] = *b"stli";

/// `structure_lighting_generic_light_type_enum` (schema `enums_flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum GenericLightType {
    #[default]
    Omni = 0,
    Spot = 1,
    Directional = 2,
    Ambient = 3,
}

impl GenericLightType {
    pub fn from_name(name: &str) -> Self {
        match name {
            "omni" => Self::Omni,
            "spot" => Self::Spot,
            "directional" => Self::Directional,
            "ambient" => Self::Ambient,
            _ => Self::Omni,
        }
    }
}

/// `structure_lighting_generic_light_shape_enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum GenericLightShape {
    #[default]
    Rectangle = 0,
    Circle = 1,
}

impl GenericLightShape {
    pub fn from_name(name: &str) -> Self {
        match name {
            "circle" => Self::Circle,
            _ => Self::Rectangle,
        }
    }
}

/// `structure_lighting_generic_light_flags` (word_flags). Bit 0 =
/// use near attenuation; bit 1 = use far attenuation.
pub mod generic_light_flags {
    pub const USE_NEAR_ATTENUATION: u16 = 1 << 0;
    pub const USE_FAR_ATTENUATION: u16 = 1 << 1;
}

/// One entry of `generic light definitions[]`.
#[derive(Debug, Clone, Default)]
pub struct GenericLightDefinition {
    pub light_type: GenericLightType,
    pub flags: u16,
    pub shape: GenericLightShape,
    /// HDR color (linear, may exceed 1.0 with intensity).
    pub color: RealRgbColor,
    /// Multiplier on color (HDR knob).
    pub intensity: f32,
    /// Spot-light cone half-angle, radians. The fully-bright inner
    /// cone (no attenuation between center and this angle).
    pub hotspot_size: f32,
    /// Spot-light outer cone half-angle, radians. Past this angle the
    /// light contributes nothing.
    pub hotspot_falloff_size: f32,
    /// Inner near-attenuation distance band (only when
    /// `USE_NEAR_ATTENUATION` is set).
    pub near_attenuation_bounds: RealBounds,
    /// Outer far-attenuation distance band — `.upper` is the
    /// effective max distance; we use it to compute
    /// `bounding_radius2` for simple-light culling.
    pub far_attenuation_bounds: RealBounds,
    /// Aspect ratio for rectangular spots (currently unused — only
    /// shape=circle has runtime support in the simple-light path).
    pub aspect: f32,
}

impl GenericLightDefinition {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let light_type = s
            .read_enum_name("type")
            .map(|n| GenericLightType::from_name(&n))
            .unwrap_or_default();
        let shape = s
            .read_enum_name("shape")
            .map(|n| GenericLightShape::from_name(&n))
            .unwrap_or_default();
        Self {
            light_type,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            shape,
            color: s.read_rgb("color"),
            intensity: s.read_real("intensity").unwrap_or(0.0),
            hotspot_size: s.read_real("hotspot size").unwrap_or(0.0),
            hotspot_falloff_size: s.read_real("hotspot falloff size").unwrap_or(0.0),
            near_attenuation_bounds: s.read_real_bounds("near attenuation bounds"),
            far_attenuation_bounds: s.read_real_bounds("far attenuation bounds"),
            aspect: s.read_real("aspect").unwrap_or(1.0),
        }
    }

    pub fn uses_far_attenuation(&self) -> bool {
        (self.flags & generic_light_flags::USE_FAR_ATTENUATION) != 0
    }

    pub fn uses_near_attenuation(&self) -> bool {
        (self.flags & generic_light_flags::USE_NEAR_ATTENUATION) != 0
    }
}

/// One entry of `generic light instances[]`.
#[derive(Debug, Clone, Default)]
pub struct GenericLightInstance {
    /// Block-relative index into `generic_light_definitions`.
    pub definition_index: i32,
    pub origin: RealPoint3d,
    /// Light's primary axis (= -light_direction for a spotlight).
    pub forward: RealVector3d,
    pub up: RealVector3d,
}

impl GenericLightInstance {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            definition_index: s.read_int_any("definition index").unwrap_or(-1) as i32,
            origin: s.read_point3d("origin"),
            forward: s.read_vec3("forward"),
            up: s.read_vec3("up"),
        }
    }
}

/// One entry of `regions[]` — a named region of the BSP that
/// receives a specific dynamic-light contribution. Each region
/// carries a triangle list defining its volume.
#[derive(Debug, Clone, Default)]
pub struct StructureLightingRegion {
    pub name: String,
    pub triangles: Vec<StructureLightingRegionTriangle>,
}

impl StructureLightingRegion {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let triangles = s
            .field("triangles")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(StructureLightingRegionTriangle::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();
        Self {
            name: s.read_long_string("name").unwrap_or_default(),
            triangles,
        }
    }
}

/// One region triangle (3 world-space vertices).
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureLightingRegionTriangle {
    pub v0: RealPoint3d,
    pub v1: RealPoint3d,
    pub v2: RealPoint3d,
}

impl StructureLightingRegionTriangle {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            v0: s.read_point3d("v0"),
            v1: s.read_point3d("v1"),
            v2: s.read_point3d("v2"),
        }
    }
}

/// Bit flags for [`StructureMaterialLighting::flags`]. Real bit
/// meanings come from the schema enum
/// `structure_material_lighting_info_flags`. Values surfaced as the
/// raw u32 — caller decides which bits matter.
#[derive(Debug, Clone, Copy, Default)]
pub struct StructureMaterialLighting {
    /// Emissive radiated power (in Watts/m² in the editor's
    /// physically-based units).
    pub emissive_power: f32,
    /// Emissive color tint.
    pub emissive_color: RealRgbColor,
    /// Schema-named "quality" — sample-density / build-quality knob.
    pub emissive_quality: f32,
    /// Forward-emission focus exponent (cos^N falloff). 0 = lambertian.
    pub emissive_focus: f32,
    /// `structure_material_lighting_info_flags` raw bits.
    pub flags: u32,
    /// Distance attenuation rolloff start.
    pub attenuation_falloff: f32,
    /// Distance at which the contribution reaches zero.
    pub attenuation_cutoff: f32,
    /// Frustum mask blend smoothness in `[0, 1]`.
    pub frustum_blend: f32,
    /// Spot-cone falloff inner half-angle (radians).
    pub frustum_falloff_angle: f32,
    /// Spot-cone outer half-angle (radians). Schema mis-spells as
    /// "frustum cutoffoff angle" — both forms accepted.
    pub frustum_cutoff_angle: f32,
}

impl StructureMaterialLighting {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            emissive_power: s.read_real("emissive power").unwrap_or(0.0),
            emissive_color: s.read_rgb("emissive color"),
            emissive_quality: s.read_real("emissive quality").unwrap_or(0.0),
            emissive_focus: s.read_real("emissive focus").unwrap_or(0.0),
            flags: s.read_int_any("flags").unwrap_or(0) as u32,
            attenuation_falloff: s.read_real("attenuation falloff").unwrap_or(0.0),
            attenuation_cutoff: s.read_real("attenuation cutoff").unwrap_or(0.0),
            frustum_blend: s.read_real("frustum blend").unwrap_or(0.0),
            frustum_falloff_angle: s.read_real("frustum falloff angle").unwrap_or(0.0),
            // Schema mis-spells the cutoff field as "cutoffoff" — try
            // both forms in case different builds correct it.
            frustum_cutoff_angle: s
                .read_real("frustum cutoffoff angle")
                .or_else(|| s.read_real("frustum cutoff angle"))
                .unwrap_or(0.0),
        }
    }
}

/// Decoded `.scenario_structure_lighting_info` tag.
#[derive(Debug, Clone, Default)]
pub struct StructureLightingInfo {
    pub import_info_checksum: i32,
    pub generic_light_definitions: Vec<GenericLightDefinition>,
    pub generic_light_instances: Vec<GenericLightInstance>,
    /// Named per-region triangle volumes that receive specific
    /// dynamic-light contributions during the lightmap bake.
    pub regions: Vec<StructureLightingRegion>,
    /// Per-material emissive lighting parameters. Drives the offline
    /// tool.exe lightmap bake's emissive contributions; runtime
    /// usage is via the baked atlas, not these fields directly.
    pub material_info: Vec<StructureMaterialLighting>,
}

impl StructureLightingInfo {
    pub fn from_tag(tag: &TagFile) -> Result<Self, StructureLightingInfoError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != STRUCTURE_LIGHTING_INFO_GROUP {
            return Err(StructureLightingInfoError::WrongGroup {
                expected: STRUCTURE_LIGHTING_INFO_GROUP,
                actual,
            });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let definitions = s
            .field("generic light definitions")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(GenericLightDefinition::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();

        let instances = s
            .field("generic light instances")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(GenericLightInstance::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();

        let regions = s
            .field("regions")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(StructureLightingRegion::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();

        let material_info = s
            .field("material info")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(StructureMaterialLighting::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();

        Self {
            import_info_checksum: s
                .read_int_any("import info checksum")
                .unwrap_or(0) as i32,
            generic_light_definitions: definitions,
            generic_light_instances: instances,
            regions,
            material_info,
        }
    }
}
