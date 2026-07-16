//! `light` (`ligh`) tag walker — the runtime light definition referenced
//! by scenario `lights[]` placements and by `light_new_attached` /
//! `light_new_unattached` (effect-spawned dynamic lights).
//!
//! Faithful 1:1 walk of the H3 `light_struct_definition` (148 bytes,
//! guid `f2b91e672d48afb6250f2d90a165b6ed`). Every authored field is
//! surfaced, in schema order. The engine struct is
//! `light_definition` (`objects/light_definitions.h`):
//!
//! ```text
//!   flags            @0x00  light_definition_flags        (long_flags)
//!   geometry         @0x04  light_geometry_parameters
//!     type           @0x04  light_type_enum_definition    (short_enum)
//!     maximum_distance@0x08 real
//!     frustum        @0x0C  { near_width, height_scale, field_of_view }
//!   color            @0x18  light_color_function_struct   (36 bytes)
//!   intensity        @0x3C  light_scalar_function_struct  (36 bytes)
//!   gel_bitmap       @0x60  tag_reference (bitm)
//!   falloff          @0x70  { distance_diffusion, angular_smoothness, percent_spherical }
//!   lifetime         @0x7C  { destroy_after }             (real, seconds)
//!   priority         @0x80  { near, far, transition_bias } (3× char_enum)
//!   lens_flare       @0x84  tag_reference (lens)
//! ```
//!
//! Engine consumers this walker unblocks:
//!
//! 1. **`c_lights_view::submit_visibility_and_render @ 0x1806C6930`** —
//!    per-light shadow scheduler. Gates on `flags & shadow_casting`
//!    AND `type == frustum` AND `frustum_field_of_view < π`.
//! 2. **`light_submit_lens_flares @ 0x18086A850`** — submits a lens
//!    flare for each light with a non-empty `Lens Flare` reference.
//! 3. **`light_new_unattached @ 0x1808698E0`** — sets the light datum's
//!    `_light_has_duration_bit` from `lifetime.destroy_after > 0`.
//!
//! `color` and `intensity` are authored as function curves
//! (`light_color_function_struct` / `light_scalar_function_struct`); the
//! engine evaluates them per-frame against light age. The vast majority
//! of light tags author Constant functions, so this walker reduces each
//! to its authored constant value (a range-mapped curve falls back to
//! the clamp midpoint). Revisit if an animated light needs the live
//! curve.
//!
//! Schema reference: `definitions/halo3_mcc/light.json`.

use crate::api::TagStruct;
use crate::fields::TagFieldType;
use crate::file::TagFile;
use crate::math::RealRgbColor;
use crate::tag_function::TagFunction;
use crate::typed_enums::{Enum, Flags};

/// Errors from `light` tag walking.
#[derive(Debug)]
pub enum LightError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for LightError {
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

impl std::error::Error for LightError {}

const LIGHT_GROUP: [u8; 4] = *b"ligh";

/// `light_definition_flags` (`long_flags`) — one variant per bit.
/// Discriminants are the canonical bit indices from
/// `definitions/halo3_mcc/light.json`. The per-light shadow gate keys
/// off [`Self::ShadowCasting`] (the TAG bit; per-instance attenuation
/// flags live on `generic_light_instances`).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum LightDefinitionFlags {
    #[strum(serialize = "allow shadows and gels")] AllowShadowsAndGels = 0,
    #[strum(serialize = "shadow casting")] ShadowCasting = 1,
    #[strum(serialize = "render first person only")] RenderFirstPersonOnly = 2,
    #[strum(serialize = "render third person only")] RenderThirdPersonOnly = 3,
    #[strum(serialize = "dont render splitscreen")] DontRenderSplitscreen = 4,
    #[strum(serialize = "render while active camo")] RenderWhileActiveCamo = 5,
    #[strum(serialize = "render in multiplayer override")] RenderInMultiplayerOverride = 6,
    #[strum(serialize = "move to camera in first person")] MoveToCameraInFirstPerson = 7,
    #[strum(serialize = "never priority cull")] NeverPriorityCull = 8,
    #[strum(serialize = "affected by game_can_use_flashlights")] AffectedByGameCanUseFlashlights = 9,
}

/// `type` (`short_enum`, `light_type_enum_definition`) — engine
/// `light_geometry_parameters::geometry_type`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum LightType {
    /// Point/spherical light (radial falloff).
    #[default]
    #[strum(serialize = "sphere")] Sphere = 0,
    /// Spotlight cone (`frustum_field_of_view` defines the cone angle).
    #[strum(serialize = "frustum")] Frustum = 1,
}

/// `light_priority_enumeration` (`char_enum`) — engine `e_light_priority`
/// (`objects/light_definitions.h`). Selects render priority so the
/// engine can pick the best lights when the active-light budget is hit;
/// `near priority` / `far priority` blend across `transition distance`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum LightPriority {
    /// `_light_priority_default`.
    #[default]
    #[strum(serialize = "default")] Default = 0,
    /// `_light_priority_absolutely_required` — "insanely high".
    #[strum(serialize = "insanely high")] AbsolutelyRequired = 1,
    /// `_light_priority_1` — "1 --- very high".
    #[strum(serialize = "1 --- very high")] Priority1 = 2,
    #[strum(serialize = "2")] Priority2 = 3,
    /// "3 --- high".
    #[strum(serialize = "3 --- high")] Priority3 = 4,
    #[strum(serialize = "4")] Priority4 = 5,
    /// "5 --- default".
    #[strum(serialize = "5 --- default")] Priority5 = 6,
    #[strum(serialize = "6")] Priority6 = 7,
    /// "7 --- low".
    #[strum(serialize = "7 --- low")] Priority7 = 8,
    #[strum(serialize = "8")] Priority8 = 9,
    /// "9 --- very low".
    #[strum(serialize = "9 --- very low")] Priority9 = 10,
    /// `_light_priority_next_to_nothing`.
    #[strum(serialize = "next to nothing")] NextToNothing = 11,
}

/// `light_priority_bias_enumeration` (`char_enum`) — engine
/// `e_light_priority_bias`. Sets the distance at which a light
/// transitions between its near and far priority.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum LightPriorityBias {
    /// `_light_priority_bias_default`.
    #[default]
    #[strum(serialize = "default")] Default = 0,
    /// `_light_priority_bias_very_close`.
    #[strum(serialize = "very close")] VeryClose = 1,
    /// `_light_priority_bias_close`.
    #[strum(serialize = "close")] Close = 2,
    /// `_light_priority_bias_middle`.
    #[strum(serialize = "middle")] Middle = 3,
    /// `_light_priority_bias_far`.
    #[strum(serialize = "far")] Far = 4,
    /// `_light_priority_bias_very_far`.
    #[strum(serialize = "very far")] VeryFar = 5,
}

/// The full walked `light_struct_definition`. Field order + names mirror
/// the H3 schema (and the engine `light_definition` struct) verbatim.
#[derive(Debug, Clone, Default)]
pub struct LightDefinition {
    /// `flags` @0x00 — `light_definition_flags`. Test with
    /// `.contains(LightDefinitionFlags::*)`.
    pub flags: Flags<LightDefinitionFlags, u32>,

    // ---- geometry (`light_geometry_parameters` @0x04) ----
    /// `type` @0x04 — sphere vs frustum.
    pub light_type: Enum<LightType, i16>,
    /// `maximum distance` @0x08 (world units) — distance at which the
    /// light is fully attenuated.
    pub maximum_distance: f32,
    /// `frustum near width` @0x0C (world units) — frustum lights only.
    pub frustum_near_width: f32,
    /// `frustum height scale` @0x10 — vertical gel stretch (0.0 or 1.0
    /// = aspect ratio matches the gel).
    pub frustum_height_scale: f32,
    /// `frustum field of view` @0x14 (**degrees** as authored) —
    /// horizontal cone angle. Convert to radians before projection math
    /// (engine shadow gate: `< π` after deg→rad). 0.0 = straight beam.
    pub frustum_field_of_view: f32,

    // ---- color (`light_color_function_struct` @0x18) ----
    /// `color` @0x18 — authored RGB tint (function reduced to its
    /// constant value). Linear; the engine gamma-corrects on submit.
    pub color: RealRgbColor,
    /// `intensity` @0x3C — authored intensity scalar (function reduced
    /// to its constant value).
    pub intensity: f32,
    /// `color` @0x18 — the FULL authored color function
    /// (`light_color_function_struct`). The engine evaluates this per-frame
    /// against the light's normalized age (`c_function_definition::
    /// evaluate_color`); [`Self::color`] is just its reduced constant.
    /// `None` when the mapping blob is absent.
    pub color_function: Option<TagFunction>,
    /// `intensity` @0x3C — the FULL authored intensity function
    /// (`light_scalar_function_struct`). Evaluated per-frame against
    /// normalized age (`c_function_definition::evaluate_scalar`) — this is
    /// the spark-flash / pulse envelope. [`Self::intensity`] is its reduced
    /// constant; `None` when the mapping blob is absent.
    pub intensity_function: Option<TagFunction>,
    /// `color` function `Input Variable` (string_id) — for an OBJECT-attached
    /// light, overrides the color function's input via
    /// `object_get_function_value_simple`. Empty ⇒ use normalized light age.
    pub color_input_variable: String,
    /// `color` function `Range Variable` — overrides the color range input
    /// (default 0.5). Empty ⇒ 0.5.
    pub color_range_variable: String,
    /// `intensity` function `Input Variable`.
    pub intensity_input_variable: String,
    /// `intensity` function `Range Variable`.
    pub intensity_range_variable: String,
    /// `gel bitmap` @0x60 — projected texture (spotlights / animated
    /// projectors). Tag-ref path; empty when unauthored.
    pub gel_bitmap: String,

    // ---- falloff (`light_falloff_parameters` @0x70) ----
    /// `distance diffusion` @0x70 — effective light-source size in world
    /// units. Small values give a hot near-field with rapid falloff.
    pub distance_diffusion: f32,
    /// `angular smoothness` @0x74 — `< 1.0` sharp gel/cone edges,
    /// `> 1.0` soft edges.
    pub angular_smoothness: f32,
    /// `percent spherical` @0x78 — fraction `[0, 1]` of energy emitted
    /// as spherical ambient vs directional (engine
    /// `light_falloff_parameters::light_angular_ambient`).
    pub percent_spherical: f32,

    // ---- lifetime (`light_lifetime_parameters` @0x7C) ----
    /// `destroy light after` @0x7C (seconds) — an unattached light
    /// auto-destroys after existing this long; `0` disables. Drives the
    /// `_light_has_duration_bit` in `light_new_unattached`.
    pub destroy_after: f32,

    // ---- priority (`light_priority_parameters` @0x80) ----
    /// `near priority` @0x80 — priority when the light is fullscreen.
    pub near_priority: Enum<LightPriority, i8>,
    /// `far priority` @0x81 — priority when the light is far away.
    pub far_priority: Enum<LightPriority, i8>,
    /// `transition distance` @0x82 — where the near→far priority
    /// transition occurs.
    pub transition_distance: Enum<LightPriorityBias, i8>,

    // ---- attachments (@0x84) ----
    /// `Lens Flare` @0x84 — tag-ref path to a `.lens_flare` (`lens`).
    /// Empty when unauthored; `light_submit_lens_flares` walks every
    /// active light whose value here is non-empty.
    pub lens_flare: String,
}

impl LightDefinition {
    pub fn from_tag(tag: &TagFile) -> Result<Self, LightError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != LIGHT_GROUP {
            return Err(LightError::WrongGroup { expected: LIGHT_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let flags: Flags<LightDefinitionFlags, u32> =
            s.try_read_flags("flags").unwrap_or_default();

        let light_type: Enum<LightType, i16> = s.try_read_enum("type").unwrap_or_default();
        let maximum_distance = s.read_real("maximum distance").unwrap_or(0.0);
        let frustum_near_width = s.read_real("frustum near width").unwrap_or(0.0);
        let frustum_height_scale = s.read_real("frustum height scale").unwrap_or(1.0);
        let frustum_field_of_view = s.read_real("frustum field of view").unwrap_or(0.0);

        let color = read_light_color(s, "color");
        let intensity = read_light_scalar(s, "intensity");
        let color_function = read_light_function(s, "color");
        let intensity_function = read_light_function(s, "intensity");
        let color_input_variable = read_light_function_var(s, "color", "Input Variable");
        let color_range_variable = read_light_function_var(s, "color", "Range Variable");
        let intensity_input_variable = read_light_function_var(s, "intensity", "Input Variable");
        let intensity_range_variable = read_light_function_var(s, "intensity", "Range Variable");
        let gel_bitmap = s.read_tag_ref_path("gel bitmap").unwrap_or_default();

        let distance_diffusion = s.read_real("distance diffusion").unwrap_or(1.0);
        let angular_smoothness = s.read_real("angular smoothness").unwrap_or(1.0);
        let percent_spherical = s.read_real("percent spherical").unwrap_or(0.0);

        let destroy_after = s.read_real("destroy light after").unwrap_or(0.0);

        let near_priority: Enum<LightPriority, i8> =
            s.try_read_enum("near priority").unwrap_or_default();
        let far_priority: Enum<LightPriority, i8> =
            s.try_read_enum("far priority").unwrap_or_default();
        let transition_distance: Enum<LightPriorityBias, i8> =
            s.try_read_enum("transition distance").unwrap_or_default();

        let lens_flare = s.read_tag_ref_path("Lens Flare").unwrap_or_default();

        Self {
            flags,
            light_type,
            maximum_distance,
            frustum_near_width,
            frustum_height_scale,
            frustum_field_of_view,
            color,
            intensity,
            color_function,
            intensity_function,
            color_input_variable,
            color_range_variable,
            intensity_input_variable,
            intensity_range_variable,
            gel_bitmap,
            distance_diffusion,
            angular_smoothness,
            percent_spherical,
            destroy_after,
            near_priority,
            far_priority,
            transition_distance,
            lens_flare,
        }
    }

    /// True if `flags & shadow_casting`. The engine's per-light shadow
    /// gate at `c_lights_view::submit_visibility_and_render` predicates
    /// off this bit (NOT the per-instance flags on
    /// `generic_light_instances`).
    pub fn casts_shadows(&self) -> bool {
        self.flags.contains(LightDefinitionFlags::ShadowCasting)
    }

    /// True if the light has a non-empty lens flare attachment.
    /// `light_submit_lens_flares` skips lights with empty references.
    pub fn has_lens_flare(&self) -> bool {
        !self.lens_flare.is_empty()
    }

    /// True if this is a frustum-shaped light (cone). The engine's
    /// per-light shadow path requires frustum AND `fov < π`.
    pub fn is_frustum(&self) -> bool {
        self.light_type == LightType::Frustum
    }

    /// True if the light auto-destroys after a finite lifetime
    /// (`destroy_after > 0`). Mirrors the `_light_has_duration_bit` gate
    /// in `light_new_unattached`.
    pub fn has_duration(&self) -> bool {
        self.destroy_after > 0.0
    }
}

/// Walk a `light_color_function_struct` field and return the authored
/// constant RGB. Non-constant functions return the gradient's first
/// stop (`m_colors[0]`) as a reasonable default.
fn read_light_color(parent: &TagStruct<'_>, name: &str) -> RealRgbColor {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .and_then(|color_struct| inner_mapping_function(&color_struct))
        .map(|func| color_from_function(&func))
        .unwrap_or(RealRgbColor { red: 1.0, green: 1.0, blue: 1.0 })
}

/// Walk a `light_scalar_function_struct` field and return the authored
/// constant scalar. Falls back to 1.0 if the function blob is missing.
fn read_light_scalar(parent: &TagStruct<'_>, name: &str) -> f32 {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .and_then(|scalar_struct| inner_mapping_function(&scalar_struct))
        .map(|func| func.as_constant().unwrap_or_else(|| {
            // Range-mapped curve — return clamp midpoint. Engine
            // evaluates against runtime time, but most light tags are
            // constant; this only fires for animated lights.
            let h = func.header();
            0.5 * (h.clamp_range_min + h.clamp_range_max)
        }))
        .unwrap_or(1.0)
}

/// Read a `light_*_function_struct`'s `Input Variable` / `Range Variable`
/// string_id (the object-function name overriding the curve input for
/// object-attached lights). Empty when unauthored.
fn read_light_function_var(parent: &TagStruct<'_>, func_name: &str, var_name: &str) -> String {
    parent
        .field(func_name)
        .and_then(|f| f.as_struct())
        .and_then(|func_struct| func_struct.read_string_id(var_name))
        .unwrap_or_default()
}

/// The FULL [`TagFunction`] for a `light_*_function_struct` field — the
/// engine `c_function_definition` the runtime evaluates per-frame against the
/// light's normalized age. `None` when the field or its mapping blob is absent.
fn read_light_function(parent: &TagStruct<'_>, name: &str) -> Option<TagFunction> {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .and_then(|func_struct| inner_mapping_function(&func_struct))
}

/// Reach into a `light_*_function_struct` and pull the
/// `mapping_function::data` blob as a parsed [`TagFunction`].
///
/// The schema declares TWO same-named "Mapping" fields inside the
/// outer function struct — a `custom` marker (group_tag `fned`) and
/// the real `mapping_function` struct that follows it. `field("Mapping")`
/// returns the marker first, so we walk by type instead.
fn inner_mapping_function(outer: &TagStruct<'_>) -> Option<TagFunction> {
    let mapping = outer
        .fields()
        .find(|f| f.field_type() == TagFieldType::Struct)?
        .as_struct()?;
    mapping.field("data").and_then(|f| f.as_function())
}

/// Decode a [`TagFunction`]'s `colors[0]` slot as ARGB-packed RGB.
/// `m_colors[0]` carries the first authored gradient stop; for
/// constant-color lights this is the single authored value. Engine
/// pixel32 layout is `0xAARRGGBB`.
fn color_from_function(func: &TagFunction) -> RealRgbColor {
    let packed = func.header().colors[0];
    if packed == 0 {
        return RealRgbColor { red: 1.0, green: 1.0, blue: 1.0 };
    }
    let r = ((packed >> 16) & 0xff) as f32 / 255.0;
    let g = ((packed >> 8) & 0xff) as f32 / 255.0;
    let b = (packed & 0xff) as f32 / 255.0;
    RealRgbColor { red: r, green: g, blue: b }
}
