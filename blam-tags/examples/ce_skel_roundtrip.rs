//! Decisive test: does blam-tags round-trip a CE skeleton_model byte-exactly,
//! and does editing ONLY two X translations change ONLY those 8 bytes?
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::fields::TagFieldData;
use blam_tags::math::RealPoint3d;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn diff(a: &[u8], b: &[u8], label: &str) {
    if a.len() != b.len() {
        println!("  {label}: LENGTH CHANGED {} -> {}", a.len(), b.len());
    }
    let n = a.len().min(b.len());
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < n {
        if a[i] != b[i] {
            let s = i;
            while i < n && a[i] != b[i] { i += 1; }
            runs.push((s, i));
        } else { i += 1; }
    }
    let total: usize = runs.iter().map(|(s, e)| e - s).sum();
    println!("  {label}: {} differing byte(s) in {} run(s)", total, runs.len());
    for (s, e) in runs.iter().take(20) {
        let av: Vec<String> = a[*s..*e].iter().take(16).map(|x| format!("{x:02x}")).collect();
        let bv: Vec<String> = b[*s..*e].iter().take(16).map(|x| format!("{x:02x}")).collect();
        println!("     @0x{s:05x}..0x{e:05x}  {} -> {}", av.join(""), bv.join(""));
    }
    if runs.len() > 20 { println!("     ... {} more runs", runs.len() - 20); }
}

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut orig = None;
    'o: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with("pelican-skeleton_model.ubulk") {
                orig = a.read(&e.path).ok(); break 'o;
            }
        }
    }
    let orig = orig.expect("pelican skel not found");
    println!("original: {} bytes", orig.len());

    // --- Test 1: pure round-trip, no edit ---
    let tag = TagFile::read_from_bytes(&orig).unwrap();
    let rt = tag.write_to_bytes().unwrap();
    println!("\n[1] PURE ROUND-TRIP (no edit):");
    diff(&orig, &rt, "orig vs rewritten");

    // --- Test 2: edit X translation on nodes 16 (upperdoor_m) and 8 (maindoor_m) ---
    let mut tag2 = TagFile::read_from_bytes(&orig).unwrap();
    for idx in [8usize, 16usize] {
        let mut root = tag2.root_mut();
        let mut nbf = root.field_path_mut("nodes").unwrap();
        let mut nb = nbf.as_block_mut().unwrap();
        let mut el = nb.element_mut(idx).unwrap();
        let cur = el.as_ref().read_point3d("default translation");
        let newp = RealPoint3d { x: cur.x + 0.25, y: cur.y, z: cur.z };
        let mut f = el.field_mut("default translation").unwrap();
        f.set(TagFieldData::RealPoint3d(newp)).unwrap();
    }
    let ed = tag2.write_to_bytes().unwrap();
    println!("\n[2] EDITED two X translations (nodes 8 + 16, +0.25 each):");
    println!("    expected: 2 runs of 4 bytes (two f32s)");
    diff(&rt, &ed, "clean-rewrite vs edited");
}
