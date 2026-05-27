//! Dump the `flags` long_flags field from a .model tag using the
//! generic TagFile reader. Output: hex value + decoded bit names per
//! the engine model_flags_definition (matches Ares
//! source/models/model_definitions.h enum order).
use blam_tags::file::TagFile;
use std::path::PathBuf;

const BIT_NAMES: &[&str] = &[
    "active_camo_always_on",        // bit 0
    "active_camo_never",            // bit 1
    "has_shield_impact_effect",     // bit 2
    "use_sky_lighting",             // bit 3 = 0x08
    "inconsequential_target",       // bit 4
    "use_airprobe_lighting",        // bit 5 = 0x20
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(
        std::env::args().nth(1).ok_or("usage: <model.tag.path>")?,
    );
    let tag = TagFile::read(&path)?;
    let root = tag.root();
    let flags = root.read_int_any("flags").unwrap_or(-1);
    println!("{}: flags = 0x{:08x}", path.display(), flags);
    for (i, name) in BIT_NAMES.iter().enumerate() {
        let set = (flags & (1 << i)) != 0;
        println!("  bit {i} (0x{:02x}) {} = {}",
                 1u32 << i, name, if set { "SET" } else { "clear" });
    }
    // Also read default lightprobe presence — sometimes tag-level signals.
    Ok(())
}
