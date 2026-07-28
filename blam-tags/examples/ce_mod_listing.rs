//! Verify `IoStoreArchive::recover_entries` names every chunk in a mod
//! container that carries no directory index — for a plain override, and for a
//! brand-new/renamed package whose ids appear in no base container.
use blam_tags::file::TagFile;
use blam_tags::fields::TagFieldData;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::writer::{write_mod_container_ex, NewPackage};
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
    let mut base = None;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        if a.entries().iter().any(|e| e.path.to_ascii_lowercase().replace('\\',"/")
            .ends_with("pelican-skeleton_model.ubulk")) { base = Some(a); break; }
    }
    let base = base.expect("base container");
    let rel = base.entries().iter()
        .find(|e| e.path.to_ascii_lowercase().replace('\\',"/").ends_with("pelican-skeleton_model.ubulk"))
        .unwrap().path.clone();
    let orig = base.read(&rel).unwrap();

    // Same-size edit.
    let mut tag = TagFile::read_from_bytes(&orig).unwrap();
    {
        let mut root = tag.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        let mut el = nb.element_mut(16).unwrap();
        let c = el.as_ref().read_point3d("default translation");
        el.field_mut("default translation").unwrap()
            .set(TagFieldData::RealPoint3d(RealPoint3d { x: c.x + 0.25, y: c.y, z: c.z })).unwrap();
    }
    let edited = tag.write_to_bytes().unwrap();

    // --- Case A: plain override (only the .ubulk lands in the mod) ---
    let out_a = std::path::Path::new(SCRATCH).join("case_a_override_P.utoc");
    write_mod_container_ex(&[(&base, rel.as_str(), edited.as_slice())], &[], &out_a).unwrap();
    let mut a = IoStoreArchive::open(&out_a).unwrap();
    println!("CASE A — override");
    println!("  entries before recovery: {}", a.entries().len());
    let n = a.recover_entries(&[&base], None);
    println!("  recovered: {n}");
    for e in a.entries() { println!("    {} (chunk {})", e.path, e.chunk_index); }
    println!("  path matches base exactly: {}", a.entries().iter().any(|e| e.path == rel));
    println!("  read-back matches edit: {}", a.read(&rel).map(|b| b == edited).unwrap_or(false));

    // --- Case B: brand-new / renamed package, resolved with NO base at all ---
    let ua_path = rel.strip_suffix(".ubulk").map(|s| format!("{s}.uasset")).unwrap();
    let template = base.read(&ua_path).unwrap();
    let new_pkg = "/Game/Tags/objects/vehicles/human/pelican/pelican_doors_open-skeleton_model";
    let out_b = std::path::Path::new(SCRATCH).join("case_b_new_P.utoc");
    write_mod_container_ex(&[], &[NewPackage {
        template_uasset: &template,
        tag_bytes: &edited,
        new_package_path: new_pkg,
        redirect_from: None,
    }], &out_b).unwrap();
    let mut b = IoStoreArchive::open(&out_b).unwrap();
    println!("\nCASE B — new/renamed package, recovered with NO base container");
    println!("  entries before recovery: {}", b.entries().len());
    let n = b.recover_entries(&[], Some("Meteorite/Content/"));
    println!("  recovered: {n}");
    for e in b.entries() { println!("    {} (chunk {})", e.path, e.chunk_index); }
    let expect = "Meteorite/Content/Tags/objects/vehicles/human/pelican/pelican_doors_open-skeleton_model.ubulk";
    println!("  .ubulk named from the package header: {}", b.contains(expect));
    println!("  read-back matches edit: {}", b.read(expect).map(|x| x == edited).unwrap_or(false));

    // --- Case B again, prefix inferred from the base instead of passed in ---
    let mut b2 = IoStoreArchive::open(&out_b).unwrap();
    let n2 = b2.recover_entries(&[&base], None);
    println!("\nCASE B (prefix inferred from base): recovered {n2}");
    for e in b2.entries() { println!("    {}", e.path); }
}
