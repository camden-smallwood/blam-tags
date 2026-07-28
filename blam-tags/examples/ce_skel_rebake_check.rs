//! (1) Every shipped CE skeleton must rebake to NO change — otherwise the
//! rebake disagrees with tool.exe and would corrupt healthy tags.
//! (2) A translation edit must be fully repaired: after edit + rebake the
//! derived data must be self-consistent again.
use blam_tags::file::TagFile;
use blam_tags::fields::TagFieldData;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::RealPoint3d;
use blam_tags::skeleton::rebake_derived_node_data;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();

    let (mut total, mut dirty) = (0usize, 0usize);
    let mut pelican: Option<Vec<u8>> = None;
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if !e.path.to_ascii_lowercase().replace('\\',"/").ends_with("-skeleton_model.ubulk") { continue }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(mut tag) = TagFile::read_from_bytes(&bytes) else { continue };
            if e.path.to_ascii_lowercase().replace('\\',"/").ends_with("/pelican/pelican-skeleton_model.ubulk") { pelican = Some(bytes.clone()); }
            total += 1;
            let r = rebake_derived_node_data(&mut tag);
            if r.changed() || !r.rotation_changed.is_empty() {
                dirty += 1;
                if dirty <= 5 {
                    println!("  DIRTY {} -> orient {} pos {} rot-changed {}",
                        e.path, r.orientations_updated, r.positions_updated, r.rotation_changed.len());
                }
            }
            // A rebake must also be byte-neutral on a healthy tag.
            if let Ok(out) = tag.write_to_bytes() {
                if out != bytes && dirty == 0 {
                    println!("  BYTES CHANGED on a clean tag: {}", e.path);
                }
            }
        }
    }
    println!("(1) shipped skeletons rebaked: {total}, reporting changes: {dirty}");

    // (2) Pepper's edit: move two node X translations, then rebake.
    let bytes = pelican.expect("pelican");
    let mut tag = TagFile::read_from_bytes(&bytes).unwrap();
    for idx in [8usize, 16usize] {
        let mut root = tag.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        let mut el = nb.element_mut(idx).unwrap();
        let c = el.as_ref().read_point3d("default translation");
        el.field_mut("default translation").unwrap()
            .set(TagFieldData::RealPoint3d(RealPoint3d { x: c.x + 0.25, y: c.y, z: c.z })).unwrap();
    }
    // Before rebake: how broken is it?
    let mut probe = TagFile::read_from_bytes(&tag.write_to_bytes().unwrap()).unwrap();
    let before = rebake_derived_node_data(&mut probe);
    println!("(2) after moving 2 nodes, BEFORE rebake:");
    println!("      stale orientation rows : {}", before.orientations_updated);
    println!("      stale inverse positions: {}   <-- the node + its whole subtree",
        before.positions_updated);
    println!("      rotation-convention nodes flagged: {}", before.rotation_changed.len());

    // Rebake for real, then confirm a second pass finds nothing left.
    let r1 = rebake_derived_node_data(&mut tag);
    let round = tag.write_to_bytes().unwrap();
    let mut again = TagFile::read_from_bytes(&round).unwrap();
    let r2 = rebake_derived_node_data(&mut again);
    println!("(2) rebake pass 1: orient {} pos {}", r1.orientations_updated, r1.positions_updated);
    println!("(2) rebake pass 2: orient {} pos {}  (must be 0/0 — idempotent)",
        r2.orientations_updated, r2.positions_updated);
    println!("(2) size unchanged: {}", round.len() == bytes.len());
}
