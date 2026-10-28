//! How a string list's text is written for editing: the escapes and `&name`
//! glyphs of each game's tool, read and written so that what the editor shows
//! parses back to the same text.
//!
//! Every tool from Halo 2 on reads text through the same two steps
//! (`import_text.cpp`): C escapes (`\"`, `\\`, `\n`, `\r`, `\t`, `\a`, `\b`,
//! `\f`, `\v`, `\'`, `\;`, `\?`), then a pass that replaces `&name` with the
//! private-use character the game's table gives it, `&U+XXXX` (four hex
//! digits, capital `U`) with that code point, and `&&` with `&`. Any other `&`
//! fails the import. Halo 2, Halo 3 and ODST take the first table entry the
//! text starts with; Reach takes the longest. Halo CE's tool does none of
//! this: its text is copied as written.
//!
//! The editor reads `&name` by the longest name in every game, so every
//! glyph can be written by its name: under Halo 3's first-match rule
//! `&stick_move_forward` reads as `&stick_move` and the text `_forward`.
//! Text that goes through the tool, a `[Strings]` file's lines, is read and
//! written by the tool's own rule ([`strings_file_line`],
//! [`parse_strings_file`]).
//!
//! The editor shows a string one line at a time, so line breaks, tabs and
//! backslashes are shown escaped in every game, Halo CE included.

use std::ops::Range;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::markup::{ColorValue, Cui, KeywordMatch, MarkupProfile, MarkupSpan, Pipes, XmlTags, scan_markup};

/// Which table entry a `&name` takes when several names start the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GlyphMatch {
    /// The first entry in table order (Halo 2, Halo 3, ODST).
    First,
    /// The longest entry (Reach).
    Longest,
}

/// One game's text syntax: what its tool reads at import (`&name` glyphs),
/// and what its renderer reads when it draws ([`MarkupProfile`]). Cheap to
/// clone.
#[derive(Debug, Clone)]
pub struct TextSyntax {
    /// Whether `&` starts a glyph. Off for Halo CE, whose tool copies text as
    /// written.
    glyphs: bool,
    /// Whether `%` starts a HUD message's runtime element (Halo CE's
    /// `hud_message_text`); see [`TextSyntax::with_hud_messages`].
    hud_messages: bool,
    /// The game's HUD runtime element names, in code order.
    hud_tokens: Arc<[String]>,
    /// The game's `&name` table, in its tool's order. Empty for a game whose
    /// table is not known.
    names: Arc<[(String, u16)]>,
    rule: GlyphMatch,
    /// What the game reads when it draws the text.
    markup: Arc<MarkupProfile>,
}

/// `_meta.json`'s `string_text`: what one game reads in string list text.
///
/// - `glyphs`: whether `&` starts a glyph (false for Halo CE).
/// - `glyph_match`: `first` or `longest`.
/// - `glyph_names`: `{"a_button": "E100", …}`, in the tool's table order,
///   which the `first` rule depends on.
/// - `keywords`: the multiplayer `#keyword`s, without `#`.
/// - `keyword_match`: `exact`, or `prefix` (Halo 2).
/// - `escapes`: the `|` escapes the renderer reads: `none`, `line_break`
///   (`|n` only), `halo2`, or `color` (`halo2`'s and `|k`/`|j`).
/// - `tags`: `null`, or `{fonts, global, save, date, screens, bad_tag}`,
///   where `screens` maps a tag to the screens that fill it in and
///   `bad_tag` is `literal` or `fails_string`.
/// - `cui`: `null`, or `{"offset": bool}` for `{cui:…}` bindings.
/// - `hud_tokens`: the names of `hud_message_text`'s runtime elements, in
///   code order (Halo CE only).
/// - `source`: where it was read from (not read by the engine).
#[derive(Deserialize)]
struct StringTextFile {
    glyphs: bool,
    glyph_match: GlyphMatch,
    /// Name → code point (hex), in the tool's table order.
    glyph_names: serde_json::Map<String, serde_json::Value>,
    keywords: Vec<String>,
    keyword_match: KeywordMatch,
    escapes: Pipes,
    tags: Option<XmlTags>,
    cui: Option<Cui>,
    #[serde(default)]
    hud_tokens: Vec<String>,
}

#[derive(Deserialize)]
struct MetaFile {
    #[serde(default)]
    string_text: Option<StringTextFile>,
}

#[derive(Debug)]
pub enum TextSyntaxError {
    Io(PathBuf, std::io::Error),
    Json(PathBuf, serde_json::Error),
    /// A glyph whose code point isn't four hex digits.
    CodePoint(PathBuf, String),
}

impl std::fmt::Display for TextSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Json(path, error) => write!(f, "{}: {error}", path.display()),
            Self::CodePoint(path, name) => write!(f, "{}: glyph `{name}` has no four-digit hex code point", path.display()),
        }
    }
}

impl std::error::Error for TextSyntaxError {}

impl TextSyntax {
    /// The syntax of the game whose definitions are in `game_dir`, from its
    /// `_meta.json`'s `string_text`. A game with none reads `&&` and
    /// `&U+XXXX` and nothing else.
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, TextSyntaxError> {
        let path = game_dir.as_ref().join("_meta.json");
        let bytes = std::fs::read(&path).map_err(|error| TextSyntaxError::Io(path.clone(), error))?;
        let meta: MetaFile = serde_json::from_slice(&bytes).map_err(|error| TextSyntaxError::Json(path.clone(), error))?;
        let Some(file) = meta.string_text else {
            return Ok(Self::unknown());
        };
        let names = file
            .glyph_names
            .iter()
            .map(|(name, value)| {
                let value = value
                    .as_str()
                    .filter(|value| value.len() == 4)
                    .and_then(|value| u16::from_str_radix(value, 16).ok())
                    .ok_or_else(|| TextSyntaxError::CodePoint(path.clone(), name.clone()))?;
                Ok((name.clone(), value))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let markup = MarkupProfile {
            pipes: file.escapes,
            xml: file.tags,
            cui: file.cui,
            keywords: file.keywords,
            keyword_match: file.keyword_match,
        };
        Ok(Self {
            glyphs: file.glyphs,
            hud_messages: false,
            hud_tokens: file.hud_tokens.into(),
            names: names.into(),
            rule: file.glyph_match,
            markup: Arc::new(markup),
        })
    }

    /// A game nothing is known of: `&&` and `&U+XXXX`, no names, no markup.
    pub fn unknown() -> Self {
        Self { glyphs: true, ..Self::plain() }
    }

    /// Text with no glyphs and no markup: `&` is an ordinary character.
    pub fn plain() -> Self {
        Self {
            glyphs: false,
            hud_messages: false,
            hud_tokens: Arc::from([]),
            names: Arc::from([]),
            rule: GlyphMatch::Longest,
            markup: Arc::default(),
        }
    }

    /// This syntax with `%` read as a HUD message's runtime element, by the
    /// game's [`hud_tokens`](Self::hud_tokens), when `enabled`.
    pub fn with_hud_messages(mut self, enabled: bool) -> Self {
        self.hud_messages = enabled;
        self
    }

    /// The game's HUD runtime element names, in code order.
    pub fn hud_tokens(&self) -> &[String] {
        &self.hud_tokens
    }

    /// The runtime element name `text` (what follows a `%`) starts with, as
    /// tool.exe's import matches it: the first in code order, ignoring
    /// case. Its code and length.
    pub(super) fn match_hud_token(&self, text: &str) -> Option<(u8, usize)> {
        self.hud_tokens.iter().enumerate().find_map(|(code, name)| {
            let matches = text.get(..name.len()).is_some_and(|start| start.eq_ignore_ascii_case(name));
            matches.then(|| (u8::try_from(code).ok(), name.len())).and_then(|(code, len)| Some((code?, len)))
        })
    }

    /// The runtime element at the start of `text` (what follows a `%`) in
    /// shown text: `{name}`, `{code}` or a name. Its code and length.
    fn hud_element_at(&self, text: &str) -> Option<(u8, usize)> {
        if let Some(inner) = text.strip_prefix('{') {
            let end = inner.find('}')?;
            let code = match self.hud_tokens.iter().position(|name| name.eq_ignore_ascii_case(&inner[..end])) {
                Some(code) => u8::try_from(code).ok()?,
                None => return super::hud::coded_element(text),
            };
            return Some((code, end + 2));
        }
        self.match_hud_token(text)
    }

    /// How runtime element `code` is shown before `suffix`: by its name if
    /// it reads back, else `{name}`, else `{code}`.
    fn hud_element_shown(&self, code: u8, suffix: &str) -> String {
        match self.hud_tokens.get(usize::from(code)) {
            Some(name) if self.match_hud_token(&format!("{name}{suffix}")) == Some((code, name.len())) => format!("%{name}"),
            Some(name) => format!("%{{{name}}}"),
            None => super::hud::element_text(code),
        }
    }

    pub fn has_glyphs(&self) -> bool {
        self.glyphs
    }

    /// The game's glyph names and code points, in table order.
    pub fn glyph_names(&self) -> &[(String, u16)] {
        &self.names
    }

    /// The game's `#keyword`s, without their `#`.
    pub fn keywords(&self) -> &[String] {
        self.markup.keywords()
    }

    /// What the game reads when it draws the text.
    pub fn markup(&self) -> &MarkupProfile {
        &self.markup
    }

    /// The table entry `text` (what follows an `&`) starts with, by `rule`.
    fn match_name(&self, text: &str, rule: GlyphMatch) -> Option<(&str, u16)> {
        let mut found: Option<(&str, u16)> = None;
        for (name, value) in self.names.iter() {
            if !text.starts_with(name.as_str()) {
                continue;
            }
            match rule {
                GlyphMatch::First => return Some((name, *value)),
                GlyphMatch::Longest => {
                    if found.is_none_or(|(best, _)| name.len() > best.len()) {
                        found = Some((name, *value));
                    }
                }
            }
        }
        found
    }

    /// The name `value` is written with, if the table has one.
    fn name_of(&self, value: u16) -> Option<&str> {
        self.names.iter().find(|(_, code)| *code == value).map(|(name, _)| name.as_str())
    }
}

/// What a stretch of shown text is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Text,
    /// A backslash escape.
    Escape,
    /// `&name` or `&U+XXXX`.
    Glyph,
    /// `&&`.
    Ampersand,
    /// A backslash escape the tools do not read.
    BadEscape,
    /// An `&` that starts nothing the tool reads.
    BadGlyph,
}

impl TokenKind {
    pub fn is_error(self) -> bool {
        matches!(self, TokenKind::BadEscape | TokenKind::BadGlyph)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Byte range in the shown text.
    pub range: Range<usize>,
    pub kind: TokenKind,
}

/// Split shown text into tokens, and read the text it stands for. Errors are
/// tokens too, so a box can mark them while the user types.
pub fn lex(shown: &str, syntax: &TextSyntax) -> (Vec<Token>, String) {
    let (tokens, text, _) = lex_mapped(shown, syntax, EDITOR_MATCH);
    (tokens, text)
}

/// How the editor reads `&name`: by the longest name, whatever the game's
/// tool does, so every glyph has a name that reads back.
const EDITOR_MATCH: GlyphMatch = GlyphMatch::Longest;

/// For each character of the text, where it starts in the text and the
/// shown bytes it came from.
type ShownMap = Vec<(usize, Range<usize>)>;

/// [`lex`], and the [`ShownMap`] from the text back to what was shown.
fn lex_mapped(shown: &str, syntax: &TextSyntax, rule: GlyphMatch) -> (Vec<Token>, String, ShownMap) {
    let mut tokens: Vec<Token> = Vec::new();
    let mut text = String::with_capacity(shown.len());
    let mut map = Vec::with_capacity(shown.len());
    let push = |tokens: &mut Vec<Token>, range: Range<usize>, kind: TokenKind| {
        if kind == TokenKind::Text
            && let Some(last) = tokens.last_mut()
            && last.kind == TokenKind::Text
            && last.range.end == range.start
        {
            last.range.end = range.end;
            return;
        }
        tokens.push(Token { range, kind });
    };
    let mut emit = |text: &mut String, c: char, from: Range<usize>| {
        map.push((text.len(), from));
        text.push(c);
    };
    let mut at = 0;
    while at < shown.len() {
        let rest = &shown[at..];
        let c = rest.chars().next().expect("not at the end");
        if c == '\\' {
            let next = rest[1..].chars().next();
            let value = next.and_then(escape_value);
            let len = 1 + next.map_or(0, char::len_utf8);
            match value {
                Some(value) => {
                    emit(&mut text, value, at..at + len);
                    push(&mut tokens, at..at + len, TokenKind::Escape);
                }
                None => push(&mut tokens, at..at + len, TokenKind::BadEscape),
            }
            at += len;
            continue;
        }
        if c == '%' && syntax.hud_messages {
            let after = &rest[1..];
            if after.starts_with('%') {
                emit(&mut text, '%', at..at + 2);
                emit(&mut text, '%', at..at + 2);
                push(&mut tokens, at..at + 2, TokenKind::Ampersand);
                at += 2;
                continue;
            }
            if let Some((code, len)) = syntax.hud_element_at(after) {
                for c in super::hud::element_text(code).chars() {
                    emit(&mut text, c, at..at + 1 + len);
                }
                push(&mut tokens, at..at + 1 + len, TokenKind::Glyph);
                at += 1 + len;
                continue;
            }
            let len = 1 + name_run(after);
            push(&mut tokens, at..at + len, TokenKind::BadGlyph);
            at += len;
            continue;
        }
        if c == '&' && syntax.glyphs {
            let after = &rest[1..];
            if after.starts_with('&') {
                emit(&mut text, '&', at..at + 2);
                push(&mut tokens, at..at + 2, TokenKind::Ampersand);
                at += 2;
                continue;
            }
            if let Some((name, value)) = syntax.match_name(after, rule) {
                emit(&mut text, char_of(value), at..at + 1 + name.len());
                push(&mut tokens, at..at + 1 + name.len(), TokenKind::Glyph);
                at += 1 + name.len();
                continue;
            }
            if let Some(value) = code_point_glyph(after) {
                emit(&mut text, char_of(value), at..at + 7);
                push(&mut tokens, at..at + 7, TokenKind::Glyph);
                at += 7;
                continue;
            }
            // Mark the `&` and the name-like run after it, so the box shows
            // the whole word as wrong.
            let len = 1 + name_run(after);
            push(&mut tokens, at..at + len, TokenKind::BadGlyph);
            at += len;
            continue;
        }
        emit(&mut text, c, at..at + c.len_utf8());
        push(&mut tokens, at..at + c.len_utf8(), TokenKind::Text);
        at += c.len_utf8();
    }
    (tokens, text, map)
}

/// Shown text read for an editor: its tokens, the text it stands for, and
/// what the game reads in that text when it draws it, placed on the shown
/// text.
#[derive(Debug, Clone)]
pub struct Analysis {
    pub tokens: Vec<Token>,
    pub text: String,
    /// Markup, with byte ranges in the shown text.
    pub markup: Vec<MarkupSpan>,
}

impl Analysis {
    pub fn has_errors(&self) -> bool {
        self.tokens.iter().any(|token| token.kind.is_error())
    }
}

pub fn analyze(shown: &str, syntax: &TextSyntax) -> Analysis {
    let (tokens, text, map) = lex_mapped(shown, syntax, EDITOR_MATCH);
    let markup = scan_markup(&text, &syntax.markup)
        .into_iter()
        .filter_map(|span| {
            let first = map.partition_point(|&(at, _)| at < span.range.start);
            let last = map.partition_point(|&(at, _)| at < span.range.end).checked_sub(1)?;
            let range = map.get(first)?.1.start..map.get(last)?.1.end;
            // The digits too: a color picked for the shown text replaces them
            // there.
            let color = span.color.and_then(|color| {
                let first = map.partition_point(|&(at, _)| at < color.digits.start);
                let last = map.partition_point(|&(at, _)| at < color.digits.end).checked_sub(1)?;
                Some(ColorValue { digits: map.get(first)?.1.start..map.get(last)?.1.end, ..color })
            });
            Some(MarkupSpan { range, color, ..span })
        })
        .collect();
    Analysis { tokens, text, markup }
}

/// The text `shown` stands for, or the byte ranges the tool would refuse.
pub fn parse_shown(shown: &str, syntax: &TextSyntax) -> Result<String, Vec<Range<usize>>> {
    parse_by(shown, syntax, EDITOR_MATCH)
}

fn parse_by(shown: &str, syntax: &TextSyntax, rule: GlyphMatch) -> Result<String, Vec<Range<usize>>> {
    let (tokens, text, _) = lex_mapped(shown, syntax, rule);
    let errors: Vec<Range<usize>> = tokens
        .into_iter()
        .filter(|token| token.kind.is_error())
        .map(|token| token.range)
        .collect();
    if errors.is_empty() { Ok(text) } else { Err(errors) }
}

/// How `text` is shown for editing: what [`parse_shown`] reads back as
/// `text`. A glyph whose name would read back as another (a name followed by
/// text that makes a longer one) is shown by its code point instead.
pub fn format_shown(text: &str, syntax: &TextSyntax) -> String {
    format_by(text, syntax, EDITOR_MATCH)
}

fn format_by(text: &str, syntax: &TextSyntax, rule: GlyphMatch) -> String {
    // Built back to front: whether a name reads back depends on what follows
    // it.
    let mut suffix = String::new();
    for unit in units(text, syntax).into_iter().rev() {
        let c = match unit {
            Unit::Char(c) => c,
            Unit::Percent => {
                suffix.insert_str(0, "%%");
                continue;
            }
            Unit::Element(code) => {
                let shown = syntax.hud_element_shown(code, &suffix);
                suffix.insert_str(0, &shown);
                continue;
            }
        };
        let piece = match c {
            '\\' => "\\\\".to_owned(),
            '\n' => "\\n".to_owned(),
            '\r' => "\\r".to_owned(),
            '\t' => "\\t".to_owned(),
            '\u{7}' => "\\a".to_owned(),
            '\u{8}' => "\\b".to_owned(),
            '\u{c}' => "\\f".to_owned(),
            '\u{b}' => "\\v".to_owned(),
            '&' if syntax.glyphs => "&&".to_owned(),
            c if syntax.glyphs && glyph_code(c).is_some() => {
                let value = glyph_code(c).expect("checked");
                match syntax.name_of(value) {
                    Some(name) if reads_back(name, value, &suffix, syntax, rule) => format!("&{name}"),
                    _ => format!("&U+{value:04X}"),
                }
            }
            c => c.to_string(),
        };
        suffix.insert_str(0, &piece);
    }
    suffix
}

/// A character of the text, or with HUD messages on, a literal percent or a
/// runtime element.
enum Unit {
    Char(char),
    Percent,
    Element(u8),
}

fn units(text: &str, syntax: &TextSyntax) -> Vec<Unit> {
    let pieces = if syntax.hud_messages { super::hud::pieces(text).ok() } else { None };
    match pieces {
        Some(pieces) => pieces
            .into_iter()
            .map(|piece| match piece {
                super::hud::Piece::Text('%') => Unit::Percent,
                super::hud::Piece::Text(c) => Unit::Char(c),
                super::hud::Piece::Element(code) => Unit::Element(code),
            })
            .collect(),
        None => text.chars().map(Unit::Char).collect(),
    }
}

/// Whether `&name` followed by `suffix` reads back as `value` by `rule`.
fn reads_back(name: &str, value: u16, suffix: &str, syntax: &TextSyntax, rule: GlyphMatch) -> bool {
    let candidate = format!("{name}{suffix}");
    syntax
        .match_name(&candidate, rule)
        .is_some_and(|(found, code)| found.len() == name.len() && code == value)
}

/// A character the editor shows as a glyph: a private-use character, or any
/// other control character with no escape of its own.
fn glyph_code(c: char) -> Option<u16> {
    let value = u32::from(c);
    let private_use = (0xE000..=0xF8FF).contains(&value);
    let unescaped_control = value < 0x20 && !matches!(c, '\n' | '\r' | '\t' | '\u{7}' | '\u{8}' | '\u{b}' | '\u{c}');
    (private_use || unescaped_control).then_some(value as u16)
}

fn escape_value(c: char) -> Option<char> {
    Some(match c {
        '"' => '"',
        '\'' => '\'',
        ';' => ';',
        '?' => '?',
        '\\' => '\\',
        'a' => '\u{7}',
        'b' => '\u{8}',
        'f' => '\u{c}',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\u{b}',
        _ => return None,
    })
}

/// `U+XXXX` at the start of `text`: exactly four hex digits.
fn code_point_glyph(text: &str) -> Option<u16> {
    let digits = text.strip_prefix("U+")?.get(..4)?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

fn char_of(value: u16) -> char {
    char::from_u32(u32::from(value)).unwrap_or(char::REPLACEMENT_CHARACTER)
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// The length of the name-like run `text` starts with.
fn name_run(text: &str) -> usize {
    text.find(|c: char| !is_name_char(c)).unwrap_or(text.len())
}

/// What a keyword is made of after its `#`, as the games read it.
fn is_keyword_char(c: char) -> bool {
    c.is_ascii_lowercase() || c == '_'
}


/// What a completion inserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    /// A CE HUD runtime prompt or substitution.
    Hud,
    /// An `&name` glyph.
    Glyph,
    /// A `#keyword` of multiplayer event text.
    Keyword,
    /// A `|` escape.
    Escape,
    /// A `<tag>`.
    Tag,
}

impl CompletionKind {
    const ALL: [CompletionKind; 5] =
        [CompletionKind::Hud, CompletionKind::Glyph, CompletionKind::Keyword, CompletionKind::Escape, CompletionKind::Tag];

    fn mark(self) -> char {
        match self {
            CompletionKind::Hud => '%',
            CompletionKind::Glyph => '&',
            CompletionKind::Keyword => '#',
            CompletionKind::Escape => '|',
            CompletionKind::Tag => '<',
        }
    }

    /// Whether `c` can be part of what is typed after the mark.
    fn is_name_char(self, c: char) -> bool {
        match self {
            CompletionKind::Hud | CompletionKind::Glyph => is_name_char(c),
            CompletionKind::Keyword => is_keyword_char(c),
            CompletionKind::Escape => c.is_ascii_alphabetic(),
            CompletionKind::Tag => c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '/' | '!'),
        }
    }

    /// The longest run typed after the mark (an escape is one character).
    fn max_run(self) -> usize {
        match self {
            CompletionKind::Escape => 1,
            _ => usize::MAX,
        }
    }

    fn offered_by(self, syntax: &TextSyntax) -> bool {
        match self {
            CompletionKind::Hud => syntax.hud_messages && !syntax.hud_tokens.is_empty(),
            CompletionKind::Glyph => syntax.glyphs,
            CompletionKind::Keyword => !syntax.keywords().is_empty(),
            CompletionKind::Escape => syntax.markup.has_pipes(),
            CompletionKind::Tag => syntax.markup.has_tags(),
        }
    }
}

/// A glyph, keyword, escape or tag being typed: its mark (`&`, `#`, `|` or
/// `<`) before the cursor and the run between it and the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub kind: CompletionKind,
    /// Byte range to replace, from the mark to the end of the run at the
    /// cursor.
    pub replace: Range<usize>,
    /// What has been typed after the mark, up to the cursor.
    pub typed: String,
}

/// What is being typed at byte `cursor`, if the cursor follows a mark the
/// game reads there: an `&` that starts a glyph (not the second `&` of `&&`,
/// nor a finished `&&`), a `#`, a `|` that isn't the second of `||`, or a
/// `<`.
pub fn completion_at(shown: &str, cursor: usize, syntax: &TextSyntax) -> Option<Completion> {
    if cursor > shown.len() || !shown.is_char_boundary(cursor) {
        return None;
    }
    let before = &shown[..cursor];
    let (tokens, _) = lex(before, syntax);
    CompletionKind::ALL.into_iter().filter(|kind| kind.offered_by(syntax)).find_map(|kind| {
        let run_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, c)| kind.is_name_char(c))
            .take(kind.max_run())
            .last()
            .map_or(cursor, |(at, _)| at);
        let mark = run_start.checked_sub(1)?;
        if !before[mark..].starts_with(kind.mark()) {
            return None;
        }
        // The mark must be read as one, as the text is read.
        let token = tokens.iter().find(|token| token.range.start <= mark && mark < token.range.end)?;
        let read_as_mark = match kind {
            CompletionKind::Hud | CompletionKind::Glyph => token.range.start == mark && token.kind != TokenKind::Ampersand,
            _ => token.kind == TokenKind::Text,
        };
        if !read_as_mark {
            return None;
        }
        // The second `|` of `||` closes the pair.
        if kind == CompletionKind::Escape && before[..=mark].bytes().rev().take_while(|&b| b == b'|').count() % 2 == 0 {
            return None;
        }
        let after = &shown[cursor..];
        let run_end = if kind.max_run() == usize::MAX {
            cursor + after.find(|c: char| !kind.is_name_char(c)).unwrap_or(after.len())
        } else {
            cursor
        };
        Some(Completion { kind, replace: mark..run_end, typed: shown[run_start..cursor].to_owned() })
    })
}

/// One thing a completion list offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// What is matched against what was typed after the mark.
    pub name: String,
    /// How the list shows it.
    pub label: String,
    /// What replaces the mark and the run.
    pub insert: String,
    /// What it is, beside it.
    pub detail: String,
}

impl Choice {
    pub(super) fn new(name: impl Into<String>, label: impl Into<String>, insert: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { name: name.into(), label: label.into(), insert: insert.into(), detail: detail.into() }
    }
}

/// Everything `syntax`'s game offers for a completion of `kind`.
pub fn choices(syntax: &TextSyntax, kind: CompletionKind) -> Vec<Choice> {
    match kind {
        CompletionKind::Hud => syntax
            .hud_tokens
            .iter()
            .map(|name| {
                let kind = if name.starts_with("custom-") { "runtime substitution" } else { "button or action" };
                Choice::new(name.as_str(), format!("%{name}"), format!("%{name}"), kind)
            })
            .collect(),
        CompletionKind::Glyph => syntax
            .names
            .iter()
            .map(|(name, value)| Choice::new(name, format!("&{name}"), format!("&{name}"), format!("U+{value:04X}")))
            .collect(),
        CompletionKind::Keyword => syntax
            .keywords()
            .iter()
            .map(|name| Choice::new(name, format!("#{name}"), format!("#{name}"), "multiplayer events"))
            .collect(),
        CompletionKind::Escape => syntax.markup.escape_choices(),
        CompletionKind::Tag => syntax.markup.tag_choices(),
    }
}

/// The choice that best completes `typed`: the first that starts with it,
/// else the first that contains it, ignoring case.
pub fn best_match<'a>(names: impl IntoIterator<Item = &'a str> + Clone, typed: &str) -> Option<usize> {
    let typed = typed.to_ascii_lowercase();
    let lower = |name: &str| name.to_ascii_lowercase();
    names
        .clone()
        .into_iter()
        .position(|name| lower(name).starts_with(&typed))
        .or_else(|| names.into_iter().position(|name| lower(name).contains(&typed)))
}

/// One string as a line of a tool `[Strings]` file: `id = "text"`, its glyphs
/// written so the game's tool reads them back (by code point where its rule
/// would read a name as another).
pub fn strings_file_line(id: &str, text: &str, syntax: &TextSyntax) -> String {
    let shown = format_by(text, syntax, syntax.rule);
    format!("{id} = \"{}\"", quote_escaped(&shown))
}

/// `shown` with every `"` not already escaped escaped, for a quoted string.
fn quote_escaped(shown: &str) -> String {
    let mut out = String::with_capacity(shown.len());
    let mut chars = shown.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                out.push('\\');
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out
}

/// The `id = "text"` lines of a tool `[Strings]` file (a `[Strings]` header
/// and blank lines are skipped), each read into its id and text as the game's
/// tool reads it. A line that is not one is an error naming its line number.
pub fn parse_strings_file(contents: &str, syntax: &TextSyntax) -> Result<Vec<(String, String)>, String> {
    let mut entries = Vec::new();
    for (number, line) in contents.lines().enumerate() {
        let line = line.trim_start_matches('\u{feff}');
        if line.trim().is_empty() || line.trim() == "[Strings]" {
            continue;
        }
        let malformed = || format!("line {} is not `id = \"text\"`", number + 1);
        let (id, rest) = line.split_once('=').ok_or_else(malformed)?;
        let id = id.trim();
        let quoted = rest.trim_start().strip_prefix('"').ok_or_else(malformed)?;
        let end = closing_quote(quoted).ok_or_else(malformed)?;
        if id.is_empty() {
            return Err(malformed());
        }
        let text = parse_by(&quoted[..end], syntax, syntax.rule)
            .map_err(|_| format!("line {} has text the tool would refuse", number + 1))?;
        entries.push((id.to_owned(), text));
    }
    Ok(entries)
}

/// The byte offset of the first `"` in `text` that no backslash escapes.
fn closing_quote(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(at),
            _ => {}
        }
    }
    None
}

const CE_STRING_END: &str = "###END-STRING###";

/// Halo CE strings as a tool `unicode-strings` file: each text, then a line
/// `###END-STRING###`.
pub fn ce_strings_file<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    texts.into_iter().map(|text| format!("{text}\r\n{CE_STRING_END}\r\n")).collect()
}

/// The texts of a tool `unicode-strings` file, as Halo CE's tool reads them
/// (`tool.exe` 0x42FDD0): a string runs to the `###END-STRING###` that ends
/// it, less the line break just before it; line breaks after a marker are
/// skipped. Text after the last marker is a string too.
pub fn parse_ce_strings_file(contents: &str) -> Vec<String> {
    let mut rest = contents.trim_start_matches('\u{feff}');
    let mut texts = Vec::new();
    while !rest.is_empty() {
        let (text, after) = match rest.find(CE_STRING_END) {
            Some(at) => (&rest[..at], &rest[at + CE_STRING_END.len()..]),
            None => (rest, ""),
        };
        let text = text
            .strip_suffix("\r\n")
            .or_else(|| text.strip_suffix('\n'))
            .or_else(|| text.strip_suffix('\r'))
            .unwrap_or(text);
        if !(after.is_empty() && text.trim().is_empty()) {
            texts.push(text.to_owned());
        }
        rest = after.trim_start_matches(['\r', '\n']);
    }
    texts
}
