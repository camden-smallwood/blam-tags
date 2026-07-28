//! Are the pelican's door nodes driven by ANIMATION (in which case the
//! skeleton_model rest pose is overridden every frame and moving it can never
//! show), or are they static (rest pose visible)?
use std::collections::BTreeSet;
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::animation::Animation;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn read(archives: &[IoStoreArchive], suffix: &str) -> Option<Vec<u8>> {
    for a in archives {
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(suffix) {
                return a.read(&e.path).ok();
            }
        }
    }
    None
}

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let archives: Vec<_> = utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    // Node names, in index order, from the skeleton.
    let skel = TagFile::read_from_bytes(
        &read(&archives, "/pelican/pelican-skeleton_model.ubulk").expect("skel")).unwrap();
    let root = skel.root();
    let nb = root.field_path("nodes").and_then(|f| f.as_block()).unwrap();
    let names: Vec<String> = (0..nb.len())
        .map(|i| nb.element(i).and_then(|n| n.read_string_id("name")).unwrap_or_default())
        .collect();

    let jmad_bytes = read(&archives, "/pelican/pelican.model_animation_graph.ubulk")
        .or_else(|| read(&archives, "/pelican/pelican-model_animation_graph.ubulk"))
        .expect("pelican jmad");
    let jmad = TagFile::read_from_bytes(&jmad_bytes).unwrap();
    let anim = Animation::new(&jmad).expect("parse jmad");

    let mut animated: BTreeSet<usize> = BTreeSet::new();
    let mut n_anims = 0;
    let mut decoded = 0;
    for g in anim.iter() {
        n_anims += 1;
        let Ok(clip) = g.decode() else { continue };
        decoded += 1;
        let Some(flags) = clip.node_flags.as_ref() else { continue };
        for i in 0..names.len() {
            if flags.static_rotation.bit(i) || flags.static_translation.bit(i)
                || flags.static_scale.bit(i) || flags.animated_rotation.bit(i)
                || flags.animated_translation.bit(i) || flags.animated_scale.bit(i) {
                animated.insert(i);
            }
        }
    }
    println!("pelican jmad: {n_anims} animations ({decoded} decoded), {} nodes\n", names.len());
    for g in anim.iter() {
        let per = g.decode().ok().and_then(|c| c.node_flags.map(|f| (0..names.len())
            .filter(|i| f.static_rotation.bit(*i) || f.static_translation.bit(*i) || f.static_scale.bit(*i)
                || f.animated_rotation.bit(*i) || f.animated_translation.bit(*i) || f.animated_scale.bit(*i))
            .count())).unwrap_or(0);
        println!("  anim[{}] {:<28} type={:<10} frames={:<4} drives {per}/{} nodes",
            g.index, g.name.clone().unwrap_or_default(),
            g.animation_type.clone().unwrap_or_default(), g.frame_count, names.len());
    }
    println!();
    println!("nodes DRIVEN by at least one animation ({}/{}):", animated.len(), names.len());
    for i in &animated {
        println!("    [{i:2}] {}", names[*i]);
    }
    println!("\nnodes NEVER animated (rest pose is what you see):");
    for (i, n) in names.iter().enumerate() {
        if !animated.contains(&i) { println!("    [{i:2}] {n}"); }
    }
    for door in ["maindoor_m", "upperdoor_m", "frontdoor_l", "cabindoor_l", "slidedoor1_l"] {
        if let Some(i) = names.iter().position(|n| n == door) {
            println!("\n{door} (index {i}): animation-driven = {}", animated.contains(&i));
        }
    }
}
