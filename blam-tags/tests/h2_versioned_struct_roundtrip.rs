//! Opt-in regression for the Halo 2 versioned inline-struct write path.
//!
//! When a fresh tagged struct (e.g. a shader animation property's
//! `mapping_function`) is created and the tag is written, the encoder must
//! stamp the struct's on-disk version header. Without it the reader defaults
//! to version 0 and resolves the wrong FieldSet variant — so the struct (and
//! often the whole tag) corrupts on the next open. See
//! `classic::synthesize_h2_struct_header`.
//!
//! Skips silently when the local game tags aren't present on this machine.

use blam_tags::classic::read_classic_tag_file;
use blam_tags::layout::TagLayout;
use std::path::Path;

const TAGS: &str = "/Users/camden/Halo/halo2_mcc/tags";
const DEFS: &str = "../definitions/halo2_mcc";

fn layout(group: &str) -> Option<TagLayout> {
    TagLayout::from_json(Path::new(DEFS).join(format!("{group}.json"))).ok()
}

fn load_h2(rel: &str, group: &str) -> Option<blam_tags::file::TagFile> {
    let bytes = std::fs::read(Path::new(TAGS).join(rel)).ok()?;
    read_classic_tag_file(&bytes, layout(group)?).ok()
}

fn reread(tag: &blam_tags::file::TagFile, group: &str) -> blam_tags::file::TagFile {
    let bytes = tag.write_to_bytes().expect("serialize classic tag");
    read_classic_tag_file(&bytes, layout(group).expect("layout")).expect("reread classic tag")
}

/// Adding an animation property creates a fresh nested `function`
/// (`mapping_function`) struct. That struct must survive a write→reread as
/// the base variant (carrying its `data` byte block), not collapse to
/// `mapping_function__v0`.
#[test]
fn fresh_h2_function_struct_keeps_version_on_roundtrip() {
    let Some(mut tag) = load_h2("incompetent/shaders/concrete_10x10_plain.shader", "shader") else {
        eprintln!("skip: halo2_mcc concrete_10x10_plain.shader not present");
        return;
    };

    // Create a new parameter, then an animation property inside it (this is
    // what spawns the fresh `function` struct).
    {
        let mut root = tag.root_mut();
        let mut field = root
            .field_path_mut("parameters")
            .expect("parameters field resolves");
        let mut params = field.as_block_mut().expect("parameters is a block");
        params.add_element();
    }
    {
        let mut root = tag.root_mut();
        let mut field = root
            .field_path_mut("parameters[1]/animation properties")
            .expect("animation properties field resolves");
        let mut anim = field.as_block_mut().expect("animation properties is a block");
        anim.add_element();
    }

    // In memory the fresh function is the base `mapping_function` (a `data`
    // byte block). Confirm that, then confirm it survives the round-trip.
    let in_mem_has_data = tag
        .root()
        .field_path("parameters[1]/animation properties[0]/function/data")
        .and_then(|f| f.as_block())
        .is_some();
    assert!(in_mem_has_data, "fresh function must expose a `data` byte block in memory");

    let rt = reread(&tag, "shader");
    let data = rt
        .root()
        .field_path("parameters[1]/animation properties[0]/function/data")
        .and_then(|f| f.as_block());
    assert!(
        data.is_some(),
        "after write+reread the function struct must still resolve as the base \
         `mapping_function` variant with a `data` byte block — a missing version \
         header would collapse it to `mapping_function__v0` and corrupt the tag",
    );
}

/// Guard against the inverse regression: writing unmodified real H2 shaders
/// must stay byte-exact (existing struct headers preserved verbatim,
/// headerless version-0 structs stay headerless — no spurious headers added).
#[test]
fn real_h2_shaders_roundtrip_byte_exact() {
    let dir = Path::new(TAGS).join("incompetent/shaders");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("skip: {} not present", dir.display());
        return;
    };
    let mut checked = 0;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("shader") {
            continue;
        }
        let orig = std::fs::read(&p).unwrap();
        let Some(tag) = read_classic_tag_file(&orig, layout("shader").unwrap()).ok() else {
            continue;
        };
        let out = tag.write_to_bytes().unwrap();
        assert_eq!(orig, out, "round-trip not byte-exact: {}", p.display());
        checked += 1;
    }
    if checked == 0 {
        eprintln!("skip: no halo2_mcc shaders found");
    } else {
        eprintln!("byte-exact round-trip verified for {checked} H2 shaders");
    }
}
