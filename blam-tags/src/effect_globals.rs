//! `effect_globals` (`effg`) tag walker — engine-wide budgets that
//! cap effect-system allocations per (component type, priority) pair.
//!
//! Loaded once via `cache_file_global_tags.effect_globals` and held in
//! the static `effect_globals` pointer consumed by `effect_allocate @
//! 0x1802FF440` (per-effect priority gate) and `effect_build_locations
//! @ 0x1803005F0` (per-location-row gate). When a component allocation
//! would exceed the resolved budget the engine drops the request.
//!
//! Authoring shape:
//!
//! - 28 holdback entries — one per `effect_holdback_type_enum` slot
//!   (effect / event / location / lightprobe / per-subsystem rows).
//! - Each holdback carries an `overall_budget` (absolute count from
//!   code) plus 3 `priority` entries (one per `global_effect_priority_enum`
//!   slot: normal / high / essential).
//! - Per priority: either an `absolute count` OR a `relative percentage`
//!   of the overall budget. The cache compiler resolves whichever is
//!   set into the `runtime available count` field consumed at runtime.
//!
//! Schema: `definitions/halo3_mcc/effect_globals.json`.

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::typed_enums::Enum;

const EFFG_GROUP: [u8; 4] = *b"effg";

#[derive(Debug)]
pub enum EffectGlobalsError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for EffectGlobalsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "effect_globals: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for EffectGlobalsError {}

/// `effect_holdback_type_enum` (long_enum) — 28 entries. Slot index
/// matches the runtime enum referenced in `effect_allocate` and the
/// per-subsystem `*_create` paths.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i32)]
pub enum EffectHoldbackType {
    #[default]
    #[strum(serialize = "type_effect")] Effect = 0,
    #[strum(serialize = "type_event")] Event = 1,
    #[strum(serialize = "type_location")] Location = 2,
    #[strum(serialize = "type_lightprobe")] Lightprobe = 3,
    #[strum(serialize = "type_effect_message")] EffectMessage = 4,
    #[strum(serialize = "type_beam_system")] BeamSystem = 5,
    #[strum(serialize = "type_beam_location")] BeamLocation = 6,
    #[strum(serialize = "type_beam")] Beam = 7,
    #[strum(serialize = "type_beam_profile_row")] BeamProfileRow = 8,
    #[strum(serialize = "type_contrail_system")] ContrailSystem = 9,
    #[strum(serialize = "type_contrail_location")] ContrailLocation = 10,
    #[strum(serialize = "type_contrail")] Contrail = 11,
    #[strum(serialize = "type_contrail_profile_row")] ContrailProfileRow = 12,
    #[strum(serialize = "type_decal_system")] DecalSystem = 13,
    #[strum(serialize = "type_decal")] Decal = 14,
    #[strum(serialize = "type_decal_vertex")] DecalVertex = 15,
    #[strum(serialize = "type_decal_index")] DecalIndex = 16,
    #[strum(serialize = "type_light_volume_system")] LightVolumeSystem = 17,
    #[strum(serialize = "type_light_volume_location")] LightVolumeLocation = 18,
    #[strum(serialize = "type_light_volume")] LightVolume = 19,
    #[strum(serialize = "type_light_volume_profile_row")] LightVolumeProfileRow = 20,
    #[strum(serialize = "type_particle_system")] ParticleSystem = 21,
    #[strum(serialize = "type_particle_location")] ParticleLocation = 22,
    #[strum(serialize = "type_particle_emitter")] ParticleEmitter = 23,
    #[strum(serialize = "type_cpu_particle")] CpuParticle = 24,
    #[strum(serialize = "type_gpu_particle_row")] GpuParticleRow = 25,
    #[strum(serialize = "type_contrail_queue")] ContrailQueue = 26,
    #[strum(serialize = "type_particle_queue")] ParticleQueue = 27,
}

/// `global_effect_priority_enum` (long_enum) — 3 entries.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i32)]
pub enum EffectPriority {
    #[default]
    #[strum(serialize = "normal")] Normal = 0,
    #[strum(serialize = "high")] High = 1,
    #[strum(serialize = "essential")] Essential = 2,
}

/// One `effect_component_holdback_block` entry — per-priority budget
/// for a given component type.
#[derive(Debug, Clone, Default)]
pub struct EffectPriorityHoldback {
    /// Authored priority slot (`global_effect_priority_enum`).
    pub priority: Enum<EffectPriority, i32>,
    /// `absolute count` — direct allocation cap. `0` means "use
    /// relative_percentage instead".
    pub absolute_count: i32,
    /// `relative percentage` (0..1 range as stored, NOT 0..100 despite
    /// the schema's `/ 100` annotation — `read_real` returns the raw
    /// fractional value).
    pub relative_percentage: f32,
    /// `How many available at this priority*!` — runtime-resolved
    /// count computed by tool.exe from absolute / relative inputs.
    /// Consumed at runtime by `effect_allocate`'s budget gate.
    pub available: i32,
}

/// One `effect_component_holdbacks_block` entry — overall budget for
/// a holdback type + 3 priority slots.
#[derive(Debug, Clone, Default)]
pub struct EffectHoldback {
    /// Authored holdback type (`effect_holdback_type_enum`).
    pub holdback_type: Enum<EffectHoldbackType, i32>,
    /// `overall budget*#from code` — engine-side cap visible to the
    /// authoring UI but ultimately driven by compile-time constants.
    pub overall_budget: i32,
    /// Authored priorities (3 entries — normal / high / essential).
    pub priorities: Vec<EffectPriorityHoldback>,
}

/// Walked `effect_globals` tag — holds all 28 holdback definitions.
#[derive(Debug, Clone, Default)]
pub struct EffectGlobals {
    /// `holdbacks` block — one entry per `EffectHoldbackType` (28).
    pub holdbacks: Vec<EffectHoldback>,
}

impl EffectGlobals {
    pub fn from_tag(tag: &TagFile) -> Result<Self, EffectGlobalsError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != EFFG_GROUP {
            return Err(EffectGlobalsError::WrongGroup { expected: EFFG_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let holdbacks = s
            .field("holdbacks")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(EffectHoldback::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();
        Self { holdbacks }
    }

    /// Look up the holdback row for a given component type.
    pub fn holdback(&self, ty: EffectHoldbackType) -> Option<&EffectHoldback> {
        self.holdbacks.iter().find(|h| h.holdback_type == ty)
    }
}

impl EffectHoldback {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let holdback_type = s.read_enum("holdback type");
        let overall_budget = s.read_int_any("overall budget").unwrap_or(0) as i32;
        let priorities = s
            .field("priorities")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(EffectPriorityHoldback::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();
        let mut out = Self { holdback_type, overall_budget, priorities };
        out.compute_budgets();
        out
    }

    /// Port of `s_effect_component_holdbacks::postprocess_block_proc`
    /// (reach `0x82e9f728`). The per-priority runtime budget
    /// (`s_holdback::m_budget`, our [`EffectPriorityHoldback::available`]) is a
    /// cache-baked value stored as 0 in authoring/loose tags, so we recompute it
    /// at parse time via the reservation cascade the engine runs at tag load:
    ///
    /// ```text
    /// consumed = 0
    /// for p in [essential, high, normal]:        // highest → lowest priority
    ///     available[p] = overall - min(consumed, overall)   // = max(overall - consumed, 0)
    ///     consumed    += max(absolute[p], relative_percentage[p]/100 * overall)
    /// ```
    ///
    /// `overall` is the authored `overall budget` — a UI mirror of the engine's
    /// compile-time per-type constant (`dword_8215C7E0[type]`); they match in
    /// shipped tags. A priority's budget is thus the overall minus what
    /// higher-priority items reserve ahead of it.
    fn compute_budgets(&mut self) {
        let overall = self.overall_budget.max(0) as f32;
        let mut consumed: f32 = 0.0;
        for pv in [EffectPriority::Essential, EffectPriority::High, EffectPriority::Normal] {
            if let Some(slot) = self.priorities.iter_mut().find(|p| p.priority == pv) {
                // available = overall - min(consumed, overall), floored at 0.
                slot.available = (overall - consumed.min(overall)).max(0.0) as i32;
                // This priority reserves max(absolute, relative% of overall).
                let relative = (slot.relative_percentage / 100.0) * overall;
                consumed += (slot.absolute_count as f32).max(relative);
            }
        }
    }

    /// Available count at the given priority — the runtime budget gate
    /// consulted by `effect_allocate`. Returns `0` if the priority is
    /// not authored for this holdback type.
    pub fn available(&self, priority: EffectPriority) -> i32 {
        self.priorities
            .iter()
            .find(|p| p.priority == priority)
            .map(|p| p.available)
            .unwrap_or(0)
    }
}

impl EffectPriorityHoldback {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let priority = s.read_enum("priority type");
        Self {
            priority,
            absolute_count: s.read_int_any("absolute count").unwrap_or(0) as i32,
            relative_percentage: s.read_real("relative percentage").unwrap_or(0.0),
            available: s
                .read_int_any("How many available at this priority")
                .unwrap_or(0) as i32,
        }
    }
}
