//! Generates the PyO3 bindings for `blam-tags` from rustdoc JSON.
//!
//! ```text
//! cargo run -p blam-tags-bindgen [-- <rustdoc json> [<bindings.toml>]]
//! ```
//!
//! With no arguments it documents `blam-tags` itself first, with the pinned
//! nightly in [`RUSTDOC_TOOLCHAIN`], the features `blam-tags-py` enables by
//! default, and a target directory of its own. So the input is always the
//! engine as it stands: a JSON left in `target/doc` could be from before the
//! last edit, from another toolchain, or deleted by a plain `cargo doc`. A path
//! given on the command line is read as it is, for when that is the point.
//!
//! Only this generator needs nightly (`rustup toolchain install` the pinned
//! one). Its output is checked into the repository, so the `blam-tags-py`
//! wheel builds on stable. CI regenerates it and fails on any difference.

mod emit;
mod policy;
mod rdoc;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The nightly whose rustdoc JSON this generator reads. The format is
/// unstable and changes between nightlies; [`rdoc::EXPECTED_FORMAT_VERSION`]
/// is this toolchain's. CI installs the same one.
const RUSTDOC_TOOLCHAIN: &str = "nightly-2026-07-02";

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
    let json = match args.next() {
        Some(json) => json,
        None => document_engine(&root)?.display().to_string(),
    };
    let manifest = args
        .next()
        .unwrap_or_else(|| root.join("blam-tags-py/bindings.toml").display().to_string());

    let krate = rdoc::Crate::load(&json)?;
    let policy = policy::Policy::load(&manifest)?;

    // A module the policy does not name used to be skipped silently, so a
    // new engine module never showed up as a decision anyone made. Every
    // public module must now be listed, even if only to skip it.
    let unlisted: Vec<String> = krate
        .public_top_modules()
        .into_iter()
        .filter(|m| !policy.module.contains_key(m))
        .collect();
    if unlisted.is_empty() && krate.public_top_modules().is_empty() {
        return Err(format!("{json}: found no public modules in the crate root"));
    }
    if !unlisted.is_empty() {
        return Err(format!(
            "{} public module(s) not listed in {manifest}; add a `[module.<name>]` \
             entry (strategy \"auto\", or \"skip\" with a reason) for each:\n  {}",
            unlisted.len(),
            unlisted.join("\n  ")
        ));
    }

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

/// Run rustdoc over `blam-tags` and return the JSON it wrote.
fn document_engine(root: &Path) -> Result<PathBuf, String> {
    let features = python_crate_features(&root.join("blam-tags-py/Cargo.toml"))?;
    let target = root.join("target/bindgen");
    println!("documenting blam-tags with {RUSTDOC_TOOLCHAIN} (features: {features})");
    // `cargo` from PATH, which is rustup's proxy and honours `+toolchain`,
    // rather than `$CARGO`, the stable cargo running this program. The outer
    // cargo's own settings are not this build's.
    let status = Command::new("cargo")
        .arg(format!("+{RUSTDOC_TOOLCHAIN}"))
        .args(["rustdoc", "-p", "blam-tags", "--features", &features])
        .arg("--manifest-path")
        .arg(root.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .args(["--", "-Z", "unstable-options", "--output-format", "json"])
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("RUSTC")
        .env_remove("RUSTDOC")
        .env_remove("CARGO_TARGET_DIR")
        .status()
        .map_err(|e| format!("running cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "rustdoc failed ({status}); is the toolchain installed? \
             rustup toolchain install {RUSTDOC_TOOLCHAIN}"
        ));
    }
    Ok(target.join("doc/blam_tags.json"))
}

/// The engine features `blam-tags-py` enables by default, comma-separated:
/// each of its default features that forwards to `blam-tags/<name>`. Read
/// from its manifest, since the bindings have to describe the engine the
/// wheel is built against, and a module behind a missing feature would be
/// invisible to the policy check.
fn python_crate_features(manifest: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(manifest)
        .map_err(|e| format!("reading {}: {e}", manifest.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("parsing {}: {e}", manifest.display()))?;
    let features = value.get("features").and_then(toml::Value::as_table);
    let list = |name: &str| -> Vec<String> {
        features
            .and_then(|table| table.get(name))
            .and_then(toml::Value::as_array)
            .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
            .unwrap_or_default()
    };
    let engine: Vec<String> = list("default")
        .iter()
        .flat_map(|feature| list(feature))
        .filter_map(|item| item.strip_prefix("blam-tags/").map(str::to_owned))
        .collect();
    if engine.is_empty() {
        return Err(format!(
            "{}: no default feature forwards to blam-tags",
            manifest.display()
        ));
    }
    Ok(engine.join(","))
}

fn write(path: &std::path::Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}
