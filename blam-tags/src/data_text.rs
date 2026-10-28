//! Data fields that hold text: what a game's editor shows and edits as text.
//!
//! A `data` field's definition (`tag_data_definition`) carries flags, and
//! bit 1 marks its bytes as text: HaloScript source, shader includes,
//! import logs, help text. Foundation shows those in a text box; ManagedBlam
//! (`TagFieldData.DataAsText`) reads the bytes up to the first NUL as ANSI
//! text, no further than the definition's maximum size, and writes text back
//! as ANSI with a NUL after it.
//!
//! A tag's own layout names a data field's definition but keeps none of it,
//! so the flags and maximum size come from the definitions:
//! `definitions/<game>/<group>.json`'s `datas`. A name is only a group's:
//! two groups can give one name different definitions (Halo 3's
//! `code_block` holds 262140 bytes in a shader include, 131070 in a render
//! method option), so a definition is looked up from the tag's group up
//! through its parent groups, whose fields a tag of the group also has.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::api::TagField;
use crate::fields::parse_group_tag;

/// `tag_data_definition` flag: the bytes are text.
const DATA_IS_TEXT: u32 = 1 << 1;

/// What the definitions say about one data definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataDefinitionInfo {
    pub flags: u32,
    /// The most bytes the field may hold.
    pub max_size: u64,
}

impl DataDefinitionInfo {
    /// Whether the editors show and edit these bytes as text.
    pub fn is_text(self) -> bool {
        self.flags & DATA_IS_TEXT != 0
    }
}

/// One game's data definitions, by group and name.
#[derive(Debug, Default, Clone)]
pub struct DataDefinitions {
    groups: HashMap<u32, GroupDefinitions>,
}

/// One group's own data definitions, and its parent group.
#[derive(Debug, Default, Clone)]
struct GroupDefinitions {
    parent: Option<u32>,
    by_name: HashMap<String, DataDefinitionInfo>,
}

#[derive(Debug)]
pub enum DataDefinitionsError {
    Io(std::io::Error),
    Json(PathBuf, serde_json::Error),
}

impl std::fmt::Display for DataDefinitionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for DataDefinitionsError {}

impl From<std::io::Error> for DataDefinitionsError {
    fn from(error: std::io::Error) -> Self { Self::Io(error) }
}

#[derive(Deserialize)]
struct GroupDatas {
    tag: String,
    #[serde(default)]
    parent_tag: Option<String>,
    #[serde(default)]
    datas: HashMap<String, DataJson>,
}

#[derive(Deserialize)]
struct DataJson {
    #[serde(default)]
    flags: u32,
    #[serde(default)]
    max_size: u64,
}

impl DataDefinitions {
    /// Load one game's data definitions from its definitions folder
    /// (`definitions/halo3_mcc`).
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, DataDefinitionsError> {
        let mut definitions = Self::default();
        for entry in std::fs::read_dir(game_dir.as_ref())? {
            let path = entry?.path();
            let is_group = path.extension().is_some_and(|ext| ext == "json")
                && !path.file_name().is_some_and(|name| name.to_string_lossy().starts_with('_'));
            if !is_group {
                continue;
            }
            let group: GroupDatas = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|error| DataDefinitionsError::Json(path.clone(), error))?;
            let Some(group_tag) = parse_group_tag(&group.tag) else {
                continue;
            };
            let by_name = group
                .datas
                .into_iter()
                .map(|(name, data)| (name, DataDefinitionInfo { flags: data.flags, max_size: data.max_size }))
                .collect();
            let parent = group.parent_tag.as_deref().and_then(parse_group_tag);
            definitions.groups.insert(group_tag, GroupDefinitions { parent, by_name });
        }
        Ok(definitions)
    }

    /// The data definition named `name` in a tag of group `group_tag`: the
    /// group's own, or failing that its nearest parent group's.
    pub fn get(&self, group_tag: u32, name: &str) -> Option<DataDefinitionInfo> {
        let mut group = Some(group_tag);
        // A parent chain is a few groups deep (weapon → item → object); the
        // bound keeps a cycle in hand-edited definitions from hanging.
        for _ in 0..8 {
            let definitions = self.groups.get(&group?)?;
            if let Some(info) = definitions.by_name.get(name) {
                return Some(*info);
            }
            group = definitions.parent;
        }
        None
    }

    /// The definition of a `data` field of a tag of group `group_tag`;
    /// `None` for any other field, or one whose definition the game doesn't
    /// have.
    pub fn of_field(&self, group_tag: u32, field: &TagField<'_>) -> Option<DataDefinitionInfo> {
        self.get(group_tag, field.data_definition_name()?)
    }
}

/// A text data field's bytes as the editors show them: up to the first NUL,
/// no further than the definition's maximum size, read as Windows-1252.
pub fn text_from_data(bytes: &[u8], definition: DataDefinitionInfo) -> String {
    let limit = usize::try_from(definition.max_size).unwrap_or(usize::MAX).min(bytes.len());
    let bytes = &bytes[..limit];
    let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
    bytes[..end].iter().map(|&byte| windows_1252_char(byte)).collect()
}

/// Text too long for its data field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataTextTooLong {
    /// The bytes the text needs, its NUL included.
    pub len: usize,
    pub max_size: u64,
}

impl std::fmt::Display for DataTextTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the text needs {} bytes; the field holds at most {}", self.len, self.max_size)
    }
}

impl std::error::Error for DataTextTooLong {}

/// The bytes a text data field stores for `text`: Windows-1252, with a NUL
/// after it. A character the code page has no byte for is written as `?`.
pub fn data_from_text(text: &str, definition: DataDefinitionInfo) -> Result<Vec<u8>, DataTextTooLong> {
    let mut bytes: Vec<u8> = text.chars().map(windows_1252_byte).collect();
    bytes.push(0);
    if bytes.len() as u64 > definition.max_size {
        return Err(DataTextTooLong { len: bytes.len(), max_size: definition.max_size });
    }
    Ok(bytes)
}

/// Windows-1252's characters for 0x80..=0x9F; the rest of the code page is
/// Latin-1. The five bytes it leaves undefined read as themselves.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}', '\u{017D}', '\u{8F}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{9D}', '\u{017E}', '\u{0178}',
];

fn windows_1252_char(byte: u8) -> char {
    match byte {
        0x80..=0x9F => WINDOWS_1252_HIGH[usize::from(byte - 0x80)],
        _ => char::from(byte),
    }
}

fn windows_1252_byte(c: char) -> u8 {
    match u32::from(c) {
        code @ (0..=0x7F | 0xA0..=0xFF) => code as u8,
        _ => WINDOWS_1252_HIGH
            .iter()
            .position(|&high| high == c)
            .map_or(b'?', |index| 0x80 + index as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definitions(game: &str) -> DataDefinitions {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../definitions").join(game);
        DataDefinitions::load(root).unwrap()
    }

    /// The text definitions are the ones the editors show as text, in a gen3
    /// game, Halo CE and Halo 2; function data, vertex buffers, sound samples
    /// and the like aren't.
    #[test]
    fn text_comes_from_the_definitions_flags() {
        let halo3 = definitions("halo3_mcc");
        let include = halo3.get(group(b"hlsl"), "code_block").unwrap();
        assert!(include.is_text());
        assert_eq!(include.max_size, 262_140);
        for (tag, name) in [(b"scnr", "hs_source_data_definition"), (b"hlmt", "parameters_text_definition"), (b"info", "event_text_data_definition")] {
            assert!(halo3.get(group(tag), name).unwrap().is_text(), "{name}");
        }
        for (tag, name) in [(b"obje", "function_definition_data"), (b"mode", "render_geometry_vertex_buffer_data"), (b"snd!", "sound_samples"), (b"unic", "utf8_string_data")] {
            assert!(!halo3.get(group(tag), name).unwrap().is_text(), "{name}");
        }
        let halo1 = definitions("haloce_mcc");
        assert!(halo1.get(group(b"scnr"), "hs_source_data_definition").unwrap().is_text());
        let halo2 = definitions("halo2_mcc");
        assert!(halo2.get(group(b"scnr"), "hs_source_data_definition").unwrap().is_text());
        assert!(halo2.get(group(b"spas"), "shader_text_definition").unwrap().is_text());
        assert!(!halo2.get(group(b"snd!"), "sound_samples").unwrap().is_text());
    }

    fn group(tag: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*tag)
    }

    /// Groups that give one name different definitions each read their own,
    /// whichever order the files are found in, and a tag reads its parent
    /// groups' definitions too.
    #[test]
    fn a_definition_is_its_groups_or_a_parent_groups() {
        let halo3 = definitions("halo3_mcc");
        assert_eq!(halo3.get(group(b"hlsl"), "code_block").unwrap().max_size, 262_140);
        assert_eq!(halo3.get(group(b"rmop"), "code_block").unwrap().max_size, 131_070);
        assert_eq!(halo3.get(group(b"weap"), "function_definition_data"), halo3.get(group(b"obje"), "function_definition_data"));
        assert!(halo3.get(group(b"weap"), "function_definition_data").is_some(), "weapon → item → object");
        assert_eq!(halo3.get(group(b"obje"), "code_block"), None);
        let halo1 = definitions("haloce_mcc");
        let hud = halo1.get(group(b"hmt "), "string data").unwrap();
        let strings = halo1.get(group(b"ustr"), "string data").unwrap();
        assert_ne!(hud, strings);
        assert_eq!(hud.max_size, 65_536);
    }

    const TEXT: DataDefinitionInfo = DataDefinitionInfo { flags: DATA_IS_TEXT, max_size: 16 };

    #[test]
    fn text_is_read_to_its_nul_and_its_maximum_size() {
        assert_eq!(text_from_data(b"a\r\nb\0junk", TEXT), "a\r\nb");
        assert_eq!(text_from_data(b"no nul here", TEXT), "no nul here");
        assert_eq!(text_from_data(b"0123456789abcdefOVER", TEXT), "0123456789abcdef");
        assert_eq!(text_from_data(b"\x93quoted\x94 \xe9", TEXT), "\u{201C}quoted\u{201D} \u{e9}");
    }

    #[test]
    fn text_is_written_with_a_nul_within_its_maximum_size() {
        assert_eq!(data_from_text("a\r\nb", TEXT).unwrap(), b"a\r\nb\0");
        assert_eq!(data_from_text("\u{201C}\u{e9}\u{4e2d}", TEXT).unwrap(), b"\x93\xe9?\0");
        assert_eq!(
            data_from_text("0123456789abcdef", TEXT),
            Err(DataTextTooLong { len: 17, max_size: 16 }),
            "16 characters and a NUL don't fit in 16 bytes"
        );
        let all: String = (0..=255u8).filter(|&b| b != 0).map(windows_1252_char).collect();
        let wide = DataDefinitionInfo { max_size: 1024, ..TEXT };
        assert_eq!(text_from_data(&data_from_text(&all, wide).unwrap(), wide), all);
    }

    /// Every Halo 3 shader include reads as text and writes back the bytes it
    /// was read from.
    #[test]
    fn halo3_shader_includes_round_trip_as_text() {
        let Some(root) = std::env::var_os("BLAM_TEST_H3EK").map(PathBuf::from) else {
            eprintln!("skipping: BLAM_TEST_H3EK not set");
            return;
        };
        let halo3 = definitions("halo3_mcc");
        let mut stack = vec![root.join("tags")];
        let (mut read, mut same) = (0, 0);
        let mut different = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "hlsl_include") {
                    continue;
                }
                let tag = crate::TagFile::read(&path).unwrap();
                let field = tag.root().fields().find(|field| field.as_data().is_some()).unwrap();
                let definition = halo3.of_field(tag.group().tag, &field).unwrap();
                assert!(definition.is_text());
                let bytes = field.as_data().unwrap();
                read += 1;
                if data_from_text(&text_from_data(bytes, definition), definition).as_deref() == Ok(bytes) {
                    same += 1;
                } else {
                    different.push(path);
                }
            }
        }
        assert!(read > 0, "no shader includes under {}", root.display());
        assert_eq!(same, read, "{} of {read} differ, e.g. {:?}", different.len(), different.first());
    }
}
