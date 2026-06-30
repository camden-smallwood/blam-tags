use blam_tags::structure_bsp::StructureBsp;
use blam_tags::TagFile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = "/Users/camden/Halo/halo3_mcc/tags/levels/multi/s3d_lockout/s3d_lockout.scenario_structure_bsp";
    let tag = TagFile::read(path)?;
    let bsp = StructureBsp::from_struct(&tag.root());
    println!("== s3d_lockout structure_bsp — full walker ==");
    println!("visible_name           = {:?}", bsp.visible_name);
    println!("clusters               = {}", bsp.clusters.len());
    println!("atmosphere_palette     = {}", bsp.atmosphere_palette.len());
    for (i, a) in bsp.atmosphere_palette.iter().enumerate() {
        println!("  atm[{i}] name={:?} setting_index={}", a.name, a.atmosphere_setting_index);
    }
    println!("cluster[0].atmosphere_index = {:?}", bsp.clusters.first().map(|c| c.atmosphere_index));
    println!("-- maximal-coverage blocks --");
    println!("seam_identifiers       = {}", bsp.seam_identifiers.len());
    println!("large_structure_surf   = {}", bsp.large_structure_surfaces.len());
    println!("weather_polyhedra      = {}", bsp.weather_polyhedra.len());
    println!("detail_objects         = {}", bsp.detail_objects.len());
    println!("conveyor_surfaces      = {}", bsp.conveyor_surfaces.len());
    println!("breakable_surface_sets = {}", bsp.breakable_surface_sets.len());
    println!("pathfinding_data       = {}", bsp.pathfinding_data.len());
    if let Some(p) = bsp.pathfinding_data.first() {
        println!("  pf.sectors={} links={} bsp2d_nodes={} vertices={} hints={} doors={}",
            p.sectors.len(), p.links.len(), p.bsp2d_nodes.len(), p.vertices.len(), p.hints.len(), p.doors.len());
    }
    println!("acoustics_palette      = {}", bsp.acoustics_palette.len());
    println!("background_sound_pal   = {}", bsp.background_sound_palette.len());
    println!("sound_environment_pal  = {}", bsp.sound_environment_palette.len());
    println!("sound_pas_data bytes   = {}", bsp.sound_pas_data.len());
    println!("marker_light_palette   = {}", bsp.marker_light_palette.len());
    println!("runtime_decals         = {}", bsp.runtime_decals.len());
    println!("environment_objects    = {}", bsp.environment_objects.len());
    println!("leaf_map_leaves        = {}", bsp.leaf_map_leaves.len());
    println!("leaf_map_connections   = {}", bsp.leaf_map_connections.len());
    println!("errors                 = {}", bsp.errors.len());
    println!("decorator_sets         = {} {:?}", bsp.decorator_sets.len(), bsp.decorator_sets);
    println!("acoustics_sound_clust  = {}", bsp.acoustics_sound_clusters.len());
    println!("transparent_planes     = {}", bsp.transparent_planes.len());
    println!("debug_info             = {}", bsp.debug_info.len());
    println!("audibility             = {}", bsp.audibility.len());
    println!("fake_lightprobes       = {}", bsp.fake_lightprobes.len());
    println!("widget_references      = {}", bsp.widget_references.len());
    Ok(())
}
