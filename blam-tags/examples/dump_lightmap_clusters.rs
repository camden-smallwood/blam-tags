//! Dump the per-cluster lightmap entries (lightprobe_texture_array_index,
//! pervertex_block_index) for a scenario_lightmap_bsp_data tag.
use blam_tags::file::TagFile;
use blam_tags::scenario_lightmap::LightmapBspData;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("usage: <scenario_lightmap>")?);
    let tag = TagFile::read(&path)?;
    let lbsp = LightmapBspData::from_tag(&tag)?;
    println!("clusters: {}", lbsp.clusters.len());
    println!("instances: {}", lbsp.instances.len());
    println!("bsp_per_vertex_data blocks: {}", lbsp.bsp_per_vertex_data.len());
    println!("probes: {}", lbsp.probes.len());
    println!("scenery_probes: {}", lbsp.scenery_probes.len());
    println!("airprobes: {}", lbsp.airprobes.len());
    println!();
    for (i, c) in lbsp.clusters.iter().enumerate() {
        println!("cluster[{i}] lp_tex_array_idx={} pervertex_block={} policy={:?}",
                 c.lightprobe_texture_array_index,
                 c.pervertex_block_index,
                 c.policy());
    }
    Ok(())
}
