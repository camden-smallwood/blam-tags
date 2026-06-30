//! Print mesh -> part -> material(shader) mapping for a render_model,
//! so we can map sky meshes to their shaders (roter_rauch/smoke/etc).

use blam_tags::{RenderModel, TagFile};

fn main() {
    let path = std::env::args().nth(1).expect("usage: dump_sky_mesh_parts <path.render_model>");
    let tag = TagFile::read(&path).expect("failed to read tag");
    let rm = RenderModel::from_tag(&tag).expect("failed to extract render_model");
    let meshes = RenderModel::derive_render_meshes(&tag).expect("failed to derive render meshes");

    // region -> mesh range, to label meshes with their region name.
    let mut mesh_region: Vec<String> = vec![String::from("?"); meshes.len()];
    for r in &rm.regions {
        for p in &r.permutations {
            for mi in p.mesh_index..(p.mesh_index + p.mesh_count) {
                if (mi as usize) < mesh_region.len() {
                    mesh_region[mi as usize] = format!("region '{}'/perm '{}'", r.name, p.name);
                }
            }
        }
    }

    for (mi, m) in meshes.iter().enumerate() {
        println!("mesh[{mi}] ({}) — {} parts", mesh_region[mi], m.parts.len());
        for (pi, part) in m.parts.iter().enumerate() {
            let mat = rm.materials.get(part.material_index as usize);
            let name = mat.map(|x| x.shader_name()).unwrap_or_else(|| "<oob>".to_string());
            let rmpath = mat.map(|x| x.render_method.clone()).unwrap_or_default();
            println!(
                "   part[{pi}] mat={} {} ({})",
                part.material_index, name, rmpath
            );
        }
    }
}
