//! Deep structural comparison between two games' layouts for the same tag
//! group, and the layout-index translation a proven-identical pair permits.
//!
//! This is the *recursive* counterpart to [`crate::schema_compare`], which
//! compares root structs only. Both exist because they answer different
//! questions at very different costs. `compare_root_layout` asks "does this tag
//! look like the group we ship?" and is cheap enough to run on every import.
//! This module asks "is this tag's whole shape interchangeable with that one's?"
//! and has to walk the entire struct graph to answer.
//!
//! The distinction is not academic. A Halo Reach `model_animation_graph` and a
//! Campaign Evolved one have root structs that are *field-for-field identical*
//! — same size, same field list, differing only in the text of a zero-byte
//! `explanation` — so the root comparison reports a clean match. Four structs
//! further down they disagree: `shared_model_animation_block` is 212 bytes in
//! Reach and 200 in Campaign Evolved. Anything that copies bytes between the two
//! on the strength of the root comparison produces a tag the simulation reads at
//! the wrong offsets.
//!
//! ## Why identity is decided by walking, not by GUID
//!
//! Struct definitions carry a 16-byte GUID that looks like it should settle
//! this. It does not: `shared_model_animation_block` has the *same* GUID in both
//! games at those two different sizes. The GUID tracks a struct's lineage, not
//! its current shape, so identity has to be established field by field.
//!
//! ## What "wire identical" means
//!
//! Two structs are interchangeable when every field that occupies bytes agrees
//! on its offset, its width, and its *wire class* — the coarse category that
//! decides how the bytes are interpreted. Fields are matched by their cleaned
//! name, positionally within the wire-significant sequence.
//!
//! Comparing wire class and width rather than type *name* is deliberate. It
//! makes `long integer` and `dword integer` — the same four bytes under two
//! spellings, and the only difference in jmad's resource header — compare equal
//! without a hand-maintained table of synonyms, while keeping `real` and
//! `long integer` distinct even though both are four bytes.
//!
//! Zero-byte editor sentinels (`custom`, `explanation`) are skipped: they carry
//! no data and the two toolsets do not agree on how many of them to emit.
//! Padding *is* kept, because it consumes bytes and therefore moves everything
//! after it. Enum and flag option *names* are not compared either: they are
//! editor strings, the simulation reads the stored value, and a shipped tag
//! routinely carries a shorter list than the current schema dump — an HREK
//! animation graph declares 14 user flags where the schema declares 16.
//! Refusing on that would call every real tag incompatible with its own game.
//! Whether the meaning survives is [`compare_group_layouts`]'s question, and it
//! answers with [`FieldVerdict::OptionsLost`].

use std::collections::HashMap;
use std::fmt;

use crate::definition::TagStructDefinition;
use crate::fields::TagFieldType;

/// Source-to-target layout-index translation for a pair of struct trees proven
/// wire-identical.
///
/// Layout table indices are *layout-local*: the same struct sits at a different
/// position in two tags' tables. Data parsed against one layout therefore cannot
/// simply be handed to another — its recorded struct, field and block indices
/// would resolve to different definitions. This map is what makes such a
/// transplant expressible, and [`struct_trees_are_wire_identical`] only produces
/// one once it has proven the transplant is meaningful.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructIndexMap {
    structs: HashMap<usize, usize>,
    fields: HashMap<usize, usize>,
    blocks: HashMap<usize, usize>,
}

impl StructIndexMap {
    /// The target layout's index for a source struct index.
    pub fn struct_index(&self, source: usize) -> Option<usize> {
        self.structs.get(&source).copied()
    }

    /// The target layout's index for a source field index.
    pub fn field_index(&self, source: usize) -> Option<usize> {
        self.fields.get(&source).copied()
    }

    /// The target layout's index for a source block index.
    pub fn block_index(&self, source: usize) -> Option<usize> {
        self.blocks.get(&source).copied()
    }

    /// How many struct pairs the map covers.
    pub fn struct_count(&self) -> usize {
        self.structs.len()
    }
}

/// The first difference that makes two struct trees non-interchangeable.
///
/// Carries the path at which it was found, because on a group like `scenario`
/// the answer "not identical" without a location is not actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireMismatch {
    /// The structs disagree on their declared byte size.
    StructSize { path: String, source: usize, target: usize },
    /// One side declares more wire-significant fields than the other.
    FieldCount { path: String, source: usize, target: usize },
    /// Fields at the same position have different cleaned names.
    FieldName { path: String, source: String, target: String },
    /// Matching fields sit at different byte offsets.
    FieldOffset { path: String, source: u32, target: u32 },
    /// Matching fields are interpreted differently, or occupy different widths.
    WireClass {
        path: String,
        source: String,
        target: String,
    },
    /// Two `data` fields name different data definitions.
    DataDefinition { path: String, source: String, target: String },
    /// A field is a container on one side and not on the other.
    ContainerShape { path: String, source: String, target: String },
    /// Two `api_interop` fields name different runtime types.
    ///
    /// Matching GUIDs are a *layout* agreement only. Whether the payload can be
    /// carried across is a separate question, and one this module does not
    /// answer: an api-interop holds a runtime pointer, so the code that moves
    /// bytes refuses it regardless of what the schemas say.
    ApiInteropGuid { path: String },
    /// A pageable resource nested inside a resource. Not seen in the shipped
    /// corpora; refused rather than guessed at.
    NestedResource { path: String },
}

impl WireMismatch {
    /// Where in the struct tree the difference was found.
    pub fn path(&self) -> &str {
        match self {
            WireMismatch::StructSize { path, .. }
            | WireMismatch::FieldCount { path, .. }
            | WireMismatch::FieldName { path, .. }
            | WireMismatch::FieldOffset { path, .. }
            | WireMismatch::WireClass { path, .. }
            | WireMismatch::DataDefinition { path, .. }
            | WireMismatch::ContainerShape { path, .. }
            | WireMismatch::ApiInteropGuid { path }
            | WireMismatch::NestedResource { path } => path,
        }
    }
}

impl fmt::Display for WireMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireMismatch::StructSize { path, source, target } => {
                write!(f, "{path} is {source} bytes on one side and {target} on the other")
            }
            WireMismatch::FieldCount { path, source, target } => {
                write!(f, "{path} declares {source} fields on one side and {target} on the other")
            }
            WireMismatch::FieldName { path, source, target } => {
                write!(f, "{path}: field `{source}` faces `{target}`")
            }
            WireMismatch::FieldOffset { path, source, target } => {
                write!(f, "{path} sits at offset {source} on one side and {target} on the other")
            }
            WireMismatch::WireClass { path, source, target } => {
                write!(f, "{path} is {source} on one side and {target} on the other")
            }
            WireMismatch::DataDefinition { path, source, target } => {
                write!(f, "{path} names data definition `{source}` on one side and `{target}` on the other")
            }
            WireMismatch::ContainerShape { path, source, target } => {
                write!(f, "{path} is {source} on one side and {target} on the other")
            }
            WireMismatch::ApiInteropGuid { path } => {
                write!(f, "{path} names a different api-interop runtime type on each side")
            }
            WireMismatch::NestedResource { path } => {
                write!(f, "{path} nests a pageable resource inside a resource")
            }
        }
    }
}

impl std::error::Error for WireMismatch {}

/// The coarse category that decides how a field's bytes are interpreted.
///
/// Two fields are interchangeable when their class and width agree — which is
/// how `long integer` and `dword integer` come out equal, and `real` and
/// `long integer` do not, without either being written down as a special case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireClass {
    Integer,
    Real,
    Enum,
    Flags,
    BlockIndex,
    /// A leaf that serializes as its own sub-chunk: string ids, tag references,
    /// `data` payloads.
    LeafChunk(TagFieldType),
    /// Consumes bytes but carries no value.
    Padding,
    /// Recursed into rather than compared directly.
    Container(TagFieldType),
    /// A `tmpl` custom: a fixed-size, unnamed stand-in for another group's
    /// inlined body.
    ///
    /// Identity is the template plus the width, because both have to agree for
    /// the bytes to mean the same thing — the same template can inherit a
    /// different amount in two games, and that is a genuine wire difference
    /// rather than a rename. Without this the hole was simply skipped, which is
    /// why a comparison reported Halo 3's whole render method as absent from
    /// Reach when in fact Reach carries the same 100 bytes unnamed.
    Template { group_tag: u32, size: u32 },
    /// No portable interpretation — compared by exact type instead.
    Opaque(TagFieldType),
}

impl WireClass {
    fn of(field_type: TagFieldType) -> Option<WireClass> {
        use TagFieldType as T;
        Some(match field_type {
            // Zero-byte editor sentinels: no data, and the two toolsets do not
            // agree on how many to emit.
            T::Custom | T::Explanation | T::Terminator | T::Unknown => return None,

            T::CharInteger
            | T::ShortInteger
            | T::LongInteger
            | T::Int64Integer
            | T::ByteInteger
            | T::WordInteger
            | T::DwordInteger
            | T::QwordInteger
            | T::Point2d
            | T::Rectangle2d
            | T::RgbColor
            | T::ArgbColor
            | T::ShortIntegerBounds
            | T::Tag => WireClass::Integer,

            T::Angle
            | T::Real
            | T::RealSlider
            | T::RealFraction
            | T::RealPoint2d
            | T::RealPoint3d
            | T::RealVector2d
            | T::RealVector3d
            | T::RealQuaternion
            | T::RealEulerAngles2d
            | T::RealEulerAngles3d
            | T::RealPlane2d
            | T::RealPlane3d
            | T::RealRgbColor
            | T::RealArgbColor
            | T::RealHsvColor
            | T::RealAhsvColor
            | T::AngleBounds
            | T::RealBounds
            | T::FractionBounds
            | T::RealMatrix3x3 => WireClass::Real,

            T::CharEnum | T::ShortEnum | T::LongEnum => WireClass::Enum,

            T::ByteFlags
            | T::WordFlags
            | T::LongFlags
            | T::ByteBlockFlags
            | T::WordBlockFlags
            | T::LongBlockFlags => WireClass::Flags,

            T::CharBlockIndex
            | T::ShortBlockIndex
            | T::LongBlockIndex
            | T::CustomCharBlockIndex
            | T::CustomShortBlockIndex
            | T::CustomLongBlockIndex => WireClass::BlockIndex,

            other @ (T::String
            | T::LongString
            | T::StringId
            | T::OldStringId
            | T::TagReference
            | T::Data) => WireClass::LeafChunk(other),

            T::Pad | T::UselessPad | T::Skip => WireClass::Padding,

            other @ (T::Struct | T::Block | T::Array | T::PageableResource) => {
                WireClass::Container(other)
            }

            other @ (T::VertexBuffer | T::Pointer | T::ApiInterop | T::NonCacheRuntimeValue) => {
                WireClass::Opaque(other)
            }
        })
    }

    fn describe(self, type_name: &str, width: u32) -> String {
        match self {
            WireClass::Container(_) | WireClass::Opaque(_) | WireClass::LeafChunk(_) => {
                type_name.to_owned()
            }
            WireClass::Template { group_tag, size } => {
                format!("{type_name} ({size}-byte {} template)", crate::format_group_tag(group_tag))
            }
            _ => format!("{type_name} ({width}-byte {self:?})"),
        }
    }
}

/// Prove that data laid out against `source` can be reinterpreted against
/// `target` byte-for-byte, and return the index translation that makes the
/// transplant expressible.
///
/// This is the strictest question in the module. It is deliberately *not* a
/// similarity score: any difference at all is a refusal, because the caller's
/// next move is to move bytes between the two.
///
/// The walk is memoized on the pair of struct indices, with the pair recorded
/// *before* its fields are visited, so a struct that reaches itself terminates
/// on the in-progress entry rather than recursing forever. That memo is also
/// what keeps a group like `scenario` — 258 shared structs, many reachable from
/// several parents — from blowing up combinatorially.
pub fn struct_trees_are_wire_identical(
    source: TagStructDefinition<'_>,
    target: TagStructDefinition<'_>,
) -> Result<StructIndexMap, WireMismatch> {
    let mut map = StructIndexMap::default();
    walk(source, target, source.name(), &mut map)?;
    Ok(map)
}

/// One field reduced to what the comparison actually looks at.
struct WireField<'a> {
    definition: crate::definition::TagFieldDefinition<'a>,
    clean_name: String,
    class: WireClass,
}

/// The wire-significant fields of a struct, in declaration order.
fn wire_fields(structure: TagStructDefinition<'_>) -> Vec<WireField<'_>> {
    structure
        .fields()
        .filter_map(|definition| {
            // Asked before `WireClass::of`, which sees only the type and would
            // discard every `custom` as a zero-byte editor sentinel. A template
            // hole is the one kind of custom that occupies bytes.
            let class = match definition.template_hole() {
                Some(hole) => WireClass::Template { group_tag: hole.group_tag, size: hole.size },
                None => WireClass::of(definition.field_type())?,
            };
            Some(WireField {
                clean_name: crate::clean_field_name(definition.name()).into_owned(),
                class,
                definition,
            })
        })
        .collect()
}

fn walk(
    source: TagStructDefinition<'_>,
    target: TagStructDefinition<'_>,
    path: &str,
    map: &mut StructIndexMap,
) -> Result<(), WireMismatch> {
    // Record the pair before descending. A struct that reaches itself — through
    // a block of its own type, say — finds this entry and stops.
    if map
        .structs
        .insert(source.index(), target.index())
        .is_some()
    {
        return Ok(());
    }

    if source.size() != target.size() {
        return Err(WireMismatch::StructSize {
            path: path.to_owned(),
            source: source.size(),
            target: target.size(),
        });
    }

    let source_fields = wire_fields(source);
    let target_fields = wire_fields(target);
    if source_fields.len() != target_fields.len() {
        return Err(WireMismatch::FieldCount {
            path: path.to_owned(),
            source: source_fields.len(),
            target: target_fields.len(),
        });
    }

    for (a, b) in source_fields.iter().zip(&target_fields) {
        let field_path = format!("{path}/{}", a.clean_name);
        compare_field(a, b, &field_path, map)?;
        map.fields.insert(a.definition.index(), b.definition.index());
    }
    Ok(())
}

fn compare_field(
    a: &WireField<'_>,
    b: &WireField<'_>,
    path: &str,
    map: &mut StructIndexMap,
) -> Result<(), WireMismatch> {
    // Padding is nameless filler; comparing the dumper's invented names for it
    // would reject identical layouts over cosmetic drift.
    if a.class != WireClass::Padding && a.clean_name != b.clean_name {
        return Err(WireMismatch::FieldName {
            path: path.to_owned(),
            source: a.clean_name.clone(),
            target: b.clean_name.clone(),
        });
    }
    if a.definition.offset() != b.definition.offset() {
        return Err(WireMismatch::FieldOffset {
            path: path.to_owned(),
            source: a.definition.offset(),
            target: b.definition.offset(),
        });
    }
    if a.class != b.class || a.definition.wire_width() != b.definition.wire_width() {
        return Err(mismatch_for(a, b, path));
    }

    match a.class {
        // Option *names* are editor strings; the simulation reads the stored
        // value. A layout that knows fewer names still describes the same
        // bytes, and shipped tags routinely carry an older, shorter list than
        // the current schema dumps -- an HREK animation graph declares 14 user
        // flags where the schema declares 16. Refusing on that would call every
        // real tag incompatible with its own game. Whether the *meaning*
        // survives is a different question, and `compare_group_layouts` answers
        // it with `OptionsLost`.
        WireClass::LeafChunk(TagFieldType::Data) => {
            let source = a.definition.data_definition_name().unwrap_or_default();
            let target = b.definition.data_definition_name().unwrap_or_default();
            if source != target {
                return Err(WireMismatch::DataDefinition {
                    path: path.to_owned(),
                    source: source.to_owned(),
                    target: target.to_owned(),
                });
            }
        }
        WireClass::Opaque(TagFieldType::ApiInterop) => {
            let guid = |field: &WireField<'_>| field.definition.as_api_interop().map(|i| i.guid());
            if guid(a) != guid(b) {
                return Err(WireMismatch::ApiInteropGuid { path: path.to_owned() });
            }
        }
        WireClass::Container(_) => return compare_container(a, b, path, map),
        _ => {}
    }
    Ok(())
}

fn compare_container(
    a: &WireField<'_>,
    b: &WireField<'_>,
    path: &str,
    map: &mut StructIndexMap,
) -> Result<(), WireMismatch> {
    let shape_mismatch = || WireMismatch::ContainerShape {
        path: path.to_owned(),
        source: a.definition.type_name().to_owned(),
        target: b.definition.type_name().to_owned(),
    };

    if let (Some(source), Some(target)) = (a.definition.as_struct(), b.definition.as_struct()) {
        return walk(source, target, path, map);
    }
    if let (Some(source), Some(target)) = (a.definition.as_block(), b.definition.as_block()) {
        map.blocks.insert(source.index(), target.index());
        return walk(source.struct_definition(), target.struct_definition(), path, map);
    }
    if let (Some(source), Some(target)) = (a.definition.as_array(), b.definition.as_array()) {
        if source.count() != target.count() {
            return Err(shape_mismatch());
        }
        return walk(source.struct_definition(), target.struct_definition(), path, map);
    }
    if let (Some(source), Some(target)) = (a.definition.as_resource(), b.definition.as_resource()) {
        return walk(source.struct_definition(), target.struct_definition(), path, map);
    }
    Err(shape_mismatch())
}

fn mismatch_for(a: &WireField<'_>, b: &WireField<'_>, path: &str) -> WireMismatch {
    // A container facing a non-container is a shape difference, not a width
    // one, and reads far better reported that way.
    if matches!(a.class, WireClass::Container(_)) != matches!(b.class, WireClass::Container(_)) {
        return WireMismatch::ContainerShape {
            path: path.to_owned(),
            source: a.definition.type_name().to_owned(),
            target: b.definition.type_name().to_owned(),
        };
    }
    WireMismatch::WireClass {
        path: path.to_owned(),
        source: a
            .class
            .describe(a.definition.type_name(), a.definition.wire_width()),
        target: b
            .class
            .describe(b.definition.type_name(), b.definition.wire_width()),
    }
}

//================================================================================
// The soft comparison: what happens to each field, rather than yes-or-no
//================================================================================

/// How much of a tag survives crossing from one game's layout to the other.
///
/// Ordered best to worst, so a struct's verdict is the `max` of its fields' and
/// a group's the `max` of its structs'. One ordering, used at all three levels,
/// means a reader learns one legend rather than three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompatSeverity {
    /// Interchangeable. Byte-for-byte, nothing to decide.
    Identical,
    /// Every authored value has somewhere to go, though not always the same
    /// name, type spelling, or option ordinal.
    Lossless,
    /// Something authored on the source side has no home on the target side and
    /// will be dropped.
    Lossy,
    /// The two cannot be reconciled by field matching at all.
    Blocked,
}

/// Position of a struct pair within [`GroupComparison::structs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructPairId(pub usize);

/// Deep comparison of two games' definitions of one tag group.
///
/// Struct pairs are *interned*: a struct reachable from several parents is
/// stored once and referred to by [`StructPairId`]. That is what keeps a group
/// like `scenario` — 258 shared structs — to a few hundred rows instead of an
/// unbounded tree, and what makes the walk terminate on structs that reach
/// themselves.
#[derive(Debug, Clone)]
pub struct GroupComparison {
    /// Every struct pair reached, `structs[0]` being the roots.
    pub structs: Vec<StructComparison>,
    /// The worst verdict anywhere in the tree.
    pub severity: CompatSeverity,
}

impl GroupComparison {
    /// The root struct pair.
    pub fn root(&self) -> &StructComparison {
        &self.structs[0]
    }

    /// Resolve a child reference.
    pub fn get(&self, id: StructPairId) -> &StructComparison {
        &self.structs[id.0]
    }
}

/// One struct pair's comparison.
#[derive(Debug, Clone)]
pub struct StructComparison {
    pub source_name: String,
    pub target_name: String,
    pub source_size: usize,
    pub target_size: usize,
    pub source_guid: [u8; 16],
    pub target_guid: [u8; 16],
    pub fields: Vec<FieldComparison>,
    /// The worst verdict among *this struct's own* fields. Does not include
    /// nested structs, which carry their own.
    pub severity: CompatSeverity,
}

/// One aligned field row. Exactly one side is `None` for an added or removed
/// field.
#[derive(Debug, Clone)]
pub struct FieldComparison {
    pub source: Option<FieldFacts>,
    pub target: Option<FieldFacts>,
    pub verdict: FieldVerdict,
    /// For a paired container, the interned struct pair beneath it.
    pub child: Option<StructPairId>,
}

/// What a comparison knows about one side of a field row.
#[derive(Debug, Clone)]
pub struct FieldFacts {
    /// The raw name, markup intact.
    pub name: String,
    pub clean_name: String,
    /// The `{alias}` this field carries, if any — the name it used to have.
    pub alias: Option<String>,
    pub type_name: String,
    pub offset: u32,
    pub width: u32,
}

/// What happens to one field when a tag crosses between the two games.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldVerdict {
    /// Same name, same wire class, same width.
    Identical,
    /// Names differ, but one side records the other's name as its `{alias}`.
    /// A rename the toolset itself vouches for, rather than one inferred.
    Renamed { alias: String },
    /// Same bytes under a different type name.
    TypeEquivalent { reason: TypeEquivalence },
    /// The same kind of value at a different width — a `word flags` facing a
    /// `long flags`, say. The bytes are *not* interchangeable, but the value is:
    /// a converter re-encodes it, and reports any value the narrower side
    /// cannot hold.
    ///
    /// Distinct from [`Self::Blocked`] on purpose. Refusing these would call
    /// `scenario` unconvertible over a flags field that widened by two bytes,
    /// which is both wrong and the difference between a usable catalogue and
    /// one that says "no" to everything interesting.
    Requantized { source_width: u32, target_width: u32 },
    /// Enum or flags matched by option *name*. Every source option has a home,
    /// though possibly at a different ordinal or bit — `remap` gives the
    /// translation.
    OptionsRemapped { remap: Vec<(u32, u32)> },
    /// As above, but some source options have no target option and will be
    /// dropped.
    OptionsLost { lost: Vec<String>, remap: Vec<(u32, u32)> },
    /// Present only on the source side. Authored data with nowhere to go.
    SourceOnly,
    /// Present only on the target side. Left at its default.
    TargetOnly,
    /// Padding or a zero-byte sentinel on one side only. Not data loss, but
    /// recorded so that offset differences further down are explainable.
    StructuralOnly,
    /// Both sides are containers whose element structs disagree. See
    /// [`FieldComparison::child`].
    ContainerDrift,
    /// Matched by name, but with no reconcilable interpretation.
    Blocked(BlockReason),
}

/// Ways two same-named fields can be irreconcilable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockReason {
    /// Same name, incompatible wire class or width.
    TypeIncompatible { source: String, target: String },
    /// A container on one side, a scalar on the other.
    ContainerShape { source: String, target: String },
    /// Two `data` fields naming different data definitions.
    DataDefinition { source: String, target: String },
    /// `api_interop` fields naming different runtime types.
    ApiInteropGuid,
    /// `custom` bytes, vertex buffers, classic pointers — opaque, with no
    /// portable meaning.
    OpaqueBytes,
}

/// Ways two differently-spelled types can still be the same bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeEquivalence {
    /// `long integer` ↔ `dword integer`, `char integer` ↔ `byte integer`: the
    /// signedness of an integer of the same width.
    SameWidthInteger,
    /// `real` ↔ `angle` ↔ `real fraction`: a float under a different unit.
    SameWidthReal,
    /// `string id` ↔ `old string id`: the same leaf chunk.
    SameLeafChunk,
}

impl FieldVerdict {
    /// How much this verdict costs.
    pub fn severity(&self) -> CompatSeverity {
        match self {
            FieldVerdict::Identical => CompatSeverity::Identical,
            // Widening always fits; narrowing might not, and the converter
            // reports the values that do not when it re-encodes them.
            FieldVerdict::Requantized { source_width, target_width }
                if target_width >= source_width =>
            {
                CompatSeverity::Lossless
            }
            FieldVerdict::Requantized { .. } => CompatSeverity::Lossy,
            FieldVerdict::Renamed { .. }
            | FieldVerdict::TypeEquivalent { .. }
            | FieldVerdict::OptionsRemapped { .. }
            | FieldVerdict::StructuralOnly
            // Nothing is lost by leaving a field the source never had at its
            // default — that is what a default is for.
            | FieldVerdict::TargetOnly => CompatSeverity::Lossless,
            FieldVerdict::OptionsLost { .. } | FieldVerdict::SourceOnly => CompatSeverity::Lossy,
            // The cost is whatever the child struct's is; the parent folds it in.
            FieldVerdict::ContainerDrift => CompatSeverity::Identical,
            FieldVerdict::Blocked(_) => CompatSeverity::Blocked,
        }
    }
}

/// Former field names, keyed by containing struct GUID and current clean name.
///
/// Layouts do not carry aliases. `clean_blay_field_name` strips `{alias}` when
/// `TagLayout::from_json` builds a layout, deliberately, because that is what
/// the toolset itself writes into a shipped tag's embedded layout. So the
/// annotation survives only in the JSON schema, and only a caller who read the
/// JSON can supply it — which is why this is a separate index rather than
/// something [`compare_group_layouts`] could look up for itself.
///
/// It is worth the extra argument. Campaign Evolved splits Halo Reach's
/// `weight source` into `primary weight source{weight source}` and a new
/// `secondary weight source`; without the alias that reads as one field lost
/// and two gained, which overstates the loss and hides the correspondence a
/// converter should use.
#[derive(Debug, Clone, Default)]
pub struct AliasIndex {
    by_struct: HashMap<[u8; 16], HashMap<String, String>>,
}

impl AliasIndex {
    /// Read the `{alias}` annotations out of a per-group definition JSON.
    ///
    /// Unreadable or unparseable files yield an empty index rather than an
    /// error: an alias is a hint that improves a comparison, never a
    /// precondition for one.
    pub fn from_schema_json(path: &std::path::Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return Self::default();
        };
        let mut by_struct: HashMap<[u8; 16], HashMap<String, String>> = HashMap::new();
        let structs = value.get("structs").and_then(serde_json::Value::as_object);
        for definition in structs.into_iter().flat_map(|structs| structs.values()) {
            let Some(guid) = definition.get("guid").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(guid) = parse_guid(guid) else { continue };
            let fields = definition.get("fields").and_then(serde_json::Value::as_array);
            for field in fields.into_iter().flatten() {
                let Some(raw) = field.get("name").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let parsed = crate::field_name::parse_field_name(raw);
                if let Some(alias) = parsed.alias {
                    by_struct
                        .entry(guid)
                        .or_default()
                        .insert(parsed.clean_name.into_owned(), alias.to_owned());
                }
            }
        }
        Self { by_struct }
    }

    /// The former name of `clean_name` within the struct identified by `guid`.
    pub fn alias_of(&self, guid: [u8; 16], clean_name: &str) -> Option<&str> {
        self.by_struct.get(&guid)?.get(clean_name).map(String::as_str)
    }

    /// `true` when nothing was loaded.
    pub fn is_empty(&self) -> bool {
        self.by_struct.is_empty()
    }

    /// Merge another index in. Both games' schemas are worth loading: an alias
    /// states that two names are the same field, which is symmetric, so
    /// whichever side wrote it down is equally usable.
    pub fn extend(&mut self, other: AliasIndex) {
        for (guid, aliases) in other.by_struct {
            self.by_struct.entry(guid).or_default().extend(aliases);
        }
    }
}

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (byte, pair) in out.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

/// Compare two games' definitions of the same tag group, field by field, all
/// the way down.
///
/// Where [`struct_trees_are_wire_identical`] answers yes or no, this says what
/// happens to each field — which is what a compatibility catalogue needs, and
/// what a conversion report needs in order to name what it dropped.
///
/// Fields are aligned by cleaned name rather than by position, so an inserted
/// field shifts one row rather than invalidating every row after it. Rows left
/// unmatched are then given a second chance against the `{alias}` annotations,
/// which is how Campaign Evolved's `primary weight source{weight source}` pairs
/// with Halo Reach's `weight source` instead of showing up as one field dropped
/// and an unrelated one added.
pub fn compare_group_layouts(
    source: TagStructDefinition<'_>,
    target: TagStructDefinition<'_>,
    aliases: &AliasIndex,
) -> GroupComparison {
    let mut builder =
        Builder { structs: Vec::new(), interned: HashMap::new(), aliases: aliases.clone() };
    builder.walk(source, target);
    let severity = builder
        .structs
        .iter()
        .map(|s| s.severity)
        .max()
        .unwrap_or(CompatSeverity::Identical);
    GroupComparison { structs: builder.structs, severity }
}

struct Builder {
    structs: Vec<StructComparison>,
    interned: HashMap<(usize, usize), StructPairId>,
    /// Aliases from either side's schema, merged. Which file an alias came from
    /// does not matter: it records that two names are the same field, and that
    /// is symmetric.
    aliases: AliasIndex,
}

impl Builder {
    fn walk(
        &mut self,
        source: TagStructDefinition<'_>,
        target: TagStructDefinition<'_>,
    ) -> StructPairId {
        let key = (source.index(), target.index());
        if let Some(id) = self.interned.get(&key) {
            return *id;
        }
        // Reserve the slot before descending, so a struct that reaches itself
        // finds this id instead of recursing forever.
        let id = StructPairId(self.structs.len());
        self.interned.insert(key, id);
        self.structs.push(StructComparison {
            source_name: source.name().to_owned(),
            target_name: target.name().to_owned(),
            source_size: source.size(),
            target_size: target.size(),
            source_guid: source.guid(),
            target_guid: target.guid(),
            fields: Vec::new(),
            severity: CompatSeverity::Identical,
        });

        let fields = self.compare_fields(source, target);
        let severity = fields
            .iter()
            .map(|row| row.verdict.severity())
            .max()
            .unwrap_or(CompatSeverity::Identical);
        self.structs[id.0].fields = fields;
        self.structs[id.0].severity = severity;
        id
    }

    /// Whether `a` and `b` are the same field under two names, according to an
    /// alias one of the two schemas wrote down.
    fn aliased(&self, guids: (&[u8; 16], &[u8; 16]), a: &WireField<'_>, b: &WireField<'_>) -> bool {
        a.class == b.class
            && (self.aliases.alias_of(*guids.1, &b.clean_name) == Some(a.clean_name.as_str())
                || self.aliases.alias_of(*guids.0, &a.clean_name) == Some(b.clean_name.as_str()))
    }

    fn compare_fields(
        &mut self,
        source: TagStructDefinition<'_>,
        target: TagStructDefinition<'_>,
    ) -> Vec<FieldComparison> {
        let guids = (source.guid(), target.guid());
        let a = comparable_fields(source);
        let b = comparable_fields(target);
        let aligned = self.rescue_by_alias(align_by_name(&a, &b), (&guids.0, &guids.1), &a, &b);
        let mut rows = Vec::new();
        for (left, right) in aligned {
            rows.push(match (left, right) {
                (Some(i), Some(j)) => self.paired((&guids.0, &guids.1), &a[i], &b[j]),
                (Some(i), None) => FieldComparison {
                    verdict: if a[i].class == WireClass::Padding {
                        FieldVerdict::StructuralOnly
                    } else {
                        FieldVerdict::SourceOnly
                    },
                    source: Some(self.facts(&guids.0, &a[i])),
                    target: None,
                    child: None,
                },
                (None, Some(j)) => FieldComparison {
                    verdict: if b[j].class == WireClass::Padding {
                        FieldVerdict::StructuralOnly
                    } else {
                        FieldVerdict::TargetOnly
                    },
                    source: None,
                    target: Some(self.facts(&guids.1, &b[j])),
                    child: None,
                },
                (None, None) => continue,
            });
        }
        rows
    }

    fn facts(&self, guid: &[u8; 16], field: &WireField<'_>) -> FieldFacts {
        FieldFacts {
            name: field.definition.name().to_owned(),
            alias: self
                .aliases
                .alias_of(*guid, &field.clean_name)
                .map(str::to_owned),
            clean_name: field.clean_name.clone(),
            type_name: field.definition.type_name().to_owned(),
            offset: field.definition.offset(),
            width: field.definition.wire_width(),
        }
    }

    /// Give unmatched rows a second chance against the `{alias}` annotations.
    ///
    /// A renamed field appears in the LCS alignment as one row dropped and one
    /// added. When a schema's alias names the other side's field, those two
    /// rows are the same field and belong together — the toolset wrote the
    /// rename down, so this is a record, not an inference.
    fn rescue_by_alias(
        &self,
        rows: Vec<(Option<usize>, Option<usize>)>,
        guids: (&[u8; 16], &[u8; 16]),
        a: &[WireField<'_>],
        b: &[WireField<'_>],
    ) -> Vec<(Option<usize>, Option<usize>)> {
        let mut rows = rows;
        let mut out: Vec<(Option<usize>, Option<usize>)> = Vec::with_capacity(rows.len());
        while !rows.is_empty() {
            let row = rows.remove(0);
            let partner = match row {
                (Some(i), None) => rows
                    .iter()
                    .position(|other| {
                        matches!(other, (None, Some(j)) if self.aliased(guids, &a[i], &b[*j]))
                    })
                    .map(|position| (i, position)),
                (None, Some(j)) => rows
                    .iter()
                    .position(|other| {
                        matches!(other, (Some(i), None) if self.aliased(guids, &a[*i], &b[j]))
                    })
                    .map(|position| (j, position)),
                _ => None,
            };
            match (row, partner) {
                ((Some(i), None), Some((_, position))) => {
                    let (None, Some(j)) = rows.remove(position) else { unreachable!() };
                    out.push((Some(i), Some(j)));
                }
                ((None, Some(j)), Some((_, position))) => {
                    let (Some(i), None) = rows.remove(position) else { unreachable!() };
                    out.push((Some(i), Some(j)));
                }
                _ => out.push(row),
            }
        }
        out
    }

    fn paired(
        &mut self,
        guids: (&[u8; 16], &[u8; 16]),
        a: &WireField<'_>,
        b: &WireField<'_>,
    ) -> FieldComparison {
        let (verdict, child) = self.verdict_for(guids, a, b);
        FieldComparison {
            source: Some(self.facts(guids.0, a)),
            target: Some(self.facts(guids.1, b)),
            verdict,
            child,
        }
    }

    fn verdict_for(
        &mut self,
        guids: (&[u8; 16], &[u8; 16]),
        a: &WireField<'_>,
        b: &WireField<'_>,
    ) -> (FieldVerdict, Option<StructPairId>) {
        if let WireClass::Container(_) = a.class {
            if !matches!(b.class, WireClass::Container(_)) {
                return (
                    FieldVerdict::Blocked(BlockReason::ContainerShape {
                        source: a.definition.type_name().to_owned(),
                        target: b.definition.type_name().to_owned(),
                    }),
                    None,
                );
            }
            return match child_structs(a, b) {
                Some((source, target)) => {
                    let id = self.walk(source, target);
                    let verdict = if self.structs[id.0].severity == CompatSeverity::Identical
                        && self.structs[id.0].source_size == self.structs[id.0].target_size
                    {
                        FieldVerdict::Identical
                    } else {
                        FieldVerdict::ContainerDrift
                    };
                    (verdict, Some(id))
                }
                None => (
                    FieldVerdict::Blocked(BlockReason::ContainerShape {
                        source: a.definition.type_name().to_owned(),
                        target: b.definition.type_name().to_owned(),
                    }),
                    None,
                ),
            };
        }
        if matches!(b.class, WireClass::Container(_)) {
            return (
                FieldVerdict::Blocked(BlockReason::ContainerShape {
                    source: a.definition.type_name().to_owned(),
                    target: b.definition.type_name().to_owned(),
                }),
                None,
            );
        }

        if a.class != b.class {
            return (
                FieldVerdict::Blocked(BlockReason::TypeIncompatible {
                    source: a.class.describe(a.definition.type_name(), a.definition.wire_width()),
                    target: b.class.describe(b.definition.type_name(), b.definition.wire_width()),
                }),
                None,
            );
        }

        // Same kind of value, different width. For enums and flags the option
        // *names* are the contract, so the width is incidental and the real
        // question is answered below. For numbers a converter re-encodes. For
        // anything else — a fixed-width string, an opaque leaf — a width change
        // is a shape change with no defined translation.
        let (source_width, target_width) = (a.definition.wire_width(), b.definition.wire_width());
        if source_width != target_width {
            match a.class {
                WireClass::Enum | WireClass::Flags | WireClass::BlockIndex => {}
                WireClass::Integer | WireClass::Real => {
                    return (FieldVerdict::Requantized { source_width, target_width }, None);
                }
                _ => {
                    return (
                        FieldVerdict::Blocked(BlockReason::TypeIncompatible {
                            source: a.class.describe(a.definition.type_name(), source_width),
                            target: b.class.describe(b.definition.type_name(), target_width),
                        }),
                        None,
                    );
                }
            }
        }

        let verdict = match a.class {
            WireClass::Enum | WireClass::Flags => {
                let options = compare_options(a, b);
                if options == FieldVerdict::Identical {
                    self.same_bytes_verdict(guids, a, b)
                } else {
                    options
                }
            }
            WireClass::LeafChunk(TagFieldType::Data) => {
                let source = a.definition.data_definition_name().unwrap_or_default();
                let target = b.definition.data_definition_name().unwrap_or_default();
                if source == target {
                    self.same_bytes_verdict(guids, a, b)
                } else {
                    FieldVerdict::Blocked(BlockReason::DataDefinition {
                        source: source.to_owned(),
                        target: target.to_owned(),
                    })
                }
            }
            WireClass::Opaque(TagFieldType::ApiInterop) => {
                let guid = |f: &WireField<'_>| f.definition.as_api_interop().map(|i| i.guid());
                if guid(a) == guid(b) {
                    self.same_bytes_verdict(guids, a, b)
                } else {
                    FieldVerdict::Blocked(BlockReason::ApiInteropGuid)
                }
            }
            // Reached only when template and width both matched, since either
            // differing makes the classes unequal and lands in `TypeChanged`
            // above. Equal on both counts means the same inlined body of the
            // same size, so the bytes carry.
            WireClass::Template { .. } => FieldVerdict::Identical,
            // Vertex buffers, classic pointers, non-cache runtime values: bytes
            // whose meaning is bound to the engine that wrote them.
            WireClass::Opaque(_) => FieldVerdict::Blocked(BlockReason::OpaqueBytes),
            _ => self.same_bytes_verdict(guids, a, b),
        };
        // An enum, flags or block index whose options line up but whose storage
        // widened is not "identical" — the value carries, the bytes do not.
        let verdict = match verdict {
            FieldVerdict::Identical if source_width != target_width => {
                FieldVerdict::Requantized { source_width, target_width }
            }
            other => other,
        };
        (verdict, None)
    }

    /// The verdict for two fields already known to occupy the same bytes:
    /// whether they also agree on name and spelling.
    fn same_bytes_verdict(
        &self,
        guids: (&[u8; 16], &[u8; 16]),
        a: &WireField<'_>,
        b: &WireField<'_>,
    ) -> FieldVerdict {
        if a.clean_name != b.clean_name {
            let alias = self
                .aliases
                .alias_of(*guids.1, &b.clean_name)
                .filter(|alias| *alias == a.clean_name)
                .or_else(|| {
                    self.aliases
                        .alias_of(*guids.0, &a.clean_name)
                        .filter(|alias| *alias == b.clean_name)
                });
            if let Some(alias) = alias {
                return FieldVerdict::Renamed { alias: alias.to_owned() };
            }
        }
        if a.definition.type_name() != b.definition.type_name() {
            if let Some(reason) = type_equivalence(a.class) {
                return FieldVerdict::TypeEquivalent { reason };
            }
        }
        FieldVerdict::Identical
    }
}

fn type_equivalence(class: WireClass) -> Option<TypeEquivalence> {
    match class {
        WireClass::Integer => Some(TypeEquivalence::SameWidthInteger),
        WireClass::Real => Some(TypeEquivalence::SameWidthReal),
        WireClass::LeafChunk(_) => Some(TypeEquivalence::SameLeafChunk),
        _ => None,
    }
}

/// Pair enum or flags options by *name*, never by ordinal: the two games
/// routinely declare the same options in a different order, and an ordinal
/// carried across unchanged would name something else.
fn compare_options(a: &WireField<'_>, b: &WireField<'_>) -> FieldVerdict {
    let target: Vec<&str> = b.definition.option_names().collect();
    let mut remap = Vec::new();
    let mut lost = Vec::new();
    for (source_index, name) in a.definition.option_names().enumerate() {
        match target.iter().position(|candidate| *candidate == name) {
            Some(target_index) => remap.push((source_index as u32, target_index as u32)),
            None => lost.push(name.to_owned()),
        }
    }
    if !lost.is_empty() {
        return FieldVerdict::OptionsLost { lost, remap };
    }
    if remap.iter().any(|(from, to)| from != to) {
        return FieldVerdict::OptionsRemapped { remap };
    }
    // Options line up exactly; the caller decides the rest from the names.
    FieldVerdict::Identical
}

fn child_structs<'a, 'b>(
    a: &WireField<'a>,
    b: &WireField<'b>,
) -> Option<(TagStructDefinition<'a>, TagStructDefinition<'b>)> {
    if let (Some(x), Some(y)) = (a.definition.as_struct(), b.definition.as_struct()) {
        return Some((x, y));
    }
    if let (Some(x), Some(y)) = (a.definition.as_block(), b.definition.as_block()) {
        return Some((x.struct_definition(), y.struct_definition()));
    }
    if let (Some(x), Some(y)) = (a.definition.as_array(), b.definition.as_array()) {
        return Some((x.struct_definition(), y.struct_definition()));
    }
    if let (Some(x), Some(y)) = (a.definition.as_resource(), b.definition.as_resource()) {
        return Some((x.struct_definition(), y.struct_definition()));
    }
    None
}

/// Every field worth a row: the wire-significant ones, plus padding, which
/// carries no data but does move everything after it.
fn comparable_fields(structure: TagStructDefinition<'_>) -> Vec<WireField<'_>> {
    wire_fields(structure)
}

/// Longest-common-subsequence alignment on cleaned names.
///
/// Positional pairing would be wrong here: one inserted field would mis-pair
/// every field after it and report a struct as wholly rewritten.
fn align_by_name(
    a: &[WireField<'_>],
    b: &[WireField<'_>],
) -> Vec<(Option<usize>, Option<usize>)> {
    let same = |i: usize, j: usize| a[i].clean_name == b[j].clean_name && a[i].class == b[j].class;
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if same(i, j) {
                dp[i][j] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (n, m);
    let mut out = Vec::with_capacity(n + m);
    while i > 0 && j > 0 {
        if same(i - 1, j - 1) {
            out.push((Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            out.push((Some(i - 1), None));
            i -= 1;
        } else {
            out.push((None, Some(j - 1)));
            j -= 1;
        }
    }
    while i > 0 {
        out.push((Some(i - 1), None));
        i -= 1;
    }
    while j > 0 {
        out.push((None, Some(j - 1)));
        j -= 1;
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::TagFile;
    use std::path::{Path, PathBuf};

    fn definitions() -> PathBuf {
        Path::new("../definitions").to_path_buf()
    }

    fn tag(game: &str, group: &str) -> TagFile {
        TagFile::new(definitions().join(game).join(format!("{group}.json")))
            .unwrap_or_else(|error| panic!("build {game}/{group}: {error}"))
    }

    /// The aliases both games' schemas record for a group, merged. Which file
    /// an alias came from does not matter — it states that two names are the
    /// same field, which is symmetric.
    fn aliases(group: &str) -> AliasIndex {
        let mut index = AliasIndex::default();
        for game in ["haloreach_mcc", "haloce_evolved", "halo3_mcc"] {
            index.extend(AliasIndex::from_schema_json(
                &definitions().join(game).join(format!("{group}.json")),
            ));
        }
        index
    }

    /// Compare a group across the two games, with both schemas' aliases loaded.
    fn compare(source_game: &str, target_game: &str, group: &str) -> GroupComparison {
        let source = tag(source_game, group);
        let target = tag(target_game, group);
        compare_group_layouts(
            source.definitions().root_struct(),
            target.definitions().root_struct(),
            &aliases(group),
        )
    }

    /// Find a struct by name anywhere in a tag's layout.
    fn nested<'a>(tag: &'a TagFile, wanted: &str) -> TagStructDefinition<'a> {
        fn walk<'a>(
            structure: TagStructDefinition<'a>,
            wanted: &str,
            seen: &mut std::collections::HashSet<usize>,
        ) -> Option<TagStructDefinition<'a>> {
            if structure.name() == wanted {
                return Some(structure);
            }
            if !seen.insert(structure.index()) {
                return None;
            }
            structure.fields().find_map(|field| {
                let nested = field
                    .as_struct()
                    .or_else(|| field.as_block().map(|b| b.struct_definition()))
                    .or_else(|| field.as_array().map(|a| a.struct_definition()))
                    .or_else(|| field.as_resource().map(|r| r.struct_definition()))?;
                walk(nested, wanted, seen)
            })
        }
        walk(
            tag.definitions().root_struct(),
            wanted,
            &mut std::collections::HashSet::new(),
        )
        .unwrap_or_else(|| panic!("no struct named {wanted}"))
    }

    /// The claim the Reach-to-Campaign-Evolved animation import rests on: the
    /// compressed animation payload and everything wrapping it are declared
    /// identically by both games, so the resource can move across untouched.
    ///
    /// Campaign Evolved inherited Reach's 68-byte, 17-field
    /// `packed_data_sizes_struct` rather than Halo 3's 16-byte one, which is
    /// also why `PackedDataSizes::layout` resolves a Campaign Evolved graph to
    /// `SizeLayout::Reach` and decodes it with Reach's rules.
    #[test]
    fn jmad_resource_tree_is_wire_identical_reach_to_campaign_evolved() {
        let reach = tag("haloreach_mcc", "model_animation_graph");
        let evolved = tag("haloce_evolved", "model_animation_graph");
        let map = struct_trees_are_wire_identical(
            nested(&reach, "model_animation_tag_resource_struct"),
            nested(&evolved, "model_animation_tag_resource_struct"),
        )
        .expect("the jmad resource subtree must be interchangeable");

        // The wrapper, the group member, and the packed-size table.
        assert_eq!(
            map.struct_count(),
            3,
            "the resource subtree is three structs deep",
        );
    }

    /// Negative control. Without it the test above could be passing because the
    /// walk accepts everything rather than because the two games agree.
    ///
    /// Halo 3's `packed_data_sizes_struct` is 16 bytes to Reach's 68, so the
    /// same comparison against Halo 3 must fail — and must say where.
    #[test]
    fn jmad_resource_tree_is_not_wire_identical_to_halo_3() {
        let reach = tag("haloreach_mcc", "model_animation_graph");
        let halo3 = tag("halo3_mcc", "model_animation_graph");
        let error = struct_trees_are_wire_identical(
            nested(&reach, "model_animation_tag_resource_struct"),
            nested(&halo3, "model_animation_tag_resource_struct"),
        )
        .expect_err("Halo 3 packs animation sizes differently");
        assert!(
            !error.path().is_empty(),
            "a refusal has to say where: {error}",
        );
    }

    /// `long integer` and `dword integer` are the same four bytes under two
    /// spellings, and that is the only difference between the two games'
    /// `model_animation_tag_resource_member`. Comparing wire class and width
    /// rather than type name is what lets it through, so pin that it does.
    #[test]
    fn signedness_spelling_is_not_a_wire_difference() {
        let reach = tag("haloreach_mcc", "model_animation_graph");
        let evolved = tag("haloce_evolved", "model_animation_graph");
        let member = "model_animation_tag_resource_member";

        let source = nested(&reach, member);
        let target = nested(&evolved, member);
        let checksum = |structure: TagStructDefinition<'_>| {
            structure
                .fields()
                .find(|field| field.name() == "animation_checksum")
                .map(|field| field.type_name().to_owned())
        };
        assert_eq!(checksum(source).as_deref(), Some("long integer"));
        assert_eq!(checksum(target).as_deref(), Some("dword integer"));

        struct_trees_are_wire_identical(source, target)
            .expect("a signedness rename is not a wire difference");
    }

    /// The whole animation graph is *not* interchangeable, and the walk finds
    /// the first place it stops being so. This is the difference the root-only
    /// comparison cannot see.
    #[test]
    fn the_animation_graph_as_a_whole_is_not_wire_identical() {
        let reach = tag("haloreach_mcc", "model_animation_graph");
        let evolved = tag("haloce_evolved", "model_animation_graph");
        let error = struct_trees_are_wire_identical(
            reach.definitions().root_struct(),
            evolved.definitions().root_struct(),
        )
        .expect_err("four structs differ in size between the two games");

        // The root itself agrees — that is exactly why the root-only comparison
        // reported a clean match — so the failure must be somewhere below it.
        assert!(
            error.path().contains('/'),
            "the difference is nested, not at the root: {error}",
        );
    }

    /// A group both games declare identically compares equal all the way down.
    /// `sound_looping` is one of the 49 such groups.
    #[test]
    fn a_group_both_games_agree_on_is_wire_identical() {
        let reach = tag("haloreach_mcc", "sound_looping");
        let evolved = tag("haloce_evolved", "sound_looping");
        let map = struct_trees_are_wire_identical(
            reach.definitions().root_struct(),
            evolved.definitions().root_struct(),
        )
        .unwrap_or_else(|error| panic!("sound_looping should be interchangeable: {error}"));
        assert!(map.struct_count() > 1, "the walk must have descended");
    }

    /// Every layout is interchangeable with itself, and the resulting map is the
    /// identity. Run over the largest group we ship, this also proves the walk
    /// terminates on self-recursive structs and that memoization keeps the cost
    /// linear rather than quadratic.
    #[test]
    fn scenario_compares_with_itself_and_terminates() {
        for game in ["halo3_mcc", "haloreach_mcc", "haloce_evolved"] {
            let scenario = tag(game, "scenario");
            let map = struct_trees_are_wire_identical(
                scenario.definitions().root_struct(),
                scenario.definitions().root_struct(),
            )
            .unwrap_or_else(|error| panic!("{game} scenario differs from itself: {error}"));

            for source in map.structs.keys() {
                assert_eq!(
                    map.struct_index(*source),
                    Some(*source),
                    "{game}: a layout maps onto itself as the identity",
                );
            }
            assert!(
                map.struct_count() > 100,
                "{game}: scenario is a large graph; only {} structs were visited",
                map.struct_count(),
            );
        }
    }

    /// Halo 3 and Reach reshaped `scenario` heavily, so the comparison must
    /// refuse it — and must terminate while doing so, on the biggest graph we
    /// ship.
    #[test]
    fn scenario_across_generations_is_refused_not_hung() {
        let h3 = tag("halo3_mcc", "scenario");
        let reach = tag("haloreach_mcc", "scenario");
        let error = struct_trees_are_wire_identical(
            h3.definitions().root_struct(),
            reach.definitions().root_struct(),
        )
        .expect_err("Halo 3 and Reach scenarios are not interchangeable");
        assert!(!error.path().is_empty(), "{error}");
    }

    /// Find a struct pair by source name in a group comparison.
    fn pair<'a>(comparison: &'a GroupComparison, name: &str) -> &'a StructComparison {
        comparison
            .structs
            .iter()
            .find(|s| s.source_name == name)
            .unwrap_or_else(|| panic!("no struct pair named {name}"))
    }

    fn row<'a>(structure: &'a StructComparison, clean_name: &str) -> &'a FieldComparison {
        structure
            .fields
            .iter()
            .find(|row| {
                row.source.as_ref().is_some_and(|f| f.clean_name == clean_name)
                    || row.target.as_ref().is_some_and(|f| f.clean_name == clean_name)
            })
            .unwrap_or_else(|| panic!("no field row for {clean_name}"))
    }

    /// The four size-changed structs, and only those four, are what the soft
    /// comparison flags as costing something. This is the regression fence: a
    /// definitions bump that widens the blast radius has to be noticed here
    /// rather than in a converted tag.
    #[test]
    fn the_animation_graph_reports_exactly_the_known_differences() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "model_animation_graph");

        let sized: Vec<&str> = comparison
            .structs
            .iter()
            .filter(|s| s.source_size != s.target_size)
            .map(|s| s.source_name.as_str())
            .collect();
        assert_eq!(
            sized,
            vec![
                "animation_graph_node_block",
                "shared_model_animation_block",
                "new_animation_blend_screen_block_struct",
                "animation_ik_set_item",
            ],
            "the set of structs that change size between the two games",
        );
        assert_eq!(comparison.severity, CompatSeverity::Lossy);
    }

    /// Campaign Evolved split Reach's single blend-screen weight source into a
    /// primary/secondary pair, and wrote Reach's names into the aliases. The
    /// comparison has to read that: without alias rescue this shows up as two
    /// fields dropped and four added, which would read as far more data loss
    /// than actually occurs.
    #[test]
    fn the_blend_screen_rename_is_read_from_the_alias() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "model_animation_graph");
        let blend = pair(&comparison, "new_animation_blend_screen_block_struct");

        assert_eq!(
            row(blend, "weight source").verdict,
            FieldVerdict::Renamed { alias: "weight source".to_owned() },
            "Reach's `weight source` is Campaign Evolved's `primary weight source`",
        );
        assert_eq!(
            row(blend, "weight source object function").verdict,
            FieldVerdict::Renamed { alias: "weight source object function".to_owned() },
        );
        // The secondary half is genuinely new: nothing in Reach maps to it, so
        // it stays at its default rather than being fed something unrelated.
        assert_eq!(row(blend, "secondary weight source").verdict, FieldVerdict::TargetOnly);
    }

    /// Reach carries two flag bytes on each skeleton node that Campaign Evolved
    /// does not. That is real, reportable data loss, and it must not be
    /// disguised as anything softer.
    #[test]
    fn the_node_flags_reach_drops_are_reported_as_loss() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "model_animation_graph");
        let node = pair(&comparison, "animation_graph_node_block");

        for field in ["node joint flags", "additional flags"] {
            assert_eq!(
                row(node, field).verdict,
                FieldVerdict::SourceOnly,
                "{field} has nowhere to go in Campaign Evolved",
            );
        }
        assert_eq!(node.severity, CompatSeverity::Lossy);
    }

    /// Padding and zero-byte sentinels drift between the two toolsets for
    /// cosmetic reasons. They must never be reported as data loss, or every
    /// group would read as lossy and the signal would be worthless.
    #[test]
    fn padding_drift_is_not_data_loss() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "model_animation_graph");

        for structure in &comparison.structs {
            for row in &structure.fields {
                let padding = |facts: &Option<FieldFacts>| {
                    facts.as_ref().is_some_and(|f| {
                        f.type_name.contains("pad") || f.type_name.contains("skip")
                    })
                };
                if padding(&row.source) || padding(&row.target) {
                    assert!(
                        matches!(
                            row.verdict,
                            FieldVerdict::Identical | FieldVerdict::StructuralOnly
                        ),
                        "{}: padding reported as {:?}",
                        structure.source_name,
                        row.verdict,
                    );
                }
            }
        }
    }

    /// A flags field that widened between the two games carries its value fine
    /// — a converter re-encodes it bit by bit, by name — so it must not be
    /// reported as blocked.
    ///
    /// This distinction is load-bearing for usefulness rather than for safety.
    /// Calling these blocked marked seventeen shared groups unconvertible,
    /// `scenario` among them, over flags fields that grew by two bytes.
    #[test]
    fn a_widened_flags_field_is_requantized_not_blocked() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "scenario");
        let object = comparison
            .structs
            .iter()
            .find(|s| s.source_name == "scenario_object_datum_struct")
            .expect("scenario declares object data");
        let flags = object
            .fields
            .iter()
            .find(|row| row.source.as_ref().is_some_and(|f| f.clean_name == "manual bsp flags"))
            .expect("object data carries manual bsp flags");

        assert_eq!(
            flags.verdict,
            FieldVerdict::Requantized { source_width: 2, target_width: 4 },
            "Campaign Evolved widened this from word to long",
        );
        assert_eq!(
            flags.verdict.severity(),
            CompatSeverity::Lossless,
            "widening always fits",
        );
    }

    /// The mirror: narrowing might not fit, so it costs something.
    #[test]
    fn a_narrowed_field_is_reported_as_lossy() {
        let widening = FieldVerdict::Requantized { source_width: 2, target_width: 4 };
        let narrowing = FieldVerdict::Requantized { source_width: 4, target_width: 2 };
        assert_eq!(widening.severity(), CompatSeverity::Lossless);
        assert_eq!(narrowing.severity(), CompatSeverity::Lossy);
    }

    /// A group both games declare identically has nothing to report at all.
    #[test]
    fn an_identical_group_is_reported_as_identical() {
        let comparison = compare("haloreach_mcc", "haloce_evolved", "sound_looping");
        assert_eq!(comparison.severity, CompatSeverity::Identical);
    }

    /// The soft comparison must agree with the strict one on the question the
    /// strict one answers. If they can disagree, one of them is lying to a
    /// caller.
    #[test]
    fn the_two_comparisons_agree_on_identity() {
        for group in ["sound_looping", "model_animation_graph", "dialogue", "biped"] {
            let reach = tag("haloreach_mcc", group);
            let evolved = tag("haloce_evolved", group);
            let strict = struct_trees_are_wire_identical(
                reach.definitions().root_struct(),
                evolved.definitions().root_struct(),
            )
            .is_ok();
            let soft = compare("haloreach_mcc", "haloce_evolved", group).severity;
            assert_eq!(
                strict,
                soft == CompatSeverity::Identical,
                "{group}: strict says identical={strict}, soft says {soft:?}",
            );
        }
    }

    /// The interning that keeps `scenario` tractable has to survive the soft
    /// walk too, which visits strictly more of the graph.
    #[test]
    fn the_soft_comparison_terminates_on_scenario() {
        let comparison = compare("halo3_mcc", "haloreach_mcc", "scenario");
        assert!(comparison.structs.len() > 100, "the walk must have descended");
        assert!(
            comparison.structs.len() < 5_000,
            "interning must keep this near-linear, got {} struct pairs",
            comparison.structs.len(),
        );
    }

    /// A biped is not a weapon, and the walk says so at the root.
    #[test]
    fn different_groups_are_refused_at_the_root() {
        let evolved_biped = tag("haloce_evolved", "biped");
        let evolved_weapon = tag("haloce_evolved", "weapon");
        let error = struct_trees_are_wire_identical(
            evolved_biped.definitions().root_struct(),
            evolved_weapon.definitions().root_struct(),
        )
        .expect_err("a biped is not a weapon");
        assert!(matches!(error, WireMismatch::StructSize { .. }), "{error}");
    }
}
