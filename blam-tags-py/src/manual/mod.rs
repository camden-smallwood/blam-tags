//! Hand-written wrappers.
//!
//! Everything here exists because it cannot be generated: the `api` facade
//! types borrow from the `TagFile` that owns them, and `TagFieldData` is a
//! 57-variant enum whose Python shape is a design decision rather than a
//! mechanical translation.
//!
//! The generator still knows about these types — the policy marks them
//! `manual`, which registers them so generated code can name `PyTagFile` in
//! its signatures, while leaving the definitions here.

mod handles;
mod value;

pub use handles::{
    PyTagArray, PyTagBlock, PyTagBlockElement, PyTagField, PyTagFile, PyTagFunction, PyTagOptions,
    PyTagResource, PyTagStruct,
};

use pyo3::prelude::*;

/// Register the hand-written classes on the extension module.
pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTagFile>()?;
    m.add_class::<PyTagStruct>()?;
    m.add_class::<PyTagField>()?;
    m.add_class::<PyTagBlock>()?;
    m.add_class::<PyTagArray>()?;
    m.add_class::<PyTagResource>()?;
    m.add_class::<PyTagOptions>()?;
    m.add_class::<PyTagFunction>()?;
    m.add_class::<PyTagBlockElement>()?;
    Ok(())
}
