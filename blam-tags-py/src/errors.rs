//! The exception types `blam_tags` raises.
//!
//! Declared by hand rather than generated because both halves of the crate
//! raise them: generated methods map a `Result`'s error onto one of these,
//! and the hand-written facade raises them directly.
//!
//! The generator emits references to `crate::errors::<Name>` and records the
//! set it needs in `COVERAGE.md`. If a new error type appears in `blam-tags`
//! and is not declared here, the build fails — which is the intent. A missing
//! exception should stop the build, not silently widen into a bare
//! `Exception`.

use pyo3::prelude::*;

pyo3::create_exception!(
    blam_tags,
    BlamTagsError,
    pyo3::exceptions::PyException,
    "Catch-all for errors that name no specific type, such as `Box<dyn Error>`."
);

pyo3::create_exception!(
    blam_tags,
    TagReadError,
    BlamTagsError,
    "Raised when a tag file cannot be parsed."
);

pyo3::create_exception!(
    blam_tags,
    StaleHandleError,
    BlamTagsError,
    "Raised when a handle outlived a structural edit that moved its element."
);

/// Register the exception types on the extension module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("BlamTagsError", m.py().get_type::<BlamTagsError>())?;
    m.add("TagReadError", m.py().get_type::<TagReadError>())?;
    m.add("StaleHandleError", m.py().get_type::<StaleHandleError>())?;
    Ok(())
}
