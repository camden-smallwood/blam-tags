//! Dump light_airprobe_name + position for all crate placements in a
//! scenario.
use blam_tags::file::TagFile;
use blam_tags::scenario::Scenario;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).ok_or("usage: <scenario>")?);
    let tag = TagFile::read(&path)?;
    let scenario = Scenario::from_tag(&tag)?;
    println!("Scenery placements: {}", scenario.scenery.len());
    println!("Crate placements:   {}", scenario.crates.len());
    println!();
    for (i, p) in scenario.crates.iter().enumerate() {
        let pos = p.object_data.position;
        let name = &p.object_data.light_airprobe_name;
        println!(
            "crate[{i}] pos=({:.2},{:.2},{:.2}) light_airprobe_name={:?}",
            pos.x, pos.y, pos.z,
            if name.is_empty() { "<empty>" } else { name.as_str() },
        );
    }
    Ok(())
}
