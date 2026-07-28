//! Validate `RenderModel::from_ue_skeletal_meshes` — the cross-game
//! super-type synthesis that drives the Baboon preview for CampaignEvolved.
//!
//! Run: cargo run -p blam-tags --features iostore --example ce_render_model

use std::io::Cursor;

use blam_tags::file::TagFile;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::jms::UeMeshPart;
use blam_tags::render_model::RenderModel;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn find(suffix: &str) -> Option<Vec<u8>> {
    let s = suffix.to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
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

fn load_mesh(suffix: &str) -> (SkeletalMesh, Vec<String>) {
    let bytes = find(suffix).expect("mesh");
    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None).unwrap();
    let names = hdr.name_map.copy_raw_names();
    let mesh = SkeletalMesh::from_package(&bytes, &names, hdr.summary.header_size as usize).unwrap();
    let mats = hdr.imported_package_names.iter()
        .filter(|p| { let b = p.rsplit('/').next().unwrap_or(""); b.starts_with("MI_") || b.starts_with("M_") })
        .map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).collect();
    (mesh, mats)
}

fn main() {
    let (body, body_mats) = load_mesh("sk_elite_common_body.uasset");
    let (head, head_mats) = load_mesh("sk_elite_common_head.uasset");
    let skel = TagFile::read_from_bytes(&find("elite_ai-skeleton_model.ubulk").expect("skel")).unwrap();

    let parts = [
        UeMeshPart { mesh: &body, region: "body".into(), permutation: String::new(), name: "sk_elite_common_body".into(), material_names: body_mats },
        UeMeshPart { mesh: &head, region: "head".into(), permutation: String::new(), name: "sk_elite_common_head".into(), material_names: head_mats },
    ];
    let (rm, meshes) = RenderModel::from_ue_skeletal_meshes(&parts, &skel).expect("synth");

    println!("RenderModel '{}': {} nodes, {} marker groups, {} materials, {} regions, {} render meshes",
        rm.name, rm.nodes.len(), rm.marker_groups.len(), rm.materials.len(), rm.regions.len(), meshes.len());
    for r in &rm.regions {
        for p in &r.permutations {
            println!("  region '{}' perm '{}' -> mesh_index={} count={}", r.name, p.name, p.mesh_index, p.mesh_count);
        }
    }
    println!("materials: {:?}", rm.materials.iter().map(|m| &m.render_method).collect::<Vec<_>>());
    for (i, m) in meshes.iter().enumerate() {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        let mut max_node = 0u8;
        for v in &m.vertices {
            let p = [v.position.x, v.position.y, v.position.z];
            for k in 0..3 { lo[k] = lo[k].min(p[k]); hi[k] = hi[k].max(p[k]); }
            max_node = max_node.max(*v.node_indices.iter().max().unwrap());
        }
        println!("  mesh[{i}]: {} verts, {} indices, {} parts, max_node_idx={max_node}, bounds [{:.2},{:.2},{:.2}]..[{:.2},{:.2},{:.2}]",
            m.vertices.len(), m.indices.len(), m.parts.len(), lo[0],lo[1],lo[2],hi[0],hi[1],hi[2]);
    }
    // node/marker sanity
    println!("node[0]='{}' parent={}, nodes with valid parents: {}",
        rm.nodes[0].name, rm.nodes[0].parent_node,
        rm.nodes.iter().filter(|n| n.parent_node == -1 || (n.parent_node as usize) < rm.nodes.len()).count());
    let total_markers: usize = rm.marker_groups.iter().map(|g| g.markers.len()).sum();
    println!("marker groups: {}, total markers: {}", rm.marker_groups.len(), total_markers);
}
