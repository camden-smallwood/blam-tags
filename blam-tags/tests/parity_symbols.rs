//! Names every engine symbol listed in the workspace `parity.toml`.
//!
//! `parity_symbols/generated.rs` is written from the `[engine.*]` tables by
//! `tests/parity_manifest.rs`: one function per capability area, gated on
//! the area's features, naming each symbol. Renaming or removing a listed
//! symbol breaks this build until `parity.toml` is updated and the file is
//! regenerated:
//!
//! ```sh
//! BLAM_PARITY_REGENERATE=1 cargo test -p blam-tags --test parity_manifest
//! ```
//!
//! Build with `--all-features` to check the feature-gated areas too.

#[allow(dead_code, unused_imports)]
#[path = "parity_symbols/generated.rs"]
mod generated;
