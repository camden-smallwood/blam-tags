use blam_tags::TagFile;
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(||
        "/Users/camden/Halo/halo3_mcc/tags/globals/effect_globals.effect_globals".into());
    let t = TagFile::read(&path).unwrap();
    let root = t.root();
    println!("ROOT fields:");
    for f in root.fields() {
        println!("  type={:?}  name={:?}", f.field_type(), f.name());
    }
    if let Some(block) = root.field("holdbacks").and_then(|f| f.as_block()) {
        println!("\nholdbacks[{}] sample 0:", block.len());
        if let Some(elem) = block.element(0) {
            for f in elem.fields() {
                let v = f.value();
                println!("  type={:?}  name={:?}  value={:?}", f.field_type(), f.name(), v);
            }
            if let Some(pb) = elem.field("priorities").and_then(|f| f.as_block()) {
                println!("\n  priorities[{}]:", pb.len());
                for i in 0..pb.len() {
                    if let Some(pe) = pb.element(i) {
                        println!("    --- priority[{}] ---", i);
                        for f in pe.fields() {
                            println!("      type={:?}  name={:?}  value={:?}", f.field_type(), f.name(), f.value());
                        }
                    }
                }
            }
        }
    }
}
