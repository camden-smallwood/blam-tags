//! The hand-written half of the bindings: the borrowing facade.
//!
//! Every type in `blam_tags::api` borrows from the [`TagFile`] that owns it —
//! `TagStruct<'a>` is three references — and PyO3 classes must be `'static`.
//! So none of them can be wrapped directly.
//!
//! Instead a Python handle stores *coordinates*: the owning file plus an
//! owned [`TagFieldPath`], re-resolved from the root on every access. Path
//! resolution is O(depth) and irrelevant at scripting speed. This is the
//! same shape Baboon already uses, which navigates by re-resolving paths
//! rather than by holding borrows.
//!
//! # Staleness
//!
//! Re-resolution reintroduces a hazard the borrow checker otherwise prevents:
//! hold a handle to element 5, delete element 2, and the path now addresses a
//! *different* element rather than failing.
//!
//! [`TagFile`] therefore records the path of every structural edit. A handle
//! captures how many edits had happened when it was made, and refuses to
//! resolve if any later edit landed on a block that *contains* it. The
//! containment test is what keeps this usable: adding an element to
//! `seats` invalidates handles pointing inside `seats`, but not the tag root,
//! not a sibling field, and not the `seats` block handle itself. Editing a
//! field's value is not structural and invalidates nothing.

use blam_tags::api::{TagBlock, TagStruct};
use blam_tags::{TagFieldPath, TagFile};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::{BlamTagsError, StaleHandleError, TagReadError};

use super::value;

/// A parsed Halo tag file.
///
/// Hand-written rather than generated because it carries the generation
/// counter the facade handles validate against.
#[pyclass(name = "TagFile", module = "blam_tags")]
pub struct PyTagFile(pub TagFile, pub Vec<TagFieldPath>);

impl PyTagFile {
    /// Record a structural edit at `path`, invalidating handles inside it.
    fn record_edit(&mut self, path: TagFieldPath) {
        self.1.push(path);
    }
}

#[pymethods]
impl PyTagFile {
    /// Create a fresh tag from a JSON schema, with one default root element.
    #[staticmethod]
    fn new(schema_path: std::path::PathBuf) -> PyResult<Self> {
        let tag = TagFile::new(schema_path)
            .map_err(|e| BlamTagsError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Read and fully parse the tag file at `path`.
    #[staticmethod]
    fn read(path: std::path::PathBuf) -> PyResult<Self> {
        let tag = TagFile::read(path)
            .map_err(|e| TagReadError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Parse a complete tag file already held in memory.
    #[staticmethod]
    fn read_from_bytes(bytes: Vec<u8>) -> PyResult<Self> {
        let tag = TagFile::read_from_bytes(&bytes)
            .map_err(|e| TagReadError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Serialize to `path`.
    fn write(&self, path: std::path::PathBuf) -> PyResult<()> {
        self.0.write(path).map_err(Into::into)
    }

    /// Serialize to `path` via a temporary file and a rename.
    fn write_atomic(&self, path: std::path::PathBuf) -> PyResult<()> {
        self.0.write_atomic(path).map_err(Into::into)
    }

    /// Serialize to a `bytes` object.
    fn write_to_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        Ok(PyBytes::new(py, &self.0.write_to_bytes()?))
    }

    /// Recompute and store the header checksum.
    fn recompute_checksum(&mut self) {
        self.0.recompute_checksum();
    }

    /// Attach a dependency-list (`want`) stream built from a JSON schema.
    fn add_dependency_list(&mut self, schema_path: std::path::PathBuf) -> PyResult<()> {
        self.0
            .add_dependency_list(schema_path)
            .map_err(|e| BlamTagsError::new_err(e.to_string()))
    }

    /// Drop the dependency-list stream, if present.
    fn remove_dependency_list(&mut self) {
        self.0.remove_dependency_list();
    }

    /// The tag's root struct — the entry point for all field access.
    fn root(slf: Bound<'_, Self>) -> PyTagStruct {
        let generation = slf.borrow().1.len();
        PyTagStruct { tag: slf.unbind(), path: TagFieldPath::new(), generation }
    }

    /// The four-character group tag, e.g. `"bipd"`.
    #[getter]
    fn group_tag(&self) -> String {
        blam_tags::fields::format_group_tag(self.0.group().tag)
    }

    fn __repr__(&self) -> String {
        format!("<TagFile {}>", self.group_tag())
    }
}

/// Refuse a handle that a later structural edit moved out from under.
///
/// Only edits landing on a *containing* block matter. The block handle itself
/// stays valid — its contents shifted, not its identity — but anything
/// addressing a particular element of it does not.
fn check_generation(file: &PyTagFile, captured: usize, path: &TagFieldPath) -> PyResult<()> {
    for edit in file.1.iter().skip(captured) {
        if !edit.is_ancestor_of(path) {
            continue;
        }
        // `with_index` marks an element by subscripting the block's own last
        // segment rather than adding one, so `seats` and `seats[0]` have the
        // same segment count. Depth alone cannot separate the block from its
        // elements — the subscript is what says "inside".
        let depth = edit.segments.len();
        let selects_element = path
            .segments
            .get(depth - 1)
            .map(|seg| seg.index.is_some())
            .unwrap_or(false);
        if path.segments.len() > depth || selects_element {
            return Err(StaleHandleError::new_err(format!(
                "handle {:?} was created before a structural edit to {:?} and no \
                 longer refers to the same element; re-resolve it from the tag root",
                path.to_string(),
                edit.to_string()
            )));
        }
    }
    Ok(())
}

/// Resolve a struct handle's path against the tag root.
fn resolve_struct<'a>(file: &'a PyTagFile, path: &TagFieldPath) -> PyResult<TagStruct<'a>> {
    let root = file.0.root();
    if path.is_empty() {
        return Ok(root);
    }
    root.descend_path(path).ok_or_else(|| {
        pyo3::exceptions::PyLookupError::new_err(format!("path no longer resolves: {path}"))
    })
}

/// A struct instance: the tag root, a block element, or a nested struct.
#[pyclass(name = "TagStruct", module = "blam_tags")]
pub struct PyTagStruct {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    generation: usize,
}

#[pymethods]
impl PyTagStruct {
    /// The struct type's schema name, e.g. `"biped"`.
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, &self.path)?.name().to_string())
    }

    /// Size in bytes of one instance of this struct.
    #[getter]
    fn size(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, &self.path)?.size())
    }

    /// This struct's path from the tag root. Empty for the root itself.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// The names of this struct's fields, in declaration order.
    fn field_names(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, &self.path)?
            .field_names()
            .map(str::to_string)
            .collect())
    }

    /// Every field of this struct, in declaration order.
    ///
    /// The returned handles are created at the *current* edit count, not this
    /// struct's: this handle just validated, so its children start fresh.
    fn fields(&self, py: Python<'_>) -> PyResult<Vec<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, &self.path)?;
        Ok(s.fields()
            .map(|f| PyTagField {
                tag: self.tag.clone_ref(py),
                path: self.path.clone().with_field(&f),
                generation: file.1.len(),
            })
            .collect())
    }

    /// Resolve one field by name. Returns `None` if there is no such field.
    fn field(&self, py: Python<'_>, name: &str) -> PyResult<Option<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, &self.path)?;
        Ok(s.field(name).map(|f| PyTagField {
            tag: self.tag.clone_ref(py),
            path: self.path.clone().with_field(&f),
            generation: file.1.len(),
        }))
    }

    /// Resolve a `/`-separated field path relative to this struct.
    fn field_path(&self, py: Python<'_>, path: &str) -> PyResult<Option<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, &self.path)?;
        let Some(f) = s.field_path(path) else { return Ok(None) };
        // Re-render rather than concatenating strings: the resolver's own
        // ordinal-qualified form is what stays valid amongst same-named
        // siblings.
        let mut full = self.path.clone();
        for seg in TagFieldPath::parse(path).segments.iter().take(
            TagFieldPath::parse(path).segments.len().saturating_sub(1),
        ) {
            full.segments.push(seg.clone());
        }
        full.push_field(&f);
        Ok(Some(PyTagField { tag: self.tag.clone_ref(py), path: full, generation: self.generation }))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match self.name(py) {
            Ok(n) => format!("<TagStruct {n:?} at {:?}>", self.path.to_string()),
            Err(_) => "<TagStruct (stale)>".to_string(),
        }
    }
}

/// A single field within a struct.
#[pyclass(name = "TagField", module = "blam_tags")]
pub struct PyTagField {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    generation: usize,
}

/// Resolve a field handle, or report why it no longer resolves.
macro_rules! field_of {
    ($file:expr, $slf:expr) => {{
        let root = $file.0.root();
        root.field_path_at(&$slf.path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!(
                "field path no longer resolves: {}",
                $slf.path
            ))
        })?
    }};
}

#[pymethods]
impl PyTagField {
    /// The field's raw schema name, markup included.
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).name().to_string())
    }

    /// The field's name with schema markup stripped.
    #[getter]
    fn clean_name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).clean_name().into_owned())
    }

    /// The field's schema type name, e.g. `"real"`.
    #[getter]
    fn type_name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).type_name().to_string())
    }

    /// The field's type as a `TagFieldType`.
    #[getter]
    fn field_type(&self, py: Python<'_>) -> PyResult<crate::generated::PyTagFieldType> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).field_type().into())
    }

    /// This field's path from the tag root.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// The field's value as a native Python object, or `None` for structural
    /// fields (blocks, arrays, structs) that hold no scalar value.
    #[getter]
    fn value(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        match field_of!(file, self).value() {
            None => Ok(py.None()),
            Some(data) => value::to_py(py, &data),
        }
    }

    /// Assign the field's value. The field's on-disk type is preserved — a
    /// Python value substitutes the payload, never the variant.
    fn set(&self, py: Python<'_>, new_value: &Bound<'_, PyAny>) -> PyResult<()> {
        let mut file = self.tag.borrow_mut(py);
        check_generation(&file, self.generation, &self.path)?;

        // Read the current value first: it is what tells us the field's shape.
        let mut data = {
            let root = file.0.root();
            root.field_path_at(&self.path)
                .and_then(|f| f.value())
                .ok_or_else(|| {
                    pyo3::exceptions::PyLookupError::new_err(format!(
                        "field has no assignable value: {}",
                        self.path
                    ))
                })?
        };
        value::apply(&mut data, new_value)?;

        let mut root = file.0.root_mut();
        let mut field = root.field_path_at_mut(&self.path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!(
                "field path no longer resolves: {}",
                self.path
            ))
        })?;
        field
            .set(data)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))
    }

    /// The names of this field's flag bits paired with their bit index, or
    /// `None` if the field is not flags-shaped.
    fn flag_names(&self, py: Python<'_>) -> PyResult<Option<Vec<(u32, String)>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        let Some(parent) = self.path.parent() else {
            return Ok(None);
        };
        let _ = parent;
        Ok(field.value().and_then(|v| match v {
            blam_tags::fields::TagFieldData::ByteFlags { names, .. }
            | blam_tags::fields::TagFieldData::WordFlags { names, .. }
            | blam_tags::fields::TagFieldData::LongFlags { names, .. } => Some(names),
            _ => None,
        }))
    }

    /// Read one named flag bit.
    fn get_flag(&self, py: Python<'_>, name: &str) -> PyResult<bool> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        field
            .flag(name)
            .map(|f| f.is_set())
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(format!("no such flag: {name}")))
    }

    /// Set or clear one named flag bit.
    fn set_flag(&self, py: Python<'_>, name: &str, on: bool) -> PyResult<()> {
        let mut file = self.tag.borrow_mut(py);
        check_generation(&file, self.generation, &self.path)?;
        let mut root = file.0.root_mut();
        let mut field = root.field_path_at_mut(&self.path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!(
                "field path no longer resolves: {}",
                self.path
            ))
        })?;
        let mut flag = field
            .flag_mut(name)
            .ok_or_else(|| pyo3::exceptions::PyKeyError::new_err(format!("no such flag: {name}")))?;
        flag.set(on);
        Ok(())
    }

    /// This field as a nested struct, or `None` if it is not one.
    fn as_struct(&self, py: Python<'_>) -> PyResult<Option<PyTagStruct>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.as_struct().map(|_| PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: self.path.clone(),
            generation: file.1.len(),
        }))
    }

    /// This field as a block, or `None` if it is not one.
    fn as_block(&self, py: Python<'_>) -> PyResult<Option<PyTagBlock>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.as_block().map(|_| PyTagBlock {
            tag: self.tag.clone_ref(py),
            path: self.path.clone(),
            generation: file.1.len(),
        }))
    }

    /// The field's raw `data` payload, or `None` if it is not a data field.
    fn as_data<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.as_data().map(|d| PyBytes::new(py, d)))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match (self.name(py), self.type_name(py)) {
            (Ok(n), Ok(t)) => format!("<TagField {n:?}: {t}>"),
            _ => "<TagField (stale)>".to_string(),
        }
    }
}

/// Resolve a block handle's path to the block itself.
fn resolve_block<'a>(file: &'a PyTagFile, path: &TagFieldPath) -> PyResult<TagBlock<'a>> {
    let root = file.0.root();
    root.field_path_at(path)
        .and_then(|f| f.as_block())
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("block no longer resolves: {path}"))
        })
}

/// A repeating block of struct elements.
#[pyclass(name = "TagBlock", module = "blam_tags")]
pub struct PyTagBlock {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    generation: usize,
}

#[pymethods]
impl PyTagBlock {
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_block(&file, &self.path)?.len())
    }

    /// Element `index`, supporting Python's negative indexing.
    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<PyTagStruct> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let len = resolve_block(&file, &self.path)?.len();
        let resolved = if index < 0 { len as isize + index } else { index };
        if resolved < 0 || resolved as usize >= len {
            return Err(pyo3::exceptions::PyIndexError::new_err(
                "block element index out of range",
            ));
        }
        Ok(PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: self.path.clone().with_index(resolved as usize),
            generation: file.1.len(),
        })
    }

    /// Size in bytes of one element.
    #[getter]
    fn element_size(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_block(&file, &self.path)?.element_size())
    }

    /// This block's path from the tag root.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// Append a default-initialized element and return its index.
    fn add(&self, py: Python<'_>) -> PyResult<usize> {
        self.mutate(py, |b| Ok(b.add_element()))
    }

    /// Insert a default-initialized element at `index`.
    fn insert(&self, py: Python<'_>, index: usize) -> PyResult<()> {
        self.mutate(py, |b| {
            b.insert_element(index)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Duplicate element `index`, returning the new element's index.
    fn duplicate(&self, py: Python<'_>, index: usize) -> PyResult<usize> {
        self.mutate(py, |b| {
            b.duplicate_element(index)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Delete element `index`.
    fn delete(&self, py: Python<'_>, index: usize) -> PyResult<()> {
        self.mutate(py, |b| {
            b.delete_element(index)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Exchange two elements.
    fn swap(&self, py: Python<'_>, i: usize, j: usize) -> PyResult<()> {
        self.mutate(py, |b| {
            b.swap_elements(i, j)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Move an element from one index to another.
    fn move_element(&self, py: Python<'_>, from: usize, to: usize) -> PyResult<()> {
        self.mutate(py, |b| {
            b.move_element(from, to)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Remove every element.
    fn clear(&self, py: Python<'_>) -> PyResult<()> {
        self.mutate(py, |b| {
            b.clear();
            Ok(())
        })
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match self.__len__(py) {
            Ok(n) => format!("<TagBlock {:?} len={n}>", self.path.to_string()),
            Err(_) => "<TagBlock (stale)>".to_string(),
        }
    }
}

impl PyTagBlock {
    /// Run a structural edit, then invalidate every outstanding handle —
    /// including this one, which the caller must re-resolve.
    fn mutate<R>(
        &self,
        py: Python<'_>,
        f: impl FnOnce(&mut blam_tags::api::TagBlockMut<'_>) -> PyResult<R>,
    ) -> PyResult<R> {
        let mut file = self.tag.borrow_mut(py);
        check_generation(&file, self.generation, &self.path)?;
        let result = {
            let mut root = file.0.root_mut();
            let mut field = root.field_path_at_mut(&self.path).ok_or_else(|| {
                pyo3::exceptions::PyLookupError::new_err(format!(
                    "block no longer resolves: {}",
                    self.path
                ))
            })?;
            let mut block = field.as_block_mut().ok_or_else(|| {
                pyo3::exceptions::PyTypeError::new_err("field is not a block")
            })?;
            f(&mut block)?
        };
        file.record_edit(self.path.clone());
        Ok(result)
    }
}
