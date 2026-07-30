//! A structured, buildable representation of a tag **field path**.
//!
//! A field path addresses a field somewhere in a tag's data tree, e.g.
//! `damage sections#3[0]/instant responses#5`. [`TagFieldPath`] is the parsed,
//! manipulable form; it round-trips to and from the `/`-separated string the
//! resolver ([`crate::api::TagStruct::field_path`]) consumes.
//!
//! ## Segment grammar
//!
//! ```text
//! segment ::= [ type ':' ] name [ '#' ordinal ] [ '[' index ']' ]
//! ```
//! - `type:` — optional [`crate::TagFieldType`] filter (disambiguates fields
//!   sharing a name). Populated only when the prefix names a real type.
//! - `name` — the **clean** (markup-free) field name (see [`crate::field_name`]).
//!   Because clean names carry none of the grammar characters, a rendered
//!   segment is always unambiguous to parse back.
//! - `#ordinal` — the field's 0-based position in its struct
//!   ([`crate::TagField::ordinal`]); Foundation-style positional addressing that
//!   pins the exact field even when siblings share a name/type.
//! - `[index]` — block / array element index.
//!
//! Parsing is tolerant (see `crate::path::parse_segment`): field-name markup
//! that reuses a grammar character (`ambient color:[0,255]`, `max sounds [1,16]`)
//! is cleaned into the name rather than mis-read as a `type:`/`[index]` token.

use std::fmt;
use std::str::FromStr;

use crate::api::TagField;
use crate::field_name::clean_field_name;

/// One `/`-separated component of a [`TagFieldPath`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TagFieldPathSegment {
    /// Optional `type:` filter — the type-name token, present only when it names
    /// a real [`crate::TagFieldType`]. Compared case-insensitively at resolution.
    pub type_filter: Option<String>,
    /// The clean (markup-free) field name.
    pub name: String,
    /// Optional `#ordinal` positional token.
    pub ordinal: Option<usize>,
    /// Optional `[index]` block/array element selector.
    pub index: Option<usize>,
}

impl TagFieldPathSegment {
    /// A bare-name segment (no type / ordinal / index).
    pub fn new(name: impl Into<String>) -> Self {
        Self { type_filter: None, name: name.into(), ordinal: None, index: None }
    }
}

impl fmt::Display for TagFieldPathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(type_filter) = &self.type_filter {
            write!(f, "{type_filter}:")?;
        }
        f.write_str(&self.name)?;
        if let Some(ordinal) = self.ordinal {
            write!(f, "#{ordinal}")?;
        }
        if let Some(index) = self.index {
            write!(f, "[{index}]")?;
        }
        Ok(())
    }
}

/// A parsed, manipulable field path — a sequence of [`TagFieldPathSegment`]s,
/// outermost first. Round-trips with the resolver's string form via
/// [`Display`](fmt::Display) / [`FromStr`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct TagFieldPath {
    pub segments: Vec<TagFieldPathSegment>,
}

impl TagFieldPath {
    /// An empty path (the tag root).
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Append a segment addressing `field` by clean name + positional ordinal —
    /// the resolvable form (targets this exact field even amid same-named
    /// siblings). Mirrors Baboon's old `append_field_path_for`.
    pub fn push_field(&mut self, field: &TagField<'_>) {
        self.segments.push(TagFieldPathSegment {
            type_filter: None,
            name: clean_field_name(field.name()).into_owned(),
            ordinal: Some(field.ordinal()),
            index: None,
        });
    }

    /// Append a bare-name segment (no ordinal). Prefer `push_field` for
    /// resolvable paths; this is for hand-built or legacy names.
    pub fn push_name(&mut self, name: impl AsRef<str>) {
        self.segments
            .push(TagFieldPathSegment::new(clean_field_name(name.as_ref()).into_owned()));
    }

    /// Builder form of `push_field`.
    pub fn with_field(mut self, field: &TagField<'_>) -> Self {
        self.push_field(field);
        self
    }

    /// Set the element index on the last segment (e.g. select block element `i`).
    pub fn with_index(mut self, index: usize) -> Self {
        if let Some(last) = self.segments.last_mut() {
            last.index = Some(index);
        }
        self
    }

    /// Drop element subscripts (`[N]`) from every segment, keeping ordinals.
    /// Two paths differing only in which parent element was selected normalize
    /// to the same value — used to gate the block clipboard on schema position.
    pub fn strip_element_indices(&self) -> TagFieldPath {
        TagFieldPath {
            segments: self
                .segments
                .iter()
                .map(|s| TagFieldPathSegment { index: None, ..s.clone() })
                .collect(),
        }
    }

    /// Drop both element subscripts (`[N]`) and ordinals (`#N`) — the canonical
    /// form used for collapse-state and search-filter keys.
    pub fn strip_node_indices(&self) -> TagFieldPath {
        TagFieldPath {
            segments: self
                .segments
                .iter()
                .map(|s| TagFieldPathSegment { index: None, ordinal: None, ..s.clone() })
                .collect(),
        }
    }

    /// The parent path (all but the last segment), or `None` at the root.
    pub fn parent(&self) -> Option<TagFieldPath> {
        if self.segments.is_empty() {
            return None;
        }
        Some(TagFieldPath { segments: self.segments[..self.segments.len() - 1].to_vec() })
    }

    /// Whether `self` is a (proper or equal) ancestor of `other`: its segments
    /// are a prefix of `other`'s, comparing node identity (name + ordinal) and
    /// ignoring element indices.
    pub fn is_ancestor_of(&self, other: &TagFieldPath) -> bool {
        self.segments.len() <= other.segments.len()
            && self.segments.iter().zip(&other.segments).all(|(a, b)| {
                a.name == b.name && a.ordinal == b.ordinal && a.type_filter == b.type_filter
            })
    }

    /// For every segment carrying an element index, the (ancestor-path, index)
    /// pair identifying that block element — outermost first. The ancestor path
    /// includes the indexed segment (with its index preserved), matching Baboon's
    /// block-selection keys.
    pub fn ancestor_indices(&self) -> Vec<(TagFieldPath, usize)> {
        let mut out = Vec::new();
        for (i, seg) in self.segments.iter().enumerate() {
            if let Some(index) = seg.index {
                out.push((TagFieldPath { segments: self.segments[..=i].to_vec() }, index));
            }
        }
        out
    }

    /// A human-readable breadcrumb: clean segment names joined by ` › `.
    pub fn breadcrumb(&self) -> String {
        self.segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(" › ")
    }
}

impl fmt::Display for TagFieldPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            write!(f, "{seg}")?;
        }
        Ok(())
    }
}

impl FromStr for TagFieldPath {
    type Err = std::convert::Infallible;

    /// Parse a `/`-separated path, cleaning each segment's name and tolerantly
    /// splitting its `type:`/`#ordinal`/`[index]` tokens. Empty segments are
    /// skipped. Never fails — an unparseable token is folded into the name.
    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let segments = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|segment| {
                let (type_filter, name, ordinal, index) = crate::path::parse_segment(segment);
                TagFieldPathSegment {
                    type_filter: type_filter.map(str::to_owned),
                    name: clean_field_name(name).into_owned(),
                    ordinal,
                    index: index.map(|i| i as usize),
                }
            })
            .collect();
        Ok(TagFieldPath { segments })
    }
}

impl TagFieldPath {
    /// Infallible parse (the `FromStr` impl never errors).
    pub fn parse(path: &str) -> TagFieldPath {
        path.parse().unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(s: &str) -> String {
        TagFieldPath::parse(s).to_string()
    }

    #[test]
    fn round_trips_clean_paths() {
        assert_eq!(rt("color/Mapping#5/Function Type"), "color/Mapping#5/Function Type");
        assert_eq!(rt("params#7[2]"), "params#7[2]");
        assert_eq!(rt("block:regions#1[0]/markers"), "block:regions#1[0]/markers");
    }

    #[test]
    fn cleans_markup_in_segment_names() {
        // Range hint and units are cleaned out of the name; the ordinal survives.
        let p = TagFieldPath::parse("ambient color:[0,255]#5");
        assert_eq!(p.segments.len(), 1);
        assert_eq!(p.segments[0].name, "ambient color");
        assert_eq!(p.segments[0].ordinal, Some(5));
        assert_eq!(p.to_string(), "ambient color#5");
        // Bare bracket range, no ordinal.
        assert_eq!(rt("max sounds [1,16]"), "max sounds");
    }

    #[test]
    fn strip_element_indices_keeps_ordinals() {
        let p = TagFieldPath::parse("damage sections#3[0]/instant responses#5");
        assert_eq!(p.strip_element_indices().to_string(), "damage sections#3/instant responses#5");
    }

    #[test]
    fn strip_node_indices_drops_both() {
        let p = TagFieldPath::parse("damage sections#3[0]/instant responses#5[2]");
        assert_eq!(p.strip_node_indices().to_string(), "damage sections/instant responses");
    }

    #[test]
    fn parent_and_ancestor() {
        let p = TagFieldPath::parse("a#0[1]/b#1[2]/c#2");
        assert_eq!(p.parent().unwrap().to_string(), "a#0[1]/b#1[2]");
        let anc = p.ancestor_indices();
        assert_eq!(anc.len(), 2);
        assert_eq!(anc[0].0.to_string(), "a#0[1]");
        assert_eq!(anc[0].1, 1);
        assert_eq!(anc[1].0.to_string(), "a#0[1]/b#1[2]");
        assert_eq!(anc[1].1, 2);

        let base = TagFieldPath::parse("a#0/b#1");
        assert!(base.is_ancestor_of(&p));
        assert!(!p.is_ancestor_of(&base));
    }

    #[test]
    fn breadcrumb_uses_clean_names() {
        let p = TagFieldPath::parse("damage sections#3[0]/instant responses#5");
        assert_eq!(p.breadcrumb(), "damage sections › instant responses");
    }

    #[test]
    fn with_index_sets_last_segment() {
        let p = TagFieldPath::parse("a#0/b#1").with_index(4);
        assert_eq!(p.to_string(), "a#0/b#1[4]");
    }
}
