//! What the games read in a string when they draw it: `|` escapes, XML-style
//! tags and entities, `{cui:…}` bindings and multiplayer `#keyword`s. None of
//! it is the tool's business, so none of it can make an import fail; at worst
//! the game shows it as written, or (for a tag it knows but can't read) drops
//! or fails the string. The scanner marks each piece, and says when the game
//! will not read it the way it looks.
//!
//! What each game reads is data: `_meta.json`'s `string_text` (see
//! [`super::TextSyntax::load`]). What each escape, tag and entity means is
//! the same in every game that has it, and is here.

use std::ops::Range;

use serde::Deserialize;

use super::syntax::Choice;

/// Which `|` escapes a game's renderer reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Pipes {
    #[default]
    None,
    /// Halo CE: `|n` only.
    LineBreak,
    /// Halo 2: `||`, `|n`, `|t`, `|l`, `|r`, `|c`.
    Halo2,
    /// Halo 3 on: Halo 2's, and `|kAARRGGBB` / `|j` color.
    Color,
}

/// What a game does with a tag it knows but can't read (a `<color>` with no
/// `argb`, a `<font>` with a face it doesn't have).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BadTag {
    /// Halo 3: shows it as written.
    Literal,
    /// ODST, Reach, Halo 4, Halo 2 Anniversary: the whole string fails.
    FailsString,
}

/// The tags a game reads: `string_text.tags`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct XmlTags {
    /// `<font face=…>`'s faces.
    fonts: Vec<String>,
    /// Tags any menu text can hold.
    global: Vec<String>,
    /// Tags only saved-file default names fill in.
    save: Vec<String>,
    /// Tags only the date format string fills in.
    date: Vec<String>,
    /// Tags only one screen fills in → the screens.
    #[serde(default)]
    screens: serde_json::Map<String, serde_json::Value>,
    bad_tag: BadTag,
}

/// How a game's `#keyword`s are matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum KeywordMatch {
    /// The whole `#[a-z_]*` run names the keyword.
    #[default]
    Exact,
    /// Halo 2: the run need only start a keyword; the first in order wins, and
    /// a bare `#` reads as the first keyword.
    Prefix,
}

/// `string_text.cui`.
#[derive(Debug, Clone, Copy, Deserialize)]
pub(super) struct Cui {
    /// Whether `[±N]` may follow the property (Halo 4, Halo 2 Anniversary).
    offset: bool,
}

/// One game's runtime text syntax.
#[derive(Debug, Clone, Default)]
pub struct MarkupProfile {
    pub(super) pipes: Pipes,
    pub(super) xml: Option<XmlTags>,
    pub(super) cui: Option<Cui>,
    pub(super) keywords: Vec<String>,
    pub(super) keyword_match: KeywordMatch,
}

impl MarkupProfile {
    pub fn keywords(&self) -> &[String] {
        &self.keywords
    }

    pub fn has_pipes(&self) -> bool {
        self.pipes != Pipes::None
    }

    pub fn has_tags(&self) -> bool {
        self.xml.is_some()
    }

    pub fn has_cui(&self) -> bool {
        self.cui.is_some()
    }

    /// The `|` escapes the game reads, for a completion list.
    pub(super) fn escape_choices(&self) -> Vec<Choice> {
        let escape = |c: &str, detail: &str| Choice::new(c, format!("|{c}"), format!("|{c}"), detail);
        let mut choices = match self.pipes {
            Pipes::None => return Vec::new(),
            Pipes::LineBreak => return vec![escape("n", "line break")],
            Pipes::Halo2 | Pipes::Color => vec![
                escape("n", "line break"),
                escape("t", "tab"),
                escape("l", "align left"),
                escape("r", "align right"),
                escape("c", "align center"),
                escape("|", "a | sign"),
            ],
        };
        if self.pipes == Pipes::Color {
            choices.push(Choice::new("k", "|kAARRGGBB", "|kFFFFFFFF", "start a color"));
            choices.push(escape("j", "end the color"));
        }
        choices
    }

    /// The tags the game reads anywhere it reads tags, for a completion list.
    /// Tags that only one screen fills in are left out: which screen shows a
    /// string isn't known from the string.
    pub(super) fn tag_choices(&self) -> Vec<Choice> {
        let Some(xml) = &self.xml else {
            return Vec::new();
        };
        let face = xml.fonts.get(1).map_or("terminal", String::as_str);
        let mut choices = vec![
            Choice::new("color", "<color argb=#AARRGGBB>", "<color argb=#FFFFFFFF>", "color"),
            Choice::new("/color", "</color>", "</color>", "end of color"),
            Choice::new("br", "<br/>", "<br/>", "line break"),
            Choice::new("p", "<p align=…>", "<p align=center>", "paragraph"),
            Choice::new("/p", "</p>", "</p>", "end of paragraph"),
            Choice::new("div", "<div align=…>", "<div align=center>", "block"),
            Choice::new("/div", "</div>", "</div>", "end of block"),
            Choice::new("font", "<font face=…>", format!("<font face=\"{face}\">"), "font"),
            Choice::new("/font", "</font>", "</font>", "end of font"),
            Choice::new("!--", "<!-- … -->", "<!--  -->", "comment"),
        ];
        let placeholder = |name: &str, detail: &str| Choice::new(name, format!("<{name}/>"), format!("<{name}/>"), detail);
        for name in &xml.global {
            choices.push(placeholder(name, global_tag_note(name)));
        }
        choices.extend(xml.save.iter().map(|name| placeholder(name, "saved-file names only")));
        choices.extend(xml.date.iter().map(|name| placeholder(name, "date format only")));
        choices
    }
}

fn global_tag_note(name: &str) -> &'static str {
    if matches!(name, "build_number" | "version_number") { "empty in MCC" } else { "filled in by the game" }
}

/// What a marked piece of text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkupKind {
    /// A `|` escape.
    Escape,
    /// `|kAARRGGBB`.
    Color,
    /// A formatting tag: `<color …>`, `</p>`, `<br/>`, …
    Tag,
    /// A tag the game fills in: `<build_number/>`, `<lobby-privacy/>`, …
    Placeholder,
    /// `<!-- … -->`.
    Comment,
    /// `&lt;`, `&#160;`, …
    Entity,
    /// `{cui:component.property}`.
    Binding,
    /// A multiplayer `#keyword`.
    Keyword,
}

/// A marked piece of a string's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkupSpan {
    /// Byte range in the text (not the text as shown for editing).
    pub range: Range<usize>,
    pub kind: MarkupKind,
    /// What it does, or why the game won't read it as it looks.
    pub note: Option<String>,
    /// The game won't read it as it looks.
    pub warning: bool,
    /// The color a `<color argb=…>` or `|kAARRGGBB` sets, and where its hex
    /// digits are.
    pub color: Option<ColorValue>,
}

/// A color written in text, as hex digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColorValue {
    /// As the game reads the digits: `0xAARRGGBB`.
    pub argb: u32,
    /// Byte range of the hex digits in the text.
    pub digits: Range<usize>,
}

impl ColorValue {
    /// `text` with this color's digits replaced by `argb`, as eight hex
    /// digits.
    pub fn replaced_in(&self, text: &str, argb: u32) -> String {
        let mut text = text.to_owned();
        text.replace_range(self.digits.clone(), &format!("{argb:08X}"));
        text
    }
}

impl MarkupSpan {
    fn new(range: Range<usize>, kind: MarkupKind) -> Self {
        Self { range, kind, note: None, warning: false, color: None }
    }

    fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    fn warn(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self.warning = true;
        self
    }
}

/// Mark what `profile`'s game reads in `text` when it draws it.
pub fn scan_markup(text: &str, profile: &MarkupProfile) -> Vec<MarkupSpan> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        let found = match bytes[at] {
            b'|' => pipe(rest, profile.pipes),
            b'#' if !profile.keywords.is_empty() => keyword(rest, profile),
            b'<' => profile.xml.as_ref().and_then(|xml| tag(rest, xml)),
            b'&' if profile.xml.is_some() => entity(rest),
            b'{' => profile.cui.and_then(|cui| binding(rest, cui.offset)),
            _ => None,
        };
        match found {
            Some(span) if span.range.end > 0 => {
                let len = span.range.end;
                let color = span.color.map(|color| ColorValue {
                    digits: at + color.digits.start..at + color.digits.end,
                    ..color
                });
                spans.push(MarkupSpan { range: at + span.range.start..at + len, color, ..span });
                at += len;
            }
            _ => at += rest.chars().next().map_or(1, char::len_utf8),
        }
    }
    spans
}

fn pipe(rest: &str, pipes: Pipes) -> Option<MarkupSpan> {
    let next = rest[1..].chars().next()?;
    let escape = |note: &str| Some(MarkupSpan::new(0..2, MarkupKind::Escape).note(note));
    match (pipes, next) {
        (Pipes::None, _) => None,
        (_, 'n') => escape("line break"),
        (Pipes::LineBreak, _) => None,
        (_, '|') => escape("a | sign"),
        (_, 't') => escape("tab"),
        (_, 'l') => escape("align left"),
        (_, 'r') => escape("align right"),
        (_, 'c') => escape("align center"),
        (Pipes::Color, 'j') => escape("end the color"),
        (Pipes::Color, 'k') => {
            let digits = rest[2..].bytes().take(8).take_while(u8::is_ascii_hexdigit).count();
            Some(if digits == 8 {
                let argb = u32::from_str_radix(&rest[2..10], 16).expect("eight hex digits");
                MarkupSpan { color: Some(ColorValue { argb, digits: 2..10 }), ..MarkupSpan::new(0..10, MarkupKind::Color) }
                    .note("color (AARRGGBB)")
            } else {
                MarkupSpan::new(0..2, MarkupKind::Color)
                    .warn("|k takes exactly 8 hex digits (AARRGGBB); the game drops the |k and shows the rest as text")
            })
        }
        _ => None,
    }
}

fn keyword(rest: &str, profile: &MarkupProfile) -> Option<MarkupSpan> {
    let len = rest[1..].find(|c: char| !(c.is_ascii_lowercase() || c == '_')).unwrap_or(rest.len() - 1);
    let run = &rest[1..1 + len];
    let span = |len: usize| MarkupSpan::new(0..1 + len, MarkupKind::Keyword);
    if profile.keywords.iter().any(|keyword| keyword == run) {
        return Some(span(len).note("filled in by the game in multiplayer event messages"));
    }
    if profile.keyword_match == KeywordMatch::Prefix
        && let Some(full) = profile.keywords.iter().find(|keyword| keyword.starts_with(run))
    {
        return Some(span(len).warn(format!(
            "In multiplayer event messages, Halo 2 reads this as #{full}: it matches keywords by their start"
        )));
    }
    None
}

const HTML_TAGS: &[(&str, &str)] = &[
    ("br", "line break"),
    ("p", "paragraph"),
    ("/p", "end of paragraph"),
    ("div", "block"),
    ("/div", "end of block"),
    ("font", "font"),
    ("/font", "end of font"),
    ("color", "color"),
    ("/color", "end of color"),
];

fn tag(rest: &str, xml: &XmlTags) -> Option<MarkupSpan> {
    let after = rest[1..].chars().next()?;
    if !(after.is_alphabetic() || matches!(after, '!' | '/' | '?')) {
        return None;
    }
    let close = rest.find('>')?;
    // A comment: `<!--`, then whitespace, `/` or `>`, through `-->`.
    if let Some(body) = rest.strip_prefix("<!--") {
        if !body.starts_with(|c: char| c.is_whitespace() || c == '/' || c == '>') {
            return None;
        }
        let end = body.find("-->").map_or(rest.len(), |at| 4 + at + 3);
        return Some(MarkupSpan::new(0..end, MarkupKind::Comment).note("comment: the game shows none of it"));
    }
    let inside = &rest[1..close];
    let name_end = inside
        .char_indices()
        .skip(1)
        .find(|&(_, c)| c.is_whitespace() || c == '\u{a0}' || c == '/' || c == '>')
        .map_or(inside.len(), |(at, _)| at);
    let name = inside[..name_end].to_ascii_lowercase();
    let attributes = &inside[name_end..];
    let span = MarkupSpan::new(0..close + 1, MarkupKind::Tag);
    let failed = |why: &str| match xml.bad_tag {
        BadTag::Literal => format!("{why}, so the game shows this tag as written"),
        BadTag::FailsString => format!("{why}, so the game fails to show the whole string"),
    };
    if let Some(&(_, what)) = HTML_TAGS.iter().find(|&&(tag, _)| tag == name) {
        let span = span.note(what);
        return Some(match name.as_str() {
            "color" => match color_value(attributes) {
                Err(why) => span.warn(failed(why)),
                Ok((at, digits)) => {
                    // `attributes` starts after the `<` and the name.
                    let start = 1 + name_end + at;
                    let hex = &rest[start..start + digits];
                    // The game reads it with `wcstoul`, which gives all ones
                    // past 32 bits.
                    let argb = u32::from_str_radix(hex, 16).unwrap_or(u32::MAX);
                    let span = MarkupSpan { color: Some(ColorValue { argb, digits: start..start + digits }), ..span };
                    if digits < 8 {
                        span.warn(format!(
                            "argb has {digits} hex digits: it is AARRGGBB, alpha first, so this color is transparent"
                        ))
                    } else {
                        span
                    }
                }
            },
            "font" => match attribute_value(attributes, "face") {
                None => span.warn(failed("<font> needs a face")),
                Some((value, quoted)) if !xml.fonts.iter().any(|font| font.eq_ignore_ascii_case(&value)) => {
                    let bare = if !quoted && attributes.contains(&format!("{value}_")) {
                        " (an unquoted value ends at its first _: quote it)"
                    } else {
                        ""
                    };
                    span.warn(failed(&format!("the game has no font face '{value}'{bare}")))
                }
                Some(_) => span,
            },
            "p" | "div" => match attribute_value(attributes, "align") {
                Some((value, _)) if !["left", "right", "center"].iter().any(|a| a.eq_ignore_ascii_case(&value)) => {
                    span.warn(format!("align is left, right or center; the game ignores '{value}'"))
                }
                _ => span,
            },
            _ => span,
        });
    }
    let placeholder = MarkupSpan::new(0..close + 1, MarkupKind::Placeholder);
    if xml.global.contains(&name) {
        let note = match global_tag_note(&name) {
            "empty in MCC" => "filled in by the game (empty in MCC)",
            note => note,
        };
        return Some(placeholder.note(note));
    }
    if xml.save.contains(&name) {
        return Some(placeholder.note("filled in only in the default names of saved films, screenshots and variants"));
    }
    if xml.date.contains(&name) {
        return Some(placeholder.note("filled in only in the date format string"));
    }
    if let Some(screens) = xml.screens.get(&name).and_then(|screens| screens.as_str()) {
        return Some(placeholder.note(format!("filled in only on these screens: {screens}")));
    }
    None
}

/// `argb`'s value: `"`? `#`? (or `0x`) then hex. Returns where its hex digits
/// start in `attributes`, and how many there are.
fn color_value(attributes: &str) -> Result<(usize, usize), &'static str> {
    let at = attributes.find("argb").ok_or("<color> needs argb=#AARRGGBB")?;
    let value = attributes[at + 4..].trim_start().strip_prefix('=').ok_or("argb has no value")?.trim_start();
    if value.starts_with('\'') {
        return Err("argb can't be in single quotes");
    }
    let value = value.strip_prefix('"').unwrap_or(value);
    let value = value.strip_prefix('#').unwrap_or(value);
    let value = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).unwrap_or(value);
    match value.bytes().take_while(u8::is_ascii_hexdigit).count() {
        0 => Err("argb's value isn't hex"),
        digits => Ok((attributes.len() - value.len(), digits)),
    }
}

/// An attribute's value as the game reads it: quoted, or a run of letters
/// and digits. Returns the value and whether it was quoted.
fn attribute_value(attributes: &str, name: &str) -> Option<(String, bool)> {
    let at = attributes.find(name)?;
    let value = attributes[at + name.len()..].trim_start().strip_prefix('=')?.trim_start();
    for quote in ['"', '\''] {
        if let Some(quoted) = value.strip_prefix(quote) {
            return Some((quoted.split(quote).next().unwrap_or_default().to_owned(), true));
        }
    }
    Some((value.chars().take_while(|c| c.is_alphanumeric()).collect(), false))
}

fn entity(rest: &str) -> Option<MarkupSpan> {
    let end = rest.find(';').filter(|&end| end <= 6)?;
    let name = rest[1..end].to_ascii_lowercase();
    let span = MarkupSpan::new(0..end + 1, MarkupKind::Entity);
    let note = match name.as_str() {
        "lt" => "<",
        "gt" => ">",
        "amp" => "&",
        "quot" => "\"",
        "nbsp" => "a space",
        "copy" => return Some(span.warn("the game turns &copy; into the wrong character (U+FFA9)")),
        _ => {
            let number: u32 = name.strip_prefix('#').filter(|digits| (2..=3).contains(&digits.len()))?.parse().ok()?;
            return Some(match number {
                0..=127 => span.note(format!("character {number}")),
                128..=255 => span.warn(format!(
                    "the game turns &#{number}; into the wrong character (U+FF{:02X})",
                    number
                )),
                _ => return None,
            });
        }
    };
    Some(span.note(format!("{note}, in text that also has a tag")))
}

fn binding(rest: &str, offset: bool) -> Option<MarkupSpan> {
    let body = rest.strip_prefix("{cui:")?;
    let close = rest.find('}');
    let ident = |text: &str| {
        let mut chars = text.char_indices();
        match chars.next() {
            Some((_, c)) if c.is_ascii_alphabetic() || c == '_' => {}
            _ => return 0,
        }
        chars.find(|&(_, c)| !(c.is_ascii_alphanumeric() || c == '_')).map_or(text.len(), |(at, _)| at)
    };
    let parsed = (|| {
        let component = ident(body);
        let after = body[component..].strip_prefix('.')?;
        let property = ident(after);
        let mut tail = &after[property..];
        if offset && let Some(signed) = tail.strip_prefix(['+', '-']) {
            let digits = signed.find(|c: char| !c.is_ascii_digit()).unwrap_or(signed.len());
            tail = &signed[digits..];
        }
        (component > 0 && property > 0 && tail.starts_with('}')).then(|| rest.len() - tail.len() + 1)
    })();
    Some(match (parsed, close) {
        (Some(end), _) => MarkupSpan::new(0..end, MarkupKind::Binding).note("filled in from the screen's data"),
        (None, Some(close)) => MarkupSpan::new(0..close + 1, MarkupKind::Binding).warn(
            "not a binding the game can read ({cui:component.property}, letters, digits and _ only), so it shows as written",
        ),
        (None, None) => MarkupSpan::new(0..5, MarkupKind::Binding).warn("{cui: binding with no closing }"),
    })
}
