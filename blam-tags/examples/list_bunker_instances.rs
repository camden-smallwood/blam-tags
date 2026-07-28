//! List instanced-geometry instances near a world point, to identify
//! which instance(s) make up a given structure (e.g. the s3d_turf bunker).
//!
//! Usage:
//!   cargo run --example list_bunker_instances -- <sbsp> [cx cy cz] [radius]

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::structure_bsp::StructureBsp;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().expect("usage: <sbsp> [cx cy cz] [radius]"));
    let cx: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(-7.0);
    let cy: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8.0);
    let cz: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let radius: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(8.0);

    let tag = TagFile::read(&path).expect("read sbsp");
    let sbsp = StructureBsp::from_tag(&tag).expect("StructureBsp::from_tag");

    eprintln!(
        "{} instances; searching within {:.1}u of ({:.1},{:.1},{:.1})",
        sbsp.instanced_geometry_instances.len(), radius, cx, cy, cz
    );
    let r2 = radius * radius;
    let mut hits: Vec<(usize, f32)> = Vec::new();
    for (i, inst) in sbsp.instanced_geometry_instances.iter().enumerate() {
        let c = inst.world_bounding_sphere_center;
        let d2 = (c.x - cx).powi(2) + (c.y - cy).powi(2) + (c.z - cz).powi(2);
        if d2 <= r2 {
            hits.push((i, d2));
        }
    }
    // Sort by bounding-sphere radius desc (biggest structures first).
    hits.sort_by(|a, b| {
        let ra = sbsp.instanced_geometry_instances[a.0].world_bounding_sphere_radius;
        let rb = sbsp.instanced_geometry_instances[b.0].world_bounding_sphere_radius;
        rb.partial_cmp(&ra).unwrap()
    });
    for (i, d2) in hits.iter().take(60) {
        let inst = &sbsp.instanced_geometry_instances[*i];
        let def = &sbsp.instance_definitions[inst.definition_index as usize];
        let nsurf = def.bsp.as_ref().map(|b| b.surfaces.len()).unwrap_or(0);
        let c = inst.world_bounding_sphere_center;
        println!(
            "inst {:>4} def={:>4} r={:>6.2} dist={:>5.2} center=({:>7.2},{:>6.2},{:>6.2}) surf={:>4} '{}'",
            i, inst.definition_index, inst.world_bounding_sphere_radius, d2.sqrt(),
            c.x, c.y, c.z, nsurf, inst.name,
        );
    }
}
