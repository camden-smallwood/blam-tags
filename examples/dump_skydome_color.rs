use blam_tags::file::TagFile;
use blam_tags::tag_function::TagFunction;
fn main() {
    let path = "/Users/camden/Halo/halo3_mcc/tags/levels/multi/riverworld/sky/shaders/skydome.shader";
    let tag = TagFile::read(path).expect("read");
    let root = tag.root();
    let params = root.field("parameters").and_then(|f| f.as_block()).expect("params block");
    for i in 0..params.len() {
        let p = params.element(i).unwrap();
        let name = p.field("parameter name").and_then(|f| f.value()).map(|v| format!("{:?}", v)).unwrap_or_default();
        if !name.contains("albedo_color") { continue; }
        println!("param {}: {}", i, name);
        let anim = p.field("animated parameters").and_then(|f| f.as_block()).unwrap();
        for k in 0..anim.len() {
            let a = anim.element(k).unwrap();
            let typ = a.field("type").and_then(|f| f.value()).map(|v| format!("{:?}", v)).unwrap_or_default();
            println!("  animated[{}] type={}", k, typ);
            let fdata = a.field_path("function/data").and_then(|f| f.as_data());
            if let Some(bytes) = fdata {
                println!("  fn bytes: {}", bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>());
                match TagFunction::parse(bytes) {
                    Ok(f) => {
                        let h = f.header();
                        println!("  type={:?} flags={:?} color_graph_type={:?}", h.function_type, h.flags, h.color_graph_type);
                        println!("  clamp_min={} clamp_max={}", h.clamp_range_min, h.clamp_range_max);
                        println!("  colors=[0x{:08x}, 0x{:08x}, 0x{:08x}, 0x{:08x}]",
                                 h.colors[0], h.colors[1], h.colors[2], h.colors[3]);
                        // Decode colors[0] as ARGB
                        let v = h.colors[0];
                        let a = ((v >> 24) & 0xff) as f32 / 255.0;
                        let r = ((v >> 16) & 0xff) as f32 / 255.0;
                        let g = ((v >> 8) & 0xff) as f32 / 255.0;
                        let b = (v & 0xff) as f32 / 255.0;
                        println!("  decoded RGBA: ({}, {}, {}, {})", r, g, b, a);
                    }
                    Err(e) => println!("  parse error: {:?}", e),
                }
            }
        }
    }
}
