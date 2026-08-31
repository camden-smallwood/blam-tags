//! End-to-end: drive the byte-order conversion path for a compiled shader with
//! the raw recompiler wired in, and confirm it regenerates byte-exact bytecode
//! instead of refusing.
//!
//! Simulates an Xbox-360 source by marking a kit `pixel_shader` big-endian (the
//! recompile path ignores the source's fields — it rebuilds from HLSL — so the
//! only thing that matters is that the group + endian trigger the upgrade).
//!
//! ```text
//! cargo run --example convert_shader_e2e --features shader-compile -- \
//!   <definitions_root> <kit_tags_root> <hlsl_root> <stock add.pixel_shader>
//! ```

#[cfg(all(feature = "shader-compile", windows))]
fn main() {
    use blam_tags::convert::{
        analyze_conversion_with_shader_recompiler, GameTagIndex, NativeTemplateIndex,
    };
    use blam_tags::shader_compile::include::DiskSource;
    use blam_tags::shader_compile::RawShaderRecompiler;
    use blam_tags::{Endian, TagFieldData, TagFile};
    use std::path::Path;

    let mut a = std::env::args().skip(1);
    let defs = a.next().expect("definitions root");
    let kit_tags = a.next().expect("kit tags root");
    let hlsl_root = a.next().expect("hlsl root");
    let stock_path = a.next().expect("stock add.pixel_shader");

    // Source: a kit pixel_shader, marked big-endian to look like a 360 build.
    let mut source = TagFile::read(&stock_path).expect("read source");
    source.endian = Endian::Be;

    // Kit native templates (so the conversion has a template to start from).
    let groups = GameTagIndex::load(Path::new(&defs), "halo3_mcc").expect("load groups");
    let templates = NativeTemplateIndex::build(Path::new(&kit_tags), &groups);

    // The recompiler, keyed to shader "add".
    let src = DiskSource::new(&hlsl_root);
    let recompiler = RawShaderRecompiler {
        provider: &src,
        dll_path: None,
        base_name: "add".to_string(),
    };

    let draft = analyze_conversion_with_shader_recompiler(
        &source,
        "halo3_mcc",
        "halo3_mcc",
        Path::new(&defs),
        Some(&templates),
        &recompiler,
    )
    .expect("conversion should succeed via recompile, not refuse");

    println!("conversion produced a {} draft", draft.target_group_name);
    for issue in &draft.report.issues {
        println!("  [{:?}] {}", issue.kind, issue.message);
    }

    // The draft's tag should carry the byte-exact recompiled bytecode.
    let mine = draft
        .tag
        .root()
        .field_path("compiled shaders[0]/compiled shader splut/dx9 compiled shader")
        .and_then(|f| f.value())
        .and_then(|v| match v { TagFieldData::Data(b) => Some(b), _ => None })
        .expect("draft dx9 blob");

    let stock = TagFile::read(&stock_path).expect("re-read stock");
    let stock_blob = stock
        .root()
        .field_path("compiled shaders[0]/compiled shader splut/dx9 compiled shader")
        .and_then(|f| f.value())
        .and_then(|v| match v { TagFieldData::Data(b) => Some(b), _ => None })
        .expect("stock dx9 blob");

    println!("\ndraft dx9 blob {} bytes, stock {} bytes", mine.len(), stock_blob.len());
    println!(
        "E2E: {}",
        if mine == stock_blob { "recompiled bytecode BYTE-IDENTICAL ✓" } else { "MISMATCH" }
    );
}

#[cfg(not(all(feature = "shader-compile", windows)))]
fn main() {
    eprintln!("build with --features shader-compile on Windows");
}
