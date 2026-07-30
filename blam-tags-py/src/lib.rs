//! Python bindings for `blam-tags`.
//!
//! The bulk of this crate is machine-generated: [`generated`] is produced by
//! `blam-tags-bindgen` from the rustdoc JSON of `blam-tags` plus the policy in
//! `bindings.toml`. Hand-written wrappers — the lifetime-parameterized facade
//! types that cannot be mechanically wrapped — live alongside it and are
//! registered here.
//!
//! Regenerate with:
//!
//! ```text
//! cargo +nightly rustdoc -p blam-tags --features audio -- \
//!     -Z unstable-options --output-format json
//! cargo run -p blam-tags-bindgen
//! ```

use pyo3::prelude::*;

mod errors;
mod generated;
mod manual;

/// The `blam_tags` extension module.
#[pymodule]
fn blam_tags(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__doc__", "Python bindings for blam-tags — Halo tag file I/O.")?;
    errors::register(m)?;
    generated::register(m)?;
    manual::register(m)?;
    Ok(())
}
