//! How each string editor builds its menu.
//!
//! Most editors list names from one block of one tag: the globals tag's
//! materials, the edited tag's own vocalizations, a referenced tag's
//! variants. [`Builder::Names`] covers all of those from data. The rest have
//! logic of their own (object functions, AI activities, incidents...) and a
//! builder each; their tables and field names are still data, in the
//! game's `_meta.json`.

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;

use super::{Context, Menu, MenuItem, StringEditors, TagSource};
use crate::fields::TagFieldData;
use crate::{TagFile, TagStruct};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "builder", rename_all = "snake_case")]
pub(crate) enum Builder {
    /// Names from blocks of a tag; see [`Names`].
    Names(Names),
    /// A fixed menu.
    List { items: Vec<ListItem> },
    ObjectFunction(ObjectFunction),
    AiActivities(AiActivities),
    Incident(Incident),
    /// The variants of the model of the object a cinematic scene object
    /// places.
    CinematicObjectVariant(CinematicObjectVariant),
    /// Whichever of several menus the field's surroundings pick; see
    /// [`Choice`].
    Choice(Choice),
    /// Sapien's object variant names; see [`ObjectVariant`].
    ObjectVariant(ObjectVariant),
    /// Foundation's object function panel on an animation graph; see
    /// [`ObjectFunctionPanel`].
    ObjectFunctionPanel(ObjectFunctionPanel),
    /// Foundation's marker name panel; see [`MarkerNames`].
    MarkerNames(MarkerNames),
    /// An editor whose list depends on something Baboon can't know yet;
    /// accepts anything, as an empty menu does.
    Unavailable {
        #[serde(default)]
        reason: String,
    },
}

/// A fixed menu entry: a name, or a submenu of more.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum ListItem {
    Item(String),
    Submenu { text: String, items: Vec<ListItem> },
}

fn list_items(items: &[ListItem]) -> Vec<MenuItem> {
    items
        .iter()
        .map(|item| match item {
            ListItem::Item(text) => MenuItem::item(text.clone()),
            ListItem::Submenu { text, items } => MenuItem::submenu(text.clone(), list_items(items)),
        })
        .collect()
}

/// Where a menu's names come from: a chain of tags, starting at the edited
/// tag (`"self"`), the globals tag (`"globals"`) or a fixed tag, and
/// following references from there.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum Step {
    /// `"self"`, `"globals"`, or `"scenario"`: the edited tag if it is a
    /// scenario, else the scenario that owns it.
    Named(String),
    /// A tag at a fixed path.
    Tag { tag: String, group: String },
    /// The tag a reference field of the current tag points at.
    Ref {
        #[serde(rename = "ref")]
        field: String,
    },
    /// The tag a reference field of the edited field's enclosing `element`
    /// of the named block points at (a cinematic scene object's model
    /// animation graph).
    Element {
        element: String,
        #[serde(rename = "ref")]
        field: String,
    },
}

fn default_source() -> Vec<Step> { vec![Step::Named("self".to_owned())] }

/// The tag `steps` lead to from the edited tag.
fn follow(editors: &StringEditors, steps: &[Step], ctx: &Context<'_>, source: &dyn TagSource) -> Option<Held> {
    let mut current: Option<Held> = None;
    for step in steps {
        current = Some(match step {
            Step::Named(name) if name == "self" => Held::Edited,
            Step::Named(name) if name == "scenario" => {
                if ctx.tag.group().tag == u32::from_be_bytes(*b"scnr") {
                    Held::Edited
                } else {
                    Held::Loaded(source.owning_scenario(ctx.tag.group().tag, ctx.tag_path)?)
                }
            }
            Step::Named(name) if name == "globals" => Held::Loaded(source.tag(u32::from_be_bytes(*b"matg"), "globals\\globals")?),
            Step::Named(_) => return None,
            Step::Tag { tag, group } => Held::Loaded(source.tag(editors.group_tag(group)?, tag)?),
            Step::Element { element, field } => Held::Loaded(source.tag_of(enclosing(ctx, element)?.field(field)?)?),
            Step::Ref { field } => {
                let root = current.as_ref().map(|held| held.root(ctx)).unwrap_or_else(|| ctx.tag.root());
                let (group, path) = inherited(root, field)?.read_ref()?;
                Held::Loaded(source.tag(group, &path)?)
            }
        });
    }
    current
}

/// A tag a builder reads: the edited one, or one the source loaded.
enum Held {
    Edited,
    Loaded(Arc<TagFile>),
}

impl Held {
    fn root<'a>(&'a self, ctx: &Context<'a>) -> TagStruct<'a> {
        match self {
            Held::Edited => ctx.tag.root(),
            Held::Loaded(tag) => tag.root(),
        }
    }
}

/// A field, looked up as a path from `root`, or failing that in the parent
/// group's struct a child group's root opens with (`weapon` → `item` →
/// `object`), where inherited fields live.
fn inherited<'a>(root: TagStruct<'a>, path: &str) -> Option<crate::TagField<'a>> {
    let mut holder = root;
    for _ in 0..8 {
        if let Some(field) = holder.field_path(path) {
            return Some(field);
        }
        holder = holder.fields().next()?.as_struct()?;
    }
    None
}

impl crate::TagField<'_> {
    fn text_of(value: &TagFieldData) -> Option<String> {
        match value {
            TagFieldData::String(text) | TagFieldData::LongString(text) => Some(text.clone()),
            TagFieldData::StringId(id) | TagFieldData::OldStringId(id) => Some(id.string.clone()),
            _ => None,
        }
    }
}

trait FieldText {
    fn text(&self) -> Option<String>;
    fn read_ref(&self) -> Option<(u32, String)>;
}

impl FieldText for crate::TagField<'_> {
    /// A string, long string or string id's text.
    fn text(&self) -> Option<String> {
        match self.value()? {
            TagFieldData::String(text) | TagFieldData::LongString(text) => Some(text),
            TagFieldData::StringId(id) | TagFieldData::OldStringId(id) => Some(id.string),
            _ => None,
        }
    }

    fn read_ref(&self) -> Option<(u32, String)> {
        match self.value()? {
            TagFieldData::TagReference(reference) => reference.group_tag_and_name,
            _ => None,
        }
    }
}

/// How siblings are ordered.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Sort {
    /// Block order (Halo 3, Reach).
    #[default]
    None,
    /// By name, ignoring case (Halo 4's editors).
    IgnoreCase,
    /// By `strcmp` (the material editors of Halo 2 and Halo 3).
    Bytes,
    /// Submenus before items, each by `strcmp` (the material editors of
    /// Reach and Halo 4).
    SubmenusFirstBytes,
}

fn sort(items: &mut [MenuItem], sort: Sort) {
    match sort {
        Sort::None => {}
        Sort::IgnoreCase => items.sort_by_key(|item| item.text.to_lowercase()),
        Sort::Bytes => items.sort_by(|a, b| a.text.as_bytes().cmp(b.text.as_bytes())),
        Sort::SubmenusFirstBytes => items.sort_by(|a, b| b.is_submenu().cmp(&a.is_submenu()).then_with(|| a.text.as_bytes().cmp(b.text.as_bytes()))),
    }
}

/// Names from a block: each element's `label` field, or the `label` of the
/// tag its `label_ref` reference points at. With `parent`, the names form a
/// tree by each element's parent name; with `nested`, each element is a
/// submenu of the names its own blocks hold.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Names {
    /// The edited tag's group must be one of these (by name), or the menu
    /// is `fallback`.
    #[serde(default)]
    require_group: Vec<String>,
    /// The menu when the editor can't build one: the wrong group, or a
    /// source tag that doesn't resolve. Empty when not given.
    #[serde(default)]
    fallback: Vec<String>,
    #[serde(default = "default_source")]
    source: Vec<Step>,
    #[serde(default)]
    sort: Sort,
    /// Tree by this field of each element, naming its parent element.
    #[serde(default)]
    parent: Option<String>,
    /// A parent lists itself as the first entry of its submenu.
    #[serde(default)]
    parent_selectable: bool,
    #[serde(flatten)]
    level: Level,
}

/// One block's worth of names.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Level {
    /// The block, as a path from the tag's root (or the enclosing element).
    block: String,
    /// The field naming each element. Several give one entry per non-empty
    /// field (a seat's open, opening and closing functions).
    #[serde(default)]
    label: Labels,
    /// The element's reference field; its tag's root `label` names it.
    #[serde(default)]
    label_ref: Option<String>,
    /// For `label_ref`: the name of an element whose reference doesn't
    /// resolve (`INVALID`, or `""`); left out when not given.
    #[serde(default)]
    unresolved: Option<String>,
    /// Each element is a submenu of these, in order.
    #[serde(default)]
    nested: Vec<Level>,
    /// Each element is a submenu of the same kind of block, to any depth
    /// (Halo 2's vocalizations and their children).
    #[serde(default)]
    recursive: Option<String>,
    /// An element whose submenu is empty is an item (the native rule);
    /// with this, it is left out instead.
    #[serde(default)]
    drop_empty_submenus: bool,
    /// Keep elements whose name is empty.
    #[serde(default)]
    keep_empty: bool,
    /// An element with a submenu lists itself as the submenu's first entry,
    /// so it can be picked too (Halo 2's vocalizations with children).
    #[serde(default)]
    self_first: bool,
    /// Only elements whose `field` equals the edited field's struct's
    /// `holder_field` (a model's region named by the placement's `region
    /// name`).
    #[serde(default)]
    matching: Option<Matching>,
    /// List each element's nested names in its place, not as a submenu.
    #[serde(default)]
    flatten: bool,
    /// Name each element by the `label` of the element of `block` (in the
    /// source tag) its `field` block index points at.
    #[serde(default)]
    label_index: Option<LabelIndex>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Matching {
    field: String,
    holder_field: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LabelIndex {
    field: String,
    block: String,
    label: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(untagged)]
pub(crate) enum Labels {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl Labels {
    fn fields(&self) -> &[String] {
        match self {
            Labels::None => &[],
            Labels::One(field) => std::slice::from_ref(field),
            Labels::Many(fields) => fields,
        }
    }
}

impl Level {
    fn items(&self, holder: TagStruct<'_>, source: &dyn TagSource, sort_by: Sort) -> Vec<MenuItem> {
        self.items_for(holder, None, source, sort_by)
    }

    /// `edited` is the struct holding the edited field, for `matching`.
    fn items_for(&self, holder: TagStruct<'_>, edited: Option<TagStruct<'_>>, source: &dyn TagSource, sort_by: Sort) -> Vec<MenuItem> {
        let Some(block) = inherited(holder, &self.block).and_then(|field| field.as_block()) else { return Vec::new() };
        let wanted = self.matching.as_ref().map(|matching| edited.and_then(|edited| edited.field(&matching.holder_field)?.text()).unwrap_or_default());
        let mut items = Vec::new();
        for element in block.iter() {
            if let (Some(matching), Some(wanted)) = (&self.matching, &wanted) {
                if element.field(&matching.field).and_then(|field| field.text()).as_deref() != Some(wanted.as_str()) {
                    continue;
                }
            }
            let children = self.children(element, source, sort_by);
            if self.flatten {
                items.extend(children);
                continue;
            }
            let names = match &self.label_index {
                Some(index) => element
                    .field(&index.field)
                    .and_then(|field| match field.value()? {
                        TagFieldData::CharBlockIndex(i) => Some(i as i64),
                        TagFieldData::ShortBlockIndex(i) => Some(i as i64),
                        TagFieldData::LongBlockIndex(i) => Some(i as i64),
                        TagFieldData::CharInteger(i) => Some(i as i64),
                        TagFieldData::ShortInteger(i) => Some(i as i64),
                        TagFieldData::LongInteger(i) => Some(i as i64),
                        _ => None,
                    })
                    .filter(|&i| i >= 0)
                    .and_then(|i| inherited(holder, &format!("{}[{i}]/{}", index.block, index.label)).and_then(|field| field.text()))
                    .into_iter()
                    .collect(),
                None => self.names(element, source),
            };
            for name in names {
                if !self.nested.is_empty() || self.recursive.is_some() {
                    if children.is_empty() && self.drop_empty_submenus {
                        continue;
                    }
                    let mut children = children.clone();
                    if self.self_first && !children.is_empty() {
                        children.insert(0, MenuItem::item(name.clone()));
                    }
                    items.push(MenuItem::submenu(name, children));
                } else {
                    items.push(MenuItem::item(name));
                }
            }
        }
        sort(&mut items, sort_by);
        items
    }

    fn names(&self, element: TagStruct<'_>, source: &dyn TagSource) -> Vec<String> {
        if let Some(reference) = &self.label_ref {
            let label = self.label.fields().first().map(String::as_str).unwrap_or("name");
            let resolved = element
                .field(reference)
                .and_then(|field| field.read_ref())
                .and_then(|(group, path)| source.tag(group, &path))
                .and_then(|tag| inherited(tag.root(), label).and_then(|field| field.text()));
            return match resolved.or_else(|| self.unresolved.clone()) {
                Some(name) => vec![name],
                None => Vec::new(),
            };
        }
        self.label
            .fields()
            .iter()
            .filter_map(|label| element.field_path(label).and_then(|field| field.text()))
            .filter(|name| self.keep_empty || !name.is_empty())
            .collect()
    }

    fn children(&self, element: TagStruct<'_>, source: &dyn TagSource, sort_by: Sort) -> Vec<MenuItem> {
        let mut children: Vec<MenuItem> = self.nested.iter().flat_map(|level| level.items(element, source, sort_by)).collect();
        if let Some(block) = &self.recursive {
            let level = Level { block: block.clone(), ..self.clone() };
            children.extend(level.items(element, source, sort_by));
        }
        children
    }
}

impl Names {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let fallback = || Menu { items: self.fallback.iter().map(MenuItem::item).collect(), ..Default::default() };
        if !self.require_group.is_empty() {
            let group = ctx.tag.group().tag;
            if !self.require_group.iter().any(|name| editors.group_tag(name) == Some(group)) {
                return fallback();
            }
        }
        let Some(held) = follow(editors, &self.source, ctx, source) else { return fallback() };
        let root = held.root(ctx);
        let items = match &self.parent {
            Some(parent) => self.tree(root, parent),
            None => self.level.items(root, source, self.sort),
        };
        if items.is_empty() { fallback() } else { Menu { items, ..Default::default() } }
    }

    /// The elements as a tree by `parent`: an element whose parent name
    /// isn't another element's name sits at the top.
    fn tree(&self, root: TagStruct<'_>, parent: &str) -> Vec<MenuItem> {
        let Some(block) = inherited(root, &self.level.block).and_then(|field| field.as_block()) else { return Vec::new() };
        let label = self.level.label.fields().first().map(String::as_str).unwrap_or("name");
        let nodes: Vec<(String, String)> = block
            .iter()
            .filter_map(|element| {
                let name = element.field(label)?.text()?;
                let parent = element.field(parent).and_then(|field| field.text()).unwrap_or_default();
                (!name.is_empty()).then_some((name, parent))
            })
            .collect();
        let index: HashMap<&str, usize> = nodes.iter().enumerate().rev().map(|(i, (name, _))| (name.as_str(), i)).collect();
        let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
        let mut top = Vec::new();
        for (i, (_, parent)) in nodes.iter().enumerate() {
            match index.get(parent.as_str()) {
                Some(&p) if p != i => children[p].push(i),
                _ => top.push(i),
            }
        }
        fn node(i: usize, nodes: &[(String, String)], children: &[Vec<usize>], names: &Names, depth: usize) -> MenuItem {
            let name = &nodes[i].0;
            if children[i].is_empty() || depth > 64 {
                return MenuItem::item(name.clone());
            }
            let mut kids: Vec<MenuItem> = children[i].iter().map(|&c| node(c, nodes, children, names, depth + 1)).collect();
            sort(&mut kids, names.sort);
            if names.parent_selectable {
                kids.insert(0, MenuItem::item(name.clone()));
            }
            MenuItem::submenu(name.clone(), kids)
        }
        let mut items: Vec<MenuItem> = top.iter().map(|&i| node(i, &nodes, &children, self, 0)).collect();
        sort(&mut items, self.sort);
        items
    }
}

/// Object function names: the tag's own exported functions, the engine's
/// fixed functions for each object type the tag is, and a few more lists
/// some games add.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ObjectFunction {
    /// List the tag's own functions (the editor instance on `primary scale`
    /// and the like) or not (`import name`, `turn off with`).
    own_functions: bool,
    /// The tag's own functions: blocks of its object struct.
    #[serde(default)]
    own: Vec<Level>,
    /// The object groups (by name) whose tags get the per-type lists: the
    /// engine's object types, not the abstract `object`, `unit`, `item` or
    /// `device`.
    object_types: Vec<String>,
    /// A level's submenu title where the engine's differs from the group's
    /// name (Halo 3's `device_terminal` is "terminal").
    #[serde(default)]
    titles: HashMap<String, String>,
    /// Group name → its fixed function names. The tag's group and its
    /// ancestors, root first, each give a submenu when they have a list.
    lists: HashMap<String, Vec<String>>,
    /// Further submenus for some object types.
    #[serde(default)]
    extra: Vec<ExtraSubmenu>,
    /// Fixed submenus last, once the object type is known.
    #[serde(default)]
    trailing: Vec<ListItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ExtraSubmenu {
    text: String,
    /// The object types (group names) that get it.
    groups: Vec<String>,
    #[serde(flatten)]
    level: Level,
}

impl ObjectFunction {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let group = ctx.tag.group().tag;
        let group_name = editors.group_name(group).unwrap_or("");
        let is_object = editors.group_chain(group).contains(&u32::from_be_bytes(*b"obje"));
        let mut items = Vec::new();
        if self.own_functions && is_object {
            let own: Vec<MenuItem> = self.own.iter().flat_map(|level| level.items(ctx.tag.root(), source, Sort::None)).collect();
            if !own.is_empty() {
                let title = ctx.tag_path.rsplit(['\\', '/']).next().unwrap_or(ctx.tag_path);
                items.push(MenuItem::submenu(title, own));
            }
        }
        if !self.object_types.iter().any(|name| name == group_name) {
            return Menu { items, ..Default::default() };
        }
        let chain: Vec<&str> = editors.group_chain(group).iter().rev().filter_map(|tag| editors.group_name(*tag)).collect();
        for level in chain {
            if let Some(names) = self.lists.get(level).filter(|names| !names.is_empty()) {
                let title = self.titles.get(level).map(String::as_str).unwrap_or(level);
                items.push(MenuItem::submenu(title, names.iter().map(MenuItem::item).collect()));
            }
        }
        for extra in &self.extra {
            if extra.groups.iter().any(|name| name == group_name) {
                let names = extra.level.items(ctx.tag.root(), source, Sort::None);
                if !names.is_empty() {
                    items.push(MenuItem::submenu(extra.text.clone(), names));
                }
            }
        }
        items.extend(list_items(&self.trailing));
        Menu { items, ..Default::default() }
    }
}

/// AI activities: a fixed table of activities, each with the postures
/// that play it, offered on a scenario's AI placements. Halo 3 and ODST
/// keep only the postures the placement's character can play.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct AiActivities {
    #[serde(default)]
    require_group: Vec<String>,
    #[serde(default)]
    fallback: Vec<String>,
    activities: Vec<Activity>,
    #[serde(default)]
    filter: Option<PostureFilter>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Activity {
    name: String,
    #[serde(default)]
    postures: Vec<String>,
}

/// Where Halo 3 and ODST find the character a placement places, and the
/// animation graph whose modes say which postures it has.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PostureFilter {
    /// Block name (as a path segment of the edited field) → the field of
    /// that block's element naming the character palette entry, or `null`
    /// to keep walking up.
    walk: Vec<(String, Option<String>)>,
    /// The scenario's character palette and the reference in its entries.
    palette: String,
    palette_ref: String,
    /// References from the character to its animation graph.
    chain: Vec<String>,
    /// The graph's modes block and the field naming each mode.
    modes: String,
    mode_label: String,
    /// The single item when the graph can't be reached.
    missing: String,
}

impl AiActivities {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let fallback = || Menu { items: self.fallback.iter().map(MenuItem::item).collect(), ..Default::default() };
        if !self.require_group.is_empty() && !self.require_group.iter().any(|name| editors.group_tag(name) == Some(ctx.tag.group().tag)) {
            return fallback();
        }
        let modes = match &self.filter {
            None => None,
            Some(filter) => match filter.modes(ctx, source) {
                Walk::Modes(modes) => Some(modes),
                Walk::NoCharacter => return fallback(),
                Walk::NoGraph => return Menu { items: vec![MenuItem::item(filter.missing.clone())], ..Default::default() },
            },
        };
        let mut items = Vec::new();
        for activity in &self.activities {
            if activity.postures.is_empty() {
                items.push(MenuItem::item(activity.name.clone()));
                continue;
            }
            let postures: Vec<&String> = match &modes {
                Some(modes) => activity.postures.iter().filter(|posture| modes.iter().any(|mode| mode == *posture)).collect(),
                None => activity.postures.iter().collect(),
            };
            if postures.is_empty() {
                continue;
            }
            let mut children = vec![MenuItem::item(activity.name.clone())];
            children.extend(postures.into_iter().map(MenuItem::item));
            items.push(MenuItem::submenu(activity.name.clone(), children));
        }
        Menu { items, ..Default::default() }
    }
}

enum Walk {
    Modes(Vec<String>),
    NoCharacter,
    NoGraph,
}

impl PostureFilter {
    fn modes(&self, ctx: &Context<'_>, source: &dyn TagSource) -> Walk {
        let root = ctx.tag.root();
        let segments: Vec<&str> = ctx.field_path.split('/').collect();
        let mut palette_index = None;
        // From the field's element up: the first block the walk knows names
        // the character, or says to keep going.
        'walk: for end in (1..segments.len()).rev() {
            let segment = segments[end - 1];
            let block = segment.split(['[', '#']).next().unwrap_or(segment);
            match self.walk.iter().find(|(name, _)| crate::field_name::clean_field_name(name) == crate::field_name::clean_field_name(block)) {
                Some((_, Some(field))) => {
                    let Some(element) = root.descend(&segments[..end].join("/")) else { break 'walk };
                    palette_index = element.field(field).and_then(|field| match field.value()? {
                        TagFieldData::CharBlockIndex(i) => Some(i as i64),
                        TagFieldData::ShortBlockIndex(i) => Some(i as i64),
                        TagFieldData::LongBlockIndex(i) => Some(i as i64),
                        _ => None,
                    });
                    // An unset character falls through to the enclosing
                    // block's (a starting location to its fire team).
                    if palette_index.is_some_and(|i| i >= 0) {
                        break 'walk;
                    }
                }
                Some((_, None)) => continue,
                None => break 'walk,
            }
        }
        let Some(index) = palette_index.filter(|&i| i >= 0) else { return Walk::NoCharacter };
        // A palette entry or character that doesn't resolve gives the
        // fallback; only a broken link past the character is "not found".
        let Some(entry) = root.descend(&format!("{}[{index}]", self.palette)) else { return Walk::NoCharacter };
        let mut tag = match entry.field(&self.palette_ref).and_then(|field| field.read_ref()).and_then(|(g, p)| source.tag(g, &p)) {
            Some(tag) => tag,
            None => return Walk::NoCharacter,
        };
        for reference in &self.chain {
            let next = inherited(tag.root(), reference).and_then(|field| field.read_ref()).and_then(|(g, p)| source.tag(g, &p));
            match next {
                Some(next) => tag = next,
                None => return Walk::NoGraph,
            }
        }
        let Some(block) = inherited(tag.root(), &self.modes).and_then(|field| field.as_block()) else { return Walk::NoGraph };
        Walk::Modes(block.iter().filter_map(|mode| mode.field(&self.mode_label)?.text()).collect())
    }
}

/// Incidents: a submenu of the engine's built-in incident names, then one
/// per incident definitions tag the globals reach, titled with its path.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Incident {
    built_in_title: String,
    built_in: Vec<String>,
    /// Where the incident definitions tags are: a block of references
    /// (Reach) or one reference (Halo 4), reached from the globals.
    definitions: IncidentDefinitions,
    /// The block of each definitions tag naming its incidents.
    incidents: Level,
    /// Group each list into `#` and `A`–`Z` by first letter (Halo 4).
    #[serde(default)]
    letter_buckets: bool,
    #[serde(default)]
    sort: Sort,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct IncidentDefinitions {
    #[serde(default = "default_source")]
    source: Vec<Step>,
    /// A block of the source tag whose elements reference definitions tags,
    /// and the reference field; or just the reference field.
    #[serde(default)]
    block: Option<String>,
    reference: String,
}

impl Incident {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let bucket = |names: Vec<MenuItem>| -> Vec<MenuItem> {
            if !self.letter_buckets {
                return names;
            }
            let mut buckets: Vec<MenuItem> = std::iter::once('#').chain('A'..='Z').map(|c| MenuItem::item(c.to_string())).collect();
            for name in names {
                let first = name.text.chars().next().map(|c| c.to_ascii_uppercase()).unwrap_or('#');
                let slot = if first.is_ascii_uppercase() { (first as u8 - b'A') as usize + 1 } else { 0 };
                buckets[slot].children.push(name);
            }
            for b in &mut buckets {
                sort(&mut b.children, self.sort);
            }
            buckets
        };
        let mut built_in: Vec<MenuItem> = self.built_in.iter().map(MenuItem::item).collect();
        sort(&mut built_in, self.sort);
        let mut items = vec![MenuItem::submenu(self.built_in_title.clone(), bucket(built_in))];
        let Some(held) = follow(editors, &self.definitions.source, ctx, source) else { return Menu { items, ..Default::default() } };
        let root = held.root(ctx);
        let references: Vec<Option<(u32, String)>> = match &self.definitions.block {
            Some(block) => inherited(root, block)
                .and_then(|field| field.as_block())
                .map(|block| block.iter().map(|element| element.field(&self.definitions.reference).and_then(|field| field.read_ref())).collect())
                .unwrap_or_default(),
            None => vec![inherited(root, &self.definitions.reference).and_then(|field| field.read_ref())],
        };
        for reference in references {
            let Some((group, path)) = reference else {
                if self.definitions.block.is_some() {
                    items.push(MenuItem::item(String::new()));
                }
                continue;
            };
            let Some(tag) = source.tag(group, &path) else { continue };
            let names = self.incidents.items(tag.root(), source, self.sort);
            items.push(MenuItem::submenu(path, bucket(names)));
        }
        Menu { items, ..Default::default() }
    }
}

/// The variants of the model of the object a cinematic scene object places:
/// the field's enclosing object element, its object reference, that
/// object's model, the model's variants.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CinematicObjectVariant {
    require_group: Vec<String>,
    /// The block (a path segment of the edited field) whose element is the
    /// scene object.
    object_block: String,
    /// The scene object's reference to the object it places.
    object: String,
    /// The object's model reference, and the model's variants.
    model: String,
    variants: Level,
    /// Halo 4: the scene object holds no reference of its own. Its `name`
    /// picks the element of the same name in the `block` of the tag the
    /// scene's `data` reference points at, which holds `object`.
    #[serde(default)]
    named_in: Option<NamedIn>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NamedIn {
    data: String,
    block: String,
    name: String,
}

impl CinematicObjectVariant {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        if !self.require_group.iter().any(|name| editors.group_tag(name) == Some(ctx.tag.group().tag)) {
            return Menu::default();
        }
        let Some(element) = enclosing(ctx, &self.object_block) else { return Menu::default() };
        let menu = (|| {
            let object = match &self.named_in {
                None => source.tag_of(element.field(&self.object)?)?,
                Some(named) => {
                    let name = element.field(&named.name)?.text()?;
                    let data = source.tag_of(inherited(ctx.tag.root(), &named.data)?)?;
                    let block = inherited(data.root(), &named.block)?.as_block()?;
                    let matched = block.iter().find(|candidate| candidate.field(&named.name).and_then(|field| field.text()).as_deref() == Some(name.as_str()))?;
                    source.tag_of(matched.field(&self.object)?)?
                }
            };
            let model = source.tag_of(inherited(object.root(), &self.model)?)?;
            Some(self.variants.items(model.root(), source, Sort::None))
        })();
        Menu { items: menu.unwrap_or_default(), ..Default::default() }
    }
}

/// Picks one of several menus by where the field sits: the first case
/// whose `inside` names a block on the field's path (or that has none)
/// wins.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct When {
    field: String,
    equals: Vec<String>,
}

impl When {
    fn holds(&self, edited: Option<TagStruct<'_>>) -> bool {
        let Some(value) = edited.and_then(|edited| edited.field(&self.field)?.value()) else { return false };
        let text = match value {
            TagFieldData::CharEnum { value, name } => name.unwrap_or_else(|| value.to_string()),
            TagFieldData::ShortEnum { value, name } => name.unwrap_or_else(|| value.to_string()),
            TagFieldData::LongEnum { value, name } => name.unwrap_or_else(|| value.to_string()),
            TagFieldData::CharInteger(v) => v.to_string(),
            TagFieldData::ShortInteger(v) => v.to_string(),
            TagFieldData::LongInteger(v) => v.to_string(),
            TagFieldData::CharBlockIndex(v) => v.to_string(),
            TagFieldData::ShortBlockIndex(v) => v.to_string(),
            TagFieldData::LongBlockIndex(v) => v.to_string(),
            other => match crate::TagField::text_of(&other) { Some(text) => text, None => return false },
        };
        self.equals.iter().any(|wanted| crate::field_name::clean_field_name(wanted) == crate::field_name::clean_field_name(&text))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Choice {
    cases: Vec<Case>,
    #[serde(default)]
    sort: Sort,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Case {
    /// A block name that must appear on the edited field's path.
    #[serde(default)]
    inside: Option<String>,
    /// A field of the struct holding the edited field, and the values (an
    /// enum's option name, or a number) that pick this case.
    #[serde(default)]
    when: Option<When>,
    /// Steps from the edited tag to the tag to read.
    #[serde(default = "default_source")]
    source: Vec<Step>,
    #[serde(flatten)]
    level: Level,
}

impl Choice {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let blocks: Vec<String> = ctx
            .field_path
            .split('/')
            .map(|segment| crate::field_name::clean_field_name(segment.split(['[', '#']).next().unwrap_or(segment)).into_owned())
            .collect();
        let edited = super::holder_of(ctx.tag.root(), ctx.field_path).map(|(holder, _)| holder);
        let case = self.cases.iter().find(|case| {
            let inside = match &case.inside {
                Some(inside) => blocks.iter().any(|block| *block == crate::field_name::clean_field_name(inside)),
                None => true,
            };
            inside && case.when.as_ref().is_none_or(|when| when.holds(edited))
        });
        let Some(case) = case else { return Menu::default() };
        let Some(held) = follow(editors, &case.source, ctx, source) else { return Menu::default() };
        Menu { items: case.level.items_for(held.root(ctx), edited, source, self.sort), ..Default::default() }
    }
}

/// The variant names of the object a scenario placement places, as Sapien
/// lists them: the placement's `type` indexes its block's palette, whose
/// entry references the object; the object's model holds the variants.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ObjectVariant {
    /// Placement block (as named in the field's path) → its palette block.
    palettes: HashMap<String, String>,
    /// The placement's palette index field, and the palette entry's
    /// reference.
    #[serde(rename = "type")]
    type_field: String,
    palette_ref: String,
    /// The object's model reference, and the model's variants.
    model: String,
    variants: Level,
}

impl ObjectVariant {
    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let segments: Vec<&str> = ctx.field_path.split('/').collect();
        let found = (1..segments.len()).rev().find_map(|end| {
            let segment = segments[end - 1];
            let name = crate::field_name::clean_field_name(segment.split(['[', '#']).next().unwrap_or(segment)).into_owned();
            let palette = self.palettes.iter().find(|(block, _)| crate::field_name::clean_field_name(block) == name)?.1;
            segment.contains('[').then(|| (segments[..end].join("/"), palette.clone()))
        });
        let Some((placement, palette)) = found else { return Menu::default() };
        let items = (|| {
            let element = ctx.tag.root().descend(&placement)?;
            let index = match element.field(&self.type_field)?.value()? {
                TagFieldData::CharBlockIndex(i) => i as i64,
                TagFieldData::ShortBlockIndex(i) => i as i64,
                TagFieldData::LongBlockIndex(i) => i as i64,
                _ => return None,
            };
            if index < 0 {
                return None;
            }
            // The palette is the edited tag's own when it has one, else the
            // owning scenario's.
            let own = inherited(ctx.tag.root(), &format!("{palette}[{index}]/{}", self.palette_ref)).and_then(|field| field.read_ref());
            let reference = match own {
                Some(reference) => reference,
                None => {
                    let scenario = follow(editors, &[Step::Named("scenario".to_owned())], ctx, source)?;
                    inherited(scenario.root(ctx), &format!("{palette}[{index}]/{}", self.palette_ref))?.read_ref()?
                }
            };
            let object = source.tag(reference.0, &reference.1)?;
            let model = source.tag_of(inherited(object.root(), &self.model)?)?;
            Some(self.variants.items(model.root(), source, Sort::None))
        })();
        Menu { items: items.unwrap_or_default(), ..Default::default() }
    }
}

/// Foundation's panel for the object function fields of an animation graph
/// (`coff`): the object the graph animates, found by its path, gets its
/// object function menu, and the graph's own suspension function names
/// follow. `fp_X` in `<dir>` animates `objects\weapons\<dir>\X\X.weapon`;
/// otherwise a biped, vehicle or creature at the graph's own path.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ObjectFunctionPanel {
    /// The editor to run on the object.
    with: String,
    /// The graph's names: their block, field, and the submenu's title.
    graph_block: String,
    graph_label: String,
    graph_title: String,
    /// The disabled entry when there's nothing.
    empty: String,
}

impl ObjectFunctionPanel {
    fn object(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Option<(Arc<TagFile>, String)> {
        let short = ctx.tag_path.rsplit('\\').next().unwrap_or(ctx.tag_path);
        // As Foundation does: the folder before the short name's *first*
        // appearance in the path (`...\\support_high\\fp_x\\fp_x` →
        // `support_high`).
        let before = ctx.tag_path.find(short).and_then(|at| ctx.tag_path.get(..at.checked_sub(1)?));
        if let (Some(weapon), Some(before)) = (short.strip_prefix("fp_").filter(|name| !name.is_empty()), before) {
            let dir = before.rsplit('\\').next().unwrap_or(before);
            let path = format!("objects\\weapons\\{dir}\\{weapon}\\{weapon}");
            if let Some(tag) = editors.group_tag("weapon").and_then(|group| source.tag(group, &path)) {
                return Some((tag, path));
            }
        }
        ["biped", "vehicle", "creature"]
            .iter()
            .find_map(|group| source.tag(editors.group_tag(group)?, ctx.tag_path).map(|tag| (tag, ctx.tag_path.to_owned())))
    }

    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let empty = || Menu { note: Some(self.empty.clone()), ..Default::default() };
        let Some((object, path)) = self.object(editors, ctx, source) else { return empty() };
        let object_ctx = Context { tag: &object, tag_path: &path, field_path: "" };
        let mut menu = editors.menu(&self.with, &object_ctx, source);
        let mut names: Vec<String> = Vec::new();
        if let Some(block) = ctx.tag.root().field_path(&self.graph_block).and_then(|field| field.as_block()) {
            for element in block.iter() {
                if let Some(name) = element.field(&self.graph_label).and_then(|field| field.text()) {
                    if !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
                        names.push(name);
                    }
                }
            }
        }
        names.sort_by_key(|name| name.to_lowercase());
        if !names.is_empty() {
            menu.items.push(MenuItem::submenu(self.graph_title.clone(), names.into_iter().map(MenuItem::item).collect()));
        }
        if menu.items.is_empty() { empty() } else { menu }
    }
}

/// Foundation's marker name panel (`amkr`): the marker names of a render
/// model, grouped into submenus by their `_`-separated parts. Which render
/// model comes from the first of `providers` that applies to the tag and
/// field, as `MarkerNameMenuPanelItemCache` lists them.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MarkerNames {
    providers: Vec<MarkerProvider>,
    /// From a model, object or render model to its markers.
    model: String,
    render_model: String,
    markers: String,
    marker_label: String,
    /// The disabled entry when no render model is found.
    empty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MarkerProvider {
    /// The tag's group, or one it descends from (by name); any when absent.
    #[serde(default)]
    group: Option<String>,
    /// The edited field's path without indices must end with this.
    #[serde(default)]
    field: Option<String>,
    from: MarkerSource,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MarkerSource {
    /// The edited tag itself.
    Itself,
    /// A reference field of the edited tag (or its parent group's struct).
    Reference(String),
    /// From the struct holding the edited field: `..` climbs out of a
    /// struct or element, `{"index": f, "block": b}` follows a block index
    /// into the root's block `b`, `{"index": f, "palettes": {...}}` into
    /// the palette of the placement block it sits in; the last step names
    /// the reference.
    Relative(Vec<RelativeStep>),
    /// The tag of this group at the edited tag's own path.
    SamePath(String),
    /// A fixed list, no model.
    List(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum RelativeStep {
    Name(String),
    Index {
        index: String,
        #[serde(default)]
        block: Option<String>,
        #[serde(default)]
        palettes: HashMap<String, String>,
    },
}

/// A field path with its indices and ordinals taken out, as clean names.
fn bare_path(path: &str) -> Vec<String> {
    path.split('/').map(|segment| crate::field_name::clean_field_name(segment.split(['[', '#']).next().unwrap_or(segment)).into_owned()).collect()
}

fn block_index(field: crate::TagField<'_>) -> Option<i64> {
    match field.value()? {
        TagFieldData::CharBlockIndex(i) => Some(i as i64),
        TagFieldData::ShortBlockIndex(i) => Some(i as i64),
        TagFieldData::LongBlockIndex(i) => Some(i as i64),
        _ => None,
    }
}

impl MarkerNames {
    fn applies(&self, editors: &StringEditors, provider: &MarkerProvider, ctx: &Context<'_>) -> bool {
        if let Some(group) = &provider.group {
            let Some(wanted) = editors.group_tag(group) else { return false };
            if !editors.group_chain(ctx.tag.group().tag).contains(&wanted) {
                return false;
            }
        }
        match &provider.field {
            Some(field) => {
                let path = bare_path(ctx.field_path);
                let wanted = bare_path(field);
                path.len() >= wanted.len() && path[path.len() - wanted.len()..] == wanted[..]
            }
            None => true,
        }
    }

    /// The tag the provider names, or the fixed list it gives.
    fn source(&self, editors: &StringEditors, from: &MarkerSource, ctx: &Context<'_>, source: &dyn TagSource) -> Result<Option<Held>, Vec<String>> {
        let root = ctx.tag.root();
        Ok(match from {
            MarkerSource::Itself => Some(Held::Edited),
            MarkerSource::Reference(field) => inherited(root, field).and_then(|field| source.tag_of(field)).map(Held::Loaded),
            MarkerSource::SamePath(group) => editors.group_tag(group).and_then(|group| source.tag(group, ctx.tag_path)).map(Held::Loaded),
            MarkerSource::List(items) => return Err(items.clone()),
            MarkerSource::Relative(steps) => (|| {
                let segments: Vec<&str> = ctx.field_path.split('/').collect();
                let mut at: Vec<&str> = segments[..segments.len().saturating_sub(1)].to_vec();
                let mut element = None;
                let (last, walk) = steps.split_last()?;
                for step in walk {
                    match step {
                        RelativeStep::Name(name) if name == ".." => {
                            at.pop()?;
                        }
                        RelativeStep::Name(_) => return None,
                        RelativeStep::Index { index, block, palettes } => {
                            let holder = if at.is_empty() { root } else { root.descend(&at.join("/"))? };
                            let i = block_index(holder.field(index)?)?;
                            if i < 0 {
                                return None;
                            }
                            let block = match block {
                                Some(block) => block.clone(),
                                None => {
                                    let placement = at.iter().rev().find_map(|segment| {
                                        let name = crate::field_name::clean_field_name(segment.split(['[', '#']).next().unwrap_or(segment)).into_owned();
                                        palettes.iter().find(|(placement, _)| crate::field_name::clean_field_name(placement) == name).map(|(_, palette)| palette.clone())
                                    })?;
                                    placement
                                }
                            };
                            element = Some(format!("{block}[{i}]"));
                            at.clear();
                        }
                    }
                }
                let RelativeStep::Name(reference) = last else { return None };
                let holder = match (&element, at.is_empty()) {
                    (Some(element), _) => root.descend(element)?,
                    (None, true) => root,
                    (None, false) => root.descend(&at.join("/"))?,
                };
                source.tag_of(holder.field(reference)?)
            })()
            .map(Held::Loaded),
        })
    }

    /// The render model a model, an object or a render model leads to.
    fn render_model(&self, editors: &StringEditors, held: Held, ctx: &Context<'_>, source: &dyn TagSource) -> Option<Held> {
        let mut held = held;
        for _ in 0..3 {
            let group = match &held {
                Held::Edited => ctx.tag.group().tag,
                Held::Loaded(tag) => tag.group().tag,
            };
            let chain = editors.group_chain(group);
            let next = if chain.contains(&u32::from_be_bytes(*b"mode")) {
                return Some(held);
            } else if chain.contains(&u32::from_be_bytes(*b"hlmt")) {
                &self.render_model
            } else if chain.contains(&u32::from_be_bytes(*b"obje")) {
                &self.model
            } else {
                return None;
            };
            let tag = source.tag_of(inherited(held.root(ctx), next)?)?;
            held = Held::Loaded(tag);
        }
        None
    }

    fn build(&self, editors: &StringEditors, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
        let empty = || Menu { note: Some(self.empty.clone()), ..Default::default() };
        let Some(provider) = self.providers.iter().find(|provider| self.applies(editors, provider, ctx)) else { return empty() };
        let held = match self.source(editors, &provider.from, ctx, source) {
            Err(list) => return Menu { items: list.into_iter().map(MenuItem::item).collect(), ..Default::default() },
            Ok(Some(held)) => held,
            Ok(None) => return empty(),
        };
        let Some(model) = self.render_model(editors, held, ctx, source) else { return empty() };
        let Some(block) = inherited(model.root(ctx), &self.markers).and_then(|field| field.as_block()) else { return empty() };
        let mut names: Vec<String> = Vec::new();
        for element in block.iter() {
            if let Some(name) = element.field(&self.marker_label).and_then(|field| field.text()) {
                if !names.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
                    names.push(name);
                }
            }
        }
        names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a)));
        let split: Vec<Vec<String>> = names.iter().map(|name| name.split('_').map(str::to_owned).collect()).collect();
        let items = marker_hierarchy(&split, 0);
        if items.is_empty() { empty() } else { Menu { items, ..Default::default() } }
    }
}

/// Foundation's `InitMenuHierarchyByName`: names split on `_`, grouped by
/// their parts from `depth` on; a group of one is an item, a larger one a
/// submenu named by the parts its members share, which lists first a member
/// that is exactly that prefix.
pub(super) fn marker_hierarchy(ids: &[Vec<String>], depth: usize) -> Vec<MenuItem> {
    let ids: Vec<&Vec<String>> = ids.iter().filter(|id| id.len() > depth).collect();
    if ids.len() <= 1 {
        return ids.first().map(|id| vec![MenuItem::item(id.join("_"))]).unwrap_or_default();
    }
    let mut max_depth = depth;
    let groups = loop {
        let mut groups: Vec<(String, Vec<Vec<String>>)> = Vec::new();
        for id in &ids {
            let key = id.get(max_depth).cloned().unwrap_or_default();
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push((*id).clone()),
                None => groups.push((key, vec![(*id).clone()])),
            }
        }
        max_depth += 1;
        if groups.len() != 1 || max_depth > 64 {
            break groups;
        }
    };
    let mut items = Vec::new();
    for (_, members) in groups {
        let first = &members[0];
        if members.len() == 1 {
            items.push(MenuItem::item(first.join("_")));
            continue;
        }
        let mut children = marker_hierarchy(&members, max_depth);
        if first.len() == max_depth {
            children.insert(0, MenuItem::item(first.join("_")));
        }
        let prefix = first[..max_depth.min(first.len())].join("_");
        items.push(MenuItem::submenu(prefix, children));
    }
    items
}

/// The element of `block` the edited field sits in.
fn enclosing<'a>(ctx: &Context<'a>, block: &str) -> Option<TagStruct<'a>> {
    let segments: Vec<&str> = ctx.field_path.split('/').collect();
    let wanted = crate::field_name::clean_field_name(block);
    for end in (1..segments.len()).rev() {
        let segment = segments[end - 1];
        let name = segment.split(['[', '#']).next().unwrap_or(segment);
        if crate::field_name::clean_field_name(name) == wanted && segment.contains('[') {
            return ctx.tag.root().descend(&segments[..end].join("/"));
        }
    }
    None
}

trait LoadRef {
    fn tag_of(&self, field: crate::TagField<'_>) -> Option<Arc<TagFile>>;
}

impl LoadRef for dyn TagSource + '_ {
    fn tag_of(&self, field: crate::TagField<'_>) -> Option<Arc<TagFile>> {
        let (group, path) = field.read_ref()?;
        self.tag(group, &path)
    }
}

impl Builder {
    /// Tags at fixed paths the menu reads.
    pub(crate) fn fixed_sources(&self) -> Vec<(u32, String)> {
        let steps: &[Step] = match self {
            Builder::Names(names) => &names.source,
            Builder::Incident(incident) => &incident.definitions.source,
            _ => &[],
        };
        steps
            .iter()
            .filter_map(|step| match step {
                Step::Named(name) if name == "globals" => Some((u32::from_be_bytes(*b"matg"), "globals\\globals".to_owned())),
                Step::Tag { tag, group } => crate::fields::parse_group_tag(group).map(|group| (group, tag.clone())),
                _ => None,
            })
            .collect()
    }
}

pub(crate) fn build(editors: &StringEditors, builder: &Builder, ctx: &Context<'_>, source: &dyn TagSource) -> Menu {
    match builder {
        Builder::Names(names) => names.build(editors, ctx, source),
        Builder::List { items } => Menu { items: list_items(items), ..Default::default() },
        Builder::ObjectFunction(spec) => spec.build(editors, ctx, source),
        Builder::AiActivities(spec) => spec.build(editors, ctx, source),
        Builder::Incident(spec) => spec.build(editors, ctx, source),
        Builder::CinematicObjectVariant(spec) => spec.build(editors, ctx, source),
        Builder::Choice(spec) => spec.build(editors, ctx, source),
        Builder::ObjectVariant(spec) => spec.build(editors, ctx, source),
        Builder::ObjectFunctionPanel(spec) => spec.build(editors, ctx, source),
        Builder::MarkerNames(spec) => spec.build(editors, ctx, source),
        Builder::Unavailable { .. } => Menu::default(),
    }
}
