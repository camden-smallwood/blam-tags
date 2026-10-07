//! Evaluating a definition's `element_label` entry against a tag.

use std::collections::HashMap;

use super::template::{
    ArithOp, CompareOp, Cond, Expr, Filter, Hash, Operand, Path, PathStart, Piece, Select, SegmentName, Slot,
    Template,
};
use super::{Context, ElementLabels};
use crate::api::{TagBlock, TagField, TagStruct};
use crate::fields::{TagFieldData, TagFieldType};

/// A definition's label entry: its alternatives, in order, and its maps.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Entry {
    /// Where the rule was read from (an editor function).
    pub source: String,
    /// `None`: the definition has a callback no template expresses; it is
    /// labelled by the generic rule.
    pub alternatives: Option<Vec<Alternative>>,
    pub maps: HashMap<String, LabelMap>,
    /// The editor formats into a buffer this many characters long (CE's
    /// unicode string list: 31).
    pub max_length: Option<usize>,
    /// The group file the entry sits in, for `|enum:` names (scoped per file
    /// in Halo 2).
    pub file: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Alternative {
    pub when: Option<Cond>,
    pub template: Template,
    /// `{"join": sep, "parts": [...]}`: the parts that come out non-empty,
    /// joined by `sep`, in place of `template`. Halo 3's chud widget states
    /// join one field's set flags with ` OR ` and the fields with ` AND `.
    pub join: Option<(String, Vec<Template>)>,
}

/// A hand-entered table: `names[value]`, else `values[value]`, else `else`.
#[derive(Debug, Clone, PartialEq, Default)]
pub(super) struct LabelMap {
    pub names: Vec<Option<String>>,
    pub values: HashMap<i64, String>,
    pub otherwise: Option<String>,
}

impl LabelMap {
    fn get(&self, value: i64) -> Option<&str> {
        usize::try_from(value)
            .ok()
            .and_then(|i| self.names.get(i))
            .and_then(Option::as_deref)
            .or_else(|| self.values.get(&value).map(String::as_str))
            .or(self.otherwise.as_deref())
    }
}

/// The element being labelled.
pub(super) struct Here<'a> {
    /// The structs holding the block, root first.
    pub chain: Vec<TagStruct<'a>>,
    pub block: TagBlock<'a>,
    pub index: i64,
    pub element: TagStruct<'a>,
}

/// What a slot or path evaluates to.
#[derive(Clone)]
enum Val<'a> {
    Unset,
    /// A field, with the structs leading to it (the last one holds it).
    Field { field: TagField<'a>, chain: Vec<TagStruct<'a>> },
    /// A block element: its label is the whole rule again.
    Element { chain: Vec<TagStruct<'a>>, block: TagBlock<'a>, index: i64 },
    Number(f64),
    Text(String),
}

impl ElementLabels {
    /// The entry's text for `here`, or `None` when no alternative applies.
    pub(super) fn evaluate_entry(&self, ctx: &Context, entry: &Entry, here: &Here<'_>) -> Option<String> {
        for alternative in entry.alternatives.as_ref()? {
            let applies = match (&alternative.when, &alternative.join) {
                (Some(cond), _) => self.holds(ctx, entry, here, &self.element_base(here), cond),
                (None, Some(_)) => true,
                (None, None) => self.all_slots_set(ctx, entry, here, &alternative.template),
            };
            if applies {
                let mut text = match &alternative.join {
                    Some((separator, parts)) => parts
                        .iter()
                        .map(|part| self.render_template(ctx, entry, here, part))
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(separator),
                    None => self.render_template(ctx, entry, here, &alternative.template),
                };
                if let Some(max) = entry.max_length
                    && let Some((cut, _)) = text.char_indices().nth(max)
                {
                    text.truncate(cut);
                }
                return Some(text);
            }
        }
        None
    }

    fn element_base<'a>(&self, here: &Here<'a>) -> Vec<TagStruct<'a>> {
        let mut base = here.chain.clone();
        base.push(here.element);
        base
    }

    fn render_template(&self, ctx: &Context, entry: &Entry, here: &Here<'_>, template: &Template) -> String {
        let base = self.element_base(here);
        let mut out = String::new();
        for piece in &template.pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Slot(slot) => {
                    let value = self.slot_value(ctx, entry, here, &base, slot);
                    out.push_str(&self.render(ctx, entry, &value, slot));
                }
            }
        }
        out
    }

    fn all_slots_set(&self, ctx: &Context, entry: &Entry, here: &Here<'_>, template: &Template) -> bool {
        let base = self.element_base(here);
        template.pieces.iter().all(|piece| match piece {
            Piece::Text(_) => true,
            Piece::Slot(slot) => self.is_set(&self.slot_value(ctx, entry, here, &base, slot), slot),
        })
    }

    fn slot_value<'a>(&self, ctx: &Context, entry: &Entry, here: &Here<'a>, base: &[TagStruct<'a>], slot: &Slot) -> Val<'a> {
        self.expr_value(ctx, entry, here, base, &slot.expr)
    }

    fn expr_value<'a>(&self, ctx: &Context, entry: &Entry, here: &Here<'a>, base: &[TagStruct<'a>], expr: &Expr) -> Val<'a> {
        match expr {
            Expr::Number(n) => Val::Number(*n),
            Expr::Hash(hash) => {
                let count = here.block.len() as f64;
                match hash {
                    Hash::Index => Val::Number(here.index as f64),
                    Hash::Index1 => Val::Number(here.index as f64 + 1.0),
                    Hash::Count => Val::Number(count),
                    // CE's look function: a one-element block is at 1.0.
                    Hash::Fraction => Val::Number(if count > 1.0 { here.index as f64 / (count - 1.0) } else { 1.0 }),
                    Hash::Block => Val::Text(here.block.definition().name().to_owned()),
                    Hash::Group => match ctx.group {
                        Some(group) => Val::Text(group.to_be_bytes().iter().map(|&b| b as char).collect()),
                        None => Val::Unset,
                    },
                }
            }
            Expr::Binary(left, op, right) => {
                let left = self.expr_value(ctx, entry, here, base, left);
                let right = self.expr_value(ctx, entry, here, base, right);
                match (number(&left), number(&right)) {
                    (Some(a), Some(b)) => Val::Number(match op {
                        ArithOp::Add => a + b,
                        ArithOp::Sub => a - b,
                        ArithOp::Mul => a * b,
                        ArithOp::Div => a / b,
                    }),
                    _ => Val::Unset,
                }
            }
            Expr::Path(path) => self.path_values(ctx, entry, here, base, path).into_iter().next().unwrap_or(Val::Unset),
        }
    }

    /// Every value `path` reaches (more than one only through `[*]`).
    fn path_values<'a>(&self, ctx: &Context, entry: &Entry, here: &Here<'a>, base: &[TagStruct<'a>], path: &Path) -> Vec<Val<'a>> {
        if path.then.is_some() {
            // Following a reference into another tag needs a way to open it,
            // which the context doesn't carry yet.
            return Vec::new();
        }
        let start: Vec<TagStruct<'a>> = match path.start {
            PathStart::Element => base.to_vec(),
            PathStart::Root => here.chain.first().copied().or(Some(here.element)).into_iter().collect(),
            PathStart::Parent(n) => {
                let keep = here.chain.len().saturating_sub(n - 1);
                if keep == 0 { return Vec::new(); }
                here.chain[..keep].to_vec()
            }
        };
        let mut out = Vec::new();
        self.walk(ctx, entry, here, start, &path.segments, &mut out);
        out
    }

    fn walk<'a>(
        &self,
        ctx: &Context,
        entry: &Entry,
        here: &Here<'a>,
        holders: Vec<TagStruct<'a>>,
        segments: &[super::template::Segment],
        out: &mut Vec<Val<'a>>,
    ) {
        let Some((segment, rest)) = segments.split_first() else { return };
        let Some(&holder) = holders.last() else { return };
        let name = match &segment.name {
            SegmentName::Literal(name) => name.clone(),
            SegmentName::Computed(slot) => {
                // A `{slot}` always refers to the element being labelled.
                let value = self.slot_value(ctx, entry, here, &self.element_base(here), slot);
                if !self.is_set(&value, slot) {
                    return;
                }
                self.render(ctx, entry, &value, slot)
            }
        };
        let Some(field) = find_field(holder, &name) else { return };
        let last = rest.is_empty();
        let Some(select) = &segment.select else {
            if last {
                out.push(Val::Field { field, chain: holders });
            } else if let Some(inner) = field.as_struct() {
                let mut next = holders;
                next.push(inner);
                self.walk(ctx, entry, here, next, rest, out);
            }
            return;
        };
        let block = field.as_block();
        let len = match (&block, field.as_array()) {
            (Some(block), _) => block.len(),
            (None, Some(array)) => array.len(),
            (None, None) => {
                // `{bounds[0]}`: one component of a multi-value field.
                if let (true, Select::Literal(i)) = (last, select)
                    && let Some(value) = field.value().and_then(|v| component(&v, *i))
                {
                    out.push(Val::Number(value));
                }
                return;
            }
        };
        let element = |index: usize| match (&block, field.as_array()) {
            (Some(block), _) => block.element(index),
            (None, Some(array)) => array.element(index),
            (None, None) => None,
        };
        let indices: Vec<i64> = match select {
            Select::ThisIndex => vec![here.index],
            Select::Literal(n) => vec![*n],
            Select::Slot(slot) => {
                let value = self.slot_value(ctx, entry, here, &self.element_base(here), slot);
                match number(&value) {
                    Some(n) => vec![n as i64],
                    None => Vec::new(),
                }
            }
            Select::All => (0..len as i64).collect(),
            Select::Where(cond) => (0..len)
                .filter(|&i| {
                    element(i).is_some_and(|candidate| {
                        let mut candidate_base = holders.clone();
                        candidate_base.push(candidate);
                        self.holds(ctx, entry, here, &candidate_base, cond)
                    })
                })
                .take(1)
                .map(|i| i as i64)
                .collect(),
        };
        for index in indices {
            let Some(item) = usize::try_from(index).ok().and_then(element) else { continue };
            if last {
                if let Some(block) = block {
                    out.push(Val::Element { chain: holders.clone(), block, index });
                }
            } else {
                let mut next = holders.clone();
                next.push(item);
                self.walk(ctx, entry, here, next, rest, out);
            }
        }
    }

    fn holds(&self, ctx: &Context, entry: &Entry, here: &Here<'_>, base: &[TagStruct<'_>], cond: &Cond) -> bool {
        match cond {
            Cond::Or(parts) => parts.iter().any(|c| self.holds(ctx, entry, here, base, c)),
            Cond::And(parts) => parts.iter().all(|c| self.holds(ctx, entry, here, base, c)),
            Cond::Not(inner) => !self.holds(ctx, entry, here, base, inner),
            Cond::BitAnd(operand, mask) => self
                .operand_number(ctx, entry, here, base, operand)
                .is_some_and(|n| (n as i64) & mask != 0),
            Cond::Truthy(operand) => match operand {
                Operand::Slot(slot) => {
                    let value = self.slot_value(ctx, entry, here, &self.element_base(here), slot);
                    self.is_set(&value, slot) && number(&value).is_none_or(|n| n != 0.0)
                }
                other => self.operand_number(ctx, entry, here, base, other).is_some_and(|n| n != 0.0),
            },
            Cond::StartsWith(operand, prefix) => {
                self.operand_text(ctx, entry, here, base, operand).is_some_and(|t| t.starts_with(prefix.as_str()))
            }
            Cond::Any(path, inner) => self.path_values(ctx, entry, here, base, path).into_iter().any(|value| {
                let Val::Element { chain, block, index } = value else { return false };
                let Some(candidate) = usize::try_from(index).ok().and_then(|i| block.element(i)) else { return false };
                let mut candidate_base = chain;
                candidate_base.push(candidate);
                self.holds(ctx, entry, here, &candidate_base, inner)
            }),
            Cond::Compare(left, op, right) => {
                let textual = matches!(left, Operand::Text(_)) || matches!(right, Operand::Text(_))
                    || self.operand_is_textual(left) || self.operand_is_textual(right);
                if textual {
                    let (Some(a), Some(b)) = (
                        self.operand_text(ctx, entry, here, base, left),
                        self.operand_text(ctx, entry, here, base, right),
                    ) else {
                        return false;
                    };
                    match op {
                        CompareOp::Eq => a == b,
                        CompareOp::Ne => a != b,
                        CompareOp::Lt => a < b,
                        CompareOp::Le => a <= b,
                        CompareOp::Gt => a > b,
                        CompareOp::Ge => a >= b,
                    }
                } else {
                    let (Some(a), Some(b)) = (
                        self.operand_number(ctx, entry, here, base, left),
                        self.operand_number(ctx, entry, here, base, right),
                    ) else {
                        return false;
                    };
                    match op {
                        CompareOp::Eq => a == b,
                        CompareOp::Ne => a != b,
                        CompareOp::Lt => a < b,
                        CompareOp::Le => a <= b,
                        CompareOp::Gt => a > b,
                        CompareOp::Ge => a >= b,
                    }
                }
            }
        }
    }

    /// A slot whose filters produce text compares as text.
    fn operand_is_textual(&self, operand: &Operand) -> bool {
        match operand {
            Operand::Slot(slot) => slot.filters.iter().any(|f| !matches!(f, Filter::Index | Filter::Count))
                || matches!(slot.expr, Expr::Hash(Hash::Block | Hash::Group)),
            _ => false,
        }
    }

    fn operand_value<'a>(&self, ctx: &Context, entry: &Entry, here: &Here<'a>, base: &[TagStruct<'a>], operand: &Operand) -> (Val<'a>, Option<Slot>) {
        match operand {
            // A `{slot}` is the element being labelled; a bare path is the
            // candidate element of a predicate or `any()`.
            Operand::Slot(slot) => (self.slot_value(ctx, entry, here, &self.element_base(here), slot), Some(slot.clone())),
            Operand::Bare(path) => {
                (self.path_values(ctx, entry, here, base, path).into_iter().next().unwrap_or(Val::Unset), None)
            }
            Operand::Number(n) => (Val::Number(*n), None),
            Operand::Text(t) => (Val::Text(t.clone()), None),
        }
    }

    fn operand_number(&self, ctx: &Context, entry: &Entry, here: &Here<'_>, base: &[TagStruct<'_>], operand: &Operand) -> Option<f64> {
        let (value, slot) = self.operand_value(ctx, entry, here, base, operand);
        match slot {
            Some(slot) if slot.filters.contains(&Filter::Count) => match value {
                Val::Field { field, .. } => field.as_block().map(|b| b.len() as f64),
                _ => None,
            },
            _ => number(&value),
        }
    }

    fn operand_text(&self, ctx: &Context, entry: &Entry, here: &Here<'_>, base: &[TagStruct<'_>], operand: &Operand) -> Option<String> {
        let (value, slot) = self.operand_value(ctx, entry, here, base, operand);
        if matches!(value, Val::Unset) {
            return None;
        }
        let slot = slot.unwrap_or(Slot { expr: Expr::Number(0.0), format: None, filters: Vec::new() });
        Some(self.render(ctx, entry, &value, &slot))
    }

    /// Whether a slot is set, for a plain-string alternative: a block index
    /// that isn't −1 and is in range, a non-empty string or data, a tag
    /// reference with a path. A slot with `|none:`/`|bad:` is always set.
    fn is_set(&self, value: &Val<'_>, slot: &Slot) -> bool {
        if slot.filters.iter().any(|f| matches!(f, Filter::None(_) | Filter::Bad(_))) {
            return true;
        }
        match value {
            Val::Unset => false,
            Val::Element { .. } | Val::Number(_) | Val::Text(_) => true,
            Val::Field { field, chain } => {
                if slot.filters.contains(&Filter::Index) || slot.filters.contains(&Filter::Count) {
                    return true;
                }
                match field.value() {
                    Some(TagFieldData::String(s) | TagFieldData::LongString(s)) => !s.is_empty(),
                    Some(TagFieldData::StringId(id) | TagFieldData::OldStringId(id)) => !id.string.is_empty(),
                    Some(TagFieldData::Data(bytes)) => !bytes.is_empty(),
                    Some(TagFieldData::TagReference(reference)) => {
                        reference.group_tag_and_name.as_ref().is_some_and(|(_, path)| !path.is_empty())
                    }
                    Some(value) => match block_index_value(&value) {
                        Some(index) => index != -1 && self.block_index_target(chain, *field, index).is_some(),
                        None => true,
                    },
                    None => field.field_type() != TagFieldType::OldStringId
                        || super::legacy_inline_old_string_id(*field, chain.last()).is_some(),
                }
            }
        }
    }

    fn block_index_target<'a>(&self, chain: &[TagStruct<'a>], field: TagField<'_>, index: i64) -> Option<(Vec<TagStruct<'a>>, TagBlock<'a>)> {
        let (depth, block) = super::find_target_block(chain, field)?;
        let element = usize::try_from(index).ok().and_then(|i| block.element(i));
        element.map(|_| (chain[..=depth].to_vec(), block))
    }

    fn render(&self, ctx: &Context, entry: &Entry, value: &Val<'_>, slot: &Slot) -> String {
        // Filters that read the field itself rather than its formatted value.
        if let Val::Field { field, chain } = value {
            for filter in &slot.filters {
                match filter {
                    Filter::Count => return field.as_block().map_or(0, |b| b.len()).to_string(),
                    Filter::Join(sep) => {
                        let Some(block) = field.as_block() else { return String::new() };
                        return (0..block.len() as i64)
                            .map(|i| self.label_in(ctx, chain, block, i))
                            .collect::<Vec<_>>()
                            .join(sep);
                    }
                    Filter::Flags(sep) => {
                        let names = match field.value() {
                            Some(TagFieldData::ByteFlags { names, .. } | TagFieldData::WordFlags { names, .. } | TagFieldData::LongFlags { names, .. }) => names,
                            _ => Vec::new(),
                        };
                        let mut names = names;
                        names.sort_by_key(|(bit, _)| *bit);
                        return names.into_iter().map(|(_, n)| n).collect::<Vec<_>>().join(sep);
                    }
                    Filter::Path | Filter::FileExt | Filter::Group => {
                        let Some(TagFieldData::TagReference(reference)) = field.value() else { return String::new() };
                        let Some((group, path)) = reference.group_tag_and_name else { return String::new() };
                        let extension = self.group_extension(group);
                        return match filter {
                            Filter::Path => path,
                            Filter::FileExt => format!("{}.{extension}", super::file_name(&path)),
                            _ => extension,
                        };
                    }
                    Filter::Text | Filter::Utf16 => {
                        let Some(bytes) = field.as_data() else { return String::new() };
                        return if *filter == Filter::Text {
                            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                            // The editors print the bytes as they are.
                            bytes[..end].iter().map(|&b| b as char).collect()
                        } else {
                            let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                            String::from_utf16_lossy(&units)
                        };
                    }
                    Filter::None(_) | Filter::Bad(_) => {
                        // `|none:` / `|bad:` replace a block index's NONE and
                        // BAD text; otherwise the slot formats as usual.
                        let Some(index) = field.value().as_ref().and_then(block_index_value) else { continue };
                        let wanted = if index == -1 {
                            slot.filters.iter().find_map(|f| match f { Filter::None(t) => Some(t), _ => None })
                        } else if self.block_index_target(chain, *field, index).is_none() {
                            slot.filters.iter().find_map(|f| match f { Filter::Bad(t) => Some(t), _ => None })
                        } else {
                            None
                        };
                        if let Some(text) = wanted {
                            return text.clone();
                        }
                    }
                    _ => {}
                }
            }
        }
        // Lookups on the raw value.
        for filter in &slot.filters {
            match filter {
                Filter::Map(name) => {
                    let Some(n) = number(value) else { return String::new() };
                    let map = entry.maps.get(name).or_else(|| self.shared_maps.get(name));
                    return map.and_then(|map| map.get(n as i64)).unwrap_or_default().to_owned();
                }
                Filter::Enum(name) => {
                    let Some(n) = number(value) else { return String::new() };
                    return self.enum_option(&entry.file, name, n as i64).unwrap_or_default();
                }
                Filter::Index => {
                    if let Some(n) = number(value) {
                        return printf_number(slot.format.as_deref(), n);
                    }
                }
                _ => {}
            }
        }
        let mut text = match value {
            Val::Unset => String::new(),
            Val::Text(text) => text.clone(),
            Val::Number(n) => printf_number(slot.format.as_deref(), *n),
            Val::Element { chain, block, index } => self.label_in(ctx, chain, *block, *index),
            Val::Field { field, chain } => match (&slot.format, number(value)) {
                (Some(format), Some(n)) => printf_number(Some(format), n),
                _ => self.format_value(ctx, chain, *field),
            },
        };
        if slot.filters.contains(&Filter::NoAlias)
            && let Some(open) = text.find('{')
        {
            text.truncate(open);
        }
        text
    }
}

/// Component `i` of a multi-value field: bounds (lower, upper), points and
/// vectors (x/i, y/j, z/k, w), planes, colors and euler angles, in storage
/// order.
fn component(value: &TagFieldData, i: i64) -> Option<f64> {
    use TagFieldData as D;
    let parts: Vec<f64> = match value {
        D::Point2d(p) => vec![p.x as f64, p.y as f64],
        D::Rectangle2d(r) => vec![r.top as f64, r.left as f64, r.bottom as f64, r.right as f64],
        D::ShortIntegerBounds(b) => vec![b.lower as f64, b.upper as f64],
        D::AngleBounds(b) | D::RealBounds(b) | D::FractionBounds(b) => vec![b.lower as f64, b.upper as f64],
        D::RealPoint2d(p) => vec![p.x as f64, p.y as f64],
        D::RealPoint3d(p) => vec![p.x as f64, p.y as f64, p.z as f64],
        D::RealVector2d(v) => vec![v.i as f64, v.j as f64],
        D::RealVector3d(v) => vec![v.i as f64, v.j as f64, v.k as f64],
        D::RealQuaternion(q) => vec![q.i as f64, q.j as f64, q.k as f64, q.w as f64],
        D::RealEulerAngles2d(e) => vec![e.yaw as f64, e.pitch as f64],
        D::RealEulerAngles3d(e) => vec![e.yaw as f64, e.pitch as f64, e.roll as f64],
        D::RealPlane2d(p) => vec![p.i as f64, p.j as f64, p.d as f64],
        D::RealPlane3d(p) => vec![p.i as f64, p.j as f64, p.k as f64, p.d as f64],
        D::RealRgbColor(c) => vec![c.red as f64, c.green as f64, c.blue as f64],
        D::RealArgbColor(c) => vec![c.alpha as f64, c.red as f64, c.green as f64, c.blue as f64],
        D::RealHsvColor(c) => vec![c.hue as f64, c.saturation as f64, c.value as f64],
        D::RealAhsvColor(c) => vec![c.alpha as f64, c.hue as f64, c.saturation as f64, c.value as f64],
        _ => return None,
    };
    parts.get(usize::try_from(i).ok()?).copied()
}

fn block_index_value(value: &TagFieldData) -> Option<i64> {
    use TagFieldData as D;
    Some(match *value {
        D::CharBlockIndex(v) | D::CustomCharBlockIndex(v) => v as i64,
        D::ShortBlockIndex(v) | D::CustomShortBlockIndex(v) => v as i64,
        D::LongBlockIndex(v) | D::CustomLongBlockIndex(v) => v as i64,
        _ => return None,
    })
}

/// A value as a number, where it has one: integers, reals, enum and flag
/// values, block indices.
fn number(value: &Val<'_>) -> Option<f64> {
    use TagFieldData as D;
    match value {
        Val::Number(n) => Some(*n),
        Val::Text(t) => t.trim().parse().ok(),
        Val::Element { index, .. } => Some(*index as f64),
        Val::Unset => None,
        Val::Field { field, .. } => Some(match field.value()? {
            D::CharInteger(v) => v as f64,
            D::ShortInteger(v) => v as f64,
            D::LongInteger(v) => v as f64,
            D::Int64Integer(v) => v as f64,
            D::ByteInteger(v) => v as f64,
            D::WordInteger(v) => v as f64,
            D::DwordInteger(v) => v as f64,
            D::QwordInteger(v) => v as f64,
            D::Tag(v) => v as f64,
            D::CharEnum { value, .. } => value as f64,
            D::ShortEnum { value, .. } => value as f64,
            D::LongEnum { value, .. } => value as f64,
            D::ByteFlags { value, .. } => value as f64,
            D::WordFlags { value, .. } => value as f64,
            D::LongFlags { value, .. } => value as f64,
            D::ByteBlockFlags(v) => v as f64,
            D::WordBlockFlags(v) => v as f64,
            D::LongBlockFlags(v) => v as f64,
            D::Angle(v) | D::Real(v) | D::RealSlider(v) | D::RealFraction(v) => v as f64,
            ref other => block_index_value(other)? as f64,
        }),
    }
}

/// The first field of `holder` whose clean name is `name`.
fn find_field<'a>(holder: TagStruct<'a>, name: &str) -> Option<TagField<'a>> {
    holder.fields_all().find(|field| field.clean_name() == name)
}

/// One printf conversion (`.2f`, `04x`, `+2.0f`, `3d`, `3`), or `mb` (a byte
/// count as `%.2f` megabytes). With no format, an integral value prints as
/// an integer and any other as `%.6g`.
pub(super) fn printf_number(format: Option<&str>, value: f64) -> String {
    let Some(spec) = format else {
        return if value.fract() == 0.0 && value.abs() < 1e15 { format!("{}", value as i64) } else { super::g6(value) };
    };
    if spec == "mb" {
        return format!("{:.2}", value / 1_048_576.0);
    }
    let mut chars = spec.chars().peekable();
    let (mut plus, mut left, mut zero, mut space) = (false, false, false, false);
    while let Some(&c) = chars.peek() {
        match c {
            '+' => plus = true,
            '-' => left = true,
            '0' => zero = true,
            ' ' => space = true,
            _ => break,
        }
        chars.next();
    }
    let mut width = String::new();
    while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit()) {
        width.push(c);
        chars.next();
    }
    let mut precision = None;
    if chars.peek() == Some(&'.') {
        chars.next();
        let mut digits = String::new();
        while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit()) {
            digits.push(c);
            chars.next();
        }
        precision = Some(digits.parse::<usize>().unwrap_or(0));
    }
    let conversion = chars.next().unwrap_or(if value.fract() == 0.0 { 'd' } else { 'g' });
    let mut body = match conversion {
        'd' | 'i' => format!("{}", value as i64),
        'x' => format!("{:x}", value as i64),
        'X' => format!("{:X}", value as i64),
        'f' | 'F' => format!("{:.*}", precision.unwrap_or(6), value),
        'e' => format!("{:.*e}", precision.unwrap_or(6), value),
        _ => super::g6(value),
    };
    let negative = body.starts_with('-');
    if !negative && (plus || space) && matches!(conversion, 'd' | 'i' | 'f' | 'F' | 'e' | 'g') {
        body.insert(0, if plus { '+' } else { ' ' });
    }
    let width: usize = width.parse().unwrap_or(0);
    if body.len() < width {
        let pad = width - body.len();
        if left {
            body.push_str(&" ".repeat(pad));
        } else if zero {
            let sign = body.starts_with(['+', '-', ' ']) as usize;
            body.insert_str(sign, &"0".repeat(pad));
        } else {
            body.insert_str(0, &" ".repeat(pad));
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::printf_number;

    #[test]
    fn printf_matches_c() {
        assert_eq!(printf_number(Some("04x"), 255.0), "00ff");
        assert_eq!(printf_number(Some(".2f"), 1.005), format!("{:.2}", 1.005));
        assert_eq!(printf_number(Some("3.1f"), 2.25), "2.2");
        assert_eq!(printf_number(Some("+2.0f"), 50.0), "+50");
        assert_eq!(printf_number(Some("+2.0f"), -5.0), "-5");
        assert_eq!(printf_number(Some("3d"), 7.0), "  7");
        assert_eq!(printf_number(Some("3"), 7.0), "  7");
        assert_eq!(printf_number(Some("mb"), 3_145_728.0), "3.00");
        assert_eq!(printf_number(None, 4.0), "4");
        assert_eq!(printf_number(None, 0.5), "0.5");
    }
}
