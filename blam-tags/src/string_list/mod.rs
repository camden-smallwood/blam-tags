//! String list tags as a list of strings: Halo 2 on's
//! `multilingual_unicode_string_list` (string ids, one text per language, all
//! the text in one UTF-8 blob) and Halo CE's `unicode_string_list` (one UTF-16
//! text per element). Read from a tag, edited, and written back whole.
//!
//! The text blob is rebuilt the way every tool from Halo 2 on writes it
//! (`import_text.cpp`): each string's languages in order, each text followed
//! by a NUL, nothing shared. Every Halo 3 and Reach kit tag is already laid
//! out that way, so an edit leaves the rest of the blob byte for byte as it
//! was. A language with no text has offset -1, as the tools write it; the
//! tools never store an empty text.

mod markup;
mod syntax;
#[cfg(test)]
mod tests;

pub use markup::*;
pub use syntax::*;

use crate::game::GameId;
use crate::{StringIdData, TagFieldData, TagFile, TagStruct, TagStructMut};

/// `multilingual_unicode_string_list`.
pub const MULTILINGUAL_GROUP: u32 = u32::from_be_bytes(*b"unic");
/// Halo CE's `unicode_string_list`.
pub const CLASSIC_GROUP: u32 = u32::from_be_bytes(*b"ustr");

const REFERENCES_BLOCK: &str = "string references";
const TEXT_DATA: &str = "string data utf8";
const STRING_ID: &str = "string id";
const CLASSIC_TEXT: &str = "string";
const OFFSET_SUFFIX: &str = " offset";

/// The most UTF-16 units of one text a tool imports: Halo 2 on cut a string
/// at 1023; Halo CE's text data holds 32768 bytes, NUL included.
const MULTILINGUAL_MAX_UNITS: usize = 1023;
const CLASSIC_MAX_UNITS: usize = 16383;

pub fn is_string_list_group(group_tag: u32) -> bool {
    group_tag == MULTILINGUAL_GROUP || group_tag == CLASSIC_GROUP
}

/// How a string list keeps its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// String ids and per-language offsets into one UTF-8 blob.
    Multilingual,
    /// One UTF-16 text per element, no ids, one language.
    Classic,
}

/// One language column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    /// As the editor shows it: `Spanish (Latin America)`.
    pub name: String,
    /// The element field holding its offset: `mexican spanish offset`.
    field: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringEntry {
    /// `None` for Halo CE.
    pub id: Option<String>,
    /// One per language; `None` where a language has no text.
    pub texts: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringList {
    pub storage: Storage,
    pub languages: Vec<Language>,
    pub entries: Vec<StringEntry>,
    /// Offsets that point past the end of the text: read as no text, and
    /// written back as -1. One Halo 2 kit tag has them
    /// (`scenarios\descriptions\anchor_point`, whose text was cut to its
    /// first string).
    pub damaged_offsets: usize,
    /// The `substitution pairs` block (Reach on): the (component, property)
    /// names `{cui:…}` bindings are looked up by. Empty where there is none.
    pub pairs: Vec<(String, String)>,
}

impl StringList {
    /// Whether the pairs hold a binding's (component, property), as the game
    /// looks it up.
    pub fn has_pair(&self, component: &str, property: &str) -> bool {
        let (component, property) = (fold_binding_name(component), fold_binding_name(property));
        self.pairs.iter().any(|(first, second)| *first == component && *second == property)
    }

    /// The most UTF-16 units one text may hold before a tool would cut it.
    pub fn max_text_units(&self) -> usize {
        match self.storage {
            Storage::Multilingual => MULTILINGUAL_MAX_UNITS,
            Storage::Classic => CLASSIC_MAX_UNITS,
        }
    }

    /// An entry with no text, and an id no other entry has.
    pub fn new_entry(&self) -> StringEntry {
        let id = match self.storage {
            Storage::Classic => None,
            Storage::Multilingual => {
                let taken = |id: &str| self.entries.iter().any(|entry| entry.id.as_deref() == Some(id));
                let id = std::iter::once("new_string".to_owned())
                    .chain((1..).map(|n| format!("new_string_{n}")))
                    .find(|id| !taken(id))
                    .expect("unbounded");
                Some(id)
            }
        };
        StringEntry { id, texts: vec![None; self.languages.len()] }
    }
}

/// Read `tag`'s strings, or `None` if it is not a string list this reads.
pub fn read_string_list(tag: &TagFile) -> Option<StringList> {
    let root = tag.root();
    let block = root.field_path(REFERENCES_BLOCK)?.as_block()?;
    if let Some(data) = root.field_path(TEXT_DATA) {
        let data = data.as_data().unwrap_or(&[]);
        let languages = block_languages(&root)?;
        let mut damaged_offsets = 0;
        let entries = block
            .iter()
            .map(|element| StringEntry {
                id: element.read_string_id(STRING_ID),
                texts: languages
                    .iter()
                    .map(|language| {
                        let offset = element.read_int_any(&language.field)?;
                        if offset >= 0 && offset >= data.len() as i128 {
                            damaged_offsets += 1;
                        }
                        text_at(data, offset)
                    })
                    .collect(),
            })
            .collect();
        let pairs = read_pairs(&root);
        return Some(StringList { storage: Storage::Multilingual, languages, entries, damaged_offsets, pairs });
    }
    // Halo CE: one UTF-16 text per element.
    let entries = block
        .iter()
        .map(|element| {
            let bytes = element.field_path(CLASSIC_TEXT).and_then(|field| field.as_data()).unwrap_or(&[]);
            StringEntry { id: None, texts: vec![Some(decode_utf16_text(bytes)).filter(|text| !text.is_empty())] }
        })
        .collect();
    Some(StringList {
        storage: Storage::Classic,
        languages: vec![Language { name: "Text".to_owned(), field: String::new() }],
        entries,
        damaged_offsets: 0,
        pairs: Vec::new(),
    })
}

/// The languages of a multilingual list, from its element's `… offset`
/// fields, in field order. Read off the element definition, so an empty list
/// has them too.
fn block_languages(root: &TagStruct<'_>) -> Option<Vec<Language>> {
    let block = root.field_path(REFERENCES_BLOCK)?.as_block()?;
    let definition = block.definition().struct_definition();
    let languages: Vec<Language> = definition
        .fields()
        .filter_map(|field| {
            let name = crate::clean_field_name(field.name());
            let language = name.strip_suffix(OFFSET_SUFFIX)?;
            Some(Language { name: language_display_name(language), field: name.to_string() })
        })
        .collect();
    (!languages.is_empty()).then_some(languages)
}

/// `english` → `English`, and the names Foundation gives the rest.
fn language_display_name(language: &str) -> String {
    match language {
        "mexican spanish" => "Spanish (Latin America)".to_owned(),
        "traditional chinese" => "Chinese (Traditional)".to_owned(),
        "simplified chinese" => "Chinese (Simplified)".to_owned(),
        other => {
            let mut name = String::with_capacity(other.len());
            let mut upper = true;
            for c in other.chars() {
                if upper {
                    name.extend(c.to_uppercase());
                } else {
                    name.push(c);
                }
                upper = c == ' ';
            }
            name
        }
    }
}

/// The text at `offset` in the blob, up to its NUL.
fn text_at(data: &[u8], offset: i128) -> Option<String> {
    let start = usize::try_from(offset).ok().filter(|&start| start < data.len())?;
    let end = data[start..].iter().position(|&b| b == 0).map_or(data.len(), |len| start + len);
    Some(decode_tool_utf8(&data[start..end]))
}

/// Read text the tools wrote: each UTF-16 unit as its own UTF-8 sequence, so
/// a character past U+FFFF is two three-byte sequences, not one four-byte
/// one. Ordinary UTF-8 reads the same.
fn decode_tool_utf8(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let mut units: Vec<u16> = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let b = bytes[at];
        let (len, initial) = match b {
            0x00..=0x7F => (1, u32::from(b)),
            0xC0..=0xDF => (2, u32::from(b & 0x1F)),
            0xE0..=0xEF => (3, u32::from(b & 0x0F)),
            0xF0..=0xF7 => (4, u32::from(b & 0x07)),
            _ => (1, 0xFFFD),
        };
        let tail = bytes.get(at + 1..at + len).filter(|tail| tail.iter().all(|&t| t & 0xC0 == 0x80));
        let Some(tail) = tail else {
            units.push(0xFFFD);
            at += 1;
            continue;
        };
        let value = tail.iter().fold(initial, |value, &t| (value << 6) | u32::from(t & 0x3F));
        match char::from_u32(value) {
            Some(c) if len == 4 => units.extend(c.encode_utf16(&mut [0; 2]).iter()),
            _ if value <= 0xFFFF => units.push(value as u16),
            _ => units.push(0xFFFD),
        }
        at += len;
    }
    String::from_utf16_lossy(&units)
}

/// Write text as the tools do: each UTF-16 unit as its own UTF-8 sequence.
fn encode_tool_utf8(text: &str, out: &mut Vec<u8>) {
    for unit in text.encode_utf16() {
        let unit = u32::from(unit);
        match unit {
            0..=0x7F => out.push(unit as u8),
            0x80..=0x7FF => out.extend([0xC0 | (unit >> 6) as u8, 0x80 | (unit & 0x3F) as u8]),
            _ => out.extend([
                0xE0 | (unit >> 12) as u8,
                0x80 | ((unit >> 6) & 0x3F) as u8,
                0x80 | (unit & 0x3F) as u8,
            ]),
        }
    }
}

fn decode_utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn encode_utf16_text(text: &str) -> Vec<u8> {
    text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

/// Write `list` into `tag`: the elements, their ids and offsets, and the
/// text blob rebuilt in the tools' order.
pub fn write_string_list(tag: &mut TagFile, list: &StringList) -> Result<(), String> {
    let mut root = tag.root_mut();
    let blob = {
        let mut field = root
            .field_path_mut(REFERENCES_BLOCK)
            .ok_or("the tag has no string references")?;
        let mut block = field.as_block_mut().ok_or("string references is not a block")?;
        while block.len() > list.entries.len() {
            block.delete_element(block.len() - 1).map_err(|error| error.to_string())?;
        }
        while block.len() < list.entries.len() {
            block.add_element();
        }
        match list.storage {
            Storage::Classic => {
                for (index, entry) in list.entries.iter().enumerate() {
                    let mut element = block.element_mut(index).ok_or("string reference out of range")?;
                    let text = entry.texts.first().cloned().flatten().unwrap_or_default();
                    set(&mut element, CLASSIC_TEXT, TagFieldData::Data(encode_utf16_text(&text)))?;
                }
                None
            }
            Storage::Multilingual => {
                let mut blob = Vec::new();
                for (index, entry) in list.entries.iter().enumerate() {
                    let mut element = block.element_mut(index).ok_or("string reference out of range")?;
                    let id = entry.id.clone().unwrap_or_default();
                    set(&mut element, STRING_ID, TagFieldData::StringId(StringIdData { string: id }))?;
                    for (language, text) in list.languages.iter().zip(&entry.texts) {
                        let offset = match text.as_deref().filter(|text| !text.is_empty()) {
                            Some(text) => {
                                let offset = i32::try_from(blob.len()).map_err(|_| "the text is too large")?;
                                encode_tool_utf8(text, &mut blob);
                                blob.push(0);
                                offset
                            }
                            None => -1,
                        };
                        set(&mut element, &language.field, TagFieldData::LongInteger(offset))?;
                    }
                }
                Some(blob)
            }
        }
    };
    if let Some(blob) = blob {
        root.field_path_mut(TEXT_DATA)
            .ok_or("the tag has no string data")?
            .set(TagFieldData::Data(blob))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn set(element: &mut TagStructMut<'_>, name: &str, value: TagFieldData) -> Result<(), String> {
    element
        .field_path_mut(name)
        .ok_or_else(|| format!("string reference has no {name}"))?
        .set(value)
        .map_err(|error| format!("{name}: {error}"))
}

/// One change to a string list, queued while the editor draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StringListEdit {
    /// Set one language's text; empty removes it.
    SetText { index: usize, language: usize, text: String },
    SetId { index: usize, id: String },
    /// Insert entries at `at`, in order.
    Insert { at: usize, entries: Vec<StringEntry> },
    /// Remove these entries.
    Remove { indices: Vec<usize> },
}

/// Apply `edits` to `tag`'s string list, in order, and write it back once.
/// Returns what changed, for the status line.
pub fn apply_string_list_edits(
    tag: &mut TagFile,
    game: Option<GameId>,
    edits: &[StringListEdit],
) -> Result<String, String> {
    let mut list = read_string_list(tag).ok_or("the tag is not a string list")?;
    let mut summary = String::new();
    for edit in edits {
        summary = edit_list(&mut list, edit)?;
    }
    write_string_list(tag, &list)?;
    if game == Some(GameId::HaloReach) {
        rebuild_substitution_pairs(tag, &list)?;
    }
    Ok(summary)
}

const SUBSTITUTION_PAIRS: &str = "substitution pairs";
const FIRST_STRING_ID: &str = "first string id";
const SECOND_STRING_ID: &str = "second string id";
/// The most pairs a list holds (Reach's block definition).
const MAXIMUM_PAIRS: usize = 2048;

fn read_pairs(root: &TagStruct<'_>) -> Vec<(String, String)> {
    let Some(block) = root.field_path(SUBSTITUTION_PAIRS).and_then(|field| field.as_block()) else {
        return Vec::new();
    };
    block
        .iter()
        .map(|pair| {
            (
                pair.read_string_id(FIRST_STRING_ID).unwrap_or_default(),
                pair.read_string_id(SECOND_STRING_ID).unwrap_or_default(),
            )
        })
        .collect()
}

/// A binding name as Reach's tool stores it: ASCII lowercase, spaces and `-`
/// as `_`, at most 127 characters.
pub fn fold_binding_name(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            ' ' | '-' => '_',
            c => c.to_ascii_lowercase(),
        })
        .take(127)
        .collect()
}

/// The `{cui:component.property}` bindings of `text`, left to right, as
/// Reach's tool finds them (`cui_string_parse.cpp`): the scan stops at the
/// first binding it can't read.
pub fn cui_bindings(text: &str) -> Vec<(String, String)> {
    let ident = |text: &str| {
        let mut chars = text.char_indices();
        match chars.next() {
            Some((_, c)) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return 0,
        }
        chars.find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '_')).map_or(text.len(), |(at, _)| at)
    };
    let mut bindings = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("{cui:") {
        let body = &rest[at + 5..];
        let component = ident(body);
        let Some(after) = body[component..].strip_prefix('.') else { break };
        let property = ident(after);
        let Some(tail) = after[property..].strip_prefix('}') else { break };
        if component == 0 || property == 0 {
            break;
        }
        bindings.push((body[..component].to_owned(), after[..property].to_owned()));
        rest = tail;
    }
    bindings
}

/// Rebuild Reach's `substitution pairs` block as its tool does on import
/// (`import_text.cpp`, 0x14034DAC0): cleared, then every `{cui:…}` binding
/// of every string's every language, in that order, each (component,
/// property) once, at most 2048.
pub fn rebuild_substitution_pairs(tag: &mut TagFile, list: &StringList) -> Result<(), String> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for text in list.entries.iter().flat_map(|entry| entry.texts.iter().flatten()) {
        for (component, property) in cui_bindings(text) {
            let pair = (fold_binding_name(&component), fold_binding_name(&property));
            if pairs.len() < MAXIMUM_PAIRS && !pairs.contains(&pair) {
                pairs.push(pair);
            }
        }
    }
    let mut root = tag.root_mut();
    let Some(mut field) = root.field_path_mut(SUBSTITUTION_PAIRS) else {
        return Ok(());
    };
    let mut block = field.as_block_mut().ok_or("substitution pairs is not a block")?;
    while block.len() > pairs.len() {
        block.delete_element(block.len() - 1).map_err(|error| error.to_string())?;
    }
    while block.len() < pairs.len() {
        block.add_element();
    }
    for (index, (first, second)) in pairs.into_iter().enumerate() {
        let mut element = block.element_mut(index).ok_or("substitution pair out of range")?;
        set(&mut element, FIRST_STRING_ID, TagFieldData::StringId(StringIdData { string: first }))?;
        set(&mut element, SECOND_STRING_ID, TagFieldData::StringId(StringIdData { string: second }))?;
    }
    Ok(())
}

fn edit_list(list: &mut StringList, edit: &StringListEdit) -> Result<String, String> {
    let count = list.entries.len();
    let out_of_range = |index: usize| format!("string {index} is out of range ({count} strings)");
    match edit {
        StringListEdit::SetText { index, language, text } => {
            let entry = list.entries.get_mut(*index).ok_or_else(|| out_of_range(*index))?;
            let slot = entry.texts.get_mut(*language).ok_or("no such language")?;
            *slot = Some(text.clone()).filter(|text| !text.is_empty());
            Ok(format!("Edited string {index}"))
        }
        StringListEdit::SetId { index, id } => {
            if list.storage == Storage::Classic {
                return Err("Halo CE strings have no ids".to_owned());
            }
            let entry = list.entries.get_mut(*index).ok_or_else(|| out_of_range(*index))?;
            entry.id = Some(id.clone());
            Ok(format!("Renamed string {index} to {id}"))
        }
        StringListEdit::Insert { at, entries } => {
            if *at > count {
                return Err(out_of_range(*at));
            }
            let languages = list.languages.len();
            let fitted = entries.iter().cloned().map(|mut entry| {
                entry.texts.resize(languages, None);
                if list.storage == Storage::Classic {
                    entry.id = None;
                }
                entry
            });
            list.entries.splice(*at..*at, fitted);
            Ok(match entries.len() {
                1 => format!("Inserted string {at}"),
                n => format!("Inserted {n} strings at {at}"),
            })
        }
        StringListEdit::Remove { indices } => {
            let mut indices = indices.clone();
            indices.sort_unstable();
            indices.dedup();
            if let Some(&last) = indices.last()
                && last >= count
            {
                return Err(out_of_range(last));
            }
            for &index in indices.iter().rev() {
                list.entries.remove(index);
            }
            Ok(match indices.len() {
                1 => format!("Removed string {}", indices[0]),
                n => format!("Removed {n} strings"),
            })
        }
    }
}
