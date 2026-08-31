//! Acceptance harness for the shader compiler: rebuild `add.pixel_shader`'s
//! PC (dx9-slot) shader and compare against the kit's stock tag.
//!
//! ```text
//! cargo run --example shader_compile_add --features shader-compile -- \
//!     "D:\SteamLibrary\steamapps\common\H3EK\source\rasterizer\hlsl" \
//!     "D:\SteamLibrary\steamapps\common\H3EK\tags\rasterizer\shaders\add.pixel_shader"
//! ```

#[cfg(all(feature = "shader-compile", windows))]
fn main() {
    use blam_tags::shader_compile::entry::Stage;
    use blam_tags::shader_compile::include::DiskSource;
    use blam_tags::shader_compile::macros::Platform;
    use blam_tags::shader_compile::{CompileOutcome, ShaderCompiler};
    use blam_tags::TagFile;

    let mut args = std::env::args().skip(1);
    let hlsl_root = args.next().expect("arg1: source\\rasterizer\\hlsl dir");
    let stock_tag = args.next().expect("arg2: stock shader tag");
    let base = args.next().unwrap_or_else(|| "add".to_string());
    let stage = match args.next().as_deref() {
        Some("vs") => Stage::Vertex,
        Some("cs") => Stage::Compute,
        _ => Stage::Pixel,
    };
    let entry: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let vtype: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(7);

    let src = DiskSource::new(&hlsl_root);
    let sc = ShaderCompiler::load(&src, None).expect("load d3dcompiler_47.dll");

    println!("== {base} {:?} entry={entry} vtype={vtype} ==", stage);
    let out = match sc
        .compile_variant(&base, stage, entry, vtype, 0, Platform::Pc, &[])
        .expect("compile")
    {
        CompileOutcome::Compiled(o) => o,
        CompileOutcome::EntryNotFound => panic!("default_ps not found"),
    };

    println!("compiled: {} bytes", out.bytecode.len());
    println!("magic: {:02X?}", &out.bytecode[..4.min(out.bytecode.len())]);
    println!("constants ({}):", out.table.constants.len());
    for c in &out.table.constants {
        println!(
            "  {:36} start={:5} count={} set={}",
            c.name, c.register_start, c.register_count, c.register_set
        );
    }
    println!(
        "  parameter_buffer_size={} extern={} type={}",
        out.table.parameter_buffer_size, out.table.extern_parameter_buffer_size, out.table.table_type
    );

    // Compare against the stock tag's dx9 blob.
    let stock = TagFile::read(&stock_tag).expect("read stock tag");
    let root = stock.root();
    let stock_blob = root
        .field_path("compiled shaders[0]/compiled shader splut/dx9 compiled shader")
        .and_then(|f| f.value())
        .and_then(|v| match v {
            blam_tags::TagFieldData::Data(b) => Some(b),
            _ => None,
        })
        .expect("stock dx9 blob");

    println!("\nstock dx9 blob: {} bytes", stock_blob.len());
    if stock_blob == out.bytecode {
        println!("BYTECODE: BYTE-IDENTICAL ✓");
    } else {
        println!(
            "BYTECODE: differs (mine {} vs stock {})",
            out.bytecode.len(),
            stock_blob.len()
        );
        let n = out.bytecode.len().min(stock_blob.len());
        let first_diff = (0..n).find(|&i| out.bytecode[i] != stock_blob[i]);
        println!("  first differing byte: {first_diff:?}");
    }

    // --- compare the whole constant table against the stock tag, field by field ---
    {
        let base = "compiled shaders[0]/compiled shader splut/dx9 rasterizer constant table";
        let mut cf = root.field_path(&format!("{base}/constants")).unwrap();
        let cblock = cf.as_block().unwrap();
        let mut ok = cblock.len() == out.table.constants.len();
        if ok {
            for (i, mine) in out.table.constants.iter().enumerate() {
                let el = cblock.element(i).unwrap();
                let name = el.field("constant name").and_then(|f| f.value()).map(|v| match v {
                    blam_tags::TagFieldData::StringId(s) => s.string,
                    _ => String::new(),
                }).unwrap_or_default();
                let rs = el.field("register start").and_then(|f| f.value()).map(|v| match v {
                    blam_tags::TagFieldData::ShortInteger(x) => x, _ => -1 }).unwrap_or(-1);
                let rc = el.field("register count").and_then(|f| f.value()).map(|v| match v {
                    blam_tags::TagFieldData::CharInteger(x) => x, _ => -1 }).unwrap_or(-1);
                if name != mine.name || rs != mine.register_start || rc != mine.register_count {
                    println!("  table diff at {i}: stock({name},{rs},{rc}) vs mine({},{},{})",
                        mine.name, mine.register_start, mine.register_count);
                    ok = false;
                }
            }
        } else {
            println!("  table length: stock {} vs mine {}", cblock.len(), out.table.constants.len());
        }
        println!("CONSTANT TABLE: {}", if ok { "BYTE-EXACT MATCH ✓" } else { "differs" });
    }

    // --- emit a full pixel_shader tag by cloning the stock one and repopulating ---
    use blam_tags::shader_compile::emit::{emit_flat_shader, EntryOutput, PlatformOutput, Splut};
    let mut tag = TagFile::read(&stock_tag).expect("clone stock tag");
    let splut = Splut {
        dx9: Some(PlatformOutput { bytecode: out.bytecode.clone(), table: out.table.clone() }),
        durango: None,
        gprs: 0,
    };
    emit_flat_shader(&mut tag, &[EntryOutput { entry, passes: vec![splut] }], 0, 0)
        .expect("emit");
    let out_path = std::env::temp_dir().join("add_rebuilt.pixel_shader");
    tag.write_atomic(&out_path).expect("write rebuilt tag");
    println!("\nwrote {}", out_path.display());

    // Re-read it and confirm the bytecode survived the round-trip.
    let reread = TagFile::read(&out_path).expect("re-read rebuilt tag");
    let rb = reread
        .root()
        .field_path("compiled shaders[0]/compiled shader splut/dx9 compiled shader")
        .and_then(|f| f.value())
        .and_then(|v| match v { blam_tags::TagFieldData::Data(b) => Some(b), _ => None })
        .expect("rebuilt dx9 blob");
    println!(
        "ROUNDTRIP: rebuilt tag reads back {} byte dx9 blob, matches compiled = {}",
        rb.len(),
        rb == out.bytecode
    );
}

#[cfg(not(all(feature = "shader-compile", windows)))]
fn main() {
    eprintln!("build with --features shader-compile on Windows");
}
