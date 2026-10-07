//! Structural comparison between two tag layouts — an *expected* layout
//! (typically built from a per-group JSON via [`TagFile::new`]) and an
//! *actual* one (typically an MCC/Reach tag parsed from bytes via
//! [`TagFile::read`] / [`TagFile::read_from_bytes`], carrying its own
//! embedded `blay` layout).
//!
//! This is the reusable core behind the corpus-wide schema-match sweeps
//! and behind the app's
//! "does this imported tag match the definitions we ship?" check. It
//! compares the two tags' **root structs**: header group + version,
//! root-struct byte size, field count, and a normalized,
//! wire-significant, LCS-aligned field-by-field diff.
//!
//! Dev-era layout drift (Bungie/343 reshaped struct definitions
//! throughout development, and each tag carries the layout current at
//! *its* save time) means a perfectly valid tag can still disagree with
//! the latest schema. Callers decide policy from [`LayoutSeverity`]:
//! [`Incompatible`](LayoutSeverity::Incompatible) is the unambiguous
//! "wrong kind of tag" (different group), while
//! [`Drift`](LayoutSeverity::Drift) is a softer "same group, shape
//! moved" signal.

use crate::definition::{TagFieldDefinition, TagStructDefinition};
use crate::fields::TagFieldType;
use crate::file::TagFile;

/// One field reduced to a `(type_name, normalized_name)` tuple for
/// set-style diffing. See [`field_key`] for the normalization rules.
pub type FieldKey = (String, String);

/// Rolled-up verdict for a [`LayoutComparison`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutSeverity {
    /// Group, version, root size, field count all agree and no
    /// wire-significant root-field drift — a clean match.
    Match,
    /// Same group tag, but something about the root struct's shape
    /// (group_version, size, field count, or field list) drifted. The
    /// tag is still editable, but may not be byte-compatible with what
    /// the base game expects for this group.
    Drift,
    /// Different group tag — almost certainly the wrong tag or wrong
    /// game entirely.
    Incompatible,
}

/// A single aligned row of the root-struct field diff. Exactly one side
/// is `None` for an inserted/removed field; both are `Some` (and equal)
/// rows are omitted from [`LayoutComparison::field_diffs`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    /// The field present on the expected (schema) side, if any.
    pub expected: Option<FieldKey>,
    /// The field present on the actual (imported tag) side, if any.
    pub actual: Option<FieldKey>,
}

/// Result of [`compare_root_layout`].
#[derive(Debug, Clone)]
pub struct LayoutComparison {
    pub group_match: bool,
    pub version_match: bool,
    pub root_size_match: bool,
    pub field_count_match: bool,

    pub expected_group: u32,
    pub actual_group: u32,
    pub expected_version: u32,
    pub actual_version: u32,
    pub expected_root_size: usize,
    pub actual_root_size: usize,
    pub expected_field_count: usize,
    pub actual_field_count: usize,

    /// Wire-significant root-struct field differences, LCS-aligned and
    /// filtered to only the rows that differ. Empty when the field
    /// lists agree.
    pub field_diffs: Vec<FieldDiff>,

    pub severity: LayoutSeverity,
}

impl LayoutComparison {
    /// `true` only when the severity is [`LayoutSeverity::Match`].
    pub fn is_match(&self) -> bool {
        self.severity == LayoutSeverity::Match
    }
}

/// Compare the root struct of `expected` against `actual`.
///
/// `expected` is conventionally the JSON-derived layout
/// ([`TagFile::new`]) and `actual` the tag whose bytes we're validating,
/// but the comparison is symmetric in everything except which side is
/// labeled expected/actual in the result.
pub fn compare_root_layout(expected: &TagFile, actual: &TagFile) -> LayoutComparison {
    let expected_group = expected.header.group_tag;
    let actual_group = actual.header.group_tag;
    let expected_version = expected.header.group_version;
    let actual_version = actual.header.group_version;

    let exp_root = expected.definitions().root_struct();
    let act_root = actual.definitions().root_struct();

    let expected_root_size = exp_root.size();
    let actual_root_size = act_root.size();
    let expected_field_count = exp_root.fields().count();
    let actual_field_count = act_root.fields().count();

    let group_match = expected_group == actual_group;
    let version_match = expected_version == actual_version;
    let root_size_match = expected_root_size == actual_root_size;
    let field_count_match = expected_field_count == actual_field_count;

    // Only walk the (relatively expensive) field alignment when the
    // coarse checks already agree on shape; when sizes/counts differ we
    // still want the aligned diff to explain *why*.
    let field_diffs = diff_fields(exp_root, act_root);

    let severity = if !group_match {
        LayoutSeverity::Incompatible
    } else if !version_match || !root_size_match || !field_count_match || !field_diffs.is_empty() {
        LayoutSeverity::Drift
    } else {
        LayoutSeverity::Match
    };

    LayoutComparison {
        group_match,
        version_match,
        root_size_match,
        field_count_match,
        expected_group,
        actual_group,
        expected_version,
        actual_version,
        expected_root_size,
        actual_root_size,
        expected_field_count,
        actual_field_count,
        field_diffs,
        severity,
    }
}

/// Wire-significant fields of a struct as normalized keys, in
/// declaration order. Drops editor sentinels whose names aren't
/// reliably preserved across dumper / serializer pairings (`custom`
/// group markers / function descriptors, 0-byte `explanation` text, and
/// the implicit list `terminator`). Padding and every real wire-data
/// type are kept (and thus contribute to field-list drift).
fn collect_fields(s: TagStructDefinition<'_>) -> Vec<FieldKey> {
    s.fields()
        .filter(|f| {
            !matches!(
                f.field_type(),
                TagFieldType::Custom | TagFieldType::Explanation | TagFieldType::Terminator,
            )
        })
        .map(field_key)
        .collect()
}

/// Reduce a field to `(type_name, normalized_name)`. The name is cleaned
/// with [`crate::clean_field_name`] (Foundation semantics — everything
/// from the first markup marker on: `&#:[{*!^|` — is dropped), so
/// cosmetic annotation drift between dumper passes doesn't fire false
/// diffs.
pub fn field_key(f: TagFieldDefinition<'_>) -> FieldKey {
    (
        f.type_name().to_owned(),
        crate::clean_field_name(f.name()).into_owned(),
    )
}

/// LCS-align the two structs' wire-significant field lists and return
/// only the differing rows (a matched field yields no row).
fn diff_fields(expected: TagStructDefinition<'_>, actual: TagStructDefinition<'_>) -> Vec<FieldDiff> {
    let a = collect_fields(expected);
    let b = collect_fields(actual);
    align_lcs(&a, &b)
        .into_iter()
        .filter(|(l, r)| l != r)
        .map(|(expected, actual)| FieldDiff { expected, actual })
        .collect()
}

/// Produce an LCS-aligned merge of two ordered sequences: a sequence of
/// `(left, right)` rows where matched items appear on both sides
/// (equal keys) and unmatched items appear on a single side. Preserves
/// relative order on each side.
fn align_lcs(a: &[FieldKey], b: &[FieldKey]) -> Vec<(Option<FieldKey>, Option<FieldKey>)> {
    lcs_align(a.len(), b.len(), |i, j| a[i] == b[j])
        .into_iter()
        .map(|(i, j)| (i.map(|i| a[i].clone()), j.map(|j| b[j].clone())))
        .collect()
}

/// Align two sequences of `n` and `m` items by longest common subsequence,
/// with `same(i, j)` saying whether `a[i]` and `b[j]` match. Returns the
/// alignment in order as index pairs: both sides for a match, one side for an
/// item only that side has. Ties prefer keeping `a`'s item first.
pub(crate) fn lcs_align(
    n: usize,
    m: usize,
    same: impl Fn(usize, usize) -> bool,
) -> Vec<(Option<usize>, Option<usize>)> {
    // dp[i][j] = LCS length of a[..i] vs b[..j]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if same(i, j) {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (n, m);
    let mut out = Vec::with_capacity(n + m);
    while i > 0 && j > 0 {
        if same(i - 1, j - 1) {
            out.push((Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            out.push((Some(i - 1), None));
            i -= 1;
        } else {
            out.push((None, Some(j - 1)));
            j -= 1;
        }
    }
    while i > 0 {
        out.push((Some(i - 1), None));
        i -= 1;
    }
    while j > 0 {
        out.push((None, Some(j - 1)));
        j -= 1;
    }
    out.reverse();
    out
}

/// How one struct of a tag's own layout differs from the same struct in the
/// current definitions, found by [`diff_layouts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDiff {
    /// Where the struct is reached from the tag's root, as field names joined
    /// with `/` (blocks and arrays without an element index); empty for the
    /// root. A struct used in several places is reported once, at the first.
    pub path: String,
    /// The struct's name in the tag and in the definitions; they differ when
    /// the definitions renamed it.
    pub name: String,
    pub current_name: String,
    /// Its size in bytes in the tag and in the definitions.
    pub size: usize,
    pub current_size: usize,
    /// The fields that differ, in the definitions' order with the tag's own
    /// fields placed where they stood.
    pub fields: Vec<FieldChange>,
}

/// One field that differs between a tag's layout and the definitions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldChange {
    /// The field's name, its markup removed.
    pub name: String,
    pub kind: FieldChangeKind,
    /// Its offset in the tag's struct and in the definitions' struct, where
    /// it has one there.
    pub offset: Option<u32>,
    pub current_offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldChangeKind {
    /// Only in the definitions: added since the tag was saved (or renamed;
    /// a rename shows as a removal and an addition).
    Added { type_name: String },
    /// Only in the tag: removed from the definitions since, or renamed.
    Removed { type_name: String },
    /// In both, with a different type.
    Retyped { from: String, to: String },
    /// In both, at a different place among the struct's fields.
    Moved,
    /// A block whose maximum element count changed.
    BlockMaximum { from: u32, to: u32 },
    /// An inline array whose length changed.
    ArrayLength { from: u32, to: u32 },
}

/// Every difference between a tag's own layout and its group's layout in the
/// current definitions, struct by struct.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayoutDiff {
    /// The tag's group version and the definitions', when they differ.
    pub version: Option<(u32, u32)>,
    pub structs: Vec<StructDiff>,
}

impl LayoutDiff {
    /// Whether the tag's layout is the current one.
    pub fn is_empty(&self) -> bool {
        self.version.is_none() && self.structs.is_empty()
    }

    /// How many fields the definitions have that the tag doesn't, and the
    /// other way round: an older layout lacks fields added since, a newer
    /// or unknown one has fields the definitions don't know.
    pub fn field_counts(&self) -> (usize, usize) {
        let mut added = 0;
        let mut removed = 0;
        for change in self.structs.iter().flat_map(|s| &s.fields) {
            match change.kind {
                FieldChangeKind::Added { .. } => added += 1,
                FieldChangeKind::Removed { .. } => removed += 1,
                _ => {}
            }
        }
        (added, removed)
    }
}

/// Compare `tag`'s own layout with `current`, the layout the definitions give
/// its group (conventionally [`TagFile::new`] on the group's JSON).
///
/// Structs are paired by walking down from the roots through the fields both
/// sides share, struct ids being kept across versions; fields are paired by
/// name, the n-th field of a name with the n-th (Halo 2 repeats names), in
/// order; one that changed places is reported as moved. Unnamed fields,
/// padding, explanations and terminators are left out: they hold no value,
/// and what a change to them does shows in the struct's size.
pub fn diff_layouts(tag: &TagFile, current: &TagFile) -> LayoutDiff {
    let mut diff = LayoutDiff {
        version: (tag.header.group_version != current.header.group_version)
            .then_some((tag.header.group_version, current.header.group_version)),
        structs: Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    diff_struct_pair(
        tag.definitions().root_struct(),
        current.definitions().root_struct(),
        String::new(),
        &mut seen,
        &mut diff.structs,
    );
    diff
}

/// A field the alignment saw removed in one place and added in another is
/// the same field moved: pair them into one [`FieldChangeKind::Moved`] (or a
/// retype, if its type changed too), and compare what it nests.
fn pair_moved_fields<'a>(
    changes: &mut Vec<FieldChange>,
    tag_fields: &[((String, usize), TagFieldDefinition<'a>)],
    current_fields: &[((String, usize), TagFieldDefinition<'a>)],
    path: &str,
    nested: &mut Vec<(TagStructDefinition<'a>, TagStructDefinition<'a>, String)>,
) {
    let removed: Vec<usize> = changes
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.kind, FieldChangeKind::Removed { .. }))
        .map(|(index, _)| index)
        .collect();
    let mut drop = Vec::new();
    for index in removed {
        let name = changes[index].name.clone();
        let Some(added) = changes
            .iter()
            .position(|c| c.name == name && matches!(c.kind, FieldChangeKind::Added { .. }))
        else {
            continue;
        };
        let (from, to) = (changes[index].offset, changes[added].current_offset);
        let field_at = |fields: &[((String, usize), TagFieldDefinition<'a>)], offset| {
            fields.iter().find(|(key, f)| key.0 == name && Some(f.offset()) == offset).map(|(_, f)| *f)
        };
        let (Some(old), Some(new)) = (field_at(tag_fields, from), field_at(current_fields, to)) else {
            continue;
        };
        changes[added] = FieldChange {
            name: name.clone(),
            kind: if old.field_type() == new.field_type() {
                FieldChangeKind::Moved
            } else {
                FieldChangeKind::Retyped { from: old.type_name().to_owned(), to: new.type_name().to_owned() }
            },
            offset: from,
            current_offset: to,
        };
        drop.push(index);
        let child_path = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
        if let (Some(a), Some(b)) = (old.as_struct(), new.as_struct()) {
            nested.push((a, b, child_path));
        } else if let (Some(a), Some(b)) = (old.as_block(), new.as_block()) {
            nested.push((a.struct_definition(), b.struct_definition(), child_path));
        } else if let (Some(a), Some(b)) = (old.as_array(), new.as_array()) {
            nested.push((a.struct_definition(), b.struct_definition(), child_path));
        }
    }
    drop.sort_unstable();
    for index in drop.into_iter().rev() {
        changes.remove(index);
    }
}

/// A struct definition's identity, for reporting each pair of structs once.
type StructIdentity = ([u8; 16], String, usize);

fn struct_identity(s: TagStructDefinition<'_>) -> StructIdentity {
    (s.guid(), s.name().to_owned(), s.size())
}

/// The fields of a struct a layout diff compares, keyed by clean name and
/// how many of that name came before.
fn named_fields(s: TagStructDefinition<'_>) -> Vec<((String, usize), TagFieldDefinition<'_>)> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    s.fields()
        .filter(|f| {
            !matches!(
                f.field_type(),
                TagFieldType::Explanation
                    | TagFieldType::Terminator
                    | TagFieldType::Pad
                    | TagFieldType::UselessPad
                    | TagFieldType::Skip,
            )
        })
        .filter_map(|f| {
            let name = crate::clean_field_name(f.name()).into_owned();
            if name.is_empty() {
                return None;
            }
            let ordinal = seen.entry(name.clone()).or_default();
            let key = (name, *ordinal);
            *ordinal += 1;
            Some((key, f))
        })
        .collect()
}

fn diff_struct_pair(
    tag: TagStructDefinition<'_>,
    current: TagStructDefinition<'_>,
    path: String,
    seen: &mut std::collections::HashSet<(StructIdentity, StructIdentity)>,
    out: &mut Vec<StructDiff>,
) {
    if !seen.insert((struct_identity(tag), struct_identity(current))) {
        return;
    }
    let tag_fields = named_fields(tag);
    let current_fields = named_fields(current);
    let rows = lcs_align(tag_fields.len(), current_fields.len(), |i, j| tag_fields[i].0 == current_fields[j].0);

    let mut changes = Vec::new();
    let mut nested = Vec::new();
    for (i, j) in rows {
        match (i.map(|i| &tag_fields[i]), j.map(|j| &current_fields[j])) {
            (Some(((name, _), field)), None) => changes.push(FieldChange {
                name: name.clone(),
                kind: FieldChangeKind::Removed { type_name: field.type_name().to_owned() },
                offset: Some(field.offset()),
                current_offset: None,
            }),
            (None, Some(((name, _), field))) => changes.push(FieldChange {
                name: name.clone(),
                kind: FieldChangeKind::Added { type_name: field.type_name().to_owned() },
                offset: None,
                current_offset: Some(field.offset()),
            }),
            (Some(((name, _), old)), Some((_, new))) => {
                let change = |kind| FieldChange {
                    name: name.clone(),
                    kind,
                    offset: Some(old.offset()),
                    current_offset: Some(new.offset()),
                };
                if old.field_type() != new.field_type() {
                    changes.push(change(FieldChangeKind::Retyped {
                        from: old.type_name().to_owned(),
                        to: new.type_name().to_owned(),
                    }));
                    continue;
                }
                let child_path = if path.is_empty() { name.clone() } else { format!("{path}/{name}") };
                if let (Some(a), Some(b)) = (old.as_struct(), new.as_struct()) {
                    nested.push((a, b, child_path));
                } else if let (Some(a), Some(b)) = (old.as_block(), new.as_block()) {
                    if a.max_count() != b.max_count() {
                        changes.push(change(FieldChangeKind::BlockMaximum { from: a.max_count(), to: b.max_count() }));
                    }
                    nested.push((a.struct_definition(), b.struct_definition(), child_path));
                } else if let (Some(a), Some(b)) = (old.as_array(), new.as_array()) {
                    if a.count() != b.count() {
                        changes.push(change(FieldChangeKind::ArrayLength { from: a.count(), to: b.count() }));
                    }
                    nested.push((a.struct_definition(), b.struct_definition(), child_path));
                } else if let (Some(a), Some(b)) = (old.as_resource(), new.as_resource()) {
                    nested.push((a.struct_definition(), b.struct_definition(), child_path));
                }
            }
            (None, None) => {}
        }
    }
    pair_moved_fields(&mut changes, &tag_fields, &current_fields, &path, &mut nested);
    if !changes.is_empty() || tag.size() != current.size() || tag.name() != current.name() {
        out.push(StructDiff {
            path,
            name: tag.name().to_owned(),
            current_name: current.name().to_owned(),
            size: tag.size(),
            current_size: current.size(),
            fields: changes,
        });
    }
    for (a, b, child_path) in nested {
        diff_struct_pair(a, b, child_path, seen, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // Definitions live one level above the crate (same relative root the
    // integration tests use, e.g. `../definitions/halo2_mcc`).
    fn tag_from(schema: &str) -> TagFile {
        TagFile::new(Path::new("../definitions").join(schema))
            .unwrap_or_else(|e| panic!("build {schema}: {e}"))
    }

    #[test]
    fn a_tag_built_from_the_definitions_has_the_current_layout() {
        for schema in ["halo3_mcc/scenario.json", "haloreach_mcc/sound_mix.json", "halo2_mcc/biped.json"] {
            let tag = tag_from(schema);
            let current = tag_from(schema);
            assert!(diff_layouts(&tag, &current).is_empty(), "{schema}");
        }
    }

    /// Two layouts of one group, written as definitions: what changed
    /// between them is reported struct by struct, padding left out.
    #[test]
    fn layout_changes_are_reported_struct_by_struct() {
        let dir = std::env::temp_dir().join(format!("layout-diff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |file: &str, root: &str, element: &str| {
            let json = format!(
                r#"{{"name":"layout_test","tag":"lyot","version":1,"flags":0,"block":"layout_test_block",
                    "blocks":{{"layout_test_block":{{"max_count":1,"struct":"layout_test_struct"}},
                              "entries_block":{{"max_count":{max},"struct":"entry_struct"}}}},
                    "structs":{{
                      "layout_test_struct":{{"guid":"00112233445566778899aabbccddeeff","size":{root_size},"fields":[{root},{{"type":"terminator","name":null}}]}},
                      "entry_struct":{{"guid":"ffeeddccbbaa99887766554433221100","size":{element_size},"fields":[{element},{{"type":"terminator","name":null}}]}}}}}}"#,
                max = if file == "old.json" { 8 } else { 16 },
                root_size = if file == "old.json" { 28 } else { 32 },
                element_size = if file == "old.json" { 4 } else { 8 },
            );
            std::fs::write(dir.join(file), json).unwrap();
        };
        write(
            "old.json",
            r#"{"type":"long_integer","name":"kept"},{"type":"short_integer","name":"widened"},{"type":"pad","name":"pad","definition":2},
               {"type":"real","name":"dropped"},{"type":"long_integer","name":"moves"},{"type":"block","name":"entries","definition":"entries_block"}"#,
            r#"{"type":"real","name":"value"}"#,
        );
        write(
            "new.json",
            r#"{"type":"long_integer","name":"moves"},{"type":"long_integer","name":"kept"},{"type":"long_integer","name":"widened"},
               {"type":"real","name":"added"},{"type":"block","name":"entries","definition":"entries_block"},{"type":"long_integer","name":"also added"}"#,
            r#"{"type":"real","name":"value"},{"type":"real","name":"weight"}"#,
        );
        let old = TagFile::new(dir.join("old.json")).unwrap();
        let new = TagFile::new(dir.join("new.json")).unwrap();
        let diff = diff_layouts(&old, &new);
        let _ = std::fs::remove_dir_all(&dir);

        let summary: Vec<(String, Vec<String>)> = diff
            .structs
            .iter()
            .map(|s| {
                let fields = s
                    .fields
                    .iter()
                    .map(|c| match &c.kind {
                        FieldChangeKind::Added { type_name } => format!("+ {} ({type_name})", c.name),
                        FieldChangeKind::Removed { type_name } => format!("- {} ({type_name})", c.name),
                        FieldChangeKind::Retyped { from, to } => format!("~ {}: {from} -> {to}", c.name),
                        FieldChangeKind::Moved => format!("> {}", c.name),
                        FieldChangeKind::BlockMaximum { from, to } => format!("# {}: {from} -> {to}", c.name),
                        FieldChangeKind::ArrayLength { from, to } => format!("[] {}: {from} -> {to}", c.name),
                    })
                    .collect();
                (format!("{} {}->{}", s.path, s.size, s.current_size), fields)
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    " 28->32".to_owned(),
                    vec![
                        "> moves".to_owned(),
                        "~ widened: short integer -> long integer".to_owned(),
                        "+ added (real)".to_owned(),
                        "- dropped (real)".to_owned(),
                        "# entries: 8 -> 16".to_owned(),
                        "+ also added (long integer)".to_owned(),
                    ],
                ),
                ("entries 4->8".to_owned(), vec!["+ weight (real)".to_owned()]),
            ],
            "{diff:#?}"
        );
        assert_eq!(diff.field_counts(), (3, 1));
    }

    /// A shipped Reach `sound_mix` was saved before "default transmission
    /// settings" was added to its root struct.
    #[test]
    fn a_shipped_sound_mix_lacks_its_transmission_settings() {
        let Some(root) = std::env::var_os("BLAM_TEST_HREK").map(std::path::PathBuf::from) else {
            eprintln!("skipping: BLAM_TEST_HREK not set");
            return;
        };
        // The kit root or its tags folder, as the other kit tests take it.
        let tags = if root.join("tags").is_dir() { root.join("tags") } else { root };
        let tag = TagFile::read(tags.join("sound/sound_mix.sound_mix")).unwrap();
        let diff = diff_layouts(&tag, &tag_from("haloreach_mcc/sound_mix.json"));
        let root_diff = diff.structs.iter().find(|s| s.path.is_empty()).expect("the root struct differs");
        assert_eq!((root_diff.size, root_diff.current_size), (132, 148));
        assert!(root_diff.fields.iter().any(|c| c.name == "default transmission settings"
            && matches!(c.kind, FieldChangeKind::Added { .. })), "{root_diff:#?}");
    }

    #[test]
    fn identical_layout_matches() {
        let a = tag_from("haloce_evolved/biped.json");
        let b = tag_from("haloce_evolved/biped.json");
        let cmp = compare_root_layout(&a, &b);
        assert!(cmp.group_match && cmp.version_match);
        assert!(cmp.root_size_match && cmp.field_count_match);
        assert!(cmp.field_diffs.is_empty(), "unexpected diffs: {:?}", cmp.field_diffs);
        assert_eq!(cmp.severity, LayoutSeverity::Match);
        assert!(cmp.is_match());
    }

    #[test]
    fn different_group_is_incompatible() {
        let biped = tag_from("haloce_evolved/biped.json");
        let weapon = tag_from("haloce_evolved/weapon.json");
        let cmp = compare_root_layout(&biped, &weapon);
        assert!(!cmp.group_match);
        assert_eq!(cmp.severity, LayoutSeverity::Incompatible);
        assert!(!cmp.is_match());
    }

    #[test]
    fn same_group_shape_drift_is_drift() {
        // `scenario` (scnr) exists in every game but has a very different
        // shape between Halo 2 and Halo 3 — same group tag, drifted layout.
        let h2 = tag_from("halo2_mcc/scenario.json");
        let h3 = tag_from("halo3_mcc/scenario.json");
        let cmp = compare_root_layout(&h2, &h3);
        assert!(cmp.group_match, "scenario should share the scnr group tag");
        assert_ne!(cmp.severity, LayoutSeverity::Match);
        assert_eq!(cmp.severity, LayoutSeverity::Drift);
        assert!(!cmp.root_size_match || !cmp.field_count_match || !cmp.field_diffs.is_empty());
    }
}
