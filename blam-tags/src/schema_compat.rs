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
//! after it.

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
    /// An enum or flags field declares a different number of options. The bytes
    /// would transfer, but their *meaning* would not.
    OptionCount { path: String, source: usize, target: usize },
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
            | WireMismatch::OptionCount { path, .. }
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
            WireMismatch::OptionCount { path, source, target } => {
                write!(f, "{path} declares {source} options on one side and {target} on the other")
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
            let class = WireClass::of(definition.field_type())?;
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
        // Same bytes, but an option list of a different length means a stored
        // ordinal or bit would name something else on the other side.
        WireClass::Enum | WireClass::Flags => {
            let source = a.definition.option_names().count();
            let target = b.definition.option_names().count();
            if source != target {
                return Err(WireMismatch::OptionCount {
                    path: path.to_owned(),
                    source,
                    target,
                });
            }
        }
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
