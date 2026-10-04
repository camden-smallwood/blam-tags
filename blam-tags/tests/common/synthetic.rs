//! Synthetic tags, built from the definitions and filled with values.
//!
//! Real tag files can't be committed, so the round-trip and mutation suites
//! make their own: a new tag of every group, with elements added to its
//! blocks and every leaf field set to a value that differs from the zero
//! fill — strings, string ids, tag references, data, enums and flags within
//! their declared options, block indices, every numeric and math type.
//! Everything is derived from a counter, so a tag is the same on every run.

use blam_tags::classic::ClassicEngine;
use blam_tags::math::{
    ArgbColor, Bounds, Point2d, RealAhsvColor, RealArgbColor, RealEulerAngles2d, RealEulerAngles3d,
    RealHsvColor, RealPlane2d, RealPlane3d, RealPoint2d, RealPoint3d, RealQuaternion,
    RealRgbColor, RealVector2d, RealVector3d, Rectangle2d, RgbColor,
};
use blam_tags::{
    ApiInteropData, StringIdData, TagFieldData, TagFieldMut, TagFieldType, TagFile,
    TagReferenceData, TagStruct, TagStructMut,
};
use std::collections::BTreeMap;

/// The classic engine a definitions folder's tags are written in, or `None`
/// for the MCC (gen3+) games.
pub fn classic_engine(game: &str) -> Option<ClassicEngine> {
    match game {
        "haloce_mcc" => Some(ClassicEngine::HaloCe),
        "halo2_mcc" => Some(ClassicEngine::Halo2V4),
        _ => None,
    }
}

/// `(fourcc, group name)` for every group a game indexes, in index order.
pub fn groups(game: &str) -> Vec<(String, String)> {
    let path = super::definitions(game).join("_meta.json");
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    meta["tag_index"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(tag, name)| (tag.clone(), name.as_str().unwrap().to_owned()))
        .collect()
}

/// A new tag of `group` in `game`'s format: `TagFile::new` for the MCC games,
/// `TagFile::new_classic` for Halo CE and Halo 2.
pub fn new_tag(game: &str, group: &str) -> TagFile {
    let path = super::definitions(game).join(format!("{group}.json"));
    match classic_engine(game) {
        Some(engine) => TagFile::new_classic(&path, engine)
            .unwrap_or_else(|e| panic!("{game} {group}: new classic tag: {e}")),
        None => TagFile::new(&path).unwrap_or_else(|e| panic!("{game} {group}: new tag: {e}")),
    }
}

/// Read bytes back the way they were written: the MCC reader, or the
/// classic reader with the group's own layout.
pub fn read_back(game: &str, group: &str, bytes: &[u8]) -> Result<TagFile, String> {
    match classic_engine(game) {
        Some(_) => {
            let path = super::definitions(game).join(format!("{group}.json"));
            let layout = blam_tags::TagLayout::from_json(&path).map_err(|e| e.to_string())?;
            blam_tags::classic::read_classic_tag_file(bytes, layout).map_err(|e| e.to_string())
        }
        None => TagFile::read_from_bytes(bytes).map_err(|e| e.to_string()),
    }
}

/// What a fill did, so a suite can check it did something.
#[derive(Debug, Default)]
pub struct FillStats {
    /// Block elements added.
    pub elements: usize,
    /// Leaf fields given a value, by type.
    pub set: BTreeMap<&'static str, usize>,
    /// Leaf fields with no value to set (opaque raw-only types), by type.
    pub unset: BTreeMap<&'static str, usize>,
    /// Pageable resources given a payload.
    pub resources: usize,
}

/// How far a fill goes. Elements per block shrink with depth, and a tag stops
/// growing once it holds `budget` elements, which keeps a scenario to a few
/// hundred kilobytes.
#[derive(Debug, Clone, Copy)]
pub struct FillPlan {
    pub per_block: [usize; 4],
    pub budget: usize,
}

impl Default for FillPlan {
    fn default() -> Self {
        Self { per_block: [2, 2, 1, 1], budget: 300 }
    }
}

struct Filler {
    plan: FillPlan,
    counter: u64,
    group: u32,
    stats: FillStats,
}

/// Fill `tag` according to `plan`.
pub fn fill(tag: &mut TagFile, plan: FillPlan) -> FillStats {
    let mut filler = Filler { plan, counter: 1, group: tag.header.group_tag, stats: FillStats::default() };
    let mut root = tag.root_mut();
    filler.fill_struct(&mut root, 0);
    filler.stats
}

impl Filler {
    fn next(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    fn fill_struct(&mut self, element: &mut TagStructMut<'_>, depth: usize) {
        element.for_each_field_mut(|mut field| self.fill_field(&mut field, depth));
    }

    fn fill_field(&mut self, field: &mut TagFieldMut<'_>, depth: usize) {
        match field.as_ref().field_type() {
            TagFieldType::Struct => {
                if let Some(mut child) = field.as_struct_mut() {
                    self.fill_struct(&mut child, depth);
                }
            }
            TagFieldType::Array => {
                if let Some(mut array) = field.as_array_mut() {
                    array.for_each_element_mut(|mut element| self.fill_struct(&mut element, depth));
                }
            }
            TagFieldType::Block => {
                let Some(mut block) = field.as_block_mut() else { return };
                let wanted = self.plan.per_block.get(depth).copied().unwrap_or(0);
                let max = block.definition().max_count() as usize;
                let room = self.plan.budget.saturating_sub(self.stats.elements);
                let count = wanted.min(max).min(room);
                for _ in 0..count {
                    block.add_element();
                }
                self.stats.elements += count;
                block.for_each_element_mut(|mut element| self.fill_struct(&mut element, depth + 1));
            }
            TagFieldType::PageableResource => {
                if depth >= self.plan.per_block.len() || field.init_resource().is_err() {
                    return;
                }
                self.stats.resources += 1;
                if let Some(mut payload) = field.as_resource_struct_mut() {
                    self.fill_struct(&mut payload, depth + 1);
                }
            }
            _ => self.fill_leaf(field),
        }
    }

    fn fill_leaf(&mut self, field: &mut TagFieldMut<'_>) {
        let ty = field.as_ref().field_type();
        let Some(current) = field.as_ref().value() else {
            *self.stats.unset.entry(ty.name()).or_default() += 1;
            return;
        };
        let options = field.as_ref().definition().option_names().count() as u64;
        let n = self.next();
        let i = n as i64;
        let f = n as f32 * 0.25 + 0.125;
        let flags = |width: u32| -> u64 {
            // Alternate bits, inside the declared ones when there are any.
            let declared = if options == 0 { u64::from(width) } else { options.min(u64::from(width)) };
            let mask = if declared >= 64 { u64::MAX } else { (1u64 << declared) - 1 };
            (0x5555_5555_5555_5555u64 >> (n % 2)) & mask
        };
        let enumerated = if options == 0 { 0 } else { (n % options) as i64 };
        let bounds = Bounds { lower: f, upper: f + 1.0 };
        use TagFieldData as D;
        let value = match current {
            D::String(_) => D::String(format!("s{n}")),
            D::LongString(_) => D::LongString(format!("long string {n}")),
            D::StringId(_) => D::StringId(StringIdData { string: format!("sid_{n}") }),
            D::OldStringId(_) => D::OldStringId(StringIdData { string: format!("old_sid_{n}") }),
            D::TagReference(_) => D::TagReference(TagReferenceData {
                group_tag_and_name: Some((self.group, format!("synthetic\\reference_{n}"))),
            }),
            D::Data(_) => D::Data((0..(1 + n % 7)).map(|b| (n + b) as u8).collect()),
            D::ApiInterop(old) => D::ApiInterop(ApiInteropData {
                raw: (0..old.raw.len()).map(|b| (n as usize + b) as u8).collect(),
                endian: old.endian,
            }),
            D::CharInteger(_) => D::CharInteger(i as i8),
            D::ShortInteger(_) => D::ShortInteger(i as i16),
            D::LongInteger(_) => D::LongInteger((i as i32).wrapping_mul(1001)),
            D::Int64Integer(_) => D::Int64Integer(i * 1_000_003),
            D::ByteInteger(_) => D::ByteInteger(n as u8),
            D::WordInteger(_) => D::WordInteger(n as u16),
            D::DwordInteger(_) => D::DwordInteger((n as u32).wrapping_mul(7919)),
            D::QwordInteger(_) => D::QwordInteger(n * 104_729),
            D::Tag(_) => D::Tag(u32::from_be_bytes(*b"syn0") + (n % 10) as u32),
            D::CharEnum { .. } => D::CharEnum { value: enumerated as i8, name: None },
            D::ShortEnum { .. } => D::ShortEnum { value: enumerated as i16, name: None },
            D::LongEnum { .. } => D::LongEnum { value: enumerated as i32, name: None },
            D::ByteFlags { .. } => D::ByteFlags { value: flags(8) as u8, names: Vec::new() },
            D::WordFlags { .. } => D::WordFlags { value: flags(16) as u16, names: Vec::new() },
            D::LongFlags { .. } => D::LongFlags { value: flags(32) as i32, names: Vec::new() },
            D::ByteBlockFlags(_) => D::ByteBlockFlags(flags(8) as u8),
            D::WordBlockFlags(_) => D::WordBlockFlags(flags(16) as u16),
            D::LongBlockFlags(_) => D::LongBlockFlags(flags(32) as i32),
            // Element 0 or NONE: both exist in any block the index names.
            D::CharBlockIndex(_) => D::CharBlockIndex(-((n % 2) as i8)),
            D::CustomCharBlockIndex(_) => D::CustomCharBlockIndex(-((n % 2) as i8)),
            D::ShortBlockIndex(_) => D::ShortBlockIndex(-((n % 2) as i16)),
            D::CustomShortBlockIndex(_) => D::CustomShortBlockIndex(-((n % 2) as i16)),
            D::LongBlockIndex(_) => D::LongBlockIndex(-((n % 2) as i32)),
            D::CustomLongBlockIndex(_) => D::CustomLongBlockIndex(-((n % 2) as i32)),
            D::Angle(_) => D::Angle(f),
            D::Real(_) => D::Real(f),
            D::RealSlider(_) => D::RealSlider(f),
            D::RealFraction(_) => D::RealFraction(1.0 / (1.0 + f)),
            D::Point2d(_) => D::Point2d(Point2d { x: i as i16, y: -(i as i16) }),
            D::Rectangle2d(_) => {
                D::Rectangle2d(Rectangle2d { top: 1, left: 2, bottom: i as i16, right: i as i16 + 3 })
            }
            D::RealPoint2d(_) => D::RealPoint2d(RealPoint2d { x: f, y: -f }),
            D::RealPoint3d(_) => D::RealPoint3d(RealPoint3d { x: f, y: -f, z: f * 2.0 }),
            D::RealVector2d(_) => D::RealVector2d(RealVector2d { i: f, j: 0.5 }),
            D::RealVector3d(_) => D::RealVector3d(RealVector3d { i: 0.5, j: f, k: -0.5 }),
            D::RealQuaternion(_) => D::RealQuaternion(RealQuaternion { i: 0.5, j: -0.5, k: 0.5, w: 0.5 }),
            D::RealEulerAngles2d(_) => D::RealEulerAngles2d(RealEulerAngles2d { yaw: f, pitch: -f }),
            D::RealEulerAngles3d(_) => {
                D::RealEulerAngles3d(RealEulerAngles3d { yaw: f, pitch: -f, roll: f / 2.0 })
            }
            D::RealPlane2d(_) => D::RealPlane2d(RealPlane2d { i: 0.0, j: 1.0, d: f }),
            D::RealPlane3d(_) => D::RealPlane3d(RealPlane3d { i: 0.0, j: 0.0, k: 1.0, d: f }),
            D::RgbColor(_) => D::RgbColor(RgbColor((n as u32).wrapping_mul(0x010203) & 0x00FF_FFFF)),
            D::ArgbColor(_) => D::ArgbColor(ArgbColor((n as u32).wrapping_mul(0x01020304))),
            D::RealRgbColor(_) => D::RealRgbColor(RealRgbColor { red: 0.25, green: 0.5, blue: f }),
            D::RealArgbColor(_) => {
                D::RealArgbColor(RealArgbColor { alpha: 1.0, red: 0.25, green: 0.5, blue: f })
            }
            D::RealHsvColor(_) => D::RealHsvColor(RealHsvColor { hue: f, saturation: 0.5, value: 0.75 }),
            D::RealAhsvColor(_) => {
                D::RealAhsvColor(RealAhsvColor { alpha: 1.0, hue: f, saturation: 0.5, value: 0.75 })
            }
            D::ShortIntegerBounds(_) => D::ShortIntegerBounds(Bounds { lower: i as i16, upper: i as i16 + 9 }),
            D::AngleBounds(_) => D::AngleBounds(bounds),
            D::RealBounds(_) => D::RealBounds(bounds),
            D::FractionBounds(_) => D::FractionBounds(Bounds { lower: 0.25, upper: 0.75 }),
            D::Custom(bytes) => {
                if bytes.is_empty() {
                    return;
                }
                D::Custom(bytes.iter().enumerate().map(|(b, _)| (n as usize + b) as u8).collect())
            }
        };
        match field.set(value) {
            Ok(()) => *self.stats.set.entry(ty.name()).or_default() += 1,
            Err(error) => panic!("set {} ({}): {error:?}", field.as_ref().name(), ty.name()),
        }
    }
}

/// Every value in `tag`, keyed by its position, for comparing two tags
/// field by field: `(path, Debug of the value)`.
pub fn dump(tag: &TagFile) -> Vec<(String, String)> {
    let mut out = Vec::new();
    dump_struct(&tag.root(), String::new(), &mut out);
    out
}

fn dump_struct(element: &TagStruct<'_>, prefix: String, out: &mut Vec<(String, String)>) {
    for (ordinal, field) in element.fields().enumerate() {
        let path = format!("{prefix}/{ordinal}:{}", field.name());
        match field.field_type() {
            TagFieldType::Struct => {
                if let Some(child) = field.as_struct() {
                    dump_struct(&child, path, out);
                }
            }
            TagFieldType::Array => {
                if let Some(array) = field.as_array() {
                    for (i, element) in array.iter().enumerate() {
                        dump_struct(&element, format!("{path}[{i}]"), out);
                    }
                }
            }
            TagFieldType::Block => {
                if let Some(block) = field.as_block() {
                    out.push((format!("{path}#len"), block.len().to_string()));
                    for (i, element) in block.iter().enumerate() {
                        dump_struct(&element, format!("{path}[{i}]"), out);
                    }
                }
            }
            TagFieldType::PageableResource => {
                if let Some(payload) = field.as_resource().and_then(|r| r.as_struct()) {
                    dump_struct(&payload, format!("{path}@"), out);
                }
            }
            _ => out.push((path, format!("{:?}", field.value()))),
        }
    }
}

/// Where a round trip of a filled tag went wrong, or `None` if it didn't.
///
/// Write, read back, write again: the two writes are byte-identical, and the
/// tag that was read holds the values that were written, field by field.
pub fn round_trip(game: &str, group: &str, tag: &TagFile) -> Option<String> {
    let first = match tag.write_to_bytes() {
        Ok(bytes) => bytes,
        Err(e) => return Some(format!("write: {e}")),
    };
    let read = match read_back(game, group, &first) {
        Ok(read) => read,
        Err(e) => return Some(format!("read back: {e}")),
    };
    let second = match read.write_to_bytes() {
        Ok(bytes) => bytes,
        Err(e) => return Some(format!("rewrite: {e}")),
    };
    if first != second {
        let at = first.iter().zip(&second).position(|(a, b)| a != b).unwrap_or(first.len().min(second.len()));
        return Some(format!("rewrite differs at byte {at} ({} vs {} bytes)", first.len(), second.len()));
    }
    let (before, after) = (dump(tag), dump(&read));
    if before != after {
        let at = before.iter().zip(&after).position(|(a, b)| a != b).unwrap_or(before.len().min(after.len()));
        let (a, b) = (before.get(at), after.get(at));
        return Some(format!("value differs: {a:?} became {b:?}"));
    }
    None
}

/// The bytes of a new tag of `group`, filled according to `plan`.
pub fn filled_bytes(game: &str, group: &str, plan: FillPlan) -> Vec<u8> {
    let mut tag = new_tag(game, group);
    fill(&mut tag, plan);
    tag.write_to_bytes().unwrap()
}
