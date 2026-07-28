//! Probe: dump a loose `physics_model`'s rigid bodies and container blocks so a
//! shape that still comes out unparented can be traced.
//!
//! Run:
//!   cargo run -p blam-tags --example phmo_chain -- <path.physics_model>

use blam_tags::file::TagFile;

fn main() {
    let path = std::env::args().nth(1).expect("physics_model path");
    let tag = TagFile::read(&path).expect("read");
    let root = tag.root();

    for name in ["nodes", "rigid bodies", "spheres", "pills", "boxes", "polyhedra", "lists", "list shapes", "mopps"] {
        if let Some(b) = root.field_path(name).and_then(|f| f.as_block()) {
            println!("  {name}: {}", b.len());
        }
    }

    if let Some(rbs) = root.field_path("rigid bodies").and_then(|f| f.as_block()) {
        println!("\n  rigid bodies:");
        for i in 0..rbs.len() {
            let rb = rbs.element(i).unwrap();
            let node = rb.read_int_any("node").map(|v| v as i64).unwrap_or(-1);
            let (ty, idx) = rb
                .field("shape reference")
                .and_then(|f| f.as_struct())
                .map(|sr| {
                    (
                        sr.read_int_any("shape type").unwrap_or(-1) as i64,
                        sr.read_int_any("shape").unwrap_or(-1) as i64,
                    )
                })
                .unwrap_or((-1, -1));
            println!("    [{i}] node={node} type={ty} index={idx}");
        }
    }
    if let Some(mopps) = root.field_path("mopps").and_then(|f| f.as_block()) {
        println!("\n  mopps:");
        for i in 0..mopps.len() {
            println!(
                "    [{i}] list={}",
                mopps.element(i).unwrap().read_block_index("list")
            );
        }
    }
    if let Some(lists) = root.field_path("lists").and_then(|f| f.as_block()) {
        println!("\n  lists:");
        for i in 0..lists.len() {
            println!(
                "    [{i}] child shapes size={:?}",
                lists.element(i).unwrap().read_int_any("child shapes size")
            );
        }
    }
    if let Some(ls) = root.field_path("list shapes").and_then(|f| f.as_block()) {
        println!("\n  list shapes:");
        for i in 0..ls.len().min(60) {
            let e = ls.element(i).unwrap();
            let (ty, idx) = e
                .field("shape reference")
                .and_then(|f| f.as_struct())
                .map(|sr| {
                    (
                        sr.read_int_any("shape type").unwrap_or(-1) as i64,
                        sr.read_int_any("shape").unwrap_or(-1) as i64,
                    )
                })
                .unwrap_or((-1, -1));
            println!("    [{i}] type={ty} index={idx}");
        }
    }
}
