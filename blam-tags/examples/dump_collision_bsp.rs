//! Smoke test for the extended Bsp3d collision blocks (D2a step 1).
//!
//! Reads an sbsp tag and reports counts of:
//!   - bsp3d nodes / planes / leaves / bsp2d refs / bsp2d nodes
//!   - collision surfaces / edges / vertices
//!
//! Usage:
//!   cargo run --example dump_collision_bsp -- <path/to/level.scenario_structure_bsp>

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::structure_bsp::StructureBsp;

fn main() {
    let Some(path_str) = std::env::args().nth(1) else {
        eprintln!("usage: dump_collision_bsp <path/to/sbsp>");
        std::process::exit(2);
    };
    let path = PathBuf::from(&path_str);
    let tag = TagFile::read(&path).unwrap_or_else(|e| {
        eprintln!("failed to read {}: {e}", path.display());
        std::process::exit(1);
    });
    let sbsp = StructureBsp::from_tag(&tag).expect("StructureBsp::from_tag failed");

    let Some(cbsp) = sbsp.collision_bsp.as_ref() else {
        eprintln!("no collision bsp on this tag");
        return;
    };

    println!("collision bsp for {}:", path.display());
    println!("  bsp3d nodes:       {}", cbsp.nodes.len());
    println!("  bsp3d planes:      {}", cbsp.planes.len());
    println!("  leaves:            {}", cbsp.leaves.len());
    println!("  bsp2d references:  {}", cbsp.bsp2d_references.len());
    println!("  bsp2d nodes:       {}", cbsp.bsp2d_nodes.len());
    println!("  surfaces:          {}", cbsp.surfaces.len());
    println!("  edges:             {}", cbsp.edges.len());
    println!("  vertices:          {}", cbsp.vertices.len());

    if let Some(s0) = cbsp.surfaces.first() {
        println!(
            "\n  surfaces[0]: plane_designator={} first_edge={} material={} flags=0x{:02x}",
            s0.plane_designator, s0.first_edge, s0.material, s0.flags,
        );
    }
    if let Some(l0) = cbsp.leaves.first() {
        println!(
            "  leaves[0]:    flags=0x{:02x} bsp2d_ref_count={} first_bsp2d_ref={}",
            l0.flags, l0.bsp2d_reference_count, l0.first_bsp2d_reference,
        );
    }
    if let Some(v0) = cbsp.vertices.first() {
        println!(
            "  vertices[0]:  ({:.3}, {:.3}, {:.3})  first_edge={}",
            v0.point.x, v0.point.y, v0.point.z, v0.first_edge,
        );
    }
}
