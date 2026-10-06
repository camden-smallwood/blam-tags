//! The label-template grammar: parsing only. Evaluation is in the parent
//! module, which owns the tag context.
//!
//! A template is text with `{slot}`s. A slot is an expression, an optional
//! `:format`, and `|filter`s:
//!
//! - `{field}`, `{a/b[2]/c}`: a path from the element; `{/a/b}` from the
//!   tag's root; `{../a}` from the element's parent struct; `{ref->a/b}`
//!   through a tag reference into the tag it names.
//! - Index selectors on a path segment: `[#]` (this element's index), `[3]`,
//!   `[{slot}]`, `[*]` (every element, for `any()`), or a predicate
//!   (`[name == {slot}]`), whose bare paths refer to the candidate element.
//! - A path segment may be computed: `{/{type|map:blocks}[...]}`.
//! - `{#}`, `{#1}`, `{#count}`, `{#fraction}`, `{#block}`, `{#group}`.
//! - Arithmetic: `{amount * 100}`, `{(scale - 1) * 100}`, with `+ - * /`
//!   between slots and numbers (spaces around `-` and `/`, which also appear
//!   in field names and paths).
//! - `:format` is a printf conversion without the `%` (`.2f`, `04x`, `+2.0f`,
//!   `3`) or `mb`.
//! - Filters: `text`, `utf16`, `path`, `file.ext`, `group`, `index`, `count`,
//!   `noalias`, `map:NAME`, `enum:NAME`, `none:TEXT`, `bad:TEXT`,
//!   `flags:SEP`, `join:SEP`.
//!
//! A `when` expression combines comparisons (`== != < <= > >=`), a bitwise
//! test (`{flags} & 1`), `&&`, `||`, `!` and parentheses over slots, bare
//! paths (in predicates), numbers and `"strings"`, plus `any(path[*]/..., expr)`
//! and `starts_with(slot, "text")`.

/// A parsed template.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    pub pieces: Vec<Piece>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Text(String),
    Slot(Slot),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub expr: Expr,
    pub format: Option<String>,
    pub filters: Vec<Filter>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Path(Path),
    Hash(Hash),
    Number(f64),
    Binary(Box<Expr>, ArithOp, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    /// `{#}`: the element's index.
    Index,
    /// `{#1}`: the index plus one.
    Index1,
    /// `{#count}`: the block's element count.
    Count,
    /// `{#fraction}`: index ÷ (count − 1).
    Fraction,
    /// `{#block}`: the block definition's own name.
    Block,
    /// `{#group}`: the owning tag's group name.
    Group,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    pub start: PathStart,
    pub segments: Vec<Segment>,
    /// `ref->path`: after `segments` reach a tag reference, continue in the
    /// referenced tag.
    pub then: Option<Box<Path>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathStart {
    /// From the element being labelled (or, in a predicate, the candidate).
    Element,
    /// `/...`: from the tag's root.
    Root,
    /// `../...` repeated n times: from an ancestor of the element.
    Parent(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub name: SegmentName,
    pub select: Option<Select>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SegmentName {
    Literal(String),
    Computed(Box<Slot>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Select {
    /// `[#]`
    ThisIndex,
    /// `[3]`
    Literal(i64),
    /// `[{slot}]`
    Slot(Box<Slot>),
    /// `[*]`
    All,
    /// `[expr]`
    Where(Box<Cond>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Text,
    Utf16,
    Path,
    FileExt,
    Group,
    Index,
    Count,
    NoAlias,
    Map(String),
    Enum(String),
    None(String),
    Bad(String),
    Flags(String),
    Join(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cond {
    Or(Vec<Cond>),
    And(Vec<Cond>),
    Not(Box<Cond>),
    Compare(Operand, CompareOp, Operand),
    /// `{a} & 1`: nonzero.
    BitAnd(Operand, i64),
    /// A lone operand: set (and nonzero, for a number).
    Truthy(Operand),
    /// `any(path, cond)`: some element the path reaches satisfies `cond`,
    /// with bare paths in `cond` relative to that element.
    Any(Path, Box<Cond>),
    StartsWith(Operand, String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Slot(Slot),
    /// A bare path: relative to the candidate element of a predicate or
    /// `any()`.
    Bare(Path),
    Number(f64),
    Text(String),
}

/// A template or `when` expression that doesn't parse.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub at: usize,
    pub message: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "at {}: {}", self.at, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parse a template string.
pub fn parse_template(text: &str) -> Result<Template, ParseError> {
    let mut p = Parser::new(text);
    let mut pieces = Vec::new();
    let mut literal = String::new();
    while let Some(c) = p.peek() {
        if c == '{' {
            if !literal.is_empty() {
                pieces.push(Piece::Text(std::mem::take(&mut literal)));
            }
            pieces.push(Piece::Slot(p.slot()?));
        } else if c == '}' {
            return Err(p.error("unmatched `}`"));
        } else {
            literal.push(c);
            p.bump();
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Text(literal));
    }
    Ok(Template { pieces })
}

/// Parse a `when` expression.
pub fn parse_condition(text: &str) -> Result<Cond, ParseError> {
    let mut p = Parser::new(text);
    let cond = p.or()?;
    p.skip_spaces();
    if p.peek().is_some() {
        return Err(p.error("unexpected text after the expression"));
    }
    Ok(cond)
}

struct Parser<'a> {
    chars: Vec<char>,
    at: usize,
    _text: &'a str,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Parser { chars: text.chars().collect(), at: 0, _text: text }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.at + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        self.at += 1;
        c
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.starts_with(s) {
            self.at += s.chars().count();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, s: &str) -> Result<(), ParseError> {
        if self.eat(s) { Ok(()) } else { Err(self.error(&format!("expected `{s}`"))) }
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(' ') {
            self.at += 1;
        }
    }

    fn error(&self, message: &str) -> ParseError {
        ParseError { at: self.at, message: message.to_owned() }
    }

    /// `{expr[:format][|filter]*}`
    fn slot(&mut self) -> Result<Slot, ParseError> {
        self.expect("{")?;
        let expr = self.arith()?;
        self.skip_spaces();
        let mut format = None;
        if self.eat(":") {
            format = Some(self.until(&['|', '}']));
        }
        let mut filters = Vec::new();
        while self.eat("|") {
            filters.push(self.filter()?);
        }
        self.expect("}")?;
        Ok(Slot { expr, format, filters })
    }

    /// Text up to (not including) the first of `stops`, unnested.
    fn until(&mut self, stops: &[char]) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if stops.contains(&c) {
                break;
            }
            out.push(c);
            self.at += 1;
        }
        out
    }

    fn filter(&mut self) -> Result<Filter, ParseError> {
        let raw = self.until(&['|', '}']);
        let (name, argument) = match raw.split_once(':') {
            Some((name, argument)) => (name, Some(argument.to_owned())),
            None => (raw.as_str(), None),
        };
        let needs = |argument: Option<String>| argument.ok_or_else(|| self.error(&format!("`{name}` needs an argument")));
        Ok(match name {
            "text" => Filter::Text,
            "utf16" => Filter::Utf16,
            "path" => Filter::Path,
            "file.ext" => Filter::FileExt,
            "group" => Filter::Group,
            "index" => Filter::Index,
            "count" => Filter::Count,
            "noalias" => Filter::NoAlias,
            "map" => Filter::Map(needs(argument)?),
            "enum" => Filter::Enum(needs(argument)?),
            "none" => Filter::None(argument.unwrap_or_default()),
            "bad" => Filter::Bad(argument.unwrap_or_default()),
            "flags" => Filter::Flags(argument.unwrap_or_default()),
            "join" => Filter::Join(argument.unwrap_or_default()),
            other => return Err(self.error(&format!("unknown filter `{other}`"))),
        })
    }

    /// `term (( + | - ) term)*`, where `-` needs spaces around it.
    fn arith(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.term()?;
        loop {
            let save = self.at;
            self.skip_spaces();
            let op = if self.eat("+") {
                ArithOp::Add
            } else if self.eat("- ") {
                ArithOp::Sub
            } else {
                self.at = save;
                break;
            };
            self.skip_spaces();
            let right = self.term()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    /// `factor (( * | / ) factor)*`, where `/` needs spaces around it.
    fn term(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.factor()?;
        loop {
            let save = self.at;
            self.skip_spaces();
            let op = if self.eat("*") {
                ArithOp::Mul
            } else if self.eat("/ ") {
                ArithOp::Div
            } else {
                self.at = save;
                break;
            };
            self.skip_spaces();
            let right = self.factor()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
        Ok(left)
    }

    fn factor(&mut self) -> Result<Expr, ParseError> {
        self.skip_spaces();
        if self.eat("(") {
            let inner = self.arith()?;
            self.skip_spaces();
            self.expect(")")?;
            return Ok(inner);
        }
        if self.peek() == Some('#') {
            return self.hash();
        }
        if let Some(number) = self.number() {
            return Ok(Expr::Number(number));
        }
        Ok(Expr::Path(self.path(&[':', '|', '}', '*', '+', ')'])?))
    }

    fn hash(&mut self) -> Result<Expr, ParseError> {
        self.expect("#")?;
        let word: String = std::iter::from_fn(|| {
            let c = self.peek()?;
            (c.is_ascii_alphanumeric()).then(|| {
                self.at += 1;
                c
            })
        })
        .collect();
        Ok(Expr::Hash(match word.as_str() {
            "" => Hash::Index,
            "1" => Hash::Index1,
            "count" => Hash::Count,
            "fraction" => Hash::Fraction,
            "block" => Hash::Block,
            "group" => Hash::Group,
            other => return Err(self.error(&format!("unknown `#{other}`"))),
        }))
    }

    /// A number literal, if one starts here (and isn't a field name that
    /// begins with a digit).
    fn number(&mut self) -> Option<f64> {
        let start = self.at;
        let mut end = start;
        if matches!(self.chars.get(end), Some('-')) {
            end += 1;
        }
        let digits_start = end;
        while matches!(self.chars.get(end), Some(c) if c.is_ascii_digit() || *c == '.') {
            end += 1;
        }
        if end == digits_start {
            return None;
        }
        // A digit run followed by a name character is a field name (`2d`).
        if matches!(self.chars.get(end), Some(c) if c.is_alphabetic() || *c == '_') {
            return None;
        }
        let text: String = self.chars[start..end].iter().collect();
        let value = text.parse().ok()?;
        self.at = end;
        Some(value)
    }

    /// A path, stopping at any of `stops` outside brackets and braces.
    fn path(&mut self, stops: &[char]) -> Result<Path, ParseError> {
        let mut start = PathStart::Element;
        if self.eat("/") {
            start = PathStart::Root;
        } else {
            let mut ups = 0;
            while self.eat("../") {
                ups += 1;
            }
            if ups > 0 {
                start = PathStart::Parent(ups);
            }
        }
        let mut segments = Vec::new();
        loop {
            segments.push(self.segment(stops)?);
            if self.starts_with("->") {
                self.at += 2;
                let rest = self.path(stops)?;
                return Ok(Path { start, segments, then: Some(Box::new(rest)) });
            }
            if !self.eat("/") {
                break;
            }
        }
        Ok(Path { start, segments, then: None })
    }

    fn segment(&mut self, stops: &[char]) -> Result<Segment, ParseError> {
        let name = if self.peek() == Some('{') {
            SegmentName::Computed(Box::new(self.slot()?))
        } else {
            let mut name = String::new();
            while let Some(c) = self.peek() {
                if c == '/' || c == '[' || stops.contains(&c) || self.starts_with("->")
                    || self.starts_with(" - ") || self.starts_with(" / ") || self.starts_with(" ==")
                    || self.starts_with(" !=") || self.starts_with(" <") || self.starts_with(" >")
                    || self.starts_with(" &") || self.starts_with(" |") || self.starts_with(" *")
                    || self.starts_with(" +") || c == ',' || c == ']'
                {
                    break;
                }
                name.push(c);
                self.at += 1;
            }
            let name = name.trim_end().to_owned();
            if name.is_empty() {
                return Err(self.error("expected a field name"));
            }
            SegmentName::Literal(name)
        };
        let select = if self.eat("[") { Some(self.select()?) } else { None };
        Ok(Segment { name, select })
    }

    fn select(&mut self) -> Result<Select, ParseError> {
        self.skip_spaces();
        let select = if self.eat("#]") {
            return Ok(Select::ThisIndex);
        } else if self.eat("*]") {
            return Ok(Select::All);
        } else if let Some(n) = self.number().filter(|n| n.fract() == 0.0) {
            Select::Literal(n as i64)
        } else if self.peek() == Some('{') && self.slot_closes_bracket() {
            Select::Slot(Box::new(self.slot()?))
        } else {
            Select::Where(Box::new(self.or()?))
        };
        self.skip_spaces();
        self.expect("]")?;
        Ok(select)
    }

    /// Whether the `{...}` starting here is the whole selector (`[{slot}]`)
    /// rather than the start of a predicate (`[{a} == b]`).
    fn slot_closes_bracket(&self) -> bool {
        let mut depth = 0;
        let mut i = self.at;
        while let Some(&c) = self.chars.get(i) {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let mut j = i + 1;
                        while self.chars.get(j) == Some(&' ') {
                            j += 1;
                        }
                        return self.chars.get(j) == Some(&']');
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn or(&mut self) -> Result<Cond, ParseError> {
        let mut parts = vec![self.and()?];
        loop {
            self.skip_spaces();
            if !self.eat("||") {
                break;
            }
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::Or(parts) })
    }

    fn and(&mut self) -> Result<Cond, ParseError> {
        let mut parts = vec![self.unary()?];
        loop {
            self.skip_spaces();
            if !self.eat("&&") {
                break;
            }
            parts.push(self.unary()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Cond::And(parts) })
    }

    fn unary(&mut self) -> Result<Cond, ParseError> {
        self.skip_spaces();
        if self.eat("!") && self.peek() != Some('=') {
            return Ok(Cond::Not(Box::new(self.unary()?)));
        }
        if self.eat("(") {
            let inner = self.or()?;
            self.skip_spaces();
            self.expect(")")?;
            return Ok(inner);
        }
        if self.eat("any(") {
            self.skip_spaces();
            let path = self.path(&[',', ')'])?;
            self.skip_spaces();
            self.expect(",")?;
            let cond = self.or()?;
            self.skip_spaces();
            self.expect(")")?;
            return Ok(Cond::Any(path, Box::new(cond)));
        }
        if self.eat("starts_with(") {
            self.skip_spaces();
            let operand = self.operand()?;
            self.skip_spaces();
            self.expect(",")?;
            self.skip_spaces();
            let Operand::Text(prefix) = self.operand()? else {
                return Err(self.error("starts_with needs a quoted prefix"));
            };
            self.skip_spaces();
            self.expect(")")?;
            return Ok(Cond::StartsWith(operand, prefix));
        }
        let left = self.operand()?;
        self.skip_spaces();
        if self.peek() == Some('&') && self.peek_at(1) != Some('&') {
            self.bump();
            self.skip_spaces();
            let Some(mask) = self.number().filter(|n| n.fract() == 0.0) else {
                return Err(self.error("`&` needs an integer mask"));
            };
            return Ok(Cond::BitAnd(left, mask as i64));
        }
        let op = [("==", CompareOp::Eq), ("!=", CompareOp::Ne), ("<=", CompareOp::Le),
                  (">=", CompareOp::Ge), ("<", CompareOp::Lt), (">", CompareOp::Gt)]
            .into_iter()
            .find(|(token, _)| self.eat(token))
            .map(|(_, op)| op);
        let Some(op) = op else {
            return Ok(Cond::Truthy(left));
        };
        self.skip_spaces();
        let right = self.operand()?;
        Ok(Cond::Compare(left, op, right))
    }

    fn operand(&mut self) -> Result<Operand, ParseError> {
        self.skip_spaces();
        if self.peek() == Some('{') {
            return Ok(Operand::Slot(self.slot()?));
        }
        if self.eat("\"") {
            let mut text = String::new();
            loop {
                match self.bump() {
                    Some('"') => break,
                    Some('\\') => text.extend(self.bump()),
                    Some(c) => text.push(c),
                    None => return Err(self.error("unterminated string")),
                }
            }
            return Ok(Operand::Text(text));
        }
        if let Some(number) = self.number() {
            return Ok(Operand::Number(number));
        }
        Ok(Operand::Bare(self.path(&[')', ']', ','])?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str) -> Expr {
        Expr::Path(Path {
            start: PathStart::Element,
            segments: vec![Segment { name: SegmentName::Literal(name.to_owned()), select: None }],
            then: None,
        })
    }

    fn slot(expr: Expr) -> Slot {
        Slot { expr, format: None, filters: Vec::new() }
    }

    #[test]
    fn text_and_slots() {
        let t = parse_template("{type|map:flag_types} #{usage id}").unwrap();
        assert_eq!(
            t.pieces,
            vec![
                Piece::Slot(Slot {
                    expr: field("type"),
                    format: None,
                    filters: vec![Filter::Map("flag_types".to_owned())],
                }),
                Piece::Text(" #".to_owned()),
                Piece::Slot(slot(field("usage id"))),
            ]
        );
    }

    #[test]
    fn formats_hashes_and_arithmetic() {
        let t = parse_template("{#:04x} {#1} {transfer amount * 100:.2f}%").unwrap();
        let Piece::Slot(first) = &t.pieces[0] else { panic!() };
        assert_eq!(first.expr, Expr::Hash(Hash::Index));
        assert_eq!(first.format.as_deref(), Some("04x"));
        let Piece::Slot(third) = &t.pieces[4] else { panic!("{:?}", t.pieces) };
        assert_eq!(
            third.expr,
            Expr::Binary(Box::new(field("transfer amount")), ArithOp::Mul, Box::new(Expr::Number(100.0)))
        );
        assert_eq!(third.format.as_deref(), Some(".2f"));
        let t = parse_template("{(winner scaling factor - 1) * 100:+2.0f}").unwrap();
        let Piece::Slot(s) = &t.pieces[0] else { panic!() };
        let Expr::Binary(left, ArithOp::Mul, _) = &s.expr else { panic!("{:?}", s.expr) };
        assert_eq!(**left, Expr::Binary(Box::new(field("winner scaling factor")), ArithOp::Sub, Box::new(Expr::Number(1.0))));
    }

    #[test]
    fn names_keep_hyphens_and_digits() {
        let t = parse_template("{post-pathfinding} {2d point}").unwrap();
        assert_eq!(t.pieces[0], Piece::Slot(slot(field("post-pathfinding"))));
        assert_eq!(t.pieces[2], Piece::Slot(slot(field("2d point"))));
    }

    #[test]
    fn root_parent_and_reference_paths() {
        let t = parse_template("{/zones[{zone|index}]/areas[{area|index}]} {../cells[#]} {graph->animations[3]/name}").unwrap();
        let Piece::Slot(s) = &t.pieces[0] else { panic!() };
        let Expr::Path(p) = &s.expr else { panic!() };
        assert_eq!(p.start, PathStart::Root);
        assert_eq!(p.segments.len(), 2);
        assert!(matches!(&p.segments[0].select, Some(Select::Slot(inner)) if inner.filters == [Filter::Index]));
        let Piece::Slot(s) = &t.pieces[2] else { panic!() };
        let Expr::Path(p) = &s.expr else { panic!() };
        assert_eq!(p.start, PathStart::Parent(1));
        assert_eq!(p.segments[0].select, Some(Select::ThisIndex));
        let Piece::Slot(s) = &t.pieces[4] else { panic!() };
        let Expr::Path(p) = &s.expr else { panic!() };
        assert!(p.then.as_ref().is_some_and(|rest| rest.segments.len() == 2));
    }

    #[test]
    fn predicates_and_computed_segments() {
        let t = parse_template(
            "{/{object id/type|map:placement_blocks}[object data/object id/unique id == {object id/unique id} && (object data/object id/source != 0 || object data/object id/origin bsp index == {object id/origin bsp index})]/name}:{node index}",
        )
        .unwrap();
        let Piece::Slot(s) = &t.pieces[0] else { panic!() };
        let Expr::Path(p) = &s.expr else { panic!() };
        assert!(matches!(p.segments[0].name, SegmentName::Computed(_)));
        let Some(Select::Where(cond)) = &p.segments[0].select else { panic!("{:?}", p.segments[0]) };
        let Cond::And(parts) = &**cond else { panic!("{cond:?}") };
        assert_eq!(parts.len(), 2);
        assert!(matches!(&parts[0], Cond::Compare(Operand::Bare(_), CompareOp::Eq, Operand::Slot(_))));
        assert!(matches!(&parts[1], Cond::Or(_)));
        assert_eq!(t.pieces.len(), 3);
    }

    #[test]
    fn conditions() {
        assert!(matches!(parse_condition("{flags} & 1").unwrap(), Cond::BitAnd(_, 1)));
        assert!(matches!(parse_condition("{#group} == \"scnr\"").unwrap(), Cond::Compare(_, CompareOp::Eq, Operand::Text(t)) if t == "scnr"));
        assert!(matches!(parse_condition("{object id/source} <= 1").unwrap(), Cond::Compare(_, CompareOp::Le, Operand::Number(n)) if n == 1.0));
        assert!(matches!(parse_condition("starts_with({name}, \"shaders\\\\\")").unwrap(), Cond::StartsWith(_, p) if p == "shaders\\"));
        let any = parse_condition("{constraints|count} == 0 || any(constraints[*], flags & 1)").unwrap();
        let Cond::Or(parts) = any else { panic!() };
        assert!(matches!(&parts[1], Cond::Any(path, inner) if path.segments[0].select == Some(Select::All) && matches!(**inner, Cond::BitAnd(Operand::Bare(_), 1))));
        assert!(matches!(parse_condition("!({a} == 1)").unwrap(), Cond::Not(_)));
    }

    #[test]
    fn errors_say_where() {
        assert!(parse_template("{type|nope}").is_err());
        assert!(parse_template("{type").is_err());
        assert!(parse_template("a } b").is_err());
        assert!(parse_condition("{a} == ").is_err());
    }
}
