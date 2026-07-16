//! `contrail_system` (`cntl`) tag walker — collection of contrail
//! definitions referenced by `effect_part_definition` with
//! `runtime_tag_reference_base_class_tag = b"cntl"`.
//!
//! Runtime: `c_contrail_system::create @ 0x1804919E0`, submit_all @
//! 0x180491E90, render @ 0x180493930. Shader is
//! c_render_method_shader_contrail (rmct stem). HLSL:
//! contrail_render_hlsl.hlsl, _spawn_, _update_. Profile direction
//! derived from neighbor profile per-frame (per HLSL :163-169).
//!
//! Schema: `definitions/halo3_mcc/contrail_system.json`.

use crate::api::TagStruct;
use crate::effects_properties::EditableProperty;
use crate::file::TagFile;
use crate::math::RealVector2d;
use crate::render_method::{RenderMethod, RenderMethodError};
use crate::typed_enums::{Enum, Flags};

const CNTL_GROUP: [u8; 4] = *b"cntl";

#[derive(Debug)]
pub enum ContrailSystemError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
    RenderMethod(RenderMethodError),
}

impl std::fmt::Display for ContrailSystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "contrail_system: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
            Self::RenderMethod(e) => write!(f, "contrail_system: actual shader: {e}"),
        }
    }
}

impl std::error::Error for ContrailSystemError {}

impl From<RenderMethodError> for ContrailSystemError {
    fn from(e: RenderMethodError) -> Self {
        Self::RenderMethod(e)
    }
}

/// `profile shape` enum — same 3 topologies as beam_system (ribbon /
/// cross / ngon). HLSL: `contrail_render_hlsl.hlsl:163-169` derives
/// direction from the next profile in the ring buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum ContrailProfileShape {
    #[default]
    #[strum(serialize = "aligned ribbon")] Ribbon = 0,
    #[strum(serialize = "cross")] Cross = 1,
    #[strum(serialize = "n-gon")] Ngon = 2,
}

/// `contrail_appearance_flags`.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum ContrailAppearanceFlags {
    #[strum(serialize = "tint from lightmap")] TintFromLightmap = 0,
    #[strum(serialize = "double-sided")] DoubleSided = 1,
    #[strum(serialize = "profile opacity from scale a")] ProfileOpacityFromScaleA = 2,
    #[strum(serialize = "random u offset")] RandomUOffset = 3,
    #[strum(serialize = "random v offset")] RandomVOffset = 4,
    /// Runtime-reconstructed (Ares `c_contrail_definition::get_origin_faded`
    /// tests bit 5): the trail head fades near its origin. Set by the contrail
    /// effect-postprocess at load; absent (0) in loose MCC tags, so
    /// `get_origin_faded` reads `false` until that reconstruction lands.
    #[strum(disabled)]
    OriginFaded = 5,
    /// Runtime-reconstructed (Ares `c_contrail_definition::get_fogged` tests
    /// bit 7): the contrail is fogged. Set by the contrail effect-postprocess at
    /// load; absent (0) in loose MCC tags, so `get_fogged` reads `false` until
    /// that reconstruction lands — never authored on disk.
    #[strum(disabled)]
    Fogged = 7,
}

/// One contrail definition entry (620B `c_contrail_definition`).
#[derive(Debug, Clone, Default)]
pub struct ContrailDefinition {
    pub name: String,
    /// Distance beyond cutoff over which the head of the trail fades.
    pub origin_fade_range: f32,
    pub origin_fade_cutoff: f32,
    pub edge_fade_range_degrees: f32,
    pub edge_fade_cutoff_degrees: f32,
    // ---- LOD ----
    pub lod_in_distance: f32,
    pub lod_feather_in_distance: f32,
    pub lod_out_distance: f32,
    pub lod_feather_out_distance: f32,
    // ---- 7 emission/motion property curves ----
    pub emission_rate: EditableProperty,
    pub profile_lifespan: EditableProperty,
    pub profile_self_acceleration: EditableProperty,
    pub profile_size: EditableProperty,
    pub profile_offset: EditableProperty,
    pub profile_rotation: EditableProperty,
    pub profile_rotation_rate: EditableProperty,
    // ---- appearance ----
    pub appearance_flags: Flags<ContrailAppearanceFlags, u16>,
    pub profile_shape: Enum<ContrailProfileShape, i8>,
    pub ngon_sides: i8,
    /// Embedded `c_render_method_shader_contrail` — group_tag stamped `b"rmct"`.
    pub shader: Option<RenderMethod>,
    pub uv_tiling: RealVector2d,
    pub uv_scrolling: RealVector2d,
    // ---- 6 appearance property curves ----
    pub profile_color: EditableProperty,
    pub profile_alpha: EditableProperty,
    /// `profile secondary alpha` (a.k.a. profile_alpha2 per Ares) —
    /// extra alpha lane for compound blends (e.g. soft edges).
    pub profile_secondary_alpha: EditableProperty,
    pub profile_black_point: EditableProperty,
    pub profile_palette: EditableProperty,
    pub profile_intensity: EditableProperty,
    // ---- runtime-resolved ----
    pub runtime_constant_per_profile_properties: i32,
    pub runtime_used_states: i32,
}

/// Walked `contrail_system` tag.
#[derive(Debug, Clone, Default)]
pub struct ContrailSystem {
    /// Up to 16 contrail definitions per system.
    pub definitions: Vec<ContrailDefinition>,
}

impl ContrailSystem {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ContrailSystemError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != CNTL_GROUP {
            return Err(ContrailSystemError::WrongGroup { expected: CNTL_GROUP, actual });
        }
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, ContrailSystemError> {
        let block = match s.field("contrails").and_then(|f| f.as_block()) {
            Some(b) => b,
            None => return Ok(Self::default()),
        };
        let mut definitions = Vec::with_capacity(block.len());
        for i in 0..block.len() {
            if let Some(elem) = block.element(i) {
                definitions.push(ContrailDefinition::from_struct(&elem)?);
            }
        }
        Ok(Self { definitions })
    }
}

impl ContrailDefinition {
    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, ContrailSystemError> {
        let shader_struct = s.descend("actual shader").or_else(|| s.descend("actual shader?"));
        let shader = match shader_struct {
            Some(view) => {
                let mut rm = RenderMethod::from_struct(&view)?;
                // Embedded contrail shader. NOTE: the old code stamped
                // `rmct` here, which is CORTANA's real group fourcc
                // (`_meta.json: rmct -> shader_cortana`) — a collision that
                // mis-routed contrail shaders onto the cortana path. The
                // real contrail slot is the embedded-only nominal `?rmc`
                // (0x3f leading byte; never on disk). Use the typed class
                // for dispatch and `?rmc` as the collision-free key.
                rm.class = crate::render_method::RenderMethodClass::Contrail;
                rm.group_tag = u32::from_be_bytes(*b"\x3frmc");
                Some(rm)
            }
            None => None,
        };

        let origin_fade_range = s
            .read_real("origin fade range")
            .or_else(|| s.read_real("origin fade distance"))
            .unwrap_or(0.0);

        Ok(Self {
            name: s
                .read_string_id("contrail name")
                .or_else(|| s.read_string_id("contrail name^"))
                .unwrap_or_default(),
            origin_fade_range,
            origin_fade_cutoff: s.read_real("origin fade cutoff").unwrap_or(0.0),
            edge_fade_range_degrees: s.read_real("edge fade range").unwrap_or(0.0),
            edge_fade_cutoff_degrees: s.read_real("edge fade cutoff").unwrap_or(0.0),
            lod_in_distance: s.read_real("lod in distance").unwrap_or(0.0),
            lod_feather_in_distance: s.read_real("lod feather in distance").unwrap_or(0.0),
            lod_out_distance: s.read_real("lod out distance").unwrap_or(0.0),
            lod_feather_out_distance: s.read_real("lod feather out distance").unwrap_or(0.0),
            emission_rate: read_property(s, "emission rate"),
            profile_lifespan: read_property(s, "profile lifespan"),
            profile_self_acceleration: read_property(s, "profile self acceleration"),
            profile_size: read_property(s, "profile size"),
            profile_offset: read_property(s, "profile offset"),
            profile_rotation: read_property(s, "profile rotation"),
            profile_rotation_rate: read_property(s, "profile rotation rate"),
            appearance_flags: s.try_read_flags("appearance flags").unwrap_or_default(),
            profile_shape: s.read_enum("profile shape"),
            ngon_sides: s.read_int_any("number of n-gon sides").unwrap_or(0) as i8,
            shader,
            uv_tiling: s.read_vec2("uv tiling"),
            uv_scrolling: s.read_vec2("uv scrolling"),
            profile_color: read_property(s, "profile color"),
            profile_alpha: read_property(s, "profile alpha"),
            profile_secondary_alpha: read_property(s, "profile secondary alpha"),
            profile_black_point: read_property(s, "profile black point"),
            profile_palette: read_property(s, "profile palette"),
            profile_intensity: read_property(s, "profile intensity"),
            runtime_constant_per_profile_properties: s
                .read_int_any("runtime m_constant_per_profile_properties")
                .unwrap_or(0) as i32,
            runtime_used_states: s.read_int_any("runtime m_used_states").unwrap_or(0) as i32,
        })
    }
}

fn read_property(parent: &TagStruct<'_>, name: &str) -> EditableProperty {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .map(|inner| EditableProperty::from_struct(&inner))
        .unwrap_or_default()
}
