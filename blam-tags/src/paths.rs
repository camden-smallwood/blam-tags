//! Filesystem helpers for tag-reference resolution. Halo stores
//! cross-tag links as backslash-separated relative paths
//! (`objects\characters\masterchief`); turning one into a real
//! `PathBuf` needs (a) the `tags/` ancestor of the input as a root
//! and (b) the target group's extension appended on the end.

use std::path::{Path, PathBuf};

use crate::fields::TagFieldData;
use crate::TagStruct;

/// Find the `tags/` ancestor of `path` and return everything up to
/// and including it. Returns `None` if `path` doesn't canonicalize
/// or no `tags/` component is found.
pub fn derive_tags_root(path: &Path) -> Option<PathBuf> {
    let abs = path.canonicalize().ok()?;
    let mut acc = PathBuf::new();
    let mut found = None;
    for component in abs.components() {
        acc.push(component);
        if matches!(component, std::path::Component::Normal(s) if s == "tags") {
            found = Some(acc.clone());
        }
    }
    found
}

/// Extract a tag file's stem (filename without extension) for
/// output-path construction. Falls back to `default` for paths
/// without a usable stem.
pub fn tag_stem(path: &Path, default: &str) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or(default).to_owned()
}

/// Read a `tag_reference` field's relative path, dropping null/empty
/// references. Accepts a `/`-separated path so it transparently
/// handles tag-reference fields nested inside inherited parent
/// structs (e.g. `object/model` on a `.crate` whose `obje` parent is
/// inlined as a named substruct).
pub fn tag_ref_path(s: &TagStruct<'_>, path: &str) -> Option<String> {
    let v = s.field_path(path)?.value()?;
    let TagFieldData::TagReference(r) = v else { return None; };
    let (_, p) = r.group_tag_and_name?;
    if p.is_empty() { None } else { Some(p) }
}

/// Resolve a Halo-style relative tag path (`objects\foo\bar`) against
/// an absolute `tags_root` and stamp on the target group's extension.
///
/// Always APPENDS `.{ext}` to the filename — never replaces an existing
/// dot-suffix. Authoring names can contain literal dots (e.g.
/// `decal_road_stripe_short_1.bitmap`) and the engine's tag-ref
/// resolver appends the group extension without stripping, producing
/// on-disk files like `..._1.bitmap.decal_system`. Using
/// `Path::set_extension` here would strip the `.bitmap` portion and
/// miss those files.
pub fn resolve_tag_path(tags_root: &Path, rel: &str, ext: &str) -> PathBuf {
    let rel_path: PathBuf = rel.split('\\').collect();
    let p = tags_root.join(&rel_path);
    let mut s = p.into_os_string();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

/// The tag path for a path relative to a `tags` folder: its components
/// joined with `\\`, the separator Halo tag paths use on every host. The
/// inverse of [`resolve_tag_path`]; a file's extension is kept, so strip it
/// first when the result should name the tag rather than its file.
pub fn relative_tag_path(rel: &Path) -> String {
    rel.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("\\")
}

/// Map a tag group FOURCC (big-endian u32 such as `rmtr`) to the
/// matching on-disk file extension. Returns `None` for unknown
/// groups — caller falls back to a sensible default (typically
/// `"shader"` for the render-method family).
///
/// Halo's tag-ref payload stores the group FOURCC alongside the
/// path, but the path itself is extension-less. Composing the right
/// extension is essential for non-`rmsh` shader chains (terrain,
/// water, foliage, decal, etc.). Source: MCC tag dump filenames.
pub fn group_tag_to_extension(group: u32) -> Option<&'static str> {
    // Combined group-FOURCC -> on-disk-extension table, generated from the
    // tag_index of EVERY MCC title's definitions/<game>/_meta.json (H3 priority
    // on the rare cross-game FOURCC collision). Regenerate by merging those
    // tables if a new group is added. Hand-maintaining a partial list is what
    // made the mainmenu storm (device_machine) resolve to a missing .machine.
    let fourcc = group.to_be_bytes();
    Some(match &fourcc {
        b"$#!+" => "cache_file_sound",
        b"*cen" => "scenario_scenery_resource",
        b"*eap" => "scenario_weapons_resource",
        b"*ehi" => "scenario_vehicles_resource",
        b"*fsc" => "scenario_effect_scenery_resource",
        b"*igh" => "scenario_lights_resource",
        b"*ipd" => "scenario_bipeds_resource",
        b"*qip" => "scenario_equipment_resource",
        b"*rea" => "scenario_creature_resource",
        b"*sce" => "scenario_sound_scenery_resource",
        b"/**/" => "scenario_comments_resource",
        b"<fx>" => "sound_effect_template",
        b"?rmc" => "shader_contrail",
        b"?rmp" => "shader_particle",
        b"BooM" => "stereo_system",
        b"DECP" => "decorators",
        b"DECR" => "decorator_set",
        b"DeLa" => "ui_widget_definition",
        b"LMMg" => "lightmap_model_globals",
        b"LMgS" => "lightmapper_globals",
        b"Lbsp" => "scenario_lightmap_bsp_data",
        b"MGS2" => "light_volume",
        b"PRTM" => "particle_model",
        b"SDzs" => "streamingzoneset",
        b"Soul" => "ui_widget_collection",
        b"ZZAP" => "lightning_system",
        b"achi" => "achievements",
        b"actr" => "actor",
        b"actv" => "actor_variant",
        b"adlg" => "ai_dialogue_globals",
        b"ai**" => "scenario_ai_resource",
        b"aigl" => "ai_globals",
        b"airs" => "airstrike",
        b"ant!" => "antenna",
        b"antr" => "model_animations",
        b"argd" => "device_arg_device",
        b"armg" => "armormod_globals",
        b"assd" => "asset_depot_storage",
        b"atgf" => "atmosphere_globals",
        b"aulp" => "authored_light_probe",
        b"avat" => "avatar_awards",
        b"bbcr" => "big_battle_creature",
        b"bdpd" => "death_program_selector",
        b"beam" => "beam_system",
        b"bink" => "bink",
        b"bipd" => "biped",
        b"bitm" => "bitmap",
        b"bkey" => "gui_button_key_definition",
        b"bloc" => "crate",
        b"bmp3" => "gui_bitmap_widget_definition",
        b"bsdt" => "breakable_surface",
        b"capg" => "custom_app_globals",
        b"cddf" => "collision_damage",
        b"cdmg" => "continuous_damage_effect",
        b"cfxs" => "camera_fx_settings",
        b"chad" => "chud_animation_definition",
        b"char" => "character",
        b"chdg" => "challenge_globals_definition",
        b"chdt" => "chud_definition",
        b"chgd" => "chud_globals_definition",
        b"chmt" => "chocolate_mountain_new",
        b"cin*" => "scenario_cinematics_resource",
        b"cine" => "cinematic",
        b"cisc" => "cinematic_scene",
        b"cisd" => "cinematic_scene_data",
        b"citr" => "cinematic_transition",
        b"clu*" => "scenario_cluster_data_resource",
        b"clwd" => "cloth",
        b"cmoe" => "camo",
        b"cmps" => "compute_shader",
        b"cmpu" => "compute_shader",
        b"cnmp" => "controller_mapping",
        b"cntl" => "contrail_system",
        b"coag" => "commendation_aggregator_list",
        b"coll" => "collision_model",
        b"coln" => "colony",
        b"colo" => "color_table",
        b"comg" => "commendation_globals_definition",
        b"coms" => "communication_sounds",
        b"cont" => "contrail",
        b"cook" => "reward_globals_definition",
        b"coop" => "coop_spawning_globals_definition",
        b"cpem" => "cheap_particle_emitter",
        b"cpgd" => "cookie_purchase_globals",
        b"cptl" => "cheap_particle_type_library",
        b"crea" => "creature",
        b"crte" => "cortana_effect_definition",
        b"crvs" => "curve_scalar",
        b"csdt" => "camera_shake",
        b"ctrl" => "device_control",
        b"cub*" => "scenario_cubemap_resource",
        b"culo" => "cui_logic",
        b"cusc" => "cui_screen",
        b"cust" => "cui_static_data",
        b"dc*s" => "scenario_decorators_resource",
        b"dctr" => "decorator_set",
        b"dec*" => "scenario_decals_resource",
        b"deca" => "decal",
        b"decs" => "decal_system",
        b"devc" => "input_device_defaults",
        b"devi" => "device",
        b"devo" => "cellular_automata",
        b"dgr*" => "scenario_devices_resource",
        b"dmp*" => "scenario_dumplings_resource",
        b"dobc" => "detail_object_collection",
        b"dpnd" => "dependency",
        b"draw" => "rasterizer_cache_file_globals",
        b"drdf" => "damage_response_definition",
        b"dspn" => "device_dispenser",
        b"dsrc" => "gui_datasource_definition",
        b"ebhd" => "particle_emitter_boat_hull_shape",
        b"effe" => "effect",
        b"effg" => "effect_globals",
        b"efsc" => "effect_scenery",
        b"egfd" => "effect_global_force",
        b"egor" => "screen_effect",
        b"elec" => "lightning",
        b"ents" => "entity",
        b"eqip" => "equipment",
        b"ffgd" => "firefight_globals",
        b"ffgt" => "GameEngineFirefightVariantTag",
        b"flag" => "flag",
        b"flck" => "flock",
        b"fldy" => "fluid_dynamics",
        b"fog " => "planar_fog",
        b"fogg" => "atmosphere_fog",
        b"font" => "font",
        b"foot" => "material_effects",
        b"forg" => "forge_globals",
        b"form" => "formation",
        b"fpch" => "patchy_fog",
        b"frag" => "fragment",
        b"frms" => "frame_event_list",
        b"fwtg" => "user_interface_fourth_wall_timing_definition",
        b"fxtt" => "fx_test",
        b"garb" => "garbage",
        b"gcrg" => "game_completion_rewards_globals",
        b"gegl" => "game_engine_globals",
        b"gggl" => "game_globals_grenade_list",
        b"ggol" => "game_globals_ordnance_list",
        b"gint" => "giant",
        b"gldf" => "cheap_light",
        b"glps" => "global_pixel_shader",
        b"glvs" => "global_vertex_shader",
        b"glw!" => "glow",
        b"gmeg" => "game_medal_globals",
        b"goof" => "multiplayer_variant_settings_interface_definition",
        b"gpdt" => "game_progression",
        b"gpix" => "global_cache_file_pixel_shaders",
        b"gptd" => "game_performance_throttle",
        b"grfr" => "grounded_friction",
        b"grhi" => "grenade_hud_interface",
        b"grup" => "gui_group_widget_definition",
        b"hcfd" => "havok_collision_filter",
        b"hlmt" => "model",
        b"hlsl" => "hlsl_include",
        b"hmt " => "hud_message_text",
        b"hsc*" => "scenario_hs_source_file",
        b"hscn" => "script_container",
        b"hsdt" => "script",
        b"hud#" => "hud_number",
        b"hudg" => "hud_globals",
        b"igpd" => "incident_global_properties_definition",
        b"iimz" => "instance_imposter_definition",
        b"impo" => "imposter_model",
        b"info" => "tag_import_information",
        b"ingd" => "incident_globals_definition",
        b"item" => "item",
        b"itmc" => "item_collection",
        b"iuii" => "InfinityUIImages",
        b"jmad" => "model_animation_graph",
        b"jmrq" => "sandbox_text_value_pair_definition",
        b"jpt!" => "damage_effect",
        b"kccd" => "KillCamCameraParamter",
        b"ldsc" => "load_screen_globals",
        b"lens" => "lens_flare",
        b"lgtd" => "loadout_globals_definition",
        b"licn" => "light_cone",
        b"lifi" => "device_light_fixture",
        b"ligh" => "light",
        b"locs" => "location_name_globals_definition",
        b"lrig" => "light_rig",
        b"lsnd" => "sound_looping",
        b"lst3" => "gui_list_widget_definition",
        b"lswd" => "leaf_system",
        b"ltmp" => "scenario_structure_lightmap",
        b"ltvl" => "light_volume_system",
        b"mach" => "device_machine",
        b"mat " => "material",
        b"matg" => "globals",
        b"mats" => "material_shader",
        b"mcsr" => "mouse_cursor_definition",
        b"mdl3" => "gui_model_widget_definition",
        b"mdlg" => "ai_mission_dialogue",
        b"mdsv" => "model_dissolve_definition",
        b"mech" => "medal_challenge_aggregator_list",
        b"meco" => "medal_commendation_aggregator_list",
        b"metr" => "meter",
        b"mffn" => "muffin",
        b"mgee" => "multiplayerEffects",
        b"mgls" => "megalogamengine_sounds",
        b"mgs2" => "light_volume",
        b"mlib" => "emblem_library",
        b"mmvo" => "main_menu_voiceover",
        b"mod2" => "gbxmodel",
        b"mode" => "render_model",
        b"motl" => "multiplayer_object_type_list",
        b"mpdt" => "material_physics",
        b"mply" => "multiplayer_scenario_description",
        b"msit" => "megalo_string_id_table",
        b"mtsb" => "material_shader_bank",
        b"mulg" => "multiplayer_globals",
        b"muxg" => "mux_generator",
        b"narg" => "NarrativeGlobals",
        b"nclt" => "new_cinematic_lighting",
        b"ngpr" => "preferences_network_game",
        b"nhdt" => "new_hud_definition",
        b"obje" => "object",
        b"paas" => "polyart_asset",
        b"pach" => "tag_package_manifest",
        b"part" => "particle",
        b"patg" => "patch_globals",
        b"pcaa" => "pca_animation",
        b"pcec" => "pgcr_enemy_to_category_mapping_definition",
        b"pctl" => "particle_system",
        b"pdti" => "pgcr_damage_type_image_mapping_definition",
        b"pecp" => "particle_emitter_custom_points",
        b"pegd" => "player_enlistment_globals_definition",
        b"perf" => "performance_throttles",
        b"pfmc" => "performance_template",
        b"pfnd" => "pathfinding",
        b"pfpt" => "planar_fog_parameters",
        b"pggd" => "player_grade_globals_definition",
        b"phmo" => "physics_model",
        b"phys" => "physics",
        b"pixl" => "pixel_shader",
        b"plac" => "placeholder",
        b"play" => "cache_file_resource_layout_table",
        b"pman" => "particleman",
        b"pmcg" => "player_model_customization_globals",
        b"pmdf" => "particle_model",
        b"pmov" => "particle_physics",
        b"pphy" => "point_physics",
        b"ppod" => "portrait_poses_definition",
        b"prf*" => "scenario_performances_resource",
        b"prfb" => "prefab",
        b"prog" => "progression",
        b"proj" => "projectile",
        b"prt3" => "particle",
        b"rain" => "rain_definition",
        b"rasg" => "rasterizer_globals",
        b"rm  " => "render_method",
        b"rmb " => "shader_beam",
        b"rmbk" => "shader_black",
        b"rmbl" => "rumble",
        b"rmcs" => "shader_custom",
        b"rmct" => "shader_cortana",
        b"rmd " => "shader_decal",
        b"rmdf" => "render_method_definition",
        b"rmfl" => "shader_foliage",
        b"rmfs" => "shader_fur_stencil",
        b"rmfu" => "shader_fur",
        b"rmgl" => "shader_glass",
        b"rmhg" => "shader_halogram",
        b"rmla" => "render_model_lightmap_atlas",
        b"rmlv" => "shader_light_volume",
        b"rmmm" => "shader_mux_material",
        b"rmmx" => "shader_mux",
        b"rmop" => "render_method_option",
        b"rmsh" => "shader",
        b"rmsk" => "shader_skin",
        b"rmss" => "shader_screen",
        b"rmt2" => "render_method_template",
        b"rmtr" => "shader_terrain",
        b"rmw " => "shader_water",
        b"rmwf" => "shader_waterfall",
        b"rsod" => "render_skins_object_globals",
        b"rwrd" => "render_water_ripple",
        b"sFdT" => "scenario_faux_data",
        b"sLdT" => "scenario_lightmap",
        b"sadt" => "spring_acceleration",
        b"sbnk" => "soundbank",
        b"sbsp" => "scenario_structure_bsp",
        b"scen" => "scenery",
        b"scex" => "shader_transparent_chicago_extended",
        b"schi" => "shader_transparent_chicago",
        b"scmb" => "sound_combiner",
        b"scn3" => "gui_screen_widget_definition",
        b"scnr" => "scenario",
        b"scol" => "scenario_ordnance_list",
        b"sddt" => "structure_design",
        b"sdzg" => "scenario_required_resource",
        b"sefc" => "area_screen_effect",
        b"senv" => "shader_environment",
        b"sfx+" => "sound_effect_collection",
        b"sgla" => "shader_transparent_glass",
        b"sgp!" => "sound_global_propagation",
        b"sgrp" => "sound_response",
        b"shad" => "shader",
        b"shdr" => "shader",
        b"shit" => "shield_impact",
        b"shvr" => "shared_variables",
        b"sict" => "self_illumination",
        b"sidt" => "simulated_input",
        b"sigd" => "SuppressedIncident",
        b"siin" => "simulation_interpolation",
        b"sily" => "text_value_pair_definition",
        b"sirg" => "sound_incident_response",
        b"sirp" => "scenario_interpolator",
        b"skin" => "user_interface_list_skin_definition",
        b"skn3" => "gui_skin_definition",
        b"sky " => "sky",
        b"sky*" => "scenario_sky_references_resource",
        b"skya" => "sky_atm_parameters",
        b"slag" => "silent_assist_globals",
        b"slit" => "shader_light_response",
        b"smap" => "shared_cache_file_layout",
        b"smdt" => "survival_mode_globals",
        b"smet" => "structure_meta",
        b"sncl" => "sound_classes",
        b"snd!" => "sound",
        b"snde" => "sound_environment",
        b"snmm" => "sound_mix_mastering",
        b"snmx" => "sound_mix",
        b"soso" => "shader_model",
        b"sotr" => "shader_transparent_generic",
        b"spas" => "shader_pass",
        b"spda" => "scenario_pda",
        b"spk!" => "sound_dialogue_constants",
        b"spla" => "shader_transparent_plasma",
        b"spnr" => "spawner",
        b"sqtm" => "squad_template",
        b"srad" => "sound_radio_settings",
        b"ssao" => "ssao_definition",
        b"ssce" => "sound_scenery",
        b"ssdf" => "SpawnSettings",
        b"sslt" => "scenario_structure_lighting_resource",
        b"stem" => "shader_template",
        b"stli" => "scenario_structure_lighting_info",
        b"str#" => "string_list",
        b"stse" => "structure_seams",
        b"styl" => "style",
        b"swat" => "shader_transparent_water",
        b"tag+" => "tag_database",
        b"tagc" => "tag_collection",
        b"tdtl" => "liquid",
        b"term" => "device_terminal",
        b"trac" => "tracer_system",
        b"trak" => "camera_track",
        b"trg*" => "scenario_trigger_volumes_resource",
        b"ttag" => "test_tag",
        b"txt3" => "gui_text_widget_definition",
        b"udlg" => "dialogue",
        b"ugh!" => "sound_cache_file_gestalt",
        b"uihg" => "user_interface_hud_globals_definition",
        b"uise" => "user_interface_sounds_definition",
        b"uiss" => "style_sheet_list",
        b"unhi" => "unit_hud_interface",
        b"unic" => "multilingual_unicode_string_list",
        b"unit" => "unit",
        b"ustr" => "unicode_string_list",
        b"uttt" => "tag_template_unit_test",
        b"vaas" => "vectorart_asset",
        b"vchd" => "vector_hud_definition",
        b"vcky" => "virtual_keyboard",
        b"vehc" => "vehicle_collection",
        b"vehi" => "vehicle",
        b"vmdx" => "vision_mode",
        b"vrtx" => "vertex_shader",
        b"vtgl" => "variant_globals",
        b"vtsh" => "vertex_shader",
        b"wacd" => "gui_widget_animation_collection_definition",
        b"wadt" => "chud_widget_animation_data_template",
        b"want" => "tag_dependency_list",
        b"wave" => "wave_template",
        b"wclr" => "gui_widget_color_animation_definition",
        b"wdst" => "chud_widget_datasource_template",
        b"weap" => "weapon",
        b"weat" => "weather_system",
        b"wetn" => "scenario_wetness_bsp_data",
        b"wezr" => "game_engine_settings_definition",
        b"wfon" => "gui_widget_font_animation_definition",
        b"wgan" => "gui_widget_animation_definition",
        b"wgit" => "user_interface_screen_widget_definition",
        b"wgtz" => "user_interface_globals_definition",
        b"whip" => "cellular_automata2d",
        b"wigl" => "user_interface_shared_globals_definition",
        b"wind" => "wind",
        b"wpdp" => "water_physics_drag_properties",
        b"wpdt" => "chud_widget_placement_data_template",
        b"wphi" => "weapon_hud_interface",
        b"wpos" => "gui_widget_position_animation_definition",
        b"wrdt" => "chud_widget_render_data_template",
        b"wrot" => "gui_widget_rotation_animation_definition",
        b"wscl" => "gui_widget_scale_animation_definition",
        b"wsdt" => "chud_widget_state_data_template",
        b"wspr" => "gui_widget_sprite_animation_definition",
        b"wtuv" => "gui_widget_texture_coordinate_animation_definition",
        b"wxcg" => "weather_globals",
        b"zone" => "cache_file_resource_gestalt",
        // Dump-artifact corrections: guerilla's _meta.json mangled the first
        // byte of these two render-method FOURCCs (it lists `?rmc`/`?rmp`); the
        // real runtime groups are `rmco`/`rmp ` (the contrail/particle shader
        // chains the renderer resolves).
        b"rmco" => "shader_contrail",
        b"rmp " => "shader_particle",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_paths_and_disk_paths_convert_both_ways() {
        let root = Path::new("kit").join("tags");
        let file = resolve_tag_path(&root, "objects\\props\\crate", "scenery");
        assert_eq!(file, root.join("objects").join("props").join("crate.scenery"));
        let rel = file.strip_prefix(&root).unwrap().with_extension("");
        assert_eq!(relative_tag_path(&rel), "objects\\props\\crate");
        // A dot in a name is part of it.
        assert_eq!(relative_tag_path(Path::new("bitmaps/probe. multipurpose")), "bitmaps\\probe. multipurpose");
    }
}
