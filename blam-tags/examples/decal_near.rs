//! Find scenario decal placements near a world point and print their
//! palette + the decal_system's radius (→ texture_scale / footprint).
//!
//! Usage: cargo run --example decal_near -- <scenario> x y z [radius]

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::decal_system::DecalSystem;
use blam_tags::paths::{derive_tags_root, resolve_tag_path};
use blam_tags::scenario::Scenario;

fn short(p: &str) -> String { p.rsplit_once(['/', '\\']).map(|(_, t)| t).unwrap_or(p).to_string() }

fn main() {
    let mut a = std::env::args().skip(1);
    let scn = PathBuf::from(a.next().unwrap());
    let tx: f32 = a.next().unwrap().parse().unwrap();
    let ty: f32 = a.next().unwrap().parse().unwrap();
    let tz: f32 = a.next().unwrap().parse().unwrap();
    let rad: f32 = a.next().and_then(|s| s.parse().ok()).unwrap_or(1.5);

    let tag = TagFile::read(&scn).unwrap();
    let scnr = Scenario::from_tag(&tag).unwrap();
    let root = derive_tags_root(&scn).unwrap();
    let r2 = rad * rad;

    let mut near: Vec<(f32, usize)> = scnr.decals.iter().enumerate().filter_map(|(i, d)| {
        let dx = d.position.x - tx; let dy = d.position.y - ty; let dz = d.position.z - tz;
        let d2 = dx*dx + dy*dy + dz*dz;
        if d2 <= r2 { Some((d2, i)) } else { None }
    }).collect();
    near.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    println!("decals within {rad} of ({tx},{ty},{tz}): {}", near.len());
    for (d2, i) in near {
        let d = &scnr.decals[i];
        let pal = scnr.decal_palette.get(d.palette_index as usize);
        let name = pal.map(|p| short(&p.decal_system)).unwrap_or("<oob>".into());
        // Load the decal_system to read radius.
        let mut rstr = String::from("?");
        if let Some(p) = pal {
            let path = resolve_tag_path(&root, &p.decal_system, "decal_system");
            if let Ok(t) = TagFile::read(&path) {
                if let Ok(ds) = DecalSystem::from_tag(&t) {
                    if let Some(def) = ds.definitions.first() {
                        rstr = format!("radius=({:.2},{:.2}) max_r={:.2}", def.radius.0, def.radius.1, ds.runtime_max_radius);
                    }
                }
            }
        }
        println!(
            "  [{:>3}] dist={:.2} pos=({:>7.2},{:>6.2},{:>5.2}) scale={:.2} pal[{:>2}] {:<22} {}",
            d2.sqrt(), i, d.position.x, d.position.y, d.position.z, d.scale, d.palette_index, name, rstr,
        );
    }
}
