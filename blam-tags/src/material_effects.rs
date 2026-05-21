//! `material_effects` (`foot`) tag walker — surface-material → effect
//! lookup tables consumed by `material_effect_find_effects @
//! 0x1804E5900` and `material_effect_create_effects @ 0x1804E5DC0`
//! (and the sound siblings). Each impact-type tag (footstep, melee,
//! collision, etc.) carries a list of material entries; the engine
//! picks the row matching the global_material_type at the hit point.
//!
//! ## Schema shape
//!
//! - Root: `effects[]` block — one entry per effect *kind* (e.g. one
//!   `material_effects` tag covers all collision-small effects).
//! - Each effect entry has THREE sub-blocks:
//!   - `old materials (DO NOT USE)!` — deprecated H1/H2 schema kept
//!     for tool.exe back-compat; modern tags leave it empty.
//!   - `sounds[]` — modern sound rows.
//!   - `effects[]` — modern effect rows.
//! - Each row carries: `tag` (effect or sound — runtime-dispatched
//!   by tag group), `secondary tag` (fallback), `material name`
//!   (string_id), `runtime material index!`, `sweetener mode`.
//!
//! Tag group `foot` is reused for both footstep AND general impact
//! lookups; the consumer's `e_effect_type` selects which row applies.
//!
//! Schema: `definitions/halo3_mcc/material_effects.json`.

use crate::api::TagStruct;
use crate::file::TagFile;

const FOOT_GROUP: [u8; 4] = *b"foot";

#[derive(Debug)]
pub enum MaterialEffectsError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for MaterialEffectsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "material_effects: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for MaterialEffectsError {}

/// One material row in the new schema. The `tag` and `secondary_tag`
/// fields can point to either a sound or an effect; runtime dispatches
/// by tag group at lookup time.
#[derive(Debug, Clone, Default)]
pub struct MaterialEntry {
    /// Primary tag — `(group_4cc, path)`. Group is `effe`, `snd!`, or
    /// similar. None when slot is empty.
    pub primary: Option<(u32, String)>,
    /// Optional secondary tag (fallback / pairing).
    pub secondary: Option<(u32, String)>,
    /// Authored `material name^` (string_id). Empty when the row is
    /// a "default" catch-all.
    pub material_name: String,
    /// `runtime material index!` — tool.exe-resolved per-tag index
    /// into `c_global_material_type` (the cache-wide material table).
    pub runtime_material_index: i16,
    /// `sweetener mode` (char_enum) — biped foot kind / pad size
    /// modifier consumed by impact-effect dispatch.
    pub sweetener_mode: i8,
}

/// Legacy H1/H2 row format kept for tool.exe back-compat. Modern
/// tags leave the parent block empty.
#[derive(Debug, Clone, Default)]
pub struct OldMaterialEntry {
    pub effect: Option<String>,
    pub sound: Option<String>,
    pub material_name: String,
    pub runtime_material_index: i16,
    pub sweetener_mode: i8,
}

/// One `effects[]` entry — covers one impact-type / size-class block.
#[derive(Debug, Clone, Default)]
pub struct MaterialEffectBlock {
    pub old_materials: Vec<OldMaterialEntry>,
    pub sounds: Vec<MaterialEntry>,
    pub effects: Vec<MaterialEntry>,
}

/// Walked `material_effects` tag.
#[derive(Debug, Clone, Default)]
pub struct MaterialEffects {
    pub effects: Vec<MaterialEffectBlock>,
}

impl MaterialEffects {
    pub fn from_tag(tag: &TagFile) -> Result<Self, MaterialEffectsError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != FOOT_GROUP {
            return Err(MaterialEffectsError::WrongGroup { expected: FOOT_GROUP, actual });
        }
        Ok(Self::from_struct(&tag.root()))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        let effects = s
            .field("effects")
            .and_then(|f| f.as_block())
            .map(|b| {
                let mut out = Vec::with_capacity(b.len());
                for i in 0..b.len() {
                    if let Some(elem) = b.element(i) {
                        out.push(MaterialEffectBlock::from_struct(&elem));
                    }
                }
                out
            })
            .unwrap_or_default();
        Self { effects }
    }
}

impl MaterialEffectBlock {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            old_materials: read_block(s, "old materials (DO NOT USE)", OldMaterialEntry::from_struct),
            sounds: read_block(s, "sounds", MaterialEntry::from_struct),
            effects: read_block(s, "effects", MaterialEntry::from_struct),
        }
    }
}

impl MaterialEntry {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            primary: s.read_tag_ref_with_group("tag (effect or sound)"),
            secondary: s.read_tag_ref_with_group("secondary tag (effect or sound)"),
            material_name: s.read_string_id("material name").unwrap_or_default(),
            runtime_material_index: s.read_int_any("runtime material index").unwrap_or(0) as i16,
            sweetener_mode: s.read_int_any("sweetener mode").unwrap_or(0) as i8,
        }
    }
}

impl OldMaterialEntry {
    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            effect: s.read_tag_ref_path("effect"),
            sound: s.read_tag_ref_path("sound"),
            material_name: s.read_string_id("material name").unwrap_or_default(),
            runtime_material_index: s.read_int_any("runtime material index").unwrap_or(0) as i16,
            sweetener_mode: s.read_int_any("sweetener mode").unwrap_or(0) as i8,
        }
    }
}

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    let block = match s.field(name).and_then(|fld| fld.as_block()) {
        Some(b) => b,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}
