//! String editors: the menus of valid values the kit tools give some
//! `string_id` fields.
//!
//! A field the tools give a string editor carries the field tag `sted`, and
//! its definition points at one of a fixed set of editor objects: material
//! names from the globals tag, the tag's own dialogue vocalizations, object
//! function names, AI activities and so on. The dumps record the tag but not
//! the object, so the definitions name the editor on each field
//! (`"string_editor": "material"`), and `_meta.json`'s `string_editors` says
//! how each of the game's editors builds its menu.
//!
//! Each editor builds a tree of names for the tag being edited and the field
//! being edited, loading other tags (globals, a referenced object) through a
//! [`TagSource`] the caller provides. What counts as a valid value is
//! Foundation's rule ([`Menu::accepts`]), not the native one: Foundation's
//! field panel never asks the native editor. The research behind every
//! editor, per game, is in Baboon's `todo/string-editors.md`.

mod build;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use crate::{TagFile, TagStruct};

/// A menu of names, as the tools show it: items, and submenus of more items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menu {
    pub items: Vec<MenuItem>,
    /// Shown as a disabled entry when there is nothing to pick
    /// ("No object functions available.").
    pub note: Option<String>,
    /// The editor never marks a value invalid (Foundation's object function
    /// panel).
    pub accepts_anything: bool,
}

/// One entry of a [`Menu`]. An entry with children is a submenu and can't be
/// picked; an entry the tools let you pick *and* open lists itself as its
/// own first child, as the native editors do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuItem {
    pub text: String,
    pub children: Vec<MenuItem>,
}

impl MenuItem {
    pub fn item(text: impl Into<String>) -> Self { MenuItem { text: text.into(), children: Vec::new() } }
    pub fn submenu(text: impl Into<String>, children: Vec<MenuItem>) -> Self { MenuItem { text: text.into(), children } }
    pub fn is_submenu(&self) -> bool { !self.children.is_empty() }
}

impl Menu {
    /// Every item that can be picked, depth first, in menu order.
    pub fn picks(&self) -> Vec<&str> {
        fn walk<'a>(items: &'a [MenuItem], out: &mut Vec<&'a str>) {
            for item in items {
                if item.is_submenu() { walk(&item.children, out) } else { out.push(&item.text) }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }

    /// Whether `value` is one the menu offers, by Foundation's rule
    /// (`StringEditorMenu.IsValueValid`): an empty value is valid, a menu
    /// with nothing to pick accepts anything, and otherwise the value must
    /// equal an item, ignoring case. Submenu headers don't count.
    pub fn accepts(&self, value: &str) -> bool {
        if value.is_empty() || self.accepts_anything {
            return true;
        }
        let picks = self.picks();
        picks.is_empty() || picks.iter().any(|pick| eq_ignore_case(pick, value))
    }
}

/// .NET's `InvariantCultureIgnoreCase` for the ASCII names these menus hold.
fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.chars().flat_map(char::to_lowercase).eq(b.chars().flat_map(char::to_lowercase))
}

/// Opens the other tags an editor reads, read-only. `path` is the tag's
/// path relative to the tags folder, with `\` separators and no extension;
/// `group` its group. `None` when the tag doesn't exist or can't be read.
pub trait TagSource {
    fn tag(&self, group: u32, path: &str) -> Option<Arc<TagFile>>;

    /// The scenario that owns the tag at `path` (of `group`), for a tag split
    /// off a scenario (a resource tag, a structure bsp, pathfinding): the
    /// scenario that references it. `None` when the caller can't tell.
    fn owning_scenario(&self, _group: u32, _path: &str) -> Option<Arc<TagFile>> { None }
}

/// The tag and field an editor builds its menu for.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    /// The tag being edited.
    pub tag: &'a TagFile,
    /// Its path relative to the tags folder, `\`-separated, no extension.
    pub tag_path: &'a str,
    /// The edited field's path in the tag, as [`TagStruct::field_path`]
    /// takes it (`objects[3]/variant name`).
    pub field_path: &'a str,
}

/// The string editors of one game, read from its definitions folder with
/// [`StringEditors::load`].
#[derive(Debug, Default)]
pub struct StringEditors {
    /// Struct guid → its fields that have an editor (gen3).
    fields_by_guid: HashMap<[u8; 16], Vec<FieldEditor>>,
    /// Struct name → the same, for definitions without guids (Halo 2) and
    /// tags saved with a layout the definitions don't know.
    fields_by_name: HashMap<String, Vec<FieldEditor>>,
    /// Editor name → how it builds its menu, from `_meta.json`.
    specs: HashMap<String, Spec>,
    /// Group tag → its parent group's tag, from each group file.
    parents: HashMap<u32, u32>,
    /// Group name (the extension) → group tag, and back.
    groups_by_name: HashMap<String, u32>,
    group_names: HashMap<u32, String>,
}

/// A field with an editor: its clean name, and which of its struct's fields
/// of that name it is. Not its position: a tag's own layout can leave out
/// fields the definitions have (explanations), which moves every field
/// after them.
#[derive(Debug, Clone)]
struct FieldEditor {
    clean_name: String,
    occurrence: usize,
    editor: String,
}

#[derive(Debug)]
pub enum StringEditorsError {
    Io(std::io::Error),
    Json(std::path::PathBuf, serde_json::Error),
    /// A field naming an editor `_meta.json` doesn't describe.
    UnknownEditor { file: std::path::PathBuf, field: String, editor: String },
}

impl std::fmt::Display for StringEditorsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
            Self::UnknownEditor { file, field, editor } =>
                write!(f, "{}: field `{field}` names string editor `{editor}`, which _meta.json doesn't describe", file.display()),
        }
    }
}

impl std::error::Error for StringEditorsError {}

impl From<std::io::Error> for StringEditorsError {
    fn from(error: std::io::Error) -> Self { Self::Io(error) }
}

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    string_editors: HashMap<String, Spec>,
    #[serde(default)]
    tag_index: HashMap<String, String>,
}

#[derive(Deserialize)]
struct GroupFile {
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    parent_tag: Option<String>,
    #[serde(default)]
    structs: HashMap<String, StructFile>,
}

#[derive(Deserialize)]
struct StructFile {
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    fields: Vec<FieldFile>,
}

#[derive(Deserialize)]
struct FieldFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    string_editor: Option<String>,
}

/// How one editor builds its menu: `_meta.json`'s `string_editors` entry.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Spec {
    /// The menu button's text, as Foundation captions it.
    #[serde(default)]
    pub caption: Option<String>,
    /// `false`: the editor never marks a value invalid.
    #[serde(default = "validates")]
    pub validate: bool,
    #[serde(flatten)]
    pub builder: build::Builder,
}

fn validates() -> bool { true }

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let text = text.trim();
    if text.len() != 32 {
        return None;
    }
    let mut guid = [0u8; 16];
    for (i, byte) in guid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    (guid != [0; 16]).then_some(guid)
}

impl StringEditors {
    /// Read a game's editors from its definitions folder
    /// (`definitions/<game>`): each group file's fields that name an editor,
    /// and `_meta.json`'s `string_editors`. A game without editors (Halo CE)
    /// loads empty.
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, StringEditorsError> {
        let game_dir = game_dir.as_ref();
        let meta_path = game_dir.join("_meta.json");
        let meta: Meta = match std::fs::read(&meta_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|error| StringEditorsError::Json(meta_path.clone(), error))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Meta { string_editors: HashMap::new(), tag_index: HashMap::new() },
            Err(error) => return Err(error.into()),
        };
        let mut editors = StringEditors { specs: meta.string_editors, ..Default::default() };
        for (tag, name) in &meta.tag_index {
            if let Some(tag) = crate::fields::parse_group_tag(tag) {
                editors.groups_by_name.insert(name.clone(), tag);
                editors.group_names.insert(tag, name.clone());
            }
        }
        let mut entries: Vec<_> = std::fs::read_dir(game_dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else { continue };
            if !file_name.ends_with(".json") || file_name.starts_with('_') {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let text = String::from_utf8_lossy(&bytes);
            let group: GroupFile = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .map_err(|error| StringEditorsError::Json(path.clone(), error))?;
            if let (Some(tag), Some(parent)) = (group.tag.as_deref().and_then(crate::fields::parse_group_tag), group.parent_tag.as_deref().and_then(crate::fields::parse_group_tag)) {
                editors.parents.insert(tag, parent);
            }
            for (struct_name, structure) in &group.structs {
                let mut fields = Vec::new();
                let mut seen: HashMap<String, usize> = HashMap::new();
                for field in &structure.fields {
                    let name = field.name.as_deref().unwrap_or("");
                    let clean_name = crate::field_name::clean_field_name(name).into_owned();
                    let occurrence = seen.entry(clean_name.clone()).or_default();
                    *occurrence += 1;
                    let Some(editor) = &field.string_editor else { continue };
                    if !editors.specs.contains_key(editor) {
                        return Err(StringEditorsError::UnknownEditor { file: path.clone(), field: format!("{struct_name}/{name}"), editor: editor.clone() });
                    }
                    fields.push(FieldEditor { occurrence: *occurrence - 1, clean_name, editor: editor.clone() });
                }
                if fields.is_empty() {
                    continue;
                }
                if let Some(guid) = structure.guid.as_deref().and_then(parse_guid) {
                    editors.fields_by_guid.insert(guid, fields.clone());
                }
                editors.fields_by_name.insert(struct_name.clone(), fields);
            }
        }
        Ok(editors)
    }

    /// Whether the game has any string editors at all.
    pub fn is_empty(&self) -> bool { self.specs.is_empty() }

    /// The editor of the field at `ordinal` in `holder`, if it has one.
    pub fn editor_of(&self, holder: TagStruct<'_>, ordinal: usize) -> Option<&str> {
        let field = holder.fields_all().nth(ordinal)?;
        let clean = field.clean_name();
        let occurrence = holder.fields_all().take(ordinal).filter(|earlier| earlier.clean_name() == clean).count();
        let matches = |entry: &&FieldEditor| entry.occurrence == occurrence && entry.clean_name == clean;
        let guid = holder.definition().guid();
        (guid != [0; 16])
            .then(|| self.fields_by_guid.get(&guid))
            .flatten()
            .and_then(|fields| fields.iter().find(matches))
            .or_else(|| self.fields_by_name.get(holder.name())?.iter().find(matches))
            .map(|entry| entry.editor.as_str())
    }

    /// The editor of the field at `field_path` in `root`, if it has one.
    pub fn editor_at(&self, root: TagStruct<'_>, field_path: &str) -> Option<&str> {
        let (holder, ordinal) = holder_of(root, field_path)?;
        self.editor_of(holder, ordinal)
    }

    /// The menu button's text for `editor`: Foundation's caption, or its
    /// default, "Select String".
    pub fn caption(&self, editor: &str) -> &str {
        self.specs.get(editor).and_then(|spec| spec.caption.as_deref()).unwrap_or("Select String")
    }

    /// Why `editor` offers no list, for an editor Baboon can't build yet.
    pub fn unavailable_reason(&self, editor: &str) -> Option<&str> {
        match &self.specs.get(editor)?.builder {
            build::Builder::Unavailable { reason } => Some(reason.as_str()),
            _ => None,
        }
    }

    /// Build `editor`'s menu for the field `ctx` names. A menu that can't be
    /// built (a source tag is missing) comes back empty, which accepts
    /// anything, as an editor with nothing loaded does in Foundation.
    pub fn menu(&self, editor: &str, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let Some(spec) = self.specs.get(editor) else { return Menu::default() };
        let mut menu = build::build(self, &spec.builder, ctx, source);
        menu.accepts_anything |= !spec.validate;
        menu
    }

    /// The tags whose contents `editor`'s menu depends on besides the edited
    /// tag itself, so a caller can rebuild it when one changes. Best effort:
    /// fixed paths only.
    pub fn sources(&self, editor: &str) -> Vec<(u32, String)> {
        self.specs.get(editor).map(|spec| spec.builder.fixed_sources()).unwrap_or_default()
    }

    /// `group` and its ancestors, nearest first.
    pub(crate) fn group_chain(&self, group: u32) -> Vec<u32> {
        let mut chain = vec![group];
        while let Some(parent) = self.parents.get(chain.last().unwrap()) {
            if chain.contains(parent) || chain.len() > 16 {
                break;
            }
            chain.push(*parent);
        }
        chain
    }

    pub(crate) fn group_tag(&self, name: &str) -> Option<u32> {
        self.groups_by_name.get(name).copied().or_else(|| crate::fields::parse_group_tag(name))
    }

    pub(crate) fn group_name(&self, tag: u32) -> Option<&str> { self.group_names.get(&tag).map(String::as_str) }
}

/// The struct holding the field at `field_path` in `root`, and the field's
/// ordinal in it.
pub(crate) fn holder_of<'a>(root: TagStruct<'a>, field_path: &str) -> Option<(TagStruct<'a>, usize)> {
    let field = root.field_path(field_path)?;
    let holder = match field_path.rsplit_once('/') {
        Some((parent, _)) => root.descend(parent)?,
        None => root,
    };
    Some((holder, field.ordinal()))
}

#[cfg(test)]
mod tests;
