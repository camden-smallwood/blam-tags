//! Static checks of a game's `element_label` entries against its definitions:
//! every definition with a label callback has an entry, and every path a
//! template names exists. Run by the tests, so a typo or a renamed field fails
//! there rather than labelling nothing in the editor.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::template::{self, Cond, Expr, Filter, Operand, Path as TPath, PathStart, Piece, Select, SegmentName, Slot};
use super::ElementLabelsError;

/// One group file's definitions, as far as the checks need them.
struct Group {
    root_struct: Option<String>,
    blocks: HashMap<String, String>,
    arrays: HashMap<String, String>,
    structs: HashMap<String, Vec<(String, String, Option<String>)>>,
}

/// Every group of a game, and its enums by name.
struct Schema {
    groups: HashMap<String, Group>,
    enums: std::collections::HashSet<String>,
}

impl Schema {
    fn load(game_dir: &Path) -> Result<(Self, Vec<(String, Value)>), ElementLabelsError> {
        let mut groups = HashMap::new();
        let mut enums = std::collections::HashSet::new();
        let mut raw = Vec::new();
        for entry in std::fs::read_dir(game_dir)? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if !name.ends_with(".json") || name.starts_with('_') {
                continue;
            }
            let value: Value = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| ElementLabelsError::Json(path.clone(), error))?;
            let file = name.trim_end_matches(".json").to_owned();
            let map = |key: &str| -> HashMap<String, String> {
                value[key]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| Some((k.clone(), v["struct"].as_str()?.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let blocks = map("blocks");
            let arrays = map("arrays");
            let structs = value["structs"]
                .as_object()
                .map(|o| {
                    o.iter()
                        .map(|(k, s)| {
                            let fields = s["fields"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                // A tag's layout carries an explanation as an unnamed,
                                // zero-width field, so no path can name one; Halo 3 ODST's
                                // scenario has an explanation and a block both called
                                // `campaign players`.
                                .filter(|f| f["type"] != "explanation")
                                .filter_map(|f| {
                                    let name = f["name"].as_str()?;
                                    Some((
                                        crate::field_name::clean_field_name(name).into_owned(),
                                        f["type"].as_str().unwrap_or("").to_owned(),
                                        f["definition"].as_str().map(str::to_owned),
                                    ))
                                })
                                .collect();
                            (k.clone(), fields)
                        })
                        .collect()
                })
                .unwrap_or_default();
            if let Some(e) = value["enums_flags"].as_object() {
                enums.extend(e.keys().cloned());
            }
            let root_struct = value["block"].as_str().and_then(|b| blocks.get(b)).cloned();
            groups.insert(file.clone(), Group { root_struct, blocks, arrays, structs });
            raw.push((file, value));
        }
        Ok((Schema { groups, enums }, raw))
    }

    /// A struct's fields by name, looked up in `file` first (names repeat
    /// across files).
    fn fields(&self, file: &str, name: &str) -> Option<&Vec<(String, String, Option<String>)>> {
        self.groups
            .get(file)
            .and_then(|g| g.structs.get(name))
            .or_else(|| self.groups.values().find_map(|g| g.structs.get(name)))
    }

    fn block_struct(&self, file: &str, name: &str) -> Option<&String> {
        self.groups.get(file).and_then(|g| g.blocks.get(name)).or_else(|| self.groups.values().find_map(|g| g.blocks.get(name)))
    }

    fn array_struct(&self, file: &str, name: &str) -> Option<&String> {
        self.groups.get(file).and_then(|g| g.arrays.get(name)).or_else(|| self.groups.values().find_map(|g| g.arrays.get(name)))
    }
}

/// What a checked path reached.
enum Reached {
    Struct(String),
    Field,
    Unknown,
}

struct Checker<'a> {
    schema: &'a Schema,
    file: &'a str,
    key: String,
    element_struct: String,
    maps: Vec<String>,
    /// The group file a root path starts in: the alternative's `{#group} ==`
    /// guard when it has one (a scenario-object block in a resource file
    /// reads the owning scenario's root), else the entry's own file.
    root_file: Option<String>,
    used_maps: &'a mut std::collections::HashSet<String>,
    problems: &'a mut Vec<String>,
}

impl Checker<'_> {
    fn problem(&mut self, message: String) {
        self.problems.push(format!("{}/{}: {message}", self.file, self.key));
    }

    fn template(&mut self, text: &str) {
        match template::parse_template(text) {
            Ok(t) => {
                for piece in &t.pieces {
                    if let Piece::Slot(slot) = piece {
                        self.slot(slot);
                    }
                }
            }
            Err(error) => self.problem(format!("{text:?}: {error}")),
        }
    }

    fn condition(&mut self, text: &str) {
        match template::parse_condition(text) {
            Ok(cond) => {
                let base = self.element_struct.clone();
                self.cond(&cond, &base);
            }
            Err(error) => self.problem(format!("when {text:?}: {error}")),
        }
    }

    fn slot(&mut self, slot: &Slot) {
        for filter in &slot.filters {
            match filter {
                Filter::Map(name) if !self.maps.contains(name) => self.problem(format!("no map `{name}`")),
                Filter::Map(name) => {
                    self.used_maps.insert(name.clone());
                }
                Filter::Enum(name) if !self.schema.enums.contains(name) => self.problem(format!("no enum `{name}`")),
                _ => {}
            }
        }
        self.expr(&slot.expr);
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Path(path) => {
                let base = self.element_struct.clone();
                self.path(path, &base);
            }
            Expr::Binary(a, _, b) => {
                self.expr(a);
                self.expr(b);
            }
            Expr::Hash(_) | Expr::Number(_) => {}
        }
    }

    fn cond(&mut self, cond: &Cond, base: &str) {
        match cond {
            Cond::Or(parts) | Cond::And(parts) => parts.iter().for_each(|c| self.cond(c, base)),
            Cond::Not(inner) => self.cond(inner, base),
            Cond::Compare(a, _, b) => {
                self.operand(a, base);
                self.operand(b, base);
            }
            Cond::BitAnd(a, _) | Cond::Truthy(a) | Cond::StartsWith(a, _) => self.operand(a, base),
            Cond::Any(path, inner) => {
                if let Reached::Struct(element) = self.path(path, base) {
                    self.cond(inner, &element);
                }
            }
        }
    }

    fn operand(&mut self, operand: &Operand, base: &str) {
        match operand {
            Operand::Slot(slot) => self.slot(slot),
            Operand::Bare(path) => {
                self.path(path, base);
            }
            Operand::Number(_) | Operand::Text(_) => {}
        }
    }

    /// Walk `path` through the definitions from `base` (the struct a bare
    /// path starts at).
    fn path(&mut self, path: &TPath, base: &str) -> Reached {
        let mut current = match path.start {
            PathStart::Element => base.to_owned(),
            PathStart::Root => match self.schema.groups.get(self.root_file.as_deref().unwrap_or(self.file)).and_then(|g| g.root_struct.clone()) {
                Some(root) => root,
                None => return Reached::Unknown,
            },
            // The parent depends on where the block sits.
            PathStart::Parent(_) => return Reached::Unknown,
        };
        for (i, segment) in path.segments.iter().enumerate() {
            let last = i + 1 == path.segments.len();
            let SegmentName::Literal(name) = &segment.name else {
                if let SegmentName::Computed(slot) = &segment.name {
                    self.slot(slot);
                }
                return Reached::Unknown;
            };
            let Some(fields) = self.schema.fields(self.file, &current) else { return Reached::Unknown };
            let Some((_, ty, definition)) = fields.iter().find(|(clean, _, _)| clean == name).cloned() else {
                self.problem(format!("no field `{name}` in {current}"));
                return Reached::Unknown;
            };
            let inner = match (ty.as_str(), &definition) {
                ("block", Some(d)) => self.schema.block_struct(self.file, d).cloned(),
                ("struct", Some(d)) => Some(d.clone()),
                ("array", Some(d)) => self.schema.array_struct(self.file, d).cloned(),
                _ => None,
            };
            match &segment.select {
                Some(Select::Slot(slot)) => self.slot(slot),
                Some(Select::Where(cond)) => {
                    if let Some(element) = &inner {
                        let element = element.clone();
                        self.cond(cond, &element);
                    }
                }
                _ => {}
            }
            if ty == "tag_reference" && path.then.is_some() {
                return Reached::Unknown;
            }
            match inner {
                Some(next) => current = next,
                None if last => return Reached::Field,
                None => {
                    self.problem(format!("`{name}` in {current} is a {ty}, not a container"));
                    return Reached::Unknown;
                }
            }
        }
        Reached::Struct(current)
    }
}

/// Check one alternative: its `when`, its `format`, and a join's prefix and
/// parts (a part may hold alternatives of its own).
fn check_alternative(checker: &mut Checker<'_>, alternative: &Value, group_files: &HashMap<String, String>) {
    match alternative {
        Value::String(text) => checker.template(text),
        other => {
            checker.root_file = other["when"].as_str().and_then(|when| guarded_group(when, group_files));
            if let Some(when) = other["when"].as_str() {
                checker.condition(when);
            }
            for template in ["format", "prefix"].iter().filter_map(|key| other[*key].as_str()) {
                checker.template(template);
            }
            for part in other["parts"].as_array().into_iter().flatten() {
                match part {
                    Value::Array(choices) => choices.iter().for_each(|choice| check_alternative(checker, choice, group_files)),
                    part => check_alternative(checker, part, group_files),
                }
            }
        }
    }
}

/// The group file a `when` limits its alternative to with `{#group} == "tag"`.
fn guarded_group(when: &str, group_files: &HashMap<String, String>) -> Option<String> {
    let start = when.find(r#"{#group} == ""#)? + r#"{#group} == ""#.len();
    let tag = when[start..].split('"').next()?;
    group_files.get(&format!("{tag:<4}")).cloned()
}

/// Problems with a game's `element_label` entries: templates or `when`s that
/// don't parse, paths that name no field, maps, enums or shared entries that
/// don't exist, shared maps and entries nothing uses, and (with
/// `require_coverage`) definitions marked `element_label_callback` that have
/// no entry. A shared entry is checked against every struct that names it.
pub fn check(game_dir: impl AsRef<Path>, require_coverage: bool) -> Result<Vec<String>, ElementLabelsError> {
    let game_dir = game_dir.as_ref();
    let (schema, raw) = Schema::load(game_dir)?;
    let meta_path = game_dir.join("_meta.json");
    let meta: Value = match std::fs::read(&meta_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| ElementLabelsError::Json(meta_path.clone(), error))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => return Err(error.into()),
    };
    let shared_maps: Vec<String> =
        meta["element_labels"]["maps"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let shared_entries = meta["element_labels"]["entries"].as_object().cloned().unwrap_or_default();
    let group_files: HashMap<String, String> = meta["tag_index"]
        .as_object()
        .map(|index| index.iter().filter_map(|(tag, file)| Some((tag.clone(), file.as_str()?.to_owned()))).collect())
        .unwrap_or_default();
    let mut used_maps = std::collections::HashSet::new();
    let mut used_entries = std::collections::HashSet::new();
    let mut problems = Vec::new();
    for (file, value) in &raw {
        let mut visit = |key: &str, entry: &Value, element_struct: Option<String>| {
            let mut label = &entry["element_label"];
            if label.is_null() {
                if require_coverage && entry.get("element_label_callback").is_some() {
                    problems.push(format!("{file}/{key}: has a label callback but no element_label"));
                }
                return;
            }
            if let Some(name) = label.as_str() {
                used_entries.insert(name.to_owned());
                match shared_entries.get(name) {
                    Some(shared) => label = shared,
                    None => {
                        problems.push(format!("{file}/{key}: no shared element_label entry `{name}`"));
                        return;
                    }
                }
            }
            let Some(element_struct) = element_struct else { return };
            let mut maps: Vec<String> = label["maps"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
            for name in &shared_maps {
                if !maps.contains(name) {
                    maps.push(name.clone());
                }
            }
            let mut checker = Checker {
                schema: &schema,
                file,
                key: key.to_owned(),
                element_struct,
                maps,
                root_file: None,
                used_maps: &mut used_maps,
                problems: &mut problems,
            };
            for alternative in label["label"].as_array().into_iter().flatten() {
                check_alternative(&mut checker, alternative, &group_files);
            }
        };
        if let Some(blocks) = value["blocks"].as_object() {
            for (key, block) in blocks {
                visit(key, block, block["struct"].as_str().map(str::to_owned));
            }
        }
        if let Some(structs) = value["structs"].as_object() {
            for (key, structure) in structs {
                visit(key, structure, Some(key.clone()));
            }
        }
    }
    for name in shared_maps.iter().filter(|name| !used_maps.contains(*name)) {
        problems.push(format!("_meta: shared map `{name}` is used by no entry"));
    }
    for name in shared_entries.keys().filter(|name| !used_entries.contains(*name)) {
        problems.push(format!("_meta: shared entry `{name}` is named by no definition"));
    }
    Ok(problems)
}
