//! Dump info about one BspInstance from a structure_bsp tag.
use blam_tags::file::TagFile;
use blam_tags::structure_bsp::StructureBsp;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("usage: <sbsp_path> [idx]")?);
    let target: Option<usize> = std::env::args().nth(2).and_then(|s| s.parse().ok());
    let tag = TagFile::read(&path)?;
    let bsp = StructureBsp::from_tag(&tag)?;
    println!("Total instances: {}", bsp.instanced_geometry_instances.len());
    let target_idx = target.unwrap_or(143);
    for (i, inst) in bsp.instanced_geometry_instances.iter().enumerate() {
        if target.is_none() || i == target_idx {
            println!(
                "instance[{i}] def_idx={} pos=({:.3},{:.3},{:.3}) scale={:.3} bs_center=({:.3},{:.3},{:.3}) bs_r={:.3} name='{}' lightmap_policy={}",
                inst.definition_index,
                inst.position.x, inst.position.y, inst.position.z,
                inst.scale,
                inst.world_bounding_sphere_center.x,
                inst.world_bounding_sphere_center.y,
                inst.world_bounding_sphere_center.z,
                inst.world_bounding_sphere_radius,
                inst.name,
                inst.lightmapping_policy,
            );
            println!(
                "             forward=({:.3},{:.3},{:.3}) left=({:.3},{:.3},{:.3}) up=({:.3},{:.3},{:.3})",
                inst.forward.i, inst.forward.j, inst.forward.k,
                inst.left.i, inst.left.j, inst.left.k,
                inst.up.i, inst.up.j, inst.up.k,
            );
        }
    }
    Ok(())
}
