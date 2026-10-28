//! Halo CE HUD messages (`hud_message_text`): named messages, each a run of
//! elements that are either UTF-16 text or a runtime element (a button, an
//! action or a custom substitution) the game draws in its place. All offsets
//! address UTF-16 units, not bytes.
//!
//! A message's text holds a runtime element by its code, as `%{18}`, and a
//! literal percent as `%%`, so it reads and writes without knowing the
//! element names. The names are the game's (`_meta.json`'s
//! `string_text.hud_tokens`, in code order): a [`TextSyntax`] with HUD
//! messages on shows and reads `%{18}` as `%action`, as glyph names show
//! private-use characters.
use super::*;

pub const HUD_GROUP: u32 = u32::from_be_bytes(*b"hmt ");

const TEXT_BLOB: &str = "text data";
const ELEMENTS_BLOCK: &str = "message elements";
pub(super) const MESSAGES_BLOCK: &str = "messages";
const NAME: &str = "name";
const TEXT_START: &str = "start index into text blob";
const FIRST_ELEMENT: &str = "start index of message block";
const ELEMENT_COUNT: &str = "panel count";
const ELEMENT_TYPE: &str = "type";
const ELEMENT_DATA: &str = "data";

/// `type` of an element of UTF-16 text, whose `data` is its length in
/// units, NUL included.
const TEXT_ELEMENT: u8 = 0;
/// `type` of a runtime element, whose `data` is its code.
const RUNTIME_ELEMENT: u8 = 1;

/// The most bytes `text data` holds (its `string data` definition).
const MAX_TEXT_BYTES: usize = 65536;
/// A name's longest: the 32-byte `string` field, NUL included. tool.exe's
/// import cuts names there too.
const MAX_NAME_LEN: usize = 31;
/// The most UTF-16 units of one text element, NUL included: its length is a
/// byte.
const MAX_RUN_UNITS: usize = 255;
/// The most elements of one message: `panel count` is a byte.
const MAX_MESSAGE_ELEMENTS: usize = 255;

/// One piece of a message's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Piece {
    Text(char),
    Element(u8),
}

/// How a message's text holds runtime element `code`.
pub(super) fn element_text(code: u8) -> String {
    format!("%{{{code}}}")
}

/// A `{code}` element at the start of `text` (what follows a `%`), and its
/// length.
pub(super) fn coded_element(text: &str) -> Option<(u8, usize)> {
    let inner = text.strip_prefix('{')?;
    let end = inner.find('}')?;
    let digits = &inner[..end];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, end + 2))
}

/// A message's text as pieces: `%%` is a literal percent, `%{code}` a
/// runtime element, and any other `%` an error.
pub(super) fn pieces(text: &str) -> Result<Vec<Piece>, String> {
    let mut pieces = Vec::with_capacity(text.len());
    let mut at = 0;
    while let Some(c) = text[at..].chars().next() {
        if c != '%' {
            pieces.push(Piece::Text(c));
            at += c.len_utf8();
            continue;
        }
        let after = &text[at + 1..];
        if after.starts_with('%') {
            pieces.push(Piece::Text('%'));
            at += 2;
        } else if let Some((code, len)) = coded_element(after) {
            pieces.push(Piece::Element(code));
            at += 1 + len;
        } else {
            return Err(format!("'%' at byte {at} is neither %% nor a %{{code}} element"));
        }
    }
    Ok(pieces)
}

pub fn validate_hud_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > MAX_NAME_LEN
        || !name.is_ascii()
        || name.trim() != name
        || name.chars().any(char::is_control)
        || name.contains('=')
    {
        return Err(format!(
            "A HUD message name must be 1–{MAX_NAME_LEN} ASCII bytes, without surrounding whitespace, control characters or '='."
        ));
    }
    Ok(())
}

/// The block at `path` of `root`, and the most elements its definition
/// holds.
fn block_of<'a>(root: &TagStruct<'a>, path: &str) -> Result<(crate::TagBlock<'a>, usize), String> {
    let block = root.field_path(path).and_then(|field| field.as_block()).ok_or_else(|| format!("Missing HUD {path}"))?;
    let max = block.definition().max_count() as usize;
    Ok((block, max))
}

pub(super) fn read(tag: &TagFile) -> Result<StringList, String> {
    let root = tag.root();
    let blob = root.field_path(TEXT_BLOB).and_then(|field| field.as_data()).ok_or("Missing HUD text data")?;
    if blob.len() % 2 != 0 || blob.len() > MAX_TEXT_BYTES {
        return Err("Invalid HUD text blob size".into());
    }
    let (elements, max_elements) = block_of(&root, ELEMENTS_BLOCK)?;
    let (messages, max_messages) = block_of(&root, MESSAGES_BLOCK)?;
    if elements.len() > max_elements || messages.len() > max_messages {
        return Err("HUD block limit exceeded".into());
    }
    let mut entries = Vec::with_capacity(messages.len());
    for message in messages.iter() {
        let name = match message.field_path(NAME).and_then(|field| field.value()) {
            Some(TagFieldData::String(name)) => name,
            _ => return Err("Missing HUD message name".into()),
        };
        let mut text_at = usize::try_from(message.read_int_any(TEXT_START).ok_or("Missing text offset")?)
            .map_err(|_| "Negative HUD text offset")?;
        let first = usize::try_from(message.read_int_any(FIRST_ELEMENT).ok_or("Missing element offset")?)
            .map_err(|_| "Negative HUD element offset")?;
        // The schema calls these char_integer, but the game reads unsigned
        // bytes.
        let count = message.read_int_any(ELEMENT_COUNT).ok_or("Missing panel count")? as u8 as usize;
        if first + count > elements.len() || text_at > blob.len() / 2 {
            return Err("HUD message span is out of range".into());
        }
        let mut text = String::new();
        for index in first..first + count {
            let element = elements.element(index).ok_or("Missing HUD element")?;
            let kind = element.read_int_any(ELEMENT_TYPE).ok_or("Missing HUD element type")?;
            let data = element.read_int_any(ELEMENT_DATA).ok_or("Missing HUD element data")? as u8;
            match u8::try_from(kind) {
                Ok(TEXT_ELEMENT) => {
                    if data == 0 {
                        return Err("Zero-length HUD text element".into());
                    }
                    let end = text_at + usize::from(data);
                    let bytes = blob.get(text_at * 2..end * 2).ok_or("HUD text span is out of range")?;
                    let units: Vec<_> =
                        bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
                    if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
                        return Err("Invalid HUD text terminator".into());
                    }
                    let run = String::from_utf16(&units[..units.len() - 1]).map_err(|_| "Invalid HUD UTF-16")?;
                    text.push_str(&run.replace('%', "%%"));
                    text_at = end;
                }
                Ok(RUNTIME_ELEMENT) => text.push_str(&element_text(data)),
                _ => return Err("Unknown HUD element type".into()),
            }
        }
        entries.push(StringEntry { id: Some(name), texts: vec![(!text.is_empty()).then_some(text)] });
    }
    Ok(StringList {
        storage: Storage::HudMessages,
        languages: vec![Language { name: "Text".into(), field: String::new() }],
        entries,
        damaged_offsets: 0,
        pairs: Vec::new(),
    })
}

struct EncodedMessage {
    name: String,
    text: i16,
    first: i16,
    count: u8,
}

/// Push `literal` as text elements of at most [`MAX_RUN_UNITS`], never in
/// the middle of a surrogate pair.
fn push_runs(literal: &str, blob: &mut Vec<u16>, elements: &mut Vec<(u8, u8)>) {
    let mut units = Vec::new();
    let mut flush = |units: &mut Vec<u16>| {
        elements.push((TEXT_ELEMENT, (units.len() + 1) as u8));
        blob.append(units);
        blob.push(0);
    };
    for c in literal.chars() {
        if units.len() + c.len_utf16() >= MAX_RUN_UNITS {
            flush(&mut units);
        }
        let mut buffer = [0; 2];
        units.extend_from_slice(c.encode_utf16(&mut buffer));
    }
    if !units.is_empty() {
        flush(&mut units);
    }
}

pub(super) fn write(tag: &mut TagFile, list: &StringList) -> Result<(), String> {
    if tag.group().tag != HUD_GROUP {
        return Err("HUD messages require a hud_message_text tag".into());
    }
    let (max_elements, max_messages) = {
        let root = tag.root();
        (block_of(&root, ELEMENTS_BLOCK)?.1, block_of(&root, MESSAGES_BLOCK)?.1)
    };
    if list.entries.len() > max_messages {
        return Err(format!("HUD messages are limited to {max_messages} entries"));
    }
    // An unchanged list must retain padding, unused storage and empty runs
    // byte-exactly.
    if read(tag).as_ref().ok() == Some(list) {
        return Ok(());
    }
    let mut blob = Vec::<u16>::new();
    let mut elements = Vec::<(u8, u8)>::new();
    let mut messages = Vec::new();
    // Complete validation and encoding before touching the tag.
    for entry in &list.entries {
        let name = entry.id.as_deref().unwrap_or_default();
        validate_hud_name(name)?;
        let start_text = i16::try_from(blob.len()).map_err(|_| "HUD text offset exceeds 32767 UTF-16 units")?;
        let first = elements.len();
        let text = entry.texts.first().and_then(Option::as_deref).unwrap_or_default();
        if text.contains('\0') {
            return Err("HUD text cannot contain NUL".into());
        }
        let mut literal = String::new();
        for piece in pieces(text).map_err(|error| format!("HUD message {name}: {error}"))? {
            match piece {
                Piece::Text(c) => literal.push(c),
                Piece::Element(code) => {
                    push_runs(&literal, &mut blob, &mut elements);
                    literal.clear();
                    elements.push((RUNTIME_ELEMENT, code));
                }
            }
        }
        push_runs(&literal, &mut blob, &mut elements);
        let count = elements.len() - first;
        if count > MAX_MESSAGE_ELEMENTS {
            return Err(format!("HUD message {name} exceeds {MAX_MESSAGE_ELEMENTS} elements"));
        }
        if elements.len() > max_elements || blob.len() * 2 > MAX_TEXT_BYTES {
            return Err("HUD message elements or text blob exceed the tag limits".into());
        }
        messages.push(EncodedMessage { name: name.into(), text: start_text, first: first as i16, count: count as u8 });
    }
    let mut root = tag.root_mut();
    {
        let mut field = root.field_path_mut(MESSAGES_BLOCK).ok_or("Missing HUD messages")?;
        let mut block = field.as_block_mut().ok_or("Invalid HUD messages")?;
        while block.len() > messages.len() {
            block.delete_element(block.len() - 1).map_err(|e| e.to_string())?;
        }
        while block.len() < messages.len() {
            block.add_element();
        }
        for (index, message) in messages.iter().enumerate() {
            let mut item = block.element_mut(index).ok_or("Missing message")?;
            set(&mut item, NAME, TagFieldData::String(message.name.clone()))?;
            set(&mut item, TEXT_START, TagFieldData::ShortInteger(message.text))?;
            set(&mut item, FIRST_ELEMENT, TagFieldData::ShortInteger(message.first))?;
            set(&mut item, ELEMENT_COUNT, TagFieldData::CharInteger(message.count as i8))?;
        }
    }
    {
        let mut field = root.field_path_mut(ELEMENTS_BLOCK).ok_or("Missing HUD elements")?;
        let mut block = field.as_block_mut().ok_or("Invalid HUD elements")?;
        while block.len() > elements.len() {
            block.delete_element(block.len() - 1).map_err(|e| e.to_string())?;
        }
        while block.len() < elements.len() {
            block.add_element();
        }
        for (index, (kind, data)) in elements.iter().enumerate() {
            let mut item = block.element_mut(index).ok_or("Missing element")?;
            set(&mut item, ELEMENT_TYPE, TagFieldData::CharInteger(*kind as i8))?;
            set(&mut item, ELEMENT_DATA, TagFieldData::CharInteger(*data as i8))?;
        }
    }
    root.field_path_mut(TEXT_BLOB)
        .ok_or("Missing HUD text data")?
        .set(TagFieldData::Data(blob.into_iter().flat_map(u16::to_le_bytes).collect()))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Native HMT source, `name=text` lines, as tool.exe's hud-messages import
/// reads it: `%name` is a runtime element by `syntax`'s names, matched as
/// the tool matches them, and there is no way to write a literal percent.
/// Each message's text comes back as [`read_string_list`] holds it. The
/// caller decodes the file's UTF-16.
pub fn parse_hud_messages_file(contents: &str, syntax: &TextSyntax) -> Result<Vec<(String, String)>, String> {
    if syntax.hud_tokens().is_empty() {
        return Err("This game has no HUD element names".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for (index, line) in contents.trim_start_matches('\u{feff}').lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let (name, text) = line.split_once('=').ok_or_else(|| format!("HMT line {} needs name=text", index + 1))?;
        validate_hud_name(name)?;
        if !seen.insert(name) {
            return Err(format!("Duplicate HUD message name: {name}"));
        }
        let mut stored = String::with_capacity(text.len());
        let mut at = 0;
        while let Some(c) = text[at..].chars().next() {
            if c != '%' {
                stored.push(c);
                at += c.len_utf8();
                continue;
            }
            let (code, len) =
                syntax.match_hud_token(&text[at + 1..]).ok_or_else(|| format!("Invalid HUD token in {name}"))?;
            stored.push_str(&element_text(code));
            at += 1 + len;
        }
        rows.push((name.to_owned(), stored));
    }
    Ok(rows)
}

/// `entries` as native HMT source (see [`parse_hud_messages_file`]), or an
/// error for a message the source can't hold: a literal percent, a line
/// break, an element `syntax` has no name for, or one whose name the text
/// after it would make read as another.
pub fn hud_messages_file(entries: &[StringEntry], syntax: &TextSyntax) -> Result<String, String> {
    let mut out = String::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let name = entry.id.as_deref().unwrap_or_default();
        validate_hud_name(name)?;
        if !seen.insert(name) {
            return Err(format!("Duplicate HUD message name: {name}"));
        }
        let text = entry.texts.first().and_then(Option::as_deref).unwrap_or_default();
        let lossy = || format!("HUD message {name} cannot be represented losslessly in native HMT; use CSV");
        // Built back to front: whether a name reads back depends on what
        // follows it.
        let mut suffix = String::new();
        for piece in pieces(text).map_err(|error| format!("HUD message {name}: {error}"))?.into_iter().rev() {
            let shown = match piece {
                Piece::Text('%' | '\r' | '\n' | '\0') => return Err(lossy()),
                Piece::Text(c) => c.to_string(),
                Piece::Element(code) => {
                    let token = syntax.hud_tokens().get(usize::from(code)).ok_or_else(lossy)?;
                    if syntax.match_hud_token(&format!("{token}{suffix}")) != Some((code, token.len())) {
                        return Err(lossy());
                    }
                    format!("%{token}")
                }
            };
            suffix.insert_str(0, &shown);
        }
        out.push_str(name);
        out.push('=');
        out.push_str(&suffix);
        out.push_str("\r\n");
    }
    Ok(out)
}
