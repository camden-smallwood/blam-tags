//! Editor-only index relationships, resolved against the tag's current data.
//!
//! Ordinary indices retain their declared block target. `index_target` field
//! annotations describe custom searches without changing wire layouts.
use crate::{TagField, TagFieldData, TagStruct};
use serde::Deserialize;
use std::{collections::HashMap, path::Path};

/// One concrete target, including the selected block element in its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexTarget {
    /// Root-relative path with field ordinals, suitable for navigation/remapping.
    pub path: String,
    /// Current number of entries in the target block.
    pub len: usize,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
enum Rule {
    Path(PathRule),
    Selected(SelectedRule),
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PathRule {
    path: String,
    #[serde(default)]
    base: Base,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Base {
    #[default]
    Root,
    Holder,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectedRule {
    /// Relative field path; leading `../` steps climb out of enclosing entries.
    selector: String,
    /// Block path inside the entry selected by that field.
    child: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Field {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    index_target: Option<Rule>,
}

#[derive(Deserialize)]
struct Structure {
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    fields: Vec<Field>,
}

#[derive(Deserialize)]
struct Group {
    #[serde(default)]
    structs: HashMap<String, Structure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Annotation {
    name: String,
    occurrence: usize,
    rule: Rule,
}

/// Per-game editor annotations. Load failures are errors, not an empty table:
/// silently dropping rules would disable structural-edit reference repair.
#[derive(Debug, Default)]
pub struct IndexTargets {
    by_guid: HashMap<[u8; 16], Vec<Annotation>>,
    by_name: HashMap<String, Vec<Annotation>>,
}

impl IndexTargets {
    /// Read all group JSON files in one game's definitions folder.
    /// Missing folders, malformed annotations and conflicting rules are errors.
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, String> {
        let game_dir = game_dir.as_ref();
        let mut entries = std::fs::read_dir(game_dir)
            .map_err(|e| format!("{}: {e}", game_dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|entry| entry.file_name());
        let mut table = Self::default();
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !name.ends_with(".json") || name.starts_with('_') {
                continue;
            }
            let text =
                std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let group: Group = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            for (name, structure) in group.structs {
                let mut occurrences = HashMap::<String, usize>::new();
                let mut annotations = Vec::new();
                for field in structure.fields {
                    if field.kind == "explanation" {
                        continue;
                    }
                    let clean =
                        crate::field_name::clean_field_name(field.name.as_deref().unwrap_or(""))
                            .into_owned();
                    let occurrence = occurrences.entry(clean.clone()).or_default();
                    if let Some(rule) = field.index_target {
                        if !field.kind.contains("block_index")
                            && !field.kind.ends_with("block_flags")
                        {
                            return Err(format!(
                                "{}: {name}/{clean}: index_target requires a block index or flags field",
                                path.display()
                            ));
                        }
                        rule.validate()
                            .map_err(|e| format!("{}: {name}/{clean}: {e}", path.display()))?;
                        annotations.push(Annotation {
                            name: clean,
                            occurrence: *occurrence,
                            rule,
                        });
                    }
                    *occurrence += 1;
                }
                if annotations.is_empty() {
                    continue;
                }
                if let Some(guid) = structure.guid.as_deref().and_then(parse_guid) {
                    merge(table.by_guid.entry(guid).or_default(), &annotations)?;
                }
                merge(table.by_name.entry(name).or_default(), &annotations)?;
            }
        }
        Ok(table)
    }

    /// Load bundled game folders for a self-describing tag with no game profile.
    /// GUID rules remain exact. Name fallback is retained only when every
    /// annotated occurrence agrees across games; ambiguous names are omitted.
    /// Invalid game metadata still fails the load rather than dropping rules.
    pub fn load_all(definitions_root: impl AsRef<Path>) -> Result<Self, String> {
        let root = definitions_root.as_ref();
        let mut directories = std::fs::read_dir(root)
            .map_err(|error| format!("{}: {error}", root.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        directories.sort_by_key(|entry| entry.file_name());
        let mut table = Self::default();
        let mut ambiguous = std::collections::HashSet::new();
        let mut loaded = 0;
        for entry in directories {
            let path = entry.path();
            if !path.is_dir() || !path.join("_meta.json").is_file() {
                continue;
            }
            let game = Self::load(&path)?;
            loaded += 1;
            for (guid, fields) in game.by_guid {
                merge(table.by_guid.entry(guid).or_default(), &fields)?;
            }
            for (name, fields) in game.by_name {
                if ambiguous.contains(&name) {
                    continue;
                }
                let existing = table.by_name.entry(name.clone()).or_default();
                if merge(existing, &fields).is_err() {
                    table.by_name.remove(&name);
                    ambiguous.insert(name);
                }
            }
        }
        if loaded == 0 {
            return Err(format!("{}: no game definitions found", root.display()));
        }
        Ok(table)
    }

    /// Resolve a field's target, including ordinary declared indices/flags.
    /// Missing data or an unset/out-of-range selector returns `None`; another
    /// parent's block is never substituted. The caller supplies the same table
    /// for drawing the picker and repairing indices after structural edits.
    pub fn resolve(&self, root: TagStruct<'_>, field_path: &str) -> Option<IndexTarget> {
        self.resolve_inner(root, field_path, 0)
    }

    /// [`resolve`](Self::resolve) for a caller already holding the field and
    /// its struct, as a walk over a whole tag does. Declared targets need no
    /// descent from the root, and their paths keep `holder_path`'s spelling.
    pub fn resolve_field(
        &self,
        root: TagStruct<'_>,
        holder: TagStruct<'_>,
        holder_path: &str,
        field: TagField<'_>,
    ) -> Option<IndexTarget> {
        if self.annotation(holder, field).is_some() {
            return self.resolve(root, &join(holder_path, &segment(field)));
        }
        declared_target(root, holder, holder_path, field)
    }

    /// Whether `holder`'s `field` has an `index_target` rule. Without one, its
    /// target depends only on the holder and the declared definition, so a
    /// caller walking a whole tag can share one [`resolve`](Self::resolve).
    pub fn is_annotated(&self, holder: TagStruct<'_>, field: TagField<'_>) -> bool {
        self.annotation(holder, field).is_some()
    }

    fn annotation(&self, holder: TagStruct<'_>, field: TagField<'_>) -> Option<&Rule> {
        if field.field_type() == crate::TagFieldType::Explanation {
            return None;
        }
        let clean = field.clean_name();
        // Counting occurrences walks the holder; most fields can skip it.
        let by_guid = self.by_guid.get(&holder.definition().guid());
        let by_name = self.by_name.get(holder.name());
        if !by_guid
            .into_iter()
            .chain(by_name)
            .flatten()
            .any(|a| a.name == clean)
        {
            return None;
        }
        let occurrence = holder
            .fields_all()
            .take(field.ordinal())
            .filter(|f| {
                f.field_type() != crate::TagFieldType::Explanation && f.clean_name() == clean
            })
            .count();
        let matches = |a: &&Annotation| a.name == clean && a.occurrence == occurrence;
        by_guid
            .and_then(|fields| fields.iter().find(matches))
            .or_else(|| by_name?.iter().find(matches))
            .map(|a| &a.rule)
    }

    fn resolve_inner(
        &self,
        root: TagStruct<'_>,
        field_path: &str,
        depth: usize,
    ) -> Option<IndexTarget> {
        if depth > 16 {
            return None;
        }
        let (holder_path, _) = field_path.rsplit_once('/').unwrap_or(("", field_path));
        let holder = if holder_path.is_empty() {
            root
        } else {
            root.descend(holder_path)?
        };
        let field = root.field_path(field_path)?;
        if let Some(rule) = self.annotation(holder, field) {
            return match rule {
                Rule::Path(rule) => {
                    let base = match rule.base {
                        Base::Root => "",
                        Base::Holder => holder_path,
                    };
                    block_at(root, &join(base, &rule.path))
                }
                Rule::Selected(rule) => {
                    let selector = relative_field(holder_path, &rule.selector)?;
                    let index = index_value(root.field_path(&selector)?)?;
                    let index = usize::try_from(index).ok()?;
                    let target = self.resolve_inner(root, &selector, depth + 1)?;
                    if index >= target.len {
                        return None;
                    }
                    block_at(
                        root,
                        &join(&format!("{}[{index}]", target.path), &rule.child),
                    )
                }
            };
        }
        let target = declared_target(root, holder, holder_path, field)?;
        block_at(root, &target.path)
    }
}

/// The first block of a field's declared definition in its holder or nearest
/// ancestor, with the path prefixed by `holder_path` as given.
fn declared_target(
    root: TagStruct<'_>,
    holder: TagStruct<'_>,
    holder_path: &str,
    field: TagField<'_>,
) -> Option<IndexTarget> {
    let definition = field.definition();
    let target = definition
        .block_index_target()
        .or_else(|| definition.block_flags_target())?;
    let mut current = holder_path;
    let mut holder = holder;
    loop {
        if let Some(field) = holder.fields_all().find(|f| {
            f.as_block()
                .is_some_and(|b| b.definition().name() == target.name())
        }) {
            return Some(IndexTarget {
                path: join(current, &segment(field)),
                len: field.as_block()?.len(),
            });
        }
        if current.is_empty() {
            return None;
        }
        current = current.rsplit_once('/').map_or("", |(parent, _)| parent);
        holder = if current.is_empty() {
            root
        } else {
            root.descend(current)?
        };
    }
}

impl Rule {
    fn validate(&self) -> Result<(), String> {
        let valid = |path: &str| !path.is_empty() && !path.starts_with('/') && !path.ends_with('/');
        match self {
            Self::Path(rule) if valid(&rule.path) && !rule.path.split('/').any(|p| p == "..") => {
                Ok(())
            }
            Self::Selected(rule)
                if valid(&rule.selector)
                    && valid(&rule.child)
                    && !rule.child.split('/').any(|p| p == "..") =>
            {
                Ok(())
            }
            _ => Err("index_target paths must be nonempty relative field paths".into()),
        }
    }
}

fn merge(existing: &mut Vec<Annotation>, incoming: &[Annotation]) -> Result<(), String> {
    for annotation in incoming {
        if let Some(old) = existing
            .iter()
            .find(|a| a.name == annotation.name && a.occurrence == annotation.occurrence)
        {
            if old.rule != annotation.rule {
                return Err(format!(
                    "conflicting index_target rules for {} occurrence {}",
                    annotation.name, annotation.occurrence
                ));
            }
        } else {
            existing.push(annotation.clone());
        }
    }
    Ok(())
}

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut guid = [0; 16];
    for (byte, pair) in guid.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    (guid != [0; 16]).then_some(guid)
}

fn join(base: &str, path: &str) -> String {
    if base.is_empty() {
        path.into()
    } else {
        format!("{base}/{path}")
    }
}
fn segment(field: TagField<'_>) -> String {
    format!("{}#{}", field.clean_name(), field.ordinal())
}

fn relative_field(base: &str, path: &str) -> Option<String> {
    let mut base = base;
    let mut path = path;
    while let Some(rest) = path.strip_prefix("../") {
        if base.is_empty() {
            return None;
        }
        base = base.rsplit_once('/').map_or("", |(parent, _)| parent);
        path = rest;
    }
    Some(join(base, path))
}

/// Resolve to canonical ordinal paths, preserving explicit selected indices.
fn block_at(root: TagStruct<'_>, path: &str) -> Option<IndexTarget> {
    let mut holder = root;
    let mut canonical = String::new();
    let pieces: Vec<_> = path.split('/').collect();
    for (i, piece) in pieces.iter().enumerate() {
        let field = holder.field_path(piece.split('[').next()?)?;
        let indexed = piece
            .rsplit_once('[')
            .and_then(|(_, n)| n.strip_suffix(']'))
            .and_then(|n| n.parse::<usize>().ok());
        canonical = join(&canonical, &segment(field));
        if i + 1 == pieces.len() {
            if indexed.is_some() {
                return None;
            }
            return Some(IndexTarget {
                path: canonical,
                len: field.as_block()?.len(),
            });
        }
        if let Some(index) = indexed {
            canonical.push_str(&format!("[{index}]"));
        }
        // Use the normal tag traversal for blocks, arrays, structs and resources.
        holder = root.descend(&canonical)?;
    }
    None
}

fn index_value(field: TagField<'_>) -> Option<i64> {
    match field.value()? {
        TagFieldData::CharBlockIndex(i) | TagFieldData::CustomCharBlockIndex(i) => Some(i as i64),
        TagFieldData::ShortBlockIndex(i) | TagFieldData::CustomShortBlockIndex(i) => Some(i as i64),
        TagFieldData::LongBlockIndex(i) | TagFieldData::CustomLongBlockIndex(i) => Some(i as i64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TagFieldData, TagFile};
    use serde_json::json;
    use std::path::PathBuf;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "index_targets_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, value: &serde_json::Value) {
            fn size_of(value: &serde_json::Value, name: &str) -> u64 {
                value["structs"][name]["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|field| match field["type"].as_str().unwrap() {
                        "block" => 12,
                        "string_id" => 4,
                        "explanation" | "terminator" => 0,
                        "array" => {
                            let array = &value["arrays"][field["definition"].as_str().unwrap()];
                            array["count"].as_u64().unwrap()
                                * size_of(value, array["struct"].as_str().unwrap())
                        }
                        _ => 2,
                    })
                    .sum()
            }
            let mut value = value.clone();
            let sizes: HashMap<_, _> = value["structs"]
                .as_object()
                .unwrap()
                .keys()
                .map(|name| (name.clone(), size_of(&value, name)))
                .collect();
            for (index, (name, structure)) in value["structs"]
                .as_object_mut()
                .unwrap()
                .iter_mut()
                .enumerate()
            {
                if structure.get("guid").is_none() {
                    structure["guid"] = json!(format!("{:032x}", index + 1));
                }
                structure["size"] = json!(sizes[name]);
                structure["fields"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"terminator"}));
            }
            std::fs::write(
                self.0.join("example.json"),
                serde_json::to_vec(&value).unwrap(),
            )
            .unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn schema() -> serde_json::Value {
        json!({
            "name":"example", "tag":"test", "version":1, "flags":2, "block":"root",
            "blocks": {
                "root":{"struct":"root", "max_count":1},
                "parents":{"struct":"parent", "max_count":10},
                "children":{"struct":"child", "max_count":10},
                "holders":{"struct":"holder", "max_count":10},
                "points":{"struct":"point", "max_count":10},
                "damage":{"struct":"damage", "max_count":1}
            },
            "structs": {
                "root":{"guid":"12345678901234567890123456789012", "fields":[
                    {"type":"block", "name":"parents", "definition":"parents"},
                    {"type":"block", "name":"holders", "definition":"holders"},
                    {"type":"block", "name":"damage", "definition":"damage"},
                    {"type":"custom_short_block_index", "name":"section", "index_target":{"path":"damage[0]/sections"}}
                ]},
                "parent":{"fields":[{"type":"block", "name":"children", "definition":"children"}]},
                "child":{"fields":[{"type":"string_id", "name":"name"}]},
                "holder":{"fields":[
                    {"type":"short_block_index", "name":"parent", "definition":"parents"},
                    {"type":"custom_short_block_index", "name":"child", "index_target":{"selector":"parent", "child":"children"}},
                    {"type":"block", "name":"points", "definition":"points"}
                ]},
                "point":{"fields":[{"type":"custom_short_block_index", "name":"point", "index_target":{"selector":"../parent", "child":"children"}}]},
                "damage":{"fields":[
                    {"type":"block", "name":"sections", "definition":"children"},
                    {"type":"custom_short_block_index", "name":"section", "index_target":{"path":"sections", "base":"holder"}}
                ]}
            }
        })
    }

    fn add(tag: &mut TagFile, path: &str, count: usize) {
        let mut root = tag.root_mut();
        let mut field = root.field_path_mut(path).unwrap();
        let mut block = field.as_block_mut().unwrap();
        for _ in 0..count {
            block.add_element();
        }
    }
    fn select(tag: &mut TagFile, index: i16) {
        tag.root_mut()
            .field_path_mut("holders[0]/parent")
            .unwrap()
            .set(TagFieldData::ShortBlockIndex(index))
            .unwrap();
    }

    #[test]
    fn root_holder_and_parent_selector_rules_resolve_current_and_saved_tags() {
        let fixture = Fixture::new();
        fixture.write(&schema());
        let targets = IndexTargets::load(&fixture.0).unwrap();
        let mut tag = TagFile::new(fixture.0.join("example.json")).unwrap();
        add(&mut tag, "parents", 2);
        add(&mut tag, "parents[0]/children", 1);
        add(&mut tag, "parents[1]/children", 3);
        add(&mut tag, "holders", 1);
        add(&mut tag, "holders[0]/points", 1);
        add(&mut tag, "damage", 1);
        add(&mut tag, "damage[0]/sections", 2);
        select(&mut tag, 1);
        let bytes = tag.write_to_bytes().unwrap();
        for mut tag in [tag, TagFile::read_from_bytes(&bytes).unwrap()] {
            for path in ["holders[0]/child", "holders[0]/points[0]/point"] {
                let target = targets.resolve(tag.root(), path).unwrap();
                assert_eq!(target.len, 3, "{path}");
                assert_eq!(target.path, "parents#0[1]/children#0");
            }
            for path in ["section", "damage[0]/section"] {
                assert_eq!(
                    targets.resolve(tag.root(), path).unwrap(),
                    IndexTarget {
                        path: "damage#2[0]/sections#0".into(),
                        len: 2
                    }
                );
            }
            for index in [-1, 99] {
                select(&mut tag, index);
                assert!(targets.resolve(tag.root(), "holders[0]/child").is_none());
                assert!(
                    targets
                        .resolve(tag.root(), "holders[0]/points[0]/point")
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn only_fields_with_rules_report_an_annotation() {
        let fixture = Fixture::new();
        fixture.write(&schema());
        let targets = IndexTargets::load(&fixture.0).unwrap();
        let mut tag = TagFile::new(fixture.0.join("example.json")).unwrap();
        add(&mut tag, "holders", 1);
        let root = tag.root();
        let holder = root.descend("holders[0]").unwrap();
        let annotated = |name| targets.is_annotated(holder, holder.field_path(name).unwrap());
        assert!(annotated("child"));
        assert!(!annotated("parent"), "a declared index has no rule");
        assert!(!annotated("points"), "a block has no rule");
    }

    #[test]
    fn resolve_field_agrees_with_resolve() {
        let fixture = Fixture::new();
        fixture.write(&schema());
        let targets = IndexTargets::load(&fixture.0).unwrap();
        let mut tag = TagFile::new(fixture.0.join("example.json")).unwrap();
        add(&mut tag, "parents", 2);
        add(&mut tag, "parents[1]/children", 3);
        add(&mut tag, "holders", 1);
        add(&mut tag, "holders[0]/points", 1);
        select(&mut tag, 1);
        let root = tag.root();
        for (holder_path, name) in [
            ("holders[0]", "parent"),
            ("holders[0]", "child"),
            ("holders[0]/points[0]", "point"),
        ] {
            let holder = root.descend(holder_path).unwrap();
            let field = holder.field_path(name).unwrap();
            let direct = targets.resolve_field(root, holder, holder_path, field);
            let by_path = targets.resolve(root, &format!("{holder_path}/{name}"));
            assert!(by_path.is_some(), "{name}");
            assert_eq!(direct, by_path, "{name}");
        }
    }

    #[test]
    fn combined_catalog_uses_guids_and_omits_conflicting_name_fallbacks() {
        let fixture = Fixture::new();
        let mut tags = Vec::new();
        for (name, guid, target) in [
            ("first", "12345678901234567890123456789012", "parents"),
            (
                "second",
                "abcdef78901234567890123456789012",
                "damage[0]/sections",
            ),
        ] {
            let game = Fixture(fixture.0.join(name));
            std::fs::create_dir_all(&game.0).unwrap();
            std::fs::write(game.0.join("_meta.json"), "{}").unwrap();
            let mut value = schema();
            value["structs"]["root"]["guid"] = json!(guid);
            value["structs"]["root"]["fields"][3]["index_target"] = json!({"path":target});
            game.write(&value);
            let mut tag = TagFile::new(game.0.join("example.json")).unwrap();
            add(&mut tag, "parents", 3);
            add(&mut tag, "damage", 1);
            add(&mut tag, "damage[0]/sections", 2);
            tags.push(tag);
            // Keep both directories until their parent fixture is dropped.
            std::mem::forget(game);
        }
        let targets = IndexTargets::load_all(&fixture.0).unwrap();
        assert_eq!(targets.resolve(tags[0].root(), "section").unwrap().len, 3);
        assert_eq!(targets.resolve(tags[1].root(), "section").unwrap().len, 2);
        assert!(!targets.by_name.contains_key("root"));
        std::fs::write(fixture.0.join("first/example.json"), "broken json").unwrap();
        assert!(
            IndexTargets::load_all(&fixture.0).is_err(),
            "a broken game table must not be omitted"
        );
    }

    #[test]
    fn repository_game_tables_load_together() {
        let root = std::env::var_os("BLAM_TEST_DEFINITIONS")
            .map(PathBuf::from)
            .unwrap_or_else(|| "../definitions".into());
        IndexTargets::load_all(root).unwrap();
    }

    #[test]
    fn non_ascii_guids_cannot_panic_during_annotation_loading() {
        assert_eq!(parse_guid(&"é".repeat(16)), None);
    }

    #[test]
    fn declared_indices_resolve_sibling_blocks_inside_arrays() {
        let fixture = Fixture::new();
        let mut value = schema();
        value["arrays"] = json!({"holder_array":{"count":1,"struct":"holder"}});
        value["structs"]["root"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"array","name":"array holders","definition":"holder_array"}));
        value["structs"]["holder"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"short_block_index","name":"point index","definition":"points"}));
        fixture.write(&value);
        let targets = IndexTargets::load(&fixture.0).unwrap();
        let mut tag = TagFile::new(fixture.0.join("example.json")).unwrap();
        add(&mut tag, "array holders[0]/points", 2);
        let bytes = tag.write_to_bytes().unwrap();
        for tag in [tag, TagFile::read_from_bytes(&bytes).unwrap()] {
            let target = targets
                .resolve(tag.root(), "array holders[0]/point index")
                .unwrap();
            assert_eq!(target.len, 2);
            assert_eq!(target.path, "array holders#4[0]/points#2");
        }
    }

    #[test]
    fn malformed_annotations_and_missing_directories_are_errors() {
        let fixture = Fixture::new();
        for rule in [
            json!({"path":""}),
            json!({"path":"../elsewhere"}),
            json!({"selector":"parent"}),
            json!({"unknown":"value"}),
        ] {
            let mut value = schema();
            value["structs"]["root"]["fields"][3]["index_target"] = rule;
            fixture.write(&value);
            assert!(IndexTargets::load(&fixture.0).is_err());
        }
        assert!(IndexTargets::load(fixture.0.join("missing")).is_err());
    }

    #[test]
    fn guid_and_name_occurrence_matching_ignore_explanation_headings() {
        let fixture = Fixture::new();
        let mut value = schema();
        value["structs"]["root"]["fields"].as_array_mut().unwrap().extend([
            json!({"type":"explanation", "name":"section", "definition":"heading"}),
            json!({"type":"custom_short_block_index", "name":"section", "index_target":{"path":"parents"}}),
        ]);
        fixture.write(&value);
        let targets = IndexTargets::load(&fixture.0).unwrap();
        // A saved layout can retain the GUID while using another struct name.
        value["structs"]["renamed"] = value["structs"]["root"].take();
        value["structs"].as_object_mut().unwrap().remove("root");
        value["blocks"]["root"]["struct"] = json!("renamed");
        fixture.write(&value);
        let mut tag = TagFile::new(fixture.0.join("example.json")).unwrap();
        add(&mut tag, "parents", 2);
        let bytes = tag.write_to_bytes().unwrap();
        for tag in [tag, TagFile::read_from_bytes(&bytes).unwrap()] {
            assert_eq!(targets.resolve(tag.root(), "section#5").unwrap().len, 2);
            assert!(
                targets.resolve(tag.root(), "section#3").is_none(),
                "missing damage entry must not guess another block"
            );
        }
    }
}
