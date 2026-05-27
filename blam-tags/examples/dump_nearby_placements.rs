//! Dump all placements (scenery + crate + machine + ...) within a
//! radius of a target XY. Used to find what's under crate[41] that
//! a downward raycast might land on.
use blam_tags::file::TagFile;
use blam_tags::scenario::Scenario;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("usage: <scenario>")?);
    let tx: f32 = std::env::args().nth(2).ok_or("missing tx")?.parse()?;
    let ty: f32 = std::env::args().nth(3).ok_or("missing ty")?.parse()?;
    let radius: f32 = std::env::args().nth(4).ok_or("missing radius")?.parse()?;
    let tag = TagFile::read(&path)?;
    let scenario = Scenario::from_tag(&tag)?;
    let near = |x: f32, y: f32| -> bool {
        let dx = x - tx;
        let dy = y - ty;
        (dx*dx + dy*dy).sqrt() <= radius
    };

    let mut report = |label: &str, list: &[blam_tags::scenario::ObjectPlacement], palette: &[blam_tags::scenario::TagReferencePalette]| {
        for (i, p) in list.iter().enumerate() {
            let pos = p.object_data.position;
            if near(pos.x, pos.y) {
                let tp = palette.get(p.palette_index as usize)
                    .map(|e| e.tag_path.as_str())
                    .unwrap_or("?");
                println!("{label}[{i}] pos=({:.2},{:.2},{:.2}) palette[{}] = {tp}",
                         pos.x, pos.y, pos.z, p.palette_index);
            }
        }
    };
    report("scenery",    &scenario.scenery,    &scenario.scenery_palette);
    report("crate",      &scenario.crates,     &scenario.crate_palette);
    report("machine",    &scenario.machines,   &scenario.machine_palette);
    report("control",    &scenario.controls,   &scenario.control_palette);
    report("weapon",     &scenario.weapons,    &scenario.weapon_palette);
    report("equipment",  &scenario.equipment,  &scenario.equipment_palette);
    Ok(())
}
