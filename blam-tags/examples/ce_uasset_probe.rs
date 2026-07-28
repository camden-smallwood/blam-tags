//! Probe A (Unreal half): parse the UE5 Zen package headers for a
//! character's SkeletalMesh / Skeleton assets, pull bone names out of the
//! local name map, and diff them against the classic `skeleton_model`
//! node names — to prove the render-mesh weight remap is a name lookup.
//! Also dumps `imported_package_names` (the real reference chain) so we
//! can see how the mesh/skeleton/region assets point at each other.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_uasset_probe -- \
//!     "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks" [name=elite]

use std::collections::BTreeSet;
use std::io::Cursor;
use std::sync::Arc;

use blam_tags::file::TagFile;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::{parse_ublock_stem, IoStoreArchive};

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
// Same version pair the proven writer path uses to parse real CE .uassets.
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

struct Mount {
    archives: Vec<Arc<IoStoreArchive>>,
    /// (normalized-lowercase-path, archive index, original rel path)
    index: Vec<(String, usize, String)>,
}

impl Mount {
    fn open(paks: &str) -> Self {
        let mut utocs: Vec<_> = std::fs::read_dir(paks)
            .expect("read_dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        let mut archives = Vec::new();
        let mut index = Vec::new();
        for utoc in &utocs {
            let Ok(a) = IoStoreArchive::open(utoc) else { continue };
            let a = Arc::new(a);
            let ai = archives.len();
            for e in a.entries() {
                index.push((e.path.to_ascii_lowercase().replace('\\', "/"), ai, e.path.clone()));
            }
            archives.push(a);
        }
        Mount { archives, index }
    }

    fn find_suffix(&self, suffix: &str) -> Option<(usize, &str)> {
        let s = suffix.to_ascii_lowercase();
        self.index
            .iter()
            .find(|(norm, _, _)| norm.ends_with(&s))
            .map(|(_, i, rel)| (*i, rel.as_str()))
    }

    fn find_all_contains(&self, needle: &str) -> Vec<(usize, String, String)> {
        let n = needle.to_ascii_lowercase();
        self.index
            .iter()
            .filter(|(norm, _, _)| norm.contains(&n))
            .map(|(norm, i, rel)| (*i, norm.clone(), rel.clone()))
            .collect()
    }

    fn read(&self, ai: usize, rel: &str) -> Option<Vec<u8>> {
        self.archives[ai].read(rel).ok()
    }
}

fn parse_zen(bytes: &[u8]) -> anyhow::Result<FZenPackageHeader> {
    let mut cur = Cursor::new(bytes);
    FZenPackageHeader::deserialize(&mut cur, None, CV, HV, None)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let name = args.next().unwrap_or_else(|| "elite".to_string()).to_ascii_lowercase();

    let mount = Mount::open(&paks);
    println!("mounted {} paks, {} entries", mount.archives.len(), mount.index.len());

    // --- classic skeleton_model node names ---
    let skel_suffix = format!("/objects/characters/{name}/{name}-skeleton_model.ubulk");
    let node_names: BTreeSet<String> = match mount.find_suffix(&skel_suffix) {
        None => {
            eprintln!("no skeleton_model tag at *{skel_suffix}");
            BTreeSet::new()
        }
        Some((ai, rel)) => {
            let bytes = mount.read(ai, rel).expect("read skel tag");
            let tag = TagFile::read_from_bytes(&bytes).expect("parse skel tag");
            let mut set = BTreeSet::new();
            if let Some(block) = tag.root().field_path("nodes").and_then(|f| f.as_block()) {
                for i in 0..block.len() {
                    if let Some(e) = block.element(i) {
                        if let Some(n) = e.read_string_id("name") {
                            set.insert(n.to_ascii_lowercase());
                        }
                    }
                }
            }
            set
        }
    };
    println!("\nskeleton_model '{name}': {} nodes", node_names.len());

    // --- UE5 SkeletalMesh / Skeleton packages for this character ---
    let mut targets: Vec<(usize, String, String)> = mount
        .find_all_contains(&format!("/characters/{name}/"))
        .into_iter()
        .filter(|(_, norm, _)| {
            norm.ends_with(".uasset")
                && norm
                    .rsplit('/')
                    .next()
                    .is_some_and(|b| b.starts_with("sk_") || b.starts_with("skel_"))
        })
        .collect();
    targets.sort_by(|a, b| a.1.cmp(&b.1));
    println!("SkeletalMesh/Skeleton candidates: {}", targets.len());

    for (ai, norm, rel) in &targets {
        let base = norm.rsplit('/').next().unwrap_or(norm);
        println!("\n================ {base} ================");
        let Some(bytes) = mount.read(*ai, rel) else {
            println!("  (read failed)");
            continue;
        };
        let hdr = match parse_zen(&bytes) {
            Ok(h) => h,
            Err(e) => {
                println!("  parse failed: {e}");
                continue;
            }
        };
        println!("  package: {}", hdr.package_name());
        println!("  exports: {}  imports: {}  names: {}",
            hdr.export_map.len(), hdr.import_map.len(), hdr.name_map.copy_raw_names().len());

        // Reference chain
        if !hdr.imported_package_names.is_empty() {
            println!("  imported packages ({}):", hdr.imported_package_names.len());
            for p in hdr.imported_package_names.iter().take(16) {
                println!("    <- {p}");
            }
            if hdr.imported_package_names.len() > 16 {
                println!("    ... +{} more", hdr.imported_package_names.len() - 16);
            }
        }

        // Bone-name overlap with the classic skeleton_model
        let raw: BTreeSet<String> =
            hdr.name_map.copy_raw_names().into_iter().map(|s| s.to_ascii_lowercase()).collect();
        if !node_names.is_empty() {
            let hit: Vec<&String> = node_names.iter().filter(|n| raw.contains(*n)).collect();
            let miss: Vec<&String> = node_names.iter().filter(|n| !raw.contains(*n)).collect();
            println!(
                "  skeleton_model node overlap: {}/{} present in this package's name map",
                hit.len(),
                node_names.len()
            );
            if !miss.is_empty() {
                let show: Vec<&str> = miss.iter().take(12).map(|s| s.as_str()).collect();
                println!("    missing: {}{}", show.join(", "),
                    if miss.len() > 12 { format!(" (+{} more)", miss.len() - 12) } else { String::new() });
            }
        }
        // Show a sample of bone-like names actually in the package
        let bones: Vec<&String> = raw
            .iter()
            .filter(|s| s.starts_with("b_") || s.starts_with("bone") || s.contains("spine") || s.contains("pelvis"))
            .take(20)
            .collect();
        if !bones.is_empty() {
            println!("  sample bone-like names in package: {}",
                bones.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
        }
    }

    // --- region/link data assets ---
    println!("\n================ region/link data assets ================");
    for (ai, norm, rel) in mount.find_all_contains(&format!("/characters/{name}/")) {
        let base = norm.rsplit('/').next().unwrap_or(&norm);
        if !(norm.ends_with(".uasset") && (base.starts_with("da_") || base.contains("region") || base.contains("meshsync"))) {
            continue;
        }
        println!("\n-- {base} --");
        let Some(bytes) = mount.read(ai, &rel) else { continue };
        match parse_zen(&bytes) {
            Ok(hdr) => {
                println!("  package: {}", hdr.package_name());
                for p in hdr.imported_package_names.iter().take(24) {
                    println!("    <- {p}");
                }
            }
            Err(e) => println!("  parse failed: {e}"),
        }
    }
}
