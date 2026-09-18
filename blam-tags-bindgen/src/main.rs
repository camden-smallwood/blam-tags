//! Generates the PyO3 bindings for `blam-tags` from rustdoc JSON.
//!
//! ```text
//! cargo +nightly rustdoc -p blam-tags --features audio -- \
//!     -Z unstable-options --output-format json
//! cargo run -p blam-tags-bindgen
//! ```
//!
//! Only this generator needs nightly. Its output is checked into the
//! repository, so the `blam-tags-py` wheel builds on stable.

mod emit;
mod policy;
mod rdoc;

use std::path::PathBuf;

fn main() {
    if let Err(e) = run() {
        eprintln!("blam-tags-bindgen: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no workspace root")?
        .to_path_buf();

    let mut args = std::env::args().skip(1);
    let json = args
        .next()
        .unwrap_or_else(|| root.join("target/doc/blam_tags.json").display().to_string());
    let manifest = args
        .next()
        .unwrap_or_else(|| root.join("blam-tags-py/bindings.toml").display().to_string());

    let krate = rdoc::Crate::load(&json)?;
    let policy = policy::Policy::load(&manifest)?;

    let mut emitter = emit::Emitter::new(&krate, &policy);
    let (rs, mut pyi, coverage) = emitter.run();

    let out = root.join("blam-tags-py");

    // The `manual/` facade types are skipped by the generator (they carry
    // lifetimes that cannot be mechanically wrapped), so their stubs are
    // hand-written. Append them verbatim so the `.pyi` covers the whole module.
    let manual_pyi = out.join("manual.pyi");
    if manual_pyi.exists() {
        let stubs = std::fs::read_to_string(&manual_pyi)
            .map_err(|e| format!("reading {}: {e}", manual_pyi.display()))?;
        pyi.push('\n');
        pyi.push_str(&stubs);
    }

    write(&out.join("src/generated.rs"), &rs)?;
    write(&out.join("blam_tags.pyi"), &pyi)?;
    write(&out.join("COVERAGE.md"), &coverage)?;

    let classes = rs.matches("#[pyclass(").count();
    let methods = rs.matches("    fn ").count();
    println!("generated {classes} classes, {methods} methods");
    println!("  src/generated.rs, blam_tags.pyi, COVERAGE.md under {}", out.display());
    Ok(())
}

fn write(path: &std::path::Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}
