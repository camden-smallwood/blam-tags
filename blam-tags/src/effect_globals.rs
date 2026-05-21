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

/// `effect_holdback_type_enum` — 28 entries. Slot index matches the
/// runtime enum referenced in `effect_allocate` and the per-subsystem
/// `*_create` paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EffectHoldbackType {
    Effect = 0,
    Event = 1,
    Location = 2,
    Lightprobe = 3,
    EffectMessage = 4,
    BeamSystem = 5,
    BeamLocation = 6,
    Beam = 7,
    BeamProfileRow = 8,
    ContrailSystem = 9,
    ContrailLocation = 10,
    Contrail = 11,
    ContrailProfileRow = 12,
    DecalSystem = 13,
    Decal = 14,
    DecalVertex = 15,
    DecalIndex = 16,
    LightVolumeSystem = 17,
    LightVolumeLocation = 18,
    LightVolume = 19,
    LightVolumeProfileRow = 20,
    ParticleSystem = 21,
    ParticleLocation = 22,
    ParticleEmitter = 23,
    CpuParticle = 24,
    GpuParticleRow = 25,
    ContrailQueue = 26,
    ParticleQueue = 27,
}

impl EffectHoldbackType {
    pub fn from_index(i: i64) -> Option<Self> {
        Some(match i {
            0 => Self::Effect,
            1 => Self::Event,
            2 => Self::Location,
            3 => Self::Lightprobe,
            4 => Self::EffectMessage,
            5 => Self::BeamSystem,
            6 => Self::BeamLocation,
            7 => Self::Beam,
            8 => Self::BeamProfileRow,
            9 => Self::ContrailSystem,
            10 => Self::ContrailLocation,
            11 => Self::Contrail,
            12 => Self::ContrailProfileRow,
            13 => Self::DecalSystem,
            14 => Self::Decal,
            15 => Self::DecalVertex,
            16 => Self::DecalIndex,
            17 => Self::LightVolumeSystem,
            18 => Self::LightVolumeLocation,
            19 => Self::LightVolume,
            20 => Self::LightVolumeProfileRow,
            21 => Self::ParticleSystem,
            22 => Self::ParticleLocation,
            23 => Self::ParticleEmitter,
            24 => Self::CpuParticle,
            25 => Self::GpuParticleRow,
            26 => Self::ContrailQueue,
            27 => Self::ParticleQueue,
            _ => return None,
        })
    }
}

/// `global_effect_priority_enum` — 3 entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum EffectPriority {
    Normal = 0,
    High = 1,
    Essential = 2,
}

impl EffectPriority {
    pub fn from_index(i: i64) -> Option<Self> {
        Some(match i {
            0 => Self::Normal,
            1 => Self::High,
            2 => Self::Essential,
            _ => return None,
        })
    }
}

/// One `effect_component_holdback_block` entry — per-priority budget
/// for a given component type.
#[derive(Debug, Clone, Default)]
pub struct EffectPriorityHoldback {
    /// Authored priority slot (`global_effect_priority_enum`). `None`
    /// when the index is out of range — should not happen in practice.
    pub priority: Option<EffectPriority>,
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
    /// Authored holdback type (`effect_holdback_type_enum`). `None`
    /// when the index is out of range.
    pub holdback_type: Option<EffectHoldbackType>,
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
        self.holdbacks.iter().find(|h| h.holdback_type == Some(ty))
    }
}

impl EffectHoldback {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let holdback_type = s
            .read_int_any("holdback type")
            .and_then(|i| EffectHoldbackType::from_index(i as i64));
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
        Self { holdback_type, overall_budget, priorities }
    }

    /// Available count at the given priority — the runtime budget gate
    /// consulted by `effect_allocate`. Returns `0` if the priority is
    /// not authored for this holdback type.
    pub fn available(&self, priority: EffectPriority) -> i32 {
        self.priorities
            .iter()
            .find(|p| p.priority == Some(priority))
            .map(|p| p.available)
            .unwrap_or(0)
    }
}

impl EffectPriorityHoldback {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let priority = s
            .read_int_any("priority type")
            .and_then(|i| EffectPriority::from_index(i as i64));
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
