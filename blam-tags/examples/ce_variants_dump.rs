//! Dump a CE .model (hlmt) variants block: variant name → region → permutation.
//! Run: cargo run -p blam-tags --features iostore --example ce_variants_dump -- [model-suffix]

use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let suffix = std::env::args().nth(1).unwrap_or_else(|| "marine/marine-model.ubulk".into()).to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let mut bytes = None;
    'o: for u in &utocs { let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() { if e.path.to_ascii_lowercase().replace('\\',"/").ends_with(&suffix) { bytes = a.read(&e.path).ok(); break 'o; } } }
    let tag = TagFile::read_from_bytes(&bytes.expect("model")).unwrap();
    let root = tag.root();
    let Some(variants) = root.field_path("variants").and_then(|f| f.as_block()) else { println!("no variants"); return; };
    println!("variants: {}", variants.len());
    for i in 0..variants.len() {
        let Some(v) = variants.element(i) else { continue };
        let name = v.read_string_id("name").or_else(|| v.read_string("name")).unwrap_or_default();
        print!("  '{name}': ");
        if let Some(regions) = v.field("regions").and_then(|f| f.as_block()) {
            let mut pairs = Vec::new();
            for j in 0..regions.len() {
                let Some(r) = regions.element(j) else { continue };
                let rn = r.read_string_id("region name").or_else(|| r.read_string("region name")).unwrap_or_default();
                let pn = r.field("permutations").and_then(|f| f.as_block()).and_then(|pb| pb.element(0))
                    .and_then(|p| p.read_string_id("permutation name").or_else(|| p.read_string("permutation name"))).unwrap_or_default();
                pairs.push(format!("{rn}={pn}"));
            }
            println!("{}", pairs.join(", "));
        } else { println!("(no regions)"); }
    }
}
