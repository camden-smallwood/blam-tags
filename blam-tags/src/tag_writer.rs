//! Helpers the JMS / ASS importers share for filling in a tag built from a
//! schema. Crate-private: each importer surfaces failures through its own
//! error type, which converts from [`MissingField`].

use crate::api::{TagBlockMut, TagStructMut};
use crate::fields::{StringIdData, TagFieldData};

/// A field or block the schema was expected to have and didn't.
pub(crate) struct MissingField(pub String);

/// Run `f` on the block at `path` under `root`.
///
/// `field_path_mut` borrows the struct and `as_block_mut` borrows the field,
/// so the block cannot outlive either — it has to be used in place rather than
/// returned.
pub(crate) fn with_block<T, E: From<MissingField>>(
    root: &mut TagStructMut<'_>,
    path: &str,
    f: impl FnOnce(&mut TagBlockMut<'_>) -> Result<T, E>,
) -> Result<T, E> {
    let mut field = root.field_path_mut(path).ok_or_else(|| MissingField(path.into()))?;
    let mut block = field
        .as_block_mut()
        .ok_or_else(|| MissingField(format!("{path} (not a block)")))?;
    f(&mut block)
}

/// Set a field, ignoring its absence — for fields that differ between schema
/// revisions or carry shipped typos. Returns whether it was set.
pub(crate) fn try_set(element: &mut TagStructMut<'_>, field: &str, value: TagFieldData) -> bool {
    match element.field_mut(field) {
        Some(mut f) => f.set(value).is_ok(),
        None => false,
    }
}

pub(crate) fn string_id(value: &str) -> TagFieldData {
    TagFieldData::StringId(StringIdData { string: value.to_owned() })
}

/// A JMS node list's parent indices as the tag's tree links: each node's first
/// child and next sibling (`-1` for none), the lowest-numbered child at the
/// head of each list. Both have at least one entry.
pub(crate) fn node_links(parents: &[i16]) -> (Vec<i16>, Vec<i16>) {
    let n = parents.len();
    let mut first_child = vec![-1i16; n.max(1)];
    let mut sibling = vec![-1i16; n.max(1)];
    // Backwards, so the lowest-numbered child ends up at the list head.
    for i in (0..n).rev() {
        let p = parents[i];
        if p >= 0 && (p as usize) < n {
            sibling[i] = first_child[p as usize];
            first_child[p as usize] = i as i16;
        }
    }
    (first_child, sibling)
}
