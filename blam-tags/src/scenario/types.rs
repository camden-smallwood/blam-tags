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
//! - Player starting profiles / spawn data

use crate::api::{TagBlock, TagStruct};
use crate::file::TagFile;
use crate::light::LightType;
use crate::math::{RealEulerAngles3d, RealPoint3d, RealQuaternion, RgbColor};
use crate::typed_enums::{Enum, Flags};

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

    /// `object names` — the level-wide named-object table. Object
    /// placements reference a parent by NAME index
    /// ([`ParentId::parent_object_name_index`]), and each sky's
    /// [`SkyReference::name_index`] names its runtime sky object. The
    /// engine maps name index → runtime object via
    /// `object_set_object_index_for_name_index` /
    /// `object_index_from_name_index`; consumers resolve a placement's
    /// parent by matching `parent_object_name_index` against this table.
    pub object_names: Vec<ObjectName>,

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

    /// `effect scenery` — `csfe` objects that ARE a continuously-playing
    /// effect (the lift columns / man cannon visuals on guardian etc.). The
    /// object carries the effect via an `effe` attachment, like scenery/crate.
    pub effect_scenery_palette: Vec<TagReferencePalette>,
    pub effect_scenery: Vec<ObjectPlacement>,

    pub light_palette: Vec<TagReferencePalette>,
    pub lights: Vec<ObjectPlacement>,
    /// The `light data` (`s_scenario_light_datum`) sub-struct per `lights[]`
    /// entry — the light-specific placement fields (lightmap type, target
    /// point, scenario geometry override) the generic [`ObjectPlacement`] does
    /// not carry. Index-parallel to [`Self::lights`].
    pub light_data: Vec<ScenarioLightDatum>,

    // ---- Decorators (foliage) ----
    pub decorators: Vec<ScenarioDecoratorBlock>,

    // ---- Decals ----
    /// `decal palette` — list of `.decal_system` tag refs. Each
    /// placement in `decals[]` cites one entry by index.
    pub decal_palette: Vec<DecalPaletteEntry>,
    /// `decals` — author-placed decal projections onto BSP surfaces.
    /// Cyberdyne (Epitaph) has 252 placements / 46 palette entries.
    pub decals: Vec<DecalPlacement>,

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

    /// `global screen effect` tag reference — points at a
    /// `.area_screen_effect` (`sefc` group) that the engine's per-frame
    /// `screen_effect_sample @ 0x1803A4E90` accumulates at falloff = 1.0
    /// (no falloff curves apply, per the schema annotation "ignores
    /// the falloff curves"). Empty when unauthored.
    pub global_screen_effect: String,

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

    /// `zone set pvs*!` (offset 0x3C in scenario tag) — one entry per
    /// scenario zone set, holding the precomputed PVS bit vectors and
    /// portal-device mappings. Indexed by `ZoneSet::pvs_index`. Engine:
    /// `s_scenario_zone_set_pvs` (44B per entry) walked by
    /// `scenario_zone_set_pvs_get_row @ 0x180333630` and
    /// `structure_bsp_compute_cluster_active_pvs @ 0x180334F80`.
    pub zone_set_pvs: Vec<ZoneSetPvs>,
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
            object_names: read_block(s, "object names", ObjectName::from_struct),

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

            effect_scenery_palette: read_block(
                s,
                "effect scenery palette",
                TagReferencePalette::from_struct,
            ),
            effect_scenery: read_block(s, "effect scenery", ObjectPlacement::from_struct),

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
            light_data: read_block_aliased(
                s,
                &["light volumes", "lights"],
                ScenarioLightDatum::from_light_block,
            ),

            decorators: read_block(s, "decorators", ScenarioDecoratorBlock::from_struct),

            decal_palette: read_block(s, "decal palette", DecalPaletteEntry::from_struct),
            decals: read_block(s, "decals", DecalPlacement::from_struct),

            cubemaps: read_block(s, "cubemaps", CubemapEntry::from_struct),
            new_lightmaps: s.read_tag_ref_path("new lightmaps").unwrap_or_default(),
            atmospheric: s.read_tag_ref_path("atmospheric").unwrap_or_default(),
            global_screen_effect: s
                .read_tag_ref_path("global screen effect")
                .or_else(|| s.read_tag_ref_path("global_screen_effect"))
                .unwrap_or_default(),
            camera_fx_settings: s
                .read_tag_ref_path("camera fx settings")
                .or_else(|| s.read_tag_ref_path("camera_fx_settings"))
                .or_else(|| s.read_tag_ref_path("camera effects"))
                .unwrap_or_default(),
            chocolate_mountain: s
                .read_tag_ref_path("chocalate mountain")
                .or_else(|| s.read_tag_ref_path("chocolate mountain"))
                .unwrap_or_default(),

            zone_set_pvs: read_block(s, "zone set pvs", ZoneSetPvs::from_struct),
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

/// `object names[i]` — one entry in the level-wide named-object table
/// ([`Scenario::object_names`]). `object_type` + `scenario_datum_index`
/// point at the placed object that owns this name when one exists; both
/// are `-1` for runtime-created named objects (e.g. the sky, which the
/// engine spawns via `create_sky_object @0x1803ABC30` and registers under
/// its [`SkyReference::name_index`]).
#[derive(Debug, Clone, Default)]
pub struct ObjectName {
    /// `name` — the 32-byte name string (e.g. `"skyzor"`).
    pub name: String,
    /// `object_type` — object-type block index of the owning placement, or `-1`.
    pub object_type: i16,
    /// `scenario_datum_index` — index of the owning placement within its
    /// type block, or `-1` (runtime-created objects like the sky).
    pub scenario_datum_index: i16,
}

impl ObjectName {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string("name").unwrap_or_default(),
            object_type: s.read_block_index("object_type"),
            scenario_datum_index: s.read_block_index("scenario_datum_index"),
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
    /// `name` field's group FOURCC (e.g. `mach` for a machine palette entry).
    /// 0 when the reference is NONE. Resolve the on-disk extension via
    /// [`crate::paths::group_tag_to_extension`] rather than guessing from the
    /// object-type label (a machine's tag is `.device_machine`, not `.machine`).
    pub group_tag: u32,
}

impl TagReferencePalette {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        match s.read_tag_ref_with_group("name") {
            Some((group_tag, tag_path)) => Self { tag_path, group_tag },
            None => Self { tag_path: String::new(), group_tag: 0 },
        }
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

/// `scenario_light_lightmap_type_enum_definition` — how a scenario light
/// participates in lighting. `LightmapsOnly` lights are baked into the lightmap
/// and are NOT created as runtime dynamic lights.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum ScenarioLightLightmapType {
    #[default]
    #[strum(serialize = "use light tag setting")] UseLightTagSetting = 0,
    #[strum(serialize = "dynamic only")] DynamicOnly = 1,
    #[strum(serialize = "dynamic with lightmaps")] DynamicWithLightmaps = 2,
    #[strum(serialize = "lightmaps only")] LightmapsOnly = 3,
}

/// `scenario_light_flags_definition` (`word_flags`).
#[derive(Clone, Copy, PartialEq, Eq, Debug,
         num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum ScenarioLightFlags {
    #[strum(serialize = "custom geometry")] CustomGeometry = 0,
    #[strum(serialize = "unused")] Unused = 1,
    #[strum(serialize = "cinematic_only")] CinematicOnly = 2,
}

/// The `light data` (`s_scenario_light_datum`, 48 B) sub-struct of a scenario
/// `lights[]` placement. Carries the runtime-vs-lightmap policy, the cone aim
/// target, and the scenario geometry override (used when the scenario light
/// overrides its `ligh` tag's shape).
#[derive(Debug, Clone, Default)]
pub struct ScenarioLightDatum {
    /// `type` — sphere vs frustum (scenario override of the light tag's type).
    pub light_type: Enum<LightType, i16>,
    pub flags: Flags<ScenarioLightFlags, u16>,
    pub lightmap_type: Enum<ScenarioLightLightmapType, i16>,
    /// `target point` — the world point a frustum light aims at.
    pub target_point: RealPoint3d,
    /// `width` — frustum near width (scenario override).
    pub width: f32,
    /// `height scale` — frustum height scale (scenario override).
    pub height_scale: f32,
    /// `field of view` in **degrees** (scenario override).
    pub field_of_view: f32,
    /// `falloff distance` (scenario override).
    pub falloff_distance: f32,
    /// `cutoff distance` (scenario override).
    pub cutoff_distance: f32,
}

impl ScenarioLightDatum {
    /// Read the `light data` sub-struct out of a `scenario_light_block` entry.
    fn from_light_block(s: &TagStruct<'_>) -> Self {
        s.field("light data")
            .and_then(|f| f.as_struct())
            .map(|light| Self::from_struct(&light))
            .unwrap_or_default()
    }

    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            light_type: s.try_read_enum("type").unwrap_or_default(),
            flags: s.try_read_flags("flags").unwrap_or_default(),
            lightmap_type: s.try_read_enum("lightmap type").unwrap_or_default(),
            target_point: s.read_point3d("target point"),
            width: s.read_real("width").unwrap_or(0.0),
            height_scale: s.read_real("height scale").unwrap_or(1.0),
            field_of_view: s.read_real("field of view").unwrap_or(0.0),
            falloff_distance: s.read_real("falloff distance").unwrap_or(0.0),
            cutoff_distance: s.read_real("cutoff distance").unwrap_or(0.0),
        }
    }

    /// True if this light is baked into the lightmap only (not a runtime dynamic
    /// light). Mirrors the engine `lightmap_type == 3` placement gate.
    pub fn is_lightmap_only(&self) -> bool {
        self.lightmap_type == ScenarioLightLightmapType::LightmapsOnly
    }

    /// True if this light renders only in cinematics (`flags & cinematic_only`).
    pub fn is_cinematic_only(&self) -> bool {
        self.flags.contains(ScenarioLightFlags::CinematicOnly)
    }
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
    /// `parent id` — optional attachment of this placement to a parent
    /// object's marker. `None` when the placement has no parent (the
    /// common case). See [`ParentId`].
    pub parent_id: Option<ParentId>,
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
            parent_id: s
                .field("parent id")
                .and_then(|f| f.as_struct())
                .map(|st| ParentId::from_struct(&st))
                .filter(|p| p.has_parent()),
        }
    }
}

/// `object data.parent id` — links a placement to a parent object by
/// NAME index (into [`Scenario::object_names`]) plus the parent marker it
/// attaches to. Engine path:
/// `object_placement_data_new_from_scenario_object @0x1808189D0` resolves
/// `parent_object_name_index` via `object_index_from_name_index`, then
/// `object_new @0x1807D51D0` calls `object_attach_to_marker @0x1807D7F70`
/// → `object_attach_to_marker_immediate @0x1807D8050`, which snaps the
/// child's origin to the parent marker's world matrix and records the
/// parent node for subsequent node-matrix composition.
#[derive(Debug, Clone, Default)]
pub struct ParentId {
    /// `parent object` — block index into [`Scenario::object_names`];
    /// `-1` = no parent. Resolved to a runtime object by name.
    pub parent_object_name_index: i16,
    /// `parent marker` — string_id of the marker ON THE PARENT this
    /// object attaches to (e.g. `"waterfall"`). Empty = attach to the
    /// parent's origin/node 0.
    pub parent_marker: String,
    /// `connection marker` — string_id of the marker on THIS object that
    /// mates with the parent marker (engine `child_marker_name`). Empty =
    /// align this object's origin to the parent marker.
    pub connection_marker: String,
}

impl ParentId {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            parent_object_name_index: s.read_block_index("parent object"),
            parent_marker: s.read_string_id("parent marker").unwrap_or_default(),
            connection_marker: s.read_string_id("connection marker").unwrap_or_default(),
        }
    }

    /// True when this placement actually references a parent (the name
    /// index is valid). Placements with no parent decode to `None` on
    /// [`PlacementObjectData::parent_id`].
    pub fn has_parent(&self) -> bool {
        self.parent_object_name_index >= 0
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
// Decals
// =============================================================================
//
// Schema (per `definitions/halo3_mcc/scenario.json`):
//   `decal palette` block — `scenario_decal_palette_block`. Each entry
//      is a single tag_reference to a `.decal_system` (`decs` group).
//   `decals` block — `scenario_decals_block` (sizeof=36).
//      Each entry has:
//        - decal palette index   (short_block_index)
//        - editing bound to bsp* (char_block_index)
//        - rotation*             (real_quaternion)
//        - position*             (real_point_3d)
//        - scale*                (real)
//      plus inline editor-only custom fields (filt header + rede footer)
//      that we ignore.
//
// Cyberdyne (Epitaph) authors 252 placements / 46 palette entries.
// Mesh projection is performed at runtime — `c_decal_system::create →
// collide → build_mesh` clips BSP triangles against the projection
// volume. That bake happens in protomorph at scenario load.

/// One entry in `scenario.decal_palette` — a tag reference to a
/// `.decal_system` tag.
#[derive(Debug, Clone, Default)]
pub struct DecalPaletteEntry {
    /// `reference^` — path to the `.decal_system` tag.
    pub decal_system: String,
}

impl DecalPaletteEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            decal_system: s
                .read_tag_ref_path("reference")
                .or_else(|| s.read_tag_ref_path("reference^"))
                .unwrap_or_default(),
        }
    }
}

/// One author-placed decal — projects onto BSP geometry at the
/// referenced palette entry's projection parameters.
#[derive(Debug, Clone, Default)]
pub struct DecalPlacement {
    /// `decal palette index` — into `Scenario::decal_palette`.
    /// -1 if the placement is unbound.
    pub palette_index: i16,
    /// `editing bound to bsp*!` — which structure_bsp this placement
    /// was authored against. -1 = unbound.
    pub editing_bound_to_bsp: i8,
    /// `rotation*` — projection orientation (forward + up axes
    /// derived from the quaternion in the runtime bake).
    pub rotation: RealQuaternion,
    /// `position*` — projection center (world units).
    pub position: RealPoint3d,
    /// `scale*` — projection-quad scale (world units). The runtime
    /// projection box is `radius × scale` along forward / up / right.
    pub scale: f32,
}

impl DecalPlacement {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            palette_index: s.read_block_index("decal palette index"),
            editing_bound_to_bsp: s.read_block_index("editing bound to bsp") as i8,
            rotation: read_quaternion(s, "rotation"),
            position: s.read_point3d("position"),
            scale: s.read_real("scale").unwrap_or(1.0),
        }
    }
}

// =============================================================================
// Cubemaps
// =============================================================================

/// One entry in scenario `cubemaps` — author-placed probe descriptor.
/// The actual cube content lives in
/// `<scenario_dir>/<scenario>_<bsp_name>_cubemaps.bitmap` as one cube
/// per scenario probe (1:1 by index for single-BSP MP maps;
/// partitioned by point-in-BSP for multi-BSP campaign maps). The
/// per-cluster routing block (`structure_cluster.cluster_cubemaps[]`)
/// is stripped on MCC; the engine recomputes it via
/// `c_dynamic_cubemap_sample::search_for_cubemap_sample_in_cluster` —
/// nearest scenario probe by 3D distance to the cluster centroid.
///
/// Schema (`scenario_cubemap_definitions.cpp`): the position field is
/// labelled `_field_real_vector_2d` in the original tag def but the
/// 16-byte struct sizing forces a 12-byte vec3 layout — both the H3
/// `tool.exe` baker and the runtime read it as a `real_point_3d`.
/// MCC's tag inspector also surfaces it as `real point 3d`. Resolution
/// is a `short_enum` with values 0=16, 1=32, 2=64, 3=128, 4=256.
#[derive(Debug, Clone, Default)]
pub struct CubemapEntry {
    /// World-space position of this probe.
    pub position: crate::math::RealPoint3d,
    /// Authored cube resolution in pixels (16 / 32 / 64 / 128 / 256).
    /// Decoded from the `cubemap resolution` short_enum.
    pub resolution_pixels: u32,
}

impl CubemapEntry {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let resolution_pixels = match s
            .read_enum_name("cubemap resolution")
            .as_deref()
        {
            Some("16") => 16,
            Some("32") => 32,
            Some("64") => 64,
            Some("128") => 128,
            Some("256") => 256,
            _ => 128,
        };
        Self {
            position: s.read_point3d("cubemap position"),
            resolution_pixels,
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


// =============================================================================
// Zone-set PVS — Ares `scenario_definitions.h:622-654` /
// MCC `scenario_zone_set_pvs_block`
// =============================================================================

/// `zone set pvs[i]` — `scenario_zone_set_pvs_block` (44B per entry,
/// MCC schema). One entry per scenario zone set. Holds the
/// precomputed cluster-PVS bit vectors (open + closed) per active BSP,
/// plus per-cluster sky/audio annotations and portal→device mappings
/// (for door-portal activation).
///
/// Engine consumers:
/// - `scenario_zone_set_pvs_get_row @ 0x180333630` reads
///   `structure_bsp_pvs[bsp_idx].cluster_pvs[cluster_idx].pvs[]` to
///   populate an `s_scenario_pvs_row` (192B per row).
/// - `structure_bsp_compute_cluster_active_pvs @ 0x180334F80` merges
///   open/closed rows based on `portal_activation` state.
#[derive(Debug, Clone, Default)]
pub struct ZoneSetPvs {
    /// `structure bsp mask` — bitmask of BSP indices this zone set
    /// activates. Bit i set ⇒ BSP i is active in this zone set.
    pub structure_bsp_mask: u32,
    /// `version` — incremented when the PVS bake format changes.
    pub version: i16,
    /// `flags` — `scenario_zone_set_pvs_flags`.
    pub flags: u16,
    /// `bsp checksums` — per-active-BSP checksum used to detect
    /// scenario↔BSP mismatch at load time.
    pub bsp_checksums: Vec<i32>,
    /// `structure bsp pvs` — per-active-BSP PVS bundle (open + closed
    /// + sky/audio annotations).
    pub structure_bsp_pvs: Vec<ZoneSetBspPvs>,
    /// `portal=>device mapping` — per-active-BSP portal→device-machine
    /// mapping for door-portal activation. Empty on BSPs with no
    /// machine doors. Phase D5 needs this for `structure_bsp_compute_cluster_active_pvs`
    /// when a cluster's `pvs_affected_by_door_portal` flag is set.
    pub portal_device_mapping: Vec<ZoneSetPortalDeviceMapping>,
}

impl ZoneSetPvs {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            structure_bsp_mask: s.read_int_any("structure bsp mask").unwrap_or(0) as u32,
            version: s.read_int_any("version").unwrap_or(0) as i16,
            flags: s.read_int_any("flags").unwrap_or(0) as u16,
            bsp_checksums: s
                .field("bsp checksums")
                .and_then(|f| f.as_block())
                .map(|b| {
                    let mut out = Vec::with_capacity(b.len());
                    for i in 0..b.len() {
                        if let Some(e) = b.element(i) {
                            out.push(e.read_int_any("bsp checksum").unwrap_or(0) as i32);
                        }
                    }
                    out
                })
                .unwrap_or_default(),
            structure_bsp_pvs: read_block(
                s,
                "structure bsp pvs",
                ZoneSetBspPvs::from_struct,
            ),
            portal_device_mapping: read_block(
                s,
                "portal=>device mapping",
                ZoneSetPortalDeviceMapping::from_struct,
            ),
        }
    }
}

/// `structure bsp pvs[i]` — `scenario_zone_set_bsp_pvs_block` (84B).
/// Per-active-BSP PVS bundle. Each `cluster_pvs[c]` row holds the
/// per-active-BSP bit vectors describing which clusters in which
/// active BSPs are reachable from cluster `c`.
#[derive(Debug, Clone, Default)]
pub struct ZoneSetBspPvs {
    /// `cluster pvs` — open PVS rows. One entry per cluster in this
    /// BSP. `cluster_pvs[c].bsp_bit_vectors[bsp][word]` = u32 word of
    /// the per-cluster reachable-from-cluster-c bitset.
    pub cluster_pvs: Vec<ZoneSetClusterPvs>,
    /// `cluster pvs doors closed` — same shape as `cluster_pvs`, but
    /// with all door portals treated as closed. Engine merges these
    /// based on portal-activation state at runtime.
    pub cluster_pvs_doors_closed: Vec<ZoneSetClusterPvs>,
    /// `attached sky indices` — per-cluster sky reference (signed
    /// 8-bit index into scenario.skies, -1 = no sky). Stored as raw
    /// `u8`; -1 sentinel comes back as `0xFF`. Cast to `i8` at use.
    pub attached_sky_indices: Vec<u8>,
    /// `visible sky indices` — per-cluster: which sky is rendered
    /// for that cluster. Same convention as `attached_sky_indices`.
    pub visible_sky_indices: Vec<u8>,
    /// `mutiple skies visible bitvector` — bit per cluster set when
    /// >1 sky is visible from that cluster (engine renders both,
    /// stencil-masked).
    pub multiple_skies_visible_bit_vector: Vec<u32>,
    /// `cluster audio bitvector` — per-cluster audio activation bits.
    pub cluster_audio_bit_vector: Vec<u32>,
    /// `cluster audio cluster neighbors` — packed neighbor table for
    /// audio propagation. Phase F+ for audio plumbing; raw bytes
    /// retained.
    pub cluster_audio_cluster_neighbors: Vec<u8>,
}

impl ZoneSetBspPvs {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cluster_pvs: read_block(s, "cluster pvs", ZoneSetClusterPvs::from_struct),
            cluster_pvs_doors_closed: read_block(
                s,
                "cluster pvs doors closed",
                ZoneSetClusterPvs::from_struct,
            ),
            attached_sky_indices: read_byte_block(s, "attached sky indices", "sky index"),
            visible_sky_indices: read_byte_block(s, "visible sky indices", "sky index"),
            multiple_skies_visible_bit_vector: read_u32_block(
                s,
                "mutiple skies visible bitvector",
                "bitvector",
            ),
            cluster_audio_bit_vector: read_u32_block(
                s,
                "cluster audio bitvector",
                "bitvector",
            ),
            cluster_audio_cluster_neighbors: read_byte_block(
                s,
                "cluster audio cluster neighbors",
                "neighbor",
            ),
        }
    }
}

/// `cluster pvs[i]` — `scenario_zone_set_cluster_pvs_block` (12B).
/// One entry per BSP cluster. Holds the per-active-BSP bit vectors
/// describing which clusters are reachable from this one.
#[derive(Debug, Clone, Default)]
pub struct ZoneSetClusterPvs {
    /// `cluster pvs bit vectors` — outer Vec is per-active-BSP (max
    /// 16); inner Vec is u32 words of the bit vector for that BSP
    /// (max 8 words = 256 cluster bits per BSP).
    pub bsp_bit_vectors: Vec<Vec<u32>>,
}

impl ZoneSetClusterPvs {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        let bsp_bit_vectors = s
            .field("cluster pvs bit vectors")
            .and_then(|f| f.as_block())
            .map(|outer| {
                let mut bsps = Vec::with_capacity(outer.len());
                for i in 0..outer.len() {
                    if let Some(bsp_elem) = outer.element(i) {
                        bsps.push(read_u32_block(&bsp_elem, "bits", "dword"));
                    }
                }
                bsps
            })
            .unwrap_or_default();
        Self { bsp_bit_vectors }
    }
}

/// `portal=>device mapping[i]` — `structure_portal_device_mapping_block`
/// (per-active-BSP). Maps cluster portals to door machines for
/// runtime portal activation (Phase D5). Most BSPs have no doors;
/// stored as opaque sub-blocks for future use.
#[derive(Debug, Clone, Default)]
pub struct ZoneSetPortalDeviceMapping {
    /// `device portal associations` — (portal_index, device_index) pairs.
    pub device_portal_associations: Vec<DevicePortalAssociation>,
    /// `game portal to portal mapping` — engine portal→game portal
    /// renumbering.
    pub game_portal_to_portal_mapping: Vec<GamePortalToPortalMapping>,
}

impl ZoneSetPortalDeviceMapping {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            device_portal_associations: read_block(
                s,
                "device portal associations",
                DevicePortalAssociation::from_struct,
            ),
            game_portal_to_portal_mapping: read_block(
                s,
                "game portal to portal mapping",
                GamePortalToPortalMapping::from_struct,
            ),
        }
    }
}

/// `structure_device_portal_association_block`. (Schema-driven;
/// fields populated when needed.)
#[derive(Debug, Clone, Default)]
pub struct DevicePortalAssociation {
    pub device_object_name: i16,
    pub portal_index: i16,
}

impl DevicePortalAssociation {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            device_object_name: s.read_block_index("device object name"),
            portal_index: s.read_int_any("portal index").unwrap_or(-1) as i16,
        }
    }
}

/// `game_portal_to_portal_mapping_block`.
#[derive(Debug, Clone, Default)]
pub struct GamePortalToPortalMapping {
    pub portal_index: i16,
    pub game_portal_index: i16,
}

impl GamePortalToPortalMapping {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            portal_index: s.read_int_any("portal index").unwrap_or(-1) as i16,
            game_portal_index: s.read_int_any("game portal index").unwrap_or(-1) as i16,
        }
    }
}

// ---- helpers used by ZoneSetBspPvs / ClusterPvs ----

fn read_byte_block(s: &TagStruct<'_>, block_name: &str, field_name: &str) -> Vec<u8> {
    s.field(block_name)
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(e.read_int_any(field_name).unwrap_or(0) as i8 as u8);
                }
            }
            out
        })
        .unwrap_or_default()
}

fn read_u32_block(s: &TagStruct<'_>, block_name: &str, field_name: &str) -> Vec<u32> {
    s.field(block_name)
        .and_then(|f| f.as_block())
        .map(|b| {
            let mut out = Vec::with_capacity(b.len());
            for i in 0..b.len() {
                if let Some(e) = b.element(i) {
                    out.push(e.read_int_any(field_name).unwrap_or(0) as u32);
                }
            }
            out
        })
        .unwrap_or_default()
}
