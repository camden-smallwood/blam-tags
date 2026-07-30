//! Shared tree walker for commands that iterate every field in a tag.
//!
//! Before this module, `inspect` / `find` / `deps` each hand-rolled
//! an `as_struct → as_block → as_array → leaf` recursion that
//! differed only in what they emitted per node. The duplication was
//! tolerable at three callers; `export`, `check`, and `data-diff`
//! are about to make it painful.
//!
//! [`walk`] drives the recursion, tracking the current `/`-separated
//! path and nesting depth. A consumer implements [`FieldVisitor`]
//! with whichever of the enter/visit hooks it cares about; defaults
//! cover the "just recurse / do nothing" case.
//!
//! `depth` is 0 at the root-level fields and increments by one for
//! every struct/block-element/array-element the walker steps into.
//! Visitors use it for depth-limited traversal without having to
//! maintain their own counters.

use blam_tags::{TagArray, TagBlock, TagField, TagResource, TagStruct};

/// Returned from container enter-hooks to control whether the walker
/// recurses into the container's children.
pub enum VisitControl {
    Descend,
    Skip,
}

/// Per-node callback surface for [`walk`]. Every method has a
/// sensible default, so implementations only override the hooks they
/// care about.
pub trait FieldVisitor {
    /// If `true`, the walker uses [`TagStruct::fields_all`] (includes
    /// pad / skip / explanation / unknown); otherwise the filtered
    /// [`TagStruct::fields`]. Default: `false`.
    fn include_padding(&self) -> bool { false }

    /// A scalar / string / enum / flag / block-index / etc. leaf.
    /// No recursion follows. Default: noop.
    fn visit_leaf(&mut self, _path: &str, _depth: usize, _field: TagField<'_>) {}

    /// Entering a `pageable_resource` field. The header struct is
    /// reachable via `resource.as_struct()` (Exploded only); the
    /// `tgdt` exploded payload is opaque per-group bytes and is not
    /// walked. Return `Skip` to suppress descent into the header.
    /// Default: `Descend`.
    fn enter_resource(
        &mut self,
        _path: &str,
        _depth: usize,
        _field: TagField<'_>,
        _resource: TagResource<'_>,
    ) -> VisitControl {
        VisitControl::Descend
    }

    /// Entering a struct field (the nested struct is reachable via
    /// `field.as_struct()`). Return `Skip` to suppress recursion.
    /// Default: `Descend`.
    fn enter_struct(
        &mut self,
        _path: &str,
        _depth: usize,
        _field: TagField<'_>,
    ) -> VisitControl {
        VisitControl::Descend
    }

    /// Entering a block field. The walker iterates elements on
    /// `Descend`, calling [`FieldVisitor::enter_element`] before each
    /// element's body. Default: `Descend`.
    fn enter_block(
        &mut self,
        _path: &str,
        _depth: usize,
        _field: TagField<'_>,
        _block: TagBlock<'_>,
    ) -> VisitControl {
        VisitControl::Descend
    }

    /// Entering an array field. Same shape as `enter_block`. Default: `Descend`.
    fn enter_array(
        &mut self,
        _path: &str,
        _depth: usize,
        _field: TagField<'_>,
        _array: TagArray<'_>,
    ) -> VisitControl {
        VisitControl::Descend
    }

    /// Called immediately before the walker recurses into element
    /// `index` of a block or array. `path` includes the `[index]`
    /// suffix; `elem` is the element struct, exposed so visitors can
    /// inspect its fields and decide whether the body recursion is
    /// needed (e.g. inline single-leaf elements). Return `Skip` to
    /// suppress the body walk. Default: `Descend`.
    fn enter_element(
        &mut self,
        _path: &str,
        _depth: usize,
        _index: usize,
        _elem: TagStruct<'_>,
    ) -> VisitControl {
        VisitControl::Descend
    }
}

/// Walk every field under `start`, calling the visitor's hooks as it
/// goes. The path passed to hooks is relative to `start` (empty
/// string at the root).
pub fn walk<V: FieldVisitor>(start: TagStruct<'_>, visitor: &mut V) {
    let mut path = String::new();
    walk_struct(start, &mut path, 0, visitor);
}

fn walk_struct<V: FieldVisitor>(
    s: TagStruct<'_>,
    path: &mut String,
    depth: usize,
    visitor: &mut V,
) {
    let fields: Vec<TagField<'_>> = if visitor.include_padding() {
        s.fields_all().collect()
    } else {
        s.fields().collect()
    };

    for field in fields {
        let saved = path.len();
        append_segment(path, field.name());

        if let Some(nested) = field.as_struct() {
            if matches!(visitor.enter_struct(path, depth, field), VisitControl::Descend) {
                walk_struct(nested, path, depth + 1, visitor);
            }
        } else if let Some(block) = field.as_block() {
            if matches!(visitor.enter_block(path, depth, field, block), VisitControl::Descend) {
                walk_elements(block.iter(), path, depth, visitor);
            }
        } else if let Some(array) = field.as_array() {
            if matches!(visitor.enter_array(path, depth, field, array), VisitControl::Descend) {
                walk_elements(array.iter(), path, depth, visitor);
            }
        } else if let Some(resource) = field.as_resource() {
            if matches!(visitor.enter_resource(path, depth, field, resource), VisitControl::Descend)
                && let Some(header) = resource.as_struct()
            {
                walk_struct(header, path, depth + 1, visitor);
            }
        } else {
            visitor.visit_leaf(path, depth, field);
        }

        path.truncate(saved);
    }
}

fn walk_elements<'a, V: FieldVisitor>(
    iter: impl Iterator<Item = TagStruct<'a>>,
    path: &mut String,
    depth: usize,
    visitor: &mut V,
) {
    for (i, elem) in iter.enumerate() {
        let saved = path.len();
        use std::fmt::Write;
        let _ = write!(path, "[{i}]");
        if matches!(visitor.enter_element(path, depth + 1, i, elem), VisitControl::Descend) {
            walk_struct(elem, path, depth + 1, visitor);
        }
        path.truncate(saved);
    }
}

fn append_segment(path: &mut String, name: &str) {
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(name);
}
