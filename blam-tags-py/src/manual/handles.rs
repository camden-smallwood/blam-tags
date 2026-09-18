//! The hand-written half of the bindings: the borrowing facade.
//!
//! Every type in `blam_tags::api` borrows from the [`TagFile`] that owns it —
//! `TagStruct<'a>` is three references — and PyO3 classes must be `'static`.
//! So none of them can be wrapped directly.
//!
//! Instead a Python handle stores *coordinates*: the owning file, an
//! [`Anchor`] naming which top-level struct to start from, and an owned
//! [`TagFieldPath`], re-resolved on every access. Path resolution is O(depth)
//! and irrelevant at scripting speed. This is the same shape Baboon already
//! uses, which navigates by re-resolving paths rather than by holding borrows.
//!
//! # Anchors
//!
//! Most handles start from the tag root, but a tag can also carry a
//! dependency-list, import-info, and asset-depot-storage struct — separate
//! top-level chunks, not reachable through `root()`. The [`Anchor`] on each
//! handle records which one it descends from, so those structs are navigable
//! and editable exactly like the root.
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

use blam_tags::api::{TagArray, TagBlock, TagBlockElement, TagResource, TagStruct, TagStructMut};
use blam_tags::{TagFieldPath, TagFile};
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::{BlamTagsError, StaleHandleError, TagReadError};

use super::value;

/// Which top-level struct a handle resolves from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Root,
    DependencyList,
    ImportInfo,
    AssetDepotStorage,
}

impl Anchor {
    fn label(self) -> &'static str {
        match self {
            Anchor::Root => "root",
            Anchor::DependencyList => "dependency list",
            Anchor::ImportInfo => "import info",
            Anchor::AssetDepotStorage => "asset depot storage",
        }
    }
}

/// The read-only base struct an [`Anchor`] resolves to.
fn base_struct(file: &PyTagFile, anchor: Anchor) -> PyResult<TagStruct<'_>> {
    let opt = match anchor {
        Anchor::Root => Some(file.0.root()),
        Anchor::DependencyList => file.0.dependency_list(),
        Anchor::ImportInfo => file.0.import_info(),
        Anchor::AssetDepotStorage => file.0.asset_depot_storage(),
    };
    opt.ok_or_else(|| {
        pyo3::exceptions::PyLookupError::new_err(format!("no {} attached to this tag", anchor.label()))
    })
}

/// The mutable base struct an [`Anchor`] resolves to.
fn base_struct_mut(file: &mut PyTagFile, anchor: Anchor) -> PyResult<TagStructMut<'_>> {
    let label = anchor.label();
    let opt = match anchor {
        Anchor::Root => Some(file.0.root_mut()),
        Anchor::DependencyList => file.0.dependency_list_mut(),
        Anchor::ImportInfo => file.0.import_info_mut(),
        Anchor::AssetDepotStorage => file.0.asset_depot_storage_mut(),
    };
    opt.ok_or_else(|| {
        pyo3::exceptions::PyLookupError::new_err(format!("no {label} attached to this tag"))
    })
}

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

    /// Build a struct handle rooted at `anchor` if that struct exists.
    fn anchored_struct(slf: &Bound<'_, Self>, anchor: Anchor) -> PyResult<Option<PyTagStruct>> {
        let file = slf.borrow();
        let exists = match anchor {
            Anchor::Root => true,
            Anchor::DependencyList => file.0.dependency_list().is_some(),
            Anchor::ImportInfo => file.0.import_info().is_some(),
            Anchor::AssetDepotStorage => file.0.asset_depot_storage().is_some(),
        };
        if !exists {
            return Ok(None);
        }
        let generation = file.1.len();
        Ok(Some(PyTagStruct {
            tag: slf.clone().unbind(),
            path: TagFieldPath::new(),
            anchor,
            generation,
        }))
    }
}

#[pymethods]
impl PyTagFile {
    /// Create a fresh tag from a JSON schema, with one default root element.
    #[staticmethod]
    fn new(schema_path: std::path::PathBuf) -> PyResult<Self> {
        let tag = TagFile::new(schema_path).map_err(|e| BlamTagsError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Read and fully parse the tag file at `path`.
    #[staticmethod]
    fn read(path: std::path::PathBuf) -> PyResult<Self> {
        let tag = TagFile::read(path).map_err(|e| TagReadError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Parse a complete tag file already held in memory.
    #[staticmethod]
    fn read_from_bytes(bytes: Vec<u8>) -> PyResult<Self> {
        let tag =
            TagFile::read_from_bytes(&bytes).map_err(|e| TagReadError::new_err(e.to_string()))?;
        Ok(Self(tag, Vec::new()))
    }

    /// Read just the dependency references from the tag at `path`, without
    /// parsing the whole file. Returns `None` if the tag carries no
    /// dependency-list stream.
    #[staticmethod]
    fn read_dependency_references(path: std::path::PathBuf) -> PyResult<Option<Vec<(u32, String)>>> {
        TagFile::read_dependency_references(path).map_err(|e| TagReadError::new_err(e.to_string()))
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

    /// Attach an import-info stream built from a JSON schema.
    fn add_import_info(&mut self, schema_path: std::path::PathBuf) -> PyResult<()> {
        self.0
            .add_import_info(schema_path)
            .map_err(|e| BlamTagsError::new_err(e.to_string()))
    }

    /// Drop the import-info stream, if present.
    fn remove_import_info(&mut self) {
        self.0.remove_import_info();
    }

    /// Attach an asset-depot-storage stream built from a JSON schema.
    fn add_asset_depot_storage(&mut self, schema_path: std::path::PathBuf) -> PyResult<()> {
        self.0
            .add_asset_depot_storage(schema_path)
            .map_err(|e| BlamTagsError::new_err(e.to_string()))
    }

    /// Drop the asset-depot-storage stream, if present.
    fn remove_asset_depot_storage(&mut self) {
        self.0.remove_asset_depot_storage();
    }

    /// The tag's root struct — the entry point for all field access.
    fn root(slf: Bound<'_, Self>) -> PyTagStruct {
        let generation = slf.borrow().1.len();
        PyTagStruct {
            tag: slf.unbind(),
            path: TagFieldPath::new(),
            anchor: Anchor::Root,
            generation,
        }
    }

    /// The dependency-list struct, or `None` if the tag has no such stream.
    fn dependency_list(slf: Bound<'_, Self>) -> PyResult<Option<PyTagStruct>> {
        Self::anchored_struct(&slf, Anchor::DependencyList)
    }

    /// The import-info struct, or `None` if the tag has no such stream.
    fn import_info(slf: Bound<'_, Self>) -> PyResult<Option<PyTagStruct>> {
        Self::anchored_struct(&slf, Anchor::ImportInfo)
    }

    /// The asset-depot-storage struct, or `None` if the tag has no such stream.
    fn asset_depot_storage(slf: Bound<'_, Self>) -> PyResult<Option<PyTagStruct>> {
        Self::anchored_struct(&slf, Anchor::AssetDepotStorage)
    }

    /// The four-character group tag, e.g. `"bipd"`.
    #[getter]
    fn group_tag(&self) -> String {
        blam_tags::fields::format_group_tag(self.0.group().tag)
    }

    /// The group's version number.
    #[getter]
    fn group_version(&self) -> u32 {
        self.0.group().version
    }

    /// The wire byte order the tag was read in.
    #[getter]
    fn endian(&self) -> crate::generated::PyEndian {
        self.0.endian.into()
    }

    /// The classic-engine identity, or `None` for a modern (MCC cache) tag.
    #[getter]
    fn classic_engine(&self) -> Option<String> {
        self.0.classic_engine().map(|e| format!("{e:?}"))
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

/// Resolve a struct handle's path against its anchor.
fn resolve_struct<'a>(
    file: &'a PyTagFile,
    anchor: Anchor,
    path: &TagFieldPath,
) -> PyResult<TagStruct<'a>> {
    let base = base_struct(file, anchor)?;
    if path.is_empty() {
        return Ok(base);
    }
    base.descend_path(path).ok_or_else(|| {
        pyo3::exceptions::PyLookupError::new_err(format!("path no longer resolves: {path}"))
    })
}

/// A struct instance: the tag root, a block element, or a nested struct.
#[pyclass(name = "TagStruct", module = "blam_tags")]
pub struct PyTagStruct {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    anchor: Anchor,
    generation: usize,
}

#[pymethods]
impl PyTagStruct {
    /// The struct type's schema name, e.g. `"biped"`.
    #[getter]
    fn name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, self.anchor, &self.path)?.name().to_string())
    }

    /// Size in bytes of one instance of this struct.
    #[getter]
    fn size(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, self.anchor, &self.path)?.size())
    }

    /// This struct's path from its anchor. Empty for an anchor's own root.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// The struct's raw on-disk bytes.
    #[getter]
    fn raw<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(PyBytes::new(py, resolve_struct(&file, self.anchor, &self.path)?.raw()))
    }

    /// The names of this struct's fields, in declaration order.
    fn field_names(&self, py: Python<'_>) -> PyResult<Vec<String>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_struct(&file, self.anchor, &self.path)?
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
        let s = resolve_struct(&file, self.anchor, &self.path)?;
        Ok(s.fields()
            .map(|f| PyTagField {
                tag: self.tag.clone_ref(py),
                path: self.path.clone().with_field(&f),
                anchor: self.anchor,
                generation: file.1.len(),
            })
            .collect())
    }

    /// Every field including padding / skip / explanation entries that
    /// [`fields`] omits. Useful for byte-level layout investigation.
    fn fields_all(&self, py: Python<'_>) -> PyResult<Vec<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, self.anchor, &self.path)?;
        Ok(s.fields_all()
            .map(|f| PyTagField {
                tag: self.tag.clone_ref(py),
                path: self.path.clone().with_field(&f),
                anchor: self.anchor,
                generation: file.1.len(),
            })
            .collect())
    }

    /// Resolve one field by name. Returns `None` if there is no such field.
    fn field(&self, py: Python<'_>, name: &str) -> PyResult<Option<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, self.anchor, &self.path)?;
        Ok(s.field(name).map(|f| PyTagField {
            tag: self.tag.clone_ref(py),
            path: self.path.clone().with_field(&f),
            anchor: self.anchor,
            generation: file.1.len(),
        }))
    }

    /// Resolve a `/`-separated field path relative to this struct.
    fn field_path(&self, py: Python<'_>, path: &str) -> PyResult<Option<PyTagField>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, self.anchor, &self.path)?;
        let Some(f) = s.field_path(path) else {
            return Ok(None);
        };
        // Re-render rather than concatenating strings: the resolver's own
        // ordinal-qualified form is what stays valid amongst same-named
        // siblings.
        let parsed = TagFieldPath::parse(path);
        let mut full = self.path.clone();
        for seg in parsed.segments.iter().take(parsed.segments.len().saturating_sub(1)) {
            full.segments.push(seg.clone());
        }
        full.push_field(&f);
        Ok(Some(PyTagField {
            tag: self.tag.clone_ref(py),
            path: full,
            anchor: self.anchor,
            generation: self.generation,
        }))
    }

    /// Descend a `/`-separated path to a nested struct (not a field).
    fn descend(&self, py: Python<'_>, path: &str) -> PyResult<Option<PyTagStruct>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let s = resolve_struct(&file, self.anchor, &self.path)?;
        let Some(_) = s.descend(path) else {
            return Ok(None);
        };
        let parsed = TagFieldPath::parse(path);
        let mut full = self.path.clone();
        for seg in parsed.segments.iter() {
            full.segments.push(seg.clone());
        }
        Ok(Some(PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: full,
            anchor: self.anchor,
            generation: self.generation,
        }))
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
    anchor: Anchor,
    generation: usize,
}

/// Resolve a field handle, or report why it no longer resolves.
macro_rules! field_of {
    ($file:expr, $slf:expr) => {{
        let base = base_struct(&$file, $slf.anchor)?;
        base.field_path_at(&$slf.path).ok_or_else(|| {
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

    /// The field's display name (markup stripped, unit suffix kept).
    #[getter]
    fn display_name(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).display_name().into_owned())
    }

    /// The field's tooltip / explanation text, if the schema carries one.
    #[getter]
    fn explanation(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).explanation().map(str::to_string))
    }

    /// The field's ordinal position within its struct (padding included).
    #[getter]
    fn ordinal(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).ordinal())
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

    /// For a `data` field, the name of the struct definition its payload
    /// follows, if any.
    #[getter]
    fn data_definition_name(&self, py: Python<'_>) -> PyResult<Option<String>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).data_definition_name().map(str::to_string))
    }

    /// This field's path from its anchor.
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
        // Enum fields also need the schema's full option list to resolve a
        // name the current value does not already carry.
        let (mut data, options) = {
            let base = base_struct(&file, self.anchor)?;
            let field = base.field_path_at(&self.path).ok_or_else(|| {
                pyo3::exceptions::PyLookupError::new_err(format!(
                    "field has no assignable value: {}",
                    self.path
                ))
            })?;
            let options = enum_name_table(&field);
            let data = field.value().ok_or_else(|| {
                pyo3::exceptions::PyLookupError::new_err(format!(
                    "field has no assignable value: {}",
                    self.path
                ))
            })?;
            (data, options)
        };
        value::apply_with_enum_names(&mut data, new_value, options.as_deref())?;

        let anchor = self.anchor;
        let path = self.path.clone();
        let mut base = base_struct_mut(&mut file, anchor)?;
        let mut field = base.field_path_at_mut(&path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("field path no longer resolves: {path}"))
        })?;
        field
            .set(data)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))
    }

    /// The names of this field's flag bits paired with their bit index, or
    /// `None` if the field is not flags-shaped. Only *set* bits appear here;
    /// use [`options`] to enumerate every defined bit.
    fn flag_names(&self, py: Python<'_>) -> PyResult<Option<Vec<(u32, String)>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.value().and_then(|v| match v {
            blam_tags::fields::TagFieldData::ByteFlags { names, .. }
            | blam_tags::fields::TagFieldData::WordFlags { names, .. }
            | blam_tags::fields::TagFieldData::LongFlags { names, .. } => Some(names),
            _ => None,
        }))
    }

    /// For an enum or flags field, the full catalog of options (every defined
    /// variant / bit, not only the current one). `None` for other fields.
    fn options(&self, py: Python<'_>) -> PyResult<Option<PyTagOptions>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.options().map(|o| match o {
            blam_tags::api::TagOptions::Enum { names, current } => PyTagOptions {
                is_enum: true,
                names: names.iter().map(|s| s.to_string()).collect(),
                current,
                flags: Vec::new(),
            },
            blam_tags::api::TagOptions::Flags(items) => PyTagOptions {
                is_enum: false,
                names: items.iter().map(|f| f.name.to_string()).collect(),
                current: None,
                flags: items
                    .iter()
                    .map(|f| (f.bit, f.name.to_string(), f.is_set))
                    .collect(),
            },
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
        let anchor = self.anchor;
        let path = self.path.clone();
        let mut base = base_struct_mut(&mut file, anchor)?;
        let mut field = base.field_path_at_mut(&path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("field path no longer resolves: {path}"))
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
            anchor: self.anchor,
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
            anchor: self.anchor,
            generation: file.1.len(),
        }))
    }

    /// This field as a fixed-count array, or `None` if it is not one.
    fn as_array(&self, py: Python<'_>) -> PyResult<Option<PyTagArray>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.as_array().map(|_| PyTagArray {
            tag: self.tag.clone_ref(py),
            path: self.path.clone(),
            anchor: self.anchor,
            generation: file.1.len(),
        }))
    }

    /// This field as a pageable resource, or `None` if it is not one.
    fn as_resource(&self, py: Python<'_>) -> PyResult<Option<PyTagResource>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let field = field_of!(file, self);
        Ok(field.as_resource().map(|_| PyTagResource {
            tag: self.tag.clone_ref(py),
            path: self.path.clone(),
            anchor: self.anchor,
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

    /// Whether this field's `data` payload is a serialized tag function.
    fn is_function_data(&self, py: Python<'_>) -> PyResult<bool> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).is_function_data())
    }

    /// Parse this field's payload as a tag function, or `None` if it is not
    /// one.
    fn as_function(&self, py: Python<'_>) -> PyResult<Option<PyTagFunction>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(field_of!(file, self).as_function().map(PyTagFunction))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match (self.name(py), self.type_name(py)) {
            (Ok(n), Ok(t)) => format!("<TagField {n:?}: {t}>"),
            _ => "<TagField (stale)>".to_string(),
        }
    }
}

/// The schema's full enum option list for a field, in declaration order, so a
/// name can be resolved to its index even when the stored value is different.
fn enum_name_table(field: &blam_tags::api::TagField<'_>) -> Option<Vec<String>> {
    match field.options()? {
        blam_tags::api::TagOptions::Enum { names, .. } => {
            Some(names.iter().map(|s| s.to_string()).collect())
        }
        blam_tags::api::TagOptions::Flags(_) => None,
    }
}

/// The full option catalog of an enum or flags field.
#[pyclass(name = "TagOptions", module = "blam_tags")]
pub struct PyTagOptions {
    is_enum: bool,
    names: Vec<String>,
    current: Option<i64>,
    flags: Vec<(u32, String, bool)>,
}

#[pymethods]
impl PyTagOptions {
    /// True for an enum field, false for a flags field.
    #[getter]
    fn is_enum(&self) -> bool {
        self.is_enum
    }

    /// True for a flags field.
    #[getter]
    fn is_flags(&self) -> bool {
        !self.is_enum
    }

    /// Every option name, in declaration order. For an enum, the list index is
    /// the option's stored value; for flags, it is the bit position.
    #[getter]
    fn names(&self) -> Vec<String> {
        self.names.clone()
    }

    /// For an enum, the currently-stored option index (or `None` if it did not
    /// resolve). Always `None` for flags.
    #[getter]
    fn current(&self) -> Option<i64> {
        self.current
    }

    /// For an enum, the current option's name, if it resolves.
    #[getter]
    fn current_name(&self) -> Option<String> {
        let idx = self.current?;
        usize::try_from(idx).ok().and_then(|i| self.names.get(i)).cloned()
    }

    /// For a flags field, `(bit, name, is_set)` for every defined bit. Empty
    /// for an enum.
    #[getter]
    fn flags(&self) -> Vec<(u32, String, bool)> {
        self.flags.clone()
    }

    fn __repr__(&self) -> String {
        if self.is_enum {
            format!("<TagOptions enum names={} current={:?}>", self.names.len(), self.current)
        } else {
            format!("<TagOptions flags bits={}>", self.flags.len())
        }
    }
}

/// A parsed tag function (`mapping_function`).
#[pyclass(name = "TagFunction", module = "blam_tags")]
pub struct PyTagFunction(blam_tags::TagFunction);

#[pymethods]
impl PyTagFunction {
    /// The function's primary graph type, e.g. `"Constant"` or `"Linear"`.
    #[getter]
    fn function_type(&self) -> String {
        format!("{:?}", self.0.function_type())
    }

    /// Whether the function evaluates to a single constant value.
    fn is_constant(&self) -> bool {
        self.0.is_constant()
    }

    /// The constant value, if this is a constant function.
    fn as_constant(&self) -> Option<f32> {
        self.0.as_constant()
    }

    /// Whether the output is clamped to the function's range.
    fn is_clamped(&self) -> bool {
        self.0.is_clamped()
    }

    /// Evaluate the function at `input` over `range`.
    fn evaluate(&self, input: f32, range: f32) -> f32 {
        self.0.evaluate(input, range)
    }

    /// Evaluate using the legacy (pre-MCC) range mapping.
    fn evaluate_legacy(&self, input: f32, range: f32) -> f32 {
        self.0.evaluate_legacy(input, range)
    }

    fn __repr__(&self) -> String {
        format!("<TagFunction {}>", self.function_type())
    }
}

/// An opaque snapshot of a block/array element, for copy/paste between blocks.
#[pyclass(name = "TagBlockElement", module = "blam_tags")]
pub struct PyTagBlockElement(TagBlockElement);

#[pymethods]
impl PyTagBlockElement {
    fn __repr__(&self) -> String {
        "<TagBlockElement>".to_string()
    }
}

/// Resolve a block handle's path to the block itself.
fn resolve_block<'a>(
    file: &'a PyTagFile,
    anchor: Anchor,
    path: &TagFieldPath,
) -> PyResult<TagBlock<'a>> {
    let base = base_struct(file, anchor)?;
    base.field_path_at(path)
        .and_then(|f| f.as_block())
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("block no longer resolves: {path}"))
        })
}

/// Resolve an array handle's path to the array itself.
fn resolve_array<'a>(
    file: &'a PyTagFile,
    anchor: Anchor,
    path: &TagFieldPath,
) -> PyResult<TagArray<'a>> {
    let base = base_struct(file, anchor)?;
    base.field_path_at(path)
        .and_then(|f| f.as_array())
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("array no longer resolves: {path}"))
        })
}

/// Resolve a resource handle's path to the resource itself.
fn resolve_resource<'a>(
    file: &'a PyTagFile,
    anchor: Anchor,
    path: &TagFieldPath,
) -> PyResult<TagResource<'a>> {
    let base = base_struct(file, anchor)?;
    base.field_path_at(path)
        .and_then(|f| f.as_resource())
        .ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("resource no longer resolves: {path}"))
        })
}

/// A repeating block of struct elements.
#[pyclass(name = "TagBlock", module = "blam_tags")]
pub struct PyTagBlock {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    anchor: Anchor,
    generation: usize,
}

#[pymethods]
impl PyTagBlock {
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_block(&file, self.anchor, &self.path)?.len())
    }

    /// Whether the block has no elements.
    fn is_empty(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.__len__(py)? == 0)
    }

    /// Element `index`, supporting Python's negative indexing.
    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<PyTagStruct> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let len = resolve_block(&file, self.anchor, &self.path)?.len();
        let resolved = if index < 0 { len as isize + index } else { index };
        if resolved < 0 || resolved as usize >= len {
            return Err(pyo3::exceptions::PyIndexError::new_err(
                "block element index out of range",
            ));
        }
        Ok(PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: self.path.clone().with_index(resolved as usize),
            anchor: self.anchor,
            generation: file.1.len(),
        })
    }

    /// Size in bytes of one element.
    #[getter]
    fn element_size(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_block(&file, self.anchor, &self.path)?.element_size())
    }

    /// This block's path from its anchor.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// The classic-engine block header bytes, if this is a classic tag.
    #[getter]
    fn classic_block_header<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_block(&file, self.anchor, &self.path)?
            .classic_block_header()
            .map(|b| PyBytes::new(py, b)))
    }

    /// An opaque snapshot of element `index`, for [`paste`] into this or
    /// another block.
    fn snapshot(&self, py: Python<'_>, index: usize) -> PyResult<PyTagBlockElement> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        resolve_block(&file, self.anchor, &self.path)?
            .element_snapshot(index)
            .map(PyTagBlockElement)
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("block element index out of range"))
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
    fn move_element(&self, py: Python<'_>, from_: usize, to: usize) -> PyResult<()> {
        self.mutate(py, |b| {
            b.move_element(from_, to)
                .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
        })
    }

    /// Paste a snapshot at `index`, returning the pasted element's index.
    fn paste(&self, py: Python<'_>, index: usize, element: &PyTagBlockElement) -> PyResult<usize> {
        let snapshot = element.0.clone();
        self.mutate(py, |b| {
            b.paste_element(index, &snapshot)
                .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))
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
        let anchor = self.anchor;
        let path = self.path.clone();
        let result = {
            let mut base = base_struct_mut(&mut file, anchor)?;
            let mut field = base.field_path_at_mut(&path).ok_or_else(|| {
                pyo3::exceptions::PyLookupError::new_err(format!("block no longer resolves: {path}"))
            })?;
            let mut block = field
                .as_block_mut()
                .ok_or_else(|| pyo3::exceptions::PyTypeError::new_err("field is not a block"))?;
            f(&mut block)?
        };
        file.record_edit(self.path.clone());
        Ok(result)
    }
}

/// A fixed-count array of struct elements.
#[pyclass(name = "TagArray", module = "blam_tags")]
pub struct PyTagArray {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    anchor: Anchor,
    generation: usize,
}

#[pymethods]
impl PyTagArray {
    fn __len__(&self, py: Python<'_>) -> PyResult<usize> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_array(&file, self.anchor, &self.path)?.len())
    }

    /// Whether the array has no elements.
    fn is_empty(&self, py: Python<'_>) -> PyResult<bool> {
        Ok(self.__len__(py)? == 0)
    }

    /// Element `index`, supporting Python's negative indexing.
    fn __getitem__(&self, py: Python<'_>, index: isize) -> PyResult<PyTagStruct> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let len = resolve_array(&file, self.anchor, &self.path)?.len();
        let resolved = if index < 0 { len as isize + index } else { index };
        if resolved < 0 || resolved as usize >= len {
            return Err(pyo3::exceptions::PyIndexError::new_err(
                "array element index out of range",
            ));
        }
        Ok(PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: self.path.clone().with_index(resolved as usize),
            anchor: self.anchor,
            generation: file.1.len(),
        })
    }

    /// This array's path from its anchor.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// An opaque snapshot of element `index`.
    fn snapshot(&self, py: Python<'_>, index: usize) -> PyResult<PyTagBlockElement> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        resolve_array(&file, self.anchor, &self.path)?
            .element_snapshot(index)
            .map(PyTagBlockElement)
            .ok_or_else(|| pyo3::exceptions::PyIndexError::new_err("array element index out of range"))
    }

    /// Exchange two elements. Arrays are fixed-count, so this is a value edit,
    /// not a structural one.
    fn swap(&self, py: Python<'_>, i: usize, j: usize) -> PyResult<()> {
        let mut file = self.tag.borrow_mut(py);
        check_generation(&file, self.generation, &self.path)?;
        let anchor = self.anchor;
        let path = self.path.clone();
        let mut base = base_struct_mut(&mut file, anchor)?;
        let mut field = base.field_path_at_mut(&path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("array no longer resolves: {path}"))
        })?;
        let mut array = field
            .as_array_mut()
            .ok_or_else(|| pyo3::exceptions::PyTypeError::new_err("field is not an array"))?;
        array
            .swap(i, j)
            .map_err(|e| pyo3::exceptions::PyIndexError::new_err(format!("{e:?}")))
    }

    /// Overwrite element `index` with a snapshot of another element.
    fn replace(&self, py: Python<'_>, index: usize, element: &PyTagBlockElement) -> PyResult<()> {
        let snapshot = element.0.clone();
        let mut file = self.tag.borrow_mut(py);
        check_generation(&file, self.generation, &self.path)?;
        let anchor = self.anchor;
        let path = self.path.clone();
        let mut base = base_struct_mut(&mut file, anchor)?;
        let mut field = base.field_path_at_mut(&path).ok_or_else(|| {
            pyo3::exceptions::PyLookupError::new_err(format!("array no longer resolves: {path}"))
        })?;
        let mut array = field
            .as_array_mut()
            .ok_or_else(|| pyo3::exceptions::PyTypeError::new_err("field is not an array"))?;
        array
            .replace_element(index, &snapshot)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(format!("{e:?}")))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match self.__len__(py) {
            Ok(n) => format!("<TagArray {:?} len={n}>", self.path.to_string()),
            Err(_) => "<TagArray (stale)>".to_string(),
        }
    }
}

/// A pageable resource attached to a field.
#[pyclass(name = "TagResource", module = "blam_tags")]
pub struct PyTagResource {
    tag: Py<PyTagFile>,
    path: TagFieldPath,
    anchor: Anchor,
    generation: usize,
}

#[pymethods]
impl PyTagResource {
    /// The resource kind: `"null"`, `"exploded"`, or `"xsync"`.
    #[getter]
    fn kind(&self, py: Python<'_>) -> PyResult<String> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(format!("{:?}", resolve_resource(&file, self.anchor, &self.path)?.kind()).to_lowercase())
    }

    /// This resource's path from its anchor.
    #[getter]
    fn path(&self) -> String {
        self.path.to_string()
    }

    /// The 8 inline engine bytes stored in the field itself.
    #[getter]
    fn inline_bytes<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyBytes>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(PyBytes::new(
            py,
            resolve_resource(&file, self.anchor, &self.path)?.inline_bytes(),
        ))
    }

    /// The exploded (`tgdt`) payload bytes, or `None` if not an exploded
    /// resource.
    #[getter]
    fn exploded_payload<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_resource(&file, self.anchor, &self.path)?
            .exploded_payload()
            .map(|b| PyBytes::new(py, b)))
    }

    /// The XSync payload bytes, or `None` if not an XSync resource.
    #[getter]
    fn xsync_payload<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        Ok(resolve_resource(&file, self.anchor, &self.path)?
            .xsync_payload()
            .map(|b| PyBytes::new(py, b)))
    }

    /// The resource's payload as a navigable struct, or `None`.
    fn as_struct(&self, py: Python<'_>) -> PyResult<Option<PyTagStruct>> {
        // The resource struct is reached from the field itself, so a struct
        // handle over this field's path resolves to it via `descend_path`'s
        // resource handling.
        let file = self.tag.borrow(py);
        check_generation(&file, self.generation, &self.path)?;
        let has_struct = resolve_resource(&file, self.anchor, &self.path)?
            .as_struct()
            .is_some();
        Ok(has_struct.then(|| PyTagStruct {
            tag: self.tag.clone_ref(py),
            path: self.path.clone(),
            anchor: self.anchor,
            generation: file.1.len(),
        }))
    }

    fn __repr__(&self, py: Python<'_>) -> String {
        match self.kind(py) {
            Ok(k) => format!("<TagResource {k} at {:?}>", self.path.to_string()),
            Err(_) => "<TagResource (stale)>".to_string(),
        }
    }
}
