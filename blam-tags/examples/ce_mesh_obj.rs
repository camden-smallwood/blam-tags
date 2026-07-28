//! Read a Campaign Evolved `USkeletalMesh` via the library reader and write
//! an OBJ (positions + UVs + normals + faces) to sanity-check the geometry,
//! plus print stats (bounds, skin-weight sums, bone list).
//!
//! Run:
//!   cargo run -p blam-tags --features iostore --example ce_mesh_obj -- \
//!     "<paks>" [uasset-suffix=sk_elite_common_body.uasset] [out.obj]

use std::io::{BufWriter, Write};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::skeletal_mesh::SkeletalMesh;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const DEFAULT_PAKS: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let mut args = std::env::args().skip(1);
    let paks = args.next().unwrap_or_else(|| DEFAULT_PAKS.to_string());
    let suffix = args
        .next()
        .unwrap_or_else(|| "sk_elite_common_body.uasset".to_string())
        .to_ascii_lowercase();
    let out = args.next().unwrap_or_else(|| {
        "/private/tmp/claude-501/-Users-camden-Source-Baboon-local/4803b682-de10-4887-907a-9f81ad3d13d0/scratchpad/ce_mesh.obj".to_string()
    });

    let mut utocs: Vec<_> = std::fs::read_dir(&paks)
        .expect("read_dir")
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
                bytes = a.read(&e.path).ok();
                break 'o;
            }
        }
    }
    let bytes = bytes.expect("uasset not found");
    let hdr = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
        .expect("zen header");
    let names = hdr.name_map.copy_raw_names();

    let mesh = SkeletalMesh::from_package(&bytes, &names, hdr.summary.header_size as usize)
        .expect("parse skeletal mesh");

    println!(
        "{}: {} bones, {} sections, {} verts, {} tris",
        hdr.package_name(),
        mesh.bones.len(),
        mesh.sections.len(),
        mesh.vertices.len(),
        mesh.indices.len() / 3
    );

    // Bounds + weight sanity.
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let mut wsum_min = f32::MAX;
    let mut wsum_max = f32::MIN;
    let mut infl_hist = [0usize; 12];
    for v in &mesh.vertices {
        for i in 0..3 {
            lo[i] = lo[i].min(v.position[i]);
            hi[i] = hi[i].max(v.position[i]);
        }
        let ws: f32 = v.influences.iter().map(|i| i.weight).sum();
        wsum_min = wsum_min.min(ws);
        wsum_max = wsum_max.max(ws);
        infl_hist[v.influences.len().min(11)] += 1;
    }
    println!(
        "bounds: [{:.1},{:.1},{:.1}] .. [{:.1},{:.1},{:.1}]  (size {:.1} x {:.1} x {:.1} cm)",
        lo[0], lo[1], lo[2], hi[0], hi[1], hi[2],
        hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]
    );
    println!("weight sums: {wsum_min:.3} .. {wsum_max:.3}  (expect ~1.0)");
    println!("influences/vertex histogram: {infl_hist:?}");
    println!(
        "first 8 bones: {:?}",
        mesh.bones.iter().take(8).map(|b| &b.name).collect::<Vec<_>>()
    );

    // Write OBJ.
    let f = std::fs::File::create(&out).expect("create obj");
    let mut w = BufWriter::new(f);
    writeln!(w, "# {} — {} verts / {} tris", hdr.package_name(), mesh.vertices.len(), mesh.indices.len() / 3).unwrap();
    for v in &mesh.vertices {
        writeln!(w, "v {} {} {}", v.position[0], v.position[1], v.position[2]).unwrap();
    }
    for v in &mesh.vertices {
        writeln!(w, "vt {} {}", v.uv[0], 1.0 - v.uv[1]).unwrap();
    }
    for v in &mesh.vertices {
        writeln!(w, "vn {} {} {}", v.normal[0], v.normal[1], v.normal[2]).unwrap();
    }
    for t in mesh.indices.chunks_exact(3) {
        let (a, b, c) = (t[0] + 1, t[1] + 1, t[2] + 1);
        writeln!(w, "f {a}/{a}/{a} {b}/{b}/{b} {c}/{c}/{c}").unwrap();
    }
    w.flush().unwrap();
    println!("wrote {out}");
}
