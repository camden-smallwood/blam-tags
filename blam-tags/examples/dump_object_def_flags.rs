//! Dump object_definition flags from any obj-derived tag.
use blam_tags::file::TagFile;
use std::path::PathBuf;

// Ares `objects/object_definitions.h:45-65` enum:
const BIT_NAMES: &[&str] = &[
    "does_not_cast_shadow",                    // bit 0
    "searches_lightmaps_on_failure",           // bit 1 = 0x02 ← OL-4 gate
    "preserves_damage_owner",                  // bit 2
    "not_pathfinding_obstacle",                // bit 3
    "is_extension_of_parent",                  // bit 4
    "cannot_cause_collision_damage",           // bit 5
    "early_mover",                             // bit 6
    "early_mover_localized_physics",           // bit 7
    "use_fake_lightprobe",                     // bit 8
    "scales_attachments",                      // bit 9
    "inherit_player_appearance",               // bit 10
    "<bit11>",                                 // bit 11 (skipped in enum?)
    "attach_to_clusters_using_dynamic_light_sphere", // bit 12
    "effects_do_not_spawn_objects_in_multiplayer",   // bit 13
    "does_not_collide_with_camera",            // bit 14
    "damage_not_blocked_by_obstructions",      // bit 15
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or("usage: <obj-tag path>")?,
    );
    let tag = TagFile::read(&path)?;
    let root = tag.root();
    // The object_definition struct is the FIRST sub-struct on every
    // obj-derived tag (crate, scenery, weapon, biped, ...). Field path
    // typically "object struct/flags" or just "flags" depending on
    // inlining. Try common paths.
    let mut flags: i128 = -1;
    for tried in &["object struct/flags", "object/flags", "flags", "object struct/object flags"] {
        if let Some(field) = root.field_path(tried) {
            if let Some(v) = field.value() {
                println!("path={tried} value={v:?}");
            }
        }
    }
    // Fall back to walking sub-structs directly.
    if let Some(obj_struct) = root.field_path("object struct").and_then(|f| f.as_struct()) {
        if let Some(v) = obj_struct.read_int_any("flags") {
            flags = v;
        }
    } else if let Some(obj_struct) = root.field_path("object").and_then(|f| f.as_struct()) {
        if let Some(v) = obj_struct.read_int_any("flags") {
            flags = v;
        }
    } else if let Some(v) = root.read_int_any("flags") {
        flags = v;
    }
    println!("\nobject flags = 0x{:08x}", flags);
    for (i, name) in BIT_NAMES.iter().enumerate() {
        let set = (flags & (1 << i)) != 0;
        if set || *name == "searches_lightmaps_on_failure" {
            println!("  bit {i} (0x{:04x}) {} = {}", 1u32 << i, name, if set { "SET" } else { "clear" });
        }
    }
    Ok(())
}
