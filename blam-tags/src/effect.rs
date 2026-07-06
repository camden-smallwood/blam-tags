//! `effect` (`effe`) tag walker — content side of the Halo 3 MCC
//! effects subsystem. An effect carries a timeline of events; each
//! event spawns parts (particles / light_volumes / contrails / beams /
//! decals / sounds / damage / screen_effects / objects / sub-effects)
//! at named marker locations.
//!
//! Three engine paths consume this tag:
//!
//! 1. **`effect_new_from_creation_data @ 0x1802FA480`** — allocates an
//!    `effect_datum` (156B) and an `event_datum` (20B) head, snapshots
//!    `definition_index`, and seeds the first event.
//!
//! 2. **`event_update_time @ 0x180309620` → `event_generate_part @
//!    0x180301C40`** — per-tick event firing. The MULTIPLEXER routes
//!    each `effect_part_block` entry by `runtime_base_group_tag` to
//!    the relevant subsystem creator (particles/light_volumes/etc.).
//!
//! 3. **`effects_render @ 0x1802FBD10`** — per-effect lens-flare submit
//!    in the front half, then `c_{particle,light_volume,contrail,beam}_system::submit_all`
//!    in the back half. Run during `_effect_pass_transparent`.
//!
//! Schema reference: `definitions/halo3_mcc/effect.json` (1650 lines).
//! Ares headers: `source/effects/effect_definitions.h` (104B
//! `effect_definition`, 68B `effect_event_definition`, 96B
//! `effect_part_definition`, 12B `effect_location_definition`, 20B
//! `effect_acceleration_definition`, 12B
//! `s_effect_conical_distribution_definition`).
//!
//! IDA gotcha: `s_cache_file_sound_definition` is aliased onto
//! `effect_definition` in decompiles — read by byte offset, never IDA
//! field name. See `reference_ida_mistyped_atmosphere_param` memory.
//!
//! ## P1 scope
//!
//! This walker handles the full effect spine: root + locations +
//! events + parts + accelerations + conical_distribution + looping
//! sound + the **outer** scalar fields of every
//! `particle_system_definition_block_new` embedded in
//! `effect_event_block.particle_systems[]`. The per-emitter property
//! evaluators (`c_editable_property`, `c_particle_movement_definition`,
//! GPU property/function/color blocks) are walked structurally as
//! [`ParticleSystemEmitter`] stubs in P1; P3 (particle subsystem) fills
//! them in.
//!
//! ## Field-name precision
//!
//! The binary tag layout embedded in `.effect` files (built by
//! tool.exe at cache-compile time) stores **normalized** field names,
//! not the raw schema strings. The normalization rules are:
//!
//! - drop trailing `^` (block primary-key marker)
//! - drop trailing `!` (runtime / internal field marker)
//! - drop `!*` / `*` annotations (and the entire field with them: see below)
//! - drop `:units` suffix (everything after the first `:`)
//! - drop `#description` suffix (everything after the first `#`)
//! - when a `{alias}` annotation is present, the alias REPLACES the
//!   base name (`"restart if within{overlap threshold}:..."` → `"overlap threshold"`)
//!
//! Some schema fields are absent from the embedded layout entirely —
//! tool.exe filtered them. For `effect_definition` these include
//! `continue if within`, `death_delay`, and `priority!*`. Their bytes
//! are still in the on-disk struct at the documented offsets, but
//! they're not addressable by name. P1 falls back to engine defaults
//! for stripped fields (priority=Normal, death_delay=0, etc.); raw
//! byte-offset reads land in P1.T2 or P3 if a use-site needs them.
//!
//! Also note: schema name `"runtime lightprobe_death_delay!"` maps to
//! the layout-stored name `"runtime death_delay"`; schema
//! `"runtime local_space_death_delay!"` maps to
//! `"runtime last_instance_index"`. The H3 engine uses the canonical
//! shorter names; the schema JSON's annotations are Reach-style
//! documentation that didn't propagate to MCC's compiled layout.

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::typed_enums::{Enum, Flags};
use crate::math::{
    AngleBounds, RealBounds, RealEulerAngles2d, RealPoint3d,
};

/// Read a `Tag(u32)` field by name. Returns `None` if missing or not
/// a `Tag` variant. `read_int_any` doesn't cover `Tag`, so per-walker
/// callers need this helper. Tag fourccs are stored big-endian (so
/// `b"prt3"` packs as `0x70727433`).
fn read_tag_fourcc(s: &TagStruct<'_>, name: &str) -> Option<u32> {
    match s.field(name)?.value()? {
        TagFieldData::Tag(v) => Some(v),
        _ => None,
    }
}

/// Errors from `effect` tag walking.
#[derive(Debug)]
pub enum EffectError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for EffectError {
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

impl std::error::Error for EffectError {}

pub const EFFECT_GROUP: [u8; 4] = *b"effe";

// ---------------------------------------------------------------------------
// effect_definition flags — `effect_flags` definition, on-disk count = 8
// (`k_on_disk_effect_definition_flag_count`). Engine `c_effect_definition_flags`
// adds 8 runtime bits past the on-disk byte.
// ---------------------------------------------------------------------------

pub const EFFECT_FLAG_DELETED_WHEN_INACTIVE: u32 = 1 << 0;
pub const EFFECT_FLAG_PARALLEL_EVENTS: u32 = 1 << 1;
pub const EFFECT_FLAG_NO_PART_REUSE: u32 = 1 << 2;
pub const EFFECT_FLAG_AGE_CREATORS_PRIMARY_WEAPON: u32 = 1 << 3;
pub const EFFECT_FLAG_LOCATIONS_HYBRID_WORLD_LOCAL: u32 = 1 << 4;
pub const EFFECT_FLAG_CAN_PENETRATE_WALLS: u32 = 1 << 5;
pub const EFFECT_FLAG_CANNOT_BE_RESTARTED: u32 = 1 << 6;
pub const EFFECT_FLAG_FORCE_USE_OWN_LIGHTPROBE: u32 = 1 << 7;

// ---------------------------------------------------------------------------
// event_flags — per-event control bits.
// ---------------------------------------------------------------------------

pub const EVENT_FLAG_DISABLED_FOR_DEBUGGING: u32 = 1 << 0;
pub const EVENT_FLAG_PARTICLES_DIE_WHEN_ORPHANED: u32 = 1 << 1;

// ---------------------------------------------------------------------------
// effect_location_flags
// ---------------------------------------------------------------------------

pub const LOCATION_FLAG_OPTIONAL: u32 = 1 << 0;
pub const LOCATION_FLAG_DESTRUCTIBLE: u32 = 1 << 1;
pub const LOCATION_FLAG_TRACK_SUBFRAME_MOVEMENTS: u32 = 1 << 2;

// ---------------------------------------------------------------------------
// effect_part_flags — Ares `c_effect_part_flags` (16-bit).
// ---------------------------------------------------------------------------

pub const PART_FLAG_WORLD_DOWN: u16 = 1 << 0;
pub const PART_FLAG_OFFSET_SPHERE: u16 = 1 << 1;
pub const PART_FLAG_NEVER_ATTACHED: u16 = 1 << 2;
pub const PART_FLAG_DISABLED_FOR_DEBUGGING: u16 = 1 << 3;
pub const PART_FLAG_DRAW_REGARDLESS_OF_DISTANCE: u16 = 1 << 4;
pub const PART_FLAG_MAKE_EVERY_TICK: u16 = 1 << 5;
pub const PART_FLAG_INHERIT_PARENT_VARIANT: u16 = 1 << 6;
pub const PART_FLAG_BATCHED_AOE_DAMAGE: u16 = 1 << 7;

// ---------------------------------------------------------------------------
// effect_part_scaleable_values — bits indicate which part parameters
// scale with `effect.scale_a` / `effect.scale_b`.
// ---------------------------------------------------------------------------

pub const SCALE_VELOCITY: u32 = 1 << 0;
pub const SCALE_VELOCITY_DELTA: u32 = 1 << 1;
pub const SCALE_VELOCITY_CONE_ANGLE: u32 = 1 << 2;
pub const SCALE_ANGULAR_VELOCITY: u32 = 1 << 3;
pub const SCALE_ANGULAR_VELOCITY_DELTA: u32 = 1 << 4;
pub const SCALE_TYPE_SPECIFIC: u32 = 1 << 5;

// ---------------------------------------------------------------------------
// particle_system_flags — applied to each particle_system inside an
// event. Per `particle_system_flags` enum in the schema.
// ---------------------------------------------------------------------------

pub const PARTICLE_SYSTEM_FLAG_FREEZE_WHEN_OFFSCREEN: u16 = 1 << 0;
pub const PARTICLE_SYSTEM_FLAG_CONTINUE_WHEN_OFFSCREEN: u16 = 1 << 1;
pub const PARTICLE_SYSTEM_FLAG_LOD_ALWAYS_1: u16 = 1 << 2;
pub const PARTICLE_SYSTEM_FLAG_LOD_SAME_IN_SPLITSCREEN: u16 = 1 << 3;
pub const PARTICLE_SYSTEM_FLAG_DISABLED_IN_SPLITSCREEN: u16 = 1 << 4;
pub const PARTICLE_SYSTEM_FLAG_DISABLED_FOR_DEBUGGING: u16 = 1 << 5;
pub const PARTICLE_SYSTEM_FLAG_INHERIT_EFFECT_VELOCITY: u16 = 1 << 6;
pub const PARTICLE_SYSTEM_FLAG_DONT_RENDER: u16 = 1 << 7;
pub const PARTICLE_SYSTEM_FLAG_RENDER_WHEN_ZOOMED: u16 = 1 << 8;
pub const PARTICLE_SYSTEM_FLAG_FORCE_CPU: u16 = 1 << 9;
pub const PARTICLE_SYSTEM_FLAG_FORCE_GPU: u16 = 1 << 10;
pub const PARTICLE_SYSTEM_FLAG_OVERRIDE_NEAR_FADE: u16 = 1 << 11;
pub const PARTICLE_SYSTEM_FLAG_DIE_WHEN_ORPHANED: u16 = 1 << 12;
pub const PARTICLE_SYSTEM_FLAG_GPU_OCCLUSION: u16 = 1 << 13;

// ---------------------------------------------------------------------------
// Enum: priority — `global_effect_priority_enum`, 3 values.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum EffectPriority {
    #[default]
    Normal = 0,
    High = 1,
    Essential = 2,
}

impl EffectPriority {
    pub fn from_int(v: i64) -> Self {
        match v {
            1 => Self::High,
            2 => Self::Essential,
            _ => Self::Normal,
        }
    }
}

// ---------------------------------------------------------------------------
// Enum: effect_environments
// ---------------------------------------------------------------------------

/// `effect_environments` (short_enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum EffectEnvironment {
    #[default]
    #[strum(serialize = "any environment")] Anywhere = 0,
    #[strum(serialize = "air only")] Air = 1,
    #[strum(serialize = "water only")] Water = 2,
    #[strum(serialize = "space only")] Vacuum = 3,
}

// ---------------------------------------------------------------------------
// Enum: effect_dispositions
// ---------------------------------------------------------------------------

/// `effect_dispositions` (short_enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum EffectDisposition {
    #[default]
    #[strum(serialize = "either mode")] Agnostic = 0,
    #[strum(serialize = "violent mode only")] Violent = 1,
    #[strum(serialize = "nonviolent mode only")] Nonviolent = 2,
}

// ---------------------------------------------------------------------------
// Enum: effect_camera_modes
// ---------------------------------------------------------------------------

/// `effect_camera_modes` (char_enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum EffectCameraMode {
    #[default]
    #[strum(serialize = "independent of camera mode")] Independent = 0,
    #[strum(serialize = "only in first person")] FirstPersonOnly = 1,
    #[strum(serialize = "only in third person")] ThirdPersonOnly = 2,
    #[strum(serialize = "both first and third")] Both = 3,
}

// ---------------------------------------------------------------------------
// Enum: coordinate_system_enum (particle systems only)
// ---------------------------------------------------------------------------

/// `coordinate_system_enum` (short_enum, particle systems only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum CoordinateSystem {
    #[default]
    #[strum(serialize = "world")] World = 0,
    #[strum(serialize = "local")] Local = 1,
}

/// `effect_flags` (long_flags) — Ares `e_effect_definition_flags`, the effect
/// DEFINITION authoring flags (distinct from the runtime `e_effect_flags`).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum EffectDefinitionFlags {
    #[strum(serialize = "deleted when attachment deactivates")] DeletedWhenAttachmentDeactivates = 0,
    #[strum(serialize = "run events in parallel")] RunEventsInParallel = 1,
    #[strum(serialize = "do not re-use parts when looping")] DoNotReusePartsWhenLooping = 2,
    #[strum(serialize = "age creator's weapon")] AgeCreatorsWeapon = 3,
    #[strum(serialize = "use parent position but world orientation")] UseParentPositionButWorldOrientation = 4,
    #[strum(serialize = "can penetrate walls (expensive)")] CanPenetrateWalls = 5,
    #[strum(serialize = "cannot be restarted")] CannotBeRestarted = 6,
    #[strum(serialize = "force use own lightprobe")] ForceUseOwnLightprobe = 7,
    #[strum(serialize = "heavy performance")] HeavyPerformance = 8,
    #[strum(serialize = "half resolution")] HalfResolution = 9,
    // --- runtime flags (Ares `e_effect_definition_flags` bits 10+) ---
    // Not authored in the tag (absent from the schema JSON, which ends at
    // `half resolution`); set by `effect_postprocess` at load. Included here so
    // the runtime `Flags<EffectDefinitionFlags, u32>` word can carry them, matching the
    // engine's single flags enum spanning on-disk (0-9) + runtime (10+).
    /// `_effect_samples_lightmap_bit` — any of the effect's particles tint from
    /// the lightmap. Gates `effect_create_lighting_samples` (dllcache 0x400).
    #[strum(disabled)] SamplesLightmap = 10,
    /// `_effect_samples_diffuse_texture_bit` — any particle tints from the
    /// diffuse texture. Gates `effect_create_lighting_samples` (dllcache 0x800).
    #[strum(disabled)] SamplesDiffuseTexture = 11,
}

/// `event_flags` (long_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum EffectEventFlags {
    #[strum(serialize = "disabled for debugging")] DisabledForDebugging = 0,
    #[strum(serialize = "particles die when effect ends")] ParticlesDieWhenEffectEnds = 1,
}

/// `effect_part_flags` (word_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum EffectPartFlags {
    #[strum(serialize = "face down regardless of location (decals)")] FaceDownRegardlessOfLocation = 0,
    #[strum(serialize = "offset origin away from geometry (lights)")] OffsetOriginAwayFromGeometry = 1,
    #[strum(serialize = "never attached to object")] NeverAttachedToObject = 2,
    #[strum(serialize = "disabled for debugging")] DisabledForDebugging = 3,
    #[strum(serialize = "draw regardless of distance")] DrawRegardlessOfDistance = 4,
    #[strum(serialize = "make every tick")] MakeEveryTick = 5,
    #[strum(serialize = "inherit parent variant")] InheritParentVariant = 6,
    #[strum(serialize = "batch AOE damage")] BatchAoeDamage = 7,
}

/// `effect_location_flags` (long_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum EffectLocationFlags {
    #[strum(serialize = "optional (no error if not found)")] Optional = 0,
    #[strum(serialize = "destructible (no error if goes away)")] Destructible = 1,
    #[strum(serialize = "track sub-frame movements")] TrackSubFrameMovements = 2,
}

/// `particle_system_flags` (word_flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum ParticleSystemFlags {
    #[strum(serialize = "particles freeze when offscreen")] ParticlesFreezeWhenOffscreen = 0,
    #[strum(serialize = "particles continue as usual when offscreen")] ParticlesContinueWhenOffscreen = 1,
    #[strum(serialize = "lod always 1.0")] LodAlways1 = 2,
    #[strum(serialize = "lod same in splitscreen")] LodSameInSplitscreen = 3,
    #[strum(serialize = "disabled in 3- and 4-way splitscreen")] DisabledIn3And4WaySplitscreen = 4,
    #[strum(serialize = "disabled for debugging")] DisabledForDebugging = 5,
    #[strum(serialize = "inherit effect velocity")] InheritEffectVelocity = 6,
    #[strum(serialize = "don't render system")] DontRenderSystem = 7,
    #[strum(serialize = "render when zoomed")] RenderWhenZoomed = 8,
    #[strum(serialize = "force cpu updating")] ForceCpuUpdating = 9,
    #[strum(serialize = "force gpu updating")] ForceGpuUpdating = 10,
    #[strum(serialize = "override near fade (use with caution)")] OverrideNearFade = 11,
    #[strum(serialize = "particles die when effect ends")] ParticlesDieWhenEffectEnds = 12,
    #[strum(serialize = "gpu occlusion (weather only)")] GpuOcclusion = 13,
    // ---- 2007-era H3 `particle_system_flags` superset ----
    // Older shipped tags (e.g. levels/multi/riverworld/fx/*) author a
    // different bit layout for this word_flags whose names are absent
    // from the MCC json schema above. Name-keyed decode resolves by
    // name, so we add the divergent names as additional variants (per
    // the SUPERSET playbook — no raw-bits escape hatch). The MCC names
    // that already string-match (disabled for debugging / inherit
    // effect velocity / don't render system / render when zoomed /
    // force cpu/gpu updating / override near fade / gpu occlusion) reuse
    // the variants above.
    #[strum(serialize = "cinematics")] Cinematics = 14,
    #[strum(serialize = "expensive visibility")] ExpensiveVisibility = 15,
    #[strum(serialize = "ignore lod (render always)")] IgnoreLodRenderAlways = 16,
    #[strum(serialize = "die when orphaned")] DieWhenOrphaned = 17,
}

/// `effect_part_scaleable_values` (long_flags) — which per-part values
/// the A/B effect scales drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum EffectPartScaleableValues {
    #[strum(serialize = "velocity")] Velocity = 0,
    #[strum(serialize = "velocity delta")] VelocityDelta = 1,
    #[strum(serialize = "velocity cone angle")] VelocityConeAngle = 2,
    #[strum(serialize = "angular velocity")] AngularVelocity = 3,
    #[strum(serialize = "angular velocity delta")] AngularVelocityDelta = 4,
    #[strum(serialize = "type-specific scale")] TypeSpecificScale = 5,
}

/// `emission_shape_enum` (char_enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum EmissionShape {
    #[default]
    #[strum(serialize = "sprayer")] Sprayer = 0,
    #[strum(serialize = "disc")] Disc = 1,
    #[strum(serialize = "globe")] Globe = 2,
    #[strum(serialize = "implode")] Implode = 3,
    #[strum(serialize = "tube")] Tube = 4,
    #[strum(serialize = "halo")] Halo = 5,
    #[strum(serialize = "impact contour")] ImpactContour = 6,
    #[strum(serialize = "impact area")] ImpactArea = 7,
    #[strum(serialize = "debris")] Debris = 8,
    #[strum(serialize = "line")] Line = 9,
    #[strum(serialize = "breakable surface")] BreakableSurface = 10,
}

/// `emission_axis_enum` (char_enum) — the particle-model spawn axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum EmissionAxis {
    #[default]
    #[strum(serialize = "constant")] Constant = 0,
    #[strum(serialize = "cone")] Cone = 1,
    #[strum(serialize = "disc")] Disc = 2,
    #[strum(serialize = "globe")] Globe = 3,
}

/// `emission_reference_axis_enum` (char_enum) — reference axis for emission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum EmissionReferenceAxis {
    #[default]
    #[strum(serialize = "x")] X = 0,
    #[strum(serialize = "y")] Y = 1,
    #[strum(serialize = "z")] Z = 2,
}

// ---------------------------------------------------------------------------
// EffectPartType — the multiplexer dispatch enum derived from
// `effect_part_block.runtime base group tag!`. Engine `event_generate_part
// @ 0x180301C40` switches on this u32 (4-byte tag fourcc, big-endian
// packed) to route each part to its subsystem creator.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EffectPartType {
    /// Unrecognized or unauthored — empty fourcc (`\0\0\0\0`). Engine
    /// `event_generate_part` logs and skips.
    #[default]
    Empty,
    /// `prt3` — particle system. → `c_particle_system::create` via
    /// `EffectMessage::CreateParticleSystem`.
    Particle,
    /// `ltvl` — light volume. → `c_light_volume_system::create`.
    LightVolume,
    /// `beam` — energy beam. → `c_beam_system::create`.
    Beam,
    /// `cntl` — contrail / ribbon trail. → `c_contrail_system::create`.
    Contrail,
    /// `decs` — decal system. → `c_decal_system::create`.
    DecalSystem,
    /// `snd!` — sound (impulse or looping). Spawned immediately (no
    /// render-thread message).
    Sound,
    /// `jpt!` — damage effect / AOE damage. Spawned immediately,
    /// optionally batched via `c_aoe_damage_batchifier`.
    DamageEffect,
    /// `sefc` — area screen effect. Spawned immediately via
    /// `screen_effect_new`.
    ScreenEffect,
    /// `obje` — generic object spawning. Engine routes via
    /// `object_placement_data_new` → `object_new`. Concrete subtypes
    /// (`proj`/`scen`/`crate`/etc.) get their own variants below — the
    /// `obje` fourcc itself is rare in effect parts.
    Object,
    /// `proj` — projectile spawning. Engine spawns via `object_new`
    /// with `e_object_type::_object_type_projectile`.
    Projectile,
    /// `scen` — scenery spawning.
    Scenery,
    /// `bloc` — crate (placed object) spawning.
    Crate,
    /// `char` — character / AI event. Calls `ai_handle_effect_creation`.
    AiEvent,
    /// `lwrd` / `rwrd` — water interaction surface event.
    WaterInteraction,
    /// `effe` — nested sub-effect. Recurses into
    /// `effect_new_from_creation_data`.
    SubEffect,
    /// `lens` — lens flare. Spawned by `effect_generate_lens_flares`
    /// when the parent effect ticks the transparent pass.
    LensFlare,
    /// `ligh` — H3 light tag. Spawned via `light_new_unattached` (or
    /// attached if part flags say so). The Ares-listed `lite` variant
    /// is Reach legacy; H3 uses `ligh`.
    Light,
    /// `lite` — Reach legacy light fourcc. H3 typically ignores.
    LightLegacy,
    /// Unrecognized fourcc. Engine logs and skips.
    Unknown([u8; 4]),
}

impl EffectPartType {
    /// Decode from `runtime base group tag!` (a `Tag(u32)` field, BE
    /// packed). The schema stores the fourcc as big-endian `u32`, so
    /// `b"prt3"` becomes `0x70727433`.
    pub fn from_fourcc(raw: u32) -> Self {
        let bytes = raw.to_be_bytes();
        if bytes == [0; 4] {
            return Self::Empty;
        }
        match &bytes {
            b"prt3" => Self::Particle,
            b"ltvl" => Self::LightVolume,
            b"beam" => Self::Beam,
            b"cntl" => Self::Contrail,
            b"decs" => Self::DecalSystem,
            b"snd!" => Self::Sound,
            b"jpt!" => Self::DamageEffect,
            b"sefc" => Self::ScreenEffect,
            b"obje" => Self::Object,
            b"proj" => Self::Projectile,
            b"scen" => Self::Scenery,
            b"bloc" => Self::Crate,
            b"char" => Self::AiEvent,
            b"lwrd" | b"rwrd" => Self::WaterInteraction,
            b"effe" => Self::SubEffect,
            b"lens" => Self::LensFlare,
            b"ligh" => Self::Light,
            b"lite" => Self::LightLegacy,
            _ => Self::Unknown(bytes),
        }
    }

    pub fn as_fourcc(&self) -> [u8; 4] {
        match self {
            Self::Empty => [0; 4],
            Self::Particle => *b"prt3",
            Self::LightVolume => *b"ltvl",
            Self::Beam => *b"beam",
            Self::Contrail => *b"cntl",
            Self::DecalSystem => *b"decs",
            Self::Sound => *b"snd!",
            Self::DamageEffect => *b"jpt!",
            Self::ScreenEffect => *b"sefc",
            Self::Object => *b"obje",
            Self::Projectile => *b"proj",
            Self::Scenery => *b"scen",
            Self::Crate => *b"bloc",
            Self::AiEvent => *b"char",
            Self::WaterInteraction => *b"lwrd",
            Self::SubEffect => *b"effe",
            Self::LensFlare => *b"lens",
            Self::Light => *b"ligh",
            Self::LightLegacy => *b"lite",
            Self::Unknown(b) => *b,
        }
    }
}

// ---------------------------------------------------------------------------
// `effect_locations_block` — 12B authored, max 8 per effect tag.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectLocation {
    /// `marker name^` — `old_string_id` referencing a node-marker on the
    /// attached object. Special names (`gravity`, `up`, `normal`,
    /// `forward`, `backward`, `reflection`, `replace`, `root`,
    /// `impact`, `water_surface`, `structure_surface`, `child`) are
    /// resolved by the engine — see schema explanation at field 0.
    pub marker_name: String,
    /// `effect_location_flags`.
    pub flags: Flags<EffectLocationFlags, u32>,
    /// `priority!*` — `global_effect_priority_enum`.
    pub priority: EffectPriority,
}

impl EffectLocation {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let marker_name = s.read_string_id("marker name").unwrap_or_default();
        let flags = s.try_read_flags("flags").unwrap_or_default();
        // priority field is stripped from the embedded layout — defaults
        // to Normal per engine `global_effect_priority_enum` zero value.
        let priority = EffectPriority::Normal;
        Self { marker_name, flags, priority }
    }
}

// ---------------------------------------------------------------------------
// `effect_part_block` — 96B authored. The multiplexer dispatch unit.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectPart {
    pub environment: Enum<EffectEnvironment, i16>,
    pub violence_mode: Enum<EffectDisposition, i16>,
    /// Block index into the owning effect's `locations[]`. -1 means
    /// "use default location" (engine substitutes the first valid).
    pub location: i16,
    /// Beams use a second endpoint marker; -1 for non-beam parts.
    pub secondary_location_beams: i16,
    /// `effect_part_flags`.
    pub flags: Flags<EffectPartFlags, u16>,
    pub priority: EffectPriority,
    pub camera_mode: Enum<EffectCameraMode, i8>,
    /// `runtime base group tag!` — engine-computed `c_tag_index`
    /// dispatch key. Schema stores as `Tag(u32)` BE fourcc.
    pub runtime_base_group_tag: [u8; 4],
    /// Decoded dispatch type. The multiplexer in `event_generate_part`
    /// switches on this.
    pub part_type: EffectPartType,
    /// `type^` — tag reference to particle / light_volume / beam /
    /// contrail / decal / sound / damage_effect / screen_effect /
    /// object / sub-effect / etc.
    pub type_tag_path: String,
    /// Tag group fourcc of the type reference (matches
    /// `runtime_base_group_tag` after engine post-processing).
    pub type_group: [u8; 4],
    /// Initial velocity along location forward. For decals this is the
    /// raycast distance (defaults to 0.5).
    pub velocity_bounds: RealBounds,
    pub velocity_orientation_yaw_pitch: RealEulerAngles2d,
    /// Cone angle (degrees) within which the initial velocity vector
    /// is randomized.
    pub velocity_cone_angle_degrees: f32,
    /// Degrees per second.
    pub angular_velocity_bounds: AngleBounds,
    pub radius_modifier_bounds: RealBounds,
    pub relative_offset: RealPoint3d,
    pub relative_orientation_yaw_pitch: RealEulerAngles2d,
    /// Bitmask of `SCALE_*` constants. Bits indicate which params
    /// multiply by `effect.scale_a`.
    pub a_scales_values: Flags<EffectPartScaleableValues, u32>,
    /// Bitmask of `SCALE_*` constants. Bits indicate which params
    /// multiply by `effect.scale_b`.
    pub b_scales_values: Flags<EffectPartScaleableValues, u32>,
}

impl EffectPart {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let environment = s.try_read_enum("create in").unwrap_or_default();
        let violence_mode = s.try_read_enum("violence mode").unwrap_or_default();
        let location = s.read_int_any("location").unwrap_or(-1) as i16;
        // `camera mode` is an authored short_enum — resolve it by name.
        let camera_mode = s.try_read_enum("camera mode").unwrap_or_default();
        // Not surfaced: secondary_location_beams (beam parts only), priority
        // (`priority!*` is a runtime/readonly field — always Normal in source
        // tags), velocity_orientation_yaw_pitch (defaults to (0,0)).
        let secondary_location_beams: i16 = -1;
        let priority = EffectPriority::Normal;
        let velocity_orientation_yaw_pitch = RealEulerAngles2d { yaw: 0.0, pitch: 0.0 };
        let flags = s.try_read_flags("flags").unwrap_or_default();
        // `runtime base group tag` (Tag(u32)) is computed by tool.exe
        // at cache-build from the `type` tag_reference's group. Read
        // it directly; if empty, fall back to type_group below.
        let runtime_base_group_tag_raw =
            read_tag_fourcc(s, "runtime base group tag").unwrap_or(0);
        let (type_group_u32, type_tag_path) = s
            .read_tag_ref_with_group("type")
            .unwrap_or((0, String::new()));
        let dispatch_fourcc = if runtime_base_group_tag_raw != 0 {
            runtime_base_group_tag_raw
        } else {
            type_group_u32
        };
        let runtime_base_group_tag = dispatch_fourcc.to_be_bytes();
        let part_type = EffectPartType::from_fourcc(dispatch_fourcc);
        let type_group = type_group_u32.to_be_bytes();
        let velocity_bounds = s.read_real_bounds("velocity bounds");
        let velocity_cone_angle_degrees =
            s.read_real("velocity cone angle").unwrap_or(0.0);
        let angular_velocity_bounds =
            s.read_angle_bounds("angular velocity bounds");
        let radius_modifier_bounds = s.read_real_bounds("radius modifier bounds");
        let relative_offset = s.read_point3d("relative offset");
        let relative_orientation_yaw_pitch =
            s.read_euler2d("relative orientation (yaw, pitch)");
        let a_scales_values = s.try_read_flags("A scales values").unwrap_or_default();
        let b_scales_values = s.try_read_flags("B scales values").unwrap_or_default();
        Self {
            environment,
            violence_mode,
            location,
            secondary_location_beams,
            flags,
            priority,
            camera_mode,
            runtime_base_group_tag,
            part_type,
            type_tag_path,
            type_group,
            velocity_bounds,
            velocity_orientation_yaw_pitch,
            velocity_cone_angle_degrees,
            angular_velocity_bounds,
            radius_modifier_bounds,
            relative_offset,
            relative_orientation_yaw_pitch,
            a_scales_values,
            b_scales_values,
        }
    }
}

// ---------------------------------------------------------------------------
// `effect_accelerations_block` — 20B authored. Applies directional
// acceleration to all particles in active locations during the event.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectAcceleration {
    pub environment: Enum<EffectEnvironment, i16>,
    pub violence_mode: Enum<EffectDisposition, i16>,
    pub location: i16,
    pub acceleration: f32,
    pub inner_cone_angle_degrees: f32,
    pub outer_cone_angle_degrees: f32,
}

impl EffectAcceleration {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let environment = s.try_read_enum("create in").unwrap_or_default();
        let violence_mode = s.try_read_enum("violence mode").unwrap_or_default();
        let location = s.read_int_any("location").unwrap_or(-1) as i16;
        let acceleration = s.read_real("acceleration").unwrap_or(0.0);
        let inner_cone_angle_degrees = s.read_real("inner cone angle").unwrap_or(0.0);
        let outer_cone_angle_degrees = s.read_real("outer cone angle").unwrap_or(0.0);
        Self {
            environment,
            violence_mode,
            location,
            acceleration,
            inner_cone_angle_degrees,
            outer_cone_angle_degrees,
        }
    }
}

// ---------------------------------------------------------------------------
// `particle_system_emitter_definition_block` — 752B authored. Tier
// 1.12 (P3) expansion: walk all 18 per-emitter property curves +
// the inline particle_physics struct so consumers can evaluate emitter
// behaviour without going back to the bytes.
// ---------------------------------------------------------------------------

use crate::effects_properties::EditableProperty;
use crate::particle_physics::ParticlePhysics;

/// `c_particle_emitter_definition::e_emitter_flags` — the runtime
/// CPU/GPU-classification bitfield. The schema option list is empty
/// (these bits are not authored: the cache compiler / runtime
/// `decide_whether_cpu_or_gpu` + `postprocess_block_proc` set them, and
/// loose tags ship them stripped to 0). Kept as named variants so the
/// runtime port sets/tests them through the typed `Flags` wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u8)]
pub enum EmitterDefinitionFlags {
    #[strum(serialize = "can be gpu")] CanBeGpu = 0,
    #[strum(serialize = "is cpu")] IsCpu = 1,
    #[strum(serialize = "is gpu")] IsGpu = 2,
    #[strum(serialize = "becomes gpu when at rest")] BecomesGpuWhenAtRest = 3,
    #[strum(serialize = "using black point")] UsingBlackPoint = 4,
}

/// `c_particle_emitter_definition::e_property` — the 18 per-emitter editable
/// property slots, used as bit indices into the runtime
/// `constant_per_particle_properties` / `constant_over_time_properties`
/// summaries. An index enum (no schema names), so `Flags<EmitterProperty, i32>`
/// carries those masks.
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive)]
#[repr(u32)]
pub enum EmitterProperty {
    EmitterOffset = 0,
    EmitterDirection = 1,
    EmissionRadius = 2,
    EmissionAngle = 3,
    ParticleAxisAngle = 4,
    EmitterStartingCount = 5,
    EmitterMaxCount = 6,
    EmissionRate = 7,
    ParticleLifespan = 8,
    ParticleSelfAcceleration = 9,
    ParticleVelocity = 10,
    ParticleRotation = 11,
    ParticleRotationRate = 12,
    ParticleSize = 13,
    ParticleScale = 14,
    ParticleTint = 15,
    ParticleAlpha = 16,
    ParticleBlackPoint = 17,
}

#[derive(Debug, Clone, Default)]
pub struct ParticleSystemEmitter {
    pub name: String,
    /// `emission_shape_enum` — sprayer/disc/globe/implode/tube/halo/
    /// impact_contour/impact_area/debris/line/breakable_surface.
    /// Raw integer until P3 ports the full enum.
    pub emission_shape: Enum<EmissionShape, i8>,
    /// `emitter_flags` (`c_particle_emitter_definition::e_emitter_flags`)
    /// — runtime CPU/GPU-classification bits, reconstructed at postprocess
    /// (loose tags ship them 0). Typed `Flags` so the runtime port
    /// sets/tests through the wrapper.
    pub flags: Flags<EmitterDefinitionFlags, u8>,
    /// `emission_axis_enum` — constant/cone/disc/globe.
    pub particle_axis_for_models: Enum<EmissionAxis, i8>,
    /// `emission_reference_axis_enum` — x/y/z.
    pub particle_reference_axis: Enum<EmissionReferenceAxis, i8>,
    pub bounding_radius_estimate: f32,
    pub bounding_radius_override: f32,
    pub uv_scrolling: crate::math::RealVector2d,

    // ---- 5 emission shape curves ----
    pub translational_offset: EditableProperty,
    pub relative_direction: EditableProperty,
    pub emission_radius: EditableProperty,
    pub emission_angle: EditableProperty,
    pub emission_axis_angle: EditableProperty,

    // ---- 4 emission rate curves ----
    pub particle_starting_count: EditableProperty,
    pub particle_max_count: EditableProperty,
    pub particle_emission_rate: EditableProperty,
    pub particle_lifespan: EditableProperty,

    /// Inline `particle_physics_struct` — engine treats this as either
    /// an external pmov template reference (`template` field set) or a
    /// fully inlined movements[] authoring. Same shape as the standalone
    /// pmov tag root struct.
    pub particle_movement: ParticlePhysics,

    // ---- 4 motion curves ----
    pub particle_self_acceleration: EditableProperty,
    pub particle_initial_velocity: EditableProperty,
    pub particle_rotation: EditableProperty,
    pub particle_initial_rotation_rate: EditableProperty,

    // ---- 5 appearance curves ----
    pub particle_size: EditableProperty,
    pub particle_scale: EditableProperty,
    /// `particle tint` — RGB color property. Layout matches scalar
    /// (`particle_property_color_struct_new` is identical to scalar);
    /// runtime interprets `constant_value` differently for color slots.
    pub particle_tint: EditableProperty,
    pub particle_alpha: EditableProperty,
    pub particle_alpha_black_point: EditableProperty,

    // ---- runtime-resolved gpu_data summary ----
    /// Bit `p` set ⇔ property `p` (`EmitterProperty`) is constant per particle.
    pub runtime_constant_per_particle_properties: Flags<EmitterProperty, i32>,
    /// Bit `p` set ⇔ property `p` (`EmitterProperty`) is constant over time.
    pub runtime_constant_over_time_properties: Flags<EmitterProperty, i32>,
    /// Particle-state bit accumulator (indexed by particle-state, OR'd across
    /// the postprocess seed pass) — kept a raw mask, not a named-flag set.
    pub runtime_used_particle_states: i32,
}

impl ParticleSystemEmitter {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let name = s.read_string_id("emitter name").unwrap_or_default();
        let emission_shape = s.try_read_enum("emission shape").unwrap_or_default();
        // `emitter_flags`: runtime bits, stripped in loose tags → empty.
        let flags = s.try_read_flags("flags").unwrap_or_default();
        let particle_axis_for_models =
            s.try_read_enum("particle axis (for models)").unwrap_or_default();
        let particle_reference_axis =
            s.try_read_enum("particle reference axis").unwrap_or_default();
        let bounding_radius_estimate = s
            .read_real("bounding radius estimate")
            .unwrap_or(0.0);
        let bounding_radius_override = s
            .read_real("bounding radius override")
            .unwrap_or(0.0);
        let uv_scrolling = s.read_vec2("uv scrolling");

        // Inline particle_physics_struct — reuse the standalone walker.
        let particle_movement = s
            .field("particle movement")
            .and_then(|f| f.as_struct())
            .map(|inner| ParticlePhysics::from_struct(&inner))
            .unwrap_or_default();

        Self {
            name,
            emission_shape,
            flags,
            particle_axis_for_models,
            particle_reference_axis,
            bounding_radius_estimate,
            bounding_radius_override,
            uv_scrolling,
            translational_offset: read_property(s, "translational offset"),
            relative_direction: read_property(s, "relative direction"),
            emission_radius: read_property(s, "emission radius"),
            emission_angle: read_property(s, "emission angle"),
            emission_axis_angle: read_property(s, "emission axis angle"),
            particle_starting_count: read_property(s, "particle starting count"),
            particle_max_count: read_property(s, "particle max count"),
            particle_emission_rate: read_property(s, "particle emission rate"),
            particle_lifespan: read_property(s, "particle lifespan"),
            particle_movement,
            particle_self_acceleration: read_property(s, "particle self-acceleration"),
            particle_initial_velocity: read_property(s, "particle initial velocity"),
            particle_rotation: read_property(s, "particle rotation"),
            particle_initial_rotation_rate: read_property(s, "particle initial rotation rate"),
            particle_size: read_property(s, "particle size"),
            particle_scale: read_property(s, "particle scale"),
            particle_tint: read_property(s, "particle tint"),
            particle_alpha: read_property(s, "particle alpha"),
            particle_alpha_black_point: read_property(s, "particle alpha black point"),
            runtime_constant_per_particle_properties: Flags::from_bits(
                s.read_int_any("runtime m_constant_per_particle_properties")
                    .unwrap_or(0) as i32,
            ),
            runtime_constant_over_time_properties: Flags::from_bits(
                s.read_int_any("runtime m_constant_over_time_properties")
                    .unwrap_or(0) as i32,
            ),
            runtime_used_particle_states: s
                .read_int_any("runtime m_used_particle_states")
                .unwrap_or(0) as i32,
        }
    }
}

fn read_property(parent: &TagStruct<'_>, name: &str) -> EditableProperty {
    parent
        .field(name)
        .and_then(|f| f.as_struct())
        .map(|inner| EditableProperty::from_struct(&inner))
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// `particle_system_definition_block_new` — 92B authored. Outer fields
// (priority/particle ref/location/coord_system/environment/disposition/
// camera/sort_bias/flags/budgets/LOD) + the full `emitters[]` block —
// each emitter walks all 18 editable-property curves (incl. the typed
// vector/point interpolants) via [`ParticleSystemEmitter::from_struct`].
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ParticleSystemDefinition {
    pub priority: EffectPriority,
    /// `tag_reference` to a `prt3` tag (the particle definition).
    pub particle_tag_path: String,
    /// Block index into the owning effect's `locations[]`. `long_block_index`
    /// per schema (wider than other location references which use
    /// `short_block_index`).
    pub location: i32,
    pub coordinate_system: Enum<CoordinateSystem, i16>,
    pub environment: Enum<EffectEnvironment, i16>,
    pub disposition: Enum<EffectDisposition, i16>,
    pub camera_mode: Enum<EffectCameraMode, i16>,
    /// `sort_bias`: -10..10 typical, positive = closer to camera.
    pub sort_bias: i16,
    /// `particle_system_flags`.
    pub flags: Flags<ParticleSystemFlags, u16>,
    pub pixel_budget_ms: f32,
    pub near_fade_range: f32,
    pub near_fade_cutoff: f32,
    pub near_fade_override: f32,
    pub lod_in_distance: f32,
    pub lod_feather_in_delta: f32,
    pub inverse_lod_feather_in: f32,
    pub lod_out_distance: f32,
    pub lod_feather_out_delta: f32,
    pub inverse_lod_feather_out: f32,
    /// Emitters block — max 8 per system per schema
    /// `c_particle_system_definition::k_maximum_emitters_per_definition`.
    pub emitters: Vec<ParticleSystemEmitter>,
    pub runtime_max_lifespan: f32,
}

impl ParticleSystemDefinition {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        // priority is stripped from the embedded layout.
        let priority = EffectPriority::Normal;
        let particle_tag_path = s.read_tag_ref_path("particle").unwrap_or_default();
        let location = s.read_int_any("location").unwrap_or(-1) as i32;
        let coordinate_system = s.try_read_enum("coordinate system").unwrap_or_default();
        let environment = s.try_read_enum("environment").unwrap_or_default();
        let disposition = s.try_read_enum("disposition").unwrap_or_default();
        let camera_mode = s.try_read_enum("camera mode").unwrap_or_default();
        let sort_bias = s.read_int_any("sort bias").unwrap_or(0) as i16;
        let flags = s.try_read_flags("flags").unwrap_or_default();
        let pixel_budget_ms = s.read_real("Pixel budget").unwrap_or(0.0);
        let near_fade_range = s.read_real("near fade range").unwrap_or(0.0);
        let near_fade_cutoff = s.read_real("near fade cutoff").unwrap_or(0.0);
        let near_fade_override = s.read_real("near fade override").unwrap_or(0.0);
        let lod_in_distance = s.read_real("LOD in distance").unwrap_or(0.0);
        let lod_feather_in_delta = s.read_real("LOD feather in delta").unwrap_or(0.0);
        let inverse_lod_feather_in = s.read_real("inverse LOD feather in").unwrap_or(0.0);
        let lod_out_distance = s.read_real("LOD out distance").unwrap_or(0.0);
        let lod_feather_out_delta = s.read_real("LOD feather out delta").unwrap_or(0.0);
        let inverse_lod_feather_out =
            s.read_real("inverse LOD feather out").unwrap_or(0.0);
        let emitters = s
            .field("emitters")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(entry) = b.element(i) {
                        out.push(ParticleSystemEmitter::from_struct(&entry));
                    }
                }
                out
            })
            .unwrap_or_default();
        let runtime_max_lifespan = s.read_real("runtime max lifespan").unwrap_or(0.0);
        Self {
            priority,
            particle_tag_path,
            location,
            coordinate_system,
            environment,
            disposition,
            camera_mode,
            sort_bias,
            flags,
            pixel_budget_ms,
            near_fade_range,
            near_fade_cutoff,
            near_fade_override,
            lod_in_distance,
            lod_feather_in_delta,
            inverse_lod_feather_in,
            lod_out_distance,
            lod_feather_out_delta,
            inverse_lod_feather_out,
            emitters,
            runtime_max_lifespan,
        }
    }
}

// ---------------------------------------------------------------------------
// `effect_event_block` — 68B authored, max 32 per effect tag.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectEvent {
    /// `event name^` — `string_id`.
    pub name: String,
    /// `event_flags`.
    pub flags: Flags<EffectEventFlags, u32>,
    pub priority: EffectPriority,
    /// `skip fraction#chance that this event will be skipped entirely`
    /// — `real_fraction` in `[0, 1]`.
    pub skip_fraction: f32,
    /// `delay bounds:seconds` — random delay before event fires.
    pub delay_bounds: RealBounds,
    /// `duration bounds:seconds` — random window during which parts fire.
    pub duration_bounds: RealBounds,
    pub parts: Vec<EffectPart>,
    pub accelerations: Vec<EffectAcceleration>,
    pub particle_systems: Vec<ParticleSystemDefinition>,
}

impl EffectEvent {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let name = s.read_string_id("event name").unwrap_or_default();
        let flags = s.try_read_flags("flags").unwrap_or_default();
        // priority stripped from layout.
        let priority = EffectPriority::Normal;
        let skip_fraction = s.read_real("skip fraction").unwrap_or(0.0);
        let delay_bounds = s.read_real_bounds("delay bounds");
        let duration_bounds = s.read_real_bounds("duration bounds");
        let parts = read_block(s, "parts", EffectPart::from_struct);
        let accelerations = read_block(s, "accelerations", EffectAcceleration::from_struct);
        let particle_systems = read_block(
            s,
            "particle systems",
            ParticleSystemDefinition::from_struct,
        );
        Self {
            name,
            flags,
            priority,
            skip_fraction,
            delay_bounds,
            duration_bounds,
            parts,
            accelerations,
            particle_systems,
        }
    }
}

// ---------------------------------------------------------------------------
// `effect_conical_distribution_block` — 12B authored, max 1 per
// effect. "Shotgun" projectile spread: `projectile_count = yaw_count *
// pitch_count`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectConicalDistribution {
    pub yaw_count: i16,
    pub pitch_count: i16,
    /// `distribution exponent` — 0.5 = even, >0.5 = tighter toward
    /// center.
    pub distribution_exponent: f32,
    /// `spread#degrees`.
    pub spread_degrees: f32,
}

impl EffectConicalDistribution {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let yaw_count = s.read_int_any("yaw count").unwrap_or(0) as i16;
        let pitch_count = s.read_int_any("pitch count").unwrap_or(0) as i16;
        let distribution_exponent =
            s.read_real("distribution exponent").unwrap_or(0.5);
        let spread_degrees = s.read_real("spread").unwrap_or(0.0);
        Self {
            yaw_count,
            pitch_count,
            distribution_exponent,
            spread_degrees,
        }
    }
}

// ---------------------------------------------------------------------------
// `effect_struct_definition` — 104B root.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct EffectDefinition {
    /// `effect_flags`.
    pub flags: Flags<EffectDefinitionFlags, u32>,
    /// `fixed random seed` — non-zero ⇒ deterministic effect.
    pub fixed_random_seed: i32,
    /// `restart if within{overlap threshold}:world units` — engine
    /// `effect_find_reusable_instance` consults this radius.
    pub restart_if_within: f32,
    /// `continue if within:world units`.
    pub continue_if_within: f32,
    pub death_delay: f32,
    pub priority: EffectPriority,
    /// `loop start event` — block index into `events[]`. -1 for
    /// non-looping effects.
    pub loop_start_event: i16,
    /// `runtime danger radius!` — computed at postprocess (max AOE
    /// danger of any part).
    pub runtime_danger_radius: f32,
    pub locations: Vec<EffectLocation>,
    pub events: Vec<EffectEvent>,
    /// `looping sound` — tag_reference to `lsnd`.
    pub looping_sound_tag_path: String,
    /// `location` block index for the looping sound origin.
    pub looping_sound_location: i8,
    /// `bind scale to event` — block index for scale lifetime binding.
    pub looping_sound_bind_scale_to_event: i8,
    pub always_play_distance: f32,
    pub never_play_distance: f32,
    pub runtime_lightprobe_death_delay: f32,
    pub runtime_local_space_death_delay: f32,
    pub conical_distribution: Vec<EffectConicalDistribution>,
}

impl EffectDefinition {
    pub fn from_tag(tag: &TagFile) -> Result<Self, EffectError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != EFFECT_GROUP {
            return Err(EffectError::WrongGroup {
                expected: EFFECT_GROUP,
                actual,
            });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let flags = s.try_read_flags("flags").unwrap_or_default();
        let fixed_random_seed = s.read_int_any("fixed random seed").unwrap_or(0) as i32;
        // Schema name uses curly-brace alias: `restart if within{overlap threshold}:world units`.
        // The alias replaces the base name in the embedded layout.
        let restart_if_within = s.read_real("overlap threshold").unwrap_or(0.0);
        // Stripped from layout: `continue if within`, `death_delay`, `priority!*`,
        // `bind scale to event`. Engine defaults: 0.0, 0.0, Normal, -1.
        let continue_if_within: f32 = 0.0;
        let death_delay: f32 = 0.0;
        let priority = EffectPriority::Normal;
        let loop_start_event = s.read_int_any("loop start event").unwrap_or(-1) as i16;
        let runtime_danger_radius = s.read_real("runtime danger radius").unwrap_or(0.0);
        let locations = read_block(s, "locations", EffectLocation::from_struct);
        let events = read_block(s, "events", EffectEvent::from_struct);
        let looping_sound_tag_path =
            s.read_tag_ref_path("looping sound").unwrap_or_default();
        let looping_sound_location = s.read_int_any("location").unwrap_or(-1) as i8;
        let looping_sound_bind_scale_to_event: i8 = -1;
        let always_play_distance = s.read_real("always play distance").unwrap_or(0.0);
        let never_play_distance = s.read_real("never play distance").unwrap_or(0.0);
        // Schema names `runtime lightprobe_death_delay!` /
        // `runtime local_space_death_delay!` map to the H3 engine's
        // canonical `runtime death_delay` / `runtime last_instance_index`.
        let runtime_lightprobe_death_delay =
            s.read_real("runtime death_delay").unwrap_or(0.0);
        let runtime_local_space_death_delay = s
            .read_int_any("runtime last_instance_index")
            .unwrap_or(0) as f32;
        let conical_distribution = read_block(
            s,
            "conical distribution",
            EffectConicalDistribution::from_struct,
        );
        Self {
            flags,
            fixed_random_seed,
            restart_if_within,
            continue_if_within,
            death_delay,
            priority,
            loop_start_event,
            runtime_danger_radius,
            locations,
            events,
            looping_sound_tag_path,
            looping_sound_location,
            looping_sound_bind_scale_to_event,
            always_play_distance,
            never_play_distance,
            runtime_lightprobe_death_delay,
            runtime_local_space_death_delay,
            conical_distribution,
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk a named block field and map each element through `f`. Returns
/// an empty `Vec` when the field is missing or not a block.
fn read_block<T>(
    s: &TagStruct<'_>,
    name: &str,
    f: impl Fn(&TagStruct<'_>) -> T,
) -> Vec<T> {
    s.field(name)
        .and_then(|field| field.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(entry) = b.element(i) {
                    out.push(f(&entry));
                }
            }
            out
        })
        .unwrap_or_default()
}
