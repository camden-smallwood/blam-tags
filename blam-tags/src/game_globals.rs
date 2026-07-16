//! Schema-faithful walker for the `globals` (matg / "game_globals") tag.
//!
//! Mirrors `definitions/halo3_mcc/globals.json` 1:1: every block/struct/field
//! maps onto the public type tree below with the same nesting. Type names are
//! PascalCase with the `_block`/`_struct`/`_definition` suffixes stripped;
//! field names are the snake_case of the schema field's canonical base name
//! (the text before the first `^ : # | ! ~` annotation).
//! `[explanation]`/`[pad]`/`[terminator]` fields carry no data and are skipped.
//!
//! The twelve inline `language pack1!` … `language pack12!` sub-structs share
//! the single [`LanguagePack`] layout and are read into a fixed `[LanguagePack;
//! 12]` array. The two `player representation` blocks (`@player representation`
//! and `@player representation debug`) share [`PlayerRepresentation`].

use crate::api::TagStruct;
use crate::file::TagFile;
use crate::typed_enums::{Enum, Flags};

//================================================================================
// Typed enums / flags (one per `enums_flags` definition in the schema).
//================================================================================

/// `language_enum` (long_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i32)]
pub enum Language {
    #[default] #[strum(serialize = "english")] English = 0,
    #[strum(serialize = "japanese")] Japanese = 1,
    #[strum(serialize = "german")] German = 2,
    #[strum(serialize = "french")] French = 3,
    #[strum(serialize = "spanish")] Spanish = 4,
    #[strum(serialize = "mexican spanish")] MexicanSpanish = 5,
    #[strum(serialize = "italian")] Italian = 6,
    #[strum(serialize = "korean")] Korean = 7,
    #[strum(serialize = "chinese-traditional")] ChineseTraditional = 8,
    #[strum(serialize = "chinese-simplified")] ChineseSimplified = 9,
    #[strum(serialize = "portuguese")] Portuguese = 10,
    #[strum(serialize = "polish")] Polish = 11,
}

/// `global_transition_functions_enum` (short_enum).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i16)]
pub enum GlobalTransitionFunctions {
    #[default] #[strum(serialize = "linear")] Linear = 0,
    #[strum(serialize = "early")] Early = 1,
    #[strum(serialize = "very early")] VeryEarly = 2,
    #[strum(serialize = "late")] Late = 3,
    #[strum(serialize = "very late")] VeryLate = 4,
    #[strum(serialize = "cosine")] Cosine = 5,
    #[strum(serialize = "one")] One = 6,
    #[strum(serialize = "zero")] Zero = 7,
}

/// `player_model_choice_enum` (stored as a `char_enum` field).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum PlayerModelChoice {
    #[default] #[strum(serialize = "spartan")] Spartan = 0,
    #[strum(serialize = "elite")] Elite = 1,
}

/// `player_representation_class_enum` (stored as a `char_enum` field).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(i8)]
pub enum PlayerRepresentationClass {
    #[default] #[strum(serialize = "campaign")] Campaign = 0,
    #[strum(serialize = "multiplayer")] Multiplayer = 1,
    #[strum(serialize = "editor")] Editor = 2,
    #[strum(serialize = "survival")] Survival = 3,
}

/// `global_material_flags_definition` (word_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u16)]
pub enum GlobalMaterialFlags {
    #[strum(serialize = "flammable")] Flammable = 0,
    #[strum(serialize = "biomass")] Biomass = 1,
    #[strum(serialize = "rad xfer interior")] RadXferInterior = 2,
}

/// `materials_sweeteners_inheritance_flags` (long_flags).
#[derive(Clone, Copy, PartialEq, Eq, Debug, num_derive::FromPrimitive, num_derive::ToPrimitive,
         strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
#[strum(ascii_case_insensitive)]
#[repr(u32)]
pub enum MaterialsSweetenersInheritanceFlags {
    #[strum(serialize = "sound_small")] SoundSmall = 0,
    #[strum(serialize = "sound_medium")] SoundMedium = 1,
    #[strum(serialize = "sound_large")] SoundLarge = 2,
    #[strum(serialize = "sound_rolling")] SoundRolling = 3,
    #[strum(serialize = "sound_grinding")] SoundGrinding = 4,
    #[strum(serialize = "sound_melee_small")] SoundMeleeSmall = 5,
    #[strum(serialize = "sound_melee")] SoundMelee = 6,
    #[strum(serialize = "sound_melee_large")] SoundMeleeLarge = 7,
    #[strum(serialize = "effect_small")] EffectSmall = 8,
    #[strum(serialize = "effect_medium")] EffectMedium = 9,
    #[strum(serialize = "effect_large")] EffectLarge = 10,
    #[strum(serialize = "effect_rolling")] EffectRolling = 11,
    #[strum(serialize = "effect_grinding")] EffectGrinding = 12,
    #[strum(serialize = "effect_melee")] EffectMelee = 13,
    #[strum(serialize = "water_ripple_small")] WaterRippleSmall = 14,
    #[strum(serialize = "water_ripple_medium")] WaterRippleMedium = 15,
    #[strum(serialize = "water_ripple_large")] WaterRippleLarge = 16,
}

//================================================================================
// Schema-faithful sub-structs (mirrors globals.json 1:1).
//================================================================================

/// `data_hash_definition` — one runtime hash byte (`array` element).
#[derive(Debug, Clone, Default)]
pub struct DataHash {
    pub hash_byte: i8,
}

impl DataHash {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            hash_byte: s.read_int_any("hash byte").unwrap_or_default() as i8,
        }
    }
}

/// `language_pack_definition` — runtime string-pack bookkeeping (all `!`).
#[derive(Debug, Clone, Default)]
pub struct LanguagePack {
    pub string_reference_pointer: i64,
    pub string_data_pointer: i64,
    pub number_of_strings: i32,
    pub string_data_size: i32,
    pub string_reference_cache_offset: i32,
    pub string_data_cache_offset: i32,
    pub string_reference_checksum: Vec<DataHash>,
    pub string_data_checksum: Vec<DataHash>,
    pub data_loaded_boolean: i32,
}

impl LanguagePack {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            string_reference_pointer: s.read_int_any("string reference pointer").unwrap_or_default() as i64,
            string_data_pointer: s.read_int_any("string data pointer").unwrap_or_default() as i64,
            number_of_strings: s.read_int_any("number of strings").unwrap_or_default() as i32,
            string_data_size: s.read_int_any("string data size").unwrap_or_default() as i32,
            string_reference_cache_offset: s.read_int_any("string reference cache offset").unwrap_or_default() as i32,
            string_data_cache_offset: s.read_int_any("string data cache offset").unwrap_or_default() as i32,
            string_reference_checksum: s.field("string reference checksum").and_then(|f| f.as_array())
                .map(|a| a.iter().map(|e| DataHash::from_struct(&e)).collect()).unwrap_or_default(),
            string_data_checksum: s.field("string data checksum").and_then(|f| f.as_array())
                .map(|a| a.iter().map(|e| DataHash::from_struct(&e)).collect()).unwrap_or_default(),
            data_loaded_boolean: s.read_int_any("data loaded boolean").unwrap_or_default() as i32,
        }
    }
}

/// `havok_cleanup_resources_block`.
#[derive(Debug, Clone, Default)]
pub struct HavokCleanupResources {
    pub object_cleanup_effect: String,
}

impl HavokCleanupResources {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            object_cleanup_effect: s.read_tag_ref_path("object cleanup effect").unwrap_or_default(),
        }
    }
}

/// `sound_globals_block`.
#[derive(Debug, Clone, Default)]
pub struct SoundGlobals {
    pub sound_classes: String,
    pub sound_effects: String,
    pub sound_mix: String,
    pub sound_combat_dialogue_constants: String,
    pub sound_propagation: String,
}

impl SoundGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sound_classes: s.read_tag_ref_path("sound classes").unwrap_or_default(),
            sound_effects: s.read_tag_ref_path("sound effects").unwrap_or_default(),
            sound_mix: s.read_tag_ref_path("sound mix").unwrap_or_default(),
            sound_combat_dialogue_constants: s.read_tag_ref_path("sound combat dialogue constants").unwrap_or_default(),
            sound_propagation: s.read_tag_ref_path("sound propagation").unwrap_or_default(),
        }
    }
}

/// `ai_globals_gravemind_block`.
#[derive(Debug, Clone, Default)]
pub struct AiGlobalsGravemind {
    pub min_retreat_time: f32,
    pub ideal_retreat_time: f32,
    pub max_retreat_time: f32,
}

impl AiGlobalsGravemind {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            min_retreat_time: s.read_real("min retreat time").unwrap_or_default(),
            ideal_retreat_time: s.read_real("ideal retreat time").unwrap_or_default(),
            max_retreat_time: s.read_real("max retreat time").unwrap_or_default(),
        }
    }
}

/// `ai_globals_styles_block`.
#[derive(Debug, Clone, Default)]
pub struct AiGlobalsStyles {
    pub style: String,
}

impl AiGlobalsStyles {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            style: s.read_tag_ref_path("style").unwrap_or_default(),
        }
    }
}

/// `ai_globals_block`.
#[derive(Debug, Clone, Default)]
pub struct AiGlobals {
    pub ai_infantry_on_ai_weapon_damage_scale: f32,
    pub ai_vehicle_on_ai_weapon_damage_scale: f32,
    pub ai_player_vehicle_on_ai_weapon_damage_scale: f32,
    pub danger_broadly_facing: f32,
    pub danger_shooting_near: f32,
    pub danger_shooting_at: f32,
    pub danger_extremely_close: f32,
    pub danger_shield_damage: f32,
    pub danger_exetended_shield_damage: f32,
    pub danger_body_damage: f32,
    pub danger_extended_body_damage: f32,
    pub global_dialogue_tag: String,
    pub default_mission_dialogue_sound_effect: String,
    pub jump_down: f32,
    pub jump_step: f32,
    pub jump_crouch: f32,
    pub jump_stand: f32,
    pub jump_storey: f32,
    pub jump_tower: f32,
    pub max_jump_down_height_down: f32,
    pub max_jump_down_height_step: f32,
    pub max_jump_down_height_crouch: f32,
    pub max_jump_down_height_stand: f32,
    pub max_jump_down_height_storey: f32,
    pub max_jump_down_height_tower: f32,
    pub hoist_step: crate::math::RealBounds,
    pub hoist_crouch: crate::math::RealBounds,
    pub hoist_stand: crate::math::RealBounds,
    pub vault_step: crate::math::RealBounds,
    pub vault_crouch: crate::math::RealBounds,
    pub gravemind_properties: Vec<AiGlobalsGravemind>,
    pub scary_target_threhold: f32,
    pub scary_weapon_threhold: f32,
    pub player_scariness: f32,
    pub berserking_actor_scariness: f32,
    pub kamikazeing_actor_scariness: f32,
    pub invincible_scariness: f32,
    pub morph_delay_ranged: f32,
    pub morph_delay_tank: f32,
    pub morph_delay_stalker: f32,
    pub min_death_time: f32,
    pub projectile_distance: f32,
    pub idle_clump_distance: f32,
    pub dangerous_clump_distance: f32,
    pub global_styles: Vec<AiGlobalsStyles>,
}

impl AiGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            ai_infantry_on_ai_weapon_damage_scale: s.read_real("AI infantry-on-AI weapon damage scale").unwrap_or_default(),
            ai_vehicle_on_ai_weapon_damage_scale: s.read_real("AI vehicle-on-AI weapon damage scale").unwrap_or_default(),
            ai_player_vehicle_on_ai_weapon_damage_scale: s.read_real("AI player vehicle-on-AI weapon damage scale").unwrap_or_default(),
            danger_broadly_facing: s.read_real("danger broadly facing").unwrap_or_default(),
            danger_shooting_near: s.read_real("danger shooting near").unwrap_or_default(),
            danger_shooting_at: s.read_real("danger shooting at").unwrap_or_default(),
            danger_extremely_close: s.read_real("danger extremely close").unwrap_or_default(),
            danger_shield_damage: s.read_real("danger shield damage").unwrap_or_default(),
            danger_exetended_shield_damage: s.read_real("danger exetended shield damage").unwrap_or_default(),
            danger_body_damage: s.read_real("danger body damage").unwrap_or_default(),
            danger_extended_body_damage: s.read_real("danger extended body damage").unwrap_or_default(),
            global_dialogue_tag: s.read_tag_ref_path("global dialogue tag").unwrap_or_default(),
            default_mission_dialogue_sound_effect: s.read_string_id("default mission dialogue sound effect").unwrap_or_default(),
            jump_down: s.read_real("jump down").unwrap_or_default(),
            jump_step: s.read_real("jump step").unwrap_or_default(),
            jump_crouch: s.read_real("jump crouch").unwrap_or_default(),
            jump_stand: s.read_real("jump stand").unwrap_or_default(),
            jump_storey: s.read_real("jump storey").unwrap_or_default(),
            jump_tower: s.read_real("jump tower").unwrap_or_default(),
            max_jump_down_height_down: s.read_real("max jump down height down").unwrap_or_default(),
            max_jump_down_height_step: s.read_real("max jump down height step").unwrap_or_default(),
            max_jump_down_height_crouch: s.read_real("max jump down height crouch").unwrap_or_default(),
            max_jump_down_height_stand: s.read_real("max jump down height stand").unwrap_or_default(),
            max_jump_down_height_storey: s.read_real("max jump down height storey").unwrap_or_default(),
            max_jump_down_height_tower: s.read_real("max jump down height tower").unwrap_or_default(),
            hoist_step: s.read_real_bounds("hoist step"),
            hoist_crouch: s.read_real_bounds("hoist crouch"),
            hoist_stand: s.read_real_bounds("hoist stand"),
            vault_step: s.read_real_bounds("vault step"),
            vault_crouch: s.read_real_bounds("vault crouch"),
            gravemind_properties: read_block_vec(s, "gravemind properties", AiGlobalsGravemind::from_struct),
            scary_target_threhold: s.read_real("scary target threhold").unwrap_or_default(),
            scary_weapon_threhold: s.read_real("scary weapon threhold").unwrap_or_default(),
            player_scariness: s.read_real("player scariness").unwrap_or_default(),
            berserking_actor_scariness: s.read_real("berserking actor scariness").unwrap_or_default(),
            kamikazeing_actor_scariness: s.read_real("kamikazeing actor scariness").unwrap_or_default(),
            invincible_scariness: s.read_real("invincible scariness").unwrap_or_default(),
            morph_delay_ranged: s.read_real("morph delay (ranged)").unwrap_or_default(),
            morph_delay_tank: s.read_real("morph delay (tank)").unwrap_or_default(),
            morph_delay_stalker: s.read_real("morph delay (stalker)").unwrap_or_default(),
            min_death_time: s.read_real("min death time").unwrap_or_default(),
            projectile_distance: s.read_real("projectile distance").unwrap_or_default(),
            idle_clump_distance: s.read_real("idle clump distance").unwrap_or_default(),
            dangerous_clump_distance: s.read_real("dangerous clump distance").unwrap_or_default(),
            global_styles: read_block_vec(s, "global styles", AiGlobalsStyles::from_struct),
        }
    }
}

/// `armor_modifier_block`.
#[derive(Debug, Clone, Default)]
pub struct ArmorModifier {
    pub name: String,
    pub damage_multiplier: f32,
}

impl ArmorModifier {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            damage_multiplier: s.read_real("damage multiplier").unwrap_or_default(),
        }
    }
}

/// `damage_group_block`.
#[derive(Debug, Clone, Default)]
pub struct DamageGroup {
    pub name: String,
    pub armor_modifiers: Vec<ArmorModifier>,
}

impl DamageGroup {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            armor_modifiers: read_block_vec(s, "armor modifiers", ArmorModifier::from_struct),
        }
    }
}

/// `game_globals_damage_block`.
#[derive(Debug, Clone, Default)]
pub struct GameGlobalsDamage {
    pub damage_groups: Vec<DamageGroup>,
}

impl GameGlobalsDamage {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            damage_groups: read_block_vec(s, "damage groups", DamageGroup::from_struct),
        }
    }
}

/// `camera_block`.
#[derive(Debug, Clone, Default)]
pub struct Camera {
    pub default_unit_camera_track: String,
    pub field_of_view_scale: f32,
    pub yaw_scale: f32,
    pub pitch_scale: f32,
    pub forward_scale: f32,
    pub side_scale: f32,
    pub up_scale: f32,
    pub transition_time: f32,
    pub falling_death_transition_time: f32,
    pub initial_distance: f32,
    pub final_distance: f32,
    pub dead_cam_z_offset: f32,
    pub dead_cam_maximum_elevation: f32,
    pub dead_cam_movement_delay: f32,
    pub time: f32,
    pub dead_camera_minimum_falling_velocity: f32,
    pub maximum_boost_speed: f32,
    pub time_to_maximum_boost: f32,
    pub boost_function: Enum<GlobalTransitionFunctions, i16>,
    pub zoomed_field_of_view: f32,
    pub zoomed_look_speed: f32,
    pub bounding_sphere_radius: f32,
    pub flying_cam_movement_delay: f32,
    pub zoom_transition_time: f32,
    pub vertical_movement_time_to_max_speed: f32,
    pub vertical_movement_function: Enum<GlobalTransitionFunctions, i16>,
    pub minimum_distance: f32,
    pub maximum_distance: f32,
    pub orbit_cam_movement_delay: f32,
    pub orbit_cam_z_offset: f32,
    pub orbit_cam_minimum_elevation: f32,
    pub orbit_cam_maximum_elevation: f32,
    pub max_playback_speed: f32,
    pub fade_out_time: f32,
    pub fade_in_time: f32,
    pub enter_vehicle_transition_time: f32,
    pub exit_vehicle_transition_time: f32,
}

impl Camera {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            default_unit_camera_track: s.read_tag_ref_path("default unit camera track").unwrap_or_default(),
            field_of_view_scale: s.read_real("field of view scale").unwrap_or_default(),
            yaw_scale: s.read_real("yaw scale").unwrap_or_default(),
            pitch_scale: s.read_real("pitch scale").unwrap_or_default(),
            forward_scale: s.read_real("forward scale").unwrap_or_default(),
            side_scale: s.read_real("side scale").unwrap_or_default(),
            up_scale: s.read_real("up scale").unwrap_or_default(),
            transition_time: s.read_real("transition time").unwrap_or_default(),
            falling_death_transition_time: s.read_real("falling death transition time").unwrap_or_default(),
            initial_distance: s.read_real("initial distance").unwrap_or_default(),
            final_distance: s.read_real("final distance").unwrap_or_default(),
            dead_cam_z_offset: s.read_real("dead cam z offset").unwrap_or_default(),
            dead_cam_maximum_elevation: s.read_real("dead cam maximum elevation").unwrap_or_default(),
            dead_cam_movement_delay: s.read_real("dead cam movement delay").unwrap_or_default(),
            time: s.read_real("time").unwrap_or_default(),
            dead_camera_minimum_falling_velocity: s.read_real("dead camera minimum falling velocity").unwrap_or_default(),
            maximum_boost_speed: s.read_real("maximum boost speed").unwrap_or_default(),
            time_to_maximum_boost: s.read_real("time to maximum_boost").unwrap_or_default(),
            boost_function: s.try_read_enum("boost function").unwrap_or_default(),
            zoomed_field_of_view: s.read_real("zoomed field of view").unwrap_or_default(),
            zoomed_look_speed: s.read_real("zoomed look speed").unwrap_or_default(),
            bounding_sphere_radius: s.read_real("bounding sphere radius").unwrap_or_default(),
            flying_cam_movement_delay: s.read_real("flying cam movement delay").unwrap_or_default(),
            zoom_transition_time: s.read_real("zoom transition time").unwrap_or_default(),
            vertical_movement_time_to_max_speed: s.read_real("vertical movement time to max speed").unwrap_or_default(),
            vertical_movement_function: s.try_read_enum("vertical movement function").unwrap_or_default(),
            minimum_distance: s.read_real("minimum distance").unwrap_or_default(),
            maximum_distance: s.read_real("maximum distance").unwrap_or_default(),
            orbit_cam_movement_delay: s.read_real("orbit cam movement delay").unwrap_or_default(),
            orbit_cam_z_offset: s.read_real("orbit cam z offset").unwrap_or_default(),
            orbit_cam_minimum_elevation: s.read_real("orbit cam minimum elevation").unwrap_or_default(),
            orbit_cam_maximum_elevation: s.read_real("orbit cam maximum elevation").unwrap_or_default(),
            max_playback_speed: s.read_real("max playback speed").unwrap_or_default(),
            fade_out_time: s.read_real("fade out time").unwrap_or_default(),
            fade_in_time: s.read_real("fade in time").unwrap_or_default(),
            enter_vehicle_transition_time: s.read_real("enter vehicle transition time").unwrap_or_default(),
            exit_vehicle_transition_time: s.read_real("exit vehicle transition time").unwrap_or_default(),
        }
    }
}

/// `look_function_block`.
#[derive(Debug, Clone, Default)]
pub struct LookFunction {
    pub scale: f32,
}

impl LookFunction {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            scale: s.read_real("scale").unwrap_or_default(),
        }
    }
}

/// `player_control_block`.
#[derive(Debug, Clone, Default)]
pub struct PlayerControl {
    pub magnetism_friction: f32,
    pub magnetism_adhesion: f32,
    pub inconsequential_target_scale: f32,
    pub crosshair_location: crate::math::RealPoint2d,
    pub seconds_to_start: f32,
    pub seconds_to_full_speed: f32,
    pub decay_rate: f32,
    pub full_speed_multiplier: f32,
    pub pegged_magnitude: f32,
    pub pegged_angular_threshold: f32,
    pub look_default_pitch_rate: f32,
    pub look_default_yaw_rate: f32,
    pub look_peg_threshold: f32,
    pub look_yaw_acceleration_time: f32,
    pub look_yaw_acceleration_scale: f32,
    pub look_pitch_acceleration_time: f32,
    pub look_pitch_acceleration_scale: f32,
    pub look_autolevelling_scale: f32,
    pub gravity_scale: f32,
    pub minimum_autolevelling_ticks: i16,
    pub minimum_angle_for_vehicle_flipping: f32,
    pub look_function: Vec<LookFunction>,
    pub minimum_action_hold_time: f32,
    pub pegged_zoom_supression_threshold: f32,
}

impl PlayerControl {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            magnetism_friction: s.read_real("magnetism friction").unwrap_or_default(),
            magnetism_adhesion: s.read_real("magnetism adhesion").unwrap_or_default(),
            inconsequential_target_scale: s.read_real("inconsequential target scale").unwrap_or_default(),
            crosshair_location: s.read_point2d("crosshair location"),
            seconds_to_start: s.read_real("seconds to start").unwrap_or_default(),
            seconds_to_full_speed: s.read_real("seconds to full speed").unwrap_or_default(),
            decay_rate: s.read_real("decay rate").unwrap_or_default(),
            full_speed_multiplier: s.read_real("full speed multiplier").unwrap_or_default(),
            pegged_magnitude: s.read_real("pegged magnitude").unwrap_or_default(),
            pegged_angular_threshold: s.read_real("pegged angular threshold").unwrap_or_default(),
            look_default_pitch_rate: s.read_real("look default pitch rate").unwrap_or_default(),
            look_default_yaw_rate: s.read_real("look default yaw rate").unwrap_or_default(),
            look_peg_threshold: s.read_real("look peg threshold [0,1]").unwrap_or_default(),
            look_yaw_acceleration_time: s.read_real("look yaw acceleration time").unwrap_or_default(),
            look_yaw_acceleration_scale: s.read_real("look yaw acceleration scale").unwrap_or_default(),
            look_pitch_acceleration_time: s.read_real("look pitch acceleration time").unwrap_or_default(),
            look_pitch_acceleration_scale: s.read_real("look pitch acceleration scale").unwrap_or_default(),
            look_autolevelling_scale: s.read_real("look autolevelling scale").unwrap_or_default(),
            gravity_scale: s.read_real("gravity_scale").unwrap_or_default(),
            minimum_autolevelling_ticks: s.read_int_any("minimum autolevelling ticks").unwrap_or_default() as i16,
            minimum_angle_for_vehicle_flipping: s.read_real("minimum angle for vehicle flipping").unwrap_or_default(),
            look_function: read_block_vec(s, "look function", LookFunction::from_struct),
            minimum_action_hold_time: s.read_real("minimum action hold time").unwrap_or_default(),
            pegged_zoom_supression_threshold: s.read_real("pegged zoom supression threshold").unwrap_or_default(),
        }
    }
}

/// `difficulty_block`.
#[derive(Debug, Clone, Default)]
pub struct Difficulty {
    pub easy_enemy_damage: f32,
    pub normal_enemy_damage: f32,
    pub hard_enemy_damage: f32,
    pub imposs_enemy_damage: f32,
    pub easy_enemy_vitality: f32,
    pub normal_enemy_vitality: f32,
    pub hard_enemy_vitality: f32,
    pub imposs_enemy_vitality: f32,
    pub easy_enemy_shield: f32,
    pub normal_enemy_shield: f32,
    pub hard_enemy_shield: f32,
    pub imposs_enemy_shield: f32,
    pub easy_enemy_recharge: f32,
    pub normal_enemy_recharge: f32,
    pub hard_enemy_recharge: f32,
    pub imposs_enemy_recharge: f32,
    pub easy_friend_damage: f32,
    pub normal_friend_damage: f32,
    pub hard_friend_damage: f32,
    pub imposs_friend_damage: f32,
    pub easy_friend_vitality: f32,
    pub normal_friend_vitality: f32,
    pub hard_friend_vitality: f32,
    pub imposs_friend_vitality: f32,
    pub easy_friend_shield: f32,
    pub normal_friend_shield: f32,
    pub hard_friend_shield: f32,
    pub imposs_friend_shield: f32,
    pub easy_friend_recharge: f32,
    pub normal_friend_recharge: f32,
    pub hard_friend_recharge: f32,
    pub imposs_friend_recharge: f32,
    pub easy_infection_forms: f32,
    pub normal_infection_forms: f32,
    pub hard_infection_forms: f32,
    pub imposs_infection_forms: f32,
    pub easy_rate_of_fire: f32,
    pub normal_rate_of_fire: f32,
    pub hard_rate_of_fire: f32,
    pub imposs_rate_of_fire: f32,
    pub easy_projectile_error: f32,
    pub normal_projectile_error: f32,
    pub hard_projectile_error: f32,
    pub imposs_projectile_error: f32,
    pub easy_burst_error: f32,
    pub normal_burst_error: f32,
    pub hard_burst_error: f32,
    pub imposs_burst_error: f32,
    pub easy_new_target_delay: f32,
    pub normal_new_target_delay: f32,
    pub hard_new_target_delay: f32,
    pub imposs_new_target_delay: f32,
    pub easy_burst_separation: f32,
    pub normal_burst_separation: f32,
    pub hard_burst_separation: f32,
    pub imposs_burst_separation: f32,
    pub easy_target_tracking: f32,
    pub normal_target_tracking: f32,
    pub hard_target_tracking: f32,
    pub imposs_target_tracking: f32,
    pub easy_target_leading: f32,
    pub normal_target_leading: f32,
    pub hard_target_leading: f32,
    pub imposs_target_leading: f32,
    pub easy_overcharge_chance: f32,
    pub normal_overcharge_chance: f32,
    pub hard_overcharge_chance: f32,
    pub imposs_overcharge_chance: f32,
    pub easy_special_fire_delay: f32,
    pub normal_special_fire_delay: f32,
    pub hard_special_fire_delay: f32,
    pub imposs_special_fire_delay: f32,
    pub easy_guidance_vs_player: f32,
    pub normal_guidance_vs_player: f32,
    pub hard_guidance_vs_player: f32,
    pub imposs_guidance_vs_player: f32,
    pub easy_melee_delay_base: f32,
    pub normal_melee_delay_base: f32,
    pub hard_melee_delay_base: f32,
    pub imposs_melee_delay_base: f32,
    pub easy_melee_delay_scale: f32,
    pub normal_melee_delay_scale: f32,
    pub hard_melee_delay_scale: f32,
    pub imposs_melee_delay_scale: f32,
    pub easy_grenade_chance_scale: f32,
    pub normal_grenade_chance_scale: f32,
    pub hard_grenade_chance_scale: f32,
    pub imposs_grenade_chance_scale: f32,
    pub easy_grenade_timer_scale: f32,
    pub normal_grenade_timer_scale: f32,
    pub hard_grenade_timer_scale: f32,
    pub imposs_grenade_timer_scale: f32,
    pub easy_major_upgrade_normal: f32,
    pub normal_major_upgrade_normal: f32,
    pub hard_major_upgrade_normal: f32,
    pub imposs_major_upgrade_normal: f32,
    pub easy_major_upgrade_few: f32,
    pub normal_major_upgrade_few: f32,
    pub hard_major_upgrade_few: f32,
    pub imposs_major_upgrade_few: f32,
    pub easy_major_upgrade_many: f32,
    pub normal_major_upgrade_many: f32,
    pub hard_major_upgrade_many: f32,
    pub imposs_major_upgrade_many: f32,
    pub easy_player_vehicle_ram_chance: f32,
    pub normal_player_vehicle_ram_chance: f32,
    pub hard_player_vehicle_ram_chance: f32,
    pub imposs_player_vehicle_ram_chance: f32,
}

impl Difficulty {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            easy_enemy_damage: s.read_real("easy enemy damage").unwrap_or_default(),
            normal_enemy_damage: s.read_real("normal enemy damage").unwrap_or_default(),
            hard_enemy_damage: s.read_real("hard enemy damage").unwrap_or_default(),
            imposs_enemy_damage: s.read_real("imposs. enemy damage").unwrap_or_default(),
            easy_enemy_vitality: s.read_real("easy enemy vitality").unwrap_or_default(),
            normal_enemy_vitality: s.read_real("normal enemy vitality").unwrap_or_default(),
            hard_enemy_vitality: s.read_real("hard enemy vitality").unwrap_or_default(),
            imposs_enemy_vitality: s.read_real("imposs. enemy vitality").unwrap_or_default(),
            easy_enemy_shield: s.read_real("easy enemy shield").unwrap_or_default(),
            normal_enemy_shield: s.read_real("normal enemy shield").unwrap_or_default(),
            hard_enemy_shield: s.read_real("hard enemy shield").unwrap_or_default(),
            imposs_enemy_shield: s.read_real("imposs. enemy shield").unwrap_or_default(),
            easy_enemy_recharge: s.read_real("easy enemy recharge").unwrap_or_default(),
            normal_enemy_recharge: s.read_real("normal enemy recharge").unwrap_or_default(),
            hard_enemy_recharge: s.read_real("hard enemy recharge").unwrap_or_default(),
            imposs_enemy_recharge: s.read_real("imposs. enemy recharge").unwrap_or_default(),
            easy_friend_damage: s.read_real("easy friend damage").unwrap_or_default(),
            normal_friend_damage: s.read_real("normal friend damage").unwrap_or_default(),
            hard_friend_damage: s.read_real("hard friend damage").unwrap_or_default(),
            imposs_friend_damage: s.read_real("imposs. friend damage").unwrap_or_default(),
            easy_friend_vitality: s.read_real("easy friend vitality").unwrap_or_default(),
            normal_friend_vitality: s.read_real("normal friend vitality").unwrap_or_default(),
            hard_friend_vitality: s.read_real("hard friend vitality").unwrap_or_default(),
            imposs_friend_vitality: s.read_real("imposs. friend vitality").unwrap_or_default(),
            easy_friend_shield: s.read_real("easy friend shield").unwrap_or_default(),
            normal_friend_shield: s.read_real("normal friend shield").unwrap_or_default(),
            hard_friend_shield: s.read_real("hard friend shield").unwrap_or_default(),
            imposs_friend_shield: s.read_real("imposs. friend shield").unwrap_or_default(),
            easy_friend_recharge: s.read_real("easy friend recharge").unwrap_or_default(),
            normal_friend_recharge: s.read_real("normal friend recharge").unwrap_or_default(),
            hard_friend_recharge: s.read_real("hard friend recharge").unwrap_or_default(),
            imposs_friend_recharge: s.read_real("imposs. friend recharge").unwrap_or_default(),
            easy_infection_forms: s.read_real("easy infection forms").unwrap_or_default(),
            normal_infection_forms: s.read_real("normal infection forms").unwrap_or_default(),
            hard_infection_forms: s.read_real("hard infection forms").unwrap_or_default(),
            imposs_infection_forms: s.read_real("imposs. infection forms").unwrap_or_default(),
            easy_rate_of_fire: s.read_real("easy rate of fire").unwrap_or_default(),
            normal_rate_of_fire: s.read_real("normal rate of fire").unwrap_or_default(),
            hard_rate_of_fire: s.read_real("hard rate of fire").unwrap_or_default(),
            imposs_rate_of_fire: s.read_real("imposs. rate of fire").unwrap_or_default(),
            easy_projectile_error: s.read_real("easy projectile error").unwrap_or_default(),
            normal_projectile_error: s.read_real("normal projectile error").unwrap_or_default(),
            hard_projectile_error: s.read_real("hard projectile error").unwrap_or_default(),
            imposs_projectile_error: s.read_real("imposs. projectile error").unwrap_or_default(),
            easy_burst_error: s.read_real("easy burst error").unwrap_or_default(),
            normal_burst_error: s.read_real("normal burst error").unwrap_or_default(),
            hard_burst_error: s.read_real("hard burst error").unwrap_or_default(),
            imposs_burst_error: s.read_real("imposs. burst error").unwrap_or_default(),
            easy_new_target_delay: s.read_real("easy new target delay").unwrap_or_default(),
            normal_new_target_delay: s.read_real("normal new target delay").unwrap_or_default(),
            hard_new_target_delay: s.read_real("hard new target delay").unwrap_or_default(),
            imposs_new_target_delay: s.read_real("imposs. new target delay").unwrap_or_default(),
            easy_burst_separation: s.read_real("easy burst separation").unwrap_or_default(),
            normal_burst_separation: s.read_real("normal burst separation").unwrap_or_default(),
            hard_burst_separation: s.read_real("hard burst separation").unwrap_or_default(),
            imposs_burst_separation: s.read_real("imposs. burst separation").unwrap_or_default(),
            easy_target_tracking: s.read_real("easy target tracking").unwrap_or_default(),
            normal_target_tracking: s.read_real("normal target tracking").unwrap_or_default(),
            hard_target_tracking: s.read_real("hard target tracking").unwrap_or_default(),
            imposs_target_tracking: s.read_real("imposs. target tracking").unwrap_or_default(),
            easy_target_leading: s.read_real("easy target leading").unwrap_or_default(),
            normal_target_leading: s.read_real("normal target leading").unwrap_or_default(),
            hard_target_leading: s.read_real("hard target leading").unwrap_or_default(),
            imposs_target_leading: s.read_real("imposs. target leading").unwrap_or_default(),
            easy_overcharge_chance: s.read_real("easy overcharge chance").unwrap_or_default(),
            normal_overcharge_chance: s.read_real("normal overcharge chance").unwrap_or_default(),
            hard_overcharge_chance: s.read_real("hard overcharge chance").unwrap_or_default(),
            imposs_overcharge_chance: s.read_real("imposs. overcharge chance").unwrap_or_default(),
            easy_special_fire_delay: s.read_real("easy special fire delay").unwrap_or_default(),
            normal_special_fire_delay: s.read_real("normal special fire delay").unwrap_or_default(),
            hard_special_fire_delay: s.read_real("hard special fire delay").unwrap_or_default(),
            imposs_special_fire_delay: s.read_real("imposs. special fire delay").unwrap_or_default(),
            easy_guidance_vs_player: s.read_real("easy guidance vs player").unwrap_or_default(),
            normal_guidance_vs_player: s.read_real("normal guidance vs player").unwrap_or_default(),
            hard_guidance_vs_player: s.read_real("hard guidance vs player").unwrap_or_default(),
            imposs_guidance_vs_player: s.read_real("imposs. guidance vs player").unwrap_or_default(),
            easy_melee_delay_base: s.read_real("easy melee delay base").unwrap_or_default(),
            normal_melee_delay_base: s.read_real("normal melee delay base").unwrap_or_default(),
            hard_melee_delay_base: s.read_real("hard melee delay base").unwrap_or_default(),
            imposs_melee_delay_base: s.read_real("imposs. melee delay base").unwrap_or_default(),
            easy_melee_delay_scale: s.read_real("easy melee delay scale").unwrap_or_default(),
            normal_melee_delay_scale: s.read_real("normal melee delay scale").unwrap_or_default(),
            hard_melee_delay_scale: s.read_real("hard melee delay scale").unwrap_or_default(),
            imposs_melee_delay_scale: s.read_real("imposs. melee delay scale").unwrap_or_default(),
            easy_grenade_chance_scale: s.read_real("easy grenade chance scale").unwrap_or_default(),
            normal_grenade_chance_scale: s.read_real("normal grenade chance scale").unwrap_or_default(),
            hard_grenade_chance_scale: s.read_real("hard grenade chance scale").unwrap_or_default(),
            imposs_grenade_chance_scale: s.read_real("imposs. grenade chance scale").unwrap_or_default(),
            easy_grenade_timer_scale: s.read_real("easy grenade timer scale").unwrap_or_default(),
            normal_grenade_timer_scale: s.read_real("normal grenade timer scale").unwrap_or_default(),
            hard_grenade_timer_scale: s.read_real("hard grenade timer scale").unwrap_or_default(),
            imposs_grenade_timer_scale: s.read_real("imposs. grenade timer scale").unwrap_or_default(),
            easy_major_upgrade_normal: s.read_real("easy major upgrade (normal)").unwrap_or_default(),
            normal_major_upgrade_normal: s.read_real("normal major upgrade (normal)").unwrap_or_default(),
            hard_major_upgrade_normal: s.read_real("hard major upgrade (normal)").unwrap_or_default(),
            imposs_major_upgrade_normal: s.read_real("imposs. major upgrade (normal)").unwrap_or_default(),
            easy_major_upgrade_few: s.read_real("easy major upgrade (few)").unwrap_or_default(),
            normal_major_upgrade_few: s.read_real("normal major upgrade (few)").unwrap_or_default(),
            hard_major_upgrade_few: s.read_real("hard major upgrade (few)").unwrap_or_default(),
            imposs_major_upgrade_few: s.read_real("imposs. major upgrade (few)").unwrap_or_default(),
            easy_major_upgrade_many: s.read_real("easy major upgrade (many)").unwrap_or_default(),
            normal_major_upgrade_many: s.read_real("normal major upgrade (many)").unwrap_or_default(),
            hard_major_upgrade_many: s.read_real("hard major upgrade (many)").unwrap_or_default(),
            imposs_major_upgrade_many: s.read_real("imposs. major upgrade (many)").unwrap_or_default(),
            easy_player_vehicle_ram_chance: s.read_real("easy player vehicle ram chance").unwrap_or_default(),
            normal_player_vehicle_ram_chance: s.read_real("normal player vehicle ram chance").unwrap_or_default(),
            hard_player_vehicle_ram_chance: s.read_real("hard player vehicle ram chance").unwrap_or_default(),
            imposs_player_vehicle_ram_chance: s.read_real("imposs. player vehicle ram chance").unwrap_or_default(),
        }
    }
}

/// `grenades_block`.
#[derive(Debug, Clone, Default)]
pub struct Grenades {
    pub maximum_count: i16,
    pub throwing_effect: String,
    pub equipment: String,
    pub projectile: String,
}

impl Grenades {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            maximum_count: s.read_int_any("maximum count").unwrap_or_default() as i16,
            throwing_effect: s.read_tag_ref_path("throwing effect").unwrap_or_default(),
            equipment: s.read_tag_ref_path("equipment").unwrap_or_default(),
            projectile: s.read_tag_ref_path("projectile").unwrap_or_default(),
        }
    }
}

/// `interface_tag_references`.
#[derive(Debug, Clone, Default)]
pub struct InterfaceTagReferences {
    pub dialog_color_table: String,
    pub mainmenu_ui_globals: String,
    pub singleplayer_ui_globals: String,
    pub multiplayer_ui_globals: String,
    pub chud_globals: String,
}

impl InterfaceTagReferences {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            dialog_color_table: s.read_tag_ref_path("dialog color table").unwrap_or_default(),
            mainmenu_ui_globals: s.read_tag_ref_path("mainmenu ui globals").unwrap_or_default(),
            singleplayer_ui_globals: s.read_tag_ref_path("singleplayer ui globals").unwrap_or_default(),
            multiplayer_ui_globals: s.read_tag_ref_path("multiplayer ui globals").unwrap_or_default(),
            chud_globals: s.read_tag_ref_path("chud globals").unwrap_or_default(),
        }
    }
}

/// `player_information_block`.
#[derive(Debug, Clone, Default)]
pub struct PlayerInformation {
    pub walking_speed: f32,
    pub run_forward: f32,
    pub run_backward: f32,
    pub run_sideways: f32,
    pub run_acceleration: f32,
    pub sneak_forward: f32,
    pub sneak_backward: f32,
    pub sneak_sideways: f32,
    pub sneak_acceleration: f32,
    pub airborne_acceleration: f32,
    pub grenade_origin: crate::math::RealPoint3d,
    pub stun_movement_penalty: f32,
    pub stun_turning_penalty: f32,
    pub stun_jumping_penalty: f32,
    pub minimum_stun_time: f32,
    pub maximum_stun_time: f32,
    pub first_person_idle_time: crate::math::RealBounds,
    pub first_person_skip_fraction: f32,
    pub coop_countdown_sound: String,
    pub coop_respawn_sound: String,
    pub coop_respawn_effect: String,
    pub binoculars_zoom_count: i32,
    pub binoculars_zoom_range: crate::math::RealBounds,
    pub flashlight_on: String,
    pub flashlight_off: String,
    pub default_damage_response: String,
}

impl PlayerInformation {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            walking_speed: s.read_real("walking speed").unwrap_or_default(),
            run_forward: s.read_real("run forward").unwrap_or_default(),
            run_backward: s.read_real("run backward").unwrap_or_default(),
            run_sideways: s.read_real("run sideways").unwrap_or_default(),
            run_acceleration: s.read_real("run acceleration").unwrap_or_default(),
            sneak_forward: s.read_real("sneak forward").unwrap_or_default(),
            sneak_backward: s.read_real("sneak backward").unwrap_or_default(),
            sneak_sideways: s.read_real("sneak sideways").unwrap_or_default(),
            sneak_acceleration: s.read_real("sneak acceleration").unwrap_or_default(),
            airborne_acceleration: s.read_real("airborne acceleration").unwrap_or_default(),
            grenade_origin: s.read_point3d("grenade origin"),
            stun_movement_penalty: s.read_real("stun movement penalty").unwrap_or_default(),
            stun_turning_penalty: s.read_real("stun turning penalty").unwrap_or_default(),
            stun_jumping_penalty: s.read_real("stun jumping penalty").unwrap_or_default(),
            minimum_stun_time: s.read_real("minimum stun time").unwrap_or_default(),
            maximum_stun_time: s.read_real("maximum stun time").unwrap_or_default(),
            first_person_idle_time: s.read_real_bounds("first person idle time"),
            first_person_skip_fraction: s.read_real("first person skip fraction").unwrap_or_default(),
            coop_countdown_sound: s.read_tag_ref_path("coop countdown sound").unwrap_or_default(),
            coop_respawn_sound: s.read_tag_ref_path("coop respawn sound").unwrap_or_default(),
            coop_respawn_effect: s.read_tag_ref_path("coop respawn effect").unwrap_or_default(),
            binoculars_zoom_count: s.read_int_any("binoculars zoom count").unwrap_or_default() as i32,
            binoculars_zoom_range: s.read_real_bounds("binoculars zoom range"),
            flashlight_on: s.read_tag_ref_path("flashlight on").unwrap_or_default(),
            flashlight_off: s.read_tag_ref_path("flashlight off").unwrap_or_default(),
            default_damage_response: s.read_tag_ref_path("default damage response").unwrap_or_default(),
        }
    }
}

/// `player_representation_block`.
#[derive(Debug, Clone, Default)]
pub struct PlayerRepresentation {
    pub name: String,
    pub model_choice: Enum<PlayerModelChoice, i8>,
    pub class: Enum<PlayerRepresentationClass, i8>,
    pub first_person_hands: String,
    pub first_person_body: String,
    pub third_person_unit: String,
    pub third_person_variant: String,
    pub binoculars_zoom_in_sound: String,
    pub binoculars_zoom_out_sounds: String,
}

impl PlayerRepresentation {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            model_choice: s.try_read_enum("model choice").unwrap_or_default(),
            class: s.try_read_enum("class").unwrap_or_default(),
            first_person_hands: s.read_tag_ref_path("first person hands").unwrap_or_default(),
            first_person_body: s.read_tag_ref_path("first person body").unwrap_or_default(),
            third_person_unit: s.read_tag_ref_path("third person unit").unwrap_or_default(),
            third_person_variant: s.read_string_id("third person variant").unwrap_or_default(),
            binoculars_zoom_in_sound: s.read_tag_ref_path("binoculars zoom in sound").unwrap_or_default(),
            binoculars_zoom_out_sounds: s.read_tag_ref_path("binoculars zoom out sounds").unwrap_or_default(),
        }
    }
}

/// `falling_damage_block`.
#[derive(Debug, Clone, Default)]
pub struct FallingDamage {
    pub harmful_falling_distance: crate::math::RealBounds,
    pub falling_damage: String,
    pub jumping_damage: String,
    pub soft_landing_damage: String,
    pub hard_landing_damage: String,
    pub hs_damage: String,
    pub maximum_falling_distance: f32,
    pub distance_damage: String,
    pub runtime_maximum_falling_velocity: f32,
    pub runtime_damage_velocity_bounds: crate::math::RealBounds,
}

impl FallingDamage {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            harmful_falling_distance: s.read_real_bounds("harmful falling distance"),
            falling_damage: s.read_tag_ref_path("falling damage").unwrap_or_default(),
            jumping_damage: s.read_tag_ref_path("jumping damage").unwrap_or_default(),
            soft_landing_damage: s.read_tag_ref_path("soft landing damage").unwrap_or_default(),
            hard_landing_damage: s.read_tag_ref_path("hard landing damage").unwrap_or_default(),
            hs_damage: s.read_tag_ref_path("hs damage").unwrap_or_default(),
            maximum_falling_distance: s.read_real("maximum falling distance").unwrap_or_default(),
            distance_damage: s.read_tag_ref_path("distance damage").unwrap_or_default(),
            runtime_maximum_falling_velocity: s.read_real("runtime_maximum_falling_velocity").unwrap_or_default(),
            runtime_damage_velocity_bounds: s.read_real_bounds("runtime_damage_velocity bounds"),
        }
    }
}

/// `material_physics_drag_properties_block`.
#[derive(Debug, Clone, Default)]
pub struct MaterialPhysicsDragProperties {
    pub drag_k: f32,
    pub drag_q: f32,
    pub drag_e: f32,
    pub super_floater: f32,
    pub floater: f32,
    pub neutral: f32,
    pub sinker: f32,
    pub super_sinker: f32,
}

impl MaterialPhysicsDragProperties {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            drag_k: s.read_real("drag k").unwrap_or_default(),
            drag_q: s.read_real("drag q").unwrap_or_default(),
            drag_e: s.read_real("drag e").unwrap_or_default(),
            super_floater: s.read_real("super floater").unwrap_or_default(),
            floater: s.read_real("floater").unwrap_or_default(),
            neutral: s.read_real("neutral").unwrap_or_default(),
            sinker: s.read_real("sinker").unwrap_or_default(),
            super_sinker: s.read_real("super sinker").unwrap_or_default(),
        }
    }
}

/// `material_physics_properties_struct`.
#[derive(Debug, Clone, Default)]
pub struct MaterialPhysicsProperties {
    pub flags: i32,
    pub friction: f32,
    pub restitution: f32,
    pub density: f32,
    pub drag: Vec<MaterialPhysicsDragProperties>,
}

impl MaterialPhysicsProperties {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            flags: s.read_int_any("flags").unwrap_or_default() as i32,
            friction: s.read_real("friction").unwrap_or_default(),
            restitution: s.read_real("restitution").unwrap_or_default(),
            density: s.read_real("density").unwrap_or_default(),
            drag: read_block_vec(s, "drag", MaterialPhysicsDragProperties::from_struct),
        }
    }
}

/// `materials_sweeteners_struct`.
#[derive(Debug, Clone, Default)]
pub struct MaterialsSweeteners {
    pub sound_sweetener_small: String,
    pub sound_sweetener_medium: String,
    pub sound_sweetener_large: String,
    pub sound_sweetener_rolling: String,
    pub sound_sweetener_grinding: String,
    pub sound_sweetener_melee_small: String,
    pub sound_sweetener_melee: String,
    pub sound_sweetener_melee_large: String,
    pub effect_sweetener_small: String,
    pub effect_sweetener_medium: String,
    pub effect_sweetener_large: String,
    pub effect_sweetener_rolling: String,
    pub effect_sweetener_grinding: String,
    pub effect_sweetener_melee: String,
    pub water_ripple_small: String,
    pub water_ripple_medium: String,
    pub water_ripple_large: String,
    pub sweetener_inheritance_flags: Flags<MaterialsSweetenersInheritanceFlags, u32>,
}

impl MaterialsSweeteners {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            sound_sweetener_small: s.read_tag_ref_path("sound sweetener (small)").unwrap_or_default(),
            sound_sweetener_medium: s.read_tag_ref_path("sound sweetener (medium)").unwrap_or_default(),
            sound_sweetener_large: s.read_tag_ref_path("sound sweetener (large)").unwrap_or_default(),
            sound_sweetener_rolling: s.read_tag_ref_path("sound sweetener rolling").unwrap_or_default(),
            sound_sweetener_grinding: s.read_tag_ref_path("sound sweetener grinding").unwrap_or_default(),
            sound_sweetener_melee_small: s.read_tag_ref_path("sound sweetener (melee small)").unwrap_or_default(),
            sound_sweetener_melee: s.read_tag_ref_path("sound sweetener (melee)").unwrap_or_default(),
            sound_sweetener_melee_large: s.read_tag_ref_path("sound sweetener (melee large)").unwrap_or_default(),
            effect_sweetener_small: s.read_tag_ref_path("effect sweetener (small)").unwrap_or_default(),
            effect_sweetener_medium: s.read_tag_ref_path("effect sweetener (medium)").unwrap_or_default(),
            effect_sweetener_large: s.read_tag_ref_path("effect sweetener (large)").unwrap_or_default(),
            effect_sweetener_rolling: s.read_tag_ref_path("effect sweetener rolling").unwrap_or_default(),
            effect_sweetener_grinding: s.read_tag_ref_path("effect sweetener grinding").unwrap_or_default(),
            effect_sweetener_melee: s.read_tag_ref_path("effect sweetener (melee)").unwrap_or_default(),
            water_ripple_small: s.read_tag_ref_path("water ripple (small)").unwrap_or_default(),
            water_ripple_medium: s.read_tag_ref_path("water ripple (medium)").unwrap_or_default(),
            water_ripple_large: s.read_tag_ref_path("water ripple (large)").unwrap_or_default(),
            sweetener_inheritance_flags: s.try_read_flags("sweetener inheritance flags").unwrap_or_default(),
        }
    }
}

/// `underwater_proxies_block`.
#[derive(Debug, Clone, Default)]
pub struct UnderwaterProxies {
    pub underwater_material: String,
    pub proxy_material: String,
    pub underwater_material_type: i16,
    pub proxy_material_type: i16,
}

impl UnderwaterProxies {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            underwater_material: s.read_string_id("underwater material").unwrap_or_default(),
            proxy_material: s.read_string_id("proxy material").unwrap_or_default(),
            underwater_material_type: s.read_int_any("underwater material type").unwrap_or_default() as i16,
            proxy_material_type: s.read_int_any("proxy material type").unwrap_or_default() as i16,
        }
    }
}

/// `materials_block`.
#[derive(Debug, Clone, Default)]
pub struct Materials {
    pub name: String,
    pub parent_name: String,
    pub runtime_material_index: i16,
    pub flags: Flags<GlobalMaterialFlags, u16>,
    pub general_armor: String,
    pub specific_armor: String,
    pub physics_properties: MaterialPhysicsProperties,
    pub breakable_surface: String,
    pub sweeteners: MaterialsSweeteners,
    pub material_effects: String,
    pub underwater_proxies: Vec<UnderwaterProxies>,
}

impl Materials {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            name: s.read_string_id("name").unwrap_or_default(),
            parent_name: s.read_string_id("parent name").unwrap_or_default(),
            runtime_material_index: s.read_int_any("runtime material index").unwrap_or_default() as i16,
            flags: s.try_read_flags("flags").unwrap_or_default(),
            general_armor: s.read_string_id("general armor").unwrap_or_default(),
            specific_armor: s.read_string_id("specific armor").unwrap_or_default(),
            physics_properties: MaterialPhysicsProperties::from_struct(
                &s.field("physics properties").and_then(|f| f.as_struct()).unwrap_or_else(|| s.clone()),
            ),
            breakable_surface: s.read_tag_ref_path("breakable surface").unwrap_or_default(),
            sweeteners: MaterialsSweeteners::from_struct(
                &s.field("sweeteners").and_then(|f| f.as_struct()).unwrap_or_else(|| s.clone()),
            ),
            material_effects: s.read_tag_ref_path("material effects").unwrap_or_default(),
            underwater_proxies: read_block_vec(s, "underwater proxies", UnderwaterProxies::from_struct),
        }
    }
}

/// `multiplayer_color_block`.
#[derive(Debug, Clone, Default)]
pub struct MultiplayerColor {
    pub color: crate::math::RealRgbColor,
}

impl MultiplayerColor {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            color: s.read_rgb("color"),
        }
    }
}

/// `cinematics_globals_block`.
#[derive(Debug, Clone, Default)]
pub struct CinematicsGlobals {
    pub cinematic_anchor_reference: String,
    pub cinematic_film_aperture: f32,
}

impl CinematicsGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            cinematic_anchor_reference: s.read_tag_ref_path("cinematic anchor reference").unwrap_or_default(),
            cinematic_film_aperture: s.read_real("cinematic film aperture").unwrap_or_default(),
        }
    }
}

/// `campaign_metagame_style_type_block`.
#[derive(Debug, Clone, Default)]
pub struct CampaignMetagameStyleType {
    pub style_multiplier: f32,
}

impl CampaignMetagameStyleType {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            style_multiplier: s.read_real("style multiplier").unwrap_or_default(),
        }
    }
}

/// `campaign_metagame_difficulty_scale_block`.
#[derive(Debug, Clone, Default)]
pub struct CampaignMetagameDifficultyScale {
    pub difficulty_multiplier: f32,
}

impl CampaignMetagameDifficultyScale {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            difficulty_multiplier: s.read_real("difficulty multiplier").unwrap_or_default(),
        }
    }
}

/// `campaign_metagame_primary_skull_block`.
#[derive(Debug, Clone, Default)]
pub struct CampaignMetagamePrimarySkull {
    pub difficulty_multiplier: f32,
}

impl CampaignMetagamePrimarySkull {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            difficulty_multiplier: s.read_real("difficulty multiplier").unwrap_or_default(),
        }
    }
}

/// `campaign_metagame_secondary_skull_block`.
#[derive(Debug, Clone, Default)]
pub struct CampaignMetagameSecondarySkull {
    pub difficulty_multiplier: f32,
}

impl CampaignMetagameSecondarySkull {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            difficulty_multiplier: s.read_real("difficulty multiplier").unwrap_or_default(),
        }
    }
}

/// `campaign_metagame_globals_block`.
#[derive(Debug, Clone, Default)]
pub struct CampaignMetagameGlobals {
    pub styles: Vec<CampaignMetagameStyleType>,
    pub difficulty: Vec<CampaignMetagameDifficultyScale>,
    pub primary_skulls: Vec<CampaignMetagamePrimarySkull>,
    pub secondary_skulls: Vec<CampaignMetagameSecondarySkull>,
    pub friendly_death_point_count: i32,
    pub player_death_point_count: i32,
    pub player_betrayal_point_count: i32,
    pub multi_kill_count: i32,
    pub multi_kill_window: f32,
    pub emp_kill_window: f32,
}

impl CampaignMetagameGlobals {
    fn from_struct(s: &TagStruct<'_>) -> Self {
        Self {
            styles: read_block_vec(s, "styles", CampaignMetagameStyleType::from_struct),
            difficulty: read_block_vec(s, "difficulty", CampaignMetagameDifficultyScale::from_struct),
            primary_skulls: read_block_vec(s, "Primary Skulls", CampaignMetagamePrimarySkull::from_struct),
            secondary_skulls: read_block_vec(s, "Secondary Skulls", CampaignMetagameSecondarySkull::from_struct),
            friendly_death_point_count: s.read_int_any("friendly_death_point_count").unwrap_or_default() as i32,
            player_death_point_count: s.read_int_any("player_death_point_count").unwrap_or_default() as i32,
            player_betrayal_point_count: s.read_int_any("player_betrayal_point_count").unwrap_or_default() as i32,
            multi_kill_count: s.read_int_any("multi kill count").unwrap_or_default() as i32,
            multi_kill_window: s.read_real("multi kill window").unwrap_or_default(),
            emp_kill_window: s.read_real("EMP kill window").unwrap_or_default(),
        }
    }
}

//================================================================================
// Root: GameGlobals + from_tag
//================================================================================

/// Errors from `globals` (matg) tag walking.
#[derive(Debug)]
pub enum GameGlobalsError {
    /// The tag's group FOURCC was not `matg`.
    WrongGroup { actual: [u8; 4] },
}

impl std::fmt::Display for GameGlobalsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongGroup { actual } => write!(
                f,
                "tag group '{}' is not 'matg' — not a game_globals tag",
                std::str::from_utf8(actual).unwrap_or("?"),
            ),
        }
    }
}

impl std::error::Error for GameGlobalsError {}

/// Root `globals_struct_definition`.
#[derive(Debug, Clone, Default)]
pub struct GameGlobals {
    pub language: Enum<Language, i32>,
    pub language_packs: [LanguagePack; 12],
    pub havok_cleanup_resources: Vec<HavokCleanupResources>,
    pub sound_globals: Vec<SoundGlobals>,
    pub ai_globals: Vec<AiGlobals>,
    pub damage_table: Vec<GameGlobalsDamage>,
    pub camera: Vec<Camera>,
    pub player_control: Vec<PlayerControl>,
    pub difficulty: Vec<Difficulty>,
    pub grenades: Vec<Grenades>,
    pub interface_tags: Vec<InterfaceTagReferences>,
    pub player_information: Vec<PlayerInformation>,
    pub player_representation: Vec<PlayerRepresentation>,
    pub player_representation_debug: Vec<PlayerRepresentation>,
    pub falling_damage: Vec<FallingDamage>,
    pub materials: Vec<Materials>,
    pub profile_colors: Vec<MultiplayerColor>,
    pub cinematics_globals: Vec<CinematicsGlobals>,
    pub campaign_metagame_globals: Vec<CampaignMetagameGlobals>,
    pub global_water_material: String,
    pub global_water_material_type: i16,
    pub multiplayer_globals: String,
    pub survival_mode_globals: String,
    pub rasterizer_globals_ref: String,
    pub default_camera_fx_settings: String,
    pub default_wind_settings: String,
    pub collision_damage_effect: String,
    pub collision_damage: String,
    pub effect_globals: String,
    pub render_object_skins: String,
}

impl GameGlobals {
    /// Walk a parsed `globals` (matg) tag into the schema-faithful type tree.
    pub fn from_tag(tag: &TagFile) -> Result<Self, GameGlobalsError> {
        let actual = tag.group().tag.to_be_bytes();
        if &actual != b"matg" {
            return Err(GameGlobalsError::WrongGroup { actual });
        }
        let root = tag.root();
        Ok(Self {
            language: root.try_read_enum("language").unwrap_or_default(),
            language_packs: std::array::from_fn(|i| {
                let key = format!("language pack{}", i + 1);
                LanguagePack::from_struct(
                    &root.field(&key).and_then(|f| f.as_struct()).unwrap_or_else(|| root.clone()),
                )
            }),
            havok_cleanup_resources: read_block_vec(&root, "havok cleanup resources", HavokCleanupResources::from_struct),
            sound_globals: read_block_vec(&root, "sound globals", SoundGlobals::from_struct),
            ai_globals: read_block_vec(&root, "ai globals", AiGlobals::from_struct),
            damage_table: read_block_vec(&root, "damage table", GameGlobalsDamage::from_struct),
            camera: read_block_vec(&root, "camera", Camera::from_struct),
            player_control: read_block_vec(&root, "player control", PlayerControl::from_struct),
            difficulty: read_block_vec(&root, "difficulty", Difficulty::from_struct),
            grenades: read_block_vec(&root, "grenades", Grenades::from_struct),
            interface_tags: read_block_vec(&root, "interface tags", InterfaceTagReferences::from_struct),
            player_information: read_block_vec(&root, "@player information", PlayerInformation::from_struct),
            player_representation: read_block_vec(&root, "@player representation", PlayerRepresentation::from_struct),
            player_representation_debug: read_block_vec(&root, "@player representation debug", PlayerRepresentation::from_struct),
            falling_damage: read_block_vec(&root, "falling damage", FallingDamage::from_struct),
            materials: read_block_vec(&root, "materials", Materials::from_struct),
            profile_colors: read_block_vec(&root, "profile colors", MultiplayerColor::from_struct),
            cinematics_globals: read_block_vec(&root, "cinematics globals", CinematicsGlobals::from_struct),
            campaign_metagame_globals: read_block_vec(&root, "campaign metagame globals", CampaignMetagameGlobals::from_struct),
            global_water_material: root.read_string_id("global water material").unwrap_or_default(),
            global_water_material_type: root.read_int_any("global water material type").unwrap_or_default() as i16,
            multiplayer_globals: root.read_tag_ref_path("multiplayer globals").unwrap_or_default(),
            survival_mode_globals: root.read_tag_ref_path("survival mode globals").unwrap_or_default(),
            rasterizer_globals_ref: root.read_tag_ref_path("rasterizer_globals_ref").unwrap_or_default(),
            default_camera_fx_settings: root.read_tag_ref_path("default camera fx settings").unwrap_or_default(),
            default_wind_settings: root.read_tag_ref_path("default wind settings").unwrap_or_default(),
            collision_damage_effect: root.read_tag_ref_path("collision damage effect").unwrap_or_default(),
            collision_damage: root.read_tag_ref_path("collision damage").unwrap_or_default(),
            effect_globals: root.read_tag_ref_path("effect globals").unwrap_or_default(),
            render_object_skins: root.read_tag_ref_path("render object skins").unwrap_or_default(),
        })
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

//================================================================================
// Tests
//================================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use strum::VariantArray;

    #[test]
    fn enum_defaults() {
        assert_eq!(Language::default(), Language::English);
        assert_eq!(GlobalTransitionFunctions::default(), GlobalTransitionFunctions::Linear);
        assert_eq!(PlayerModelChoice::default(), PlayerModelChoice::Spartan);
        assert_eq!(PlayerRepresentationClass::default(), PlayerRepresentationClass::Campaign);
    }

    #[test]
    fn strum_roundtrips() {
        // IntoStaticStr yields the exact schema option string.
        let s: &'static str = Language::MexicanSpanish.into();
        assert_eq!(s, "mexican spanish");
        let s: &'static str = GlobalMaterialFlags::RadXferInterior.into();
        assert_eq!(s, "rad xfer interior");
        let s: &'static str = MaterialsSweetenersInheritanceFlags::WaterRippleLarge.into();
        assert_eq!(s, "water_ripple_large");
        // EnumString parses the schema option string (case-insensitive) back.
        assert_eq!(
            "chinese-traditional".parse::<Language>().unwrap(),
            Language::ChineseTraditional
        );
        assert_eq!(
            "VERY LATE".parse::<GlobalTransitionFunctions>().unwrap(),
            GlobalTransitionFunctions::VeryLate
        );
        assert_eq!(
            "elite".parse::<PlayerModelChoice>().unwrap(),
            PlayerModelChoice::Elite
        );
    }

    #[test]
    fn variant_counts_match_schema() {
        assert_eq!(Language::VARIANTS.len(), 12);
        assert_eq!(GlobalTransitionFunctions::VARIANTS.len(), 8);
        assert_eq!(PlayerModelChoice::VARIANTS.len(), 2);
        assert_eq!(PlayerRepresentationClass::VARIANTS.len(), 4);
        assert_eq!(GlobalMaterialFlags::VARIANTS.len(), 3);
        assert_eq!(MaterialsSweetenersInheritanceFlags::VARIANTS.len(), 17);
    }
}
