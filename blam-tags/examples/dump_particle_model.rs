//! Dump a particle_model (pmdf) tag.

use blam_tags::file::TagFile;
use blam_tags::particle_model::ParticleModel;
use blam_tags::render_model::extract_render_geometry_meshes;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_particle_model <path>");
    let tag = TagFile::read(&path).expect("read");
    let pm = ParticleModel::from_tag(&tag).expect("walk");
    println!("=== {} ===", path);
    println!("variant_count:               {}", pm.variant_count());
    println!("mesh_count:                  {}", pm.mesh_count());
    for (v, var) in pm.variants.iter().enumerate() {
        println!("  variant[{v}]: {} registers", var.registers.len());
    }
    // Renderer-ready meshes (vertices/indices) via the shared extractor.
    match extract_render_geometry_meshes(&tag.root()) {
        Ok(meshes) => {
            for (i, m) in meshes.iter().enumerate() {
                println!(
                    "  mesh[{i}]: verts={} indices={} parts={}",
                    m.vertices.len(),
                    m.indices.len(),
                    m.parts.len()
                );
            }
        }
        Err(e) => println!("  (mesh extract failed: {e})"),
    }
}
