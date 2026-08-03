//! End-to-end: author a tag of a group the game ships none of, the way Baboon
//! does, and read it back out of the container that results.
//!
//! The unit tests prove the wrapper's bytes; this proves the path around them —
//! schema to `TagFile`, group to wrapper, wrapper through the writer that was
//! built for cloned templates, container back to a parsed tag.
//!
//! Run: cargo run --release --features iostore --example ce_new_bare_tag_e2e

use blam_tags::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::asset::tag_package::build_bare_tag_package;
use blam_tags::iostore::object::usmap::Usmap;

const DEFS: &str = "/Users/camden/Source/Baboon-local/definitions/haloce_evolved";

fn main() {
    let usmap = Usmap::meteorite().expect("bundled usmap");
    let out_dir = std::env::temp_dir().join(format!("ce_bare_e2e_{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).expect("temp dir");

    let mut failures = 0usize;
    let mut checked = 0usize;

    for group in [
        "cinematic_scene",
        "scenario_hs_source_file",
        "flock",
        "point_physics",
        "render_method",
        "sound_environment",
    ] {
        let schema = format!("{DEFS}/{group}.json");
        // 1. The body, from the group schema alone -- no pak involved.
        let tag = match TagFile::new(&schema) {
            Ok(tag) => tag,
            Err(error) => {
                println!("{group:<26} SKIP  (schema: {error})");
                continue;
            }
        };
        let body = tag.write_to_bytes().expect("serialize new tag");

        // 2. The wrapper, from the group and the package path.
        let package = format!("/Game/Tags/test/mytag-{group}");
        let (wrapper, _store) = match build_bare_tag_package(group, &package, body.len() as u64, &usmap)
        {
            Ok(built) => built,
            Err(error) => {
                println!("{group:<26} FAIL  derive: {error}");
                failures += 1;
                continue;
            }
        };

        // 3. Through the writer built for cloned templates. A derived wrapper is
        //    a valid template of its own group, so this needs no new code path.
        let utoc = out_dir.join(format!("mytag-{group}_P.utoc"));
        if let Err(error) = blam_tags::iostore::writer::write_new_tag_container(
            &wrapper, &body, &package, None, &utoc,
        ) {
            println!("{group:<26} FAIL  write: {error}");
            failures += 1;
            continue;
        }

        // 4. Read it back the way the game addresses it: by chunk id, from a
        //    container that carries no directory index.
        let mut archive = IoStoreArchive::open(&utoc).expect("reopen written container");
        let recovered = archive.recover_entries(&[], Some("Meteorite/Content/"));
        let ubulk = format!("Meteorite/Content/Tags/test/mytag-{group}.ubulk");
        let uasset = format!("Meteorite/Content/Tags/test/mytag-{group}.uasset");
        let read_body = archive.read(&ubulk);
        let read_wrapper = archive.read(&uasset);

        let body_ok = read_body.as_deref().ok() == Some(body.as_slice());
        let wrapper_ok = read_wrapper.as_deref().ok() == Some(wrapper.as_slice());
        let ok = body_ok && wrapper_ok;
        // 5. And the body still parses as the tag it was.
        let reparsed = read_body
            .as_ref()
            .ok()
            .and_then(|bytes| TagFile::read_from_bytes(bytes).ok())
            .is_some();

        checked += 1;
        if ok && reparsed {
            println!(
                "{group:<26} ok    {} byte body, {} byte wrapper, {recovered} entries recovered",
                body.len(),
                wrapper.len()
            );
        } else {
            failures += 1;
            println!(
                "{group:<26} FAIL  roundtrip body={body_ok} wrapper={wrapper_ok} reparsed={reparsed}",
            );
        }
    }

    // The control: a group that must NOT be authorable this way.
    match build_bare_tag_package("biped", "/Game/Tags/test/mytag-biped", 64, &usmap) {
        Ok(_) => {
            println!("\nbiped                      FAIL  should have been refused");
            failures += 1;
        }
        Err(error) => println!("\nbiped                      ok    refused: {error}"),
    }

    let _ = std::fs::remove_dir_all(&out_dir);
    println!("\n{checked} groups authored end to end, {failures} failures");
    if failures > 0 {
        std::process::exit(1);
    }
}
