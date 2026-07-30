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
    let n = a.len();
    let m = b.len();
    // dp[i][j] = LCS length of a[..i] vs b[..j]
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if a[i] == b[j] {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut out = Vec::with_capacity(n + m);
    let mut i = n;
    let mut j = m;
    while i > 0 && j > 0 {
        if a[i - 1] == b[j - 1] {
            out.push((Some(a[i - 1].clone()), Some(b[j - 1].clone())));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            out.push((Some(a[i - 1].clone()), None));
            i -= 1;
        } else {
            out.push((None, Some(b[j - 1].clone())));
            j -= 1;
        }
    }
    while i > 0 {
        out.push((Some(a[i - 1].clone()), None));
        i -= 1;
    }
    while j > 0 {
        out.push((None, Some(b[j - 1].clone())));
        j -= 1;
    }
    out.reverse();
    out
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
