//! What does the `.usmap` actually say about `FBox2f`?
//!
//! Nine data tables re-encode with the wrong length there, and the block records
//! a 0-slot schema while `flattened_schema` reports 3. One of those two readings
//! is not what it looks like, and guessing has already cost two wrong fixes.
use blam_tags::iostore::usmap::Usmap;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap").into()
    });
    let mut usmap = match std::fs::read(&path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);
    for name in ["Box2f", "Box2D", "SlateBrush"] {
        match usmap.get(name) {
            None => println!("{name}: absent from the .usmap"),
            Some(s) => {
                println!(
                    "{name}: {} own properties, super {:?}, flattened {:?}",
                    s.properties.len(),
                    s.super_name,
                    usmap.flattened_owned_slots(name).map(|v| v.len()),
                );
                for p in &s.properties {
                    println!("    [{}] {} {:?} array_dim {}", p.schema_index, p.name, p.ty, p.array_dim);
                }
            }
        }
    }
}
