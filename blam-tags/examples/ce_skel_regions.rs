//! Dump a skeleton_model's regions + permutation names (the authoritative
//! region/permutation taxonomy the preview should use).
//! Run: cargo run -p blam-tags --features iostore --example ce_skel_regions -- [skel-suffix]

use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let suffix = std::env::args().nth(1).unwrap_or_else(|| "marine-skeleton_model.ubulk".to_string()).to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut bytes = None;
    'o: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix) {
                bytes = a.read(&e.path).ok(); break 'o;
            }
        }
    }
    let tag = TagFile::read_from_bytes(&bytes.expect("skel not found")).unwrap();
    let root = tag.root();
    let nodes = root.field_path("nodes").and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
    println!("skeleton_model {suffix}: {nodes} nodes");
    if let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) {
        println!("regions: {}", regions.len());
        for i in 0..regions.len() {
            let Some(r) = regions.element(i) else { continue };
            let rname = r.read_string_id("name").unwrap_or_default();
            let perms: Vec<String> = r.field_path("permutations").and_then(|f| f.as_block())
                .map(|pb| (0..pb.len()).filter_map(|j| pb.element(j)).filter_map(|p| p.read_string_id("name")).collect())
                .unwrap_or_default();
            println!("  region '{rname}': {} perms -> {:?}", perms.len(), perms);
        }
    }
}
