//! End-to-end: combine a Campaign Evolved character's UE5 `USkeletalMesh`
//! pieces (body + head) with its `skeleton_model` into one multi-region JMS
//! for MCC tool.exe import, with region/permutation-tagged material names.
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_mesh_jms -- \
//!     "<paks>" [skel=elite_ai-skeleton_model.ubulk] [out.jms] [meshes...]

use std::io::{BufWriter, Cursor};

use blam_tags::file::TagFile;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::jms::UeMeshPart;
use blam_tags::JmsFile;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn find(paks: &str, suffix: &str) -> Option<Vec<u8>> {
    let s = suffix.to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(paks)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&s) {
                return a.read(&e.path).ok();
            }
        }
    }
    None
}

/// Load a SkeletalMesh + its material import names (in import order).
fn load_mesh(paks: &str, suffix: &str) -> Option<(SkeletalMesh, Vec<String>)> {
    let bytes = find(paks, suffix)?;
    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None).ok()?;
    let names = hdr.name_map.copy_raw_names();
    let mesh = SkeletalMesh::from_package(&bytes, &names, hdr.summary.header_size as usize).ok()?;
    let mats: Vec<String> = hdr
        .imported_package_names
        .iter()
        .filter(|p| {
            let b = p.rsplit('/').next().unwrap_or("");
            b.starts_with("MI_") || b.starts_with("M_") || p.contains("/Materials/")
        })
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string())
        .collect();
    Some((mesh, mats))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let skel_suffix = args.next().unwrap_or_else(|| "elite_ai-skeleton_model.ubulk".to_string());
    let out = args.next().unwrap_or_else(|| {
        "/private/tmp/claude-501/-Users-camden-Source-Baboon-local/4803b682-de10-4887-907a-9f81ad3d13d0/scratchpad/ce_elite.JMS".to_string()
    });

    // (region, mesh-suffix) pieces of the base variant.
    let pieces = [
        ("body", "sk_elite_common_body.uasset"),
        ("head", "sk_elite_common_head.uasset"),
    ];
    let loaded: Vec<(String, SkeletalMesh, Vec<String>)> = pieces
        .iter()
        .filter_map(|(region, suffix)| {
            load_mesh(&paks, suffix).map(|(m, mats)| (region.to_string(), m, mats))
        })
        .collect();
    for (region, m, mats) in &loaded {
        println!("  {region}: {} verts, {} tris, materials={:?}", m.vertices.len(), m.indices.len() / 3, mats);
    }

    let skel_bytes = find(&paks, &skel_suffix).expect("skeleton_model not found");
    let skel = TagFile::read_from_bytes(&skel_bytes).expect("parse skeleton_model");

    let parts: Vec<UeMeshPart> = loaded
        .iter()
        .map(|(region, mesh, mats)| UeMeshPart {
            mesh,
            region: region.clone(),
            permutation: "base".to_string(),
            name: region.clone(),
            material_names: mats.clone(),
        })
        .collect();

    let jms = JmsFile::from_ue_skeletal_meshes(&parts, &skel).expect("fuse jms");
    println!(
        "\nJMS: {} nodes, {} materials, {} markers, {} verts, {} tris",
        jms.nodes.len(), jms.materials.len(), jms.markers.len(), jms.vertices.len(), jms.triangles.len()
    );
    println!("materials: {:?}", jms.materials.iter().map(|m| &m.name).collect::<Vec<_>>());

    let f = std::fs::File::create(&out).expect("create");
    let mut w = BufWriter::new(f);
    jms.write(&mut w, 8213).expect("write jms");
    println!("wrote {out}");
}
