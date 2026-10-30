use super::*;

fn menu(items: Vec<MenuItem>) -> Menu { Menu { items, ..Default::default() } }

#[test]
fn foundation_accepts_empty_and_any_item_ignoring_case() {
    let m = menu(vec![
        MenuItem::item("Hard_Metal"),
        MenuItem::submenu("soft", vec![MenuItem::item("soft"), MenuItem::item("cloth")]),
        MenuItem::submenu("folder", vec![MenuItem::item("leaf")]),
    ]);
    assert!(m.accepts(""), "empty is valid");
    assert!(m.accepts("hard_metal"), "case is ignored");
    assert!(m.accepts("CLOTH"), "items at any depth");
    assert!(m.accepts("soft"), "a parent that lists itself");
    assert!(!m.accepts("folder"), "a plain submenu header is not a value");
    assert!(!m.accepts("stone"), "a name the menu doesn't offer");
}

#[test]
fn a_menu_with_nothing_to_pick_accepts_anything() {
    assert!(Menu::default().accepts("anything"));
    let only_headers = menu(vec![MenuItem::submenu("a", vec![MenuItem::submenu("b", vec![MenuItem::item("")])])]);
    assert!(!only_headers.picks().is_empty());
}

/// Every game's editors load: each field naming an editor names one its
/// `_meta.json` describes. Halo CE has none.
#[test]
fn every_games_editors_load() {
    for game in ["haloce_mcc", "halo2_mcc", "halo3_mcc", "halo3odst_mcc", "haloreach_mcc", "halo4_mcc", "halo2amp_mcc"] {
        let editors = StringEditors::load(format!("../definitions/{game}")).unwrap_or_else(|error| panic!("{game}: {error}"));
        assert_eq!(editors.is_empty(), game == "haloce_mcc", "{game}");
    }
}

/// A field naming an editor `_meta.json` doesn't describe fails the load,
/// rather than giving the field no menu.
#[test]
fn an_undescribed_editor_fails_the_load() {
    let dir = std::env::temp_dir().join(format!("string_editor_undescribed_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_meta.json"), r#"{"string_editors": {"material": {"builder": "list", "items": ["a"]}}}"#).unwrap();
    std::fs::write(dir.join("thing.json"), r#"{"tag": "thng", "structs": {"s": {"fields": [{"type": "string_id", "name": "x", "string_editor": "nope"}]}}}"#).unwrap();
    let error = StringEditors::load(&dir).unwrap_err();
    std::fs::remove_dir_all(&dir).ok();
    assert!(matches!(error, StringEditorsError::UnknownEditor { ref editor, .. } if editor == "nope"), "{error}");
}

/// Reads a kit's tags for menus: gen3 tags as they are, Halo 2's through
/// the definitions' layout.
pub(super) struct KitTags {
    pub tags: std::path::PathBuf,
    pub game: &'static str,
    pub names: HashMap<u32, String>,
    pub classic: bool,
    pub cache: std::cell::RefCell<HashMap<std::path::PathBuf, Option<Arc<TagFile>>>>,
}

impl KitTags {
    pub fn new(tags: std::path::PathBuf, game: &'static str) -> Self {
        let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("../definitions/{game}/_meta.json")).unwrap()).unwrap();
        let names = meta["tag_index"].as_object().unwrap().iter().filter_map(|(tag, name)| Some((crate::fields::parse_group_tag(tag)?, name.as_str()?.to_owned()))).collect();
        KitTags { tags, game, names, classic: game == "halo2_mcc", cache: Default::default() }
    }

    pub fn read(&self, file: &std::path::Path) -> Option<Arc<TagFile>> {
        if let Some(tag) = self.cache.borrow().get(file) {
            return tag.clone();
        }
        let tag = if self.classic {
            let group = file.extension()?.to_string_lossy().into_owned();
            let layout = crate::TagLayout::from_json(format!("../definitions/{}/{group}.json", self.game)).ok()?;
            crate::classic::read_classic_tag_file(&std::fs::read(file).ok()?, layout).ok()
        } else {
            TagFile::read(file).ok()
        }
        .map(Arc::new);
        self.cache.borrow_mut().insert(file.to_path_buf(), tag.clone());
        tag
    }
}

impl TagSource for KitTags {
    fn tag(&self, group: u32, path: &str) -> Option<Arc<TagFile>> {
        let file = crate::paths::resolve_tag_path(&self.tags, path, self.names.get(&group)?);
        self.read(&file)
    }
}

/// Every editor field's path in `root`, depth first.
pub(super) fn editor_fields(editors: &StringEditors, root: TagStruct<'_>) -> Vec<(String, String)> {
    fn walk(editors: &StringEditors, holder: TagStruct<'_>, prefix: &str, out: &mut Vec<(String, String)>) {
        for field in holder.fields() {
            let name = field.name();
            let segment = format!("{}#{}", crate::field_name::clean_field_name(name), field.ordinal());
            let path = if prefix.is_empty() { segment.clone() } else { format!("{prefix}/{segment}") };
            if let Some(editor) = editors.editor_of(holder, field.ordinal()) {
                out.push((path.clone(), editor.to_owned()));
            }
            if let Some(child) = field.as_struct() {
                walk(editors, child, &path, out);
            } else if let Some(block) = field.as_block() {
                for (i, element) in block.iter().enumerate() {
                    walk(editors, element, &format!("{path}[{i}]"), out);
                }
            } else if let Some(array) = field.as_array() {
                for (i, element) in array.iter().enumerate() {
                    walk(editors, element, &format!("{path}[{i}]"), out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(editors, root, "", &mut out);
    out
}

/// How well the editors' menus take the values a kit's own tags hold: for
/// every tag of a group with editor fields (up to `BLAM_TEST_SWEEP_LIMIT`
/// per group, default 40), every editor field's value against its menu.
/// Prints the counts and the first rejects per editor. Set
/// `BLAM_TEST_SWEEP=<game>` and that kit's `BLAM_TEST_*`.
#[test]
fn sweep_kit_values() {
    let Ok(game) = std::env::var("BLAM_TEST_SWEEP") else { return };
    let (var, game): (&str, &'static str) = match game.as_str() {
        "halo2_mcc" => ("BLAM_TEST_H2EK", "halo2_mcc"),
        "halo3_mcc" => ("BLAM_TEST_H3EK", "halo3_mcc"),
        "halo3odst_mcc" => ("BLAM_TEST_H3ODSTEK", "halo3odst_mcc"),
        "haloreach_mcc" => ("BLAM_TEST_HREK", "haloreach_mcc"),
        other => panic!("no kit variable for {other}"),
    };
    let root = std::path::PathBuf::from(std::env::var_os(var).expect(var));
    let tags = if root.join("tags").is_dir() { root.join("tags") } else { root };
    let limit: usize = std::env::var("BLAM_TEST_SWEEP_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let editors = StringEditors::load(format!("../definitions/{game}")).unwrap();
    let kit = KitTags::new(tags.clone(), game);
    // Groups whose files name an editor.
    let mut groups = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(format!("../definitions/{game}")).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        if text.contains("\"string_editor\"") && !path.file_name().unwrap().to_string_lossy().starts_with('_') {
            groups.insert(path.file_stem().unwrap().to_string_lossy().into_owned());
        }
    }
    let mut by_group: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
    let mut stack = vec![tags.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(ext) = path.extension().map(|e| e.to_string_lossy().into_owned()) {
                if groups.contains(&ext) {
                    let list = by_group.entry(ext).or_default();
                    if list.len() < limit {
                        list.push(path);
                    }
                }
            }
        }
    }
    #[derive(Default)]
    struct Count { accepted: usize, rejected: usize, empty_menu: usize, samples: Vec<String> }
    let mut counts: std::collections::BTreeMap<String, Count> = Default::default();
    for (_, files) in by_group {
        for file in files {
            let Some(tag) = kit.read(&file) else { continue };
            let rel = crate::paths::relative_tag_path(&file.strip_prefix(&tags).unwrap().with_extension(""));
            for (path, editor) in editor_fields(&editors, tag.root()) {
                let Some(field) = tag.root().field_path(&path) else { continue };
                let value = match field.value() {
                    Some(crate::fields::TagFieldData::StringId(id)) | Some(crate::fields::TagFieldData::OldStringId(id)) => id.string,
                    Some(crate::fields::TagFieldData::String(s)) | Some(crate::fields::TagFieldData::LongString(s)) => s,
                    _ => continue,
                };
                if value.is_empty() {
                    continue;
                }
                let ctx = Context { tag: &tag, tag_path: &rel, field_path: &path };
                let menu = editors.menu(&editor, &ctx, &kit);
                let count = counts.entry(format!("{editor} ({})", crate::fields::format_group_tag(tag.group().tag).trim())).or_default();
                if menu.picks().is_empty() {
                    count.empty_menu += 1;
                    if count.samples.len() < 4 {
                        count.samples.push(format!("(empty) {rel} {path} = {value:?}"));
                    }
                } else if menu.accepts(&value) {
                    count.accepted += 1;
                } else {
                    count.rejected += 1;
                    if count.samples.len() < 4 {
                        count.samples.push(format!("{rel} {path} = {value:?}"));
                    }
                }
            }
        }
    }
    eprintln!("{game}: editor  accepted / rejected / empty menu");
    for (editor, count) in &counts {
        eprintln!("  {editor:40} {:6} / {:5} / {:5}", count.accepted, count.rejected, count.empty_menu);
        for sample in &count.samples {
            eprintln!("      {sample}");
        }
    }
}

/// Prints one field's menu: `BLAM_TEST_MENU=<game>|<tag file>|<field path>`
/// with the kit's `BLAM_TEST_*` set. For looking into a sweep's rejects.
#[test]
fn print_menu() {
    let Ok(spec) = std::env::var("BLAM_TEST_MENU") else { return };
    let mut parts = spec.splitn(3, '|');
    let (game, file, path) = (parts.next().unwrap(), parts.next().unwrap(), parts.next().unwrap());
    let game: &'static str = Box::leak(game.to_owned().into_boxed_str());
    let file = std::path::PathBuf::from(file);
    let tags = file.ancestors().find(|dir| dir.file_name().is_some_and(|name| name == "tags")).unwrap().to_path_buf();
    let editors = StringEditors::load(format!("../definitions/{game}")).unwrap();
    let kit = KitTags::new(tags.clone(), game);
    let tag = kit.read(&file).unwrap();
    let rel = crate::paths::relative_tag_path(&file.strip_prefix(&tags).unwrap().with_extension(""));
    let Some(editor) = editors.editor_at(tag.root(), path) else {
        let (holder, ordinal) = holder_of(tag.root(), path).expect("the field");
        let guid: String = holder.definition().guid().iter().map(|b| format!("{b:02x}")).collect();
        panic!("no editor: holder {:?} guid {guid} ordinal {ordinal} field {:?}", holder.name(), holder.fields_all().nth(ordinal).map(|f| f.name().to_owned()));
    };
    let menu = editors.menu(editor, &Context { tag: &tag, tag_path: &rel, field_path: path }, &kit);
    fn show(items: &[MenuItem], depth: usize) {
        for item in items {
            eprintln!("{}{}{}", "  ".repeat(depth), item.text, if item.is_submenu() { " >" } else { "" });
            show(&item.children, depth + 1);
        }
    }
    eprintln!("editor {editor}, group {}", crate::fields::format_group_tag(tag.group().tag));
    show(&menu.items, 0);
    if let Some(note) = &menu.note { eprintln!("(note) {note}"); }
}

/// Foundation groups marker names by their `_` parts: a name alone in its
/// group is an item, a group is a submenu named by its shared parts, a name
/// that is exactly that prefix comes first in it, and a prefix every name
/// shares is skipped.
#[test]
fn marker_names_group_by_their_parts() {
    let split = |names: &[&str]| -> Vec<Vec<String>> { names.iter().map(|name| name.split('_').map(str::to_owned).collect()).collect() };
    let menu = build::marker_hierarchy(&split(&["body", "primary", "primary_ejection", "primary_trigger", "secondary_trigger"]), 0);
    assert_eq!(menu, vec![
        MenuItem::item("body"),
        MenuItem::submenu("primary", vec![MenuItem::item("primary"), MenuItem::item("primary_ejection"), MenuItem::item("primary_trigger")]),
        MenuItem::item("secondary_trigger"),
    ]);
    // Everything shares `left_hand`: that level is skipped, not a submenu.
    let menu = build::marker_hierarchy(&split(&["left_hand_a", "left_hand_b"]), 0);
    assert_eq!(menu, vec![MenuItem::item("left_hand_a"), MenuItem::item("left_hand_b")]);
}

/// `like` takes another editor's spec with some keys replaced; `null`
/// clears one. A `like` naming no editor fails the load.
#[test]
fn like_borrows_another_editors_spec() {
    let dir = std::env::temp_dir().join(format!("string_editor_like_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_meta.json"), r#"{"string_editors": {
        "base": {"caption": "Base", "builder": "list", "items": ["a", "b"]},
        "same": {"like": "base", "caption": null},
        "other": {"like": "base", "items": ["c"]}}}"#).unwrap();
    let editors = StringEditors::load(&dir).unwrap();
    assert_eq!(editors.caption("base"), "Base");
    assert_eq!(editors.caption("same"), "Select String", "null clears the caption");
    let source = KitTags::new(dir.clone(), "halo3_mcc");
    let tag = TagFile::new("../definitions/halo3_mcc/model.json").unwrap();
    let ctx = Context { tag: &tag, tag_path: "", field_path: "" };
    assert_eq!(editors.menu("same", &ctx, &source).picks(), vec!["a", "b"]);
    assert_eq!(editors.menu("other", &ctx, &source).picks(), vec!["c"]);
    std::fs::write(dir.join("_meta.json"), r#"{"string_editors": {"lost": {"like": "nothing"}}}"#).unwrap();
    let error = StringEditors::load(&dir).unwrap_err();
    std::fs::remove_dir_all(&dir).ok();
    assert!(matches!(error, StringEditorsError::BadLike { ref editor, .. } if editor == "lost"), "{error}");
}


#[test]
fn built_in_function_catalog_is_static_grouped_and_deterministic() {
    let dir = std::env::temp_dir().join(format!("builtin_function_catalog_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_meta.json"), r#"{
        "string_editors": {"object_function": {
            "builder": "object_function", "own_functions": true,
            "own": [{"block": "functions", "label": "export name"}],
            "object_types": ["vehicle"], "titles": {"vehicle": "Vehicles"},
            "lists": {"vehicle": ["engine_power", "ENGINE_POWER"],
                      "object": ["zero", "one"], "weapon": ["heat"]},
            "extra": [{"text": "custom seats", "groups": ["vehicle"],
                       "block": "seats", "label": "open function name"}],
            "trailing": [{"text": "scenario interpolators", "items": ["scenario_interpolator1"]}]
        }}
    }"#).unwrap();
    let editors = StringEditors::load(&dir).unwrap();
    let catalog = editors.built_in_function_menu().unwrap();
    assert_eq!(catalog, editors.built_in_function_menu().unwrap());
    assert_eq!(catalog.items, vec![
        MenuItem::submenu("object", vec![MenuItem::item("zero"), MenuItem::item("one")]),
        MenuItem::submenu("Vehicles", vec![MenuItem::item("engine_power")]),
        MenuItem::submenu("weapon", vec![MenuItem::item("heat")]),
        MenuItem::submenu("scenario interpolators", vec![MenuItem::item("scenario_interpolator1")]),
    ]);
    assert!(catalog.accepts("my_custom_function"), "suggestions must not reject manual names");
    std::fs::write(dir.join("_meta.json"), "{}").unwrap();
    assert!(StringEditors::load(&dir).unwrap().built_in_function_menu().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// `ref` is also present in Element and BlockRef rules. The more specific
/// variants must deserialize first rather than silently discarding context.
#[test]
fn contextual_reference_sources_keep_their_enclosing_element() {
    let element: build::Step = serde_json::from_str(r#"{"element":"objects","ref":"child object"}"#).unwrap();
    assert!(matches!(element, build::Step::Element { element, field } if element == "objects" && field == "child object"));
    let block: build::Step = serde_json::from_str(r#"{"block":"hud globals","ref":"string collection"}"#).unwrap();
    assert!(matches!(block, build::Step::BlockRef { block, field } if block == "hud globals" && field == "string collection"));
    let reference: build::Step = serde_json::from_str(r#"{"ref":"model"}"#).unwrap();
    assert!(matches!(reference, build::Step::Ref { field } if field == "model"));
}

#[test]
fn explanatory_headings_do_not_shift_string_editor_occurrences() {
    use serde_json::json;
    let dir = std::env::temp_dir().join(format!("string_editor_headings_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("_meta.json"), r#"{"string_editors": {
        "first": {"builder":"list","items":["a"]},
        "second": {"builder":"list","items":["b"]}}}"#).unwrap();
    let fields = vec![
        json!({"type":"explanation","name":"marker","definition":"first marker"}),
        json!({"type":"string_id","name":"marker","string_editor":"first"}),
        json!({"type":"explanation","name":"marker","definition":"second marker"}),
        json!({"type":"string_id","name":"marker","string_editor":"second"}),
        json!({"type":"terminator","name":null}),
    ];
    let schema = |fields| json!({"name":"test","tag":"tst!","version":1,"flags":0,"block":"root",
        "blocks":{"root":{"max_count":1,"struct":"root"}},
        "structs":{"root":{"guid":"12345678901234567890123456789012","size":8,"fields":fields}}});
    let path = dir.join("test.json");
    std::fs::write(&path, schema(fields.clone()).to_string()).unwrap();
    let editors = StringEditors::load(&dir).unwrap();
    for headings in [true, false] {
        let fields: Vec<_> = fields.iter().filter(|f| headings || f["type"] != "explanation").cloned().collect();
        std::fs::write(&path, schema(fields).to_string()).unwrap();
        let tag = TagFile::new(&path).unwrap();
        let bytes = tag.write_to_bytes().unwrap();
        for tag in [tag, TagFile::read_from_bytes(&bytes).unwrap()] {
            let root = tag.root();
            let actual: Vec<_> = root.fields_all().enumerate().filter_map(|(ordinal, field)| {
                if field.field_type() == crate::TagFieldType::Explanation {
                    assert!(editors.editor_of(root, ordinal).is_none());
                    None
                } else { editors.editor_of(root, ordinal) }
            }).collect();
            assert_eq!(actual, ["first", "second"]);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
