//! Canonical parsing of Halo tag **field names**.
//!
//! Tag field names embed inline markup that the tools use for presentation and
//! that must be stripped to recover the bare, *addressable* name. This module is
//! the single source of truth for that grammar, ported from Foundation's
//! `Bungie.Tags.TagFieldNameInfo` (HREK `ManagedBlam`), extended with the
//! `[min,max]` range-hint split (which Foundation leaves to native code).
//!
//! ## Markup grammar
//!
//! A raw name is `clean_name` followed, in any order, by marker-introduced
//! sections and trailing flag markers:
//!
//! | marker | meaning |
//! |--------|---------|
//! | `&`    | display-name override (what the grid shows) |
//! | `#`    | description / tooltip |
//! | `:`    | units |
//! | `[…]`  | numeric range hint (e.g. `[0,1]`, `[0,+inf]`) |
//! | `{…}`  | alias annotation (dropped) |
//! | `*`    | read-only (presence-tested, anywhere) |
//! | `!`    | hidden / invisible (presence-tested, anywhere) |
//! | `^`    | trailing-only: this field labels its block's elements |
//!
//! The **clean name** is everything up to the first markup character, with any
//! `{alias}` removed and `/` normalized to `\` (so a clean name can never
//! contain the path separator `/` — matching Foundation, which relies on this
//! instead of per-segment escaping).
//!
//! ## Why this matters
//!
//! Field paths (`crate::field_path`) address fields by their *clean* name. If a
//! raw name like `ambient color:[0,255]` or `max sounds [1,16]` reached the path
//! layer verbatim, its `:`/`[`/`#` would collide with the path grammar's own
//! `type:`, `[index]`, and `#ordinal` tokens. Cleaning both the stored name and
//! the lookup query (see [`crate::data::field_name_matches`]) makes lookups
//! markup-insensitive regardless of how a given tag happened to store its names.

use std::borrow::Cow;

/// Markup characters that terminate the clean (addressable) portion of a field
/// name. The first occurrence of any of these ends the clean name.
///
/// `|` is included for the Halo 2 `model_animation_graph` dumper artifact: block
/// names are stored as `animations|ABCDCC`, where the part after `|` is a
/// name-code, not part of the addressable name. Cutting it here makes the clean
/// name (`animations`) consistent across paths, display, and matching — so a
/// path built from the clean name resolves against the stored raw name.
const NAME_MARKERS: &[char] = &['&', '#', ':', '[', '{', '*', '!', '^', '|'];

/// Decomposition of a raw field name into its clean name plus presentation
/// metadata. All borrowed slices point into the input `raw`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldNameInfo<'a> {
    /// The bare addressable name: markup-free, `{alias}` dropped, `/`→`\`.
    /// This is what paths and lookups use.
    pub clean_name: Cow<'a, str>,
    /// `&`-introduced display-name override, if any (else use `clean_name`).
    pub display_name: Option<&'a str>,
    /// `#`-introduced description / tooltip text.
    pub description: Option<&'a str>,
    /// `:`-introduced units string (e.g. `"dB"`, `"seconds"`).
    pub units: Option<&'a str>,
    /// `[…]` range hint, **with** its brackets (e.g. `"[0,1]"`).
    pub range: Option<&'a str>,
    /// `*` present anywhere — the field is read-only.
    pub read_only: bool,
    /// `!` present anywhere — the field is hidden / invisible.
    pub hidden: bool,
    /// Trailing `^` — this field supplies the label for its block's elements.
    pub is_block_label: bool,
}

impl<'a> FieldNameInfo<'a> {
    /// The name to display in a grid/tree: the `&` override if present, else the
    /// clean name.
    pub fn display(&self) -> &str {
        self.display_name.unwrap_or(&self.clean_name)
    }
}

/// Parse a raw field name into its [`FieldNameInfo`] components.
///
/// Cheap and allocation-free unless the clean name actually contains a `/`
/// (rare) — then it is normalized to `\` in an owned string.
///
/// Sections are parsed in the canonical order `name [&display] [:units] [#help]`.
/// `#help` is split off first and runs to the end (so it may itself contain any
/// character); `&display` and `:units` are then read out of the head, and a
/// `[range]` hint is excised from the units slot (e.g. `:[0.1-1]seconds` yields
/// units `seconds` + range `[0.1-1]`).
pub fn parse_field_name(raw: &str) -> FieldNameInfo<'_> {
    // Flag markers are presence-tested (mirror Foundation), so their order and
    // position don't matter.
    let read_only = raw.contains('*');
    let hidden = raw.contains('!');

    // `#help` runs to the end; everything before it is the "head".
    let (head, description) = match raw.find('#') {
        Some(hash) => (&raw[..hash], non_empty(raw[hash + 1..].trim())),
        None => (raw, None),
    };

    // The block-element-label marker `^` is trailing on the head (i.e. it comes
    // after the name/units and before any `#help`) — a small improvement over
    // Foundation's raw `EndsWith("^")`, which misses `name^#help`.
    let is_block_label = head.ends_with('^');

    let range = range_hint(head);

    FieldNameInfo {
        clean_name: clean_name_cow(raw),
        // `&display` runs from `&` up to the `:units` marker (or end of head).
        display_name: between(head, '&', &[':']),
        // `:units` runs from `:` to the end of head, with the range excised.
        units: units_slot(head, range),
        range,
        description,
        read_only,
        hidden,
        is_block_label,
    }
}

/// `Some(s)` if `s` is non-empty, else `None`.
fn non_empty(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

/// Text from just after `start` up to the first of `stops` (or the end),
/// trimmed. `None` if `start` is absent or the section is empty.
fn between<'a>(text: &'a str, start: char, stops: &[char]) -> Option<&'a str> {
    let from = text.find(start)? + start.len_utf8();
    let rest = &text[from..];
    let end = rest.find(stops).unwrap_or(rest.len());
    non_empty(rest[..end].trim())
}

/// The `:units` value: everything after the first `:` in `head`, with the
/// `[range]` substring (if it falls inside) removed. `None` if absent/empty.
/// Handles the range sitting before (`:[0.1-1]seconds`), after (`:seconds[..]`),
/// or being the whole slot (`:[0,255]` → no unit).
fn units_slot<'a>(head: &'a str, range: Option<&str>) -> Option<&'a str> {
    let from = head.find(':')? + 1;
    let slot = head[from..].trim();
    let unit = match range.and_then(|r| slot.find(r).map(|p| (p, r.len()))) {
        Some((pos, len)) => {
            let after = slot[pos + len..].trim();
            if after.is_empty() { slot[..pos].trim() } else { after }
        }
        None => slot,
    };
    non_empty(unit)
}

/// The bare addressable name for `raw` — markup stripped. Borrows when possible.
///
/// This is the hot path used by [`crate::data::field_name_matches`] and the
/// path layer; keep it allocation-free for the common (no-`/`) case.
pub fn clean_field_name(raw: &str) -> Cow<'_, str> {
    clean_name_cow(raw)
}

fn clean_name_cow(raw: &str) -> Cow<'_, str> {
    // Clean name = up to the first markup marker.
    let end = raw.find(NAME_MARKERS).unwrap_or(raw.len());
    let base = raw[..end].trim_end();
    // Foundation normalizes `/`→`\` so a name can't split a path.
    if base.contains('/') {
        Cow::Owned(base.replace('/', "\\"))
    } else {
        Cow::Borrowed(base)
    }
}

/// The `[…]` range hint including its brackets, if the name carries a
/// well-formed one. Uses the first `[` and its matching-position `]`.
fn range_hint(raw: &str) -> Option<&str> {
    let open = raw.find('[')?;
    let close = raw[open..].find(']')? + open;
    (close > open).then(|| &raw[open..=close])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(raw: &str) -> String {
        clean_field_name(raw).into_owned()
    }

    #[test]
    fn plain_name_is_unchanged() {
        let info = parse_field_name("jump velocity");
        assert_eq!(info.clean_name, "jump velocity");
        assert_eq!(info.display(), "jump velocity");
        assert!(!info.read_only && !info.hidden && !info.is_block_label);
        assert!(info.units.is_none() && info.range.is_none() && info.description.is_none());
    }

    #[test]
    fn units_and_bracket_range_after_colon() {
        // The reported bug's canonical shape.
        let info = parse_field_name("ambient color:[0,255]");
        assert_eq!(info.clean_name, "ambient color");
        assert_eq!(info.range.as_deref(), Some("[0,255]"));
        // units section is "[0,255]" text before the next marker '[' → empty,
        // so units stays None; the range carries it.
        assert!(info.units.is_none());
    }

    #[test]
    fn units_then_range_then_description() {
        let info = parse_field_name("acceleration scale:[0,+inf]#marine 1.0, grunt 1.4");
        assert_eq!(info.clean_name, "acceleration scale");
        assert_eq!(info.range.as_deref(), Some("[0,+inf]"));
        assert_eq!(info.description.as_deref(), Some("marine 1.0, grunt 1.4"));
    }

    #[test]
    fn units_after_an_inline_range() {
        // `:[range]units` — the unit sits after the bracket; capture both.
        let info = parse_field_name("auto-exposure delay:[0.1-1]seconds#how long");
        assert_eq!(info.clean_name, "auto-exposure delay");
        assert_eq!(info.units.as_deref(), Some("seconds"));
        assert_eq!(info.range.as_deref(), Some("[0.1-1]"));
        assert_eq!(info.description.as_deref(), Some("how long"));
    }

    #[test]
    fn help_runs_to_end_including_ampersand() {
        // `#help` is split first and runs to the end, so a `&` inside it is not
        // mistaken for a display-name override.
        let info = parse_field_name("full name^#name of the mode & state");
        assert_eq!(info.clean_name, "full name");
        assert!(info.is_block_label);
        assert_eq!(info.description.as_deref(), Some("name of the mode & state"));
        assert!(info.display_name.is_none());
    }

    #[test]
    fn real_units_string() {
        let info = parse_field_name("air reverb gain{reverb gain}:dB#how much reverb");
        assert_eq!(info.clean_name, "air reverb gain");
        assert_eq!(info.units.as_deref(), Some("dB"));
        assert_eq!(info.description.as_deref(), Some("how much reverb"));
    }

    #[test]
    fn bare_bracket_range_no_colon() {
        // Survives clean_blay compilation with brackets intact → the worst case.
        let info = parse_field_name("max sounds [1,16]");
        assert_eq!(info.clean_name, "max sounds");
        assert_eq!(info.range.as_deref(), Some("[1,16]"));
    }

    #[test]
    fn read_only_and_hidden_flags_are_order_independent() {
        // Foundation semantics: '*' = read-only, '!' = hidden, presence-tested.
        // Baboon's old ends_with+pop dropped '*' when the order was '*!'.
        for name in ["angle*!", "aabb center!*"] {
            let info = parse_field_name(name);
            assert!(info.read_only, "{name}: read_only");
            assert!(info.hidden, "{name}: hidden");
        }
        assert_eq!(clean("angle*!"), "angle");
        assert_eq!(clean("aabb center!*"), "aabb center");
    }

    #[test]
    fn hidden_only() {
        let info = parse_field_name("activity!");
        assert!(info.hidden && !info.read_only);
        assert_eq!(info.clean_name, "activity");
    }

    #[test]
    fn trailing_caret_is_block_label() {
        let info = parse_field_name("activity name^");
        assert!(info.is_block_label);
        assert_eq!(info.clean_name, "activity name");
        // '^' followed by '*' is not trailing → not a block label (Foundation).
        assert!(!parse_field_name("bsp^*").is_block_label);
        assert_eq!(clean("bsp^*"), "bsp");
    }

    #[test]
    fn display_name_override() {
        let info = parse_field_name("bounds 480i&bounds 4x3 (640x480)");
        assert_eq!(info.clean_name, "bounds 480i");
        assert_eq!(info.display_name.as_deref(), Some("bounds 4x3 (640x480)"));
        assert_eq!(info.display(), "bounds 4x3 (640x480)");
    }

    #[test]
    fn alias_is_dropped() {
        assert_eq!(clean("acoustics{background sounds}*"), "acoustics");
        assert_eq!(clean("acoustics palette{background sound palette}"), "acoustics palette");
        // Alias containing a colon — clean name cuts at '{', not the inner ':'.
        assert_eq!(clean("durango compiled shader{..:durango compiled shader}"), "durango compiled shader");
    }

    #[test]
    fn jmad_name_code_dumper_suffix_is_cut() {
        // H2 model_animation_graph block names carry a `|<code>` dumper artifact.
        let info = parse_field_name("animations|ABCDCC");
        assert_eq!(info.clean_name, "animations");
        assert_eq!(clean("skeleton nodes|ABCDCC"), "skeleton nodes");
        assert_eq!(clean("modes|AABBCC"), "modes");
    }

    #[test]
    fn forward_slash_becomes_backslash() {
        // A clean name must never contain the path separator.
        assert_eq!(clean("a/b"), "a\\b");
        // Borrow when there's no slash.
        assert!(matches!(clean_field_name("plain name"), Cow::Borrowed(_)));
    }

    #[test]
    fn cleaning_is_idempotent() {
        for raw in [
            "ambient color:[0,255]",
            "max sounds [1,16]",
            "angle*!",
            "activity name^",
            "acoustics{background sounds}*",
        ] {
            let once = clean(raw);
            assert_eq!(clean(&once), once, "idempotent for {raw:?}");
        }
    }
}
