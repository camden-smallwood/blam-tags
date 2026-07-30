//! Is a tag's import-map *slot order* reproducible?
//!
//! `ce_import_order` settles the arrays: `imported_packages` is ascending by
//! `FPackageId`, the ids are the CityHash of the parallel names, and
//! `imported_public_export_hashes` is one entry per unique (package, hash) pair
//! in first-use order — 8,733/8,733 each. What it does not settle is the order
//! of the `import_map` slots themselves, and that is what stops `ce_synth_bytes`
//! from holding these 8,733 tags to byte identity.
//!
//! The package spec calls that order the cooker's and not reproducible. Worth
//! retesting: the same spec said the three script imports were unordered, and
//! they are always [Default__CDO, class, module].
//!
//! This classifies every slot (S = script, N = null `UPackage`, P = package
//! import), collapses the run-length shape, and checks the rules a builder would
//! need to emit the map itself.
//!
//! The answer is that it cannot. There are 476 distinct shapes; the three script
//! imports lead the map in 2 of 8,733 tags; and package-import slots run in
//! `imported_packages` order in 6,725, not all. The slots are genuinely
//! interleaved in linker order, so the spec is right about this one and byte
//! identity is the wrong bar for these tags — they need an equivalence check
//! that permits a different order and remaps every index that points into it.
//!
//! Run: cargo run --release --features iostore --example ce_import_shape [substr]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndexType};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let filter = std::env::args()
        .nth(1)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read_dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    let mut shapes: BTreeMap<String, usize> = BTreeMap::new();
    let mut n = 0usize;
    let mut scripts_first = 0usize;
    let mut pkg_index_ascending = 0usize;
    let mut null_follows_group = 0usize;
    let mut samples: Vec<String> = Vec::new();

    for utoc in &utocs {
        let Ok(a) = IoStoreArchive::open(utoc) else {
            continue;
        };
        for e in a.entries() {
            let lp = e.path.to_ascii_lowercase();
            if !lp.contains("/tags/") || !lp.ends_with(".uasset") {
                continue;
            }
            if !filter.is_empty() && !lp.contains(&filter) {
                continue;
            }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(h) =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
            else {
                continue;
            };
            if h.imported_packages.is_empty() {
                continue;
            }
            n += 1;

            let kinds: Vec<char> = h
                .import_map
                .iter()
                .map(|i| match i.kind() {
                    FPackageObjectIndexType::Null => 'N',
                    FPackageObjectIndexType::ScriptImport => 'S',
                    FPackageObjectIndexType::PackageImport => 'P',
                    _ => '?',
                })
                .collect();

            if kinds.iter().take(3).all(|c| *c == 'S') {
                scripts_first += 1;
            }

            // Do the package-import slots reference packages in non-decreasing
            // `imported_packages` index order? That is the rule a builder needs:
            // emit each imported package's slots in array order.
            let pidx: Vec<u32> = h
                .import_map
                .iter()
                .filter_map(|i| i.package_import().map(|r| r.imported_package_index))
                .collect();
            if pidx.windows(2).all(|w| w[0] <= w[1]) {
                pkg_index_ascending += 1;
            }

            // The spec says the cooker emits one null `UPackage` slot per
            // imported package alongside its export slots. Measure which side it
            // lands on rather than assuming: is every run of package imports
            // immediately *followed* by a null slot?
            let mut ok = true;
            for (i, k) in kinds.iter().enumerate() {
                if *k == 'P' && kinds.get(i + 1).is_some_and(|c| *c != 'P' && *c != 'N') {
                    ok = false;
                    break;
                }
            }
            if ok {
                null_follows_group += 1;
            }

            let mut collapsed = String::new();
            let mut it = kinds.iter().peekable();
            while let Some(c) = it.next() {
                let mut run = 1;
                while it.peek() == Some(&c) {
                    it.next();
                    run += 1;
                }
                collapsed.push(*c);
                if run > 1 {
                    collapsed.push_str(&run.to_string());
                }
            }
            if samples.len() < 8 && collapsed.matches('N').count() > 1 {
                samples.push(format!("{collapsed:<28} {}", e.path));
            }
            *shapes.entry(collapsed).or_default() += 1;
        }
    }

    println!("tags with package imports            : {n}");
    println!("  first three slots are script imports: {scripts_first}");
    println!("  package-import slots in array order : {pkg_index_ascending}");
    println!("  every P run followed by N            : {null_follows_group}");
    println!("\nimport-map shapes (S=script, N=null UPackage, P=package import):");
    let mut v: Vec<_> = shapes.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1));
    for (shape, c) in v.iter().take(18) {
        println!("   {c:>6}  {shape}");
    }
    println!("   ({} distinct shapes)", shapes.len());
    if !samples.is_empty() {
        println!("\nsamples with more than one null slot:");
        for s in &samples {
            println!("   {s}");
        }
    }
}
