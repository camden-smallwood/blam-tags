//! Census: compile every single-variant `screen` postprocess shader from source
//! and compare bytecode + constant table against the kit's stock tag. Reports
//! the byte-exact match count.
//!
//! ```text
//! cargo run --release --example shader_compile_census --features shader-compile -- \
//!   "D:\...\H3EK\source\rasterizer\hlsl" "D:\...\H3EK\tags\rasterizer\shaders"
//! ```

#[cfg(all(feature = "shader-compile", windows))]
fn main() {
    use blam_tags::shader_compile::entry::Stage;
    use blam_tags::shader_compile::include::DiskSource;
    use blam_tags::shader_compile::macros::Platform;
    use blam_tags::shader_compile::reflect::ConstantEntry;
    use blam_tags::shader_compile::{CompileOutcome, ShaderCompiler};
    use blam_tags::{TagFieldData, TagFile};

    let mut args = std::env::args().skip(1);
    let hlsl_root = args.next().expect("arg1: hlsl source dir");
    let tags_dir = args.next().expect("arg2: rasterizer/shaders tags dir");

    let src = DiskSource::new(&hlsl_root);
    let sc = ShaderCompiler::load(&src, None).expect("load d3dcompiler_47.dll");

    // helper: read a tag's dx9 blob + constant table for compiled shader 0
    fn read_stock(tag: &TagFile) -> Option<(Vec<u8>, Vec<(String, i16, i8, i8)>)> {
        let root = tag.root();
        // only single-compiled-shader tags (one entry, one variant)
        let count = root.field_path("compiled shaders").and_then(|f| f.as_block()).map(|b| b.len())?;
        if count != 1 {
            return None;
        }
        let blob = match root
            .field_path("compiled shaders[0]/compiled shader splut/dx9 compiled shader")?
            .value()?
        {
            TagFieldData::Data(b) => b,
            _ => return None,
        };
        let base = "compiled shaders[0]/compiled shader splut/dx9 rasterizer constant table/constants";
        let mut table = Vec::new();
        if let Some(mut cf) = root.field_path(base) {
            if let Some(cb) = cf.as_block() {
                for i in 0..cb.len() {
                    let el = cb.element(i).unwrap();
                    let name = match el.field("constant name").and_then(|f| f.value()) {
                        Some(TagFieldData::StringId(s)) => s.string,
                        _ => String::new(),
                    };
                    let rs = match el.field("register start").and_then(|f| f.value()) {
                        Some(TagFieldData::ShortInteger(x)) => x,
                        _ => 0,
                    };
                    let rc = match el.field("register count").and_then(|f| f.value()) {
                        Some(TagFieldData::CharInteger(x)) => x,
                        _ => 0,
                    };
                    let set = match el.field("register set").and_then(|f| f.value()) {
                        Some(TagFieldData::CharEnum { value, .. }) => value,
                        _ => 0,
                    };
                    table.push((name, rs, rc, set));
                }
            }
        }
        Some((blob, table))
    }

    let mut total = 0;
    let mut bytecode_ok = 0;
    let mut table_ok = 0;
    let mut mismatches: Vec<String> = Vec::new();

    for ext in ["pixel_shader", "vertex_shader"] {
        let stage = if ext == "vertex_shader" { Stage::Vertex } else { Stage::Pixel };
        let entries = std::fs::read_dir(&tags_dir).unwrap();
        for de in entries.flatten() {
            let path = de.path();
            if path.extension().and_then(|e| e.to_str()) != Some(ext) {
                continue;
            }
            let base = path.file_stem().unwrap().to_string_lossy().to_string();

            // read the source and find the single @generate; only screen singles
            let source = match src_read(&hlsl_root, &base) {
                Some(s) => s,
                None => continue,
            };
            let generates = count_generates(&source);
            if generates != vec!["screen".to_string()] {
                continue; // only single-screen shaders in this census
            }

            let tag = match TagFile::read(&path) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let (stock_blob, stock_table) = match read_stock(&tag) {
                Some(x) => x,
                None => continue,
            };

            let out = match sc.compile_variant(&base, stage, 0, 7, 0, Platform::Pc, &[]) {
                Ok(CompileOutcome::Compiled(o)) => o,
                Ok(CompileOutcome::EntryNotFound) => continue,
                Err(_) => {
                    continue;
                }
            };

            total += 1;
            let bc = out.bytecode == stock_blob;
            if bc {
                bytecode_ok += 1;
            }
            let mine: Vec<(String, i16, i8, i8)> = out
                .table
                .constants
                .iter()
                .map(|c: &ConstantEntry| (c.name.clone(), c.register_start, c.register_count, c.register_set))
                .collect();
            let tb = mine == stock_table;
            if tb {
                table_ok += 1;
            }
            if !bc || !tb {
                mismatches.push(format!("{base}.{ext} (bytecode={bc} table={tb})"));
            }
        }
    }

    println!("\n=== shader compile census (single-screen raw shaders) ===");
    println!("tested:        {total}");
    println!("bytecode byte-identical: {bytecode_ok}/{total}");
    println!("constant table byte-exact: {table_ok}/{total}");
    if !mismatches.is_empty() {
        println!("mismatches ({}):", mismatches.len());
        for m in &mismatches {
            println!("  {m}");
        }
    }
}

#[cfg(all(feature = "shader-compile", windows))]
fn src_read(root: &str, base: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(root);
    std::fs::read(p.join(format!("{base}.hlsl")))
        .or_else(|_| std::fs::read(p.join(format!("{base}.fx"))))
        .ok()
}

/// Collect the `@generate <name>` directive names in a source file.
#[cfg(all(feature = "shader-compile", windows))]
fn count_generates(source: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(source);
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.trim_start_matches('/').trim();
        if let Some(rest) = l.strip_prefix("@generate") {
            let name = rest.trim().split_whitespace().next().unwrap_or("").to_string();
            if !name.is_empty() {
                out.push(name);
            }
        }
    }
    out
}

#[cfg(not(all(feature = "shader-compile", windows)))]
fn main() {
    eprintln!("build with --features shader-compile on Windows");
}
