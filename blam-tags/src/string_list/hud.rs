//! CE HUD messages. All offsets address UTF-16 units, not bytes.
use super::*;

pub const HUD_GROUP: u32 = u32::from_be_bytes(*b"hmt ");
/// Runtime element codes in the engine's order (buttons, actions, substitutions).
// Runtime order cross-checked against Invader src/string/button_type.hpp.
pub const HUD_TOKENS: &[&str] = &[
    "a-button",
    "b-button",
    "x-button",
    "y-button",
    "black-button",
    "white-button",
    "left-trigger",
    "right-trigger",
    "dpad-up",
    "dpad-down",
    "dpad-left",
    "dpad-right",
    "start-button",
    "back-button",
    "left-thumb",
    "right-thumb",
    "left-stick",
    "right-stick",
    "action",
    "throw-grenade",
    "primary-trigger",
    "integrated-light",
    "jump",
    "use-equipment",
    "rotate-weapons",
    "rotate-grenades",
    "zoom",
    "crouch",
    "accept",
    "back",
    "move",
    "look",
    "custom-1",
    "custom-2",
    "custom-3",
    "custom-4",
    "custom-5",
    "custom-6",
    "custom-7",
    "custom-8",
];

pub fn validate_hud_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 31
        || !name.is_ascii()
        || name.trim() != name
        || name.chars().any(char::is_control)
        || name.contains('=')
    {
        return Err("A HUD message name must be 1–31 ASCII bytes, without surrounding whitespace, control characters or '='.".to_owned());
    }
    Ok(())
}

pub(super) fn token_at(text: &str) -> Option<(usize, usize)> {
    if let Some(after) = text.strip_prefix('{') {
        let end = after.find('}')?;
        let code = HUD_TOKENS.iter().position(|name| *name == &after[..end])?;
        return Some((code, end + 2));
    }
    HUD_TOKENS
        .iter()
        .enumerate()
        .filter(|(_, name)| text.starts_with(**name))
        .max_by_key(|(_, name)| name.len())
        .map(|(code, name)| (code, name.len()))
}

pub(super) fn read(tag: &TagFile) -> Result<StringList, String> {
    let root = tag.root();
    let blob = root
        .field_path("text data")
        .and_then(|field| field.as_data())
        .ok_or("Missing HUD text data")?;
    if blob.len() % 2 != 0 || blob.len() > 65536 {
        return Err("Invalid HUD text blob size".into());
    }
    let elements = root
        .field_path("message elements")
        .and_then(|field| field.as_block())
        .ok_or("Missing HUD elements")?;
    let messages = root
        .field_path("messages")
        .and_then(|field| field.as_block())
        .ok_or("Missing HUD messages")?;
    if elements.len() > 8192 || messages.len() > 1024 {
        return Err("HUD block limit exceeded".into());
    }
    let mut entries = Vec::with_capacity(messages.len());
    for message in messages.iter() {
        let name = match message.field_path("name").and_then(|field| field.value()) {
            Some(TagFieldData::String(name)) => name,
            _ => return Err("Missing HUD message name".into()),
        };
        let mut text_at = usize::try_from(
            message
                .read_int_any("start index into text blob")
                .ok_or("Missing text offset")?,
        )
        .map_err(|_| "Negative HUD text offset")?;
        let first = usize::try_from(
            message
                .read_int_any("start index of message block")
                .ok_or("Missing element offset")?,
        )
        .map_err(|_| "Negative HUD element offset")?;
        // The schema calls these char_integer, but the game consumes unsigned bytes.
        let count = message
            .read_int_any("panel count")
            .ok_or("Missing panel count")? as u8 as usize;
        if first + count > elements.len() || text_at > blob.len() / 2 {
            return Err("HUD message span is out of range".into());
        }
        let mut text = String::new();
        let mut runtime_spans = Vec::new();
        for index in first..first + count {
            let element = elements.element(index).ok_or("Missing HUD element")?;
            let kind = element
                .read_int_any("type")
                .ok_or("Missing HUD element type")?;
            let data = element
                .read_int_any("data")
                .ok_or("Missing HUD element data")? as u8;
            match kind {
                0 => {
                    if data == 0 {
                        return Err("Zero-length HUD text element".into());
                    }
                    let end = text_at + usize::from(data);
                    let bytes = blob
                        .get(text_at * 2..end * 2)
                        .ok_or("HUD text span is out of range")?;
                    let units: Vec<_> = bytes
                        .chunks_exact(2)
                        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                        .collect();
                    if units.last() != Some(&0) || units[..units.len() - 1].contains(&0) {
                        return Err("Invalid HUD text terminator".into());
                    }
                    // A doubled percent preserves literal text separately from runtime elements.
                    text.push_str(
                        &String::from_utf16(&units[..units.len() - 1])
                            .map_err(|_| "Invalid HUD UTF-16")?
                            .replace('%', "%%"),
                    );
                    text_at = end;
                }
                1 => {
                    let start = text.len();
                    text.push('%');
                    text.push_str(
                        HUD_TOKENS
                            .get(data as usize)
                            .ok_or("Unknown HUD runtime token")?,
                    );
                    runtime_spans.push((start, text.len(), data as usize));
                }
                _ => return Err("Unknown HUD element type".into()),
            }
        }
        // Keep a short token followed by literal text from becoming another token:
        // runtime %back + literal -button is distinct from runtime %back-button.
        for (start, end, code) in runtime_spans.into_iter().rev() {
            if token_at(&text[start + 1..]).is_some_and(|(found, _)| found != code) {
                text.replace_range(start..end, &format!("%{{{}}}", HUD_TOKENS[code]));
            }
        }
        entries.push(StringEntry {
            id: Some(name),
            texts: vec![(!text.is_empty()).then_some(text)],
        });
    }
    Ok(StringList {
        storage: Storage::HudMessages,
        languages: vec![Language {
            name: "Text".into(),
            field: String::new(),
        }],
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

pub(super) fn write(tag: &mut TagFile, list: &StringList) -> Result<(), String> {
    if tag.group().tag != HUD_GROUP {
        return Err("HUD messages require a hud_message_text tag".into());
    }
    if list.entries.len() > 1024 {
        return Err("HUD messages are limited to 1024 entries".into());
    }
    // An unchanged list must retain padding, unused storage and empty runs byte-exactly.
    if read(tag).as_ref().ok() == Some(list) {
        return Ok(());
    }
    let mut blob = Vec::<u16>::new();
    let mut elements = Vec::<(u8, u8)>::new();
    let mut messages = Vec::new();
    // Complete validation/encoding before touching the tag.
    for entry in &list.entries {
        let name = entry.id.as_deref().unwrap_or_default();
        validate_hud_name(name)?;
        let start_text =
            i16::try_from(blob.len()).map_err(|_| "HUD text offset exceeds 32767 UTF-16 units")?;
        let first = elements.len();
        let text = entry
            .texts
            .first()
            .and_then(Option::as_deref)
            .unwrap_or_default();
        if text.contains('\0') {
            return Err("HUD text cannot contain NUL".into());
        }
        let mut literal = String::new();
        let flush = |literal: &mut String, blob: &mut Vec<u16>, elements: &mut Vec<(u8, u8)>| {
            // Split on character boundaries, never in the middle of a surrogate pair.
            let mut units = Vec::new();
            for c in literal.chars() {
                if units.len() + c.len_utf16() > 254 {
                    elements.push((0, (units.len() + 1) as u8));
                    blob.append(&mut units);
                    blob.push(0);
                }
                let mut buffer = [0; 2];
                units.extend_from_slice(c.encode_utf16(&mut buffer));
            }
            if !units.is_empty() {
                elements.push((0, (units.len() + 1) as u8));
                blob.append(&mut units);
                blob.push(0);
            }
            literal.clear();
        };
        let mut at = 0;
        while at < text.len() {
            let rest = &text[at..];
            if let Some(after) = rest.strip_prefix('%') {
                if after.starts_with('%') {
                    literal.push('%');
                    at += 2;
                    continue;
                }
                let (code, len) = token_at(after)
                    .ok_or_else(|| format!("Unknown HUD token in {name}: {rest}"))?;
                flush(&mut literal, &mut blob, &mut elements);
                elements.push((1, code as u8));
                at += len + 1;
            } else {
                let c = rest.chars().next().unwrap();
                literal.push(c);
                at += c.len_utf8();
            }
        }
        flush(&mut literal, &mut blob, &mut elements);
        let count = u8::try_from(elements.len() - first)
            .map_err(|_| format!("HUD message {name} exceeds 255 elements"))?;
        if elements.len() > 8192 || blob.len() * 2 > 65536 {
            return Err("HUD message elements or text blob exceed the tag limits".into());
        }
        messages.push(EncodedMessage {
            name: name.into(),
            text: start_text,
            first: first as i16,
            count,
        });
    }
    let mut root = tag.root_mut();
    {
        let mut field = root
            .field_path_mut("messages")
            .ok_or("Missing HUD messages")?;
        let mut block = field.as_block_mut().ok_or("Invalid HUD messages")?;
        while block.len() > messages.len() {
            block
                .delete_element(block.len() - 1)
                .map_err(|e| e.to_string())?;
        }
        while block.len() < messages.len() {
            block.add_element();
        }
        for (index, message) in messages.iter().enumerate() {
            let mut item = block.element_mut(index).ok_or("Missing message")?;
            set(
                &mut item,
                "name",
                TagFieldData::String(message.name.clone()),
            )?;
            set(
                &mut item,
                "start index into text blob",
                TagFieldData::ShortInteger(message.text),
            )?;
            set(
                &mut item,
                "start index of message block",
                TagFieldData::ShortInteger(message.first),
            )?;
            set(
                &mut item,
                "panel count",
                TagFieldData::CharInteger(message.count as i8),
            )?;
        }
    }
    {
        let mut field = root
            .field_path_mut("message elements")
            .ok_or("Missing HUD elements")?;
        let mut block = field.as_block_mut().ok_or("Invalid HUD elements")?;
        while block.len() > elements.len() {
            block
                .delete_element(block.len() - 1)
                .map_err(|e| e.to_string())?;
        }
        while block.len() < elements.len() {
            block.add_element();
        }
        for (index, (kind, data)) in elements.iter().enumerate() {
            let mut item = block.element_mut(index).ok_or("Missing element")?;
            set(&mut item, "type", TagFieldData::CharInteger(*kind as i8))?;
            set(&mut item, "data", TagFieldData::CharInteger(*data as i8))?;
        }
    }
    root.field_path_mut("text data")
        .ok_or("Missing HUD text data")?
        .set(TagFieldData::Data(
            blob.into_iter().flat_map(u16::to_le_bytes).collect(),
        ))
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Native HMT is UTF-16 source containing one name=text record per line.
pub fn parse_hud_messages_file(contents: &str) -> Result<Vec<(String, String)>, String> {
    let mut seen = std::collections::BTreeSet::new();
    contents
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| !line.is_empty())
        .enumerate()
        .map(|(index, line)| {
            let (name, text) = line
                .split_once('=')
                .ok_or_else(|| format!("HMT line {} needs name=text", index + 1))?;
            validate_hud_name(name)?;
            if text.contains("%%") || text.contains("%{") {
                return Err(format!(
                    "Editor-only percent notation in native HMT message {name}; use CSV"
                ));
            }
            if !seen.insert(name) {
                return Err(format!("Duplicate HUD message name: {name}"));
            }
            // Native HMT has no editor escapes; percent is reserved for runtime tokens.
            parse_shown(
                &format_shown(text, &TextSyntax::plain()),
                &TextSyntax::plain().with_hud_messages(true),
            )
            .map_err(|_| format!("Invalid HUD token in {name}"))?;
            Ok((name.to_owned(), text.to_owned()))
        })
        .collect()
}

pub fn hud_messages_file(entries: &[StringEntry]) -> Result<String, String> {
    let mut out = String::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in entries {
        let name = entry.id.as_deref().unwrap_or_default();
        validate_hud_name(name)?;
        if !seen.insert(name) {
            return Err(format!("Duplicate HUD message name: {name}"));
        }
        let text = entry
            .texts
            .first()
            .and_then(Option::as_deref)
            .unwrap_or_default();
        if text.contains(['\r', '\n', '\0']) || text.contains("%%") || text.contains("%{") {
            return Err(format!(
                "HUD message {name} cannot be represented losslessly in native HMT; use CSV"
            ));
        }
        if parse_shown(
            &format_shown(text, &TextSyntax::plain()),
            &TextSyntax::plain().with_hud_messages(true),
        )
        .is_err()
        {
            return Err(format!("Invalid HUD token in {name}"));
        }
        out.push_str(name);
        out.push('=');
        out.push_str(text);
        out.push_str("\r\n");
    }
    Ok(out)
}
