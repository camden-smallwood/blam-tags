//! `particle` (`prt3`) tag walker — per-particle definition that the
//! effect particle_systems reference. Engine `c_particle_definition`
//! (404 bytes) per Ares `source/effects/particle.h` +
//! `particle_definitions.h`.
//!
//! Layout summary (offsets are runtime engine layout, not authored):
//! - main flags + appearance flags + billboard style + sequence range
//! - center_offset, curvature, angle_fade, motion_blur scales
//! - shader (embedded `c_render_method` — handled via existing
//!   [`crate::render_method::RenderMethod`] walker)
//! - 7 property slots (aspect / color / intensity / alpha / frame_index
//!   / animation_rate / palette_animation) — each 32B
//!   `c_particle_property`. P1 captures constants + state inputs;
//!   per-frame curve evaluation lives in the protomorph particle
//!   subsystem (P3.T3+).
//! - model reference (`pmdf` for mesh particles)
//! - GPU sprite/frame UV corners (s_gpu_data)
//! - 4 attachment slots (effe/snd!/material_effect on birth/collision/death)
//!
//! Riverworld coverage: 3 prt3 tags exercised by the spine —
//! `rolling_mist`, `mist`, `water_spray` (all under
//! `levels/multi/riverworld/fx/waterfall/particles/`). Each uses the
//! `shaders\particle` render-method template family with different
//! option tuples (`_1_3_0_0_1_1_1_0_0` / `_1_3_0_0_1_1_1_0_1` /
//! `_3_3_0_0_1_0_1_0_0`).

use std::sync::Arc;

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealPoint2d, RealVector3d};
use crate::render_method::{RenderMethod, RenderMethodError};

pub const PARTICLE_GROUP: [u8; 4] = *b"prt3";

#[derive(Debug)]
pub enum ParticleError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
    RenderMethod(RenderMethodError),
}

impl std::fmt::Display for ParticleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "expected group '{}', got '{}'",
                std::str::from_utf8(expected).unwrap_or("?"),
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
            Self::RenderMethod(e) => write!(f, "shader: {e}"),
        }
    }
}

impl std::error::Error for ParticleError {}

impl From<RenderMethodError> for ParticleError {
    fn from(e: RenderMethodError) -> Self {
        Self::RenderMethod(e)
    }
}

// ---------------------------------------------------------------------------
// `particle_main_flags` (long_flags) — per `particle_main_flags` enum
// in particle.json. P1 captures the bits we know cases for; engine
// adds more at higher bits.
// ---------------------------------------------------------------------------

pub const PARTICLE_MAIN_FLAG_DIES_AT_REST: u32 = 1 << 0;
pub const PARTICLE_MAIN_FLAG_DIES_ON_STRUCTURE_COLLISION: u32 = 1 << 1;
pub const PARTICLE_MAIN_FLAG_DIES_IN_MEDIA: u32 = 1 << 2;
pub const PARTICLE_MAIN_FLAG_DIES_IN_AIR: u32 = 1 << 3;
pub const PARTICLE_MAIN_FLAG_HAS_SWEETENER: u32 = 1 << 4;

// ---------------------------------------------------------------------------
// `particle_appearance_flags` (long_flags) — visual control bits per
// `particle_appearance_flags` enum.
// ---------------------------------------------------------------------------

pub const PARTICLE_APPEARANCE_RANDOM_U_MIRROR: u32 = 1 << 0;
pub const PARTICLE_APPEARANCE_RANDOM_V_MIRROR: u32 = 1 << 1;
pub const PARTICLE_APPEARANCE_RANDOM_ROTATION: u32 = 1 << 2;
pub const PARTICLE_APPEARANCE_TINT_FROM_LIGHTMAP: u32 = 1 << 3;
pub const PARTICLE_APPEARANCE_TINT_FROM_DIFFUSE: u32 = 1 << 4;
pub const PARTICLE_APPEARANCE_MOTION_BLUR: u32 = 1 << 5;
pub const PARTICLE_APPEARANCE_DOUBLE_SIDED: u32 = 1 << 6;
pub const PARTICLE_APPEARANCE_EDGE_FADE: u32 = 1 << 7;

// ---------------------------------------------------------------------------
// `particle_animation_flags`.
// ---------------------------------------------------------------------------

pub const PARTICLE_ANIM_FRAME_ANIMATION_ONE_SHOT: u32 = 1 << 0;
pub const PARTICLE_ANIM_CAN_ANIMATE_BACKWARDS: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// `particle_billboard_type_enum` — controls how the VS expands a
// particle into a quad. Per particle.json:
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i16)]
pub enum ParticleBillboardStyle {
    #[default]
    ScreenFacing = 0,
    CameraFacing = 1,
    ParallelToDirection = 2,
    Perpendicular = 3,
    Vertical = 4,
    Horizontal = 5,
    LocalVertical = 6,
    LocalHorizontal = 7,
    WorldModel = 8,
    VelocityHorizontal = 9,
}

impl ParticleBillboardStyle {
    pub fn from_int(v: i64) -> Self {
        match v {
            1 => Self::CameraFacing,
            2 => Self::ParallelToDirection,
            3 => Self::Perpendicular,
            4 => Self::Vertical,
            5 => Self::Horizontal,
            6 => Self::LocalVertical,
            7 => Self::LocalHorizontal,
            8 => Self::WorldModel,
            9 => Self::VelocityHorizontal,
            _ => Self::ScreenFacing,
        }
    }
}

// ---------------------------------------------------------------------------
// `attachment_type_enum` — when the attachment fires.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i8)]
pub enum ParticleAttachmentTrigger {
    #[default]
    Birth = 0,
    Collision = 1,
    Death = 2,
}

impl ParticleAttachmentTrigger {
    pub fn from_int(v: i64) -> Self {
        match v {
            1 => Self::Collision,
            2 => Self::Death,
            _ => Self::Birth,
        }
    }
}

// ---------------------------------------------------------------------------
// Output modifier — c_particle_property.m_output_modifier.
// Per `output_mod_enum` in particle.json.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(i8)]
pub enum ParticlePropertyOutputModifier {
    #[default]
    None = 0,
    Plus = 1,
    Times = 2,
}

impl ParticlePropertyOutputModifier {
    pub fn from_int(v: i64) -> Self {
        match v {
            1 => Self::Plus,
            2 => Self::Times,
            _ => Self::None,
        }
    }
}

// ---------------------------------------------------------------------------
// `s_particle_attachment` (20B, max 4 per particle).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ParticleAttachment {
    /// Tag path of the attachment target (effe / snd! / foot etc.).
    pub type_ref: String,
    /// Group fourcc of the target tag (effe / snd! / foot / etc.).
    pub type_group: [u8; 4],
    pub trigger: ParticleAttachmentTrigger,
    pub flags: u8,
    /// Indexes into `game_state_type_enum` — drives scale at attachment
    /// fire time. P1 stores the raw byte; the particle subsystem
    /// evaluates state→scale at fire time.
    pub primary_scale: i8,
    pub secondary_scale: i8,
}

impl ParticleAttachment {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let (type_group_u32, type_ref) =
            s.read_tag_ref_with_group("type").unwrap_or((0, String::new()));
        Self {
            type_ref,
            type_group: type_group_u32.to_be_bytes(),
            trigger: ParticleAttachmentTrigger::from_int(
                s.read_int_any("trigger").unwrap_or(0) as i64,
            ),
            flags: s.read_int_any("flags").unwrap_or(0) as u8,
            primary_scale: s.read_int_any("primary scale").unwrap_or(0) as i8,
            secondary_scale: s.read_int_any("secondary scale").unwrap_or(0) as i8,
        }
    }
}

// ---------------------------------------------------------------------------
// `c_particle_property` (32B) — scalar variant.
//
// P1 scope: capture state inputs + output modifier + constant value +
// runtime flags. The `mapping_function` curve walk lives in the
// protomorph particle property evaluator (P3.T3+) where it actually
// runs against game_state_type_enum values per frame.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ParticlePropertyScalar {
    /// `game_state_type_enum` index — what feeds the function input.
    pub input_variable: i8,
    /// Second `game_state_type_enum` index — feeds the range axis for
    /// ranged interpolation curves.
    pub range_variable: i8,
    pub output_modifier: ParticlePropertyOutputModifier,
    /// `game_state_type_enum` index — feeds the modifier's input.
    pub output_modifier_input: i8,
    /// Fallback constant when the curve is the identity / not authored.
    /// Engine reads this at evaluate time when `m_flags & is_constant`.
    pub constant_value: f32,
    /// Runtime flag byte. Bits aren't fully decoded by P1; the
    /// particle subsystem owns the bit interpretation.
    pub runtime_flags: u8,
}

impl ParticlePropertyScalar {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            input_variable: s.read_int_any("Input Variable").unwrap_or(0) as i8,
            range_variable: s.read_int_any("Range Variable").unwrap_or(0) as i8,
            output_modifier: ParticlePropertyOutputModifier::from_int(
                s.read_int_any("Output Modifier").unwrap_or(0) as i64,
            ),
            output_modifier_input: s
                .read_int_any("Output Modifier Input")
                .unwrap_or(0) as i8,
            constant_value: s.read_real("runtime m_constant_value").unwrap_or(0.0),
            runtime_flags: s.read_int_any("runtime m_flags").unwrap_or(0) as u8,
        }
    }
}

/// `c_particle_property` (32B) — color variant. Same layout as scalar
/// at the engine level (the underlying struct IS the same) but the
/// constant value spans 3 floats (RGB) in the function blob. P1 keeps
/// it conservative: same field set as scalar, leave the RGB constant
/// for the particle subsystem to extract from the curve blob.
pub type ParticlePropertyColor = ParticlePropertyScalar;

// ---------------------------------------------------------------------------
// GPU sprite + frame UV corners — runtime engine bakes these per-tag.
// ---------------------------------------------------------------------------

/// One sprite — a `real_vector4d` UV rect (x, y, width, height OR
/// min/max corners — engine picks per particle's billboard style).
#[derive(Debug, Clone, Copy, Default)]
pub struct ParticleGpuSprite {
    pub corner: [f32; 4],
}

/// Up to 16 frame UVs (for sprite-sheet animation). Engine slot 15 is
/// padding (`m_frames[15]` per Ares).
#[derive(Debug, Clone, Default)]
pub struct ParticleGpuFrames {
    /// Authored frame count (engine stores as float in slot 0).
    pub count: f32,
    /// Per-frame UV corner (up to 15 valid + 1 pad).
    pub frames: Vec<[f32; 4]>,
}

/// GPU sprite/frames metadata. Runtime-baked at tag postprocess time.
#[derive(Debug, Clone, Default)]
pub struct ParticleGpuData {
    pub sprite: Option<ParticleGpuSprite>,
    pub frames: Option<ParticleGpuFrames>,
}

impl ParticleGpuData {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let sprite = s
            .field("runtime m_sprite")
            .and_then(|f| f.as_block())
            .and_then(|b| b.element(0))
            .map(|sprite_block| {
                // sprite_block contains a `gpu_single_constant_register_array`
                // which is an inline array of 4 reals — the corner vec4.
                let mut corner = [0.0f32; 4];
                if let Some(arr_struct) = sprite_block
                    .fields_all()
                    .find_map(|f| f.as_struct())
                {
                    // Each array element is a single real. Walk in order.
                    for (i, field) in arr_struct.fields_all().enumerate().take(4) {
                        if let Some(crate::fields::TagFieldData::Real(v)) =
                            field.value()
                        {
                            corner[i] = v;
                        }
                    }
                }
                ParticleGpuSprite { corner }
            });

        let frames = s
            .field("runtime m_frames")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = ParticleGpuFrames::default();
                if let Some(head) = b.element(0)
                    && let Some(count_field) = head.field("runtime gpu_variants_count")
                    && let Some(crate::fields::TagFieldData::Real(c)) =
                        count_field.value()
                {
                    out.count = c;
                }
                for i in 0..b.len() {
                    let Some(elem) = b.element(i) else { continue };
                    // Each element holds an array of 4 reals (one frame's UV).
                    let mut corner = [0.0f32; 4];
                    if let Some(arr_struct) = elem
                        .fields_all()
                        .find_map(|f| f.as_struct())
                    {
                        for (j, field) in arr_struct.fields_all().enumerate().take(4) {
                            if let Some(crate::fields::TagFieldData::Real(v)) =
                                field.value()
                            {
                                corner[j] = v;
                            }
                        }
                    }
                    out.frames.push(corner);
                }
                out
            });

        Self { sprite, frames }
    }
}

// ---------------------------------------------------------------------------
// `c_particle_definition` (404B root).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ParticleDefinition {
    pub main_flags: u32,
    pub attachments: Vec<ParticleAttachment>,
    pub appearance_flags: u32,
    pub billboard_style: ParticleBillboardStyle,
    pub first_sequence_index: i16,
    pub sequence_count: i16,
    pub center_offset: RealPoint2d,
    pub curvature: f32,
    pub angle_fade_range_degrees: f32,
    pub angle_fade_cutoff_degrees: f32,
    pub motion_blur_translation_scale: f32,
    pub motion_blur_rotation_scale: f32,
    pub motion_blur_aspect_scale: f32,
    /// Render method (shader_particle_struct_definition). Carries the
    /// `rmdf` reference, options, parameters, postprocess. Reuses the
    /// existing [`RenderMethod`] walker — protomorph already knows how
    /// to bind these at draw time.
    pub shader: Option<Arc<RenderMethod>>,

    // Properties — each 32B `c_particle_property`.
    pub aspect_ratio: ParticlePropertyScalar,
    pub color: ParticlePropertyColor,
    pub intensity: ParticlePropertyScalar,
    pub alpha: ParticlePropertyScalar,

    pub animation_flags: u32,
    pub frame_index: ParticlePropertyScalar,
    pub animation_rate: ParticlePropertyScalar,
    pub palette_animation: ParticlePropertyScalar,

    /// pmdf model reference for mesh-based particles. Empty for the
    /// common case (billboards).
    pub model: String,

    /// Runtime bitmask: which `game_state_type_enum` inputs any of
    /// the property curves reference.
    pub runtime_used_particle_states: u32,
    pub runtime_constant_per_particle_properties: u32,
    pub runtime_constant_over_time_properties: u32,

    pub gpu_data: ParticleGpuData,

    /// `_arbitrary_vector3d` of the particle's sample axis (used by
    /// VS for parallel/perpendicular billboard styles). Not authored
    /// at the prt3 level — derived per-emitter at spawn time. Kept
    /// here for diagnostic purposes; default = +X.
    pub diagnostic_axis: RealVector3d,
}

impl ParticleDefinition {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ParticleError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != PARTICLE_GROUP {
            return Err(ParticleError::WrongGroup {
                expected: PARTICLE_GROUP,
                actual,
            });
        }
        Self::from_struct(&tag.root())
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Result<Self, ParticleError> {
        let main_flags = s.read_int_any("main flags").unwrap_or(0) as u32;
        let attachments = s
            .field("attachments")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(entry) = b.element(i) {
                        out.push(ParticleAttachment::from_struct(&entry));
                    }
                }
                out
            })
            .unwrap_or_default();
        let appearance_flags =
            s.read_int_any("appearance flags").unwrap_or(0) as u32;
        let billboard_style = ParticleBillboardStyle::from_int(
            s.read_int_any("particle billboard style").unwrap_or(0) as i64,
        );
        let first_sequence_index =
            s.read_int_any("first sequence index").unwrap_or(0) as i16;
        let sequence_count =
            s.read_int_any("sequence count").unwrap_or(0) as i16;
        let center_offset = s.read_point2d("center offset");
        let curvature = s.read_real("curvature").unwrap_or(0.0);
        let angle_fade_range_degrees = s.read_real("angle fade range").unwrap_or(0.0);
        let angle_fade_cutoff_degrees =
            s.read_real("angle fade cutoff").unwrap_or(0.0);
        let motion_blur_translation_scale =
            s.read_real("motion blur translation scale").unwrap_or(0.0);
        let motion_blur_rotation_scale =
            s.read_real("motion blur rotation scale").unwrap_or(0.0);
        let motion_blur_aspect_scale =
            s.read_real("motion blur aspect scale").unwrap_or(0.0);

        // Shader is a struct field — descend + walk via existing
        // RenderMethod::from_struct. Schema field name is "actual shader?"
        // (yes, with the `?`); the embedded layout name might differ —
        // we walk via the struct field iterator to find the first
        // child struct that's the render_method (typical position).
        let shader = s
            .field("actual shader?")
            .and_then(|f| f.as_struct())
            .and_then(|sub| RenderMethod::from_struct(&sub).ok())
            .map(Arc::new);

        let aspect_ratio = read_property_scalar(s, "aspect ratio");
        let color = read_property_scalar(s, "color");
        let intensity = read_property_scalar(s, "intensity");
        let alpha = read_property_scalar(s, "alpha");

        let animation_flags = s.read_int_any("animation flags").unwrap_or(0) as u32;
        let frame_index = read_property_scalar(s, "frame index");
        let animation_rate = read_property_scalar(s, "animation rate");
        let palette_animation = read_property_scalar(s, "palette animation");

        let model = s.read_tag_ref_path("Model").unwrap_or_default();

        let runtime_used_particle_states =
            s.read_int_any("runtime m_used_particle_states").unwrap_or(0) as u32;
        let runtime_constant_per_particle_properties = s
            .read_int_any("runtime m_constant_per_particle_properties")
            .unwrap_or(0) as u32;
        let runtime_constant_over_time_properties = s
            .read_int_any("runtime m_constant_over_time_properties")
            .unwrap_or(0) as u32;

        let gpu_data = s
            .field("runtime m_gpu_data")
            .and_then(|f| f.as_struct())
            .map(|sub| ParticleGpuData::from_struct(&sub))
            .unwrap_or_default();

        Ok(Self {
            main_flags,
            attachments,
            appearance_flags,
            billboard_style,
            first_sequence_index,
            sequence_count,
            center_offset,
            curvature,
            angle_fade_range_degrees,
            angle_fade_cutoff_degrees,
            motion_blur_translation_scale,
            motion_blur_rotation_scale,
            motion_blur_aspect_scale,
            shader,
            aspect_ratio,
            color,
            intensity,
            alpha,
            animation_flags,
            frame_index,
            animation_rate,
            palette_animation,
            model,
            runtime_used_particle_states,
            runtime_constant_per_particle_properties,
            runtime_constant_over_time_properties,
            gpu_data,
            diagnostic_axis: RealVector3d { i: 1.0, j: 0.0, k: 0.0 },
        })
    }
}

/// Walk a 32B `c_particle_property` substruct by field name. Returns
/// [`ParticlePropertyScalar::default()`] if the field is absent.
fn read_property_scalar(parent: &TagStruct<'_>, name: &str) -> ParticlePropertyScalar {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .map(|sub| ParticlePropertyScalar::from_struct(&sub))
        .unwrap_or_default()
}
