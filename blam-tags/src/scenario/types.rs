//! Scenario tag (`scnr`) types — author-time tag format.
//!
//! This is the rendering-relevant subset: BSPs, skies, zone sets,
//! placement palettes, scenery/biped/vehicle/etc. placements, decorators,
//! cubemaps. AI scripting / cinematics / multiplayer-engine fields are
//! deliberately omitted — they don't drive rendering.
//!
//! Field names mirror the **MCC tag schema** (with spaces, e.g.
//! `"structure bsps"`, `"object data"`), NOT the older Ares C++ names
//! (`structure_bsp_references`). Schemas drift between MCC builds, so
//! parsers read by name and tolerate missing fields.
//!
//! Reference: `Ares/source/scenario/scenario_definitions.h:195` for the
//! older shape; the latest MCC schema is the authority.
//!
//! ## Scope
//!
//! Today we capture:
//! - `Scenario` root: BSP refs, skies, zone sets, palettes, placements,
//!   decorators, cubemaps, lightmaps reference.
//! - One `ScenarioObjectPlacement` type covering all object types
//!   (scenery / biped / vehicle / weapon / equipment / crate / etc.) —
//!   they share the schema for `object data` + `permutation data`.
//!   Type-specific extensions (scenery_data, multiplayer_data) are on
//!   the placement struct as optional sub-data.
//!
//! Skipped (zero rendering value):
//! - AI squads / orders / triggers / pathfinding / character palettes
//! - HS scripts / globals / source files
//! - Cutscenes / cinematic camera points
//! - Trigger volumes / kill zones / safe zones
//! - Decals (TODO — visible but additive, not foundational)
//! - Player starting profiles / spawn data

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::math::{RealEulerAngles3d, RealPoint3d, RealQuaternion, RgbColor};

const SCNR_GROUP: [u8; 4] = *b"scnr";

#[derive(Debug)]
pub enum ScenarioError {
    WrongGroup { expected: [u8; 4], actual: [u8; 4] },
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { expected, actual } => write!(
                f,
                "scenario: wrong tag group (expected {:?}, got {:?})",
                std::str::from_utf8(expected).unwrap_or("????"),
                std::str::from_utf8(actual).unwrap_or("????"),
            ),
        }
    }
}

impl std::error::Error for ScenarioError {}

// =============================================================================
// Top-level scenario
// =============================================================================

/// Scenario tag (`scnr`) — root of a level's tag tree. Mirrors Ares
/// `struct scenario` with rendering-relevant fields only.
#[derive(Debug, Clone, Default)]
pub struct Scenario {
    /// `type` field — campaign / multiplayer / etc.
    pub scenario_type: i32,
    /// Map ID (engine identifier; matches `e_map_id` in Ares).
    pub map_id: i32,
    /// `local north` — radians, level orientation hint.
    pub local_north: f32,

    // ---- Geometry references ----
    pub structure_bsps: Vec<StructureBspReference>,
    pub structure_seams: String,
    pub skies: Vec<SkyReference>,

    // ---- Streaming / visibility ----
    pub zone_sets: Vec<ZoneSet>,

    // ---- Object placement ----
    /// 26 unique scenery types in riverworld for instance.
    pub scenery_palette: Vec<TagReferencePalette>,
    pub scenery: Vec<ObjectPlacement>,

    pub biped_palette: Vec<TagReferencePalette>,
    pub bipeds: Vec<ObjectPlacement>,

    pub vehicle_palette: Vec<TagReferencePalette>,
    pub vehicles: Vec<ObjectPlacement>,

    pub equipment_palette: Vec<TagReferencePalette>,
    pub equipment: Vec<ObjectPlacement>,

    pub weapon_palette: Vec<TagReferencePalette>,
    pub weapons: Vec<ObjectPlacement>,

    pub machine_palette: Vec<TagReferencePalette>,
    pub machines: Vec<ObjectPlacement>,

    pub control_palette: Vec<TagReferencePalette>,
    pub controls: Vec<ObjectPlacement>,

    pub sound_scenery_palette: Vec<TagReferencePalette>,
    pub sound_scenery: Vec<ObjectPlacement>,

    pub crate_palette: Vec<TagReferencePalette>,
    pub crates: Vec<ObjectPlacement>,

    pub light_palette: Vec<TagReferencePalette>,
    pub lights: Vec<ObjectPlacement>,

    // ---- Decorators (foliage) ----
    pub decorators: Vec<ScenarioDecoratorBlock>,

    // ---- Cubemaps + lightmaps ----
    pub cubemaps: Vec<CubemapEntry>,
    /// Lightmap tag reference (`new lightmaps` field). Empty when the
    /// scenario uses per-BSP lightmap_info instead.
    pub new_lightmaps: String,

    // ---- Atmosphere ----
    /// `atmospheric` tag reference — points at a `.sky_atm_parameters`
    /// tag (`skya` group) that holds Rayleigh/Mie multipliers, sun
    /// pitch/heading, fog colors, etc. Used by `compute_scattering`.
    /// Empty for scenarios with no atmosphere.
    pub atmospheric: String,

    /// `camera fx settings` tag reference — points at a
    /// `.camera_fx_settings` tag (`cfxs` group) that holds the level's
    /// exposure / bloom / tone curve. Halo's
    /// `c_player_view::setup_camera_fx_parameters @ 0x180689c20`
    /// reads `scenario.camera_effects.index` and feeds it through
    /// `c_camera_fx_values::get_render_exposure @ 0x18068e3e0` to
    /// produce the per-frame `view_exposure` (typically ~0.67).
    pub camera_fx_settings: String,

    /// `chocalate mountain` tag reference — the per-object-type
    /// minimum-luminance table (chmt). Cache-bake target field;
    /// engine reads `global_scenario->chocalate_mountain_settings`
    /// from here. Schema also exposes a `chocalate mountains` block
    /// of overrides — only the singular bake target is surfaced.
    pub chocolate_mountain: String,
}

impl Scenario {
    pub fn from_tag(tag: &TagFile) -> Result<Self, ScenarioError> {
        let actual = tag.group().tag.to_be_bytes();
        if actual != SCNR_GROUP {
            return Err(ScenarioError::WrongGroup { expected: SCNR_GROUP, actual });
        }
        let root = tag.root();
        Ok(Self::from_struct(&root))
    }

    pub fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            scenario_type: s.read_int_any("type").unwrap_or(0) as i32,
            map_id: s.read_int_any("map id").unwrap_or(0) as i32,
            local_north: s.read_real("local north").unwrap_or(0.0),

            structure_bsps: read_block(s, "structure bsps", StructureBspReference::from_struct),
            structure_seams: s.read_tag_ref_path("structure seams").unwrap_or_default(),
            skies: read_block(s, "skies", SkyReference::from_struct),

            zone_sets: read_block(s, "zone sets", ZoneSet::from_struct),

            scenery_palette: read_block(s, "scenery palette", TagReferencePalette::from_struct),
            scenery: read_block(s, "scenery", ObjectPlacement::from_struct),

            biped_palette: read_block(s, "biped palette", TagReferencePalette::from_struct),
            bipeds: read_block(s, "bipeds", ObjectPlacement::from_struct),

            vehicle_palette: read_block(s, "vehicle palette", TagReferencePalette::from_struct),
            vehicles: read_block(s, "vehicles", ObjectPlacement::from_struct),

            equipment_palette: read_block(s, "equipment palette", TagReferencePalette::from_struct),
            equipment: read_block(s, "equipment", ObjectPlacement::from_struct),

            weapon_palette: read_block(s, "weapon palette", TagReferencePalette::from_struct),
            weapons: read_block(s, "weapons", ObjectPlacement::from_struct),

            machine_palette: read_block(s, "machine palette", TagReferencePalette::from_struct),
            machines: read_block(s, "machines", ObjectPlacement::from_struct),

            control_palette: read_block(s, "control palette", TagReferencePalette::from_struct),
            controls: read_block(s, "controls", ObjectPlacement::from_struct),

            sound_scenery_palette: read_block(
                s,
                "sound scenery palette",
                TagReferencePalette::from_struct,
            ),
            sound_scenery: read_block(s, "sound scenery", ObjectPlacement::from_struct),

            crate_palette: read_block(s, "crate palette", TagReferencePalette::from_struct),
            crates: read_block(s, "crates", ObjectPlacement::from_struct),

            // Halo 3 schema names this "light volumes" / "light volumes palette"
            // (light_palette in Ares). Keep both names tried via the helper.
            light_palette: read_block_aliased(
                s,
                &["light volumes palette", "light palette", "lights palette"],
                TagReferencePalette::from_struct,
            ),
            lights: read_block_aliased(
                s,
                &["light volumes", "lights"],
                ObjectPlacement::from_struct,
            ),

            decorators: read_block(s, "decorators", ScenarioDecoratorBlock::from_struct),

            cubemaps: read_block(s, "cubemaps", CubemapEntry::from_struct),
            new_lightmaps: s.read_tag_ref_path("new lightmaps").unwrap_or_default(),
            atmospheric: s.read_tag_ref_path("atmospheric").unwrap_or_default(),
            camera_fx_settings: s
                .read_tag_ref_path("camera fx settings")
                .or_else(|| s.read_tag_ref_path("camera_fx_settings"))
                .or_else(|| s.read_tag_ref_path("camera effects"))
                .unwrap_or_default(),
            chocolate_mountain: s
                .read_tag_ref_path("chocalate mountain")
                .or_else(|| s.read_tag_ref_path("chocolate mountain"))
                .unwrap_or_default(),
        }
    }

    /// Convenience: which BSPs are activated by `zone_sets[i]` per the
    /// `bsp zone flags` mask. Returns indices into `self.structure_bsps`.
    pub fn zone_set_active_bsps(&self, zone_set_index: usize) -> Vec<usize> {
        let Some(zs) = self.zone_sets.get(zone_set_index) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..self.structure_bsps.len() {
            if (zs.bsp_zone_flags >> i) & 1 != 0 {
                out.push(i);
            }
        }
        out
    }
}

// =============================================================================
// Sub-blocks
// =============================================================================

/// `structure bsps[i]` — references a `.scenario_structure_bsp` tag and
/// its associated design / lighting info.
#[derive(Debug, Clone, Default)]
pub struct StructureBspReference {
    /// `.scenario_structure_bsp` tag path.
    pub structure_bsp: String,
    /// `.structure_design` tag path (optional designer overlay).
    pub structure_design: String,
    /// `.scenario_structure_lighting_info` tag path.
    pub structure_lighting_info: String,
    /// `default sky` block index — points into `Scenario::skies`.
    pub default_sky_index: i16,
    /// `cubemap bitmap group reference` — per-BSP cubemap atlas.
    pub cubemap_bitmap_group: String,
    /// `wind` tag reference.
    pub wind: String,
    /// 16-bit flags (e.g. `lightmaps reduce stretch hack`).
    pub flags: u16,
}

impl StructureBspReference {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            structure_bsp: s.read_tag_ref_path("structure bsp").unwrap_or_default(),
            structure_design: s.read_tag_ref_path("structure design").unwrap_or_default(),
            structure_lighting_info: s
                .read_tag_ref_path("structure lighting_info")
                .or_else(|| s.read_tag_ref_path("structure lighting info"))
                .unwrap_or_default(),
            default_sky_index: s.read_block_index("default sky"),
            cubemap_bitmap_group: s.read_tag_ref_path("cubemap bitmap group reference").unwrap_or_default(),
            wind: s.read_tag_ref_path("wind").unwrap_or_default(),
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
        }
    }
}

/// `skies[i]` — sky scenery reference + which BSPs it's active on.
#[derive(Debug, Clone, Default)]
pub struct SkyReference {
    /// `.scenery` tag path for the sky model.
    pub sky: String,
    /// `name` block index into `object_names` (most levels: NONE).
    pub name_index: i16,
    /// `active on bsps` — bitmask of BSPs in the scenario for which this
    /// sky is active. Each bit = one BSP in `Scenario::structure_bsps`.
    pub active_on_bsp_flags: u16,
}

impl SkyReference {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sky: s.read_tag_ref_path("sky").unwrap_or_default(),
            name_index: s.read_block_index("name"),
            active_on_bsp_flags: s.read_int_any("active on bsps").unwrap_or(0) as u16,
        }
    }
}

/// `zone sets[i]` — declares which BSPs / designer zones are active when
/// this zone set is the current one. Halo 3 streams BSPs by zone set.
#[derive(Debug, Clone, Default)]
pub struct ZoneSet {
    /// `name` (string_id).
    pub name: String,
    /// `pvs index` — into `Scenario::zone_set_pvs` (visibility).
    pub pvs_index: i32,
    /// `flags` — zone set flags.
    pub flags: u32,
    /// `bsp zone flags` — bitmask of which BSPs are active.
    pub bsp_zone_flags: u32,
    /// `required designer zones` — designer-zone bitmask required.
    pub required_designer_zone_flags: u32,
    /// `forbidden designer zones` — designer-zone bitmask forbidden.
    pub forbidden_designer_zone_flags: u32,
    /// `cinematic zones` — cinematic-only zones.
    pub cinematic_zone_flags: u32,
    /// `audibility index` — into `Scenario::zone_set_audibility`.
    pub audibility_index: i32,
}

impl ZoneSet {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            pvs_index: s.read_int_any("pvs index").unwrap_or(-1) as i32,
            flags: s.read_int_any("flags").unwrap_or(0) as u32,
            bsp_zone_flags: s.read_int_any("bsp zone flags").unwrap_or(0) as u32,
            required_designer_zone_flags: s
                .read_int_any("required designer zones")
                .unwrap_or(0) as u32,
            forbidden_designer_zone_flags: s
                .read_int_any("forbidden designer zones")
                .unwrap_or(0) as u32,
            cinematic_zone_flags: s.read_int_any("cinematic zones").unwrap_or(0) as u32,
            audibility_index: s.read_int_any("audibility index").unwrap_or(-1) as i32,
        }
    }
}

/// Common shape for every "X palette" block in the scenario — a list of
/// tag references to .obje children (scenery, biped, vehicle, ...).
#[derive(Debug, Clone, Default)]
pub struct TagReferencePalette {
    /// `name` field — the tag reference path (or empty if NONE).
    pub tag_path: String,
}

impl TagReferencePalette {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self { tag_path: s.read_tag_ref_path("name").unwrap_or_default() }
    }
}

// =============================================================================
// Object placement
// =============================================================================

/// One placed object in a scenario — covers scenery, biped, vehicle,
/// weapon, equipment, machine, control, sound_scenery, crate, light.
/// Per-type extensions (scenery_data, multiplayer_data, etc.) are
/// captured as flat fields where possible.
#[derive(Debug, Clone, Default)]
pub struct ObjectPlacement {
    /// `type` — index into the matching palette (e.g. scenery_palette).
    /// -1 if NONE (the placement is invalid).
    pub palette_index: i16,
    /// `name` — index into `object_names` (level-wide name table).
    pub name_index: i16,
    /// Common `object data` sub-struct.
    pub object_data: PlacementObjectData,
    /// Common `permutation data` sub-struct.
    pub permutation_data: PlacementPermutationData,
    /// Optional `multiplayer data` sub-struct (multiplayer maps use
    /// this to drive game-mode visibility).
    pub multiplayer_data: Option<PlacementMultiplayerData>,
}

impl ObjectPlacement {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            palette_index: s.read_block_index("type"),
            name_index: s.read_block_index("name"),
            object_data: s
                .field("object data")
                .and_then(|f| f.as_struct())
                .map(|st| PlacementObjectData::from_struct(&st))
                .unwrap_or_default(),
            permutation_data: s
                .field("permutation data")
                .and_then(|f| f.as_struct())
                .map(|st| PlacementPermutationData::from_struct(&st))
                .unwrap_or_default(),
            multiplayer_data: s
                .field("multiplayer data")
                .and_then(|f| f.as_struct())
                .map(|st| PlacementMultiplayerData::from_struct(&st)),
        }
    }
}

/// Common `object data` sub-struct on every placement.
#[derive(Debug, Clone, Default)]
pub struct PlacementObjectData {
    pub placement_flags: u32,
    pub position: RealPoint3d,
    pub rotation: RealEulerAngles3d,
    /// `scale` — `0.0` means "use object's default scale" per
    /// runtime convention.
    pub scale: f32,
    pub transform_flags: u16,
    pub manual_bsp_flags: u16,
    pub light_airprobe_name: String,
    pub bsp_policy: i32,
    pub editor_folder_index: i16,
    pub can_attach_to_bsp_flags: u16,
}

impl PlacementObjectData {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            placement_flags: s.read_int_any("placement flags").unwrap_or(0) as u32,
            position: s.read_point3d("position"),
            rotation: read_euler3d(s, "rotation"),
            scale: s.read_real("scale").unwrap_or(0.0),
            transform_flags: s.read_int_any("transform flags").unwrap_or(0) as u16,
            manual_bsp_flags: s.read_int_any("manual bsp flags").unwrap_or(0) as u16,
            light_airprobe_name: s.read_string_id("light airprobe name").unwrap_or_default(),
            bsp_policy: s.read_int_any("bsp policy").unwrap_or(0) as i32,
            editor_folder_index: s.read_block_index("editor folder"),
            can_attach_to_bsp_flags: s.read_int_any("can attach to bsp flags").unwrap_or(0) as u16,
        }
    }
}

/// `permutation data` — variant + change-color overrides per placement.
#[derive(Debug, Clone, Default)]
pub struct PlacementPermutationData {
    pub variant_name: String,
    pub active_change_colors: u32,
    pub primary_color: RgbColor,
    pub secondary_color: RgbColor,
    pub tertiary_color: RgbColor,
    pub quaternary_color: RgbColor,
}

impl PlacementPermutationData {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            variant_name: s.read_string_id("variant name").unwrap_or_default(),
            active_change_colors: s.read_int_any("active change colors").unwrap_or(0) as u32,
            primary_color: read_rgb_color(s, "primary color"),
            secondary_color: read_rgb_color(s, "secondary color"),
            tertiary_color: read_rgb_color(s, "tertiary color"),
            quaternary_color: read_rgb_color(s, "quaternary color"),
        }
    }
}

/// `multiplayer data` — game-engine flags for placements that only
/// appear in certain game modes.
#[derive(Debug, Clone, Default)]
pub struct PlacementMultiplayerData {
    pub symmetric_placement: i32,
    pub game_engine_flags: u16,
    pub owner_team: i16,
    pub spawn_order: i8,
    pub quota_minimum: i8,
}

impl PlacementMultiplayerData {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            symmetric_placement: s
                .read_int_any("game engine symmetric placement")
                .unwrap_or(0) as i32,
            game_engine_flags: s.read_int_any("game engine flags").unwrap_or(0) as u16,
            owner_team: s.read_int_any("owner team").unwrap_or(0) as i16,
            spawn_order: s.read_int_any("spawn order").unwrap_or(0) as i8,
            quota_minimum: s.read_int_any("quota minimum").unwrap_or(0) as i8,
        }
    }
}

// =============================================================================
// Decorators
// =============================================================================
//
// Schema (per `definitions/halo3_mcc/scenario_decorators_resource.json` and
// confirmed via `blam-tag-shell --game halo3_mcc inspect --full
// <scenario>`):
//
//   decorators [block, top-level on scenario]
//   └─ each entry = ScenarioDecoratorBlock
//      ├─ brush [struct]            (editor settings — ignored at runtime)
//      ├─ decorator count* [long]   (computed total across all sets)
//      ├─ current bsp count* [long]
//      ├─ global offset/x/y/z [real_vector_3d × 4]
//      ├─ palette [block of DecoratorPalette]
//      │  └─ name + 8× (decorator_set_block_index, weight)
//      └─ sets [block of DecoratorSetEntry]
//         └─ decorator_set tag ref + placements [block of ScenarioDecoratorPlacement]
//
// Riverworld carries 11 sets, ~26K placements (thistle 17K, wildgrass 5.5K,
// etc). MCC tag-ships the AUTHORING data but NOT the runtime per-cluster
// arrays in the sbsp's `decorator sets` (which is `[0 elements]` on every
// MCC sbsp). Runtime cluster assignment must be re-computed at load time —
// each placement carries `runtime_bsp_index = -1`, `cluster_index = -1`.
// See `reference_mcc_strips_decorator_data.md` for the data-availability
// audit; the WAY foliage data lives is in this scenario block, not in the
// sbsp.

/// Top-level `decorators[i]` entry on a scenario. Holds the editor brush
/// + per-palette weights + the actual per-set placement arrays.
#[derive(Debug, Clone, Default)]
pub struct ScenarioDecoratorBlock {
    /// `decorator count` (computed total of all set placement counts).
    pub decorator_count: i32,
    /// `current bsp count`.
    pub current_bsp_count: i32,
    /// `palette` block — named groupings of decorator sets with weights.
    pub palettes: Vec<DecoratorPalette>,
    /// `sets` block — actual decorator_set tag refs + placement arrays.
    pub sets: Vec<DecoratorSetEntry>,
}

impl ScenarioDecoratorBlock {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            decorator_count: s.read_int_any("decorator count").unwrap_or(0) as i32,
            current_bsp_count: s.read_int_any("current bsp count").unwrap_or(0) as i32,
            palettes: read_block(s, "palette", DecoratorPalette::from_struct),
            sets: read_block(s, "sets", DecoratorSetEntry::from_struct),
        }
    }
}

/// One entry in `palette` — a named collection (e.g. "ferns",
/// "grass_cover", "wetlands") of up to 8 decorator-set indices with
/// authoring weights. The runtime distributes painted placements by
/// weight; we just preserve the data.
#[derive(Debug, Clone, Default)]
pub struct DecoratorPalette {
    pub name: String,
    /// 8 × (set_block_index, weight). Block indices point into the
    /// containing `ScenarioDecoratorBlock::sets`; -1 = unused slot.
    pub set_indices: [i16; 8],
    pub set_weights: [i16; 8],
}

impl DecoratorPalette {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let mut set_indices = [-1i16; 8];
        let mut set_weights = [0i16; 8];
        for i in 0..8 {
            set_indices[i] = s.read_block_index(&format!("decorator set {i}"));
            set_weights[i] = s
                .read_int_any(&format!("decorator weight {i}"))
                .unwrap_or(0) as i16;
        }
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            set_indices,
            set_weights,
        }
    }
}

/// One entry in `sets` — a decorator_set tag ref + the array of
/// authored placements that reference it.
#[derive(Debug, Clone, Default)]
pub struct DecoratorSetEntry {
    /// Tag reference path to the `.decorator_set` tag.
    pub decorator_set: String,
    pub placements: Vec<ScenarioDecoratorPlacement>,
}

impl DecoratorSetEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            decorator_set: s.read_tag_ref_path("decorator set").unwrap_or_default(),
            placements: read_block(s, "placements", ScenarioDecoratorPlacement::from_struct),
        }
    }
}

/// One painted decorator placement. Position + orientation + per-instance
/// tint + scale + the decorator-internal type index. Runtime cluster
/// assignment fields (`runtime_bsp_index`, `cluster_index`,
/// `cluster_decorator_set_index`) are all `-1` in MCC-shipped tags —
/// the cache-builder normally fills them; we have to recompute at load.
#[derive(Debug, Clone, Default)]
pub struct ScenarioDecoratorPlacement {
    pub position: RealPoint3d,
    /// Index into the parent decorator_set's `decorator_types` array
    /// (which mesh subpart this placement instances).
    pub type_index: i8,
    /// Wind-sway intensity scalar (0-255 → 0.0-1.0 in shader).
    pub motion_scale: i8,
    /// Per-instance ground-tint blend factor.
    pub ground_tint: i8,
    pub flags: u8,
    pub rotation: RealQuaternion,
    pub scale: f32,
    pub tint_color: RealPoint3d,
    pub original_point: RealPoint3d,
    pub original_normal: RealPoint3d,
    /// Block index into the BSP-level decorator structures
    /// (editor_bound_to_bsp). -1 = unbound at authoring time.
    pub editor_bound_to_bsp: i8,
    /// `runtime_bsp_index` — -1 in MCC-shipped tags. The cache-builder
    /// fills this with the BSP this placement falls inside; we
    /// recompute at load.
    pub runtime_bsp_index: i8,
    /// `cluster_index` — -1 in MCC. Fills via point-in-cluster test.
    pub cluster_index: i16,
    /// `cluster_decorator_set_index` — -1 in MCC. Index into the
    /// per-cluster runtime decorator-set table.
    pub cluster_decorator_set_index: i16,
    /// Compressed grid-aligned block coordinates for the runtime LOD
    /// chunking (-1 if not yet computed).
    pub block_x: i8,
    pub block_y: i8,
}

impl ScenarioDecoratorPlacement {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            position: s.read_point3d("position"),
            type_index: s.read_int_any("type index").unwrap_or(0) as i8,
            motion_scale: s.read_int_any("motion scale").unwrap_or(0) as i8,
            ground_tint: s.read_int_any("ground tint").unwrap_or(0) as i8,
            flags: s.read_int_any("flags").unwrap_or(0) as u8,
            rotation: read_quaternion(s, "rotation"),
            scale: s.read_real("scale").unwrap_or(0.0),
            tint_color: s.read_point3d("tint color"),
            original_point: s.read_point3d("original point"),
            original_normal: s.read_point3d("original normal"),
            editor_bound_to_bsp: s.read_block_index("editor bound to bsp") as i8,
            runtime_bsp_index: s.read_int_any("runtime bsp index").unwrap_or(-1) as i8,
            cluster_index: s.read_int_any("cluster index").unwrap_or(-1) as i16,
            cluster_decorator_set_index: s
                .read_int_any("cluster decorator set index")
                .unwrap_or(-1) as i16,
            block_x: s.read_int_any("block x").unwrap_or(0) as i8,
            block_y: s.read_int_any("block y").unwrap_or(0) as i8,
        }
    }
}

// =============================================================================
// Cubemaps
// =============================================================================

/// One entry in scenario `cubemaps` — global cubemap palette. Per-cluster
/// references come from `structure_bsp.clusters[].cubemaps[]`.
#[derive(Debug, Clone, Default)]
pub struct CubemapEntry {
    /// `cubemap` tag reference (`.bitmap` of cubemap type).
    pub cubemap_bitmap: String,
}

impl CubemapEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cubemap_bitmap: s.read_tag_ref_path("cubemap").unwrap_or_default(),
        }
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn read_block<T, F>(s: &TagStruct<'_>, name: &str, mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    s.field(name)
        .and_then(|fld| fld.as_block())
        .map(|b| read_block_vec(&b, &mut f))
        .unwrap_or_default()
}

fn read_block_aliased<T, F>(s: &TagStruct<'_>, names: &[&str], mut f: F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    for name in names {
        if let Some(b) = s.field(name).and_then(|fld| fld.as_block()) {
            return read_block_vec(&b, &mut f);
        }
    }
    Vec::new()
}

fn read_block_vec<T, F>(block: &TagBlock<'_>, f: &mut F) -> Vec<T>
where
    F: FnMut(&TagStruct<'_>) -> T,
{
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        if let Some(elem) = block.element(i) {
            out.push(f(&elem));
        }
    }
    out
}

fn read_euler3d(s: &TagStruct<'_>, name: &str) -> RealEulerAngles3d {
    use crate::fields::TagFieldData;
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealEulerAngles3d(a)) => a,
        _ => RealEulerAngles3d::default(),
    }
}

fn read_quaternion(s: &TagStruct<'_>, name: &str) -> RealQuaternion {
    use crate::fields::TagFieldData;
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RealQuaternion(q)) => q,
        _ => RealQuaternion::default(),
    }
}

fn read_rgb_color(s: &TagStruct<'_>, name: &str) -> RgbColor {
    use crate::fields::TagFieldData;
    match s.field(name).and_then(|f| f.value()) {
        Some(TagFieldData::RgbColor(c)) => c,
        _ => RgbColor::default(),
    }
}
