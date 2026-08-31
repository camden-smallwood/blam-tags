//! Validate the full raw driver + emitter: clone a stock shader tag, recompile
//! it from HLSL with `recompile_raw_into`, and compare every compiled shader's
//! dx9 slot (bytecode + constant table) to the stock tag.
//!
//! ```text
//! cargo run --example shader_recompile --features shader-compile -- \
//!   <hlsl_root> <stock_tag> <base> <ps|vs|cs>
//! ```

#[cfg(all(feature = "shader-compile", windows))]
fn main() {
    use blam_tags::shader_compile::entry::Stage;
    use blam_tags::shader_compile::include::DiskSource;
    use blam_tags::shader_compile::recompile_raw_into;
    use blam_tags::{TagFieldData, TagFile};

    let mut a = std::env::args().skip(1);
    let hlsl_root = a.next().expect("hlsl root");
    let stock_path = a.next().expect("stock tag");
    let base = a.next().expect("base name");
    let stage = match a.next().as_deref() {
        Some("vs") => Stage::Vertex,
        Some("cs") => Stage::Compute,
        _ => Stage::Pixel,
    };

    let src = DiskSource::new(&hlsl_root);
    let stock = TagFile::read(&stock_path).expect("read stock");
    let mut target = TagFile::read(&stock_path).expect("clone stock");

    recompile_raw_into(&mut target, &src, None, &base, stage).expect("recompile");

    // Compare each compiled shader's dx9 blob.
    let count = |t: &TagFile| {
        t.root().field_path("compiled shaders").and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0)
    };
    let blob = |t: &TagFile, i: usize| {
        t.root()
            .field_path(&format!("compiled shaders[{i}]/compiled shader splut/dx9 compiled shader"))
            .and_then(|f| f.value())
            .and_then(|v| match v { TagFieldData::Data(b) => Some(b), _ => None })
            .unwrap_or_default()
    };

    let (sc, tc) = (count(&stock), count(&target));
    println!("== {base} {stage:?} :: stock {sc} compiled, mine {tc} ==");
    let n = sc.min(tc);
    let mut ok = sc == tc;
    for i in 0..n {
        let (s, m) = (blob(&stock, i), blob(&target, i));
        let same = s == m;
        if !same {
            ok = false;
        }
        println!("  compiled[{i}]: stock {} vs mine {} bytes -> {}", s.len(), m.len(),
            if same { "identical" } else { "DIFFERS" });
    }
    println!("RESULT: {}", if ok && sc == tc { "full dx9 match ✓" } else { "mismatch" });

    // structural round-trip
    let out = std::env::temp_dir().join("recompiled_shader.tag");
    target.write_atomic(&out).expect("write");
    TagFile::read(&out).expect("re-read");
    println!("round-trip: ok ({})", out.display());
}

#[cfg(not(all(feature = "shader-compile", windows)))]
fn main() {
    eprintln!("build with --features shader-compile on Windows");
}
