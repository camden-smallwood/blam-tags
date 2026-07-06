//! `beam_system` (`beam`) tag walker — collection of beam definitions
//! referenced by `effect_part_definition` with
//! `runtime_tag_reference_base_class_tag = b"beam"`.
//!
//! Runtime: `c_beam_system::create @ 0x18049B110`, submit_all @
//! 0x18049B650, render @ 0x18049CBF0. Shader is
//! c_render_method_shader_beam (rmb stem). HLSL: beam_fx.hlsl + 3
//! profile types (ribbon / cross / ngon).
//!
//! Schema: `definitions/halo3_mcc/beam_system.json`.

use crate::api::TagStruct;
use crate::effects_properties::EditableProperty;
use crate::file::TagFile;
use crate::math::RealVector2d;
use crate::render_method::{RenderMethod, RenderMethodError};
use crate::typed_enums::{Enum, Flags};

const BEAM_GROUP: [u8; 4] = *b"beam";

#[derive(Debug)]
pub enum BeamSystemError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
    RenderMethod(RenderMethodError),
}

impl std::fmt::Display for BeamSystemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "beam_system: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
            Self::RenderMethod(e) => write!(f, "beam_system: actual shader: {e}"),
        }
    }
}

impl std::error::Error for BeamSystemError {}

impl From<RenderMethodError> for BeamSystemError {
    fn from(e: RenderMethodError) -> Self {
        Self::RenderMethod(e)
    }
}

/// `beam_profile_shape_enum` — 3 cross-section topologies (HLSL `_profile_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum BeamProfileShape {
    /// Billboard-based, camera-facing 2-tri ribbon (HLSL `_profile_ribbon`).
    #[default]
    #[strum(serialize = "aligned ribbon")] Ribbon = 0,
    /// World-space + axis-aligned cross (4-tri cross).
    #[strum(serialize = "cross")] Cross = 1,
    /// N-sided polygon (n from `number_of_ngon_sides`).
    #[strum(serialize = "n-gon")] Ngon = 2,
}

/// `beam_appearance_flags`.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum BeamAppearanceFlags {
    #[strum(serialize = "double-sided")] DoubleSided = 0,
    /// Runtime-reconstructed (Ares `c_beam_definition::get_fogged` tests bit 3):
    /// the beam is fogged. Set by the beam effect-postprocess at load; absent
    /// (0) in loose MCC tags, so `get_fogged` reads `false` until that
    /// reconstruction lands — never authored on disk.
    #[strum(disabled)]
    Fogged = 3,
}

/// One beam definition entry (520B `c_beam_definition`).
#[derive(Debug, Clone, Default)]
pub struct BeamDefinition {
    pub name: String,
    /// Embedded `c_render_method_shader_beam` — group stamped to `b"rmb "`.
    pub shader: Option<RenderMethod>,
    pub appearance_flags: Flags<BeamAppearanceFlags, u16>,
    pub profile_shape: Enum<BeamProfileShape, i8>,
    /// n for n-gon profile shape (3..=N). Ignored for ribbon/cross.
    pub ngon_sides: i8,
    /// uv tiling — u along length (tiles/world unit), v across (absolute).
    pub uv_tiling: RealVector2d,
    /// uv scrolling — tiles per second (u, v).
    pub uv_scrolling: RealVector2d,
    /// Fade beyond `origin_fade_cutoff` world units from origin.
    pub origin_fade_range: f32,
    pub origin_fade_cutoff: f32,
    /// Fade beyond `edge_fade_cutoff` degrees from edge-on view.
    pub edge_fade_range_degrees: f32,
    pub edge_fade_cutoff_degrees: f32,
    // ---- 11 property curves ----
    pub length: EditableProperty,
    pub offset: EditableProperty,
    pub profile_density: EditableProperty,
    pub profile_offset: EditableProperty,
    pub profile_rotation: EditableProperty,
    pub profile_thickness: EditableProperty,
    pub profile_color: EditableProperty,
    pub profile_alpha: EditableProperty,
    pub profile_black_point: EditableProperty,
    pub profile_palette: EditableProperty,
    pub profile_intensity: EditableProperty,
    // ---- runtime-resolved ----
    pub runtime_constant_per_profile_properties: i32,
    pub runtime_used_states: i32,
    pub runtime_max_profile_count: i32,
    /// `runtime m_gpu_data!` (`gpu_property_function_color_struct`) — the baked
    /// per-profile GPU LUT triad. Stripped to empty in loose MCC tags
    /// (reconstructed at load); populated in cached `.map` tags.
    pub gpu_data: BeamGpuData,
}

/// `gpu_property_function_color_struct` (36B) — the beam's baked GPU
/// evaluation LUTs: property rows (`[f32;4]`), function rows (`[f32;16]`), and
/// color rows (`[f32;4]`). Shared shape with contrail / light_volume gpu_data.
#[derive(Debug, Clone, Default)]
pub struct BeamGpuData {
    /// `runtime gpu_property_block!` — one `[f32;4]` per baked property.
    pub properties: Vec<[f32; 4]>,
    /// `runtime gpu_functions_block!` — one `[f32;16]` per baked function.
    pub functions: Vec<[f32; 16]>,
    /// `runtime gpu_colors_block!` — one `[f32;4]` per baked color.
    pub colors: Vec<[f32; 4]>,
}

impl BeamGpuData {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            properties: read_gpu_rows::<4>(s, "runtime gpu_property_block"),
            functions: read_gpu_rows::<16>(s, "runtime gpu_functions_block"),
            colors: read_gpu_rows::<4>(s, "runtime gpu_colors_block"),
        }
    }
}

/// Read a `runtime gpu_*_block!` block: each element carries one inline
/// `runtime gpu_*_sub_array!` of `N` reals. Empty when the block is absent
/// (loose tags). Mirrors the particle `ParticleGpuData` sprite-corner read.
fn read_gpu_rows<const N: usize>(s: &TagStruct<'_>, block_name: &str) -> Vec<[f32; N]> {
    let block = match s.field(block_name).and_then(|f| f.as_block()) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(elem) = block.element(i) else { continue };
        let mut row = [0.0f32; N];
        if let Some(arr) = elem.fields_all().find_map(|f| f.as_struct()) {
            for (j, field) in arr.fields_all().enumerate().take(N) {
                if let Some(crate::fields::TagFieldData::Real(v)) = field.value() {
                    row[j] = v;
                }
            }
        }
        out.push(row);
    }
    out
}

/// Walked `beam_system` tag.
#[derive(Debug, Clone, Default)]
pub struct BeamSystem {
    /// Up to 16 beam definitions per system.
    pub definitions: Vec<BeamDefinition>,
}

impl BeamSystem {
    pub fn from_tag(tag: &TagFile) -> Result<Self, BeamSystemError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != BEAM_GROUP {
            return Err(BeamSystemError::WrongGroup { expected: BEAM_GROUP, actual });
        }
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, BeamSystemError> {
        let block = match s.field("beams").and_then(|f| f.as_block()) {
            Some(b) => b,
            None => return Ok(Self::default()),
        };
        let mut definitions = Vec::with_capacity(block.len());
        for i in 0..block.len() {
            if let Some(elem) = block.element(i) {
                definitions.push(BeamDefinition::from_struct(&elem)?);
            }
        }
        Ok(Self { definitions })
    }
}

impl BeamDefinition {
    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, BeamSystemError> {
        let shader_struct = s.descend("actual shader").or_else(|| s.descend("actual shader?"));
        let shader = match shader_struct {
            Some(view) => {
                let mut rm = RenderMethod::from_struct(&view)?;
                rm.class = crate::render_method::RenderMethodClass::Beam;
                rm.group_tag = u32::from_be_bytes(*b"rmb ");
                Some(rm)
            }
            None => None,
        };

        Ok(Self {
            name: s
                .read_string_id("beam name")
                .or_else(|| s.read_string_id("beam name^"))
                .unwrap_or_default(),
            shader,
            appearance_flags: s.try_read_flags("appearance flags").unwrap_or_default(),
            profile_shape: s.read_enum("profile shape"),
            ngon_sides: s.read_int_any("number of n-gon sides").unwrap_or(0) as i8,
            uv_tiling: s.read_vec2("uv tiling"),
            uv_scrolling: s.read_vec2("uv scrolling"),
            origin_fade_range: read_origin_fade_range(s),
            origin_fade_cutoff: s.read_real("origin fade cutoff").unwrap_or(0.0),
            edge_fade_range_degrees: s.read_real("edge fade range").unwrap_or(0.0),
            edge_fade_cutoff_degrees: s.read_real("edge fade cutoff").unwrap_or(0.0),
            length: read_property(s, "length"),
            offset: read_property(s, "offset"),
            profile_density: read_property(s, "profile_density"),
            profile_offset: read_property(s, "profile_offset"),
            profile_rotation: read_property(s, "profile_rotation"),
            profile_thickness: read_property(s, "profile_thickness"),
            profile_color: read_property(s, "profile_color"),
            profile_alpha: read_property(s, "profile_alpha"),
            profile_black_point: read_property(s, "profile_black_point"),
            profile_palette: read_property(s, "profile_palette"),
            profile_intensity: read_property(s, "profile_intensity"),
            runtime_constant_per_profile_properties: s
                .read_int_any("runtime m_constant_per_profile_properties")
                .unwrap_or(0) as i32,
            runtime_used_states: s.read_int_any("runtime m_used_states").unwrap_or(0) as i32,
            runtime_max_profile_count: s
                .read_int_any("runtime m_max_profile_count")
                .unwrap_or(0) as i32,
            gpu_data: s
                .field("runtime m_gpu_data")
                .and_then(|f| f.as_struct())
                .map(|sub| BeamGpuData::from_struct(&sub))
                .unwrap_or_default(),
        })
    }
}

/// Schema annotates this with `{origin fade distance}` alias; try
/// both spellings since cached tags may use either.
fn read_origin_fade_range(s: &TagStruct<'_>) -> f32 {
    s.read_real("origin fade range")
        .or_else(|| s.read_real("origin fade distance"))
        .unwrap_or(0.0)
}

fn read_property(parent: &TagStruct<'_>, name: &str) -> EditableProperty {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .map(|inner| EditableProperty::from_struct(&inner))
        .unwrap_or_default()
}
