//! End-to-end test of Baboon's "Export Mod" path for a SAME-SIZE tag edit:
//! build the overlay container, then read it back and verify the game would
//! see exactly the intended bytes under exactly the right chunk id.
use blam_tags::file::TagFile;
use blam_tags::fields::TagFieldData;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::RealPoint3d;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const SCRATCH: &str = "/private/tmp/claude-501/-Users-camden-Source-Baboon-local/fa9ca3df-b4ca-4271-9b86-93c4e20fddf5/scratchpad";

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let mut found: Option<(IoStoreArchive, String, Vec<u8>)> = None;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let hit = a.entries().iter()
            .find(|e| e.path.to_ascii_lowercase().replace('\\', "/").ends_with("pelican-skeleton_model.ubulk"))
            .map(|e| e.path.clone());
        if let Some(rel) = hit {
            let bytes = a.read(&rel).unwrap();
            println!("base container: {}", u.file_name().unwrap().to_string_lossy());
            found = Some((a, rel, bytes));
            break;
        }
    }
    let (archive, rel, orig) = found.expect("pelican skel not found");
    println!("tag path: {rel}\norig len: {}", orig.len());
    let base_id = archive.chunk_id_for(&rel).unwrap();
    println!("base chunk id: {base_id:?}");

    // Same-size edit: two X translations.
    let mut tag = TagFile::read_from_bytes(&orig).unwrap();
    for idx in [8usize, 16usize] {
        let mut root = tag.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        let mut el = nb.element_mut(idx).unwrap();
        let cur = el.as_ref().read_point3d("default translation");
        let np = RealPoint3d { x: cur.x + 0.25, y: cur.y, z: cur.z };
        el.field_mut("default translation").unwrap().set(TagFieldData::RealPoint3d(np)).unwrap();
    }
    let edited = tag.write_to_bytes().unwrap();
    println!("edited len: {} (same size: {})", edited.len(), edited.len() == orig.len());

    // Export exactly as Baboon's Export Mod does.
    let out = std::path::Path::new(SCRATCH).join("pelican_test_P.utoc");
    let overrides: Vec<(&IoStoreArchive, &str, &[u8])> = vec![(&archive, rel.as_str(), edited.as_slice())];
    blam_tags::iostore::writer::write_mod_container_ex(&overrides, &[], &out).expect("write overlay");
    for ext in ["utoc", "ucas", "pak"] {
        let p = out.with_extension(ext);
        println!("  wrote {} ({} bytes)", p.file_name().unwrap().to_string_lossy(),
            std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0));
    }

    // Read the overlay back the way the engine would: by chunk id.
    let ov = IoStoreArchive::open(&out).expect("reopen overlay");
    println!("\noverlay chunk count: {}", ov.entries().len());
    let mut ok = false;
    for i in 0..ov.entries().len().max(1) {
        let Ok(id) = ov.chunk_id(i as u32) else { continue };
        let Ok(data) = ov.read_chunk(i as u32) else { continue };
        let id_match = format!("{id:?}") == format!("{base_id:?}");
        println!("  chunk[{i}] id_matches_base={id_match} len={} bytes_match_intended={}",
            data.len(), data == edited);
        if id_match && data == edited { ok = true; }
    }
    println!("\nRESULT: overlay delivers the intended bytes under the base chunk id: {ok}");
}
