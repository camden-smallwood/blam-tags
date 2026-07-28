//! Re-edit a tag inside an already-exported mod container, in place.
//!
//! Reproduces the reported failure: edit a tag, export a mod, reload the folder
//! (which now mounts the mod), edit that tag again and Save. The overwrite used
//! to resolve the path through a freshly opened handle, and a mod container
//! ships no directory index — so every save failed with
//! `path not found in container: …`.
//!
//! Both second edits are exercised: one that keeps the tag's length and one that
//! changes it (which also has to repoint the paired `.uasset`).
use blam_tags::fields::TagFieldData;
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::writer::{overwrite_tag_in_place_with, write_mod_container_ex};
use blam_tags::math::RealPoint3d;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const SCRATCH: &str =
    "/private/tmp/claude-501/-Users-camden-Source-Baboon-local/19d92cda-b2d2-479b-9618-599cb1e08a26/scratchpad";

/// Nudge node 16's default translation by `dx` — a same-length edit.
fn nudge(bytes: &[u8], dx: f32) -> Vec<u8> {
    let mut tag = TagFile::read_from_bytes(bytes).unwrap();
    {
        let mut root = tag.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        let mut el = nb.element_mut(16).unwrap();
        let c = el.as_ref().read_point3d("default translation");
        el.field_mut("default translation")
            .unwrap()
            .set(TagFieldData::RealPoint3d(RealPoint3d {
                x: c.x + dx,
                y: c.y,
                z: c.z,
            }))
            .unwrap();
    }
    tag.write_to_bytes().unwrap()
}

/// Duplicate a node element — changes the tag's serialized length.
fn grow(bytes: &[u8]) -> Vec<u8> {
    let mut tag = TagFile::read_from_bytes(bytes).unwrap();
    {
        let mut root = tag.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        nb.duplicate_element(16).unwrap();
    }
    tag.write_to_bytes().unwrap()
}

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let leaf = "pelican-skeleton_model.ubulk";
    let mut base = None;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else {
            continue;
        };
        if a.entries()
            .iter()
            .any(|e| e.path.to_ascii_lowercase().replace('\\', "/").ends_with(leaf))
        {
            base = Some(a);
            break;
        }
    }
    let base = base.expect("base container");
    let rel = base
        .entries()
        .iter()
        .find(|e| e.path.to_ascii_lowercase().replace('\\', "/").ends_with(leaf))
        .unwrap()
        .path
        .clone();
    let orig = base.read(&rel).unwrap();

    for (label, first, second) in [
        ("same-length export, same-length re-edit", nudge(&orig, 0.25), nudge(&orig, 0.5)),
        ("same-length export, size-changing re-edit", nudge(&orig, 0.25), grow(&orig)),
        ("size-changing export, same-length re-edit", grow(&orig), nudge(&grow(&orig), 0.5)),
    ] {
        println!("\n{label}");
        println!(
            "  lengths: base {} -> export {} -> re-edit {}",
            orig.len(),
            first.len(),
            second.len()
        );

        // 1. Export the mod, exactly as "Export Mod" does.
        let out = std::path::Path::new(SCRATCH).join("reedit_P.utoc");
        for ext in ["utoc", "ucas", "pak"] {
            let _ = std::fs::remove_file(out.with_extension(ext));
        }
        write_mod_container_ex(&[(&base, rel.as_str(), first.as_slice())], &[], &out).unwrap();

        // 2. Reload the folder: the mod mounts with no directory index and its
        //    file list is rebuilt from the base containers.
        let mut modded = IoStoreArchive::open(&out).unwrap();
        println!("  entries on a fresh open: {}", modded.entries().len());
        modded.recover_entries(&[&base], None);
        println!("  entries after recovery:  {}", modded.entries().len());
        assert_eq!(modded.read(&rel).unwrap(), first, "mod serves the first edit");

        // 3. Edit again and Save (overwrite the mod container in place). The
        //    old path — resolving through a fresh handle — is what failed.
        match blam_tags::iostore::writer::overwrite_tag_in_place(&out, &rel, &second) {
            Ok(()) => println!("  fresh-handle overwrite: ok (unexpected)"),
            Err(e) => println!("  fresh-handle overwrite fails, as reported: {e}"),
        }
        match overwrite_tag_in_place_with(&modded, &out, &rel, &second) {
            Ok(()) => println!("  overwrite: ok"),
            Err(e) => {
                println!("  overwrite FAILED: {e}");
                continue;
            }
        }
        drop(modded);

        // 4. Reopen as the app does after a save, and read the tag back.
        let mut after = IoStoreArchive::open(&out).unwrap();
        after.recover_entries(&[&base], None);
        let read_back = after.read(&rel).unwrap();
        println!("  read-back matches the second edit: {}", read_back == second);
        assert_eq!(read_back, second);
        assert!(TagFile::read_from_bytes(&read_back).is_ok(), "re-parses as a tag");

        // The declared bulk length must track the tag, or the game reads garbage.
        let ua = rel.strip_suffix(".ubulk").map(|s| format!("{s}.uasset")).unwrap();
        let declared = declared_serial_size(&after.read(&ua).unwrap());
        println!("  .uasset SerialSize: {declared:?} (tag is {})", second.len());
        assert_eq!(declared, Some(second.len() as u64));
    }
    println!("\nall cases pass");
}

/// The bulk-data map's `SerialSize` — the length UE reads the `.ubulk` at.
fn declared_serial_size(uasset: &[u8]) -> Option<u64> {
    let ipeh = i32::from_le_bytes(uasset.get(0x18..0x1c)?.try_into().ok()?) as usize;
    Some(u64::from_le_bytes(
        uasset.get(ipeh - 16..ipeh - 8)?.try_into().ok()?,
    ))
}
