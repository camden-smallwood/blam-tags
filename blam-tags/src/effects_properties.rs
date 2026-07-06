//! Shared authoring shape for `c_editable_property<*>` slots — the
//! 32B-aligned struct that powers per-frame value evaluation across
//! the particle, beam, contrail, light_volume, and screen_effect
//! systems.
//!
//! The 4 state-list specializations (`c_particle_state_list`,
//! `c_beam_states`, `c_contrail_states`, `c_light_volume_states`) share
//! identical TAG layouts — only the semantic meaning of `input_index`
//! differs (each picks slots into its own state table). The tag walker
//! is layout-only; runtime consumers re-tag the indices against the
//! relevant state list.
//!
//! Schema name varies per tag:
//! - particle_physics: `particle_property_scalar_struct_new`
//! - light_volume_system: `light_volume_property_real` /
//!   `light_volume_property_real_rgb_color`
//! - beam_system: `beam_property_real` / `beam_property_real_rgb_color`
//! - contrail_system: `contrail_property_real` / `contrail_property_real_rgb_color`
//!
//! Field names are TitleCase + the runtime-resolved fields end with `!`:
//! Input Variable, Range Variable, Output Modifier, Output Modifier
//! Input, Mapping, runtime m_constant_value!, runtime m_flags!.
//!
//! Runtime mirror: `c_editable_property_base` (32B) — see
//! [reference_effect_system_dllcache_layouts_2026_05_21].

use crate::api::TagStruct;
use crate::fields::TagFieldType;
use crate::tag_function::TagFunction;

/// One `c_editable_property<*>` slot. Used by particle / beam /
/// contrail / light_volume property curves.
#[derive(Debug, Clone, Default)]
pub struct EditableProperty {
    /// `Input Variable` (char_enum) — primary state-list slot index.
    /// Semantic meaning depends on the tag's state list (e.g.
    /// 0=particle_age for particle_state_list, 0=beam_age for beam_states).
    pub input_index: u8,
    /// `Range Variable` (char_enum) — secondary state-list slot.
    pub range_input_index: u8,
    /// `Output Modifier` (char_enum: 0=none, 1=Plus, 2=Times) —
    /// composition mode for blending mapping output with the modifier
    /// input.
    pub output_modifier_type: u8,
    /// `Output Modifier Input` (char_enum) — state-list slot driving
    /// the modifier.
    pub output_modifier_input_index: u8,
    /// Authored curve / function blob. `None` when the property is
    /// constant (engine reads `constant_value` instead).
    pub function: Option<TagFunction>,
    /// `runtime m_constant_value!` — tool.exe-resolved constant for
    /// constant-time properties.
    pub constant_value: f32,
    /// `runtime m_flags!` (char) — evaluation-mode shortcut bits:
    /// is_constant / is_constant_over_time / is_constant_per_instance.
    pub runtime_flags: u8,
    /// `Starting interpolant` for TYPED (point3d / vector3d / spherical)
    /// properties — `None` for scalars. The engine's
    /// `c_editable_property_type<vector>::evaluate` returns
    /// `interpolate(starting, ending, function(t))`. For spherical
    /// (relative_direction) the stored {Radius, Euler(yaw,pitch)} is
    /// converted to a vector (Radius·dir(yaw,pitch); identity = +X).
    pub starting_interpolant: Option<crate::math::RealVector3d>,
    /// `Ending interpolant` — see [`Self::starting_interpolant`].
    pub ending_interpolant: Option<crate::math::RealVector3d>,
}

impl EditableProperty {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            input_index: s.read_int_any("Input Variable").unwrap_or(0) as u8,
            range_input_index: s.read_int_any("Range Variable").unwrap_or(0) as u8,
            output_modifier_type: s.read_int_any("Output Modifier").unwrap_or(0) as u8,
            output_modifier_input_index: s.read_int_any("Output Modifier Input").unwrap_or(0) as u8,
            function: read_mapping_function(s, "Mapping"),
            constant_value: s.read_real("runtime m_constant_value").unwrap_or(0.0),
            runtime_flags: s.read_int_any("runtime m_flags").unwrap_or(0) as u8,
            starting_interpolant: read_interpolant(s, "Starting interpolant"),
            ending_interpolant: read_interpolant(s, "Ending interpolant"),
        }
    }
}

/// Read a typed property's interpolant vector. Handles these forms:
/// - `real_point_3d` / `real_vector_3d` (translational_offset,
///   self_acceleration) — read directly.
/// - `real_point_2d` / `real_vector_2d` (contrail `profile offset`) —
///   read as `{x, y}` into `{i, j, k=0}` (the shared interpolant slot is
///   a `RealVector3d`; the 2D consumer uses `.i`/`.j`).
/// - the spherical struct `{Radius, Euler angles 2d (yaw, pitch)}`
///   (relative_direction) — convert to a vector
///   `Radius · (cos pitch cos yaw, cos pitch sin yaw, sin pitch)`
///   (Radius=1, Euler=0 → +X, the engine's emitter-forward identity).
///
/// Returns `None` for scalar properties (no interpolant field).
fn read_interpolant(parent: &TagStruct<'_>, name: &str) -> Option<crate::math::RealVector3d> {
    use crate::math::RealVector3d;
    // Spherical: a struct-typed field of this name carrying Radius+Euler
    // (relative_direction). Convert {Radius, yaw, pitch} → vector.
    if let Some(inner) = parent
        .fields()
        .find(|f| f.name() == name && f.field_type() == TagFieldType::Struct)
        .and_then(|f| f.as_struct())
    {
        if let Some(radius) = inner.read_real("Radius") {
            let e = inner.read_euler2d("Euler angles");
            let (cy, sy) = (e.yaw.cos(), e.yaw.sin());
            let (cp, sp) = (e.pitch.cos(), e.pitch.sin());
            return Some(RealVector3d {
                i: radius * cp * cy,
                j: radius * cp * sy,
                k: radius * sp,
            });
        }
    }
    // Direct point3d / vector3d field (translational_offset,
    // self_acceleration). Use the reader matching the actual type — the
    // readers are type-strict (fail-loud on mismatch).
    let ft = parent.fields().find(|f| f.name() == name).map(|f| f.field_type());
    match ft {
        Some(TagFieldType::RealPoint3d) => {
            let p = parent.read_point3d(name);
            Some(RealVector3d { i: p.x, j: p.y, k: p.z })
        }
        Some(TagFieldType::RealVector3d) => Some(parent.read_vec3(name)),
        Some(TagFieldType::RealPoint2d) => {
            let p = parent.read_point2d(name);
            Some(RealVector3d { i: p.x, j: p.y, k: 0.0 })
        }
        Some(TagFieldType::RealVector2d) => {
            let v = parent.read_vec2(name);
            Some(RealVector3d { i: v.i, j: v.j, k: 0.0 })
        }
        _ => None,
    }
}

/// Walk the schema's two-stage "Mapping" wrapper to reach the curve
/// payload. The schema declares both a `custom` marker AND a `struct`
/// with the same name; we find the struct by type, then pull the
/// `data` field out of it.
pub fn read_mapping_function(parent: &TagStruct<'_>, name: &str) -> Option<TagFunction> {
    let outer = parent
        .fields()
        .find(|f| f.name() == name && f.field_type() == TagFieldType::Struct)?
        .as_struct()?;
    outer.field("data").and_then(|f| f.as_function())
}
