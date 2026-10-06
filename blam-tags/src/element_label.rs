//! Block element labels, as the games' own editors write them.
//!
//! Every Halo editor names a block element with one function: Guerilla's
//! `tag_block_format_element` (CE, Halo 2, Halo 3, ODST) and ManagedBlam's
//! native equivalent behind Foundation (Reach, Halo 4, H2A MP). Their order is
//! the same in every game:
//!
//! 1. a bad index gives `NONE` (−1) or `BAD` / `BAD: %d`;
//! 2. a block (CE, H2) or struct (gen3) with a label callback uses that
//!    callback's text, even when empty (the overlay, not yet here);
//! 3. otherwise the first field whose raw name contains `^`, walking inline
//!    structs and arrays, formatted by the generic value formatter;
//! 4. otherwise `"%d. %s"` with the block's display name (CE, H2) or the
//!    struct's short name (gen3).
//!
//! The per-game differences are data: `definitions/<game>/_element_labels.json`
//! carries the settings, and the dumps carry the names step 4 prints (a block's
//! `display_name`, a struct's `short_name`, each written only when it differs
//! from the key). The rules were read from each editor's code; the plan in
//! Baboon's `todo/block-element-labels.md` lists the functions.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::api::{TagBlock, TagField, TagStruct};
use crate::fields::{TagFieldData, TagFieldType};

/// What the generic rule needs to know about one game. Loaded from its
/// definitions folder with [`ElementLabels::load`].
#[derive(Debug, Clone, Default)]
pub struct ElementLabels {
    settings: Settings,
    /// Block key → display name, where they differ (CE, H2).
    block_display_names: HashMap<String, String>,
    /// Struct guid → short name, where it differs from the key (gen3).
    struct_short_names: HashMap<[u8; 16], String>,
    /// Each struct definition's first `^` field, by guid (gen3) and by name.
    /// A tag's own layout can't say which field is marked: shipped tags and
    /// layouts built from the definitions both store field names with their
    /// markup stripped, so the marker comes from the definitions.
    markers_by_guid: HashMap<[u8; 16], Vec<Marker>>,
    markers_by_name: HashMap<String, Vec<Marker>>,
}

/// Where a struct definition's first `^` field sits: its position among the
/// struct's fields and its clean name, which must both match a loaded struct's
/// field for it to count (a tag saved with an older layout may differ).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Marker {
    ordinal: usize,
    clean_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Settings {
    /// Where a label callback sits: on the block (CE, H2) or the struct (gen3).
    #[allow(dead_code)]
    callback: CallbackScope,
    /// Whose name the `"%d. %s"` fallback prints.
    fallback_name: CallbackScope,
    /// The text for an out-of-range index other than −1: `BAD` or `BAD: %d`.
    bad_index: String,
    /// Flags print as a number (CE, H2) or as their set options' names (gen3).
    flags: FlagsStyle,
}

impl Default for Settings {
    /// Gen3's rules, which every editor since Halo 3 shares.
    fn default() -> Self {
        Settings {
            callback: CallbackScope::Struct,
            fallback_name: CallbackScope::Struct,
            bad_index: "BAD: %d".to_owned(),
            flags: FlagsStyle::Names,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CallbackScope {
    Block,
    Struct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FlagsStyle {
    Number,
    Names,
}

#[derive(Deserialize)]
struct LabelsFile {
    settings: Settings,
}

#[derive(Deserialize)]
struct GroupNames {
    #[serde(default)]
    blocks: HashMap<String, BlockNames>,
    #[serde(default)]
    structs: HashMap<String, StructNames>,
}

#[derive(Deserialize)]
struct BlockNames {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct StructNames {
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    short_name: Option<String>,
    #[serde(default)]
    fields: Vec<FieldNames>,
}

#[derive(Deserialize)]
struct FieldNames {
    #[serde(default)]
    name: Option<String>,
}

/// Why a game's label rules couldn't be loaded.
#[derive(Debug)]
pub enum ElementLabelsError {
    Io(std::io::Error),
    Json(std::path::PathBuf, serde_json::Error),
}

impl std::fmt::Display for ElementLabelsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for ElementLabelsError {}

impl From<std::io::Error> for ElementLabelsError {
    fn from(error: std::io::Error) -> Self { Self::Io(error) }
}

impl ElementLabels {
    /// Load one game's rules from its definitions folder
    /// (`definitions/halo3_mcc`). A folder with no `_element_labels.json`
    /// gets gen3's rules.
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, ElementLabelsError> {
        let game_dir = game_dir.as_ref();
        let settings_path = game_dir.join("_element_labels.json");
        let settings = match std::fs::read(&settings_path) {
            Ok(bytes) => serde_json::from_slice::<LabelsFile>(&bytes)
                .map_err(|error| ElementLabelsError::Json(settings_path.clone(), error))?
                .settings,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(error) => return Err(error.into()),
        };
        let mut labels = ElementLabels { settings, ..ElementLabels::default() };
        for entry in std::fs::read_dir(game_dir)? {
            let path = entry?.path();
            let is_group = path.extension().is_some_and(|ext| ext == "json")
                && !path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('_'));
            if !is_group {
                continue;
            }
            let group: GroupNames = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| ElementLabelsError::Json(path.clone(), error))?;
            for (key, block) in group.blocks {
                if let Some(display) = block.display_name {
                    labels.block_display_names.insert(key, display);
                }
            }
            for (key, structure) in group.structs {
                let guid = structure.guid.as_deref().and_then(parse_guid).filter(|g| *g != [0; 16]);
                if let (Some(guid), Some(short)) = (guid, structure.short_name) {
                    labels.struct_short_names.insert(guid, short);
                }
                let marker = structure.fields.iter().enumerate().find_map(|(ordinal, field)| {
                    let name = field.name.as_deref()?;
                    name.contains('^').then(|| Marker {
                        ordinal,
                        clean_name: crate::field_name::clean_field_name(name).into_owned(),
                    })
                });
                if let Some(marker) = marker {
                    if let Some(guid) = guid {
                        push_unique(labels.markers_by_guid.entry(guid).or_default(), marker.clone());
                    }
                    push_unique(labels.markers_by_name.entry(key).or_default(), marker);
                }
            }
        }
        Ok(labels)
    }

    /// The label of element `index` of `block`.
    ///
    /// `ancestors` are the structs that hold `block`, outermost (the tag's
    /// root) first and the struct with the block field last. They are where a
    /// block-index field looks for its target block, after the element itself.
    pub fn label(&self, ancestors: &[TagStruct<'_>], block: TagBlock<'_>, index: i64) -> String {
        let element = usize::try_from(index).ok().and_then(|i| block.element(i));
        let Some(element) = element else {
            return self.bad_index(index);
        };
        if let Some(label) = self.marked_field_label(ancestors, element) {
            return label;
        }
        format!("{index}. {}", self.fallback_name(block, element))
    }

    /// The label of element `index` of the block at `block_path` under `root`,
    /// e.g. `"squads[2]/spawn points"`. `None` when the path doesn't reach a
    /// block.
    pub fn label_at(&self, root: TagStruct<'_>, block_path: &str, index: i64) -> Option<String> {
        let mut ancestors = vec![root];
        let mut segments = block_path.split('/').peekable();
        let mut so_far = String::new();
        let mut block = None;
        while let Some(segment) = segments.next() {
            if segments.peek().is_none() {
                block = ancestors.last()?.field_path(segment)?.as_block();
                break;
            }
            if !so_far.is_empty() {
                so_far.push('/');
            }
            so_far.push_str(segment);
            ancestors.push(root.descend(&so_far)?);
        }
        Some(self.label(&ancestors, block?, index))
    }

    /// The field whose value labels `element` under the `^` rule, if any.
    pub fn marked_field<'a>(&self, element: TagStruct<'a>) -> Option<TagField<'a>> {
        self.find_marked_field(element).map(|(field, _)| field)
    }

    fn bad_index(&self, index: i64) -> String {
        if index == -1 {
            "NONE".to_owned()
        } else {
            self.settings.bad_index.replace("%d", &index.to_string())
        }
    }

    fn fallback_name<'a>(&'a self, block: TagBlock<'a>, element: TagStruct<'a>) -> &'a str {
        match self.settings.fallback_name {
            CallbackScope::Block => {
                let name = block.definition().name();
                self.block_display_names.get(name).map(String::as_str).unwrap_or(name)
            }
            CallbackScope::Struct => self
                .struct_short_names
                .get(&element.definition().guid())
                .map(String::as_str)
                .unwrap_or_else(|| element.name()),
        }
    }

    /// Step 3: the first `^` field, in flattened field order, formatted.
    fn marked_field_label(&self, ancestors: &[TagStruct<'_>], element: TagStruct<'_>) -> Option<String> {
        let (field, holder) = self.find_marked_field(element)?;
        let mut chain = ancestors.to_vec();
        chain.push(element);
        if holder.raw().as_ptr() != element.raw().as_ptr() {
            chain.push(holder);
        }
        Some(self.format_value(&chain, field))
    }

    /// The generic value formatter, with separator `,` and verbose off.
    /// `chain` ends with the struct holding `field`.
    fn format_value(&self, chain: &[TagStruct<'_>], field: TagField<'_>) -> String {
        use TagFieldType as T;
        match field.field_type() {
            T::Block => return field.as_block().map_or(0, |b| b.len()).to_string(),
            T::Data => return field.as_data().map_or(0, <[u8]>::len).to_string(),
            T::Struct | T::Array | T::Pad | T::UselessPad | T::Skip | T::Custom | T::Explanation
            | T::VertexBuffer | T::NonCacheRuntimeValue | T::Pointer | T::RealMatrix3x3
            | T::Terminator | T::Unknown => return " ".to_owned(),
            T::PageableResource => {
                return field
                    .as_resource()
                    .and_then(|resource| resource.as_struct())
                    .map(|inner| self.marked_field_label(chain, inner).unwrap_or_default())
                    .unwrap_or_else(|| "<unavailable>".to_owned());
            }
            T::ApiInterop => return "<unavailable>".to_owned(),
            _ => {}
        }
        let value = field.value().or_else(|| legacy_inline_old_string_id(field, chain.last()));
        let Some(value) = value else {
            return " ".to_owned();
        };
        use TagFieldData as D;
        match value {
            D::String(text) | D::LongString(text) => text,
            D::StringId(id) | D::OldStringId(id) => id.string,
            D::CharInteger(v) => v.to_string(),
            D::ShortInteger(v) => v.to_string(),
            D::LongInteger(v) => v.to_string(),
            // Gen3 reads byte and word integers as signed.
            D::ByteInteger(v) => (v as i8).to_string(),
            D::WordInteger(v) => (v as i16).to_string(),
            D::DwordInteger(v) => (v as i32).to_string(),
            D::Int64Integer(_) | D::QwordInteger(_) => " ".to_owned(),
            D::Tag(tag) => tag.to_be_bytes().iter().map(|&b| b as char).collect(),
            D::CharEnum { name, .. } | D::ShortEnum { name, .. } | D::LongEnum { name, .. } => {
                name.unwrap_or_else(|| " ".to_owned())
            }
            D::ByteFlags { value, names } => self.flags(value as i64, names),
            D::WordFlags { value, names } => self.flags(value as i16 as i64, names),
            D::LongFlags { value, names } => self.flags(value as i64, names),
            D::ByteBlockFlags(v) => v.to_string(),
            D::WordBlockFlags(v) => (v as i16).to_string(),
            D::LongBlockFlags(v) => v.to_string(),
            D::CharBlockIndex(v) | D::CustomCharBlockIndex(v) => self.block_index(chain, field, v as i64),
            D::ShortBlockIndex(v) | D::CustomShortBlockIndex(v) => self.block_index(chain, field, v as i64),
            D::LongBlockIndex(v) | D::CustomLongBlockIndex(v) => self.block_index(chain, field, v as i64),
            D::Angle(v) => g6(degrees(v)),
            D::Real(v) | D::RealSlider(v) | D::RealFraction(v) => g6(v as f64),
            D::Point2d(p) => format!("{},{}", p.x, p.y),
            D::Rectangle2d(r) => format!("{},{},{},{}", r.top, r.left, r.bottom, r.right),
            D::ShortIntegerBounds(b) => format!("{},{}", b.lower, b.upper),
            D::RealPoint2d(p) => join(&[p.x, p.y]),
            D::RealVector2d(v) => join(&[v.i, v.j]),
            D::FractionBounds(b) => join(&[b.lower, b.upper]),
            D::RealBounds(b) => join(&[b.lower, b.upper]),
            D::RealPoint3d(p) => join(&[p.x, p.y, p.z]),
            D::RealVector3d(v) => join(&[v.i, v.j, v.k]),
            D::RealPlane2d(p) => join(&[p.i, p.j, p.d]),
            D::RealRgbColor(c) => join(&[c.red, c.green, c.blue]),
            D::RealHsvColor(c) => join(&[c.hue, c.saturation, c.value]),
            D::RealQuaternion(q) => join(&[q.i, q.j, q.k, q.w]),
            D::RealPlane3d(p) => join(&[p.i, p.j, p.k, p.d]),
            D::RealArgbColor(c) => join(&[c.alpha, c.red, c.green, c.blue]),
            D::RealAhsvColor(c) => join(&[c.alpha, c.hue, c.saturation, c.value]),
            D::RealEulerAngles2d(e) => join_degrees(&[e.yaw, e.pitch]),
            D::AngleBounds(b) => join_degrees(&[b.lower, b.upper]),
            D::RealEulerAngles3d(e) => join_degrees(&[e.yaw, e.pitch, e.roll]),
            // The stored dword. Gen3 round-trips these through a real color
            // first, which hasn't been read; no `^` field in any game is a
            // packed color (checked over every definitions folder).
            D::RgbColor(c) => (c.0 as i32).to_string(),
            D::ArgbColor(c) => (c.0 as i32).to_string(),
            D::TagReference(reference) => reference
                .group_tag_and_name
                .map(|(_, path)| file_name(&path).to_owned())
                .unwrap_or_default(),
            D::Data(bytes) => bytes.len().to_string(),
            D::ApiInterop(_) => "<unavailable>".to_owned(),
            D::Custom(_) => " ".to_owned(),
        }
    }

    /// The first `^` field, walking inline structs and arrays (their first
    /// element) in declaration order, and the struct that holds it.
    fn find_marked_field<'a>(&self, holder: TagStruct<'a>) -> Option<(TagField<'a>, TagStruct<'a>)> {
        let marker = self.marker(holder);
        for (ordinal, field) in holder.fields_all().enumerate() {
            if marker.is_some_and(|m| m.ordinal == ordinal && m.clean_name == field.clean_name()) {
                return Some((field, holder));
            }
            let inner = match field.field_type() {
                TagFieldType::Struct => field.as_struct(),
                TagFieldType::Array => field.as_array().and_then(|array| array.element(0)),
                _ => None,
            };
            if let Some(found) = inner.and_then(|inner| self.find_marked_field(inner)) {
                return Some(found);
            }
        }
        None
    }

    /// The marker recorded for `holder`'s definition whose field it matches.
    fn marker(&self, holder: TagStruct<'_>) -> Option<&Marker> {
        let fields: Vec<_> = holder.fields_all().map(|f| f.clean_name().into_owned()).collect();
        let matches = |m: &&Marker| fields.get(m.ordinal).is_some_and(|name| *name == m.clean_name);
        let by_guid = self.markers_by_guid.get(&holder.definition().guid());
        by_guid
            .and_then(|markers| markers.iter().find(matches))
            .or_else(|| self.markers_by_name.get(holder.name())?.iter().find(matches))
    }

    fn flags(&self, value: i64, names: Vec<(u32, String)>) -> String {
        match self.settings.flags {
            FlagsStyle::Number => value.to_string(),
            FlagsStyle::Names => {
                let mut names = names;
                names.sort_by_key(|(bit, _)| *bit);
                names.into_iter().map(|(_, name)| name).collect::<Vec<_>>().join(",")
            }
        }
    }

    /// A block index takes its target element's label: the whole rule again,
    /// on the block the field points into.
    fn block_index(&self, chain: &[TagStruct<'_>], field: TagField<'_>, index: i64) -> String {
        if index == -1 {
            return "NONE".to_owned();
        }
        match find_target_block(chain, field) {
            Some((depth, block)) => self.label(&chain[..=depth], block, index),
            None => self.bad_index(index),
        }
    }
}

/// The block a block-index field points into: the first field holding a block
/// of the target definition, searched in the field's own struct and then
/// outward through its ancestors. Returns the depth in `chain` of the struct
/// that holds the found block.
fn find_target_block<'a>(chain: &[TagStruct<'a>], field: TagField<'_>) -> Option<(usize, TagBlock<'a>)> {
    let target = field.definition().block_index_target()?;
    let target = target.name();
    chain.iter().enumerate().rev().find_map(|(depth, holder)| {
        holder
            .fields_all()
            .filter_map(|candidate| candidate.as_block())
            .find(|block| block.definition().name() == target)
            .map(|block| (depth, block))
    })
}

/// Halo 2's oldest tags store an `old_string_id` as 32 inline bytes that the
/// layout reads as absent.
fn legacy_inline_old_string_id(field: TagField<'_>, holder: Option<&TagStruct<'_>>) -> Option<TagFieldData> {
    if field.field_type() != TagFieldType::OldStringId {
        return None;
    }
    let offset = field.definition().offset() as usize;
    let bytes = holder?.raw().get(offset..offset + 32)?;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let text = std::str::from_utf8(&bytes[..end]).ok()?.trim();
    let printable = !text.is_empty() && text.bytes().all(|b| (0x20..=0x7e).contains(&b));
    printable.then(|| TagFieldData::OldStringId(crate::fields::StringIdData { string: text.to_owned() }))
}

/// A tag path's file name: the text after the last `\`.
fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

/// Radians to degrees the way the editors do it: in 32-bit float.
fn degrees(radians: f32) -> f64 {
    (radians * 57.29578_f32) as f64
}

fn join(values: &[f32]) -> String {
    values.iter().map(|&v| g6(v as f64)).collect::<Vec<_>>().join(",")
}

fn join_degrees(values: &[f32]) -> String {
    values.iter().map(|&v| g6(degrees(v))).collect::<Vec<_>>().join(",")
}

/// C's `printf("%.6g", value)`.
pub fn g6(value: f64) -> String {
    const PRECISION: i32 = 6;
    if value.is_nan() {
        return "nan".to_owned();
    }
    if value.is_infinite() {
        return if value < 0.0 { "-inf" } else { "inf" }.to_owned();
    }
    if value == 0.0 {
        return if value.is_sign_negative() { "-0" } else { "0" }.to_owned();
    }
    // The exponent after rounding to six significant digits decides the style.
    let scientific = format!("{:.*e}", (PRECISION - 1) as usize, value);
    let (mantissa, exponent) = scientific.split_once('e').unwrap_or((&scientific, "0"));
    let exponent: i32 = exponent.parse().unwrap_or(0);
    if exponent < -4 || exponent >= PRECISION {
        let mantissa = trim_fraction(mantissa);
        let sign = if exponent < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exponent.abs())
    } else {
        let decimals = (PRECISION - 1 - exponent).max(0) as usize;
        trim_fraction(&format!("{value:.decimals$}")).to_owned()
    }
}

fn trim_fraction(text: &str) -> &str {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        text
    }
}

fn push_unique(markers: &mut Vec<Marker>, marker: Marker) {
    if !markers.contains(&marker) {
        markers.push(marker);
    }
}

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut guid = [0u8; 16];
    for (i, byte) in guid.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(guid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g6_matches_c() {
        let cases = [
            (0.0, "0"),
            (1.0, "1"),
            (0.5, "0.5"),
            (0.01, "0.01"),
            (123456.0, "123456"),
            (1234567.0, "1.23457e+06"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (-2.5, "-2.5"),
            (100.0, "100"),
            (999999.5, "1e+06"),
            (57.29578, "57.2958"),
            (0.1f32 as f64, "0.1"),
        ];
        for (value, want) in cases {
            assert_eq!(g6(value), want, "{value}");
        }
    }

    #[test]
    fn degrees_round_in_f32() {
        assert_eq!(g6(degrees(std::f32::consts::FRAC_PI_2)), "90");
        assert_eq!(g6(degrees(1.0)), "57.2958");
    }

    #[test]
    fn file_name_drops_folders() {
        assert_eq!(file_name("objects\\characters\\cyborg\\cyborg"), "cyborg");
        assert_eq!(file_name("cyborg"), "cyborg");
        assert_eq!(file_name(""), "");
    }

    #[test]
    fn bad_index_text_follows_the_game() {
        let classic = ElementLabels {
            settings: Settings { bad_index: "BAD".to_owned(), ..Settings::default() },
            ..ElementLabels::default()
        };
        assert_eq!(classic.bad_index(-1), "NONE");
        assert_eq!(classic.bad_index(7), "BAD");
        assert_eq!(ElementLabels::default().bad_index(7), "BAD: 7");
    }

    /// A kit's tags folder from its `BLAM_TEST_*` variable (the kit root or
    /// its `tags` folder), or `None` with a note naming the variable.
    fn kit_tags(var: &str) -> Option<std::path::PathBuf> {
        let Some(root) = std::env::var_os(var).map(std::path::PathBuf::from) else {
            eprintln!("skipping: set {var}");
            return None;
        };
        Some(if root.join("tags").is_dir() { root.join("tags") } else { root })
    }

    fn rules(game: &str) -> ElementLabels {
        ElementLabels::load(format!("../definitions/{game}")).expect("load label rules")
    }

    fn classic_tag(game: &str, path: &std::path::Path) -> crate::TagFile {
        let group = path.extension().unwrap().to_string_lossy().into_owned();
        let layout = crate::TagLayout::from_json(format!("../definitions/{game}/{group}.json")).unwrap();
        crate::classic::read_classic_tag_file(&std::fs::read(path).unwrap(), layout).unwrap()
    }

    fn label(rules: &ElementLabels, tag: &crate::TagFile, block: &str, index: i64) -> String {
        rules.label_at(tag.root(), block, index).unwrap_or_else(|| panic!("no block {block}"))
    }

    /// CE's markers come from the definitions: the tag's layout strips `^`.
    /// A tag reference prints its file name; a struct with no `^` and no
    /// callback falls back to "%d. <block name>".
    #[test]
    fn ce_labels_follow_guerilla() {
        let Some(tags) = kit_tags("BLAM_TEST_HCEEK") else { return };
        let rules = rules("haloce_mcc");
        let a50 = classic_tag("haloce_mcc", &tags.join("levels/a50/a50_cinema.scenario"));
        assert_eq!(label(&rules, &a50, "object names", 0), "keyes");
        assert_eq!(label(&rules, &a50, "skies", 0), "skynight0");
        assert_eq!(label(&rules, &a50, "player starting locations", 1), "1. scenario_players_block");
        assert_eq!(label(&rules, &a50, "object names", -1), "NONE");
        assert_eq!(label(&rules, &a50, "object names", 9999), "BAD");
        let model = classic_tag("haloce_mcc", &tags.join("weapons/sniper rifle/sniper rifle.gbxmodel"));
        assert_eq!(label(&rules, &model, "shaders", 0), "sniper rifle metal");
    }

    /// H2: an enum prints its option name, an old_string_id its string.
    #[test]
    fn h2_labels_follow_guerilla() {
        let Some(tags) = kit_tags("BLAM_TEST_H2EK") else { return };
        let rules = rules("halo2_mcc");
        let shader = classic_tag("halo2_mcc", &tags.join("ui/hud/shaders/ammo_meter.shader"));
        assert_eq!(label(&rules, &shader, "parameters[0]/animation properties", 0), "value");
        let effect = classic_tag("halo2_mcc", &tags.join("sound/weapons/plasma_grenade/throwgren.effect"));
        assert_eq!(label(&rules, &effect, "locations", 0), "root");
    }

    /// H3: enums and flags print names; a block index takes its target
    /// element's label; an unmarked struct falls back to its name.
    #[test]
    fn h3_labels_follow_guerilla() {
        let Some(tags) = kit_tags("BLAM_TEST_H3EK") else { return };
        let rules = rules("halo3_mcc");
        let chud = crate::TagFile::read(tags.join("ui/chud/globals.chud_globals_definition")).unwrap();
        assert_eq!(label(&rules, &chud, "skins", 1), "dervish");
        assert_eq!(label(&rules, &chud, "skins[0]/curvature infos", 1), "720p halfscreen");
        let decorators = crate::TagFile::read(
            tags.join("objects/levels/shared/flood_pustule/flood_pustule/flood_pustule.decorator_set"),
        )
        .unwrap();
        assert_eq!(label(&rules, &decorators, "decorator types", 0), "%flood_pustule");
        let jungle = crate::TagFile::read(tags.join("levels/solo/010_jungle/010_jungle.scenario")).unwrap();
        assert_eq!(label(&rules, &jungle, "zone set pvs", 0), "0. scenario_zone_set_pvs_block");
        assert_eq!(label(&rules, &jungle, "zone set pvs", 9999), "BAD: 9999");
    }

    /// Reach: a long block index recurses, several set flags join with `,`,
    /// and a real prints as `%.6g`.
    #[test]
    fn reach_labels_follow_managedblam() {
        let Some(tags) = kit_tags("BLAM_TEST_HREK") else { return };
        let rules = rules("haloreach_mcc");
        let objects = crate::TagFile::read(tags.join("multiplayer/globals.multiplayer_object_type_list")).unwrap();
        assert_eq!(label(&rules, &objects, "weapons", 0), "dmr");
        let settings =
            crate::TagFile::read(tags.join("multiplayer/game_engine_settings.game_engine_settings_definition")).unwrap();
        assert_eq!(
            label(&rules, &settings, "survival variants[1]/round properties", 1),
            "skull_tough_luck,skull_catch",
        );
        let deaths = crate::TagFile::read(tags.join("objects/characters/default.death_program_selector")).unwrap();
        assert_eq!(label(&rules, &deaths, "special type[0]/damage type[0]/velocity", 1), "0.6");
    }
}
