//! Probe why `bitmaps#N[0]/signature` descent fails on an H2 bitmap.
use blam_tags::classic::{read_classic_tag_file, ClassicHeader};
use blam_tags::{TagLayout, TagFile};

fn main() {
    let path = "/Users/camden/Halo/halo2_mcc/tags/globals/loading_screen.bitmap";
    let bytes = std::fs::read(path).unwrap();
    let (_h, _e) = ClassicHeader::parse(&bytes).unwrap();
    let layout = TagLayout::from_json("definitions/halo2_mcc/bitmap.json").unwrap();
    let tag = read_classic_tag_file(&bytes, layout).unwrap();
    let root = tag.root();

    // Find the `bitmaps` block field, report its ordinal + block shape.
    for f in root.fields() {
        if f.clean_name() == "bitmaps" {
            println!("bitmaps field: ordinal={}, raw_name={:?}", f.ordinal(), f.name());
            if let Some(b) = f.as_block() {
                println!("  block len (elements) = {}", b.len());
                println!("  element_size = {}", b.element_size());
                println!("  element(0).is_some() = {}", b.element(0).is_some());
            }
            let ord = f.ordinal();
            println!("resolve 'bitmaps#{ord}'          = {}", root.field_path(&format!("bitmaps#{ord}")).is_some());
            println!("resolve 'bitmaps'                = {}", root.field_path("bitmaps").is_some());
            println!("resolve 'bitmaps#{ord}[0]/signature'   = {}", root.field_path(&format!("bitmaps#{ord}[0]/signature")).is_some());
            println!("resolve 'bitmaps[0]/signature'   = {}", root.field_path("bitmaps[0]/signature").is_some());
            println!("descend 'bitmaps[0]' is_some     = {}", root.descend("bitmaps[0]").is_some());
        }
    }
}
