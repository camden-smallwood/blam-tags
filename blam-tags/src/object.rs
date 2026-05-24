//! `object_definition` common substruct — the `object` block shared by
//! all 14 object subgroups (.scenery, .crate, .weapon, .biped, .vehicle,
//! .equipment, .device_machine, .device_control, .device_terminal,
//! .projectile, .creature, .giant, .effect_scenery, .sound_scenery).
//! Each subgroup's tag file embeds this same `object_struct_definition`
//! at a path that depends on its inheritance chain:
//!   `.object` (obje, abstract base) → `` (the tag root)
//!   `.scenery / .crate / .sound_scenery / .creature / .projectile / .effect_scenery / .giant`
//!                                      → `object`
//!   `.biped / .vehicle`                → `unit/object`
//!   `.weapon`                          → `weapon/item/object`
//!   `.equipment`                       → `item/object`
//!   `.device_machine / .device_control / .device_terminal` → `device/object`
//!
//! Schema reference: `definitions/halo3_mcc/object.json`
//! → `object_struct_definition` (size 248, guid
//! `6c5aa9947a45fcf55742a488f0943380`). The field set below mirrors the
//! schema's field order with runtime-only fields
//! (`runtime object type!`, `runtime flags!*`) and not-yet-consumed
//! sub-blocks (`early mover OBB`, `ai properties`, `attachments`,
//! `widgets`, `change colors`, `multiplayer object`, `health packs`)
//! omitted — add them as consumers come online.
//!
//! Drives two runtime paths:
//!
//! 1. **`object_get_function_value @ dllcache 0x1807DBA60`** — when a
//!    render-method asks for an input by name (e.g. `bar` on
//!    `marinebeacon.scenery`) and the type-specific
//!    `<type>_compute_function_value` returns false, the engine walks
//!    `functions[]` looking for an entry whose `export_name` matches
//!    the requested name, then evaluates that entry's curve via
//!    `object_function_get_function_value @ 0x1807E85B0`.
//!
//! 2. **`object_get_bounding_sphere @ 0x1802473A0`** — reads
//!    `(bounding_offset, bounding_radius)` and transforms by the
//!    object's runtime matrix for cull/shadow/lights bookkeeping.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::math::{RealEulerAngles3d, RealPoint3d, RealRgbColor};
use crate::tag_function::TagFunction;

/// All 14 object subgroups that share the `object_struct_definition`.
/// Any tag in this set has a valid `object` substruct (under the
/// inheritance-chain prefix returned by [`OBJECT_INHERITANCE_PREFIXES`]).
pub const OBJECT_SUBGROUPS: &[[u8; 4]] = &[
    *b"obje", // object_definition (abstract base)
    *b"scen", // scenery
    *b"bipd", // biped
    *b"vehi", // vehicle
    *b"weap", // weapon
    *b"eqip", // equipment
    *b"ssce", // sound_scenery
    *b"bloc", // crate (.crate extension → `bloc` FOURCC per crate.json:3)
    *b"mach", // device_machine
    *b"ctrl", // device_control
    *b"term", // device_terminal
    *b"proj", // projectile
    *b"crea", // creature
    *b"gint", // giant
    *b"efsc", // effect_scenery
];

/// Inheritance-chain prefixes that wrap the `object_struct_definition`
/// in each subgroup's tag layout. Ordered deepest-first so the more
/// specific path wins when the same struct-name collides at multiple
/// levels (it shouldn't in practice, but cheap insurance).
const OBJECT_INHERITANCE_PREFIXES: &[&str] = &[
    "weapon/item/object", // .weapon
    "item/object",        // .equipment
    "unit/object",        // .biped / .vehicle / .giant
    "device/object",      // .device_machine / .device_control / .device_terminal
    "object",             // .scenery / .crate / .sound_scenery / .creature / .projectile / .effect_scenery
    "",                   // .object itself (abstract base)
];

/// Errors from `object_definition` tag walking.
#[derive(Debug)]
pub enum ObjectDefinitionError {
    WrongGroup { actual: [u8; 4] },
}

impl std::fmt::Display for ObjectDefinitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { actual } => write!(
                f,
                "tag group '{}' is not in OBJECT_SUBGROUPS — not an object_definition tag",
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
        }
    }
}

impl std::error::Error for ObjectDefinitionError {}

// ---------------------------------------------------------------------------
// object_definition_flags (bit names from object_definition_flags enum)
// ---------------------------------------------------------------------------

/// `flags & 0x0001` — engine `object_definition_flags::does_not_cast_shadow`.
/// Drives `render_object_has_lightmap_shadow @ 0x180696EE0`.
pub const OBJ_FLAG_DOES_NOT_CAST_SHADOW: u16 = 1 << 0;

/// `flags & 0x0002` — engine `_object_searches_lightmaps_on_failure_bit`.
/// When set, `lights_prepare_for_object_static_new @ 0x1808A2930:376`
/// or's `flags |= 1` before calling `lights_distant_lighting_at_point_new
/// @ 0x1808A3220`, which selects the 9-ray sideways direction-builder
/// branch (`lightmap_sample_raycast_sideways`). When unset, the
/// default 1-ray-with-offset branch fires instead. The flag is per-tag,
/// not per-class — IDA's mistyped `v71->class_index` decompile reads
/// the low byte of `_object_definition.flags` at +2.
pub const OBJ_FLAG_SEARCHES_LIGHTMAPS_ON_FAILURE: u16 = 1 << 1;

// ---------------------------------------------------------------------------
// object_function_flags
// ---------------------------------------------------------------------------

/// Bit 0 — invert the resolved `import_name` magnitude before evaluating
/// the curve. Engine: `*magnitude = 1.0 - *magnitude;`.
pub const FN_FLAG_INVERT: u32 = 1 << 0;

/// Bit 1 — when SET the entry evaluates ONLY when the import-name
/// resolution itself returned active. When CLEAR the entry always
/// evaluates against the magnitude even if import resolution failed
/// and reactivates when curve output > 0.
pub const FN_FLAG_ADDITIVE: u32 = 1 << 1;

/// Bit 2 — force `result = 1` regardless of curve output. Engine:
/// `if ( (function->flags & 4) != 0 ) result = 1;`.
pub const FN_FLAG_ALWAYS_ACTIVE: u32 = 1 << 2;

/// Bit 3 — periodic-time evaluation adds a per-object random offset
/// to the time input. Engine hashes `object_index` via
/// `LCG: 1664525 * object_index + 1013904223`, takes the high 16
/// bits, scales by `1/65535` and adds to game time.
pub const FN_FLAG_RANDOM_TIME_OFFSET: u32 = 1 << 3;

/// Bit 4 — emit the curve magnitude even when the entry is inactive.
/// Without this bit, inactive entries return magnitude=0.0.
pub const FN_FLAG_ALWAYS_EMIT_MAGNITUDE: u32 = 1 << 4;

/// Bit 5 — the `turn_off_with` test additionally requires the source
/// magnitude to be non-zero (not just active).
pub const FN_FLAG_TURN_OFF_REQUIRES_NONZERO: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// ObjectFunctionDefinition
// ---------------------------------------------------------------------------

/// One entry of `object_definition::functions[]`. Engine
/// `s_object_function_definition` (44 bytes). Schema:
/// `object_function_block` (`object.json:331-381`).
#[derive(Debug, Clone, Default)]
pub struct ObjectFunctionDefinition {
    /// `flags` (long_flags, `object_function_flags`) — test against `FN_FLAG_*`.
    pub flags: u32,
    /// `import name` (string_id) — the name whose magnitude this entry
    /// reads. Resolved via `object_get_function_value(object_index,
    /// import_name, …)` — may recurse into another entry in this
    /// `functions[]` block, into a `<type>_compute_function_value`
    /// case, or hit the built-in `""`/`"one"`/`"zero"` shortcuts.
    pub import_name: String,
    /// `export name` (string_id) — the name this entry defines. The
    /// engine's `object_get_function_value` walker matches against
    /// this when looking for an unresolved input.
    pub export_name: String,
    /// `turn off with` (string_id) — engine field name
    /// `turn_off_with_function_name`. If non-empty, the entry is
    /// inactive when the named function fails to resolve.
    pub turn_off_with_function_name: String,
    /// `min value` (real). Engine field name `lower_bound`. When > 0,
    /// the entry is active only when the curve output exceeds it.
    pub lower_bound: f32,
    /// `default function` (struct, `mapping_function`). Engine field
    /// name `function_value`. The curve applied to the resolved input.
    /// `None` when the authored curve was empty/unset.
    pub function_value: Option<TagFunction>,
    /// `scale by` (string_id). If non-empty, multiply the curve output
    /// by `object_get_function_value(object_index, scale_by, …)`.
    pub scale_by: String,
}

impl ObjectFunctionDefinition {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let flags = s.read_int_any("flags").unwrap_or(0) as u32;
        let import_name = s.read_string_id("import name").unwrap_or_default();
        let export_name = s.read_string_id("export name").unwrap_or_default();
        let turn_off_with_function_name =
            s.read_string_id("turn off with").unwrap_or_default();
        let lower_bound = s.read_real("min value").unwrap_or(0.0);
        let function_value = s
            .field("default function")
            .and_then(|f| f.as_struct())
            .and_then(|inner| inner.field("data"))
            .and_then(|f| f.as_function());
        let scale_by = s.read_string_id("scale by").unwrap_or_default();
        Self {
            flags,
            import_name,
            export_name,
            turn_off_with_function_name,
            lower_bound,
            function_value,
            scale_by,
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-block structs (in schema declaration order)
// ---------------------------------------------------------------------------

/// `object_early_mover_obb_block` (40 bytes). Field order matches
/// `object.json:254-298` verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectEarlyMoverObb {
    /// `node name` (string_id) — empty means object space.
    pub node_name: String,
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub z0: f32,
    pub z1: f32,
    /// `angles` (real_euler_angles_3d).
    pub angles: RealEulerAngles3d,
}

impl ObjectEarlyMoverObb {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            node_name: s.read_string_id("node name").unwrap_or_default(),
            x0: s.read_real("x0").unwrap_or(0.0),
            x1: s.read_real("x1").unwrap_or(0.0),
            y0: s.read_real("y0").unwrap_or(0.0),
            y1: s.read_real("y1").unwrap_or(0.0),
            z0: s.read_real("z0").unwrap_or(0.0),
            z1: s.read_real("z1").unwrap_or(0.0),
            angles: s.read_euler3d("angles"),
        }
    }
}

/// `object_ai_properties_block` (12 bytes). Field order matches
/// `object.json:299-330` verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectAiProperties {
    /// `ai flags` (long_flags, `ai_properties_flags`).
    pub ai_flags: u32,
    /// `ai type name` (string_id) — combat dialogue category.
    pub ai_type_name: String,
    /// `ai size` (short_enum, `ai_size_enum`).
    pub ai_size: i16,
    /// `leap jump speed` (short_enum, `global_ai_jump_height_enum`).
    pub leap_jump_speed: i16,
}

impl ObjectAiProperties {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            ai_flags: s.read_int_any("ai flags").unwrap_or(0) as u32,
            ai_type_name: s.read_string_id("ai type name").unwrap_or_default(),
            ai_size: s.read_int_any("ai size").unwrap_or(0) as i16,
            leap_jump_speed: s.read_int_any("leap jump speed").unwrap_or(0) as i16,
        }
    }
}

/// `object_attachment_block` (32 bytes). Field order matches
/// `object.json:410-461` verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectAttachment {
    /// `type^` (tag_reference — many allowed groups, mostly effe/lens/snd!/cont/lsnd).
    pub type_ref: String,
    /// 4-byte big-endian group fourcc of the [`Self::type_ref`] tag —
    /// `b"effe"` / `b"lens"` / `b"snd!"` / `b"cont"` / `b"lsnd"` /
    /// `b"ligh"` / etc. Engine `attachments_new @ 0x1807E2F60`
    /// dispatches on this exact value (effe→effect_check_object_function_determinacy,
    /// ligh→light_new_attached, lsnd→game_looping_sound_attachment_new,
    /// lens→queued per-frame). `[0; 4]` when the attachment is null.
    pub type_group: [u8; 4],
    /// `marker` (old_string_id) — the node/marker the attachment binds to.
    pub marker: String,
    /// `change color` (short_enum, `global_object_change_color_enum`).
    pub change_color: i16,
    /// `primary scale` (string_id).
    pub primary_scale: String,
    /// `secondary scale` (string_id).
    pub secondary_scale: String,
}

impl ObjectAttachment {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let (type_group_u32, type_ref) = s
            .read_tag_ref_with_group("type")
            .unwrap_or((0, String::new()));
        Self {
            type_ref,
            type_group: type_group_u32.to_be_bytes(),
            marker: s.read_string_id("marker").unwrap_or_default(),
            change_color: s.read_int_any("change color").unwrap_or(0) as i16,
            primary_scale: s.read_string_id("primary scale").unwrap_or_default(),
            secondary_scale: s.read_string_id("secondary scale").unwrap_or_default(),
        }
    }
}

/// `object_widget_block` (16 bytes). Field order matches
/// `object.json:463-?`.
#[derive(Debug, Clone, Default)]
pub struct ObjectWidget {
    /// `type` (tag_reference to a widget tag — antenna/light/glow/etc.).
    pub type_ref: String,
}

impl ObjectWidget {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            type_ref: s.read_tag_ref_path("type").unwrap_or_default(),
        }
    }
}

/// `object_change_color_initial_permutation` (32 bytes). Field order
/// matches schema verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectChangeColorInitialPermutation {
    pub weight: f32,
    pub color_lower_bound: RealRgbColor,
    pub color_upper_bound: RealRgbColor,
    /// `variant name` (string_id) — empty = any variant.
    pub variant_name: String,
}

impl ObjectChangeColorInitialPermutation {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            weight: s.read_real("weight").unwrap_or(0.0),
            color_lower_bound: s.read_rgb("color lower bound"),
            color_upper_bound: s.read_rgb("color upper bound"),
            variant_name: s.read_string_id("variant name").unwrap_or_default(),
        }
    }
}

/// `object_change_color_function` (36 bytes). Field order matches
/// schema verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectChangeColorFunction {
    /// `scale flags` (long_flags).
    pub scale_flags: u32,
    pub color_lower_bound: RealRgbColor,
    pub color_upper_bound: RealRgbColor,
    pub darken_by: String,
    pub scale_by: String,
}

impl ObjectChangeColorFunction {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            scale_flags: s.read_int_any("scale flags").unwrap_or(0) as u32,
            color_lower_bound: s.read_rgb("color lower bound"),
            color_upper_bound: s.read_rgb("color upper bound"),
            darken_by: s.read_string_id("darken by").unwrap_or_default(),
            scale_by: s.read_string_id("scale by").unwrap_or_default(),
        }
    }
}

/// `object_change_colors` (24 bytes — holds 2 sub-blocks). Field
/// order matches schema verbatim.
#[derive(Debug, Clone, Default)]
pub struct ObjectChangeColors {
    pub initial_permutations: Vec<ObjectChangeColorInitialPermutation>,
    pub functions: Vec<ObjectChangeColorFunction>,
}

impl ObjectChangeColors {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            initial_permutations: read_block_vec(
                s,
                "initial permutations",
                ObjectChangeColorInitialPermutation::from_struct,
            ),
            functions: read_block_vec(s, "functions", ObjectChangeColorFunction::from_struct),
        }
    }
}

/// `multiplayer_object_block` (196 bytes). Field order matches
/// schema verbatim.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerObject {
    /// `game engine flags` (word_flags) — which gametypes include this.
    pub game_engine_flags: u16,
    /// `type` (char_enum) — MP object type (weapon/grenade/spawn/etc.).
    pub mp_type: i8,
    /// `teleporter passability` (byte_flags) — teleporter-only.
    pub teleporter_passability: u8,
    /// `flags` (word_flags) — MP-specific.
    pub flags: u16,
    /// `boundary shape` (char_enum).
    pub boundary_shape: i8,
    /// `spawn timer type` (char_enum).
    pub spawn_timer_type: i8,
    pub default_spawn_time: i16,
    pub default_abandonment_time: i16,
    pub boundary_width_or_radius: f32,
    pub boundary_box_length: f32,
    pub boundary_positive_height: f32,
    pub boundary_negative_height: f32,
    pub normal_weight: f32,
    pub flag_away_weight: f32,
    pub flag_at_home_weight: f32,
    pub boundary_center_marker: String,
    pub spawned_object_marker_name: String,
    pub spawned_object: String,
    pub nyi_boundary_material: String,
    pub boundary_standard_shader: String,
    pub boundary_opaque_shader: String,
    pub sphere_standard_shader: String,
    pub sphere_opaque_shader: String,
    pub cylinder_standard_shader: String,
    pub cylinder_opaque_shader: String,
    pub box_standard_shader: String,
    pub box_opaque_shader: String,
}

impl MultiplayerObject {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            game_engine_flags: s.read_int_any("game engine flags").unwrap_or(0) as u16,
            mp_type: s.read_int_any("type").unwrap_or(0) as i8,
            teleporter_passability: s.read_int_any("teleporter passability").unwrap_or(0) as u8,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            boundary_shape: s.read_int_any("boundary shape").unwrap_or(0) as i8,
            spawn_timer_type: s.read_int_any("spawn timer type").unwrap_or(0) as i8,
            default_spawn_time: s.read_int_any("default spawn time").unwrap_or(0) as i16,
            default_abandonment_time: s.read_int_any("default abandonment time").unwrap_or(0) as i16,
            boundary_width_or_radius: s.read_real("boundary width/radius").unwrap_or(0.0),
            boundary_box_length: s.read_real("boundary box length").unwrap_or(0.0),
            boundary_positive_height: s.read_real("boundary +height").unwrap_or(0.0),
            boundary_negative_height: s.read_real("boundary -height").unwrap_or(0.0),
            normal_weight: s.read_real("normal weight").unwrap_or(0.0),
            flag_away_weight: s.read_real("flag away weight").unwrap_or(0.0),
            flag_at_home_weight: s.read_real("flag at home weight").unwrap_or(0.0),
            boundary_center_marker: s.read_string_id("boundary center marker").unwrap_or_default(),
            spawned_object_marker_name: s
                .read_string_id("spawned object marker name")
                .unwrap_or_default(),
            spawned_object: s.read_tag_ref_path("spawned object").unwrap_or_default(),
            nyi_boundary_material: s
                .read_string_id("NYI boundary material")
                .unwrap_or_default(),
            boundary_standard_shader: s
                .read_tag_ref_path("boundary standard shader")
                .unwrap_or_default(),
            boundary_opaque_shader: s
                .read_tag_ref_path("boundary opaque shader")
                .unwrap_or_default(),
            sphere_standard_shader: s
                .read_tag_ref_path("sphere standard shader")
                .unwrap_or_default(),
            sphere_opaque_shader: s
                .read_tag_ref_path("sphere opaque shader")
                .unwrap_or_default(),
            cylinder_standard_shader: s
                .read_tag_ref_path("cylinder standard shader")
                .unwrap_or_default(),
            cylinder_opaque_shader: s
                .read_tag_ref_path("cylinder opaque shader")
                .unwrap_or_default(),
            box_standard_shader: s
                .read_tag_ref_path("box standard shader")
                .unwrap_or_default(),
            box_opaque_shader: s
                .read_tag_ref_path("box opaque shader")
                .unwrap_or_default(),
        }
    }
}

/// `object_health_pack_block` (16 bytes). Field order matches schema.
#[derive(Debug, Clone, Default)]
pub struct ObjectHealthPack {
    pub health_pack_equipment: String,
}

impl ObjectHealthPack {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            health_pack_equipment: s
                .read_tag_ref_path("health pack equipment")
                .unwrap_or_default(),
        }
    }
}

/// Helper: walk a tag block field and collect parsed elements.
fn read_block_vec<T, F>(s: &TagStruct<'_>, name: &str, mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|f| f.as_block())
        .map(|block| block.iter().map(|e| f(&e)).collect::<Vec<_>>())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// ObjectDefinition
// ---------------------------------------------------------------------------

/// Walked subset of the engine `object_struct_definition` (size 248).
/// Field order matches the schema's `fields` array verbatim
/// (`object.json:75-251`) so a future side-by-side reads cleanly.
/// Not-yet-consumed sub-blocks (`early mover OBB`, `ai properties`,
/// `attachments`, `widgets`, `change colors`, `multiplayer object`,
/// `health packs`) are omitted — add them in schema order as
/// consumers come online.
#[derive(Debug, Clone, Default)]
pub struct ObjectDefinition {
    /// `flags` (word_flags, `object_definition_flags`). Test against
    /// `OBJ_FLAG_*` (e.g. `OBJ_FLAG_DOES_NOT_CAST_SHADOW`).
    pub flags: u16,
    /// `bounding radius:world units` (real).
    pub bounding_radius: f32,
    /// `bounding offset` (real_point_3d).
    pub bounding_offset: RealPoint3d,
    /// `acceleration scale` (real) — AI movement scale; not used by
    /// renderer but kept for completeness.
    pub acceleration_scale: f32,
    /// `lightmap shadow mode` (short_enum). 0 = `default` (engine
    /// type-matrix decides), 1 = `never`, 2 = `always`. Gate in
    /// `render_object_has_lightmap_shadow`.
    pub lightmap_shadow_mode: i16,
    /// `sweetener size` (char_enum) — sound-system field; not used
    /// by renderer.
    pub sweetener_size: i8,
    /// `water density` (char_enum) — physics field; not used by
    /// renderer.
    pub water_density: i8,
    /// `dynamic light sphere radius` (real). Override sphere for
    /// dynamic-lights bookkeeping; only used when non-zero.
    pub dynamic_light_sphere_radius: f32,
    /// `dynamic light sphere offset` (real_point_3d). Only consulted
    /// when `dynamic_light_sphere_radius > 0`.
    pub dynamic_light_sphere_offset: RealPoint3d,
    /// `default model variant` (string_id).
    pub default_model_variant: String,
    /// `model` (tag_reference → `hlmt`) — path to the `.model` tag.
    /// Empty when the placement is geometry-less. Chain:
    /// `model.render_model.path` → `.render_model` file.
    pub model: String,
    /// `crate object` (tag_reference → `bloc`).
    pub crate_object: String,
    /// `collision damage` (tag_reference → `cddf`).
    pub collision_damage: String,
    /// `early mover OBB` block (max 1) — engine
    /// `s_object_early_mover_obb_definition`.
    pub early_mover_obb: Vec<ObjectEarlyMoverObb>,
    /// `creation effect` (tag_reference → `effe`).
    pub creation_effect: String,
    /// `material effects` (tag_reference → `foot`).
    pub material_effects: String,
    /// `melee sound` (tag_reference → `snd!`).
    pub melee_sound: String,
    /// `ai properties` block (max 1) — combat dialogue + AI sizing.
    pub ai_properties: Vec<ObjectAiProperties>,
    /// `functions` (block, `object_function_block[]`, max 256).
    pub functions: Vec<ObjectFunctionDefinition>,
    /// `hud text message index` (short_integer).
    pub hud_text_message_index: i16,
    /// `secondary flags` (word_flags, `object_definition_secondary_flags`).
    pub secondary_flags: u16,
    /// `attachments` block (max 16).
    pub attachments: Vec<ObjectAttachment>,
    /// `widgets` block (max 4).
    pub widgets: Vec<ObjectWidget>,
    /// `change colors` block (max 4) — initial-permutation + function
    /// pairs per color channel.
    pub change_colors: Vec<ObjectChangeColors>,
    /// `multiplayer object` block (max 1) — MP gametype inclusion +
    /// boundary shape + spawn timing + boundary shaders.
    pub multiplayer_object: Vec<MultiplayerObject>,
    /// `health packs` block (max 16) — health-pack equipment refs.
    pub health_packs: Vec<ObjectHealthPack>,
}

impl ObjectDefinition {
    /// Read the `object_struct_definition` out of any of the 14
    /// object-subgroup tag files. Probes each inheritance prefix
    /// (`weapon/item/object`, `item/object`, `unit/object`,
    /// `device/object`, `object`, the root) and uses the first one
    /// where the `object_struct_definition`-specific field
    /// `lightmap shadow mode` is readable — the unique identifier
    /// among the inheritance levels.
    ///
    /// Errors when the tag's group is not in [`OBJECT_SUBGROUPS`].
    /// Returns a default-filled struct when the tag has no probable
    /// object substruct (unusual — most authored tags should have one).
    pub fn from_tag(tag: &TagFile) -> Result<Self, ObjectDefinitionError> {
        let actual = tag.group().tag.to_be_bytes();
        if !OBJECT_SUBGROUPS.contains(&actual) {
            return Err(ObjectDefinitionError::WrongGroup { actual });
        }
        let root = tag.root();
        for prefix in OBJECT_INHERITANCE_PREFIXES {
            let s = if prefix.is_empty() {
                root.clone()
            } else {
                match root.descend(prefix) {
                    Some(s) => s,
                    None => continue,
                }
            };
            // `lightmap shadow mode` is authored only in the
            // object_struct_definition — use it as the probe field.
            if s.read_int_any("lightmap shadow mode").is_some() {
                return Ok(Self::from_object_struct(&s));
            }
        }
        Ok(Self::default())
    }

    /// Construct directly from a [`TagStruct`] that IS the
    /// `object_struct_definition` (i.e. the caller already descended
    /// to the right inheritance level). Useful when an outer tag
    /// reader has already walked there.
    pub fn from_object_struct(obj: &TagStruct<'_>) -> Self {
        let flags = obj.read_int_any("flags").unwrap_or(0) as u16;
        let bounding_radius = obj.read_real("bounding radius").unwrap_or(0.0);
        let bounding_offset = obj.read_point3d("bounding offset");
        let acceleration_scale = obj.read_real("acceleration scale").unwrap_or(0.0);
        let lightmap_shadow_mode = obj
            .read_int_any("lightmap shadow mode")
            .unwrap_or(0) as i16;
        let sweetener_size = obj.read_int_any("sweetener size").unwrap_or(0) as i8;
        let water_density = obj.read_int_any("water density").unwrap_or(0) as i8;
        let dynamic_light_sphere_radius = obj
            .read_real("dynamic light sphere radius")
            .unwrap_or(0.0);
        let dynamic_light_sphere_offset = obj.read_point3d("dynamic light sphere offset");
        let default_model_variant = obj
            .read_string_id("default model variant")
            .unwrap_or_default();
        let model = obj.read_tag_ref_path("model").unwrap_or_default();
        let crate_object = obj.read_tag_ref_path("crate object").unwrap_or_default();
        let collision_damage = obj.read_tag_ref_path("collision damage").unwrap_or_default();
        let early_mover_obb = read_block_vec(obj, "early mover OBB", ObjectEarlyMoverObb::from_struct);
        let creation_effect = obj.read_tag_ref_path("creation effect").unwrap_or_default();
        let material_effects = obj.read_tag_ref_path("material effects").unwrap_or_default();
        let melee_sound = obj.read_tag_ref_path("melee sound").unwrap_or_default();
        let ai_properties = read_block_vec(obj, "ai properties", ObjectAiProperties::from_struct);
        let functions = read_block_vec(obj, "functions", ObjectFunctionDefinition::from_struct);
        let hud_text_message_index = obj
            .read_int_any("hud text message index")
            .unwrap_or(0) as i16;
        let secondary_flags = obj.read_int_any("secondary flags").unwrap_or(0) as u16;
        let attachments = read_block_vec(obj, "attachments", ObjectAttachment::from_struct);
        let widgets = read_block_vec(obj, "widgets", ObjectWidget::from_struct);
        let change_colors = read_block_vec(obj, "change colors", ObjectChangeColors::from_struct);
        let multiplayer_object = read_block_vec(obj, "multiplayer object", MultiplayerObject::from_struct);
        let health_packs = read_block_vec(obj, "health packs", ObjectHealthPack::from_struct);

        Self {
            flags,
            bounding_radius,
            bounding_offset,
            acceleration_scale,
            lightmap_shadow_mode,
            sweetener_size,
            water_density,
            dynamic_light_sphere_radius,
            dynamic_light_sphere_offset,
            default_model_variant,
            model,
            crate_object,
            collision_damage,
            early_mover_obb,
            creation_effect,
            material_effects,
            melee_sound,
            ai_properties,
            functions,
            hud_text_message_index,
            secondary_flags,
            attachments,
            widgets,
            change_colors,
            multiplayer_object,
            health_packs,
        }
    }

    /// Linear scan of `functions[]` for an entry whose `export_name`
    /// matches `name`. Engine equivalent: the inner loop in
    /// `object_get_function_value @ 0x1807DBA60` between LABEL_34 and
    /// the `goto LABEL_34;` step (`if ( v20->export_name == v9 )
    /// break;`).
    pub fn find_function_by_export(&self, name: &str) -> Option<&ObjectFunctionDefinition> {
        self.functions.iter().find(|f| f.export_name == name)
    }

    /// `true` iff `bounding_radius` is non-zero — i.e. the tag has an
    /// authored sphere (vs `0` = "use autogen", a Bungie convention).
    /// Callers should fall through to the .model's
    /// `model object data[0]` auto-bake or a vertex-walk autogen when
    /// this returns false.
    pub fn has_authored_bounding_sphere(&self) -> bool {
        self.bounding_radius > 0.0
    }

    /// Engine-relaxed shadow-eligibility gate. Returns `false` when
    /// either:
    ///   - `flags & OBJ_FLAG_DOES_NOT_CAST_SHADOW` (explicit tag-time
    ///     opt-out), OR
    ///   - `lightmap_shadow_mode == 1` (`never`).
    ///
    /// Otherwise `true`. The full engine gate
    /// (`render_object_has_lightmap_shadow @ 0x180696EE0`) drops
    /// static scenery from the runtime shadow loop when the cache
    /// builder bakes their shadows offline into the lightmap atlas;
    /// protomorph doesn't have offline object-shadow baking yet, so
    /// we treat everything not explicitly opted-out as runtime-cast.
    /// See `protomorph/src/halo/loader.rs::read_casts_shadow_flag`
    /// for the prior in-place implementation.
    pub fn casts_shadow_runtime(&self) -> bool {
        if self.flags & OBJ_FLAG_DOES_NOT_CAST_SHADOW != 0 {
            return false;
        }
        if self.lightmap_shadow_mode == 1 {
            return false;
        }
        true
    }
}
