//! Cross-game tag conversion: what a tag becomes when it moves to another engine.
//!
//! Owns group routing, struct pairing, field matching, value translation,
//! companion-tag synthesis and the conversion report. Presentation, kit mounting,
//! dialog state and the bulk-conversion worker belong to the editor, not here.

use crate::classic::{ClassicHeader, read_classic_tag_file};
use crate::file::TagFileHeader;
use crate::paths::group_tag_to_extension;
use crate::{
    ApiInteropData, Endian, FunctionFlags, FunctionType, StringIdData, TagBlock, TagField,
    TagFieldData, TagFieldMut, TagFieldPath, TagFieldType, TagFile, TagLayout, TagOptions,
    TagReferenceData, TagResourceKind, TagStruct, TagStructMut, format_group_tag, parse_group_tag,
};
use serde::Deserialize;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

mod companions;
pub use companions::*;
mod resources;
pub use resources::*;

/// A field name reduced to the key the matcher compares on.
///
/// Element indices and field ordinals come off, the markup grammar's annotations
/// are already gone by the time a tag carries the name, and the result is
/// lowercased so two schemas that disagree only on capitalisation still pair. The
/// editor has its own copy for display purposes; this one is the converter's, and
/// every alias table in here is keyed with it — keying with anything else silently
/// loses every rename whose name carries a capital, an underscore or a hyphen.
pub fn clean_field_key(name: &str) -> String {
    TagFieldPath::parse(name)
        .strip_node_indices()
        .to_string()
        .to_ascii_lowercase()
}

/// The `definitions/` tree the conversion tests read their schemas from.
///
/// The editor ships its copy beside the executable so schemas stay editable without
/// a rebuild; the engine only ever needs the submodule checkout in its own repo, and
/// both repos pin the same revision.
#[cfg(test)]
fn locate_definitions_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("definitions")
}

/// Every file beneath `root`, symlinks not followed.
///
/// Stands in for the editor's `walkdir` dependency, which the engine has no other
/// use for. An unreadable directory is skipped rather than fatal: a kit walk is a
/// best-effort survey of someone else's install, and one locked folder should not
/// fail a conversion.
pub fn walk_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            // `DirEntry::file_type` does not follow the link, which is what
            // `follow_links(false)` bought.
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out
}

/// Read a tag from disk, honoring the classic (Halo CE / Halo 2) formats.
///
/// Classic containers carry no embedded `blay` layout, so **`TagFile::read` cannot
/// parse them** — the per-game JSON schema has to be supplied out of band. Reaching
/// for `TagFile::read` here fails silently and in a way that never points at the
/// reader: it has variously looked like a missing kit template, a tag that would not
/// reopen after writing, and a dropped byte cache. Anything in the converter that
/// opens a tag from a kit goes through this.
pub fn read_tag_for_conversion(
    path: &Path,
    game: Option<&str>,
    definitions_root: Option<&Path>,
    group_tag: u32,
) -> Result<TagFile, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    if ClassicHeader::parse(&bytes).is_some() {
        let game = game.ok_or("classic tag requires a detected game profile")?;
        let definitions_root = definitions_root.ok_or("classic tag requires a definitions root")?;
        let group_name =
            group_tag_to_extension(group_tag).ok_or("unknown group for classic tag layout")?;
        let definition = definitions_root.join(game).join(format!("{group_name}.json"));
        let layout = TagLayout::from_json(&definition).map_err(|error| {
            format!("failed to load classic layout {}: {error}", definition.display())
        })?;
        return read_classic_tag_file(&bytes, layout)
            .map_err(|error| format!("failed to decode classic tag: {error}"));
    }
    TagFile::read(path).map_err(|error| format!("Could not read {}: {error}", path.display()))
}

pub const CONVERSION_GAMES: &[&str] = &[
    "halo3_mcc",
    "halo3odst_mcc",
    "haloreach_mcc",
    "halo4_mcc",
    "halo2amp_mcc",
];

/// Every profile a tag may be recognized as, or converted to or from.
///
/// Campaign Evolved is here and deliberately not in [`CONVERSION_GAMES`].
/// That list is the reviewed *common base*, and the catalog's coverage
/// denominator is the intersection of its members: the five share 125 groups,
/// but adding Campaign Evolved drops the intersection to 68. Folding it in
/// would not add a profile — it would delete 57 reviewed groups from
/// `conversion_mappings.json`.
pub const CONVERSION_PROFILES: &[&str] = &[
    "haloce_mcc",
    "halo2_mcc",
    "halo3_mcc",
    "halo3odst_mcc",
    "haloreach_mcc",
    "halo4_mcc",
    "halo2amp_mcc",
    CAMPAIGN_EVOLVED_GAME,
];

/// The classic-container profiles: Halo CE and Halo 2.
///
/// Kept apart from [`CONVERSION_GAMES`] because that list is the *reviewed*
/// common base and its intersection is the catalog's coverage denominator —
/// folding these in would drop it from 125 groups to 51. These convert on the
/// strength of schema-derived matching plus whatever reviewed rules name them,
/// and their coverage is reported per pair rather than counted against the base.
///
/// They differ from the MCC profiles in ways the converter has to respect: Halo
/// CE bodies are big-endian, every struct GUID is all-zero (see
/// [`schema_struct_key`]), Halo 2 carries versioned structs, and neither has an
/// MCC generation header (see [`apply_editing_kit_mcc_header`]). A classic
/// *target* also cannot be built by `TagFile::new`, so it needs a kit-authored
/// template to start from.
pub const CLASSIC_CONVERSION_GAMES: &[&str] = &["haloce_mcc", "halo2_mcc"];

/// The profile Campaign Evolved's schemas descend from, and the only one it
/// converts with.
pub const CAMPAIGN_EVOLVED_PARENT: &str = "haloreach_mcc";

/// Whether the converter will attempt this ordered pair.
///
/// The five MCC profiles convert to each other in any direction: their mutual
/// surface is the reviewed common base the catalog covers. Campaign Evolved
/// pairs only with Halo Reach, both ways — a Campaign Evolved tag *is* a
/// Reach-format tag at a different schema revision, and no other profile has
/// that relationship. Refusing the rest by name beats converting a Halo 3 tag
/// into Campaign Evolved through a Reach-shaped hole nobody reviewed.
pub fn conversion_pair_supported(source_game: &str, target_game: &str) -> bool {
    if source_game == target_game {
        return false;
    }
    let mcc = |game: &str| CONVERSION_GAMES.contains(&game);
    let classic = |game: &str| CLASSIC_CONVERSION_GAMES.contains(&game);
    match (source_game, target_game) {
        (CAMPAIGN_EVOLVED_GAME, other) | (other, CAMPAIGN_EVOLVED_GAME) => {
            other == CAMPAIGN_EVOLVED_PARENT
        }
        // Halo CE and Halo 2 convert with each other and with the MCC base, in
        // both directions. They are deliberately not paired with Campaign
        // Evolved: it shares 28 groups with Halo CE and 62 with Halo 2, and its
        // schemas descend from Reach's, so there is nothing reviewed to lean on.
        (source, target) => {
            (classic(source) || mcc(source)) && (classic(target) || mcc(target))
        }
    }
}

/// The profiles a conversion from `source_game` may target, in menu order.
pub fn conversion_targets_for(source_game: &str) -> Vec<&'static str> {
    CONVERSION_PROFILES
        .iter()
        .copied()
        .filter(|target| conversion_pair_supported(source_game, target))
        .collect()
}

/// The engine generations in order, as the tag formats actually descend.
///
/// This is the order the reviewed rules were authored along: every mapping in the
/// catalog names an adjacent pair or a set of them, because that is where one
/// engine's designers were looking at the previous one's tags. A route between
/// distant profiles therefore walks this list rather than jumping, which is what
/// makes each hop a conversion somebody has checked.
///
/// Campaign Evolved is deliberately absent. It is not a generation — it is a UE5
/// remake whose schemas descend from Reach's, so it hangs off Reach and pairs
/// with nothing else (see [`conversion_pair_supported`]). Routing *through* it
/// would mean passing a tag through a game that has no such tag class.
pub const CONVERSION_CHAIN: &[&str] = &[
    "haloce_mcc",
    "halo2_mcc",
    "halo3_mcc",
    "halo3odst_mcc",
    "haloreach_mcc",
    "halo4_mcc",
    "halo2amp_mcc",
];

/// Routes from `source_game` to `target_game`, shortest first.
///
/// The first entry is always the direct pair, when it is allowed at all — a
/// caller tries these in order and stops at the first that carries the tag, so
/// routing never happens to a conversion that worked. Later entries add one
/// intermediate at a time, taken from [`CONVERSION_CHAIN`] *between* the two
/// endpoints: converting Halo 2 to Reach may pass through Halo 3 and ODST, but
/// never out to Halo 4 and back.
///
/// Campaign Evolved has exactly one partner, so it never routes: a Halo 3 tag
/// cannot reach it via Reach, because the reviewed relationship is between
/// Reach's schemas and its own, not between anything else's.
pub fn conversion_routes(source_game: &str, target_game: &str) -> Vec<Vec<String>> {
    let mut routes = Vec::new();
    if conversion_pair_supported(source_game, target_game) {
        routes.push(vec![source_game.to_owned(), target_game.to_owned()]);
    }
    if source_game == CAMPAIGN_EVOLVED_GAME || target_game == CAMPAIGN_EVOLVED_GAME {
        return routes;
    }
    let position = |game: &str| CONVERSION_CHAIN.iter().position(|entry| *entry == game);
    let (Some(from), Some(to)) = (position(source_game), position(target_game)) else {
        return routes;
    };
    // Only the profiles strictly between the endpoints, in travel order. A route
    // that doubled back would be transcoding through a generation the tag has no
    // business visiting.
    let between: Vec<&str> = if from < to {
        CONVERSION_CHAIN[from + 1..to].to_vec()
    } else {
        CONVERSION_CHAIN[to + 1..from].iter().rev().copied().collect()
    };
    // Shortest first: one intermediate, then two, and so on. Subsets keep their
    // travel order, so a route is always a walk along the chain.
    for count in 1..=between.len() {
        for stops in subsequences(&between, count) {
            let mut route = vec![source_game.to_owned()];
            route.extend(stops.iter().map(|game| (*game).to_string()));
            route.push(target_game.to_owned());
            // Every hop still has to be a pair the converter will attempt.
            if route
                .windows(2)
                .all(|hop| conversion_pair_supported(&hop[0], &hop[1]))
            {
                routes.push(route);
            }
        }
    }
    routes
}

/// Order-preserving subsequences of `items` of exactly `count` elements.
fn subsequences<'a>(items: &[&'a str], count: usize) -> Vec<Vec<&'a str>> {
    if count == 0 {
        return vec![Vec::new()];
    }
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if items.len() - index < count {
            break;
        }
        for mut rest in subsequences(&items[index + 1..], count - 1) {
            let mut one = vec![*item];
            one.append(&mut rest);
            out.push(one);
        }
    }
    out
}

/// Why a pair is refused, in a sentence that says what to do instead.
fn unsupported_pair_message(source_game: &str, target_game: &str) -> String {
    if source_game == target_game {
        return "Choose a different target game profile".to_owned();
    }
    if source_game == CAMPAIGN_EVOLVED_GAME || target_game == CAMPAIGN_EVOLVED_GAME {
        let other =
            if source_game == CAMPAIGN_EVOLVED_GAME { target_game } else { source_game };
        return format!(
            "Campaign Evolved converts only to and from {CAMPAIGN_EVOLVED_PARENT}, because its \
             schemas descend from Reach's and no other profile's do. Convert to \
             {CAMPAIGN_EVOLVED_PARENT} first, then to {other}."
        );
    }
    "The selected source or target profile is not supported by this converter".to_owned()
}

const CONVERSION_MAPPING_CATALOG: &str = include_str!("conversion_mappings.json");

/// These groups contain layout features which `TagFile::new` cannot currently
/// reconstruct closely enough for the native editing kits. Start from an
/// editing-kit-authored target tag so its embedded layout tables stay native.
#[cfg(test)]
const NATIVE_LAYOUT_TEMPLATE_GROUPS: &[&str] = &["particle", "model", "biped"];

#[cfg(test)]
fn requires_native_layout_template(group_name: &str) -> bool {
    NATIVE_LAYOUT_TEMPLATE_GROUPS
        .iter()
        .any(|group| group_name.eq_ignore_ascii_case(group))
}

/// Stamp a freshly-created MCC tag with the file-header generation expected by
/// the corresponding editing kit. `TagFile::new` deliberately initializes
/// these fields to zero, which is sufficient for the library's own parser but
/// is rejected (and can crash) in the native editing-kit tools.
///
/// Campaign Evolved rides along despite having no editing kit: it is the same
/// question — which generation does a freshly created tag claim — and answering
/// it in two places is how the CE case came to be missing in the first place.
pub fn apply_editing_kit_mcc_header(tag: &mut TagFile, game: &str) -> Result<(), String> {
    // A classic tag has no MCC generation to stamp: `write_classic_tag` copies
    // the original 64-byte header through verbatim and patches only the
    // checksum, so the three fields below are not part of its format. Writing
    // them would corrupt the header bytes the kit reads.
    if CLASSIC_CONVERSION_GAMES.contains(&game) {
        return Ok(());
    }
    let build_number = match game {
        "halo3_mcc" | "halo3odst_mcc" => 1,
        // Campaign Evolved carries Reach's generation exactly, because its
        // `.ubulk` blobs *are* Reach-format tags — the UE5 package around them
        // is a wrapper, and the simulation reads the blob. Measured, not
        // assumed: all 12,289 shipped tag blobs across all 101 groups read
        // `1 / 2 / 0xffffffff`, with no per-group variation. The gate is
        // `the_shipped_tag_header_generation` in blam-tags.
        "haloreach_mcc" | "halo4_mcc" | "halo2amp_mcc" | "haloce_evolved" => 2,
        _ => return Err(format!("No MCC tag-header defaults are known for {game}")),
    };
    tag.header.build_version = 1;
    tag.header.build_number = build_number;
    // Stock/tool-created tags use -1 when no per-file source revision is known.
    tag.header.version = u32::MAX;
    Ok(())
}

/// The definitions-directory name for Campaign Evolved. Defined here rather
/// than shared because the two other copies (`controller::tools`,
/// `source::loading`) are equally local and the string is the on-disk folder
/// name, not a value anything derives.
pub const CAMPAIGN_EVOLVED_GAME: &str = "haloce_evolved";

/// The generation every shipped Campaign Evolved tag blob carries:
/// `(build_version, build_number, version)`.
///
/// Identical to Halo Reach's, which is the point — a CE `.ubulk` is a
/// Reach-format tag. `TagFile::new` leaves all three at zero, which is the one
/// value nothing shipped has, so an unstamped tag is one the simulation has
/// never seen the like of.
pub const CAMPAIGN_EVOLVED_GENERATION: (i32, i32, u32) = (1, 2, u32::MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversionIssueKind {
    Unsupported,
    Truncated,
    Warning,
}

#[derive(Clone, Debug)]
pub struct ConversionIssue {
    pub kind: ConversionIssueKind,
    pub path: String,
    pub message: String,
}

#[derive(Default)]
pub struct TagConversionReport {
    pub copied_exact: usize,
    pub converted_semantic: usize,
    pub mapped_aliases: usize,
    pub defaulted_target: usize,
    pub unsupported_source: usize,
    pub truncated: usize,
    /// Pageable resources carried across whole. Worth counting separately from
    /// ordinary fields: one of these can be the bulk of the tag, and a reader
    /// comparing an animation graph's before and after wants to see that its
    /// payload went with it.
    pub transferred_resources: usize,
    /// References left empty because the target game has no such tag class.
    ///
    /// Counted apart from `unsupported_source` because it is a different kind
    /// of fact: not a field the conversion failed to carry, but one the
    /// destination has no home for by design. It is also the count a user acts
    /// on — each one is something to reconnect by hand.
    pub dropped_references: usize,
    pub issues: Vec<ConversionIssue>,
}

pub struct TagConversionDraft {
    pub tag: TagFile,
    pub companion_tags: Vec<CompanionTagDraft>,
    pub report: TagConversionReport,
    pub target_group_name: String,
    pub target_extension: String,
    pub native_layout_template: Option<PathBuf>,
    /// Every profile the tag passed through, source first and destination last,
    /// when the direct pair could not carry it. Empty for a direct conversion.
    ///
    /// Worth carrying rather than reporting and forgetting: a routed conversion
    /// has been through two or more engines' worth of loss, and a reader deciding
    /// whether to trust the result needs to know that before they look at the
    /// numbers.
    pub route: Vec<String>,
}

pub struct CompanionTagDraft {
    pub key: String,
    pub file_suffix: String,
    pub group_name: String,
    pub extension: String,
    pub tag: TagFile,
    pub native_layout_template: Option<PathBuf>,
}

#[derive(Default)]
pub struct GameTagIndex {
    pub by_tag: HashMap<u32, String>,
    pub by_name: HashMap<String, u32>,
}

#[derive(Default)]
pub struct NativeTemplateIndex {
    by_group: HashMap<u32, Vec<PathBuf>>,
    cached: RefCell<HashMap<u32, Option<(Vec<u8>, PathBuf)>>>,
}

impl NativeTemplateIndex {
    pub fn build(tags_root: &Path, groups: &GameTagIndex) -> Self {
        let mut by_extension = HashMap::new();
        for (group_tag, group_name) in &groups.by_tag {
            let extension = group_tag_to_extension(*group_tag).unwrap_or(group_name);
            by_extension.insert(extension.to_ascii_lowercase(), *group_tag);
        }
        let mut result = Self::default();
        for item in walk_files(tags_root) {
            let Some(extension) = item.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(group_tag) = by_extension.get(&extension.to_ascii_lowercase()).copied() else {
                continue;
            };
            result
                .by_group
                .entry(group_tag)
                .or_default()
                .push(item);
        }
        for paths in result.by_group.values_mut() {
            paths.sort();
        }
        result
    }
}

impl GameTagIndex {
    pub fn load(definitions_root: &Path, game: &str) -> Result<Self, String> {
        let path = definitions_root.join(game).join("_meta.json");
        let bytes = fs::read(&path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
        let entries = value
            .get("tag_index")
            .and_then(Value::as_object)
            .ok_or_else(|| format!("{} is missing tag_index", path.display()))?;
        let mut index = Self::default();
        for (fourcc, name) in entries {
            let (Some(tag), Some(name)) = (parse_group_tag(fourcc), name.as_str()) else {
                continue;
            };
            index.by_tag.insert(tag, name.to_owned());
            index.by_name.insert(name.to_ascii_lowercase(), tag);
        }
        Ok(index)
    }
}

struct ConversionContext<'a> {
    source_groups: &'a GameTagIndex,
    target_groups: &'a GameTagIndex,
    source_field_aliases: &'a SchemaFieldAliases,
    target_field_aliases: &'a SchemaFieldAliases,
    mapping_catalog: &'a ConversionMappingCatalog,
    definitions_root: &'a Path,
    native_templates: Option<&'a NativeTemplateIndex>,
    source_game: &'a str,
    target_game: &'a str,
    group_name: &'a str,
    report: TagConversionReport,
    companion_tags: Vec<CompanionTagDraft>,
    fatal_error: Option<String>,
    root_matches: usize,
    /// Paths of non-empty `data` blobs the conversion could not carry.
    ///
    /// Separate from `resources_left_behind` because the two fail differently: a
    /// pageable resource is a whole payload chunk, a `data` field is an inline
    /// blob. Both are the substance of the tag rather than a property of it.
    payloads_left_behind: Vec<String>,
    /// Paths of non-null pageable resources the conversion could not carry.
    ///
    /// A resource holds the bulk of what some tags are — an animation graph's
    /// entire compressed payload is one — so leaving one behind silently would
    /// produce a tag that looks converted and plays nothing. The safety check
    /// reads this rather than asking whether the source had any resources at
    /// all, which is what it used to do and what made every animation graph
    /// unconvertible.
    resources_left_behind: Vec<String>,
    /// Memo of `(source struct index, target struct index) -> wire-identical`.
    ///
    /// `struct_trees_are_wire_identical` walks a whole struct tree, and
    /// `convert_struct` runs once per struct *instance* — including once per block
    /// element — so an uncached call would rewalk the same pair thousands of times
    /// for one tag.
    wire_identical: HashMap<(u32, u32), bool>,
}

#[derive(serde::Deserialize)]
struct ConversionMappingCatalog {
    version: u32,
    coverage: String,
    /// Canonical groups whose five-game mapping surface has been reviewed.
    /// Most fields in these groups deliberately remain schema-derived; this
    /// list makes that coverage explicit and machine-checkable without
    /// duplicating thousands of identical field names in the JSON catalog.
    #[serde(default)]
    covered_groups: Vec<String>,
    #[serde(default)]
    struct_mappings: Vec<StructMappingRule>,
    #[serde(default)]
    incompatible_pairs: Vec<IncompatiblePairRule>,
    #[serde(default)]
    unusable_schemas: Vec<UnusableSchemaRule>,
    #[serde(default)]
    reference_drops: Vec<ReferenceDropRule>,
    /// Source fields the target profile does not define at all, reviewed and
    /// accepted as dropped.
    ///
    /// `reference_drops` answers the same question for tag references only.
    /// This covers everything else, and exists because the fail-closed groups
    /// cannot otherwise distinguish "the destination has no such field, and we
    /// have checked that is fine" from "something went wrong". Without it every
    /// Halo Reach animation graph refuses on the two node-flag bytes Campaign
    /// Evolved does not carry.
    #[serde(default)]
    accepted_field_drops: Vec<ReferenceDropRule>,
    /// Inline `data` blobs both profiles declare but neither can hand to the
    /// other, reviewed and accepted as lost.
    ///
    /// Distinct from `accepted_field_drops`, which is for fields the target does
    /// not declare *at all*. Here both sides have the field and the blob still
    /// cannot cross, because the opaque copy path pairs on the data definition's
    /// name and the two disagree. Keeping the two apart is what lets each be
    /// checked for the invariant that actually applies to it — an accepted field
    /// drop must be absent on the far side, and one of these must be present.
    #[serde(default)]
    accepted_payload_drops: Vec<ReferenceDropRule>,
    #[serde(default)]
    field_aliases: Vec<FieldAliasRule>,
    #[serde(default)]
    option_aliases: Vec<OptionAliasRule>,
    /// Groups the same tag class is called by in different profiles.
    ///
    /// Every other rule kind maps *within* a group; this one decides which group
    /// a conversion even lands in. Without it a renamed class is simply absent:
    /// an H2 projectile's attachment points at a `contrail`, Halo 3 calls that
    /// class `contrail_system`, so the canonical-name lookup finds nothing and
    /// the reference is dropped. Measured on
    /// `battle_rifle_bullet.projectile` — one lost reference, reported as
    /// `dropped_refs=1`.
    #[serde(default)]
    group_aliases: Vec<GroupAliasRule>,
    #[serde(default)]
    payload_aliases: Vec<PayloadAliasRule>,
}

#[derive(serde::Deserialize)]
struct GroupAliasRule {
    source: String,
    target: String,
    #[serde(default)]
    source_games: Vec<String>,
    #[serde(default)]
    target_games: Vec<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
struct FieldAliasRule {
    group: String,
    #[serde(default)]
    source_games: Vec<String>,
    #[serde(default)]
    target_games: Vec<String>,
    source_struct_guid: Option<String>,
    target_struct_guid: Option<String>,
    source: String,
    target: String,
}

/// A reviewed pair of `data` definition names that hold the same kind of payload.
///
/// The derived rule — carry a blob when both sides declare the *same* data
/// definition name — covers Halo 1 to Halo 2, where both say
/// `processed_pixel_data_data`. It cannot cover Halo 2 to Halo 3, which renamed the
/// definition to `bitmap_group_pixel_data_def` while keeping the same field name and
/// the same bytes. Renames are judgement, so they are declared here.
#[derive(serde::Deserialize)]
struct PayloadAliasRule {
    group: String,
    #[serde(default)]
    source_games: Vec<String>,
    #[serde(default)]
    target_games: Vec<String>,
    source_definition: String,
    target_definition: String,
}

#[derive(serde::Deserialize)]
struct StructMappingRule {
    group: String,
    source_games: Vec<String>,
    target_games: Vec<String>,
    source_path: String,
    target_path: String,
    /// Whether swapping the two paths is also a valid reparent.
    ///
    /// True for a pair that merely nests the same body at different depths, which
    /// is every rule here except one. It is *false* for `shader` -> `material`,
    /// because the reparent is only correct when the target group is `material`:
    /// going the other way the source is Halo 4's vestigial `shader`, whose root
    /// has the same shape as Reach's, so reparenting it into Reach's
    /// `render_method` finds nothing while plain root-to-root matches both
    /// `render_method` and `material name`.
    #[serde(default = "yes")]
    bidirectional: bool,
}

fn yes() -> bool {
    true
}

#[derive(serde::Deserialize)]
struct IncompatiblePairRule {
    group: String,
    source_games: Vec<String>,
    target_games: Vec<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
struct UnusableSchemaRule {
    group: String,
    games: Vec<String>,
    reason: String,
}

#[derive(serde::Deserialize)]
struct ReferenceDropRule {
    group: String,
    source_games: Vec<String>,
    target_games: Vec<String>,
    source_path: String,
    reason: String,
}

#[derive(serde::Deserialize)]
struct OptionAliasRule {
    group: String,
    field: String,
    #[serde(default)]
    source_games: Vec<String>,
    #[serde(default)]
    target_games: Vec<String>,
    source: String,
    target: String,
}

impl ConversionMappingCatalog {
    fn load() -> Result<Self, String> {
        let catalog: Self = serde_json::from_str(CONVERSION_MAPPING_CATALOG)
            .map_err(|error| format!("Could not parse conversion_mappings.json: {error}"))?;
        if catalog.version != 1 {
            return Err(format!(
                "Unsupported conversion mapping catalog version {}",
                catalog.version
            ));
        }
        if catalog.coverage != "all_supported_groups" {
            return Err(format!(
                "Unsupported conversion mapping coverage policy {}",
                catalog.coverage
            ));
        }
        let mut covered_groups = HashSet::new();
        for (index, group) in catalog.covered_groups.iter().enumerate() {
            let normalized = group.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                return Err(format!(
                    "conversion_mappings.json covered_groups[{index}] is empty"
                ));
            }
            if !covered_groups.insert(normalized) {
                return Err(format!(
                    "conversion_mappings.json covered_groups[{index}] duplicates {group}"
                ));
            }
        }
        for (index, rule) in catalog.field_aliases.iter().enumerate() {
            validate_mapping_rule_scope(
                "field_aliases",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
                &rule.source,
                &rule.target,
            )?;
            for (label, guid) in [
                ("source_struct_guid", rule.source_struct_guid.as_deref()),
                ("target_struct_guid", rule.target_struct_guid.as_deref()),
            ] {
                if guid.is_some_and(|guid| parse_schema_guid(guid).is_none()) {
                    return Err(format!(
                        "conversion_mappings.json field_aliases[{index}] has an invalid {label}"
                    ));
                }
            }
        }
        for (index, rule) in catalog.group_aliases.iter().enumerate() {
            validate_mapping_rule_scope(
                "group_aliases",
                index,
                &rule.source,
                &rule.source_games,
                &rule.target_games,
                &rule.source,
                &rule.target,
            )?;
            // A group alias decides which tag class a conversion lands in, so an
            // unscoped one would rename a class in every direction at once.
            if rule.source_games.is_empty() || rule.target_games.is_empty() {
                return Err(format!(
                    "conversion_mappings.json group_aliases[{index}] must name both \
                     source_games and target_games"
                ));
            }
            if normalized_names_equal(&rule.source, &rule.target) {
                return Err(format!(
                    "conversion_mappings.json group_aliases[{index}] renames a group to itself"
                ));
            }
            if rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json group_aliases[{index}] needs a reason"
                ));
            }
        }
        for (index, rule) in catalog.struct_mappings.iter().enumerate() {
            validate_game_scopes(
                "struct_mappings",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
            )?;
            if rule.source_path.split('/').any(str::is_empty) && !rule.source_path.is_empty()
                || rule.target_path.split('/').any(str::is_empty) && !rule.target_path.is_empty()
            {
                return Err(format!(
                    "conversion_mappings.json struct_mappings[{index}] has an invalid path"
                ));
            }
        }
        for (index, rule) in catalog.incompatible_pairs.iter().enumerate() {
            validate_game_scopes(
                "incompatible_pairs",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
            )?;
            if rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json incompatible_pairs[{index}] has no reason"
                ));
            }
        }
        for (index, rule) in catalog.unusable_schemas.iter().enumerate() {
            validate_game_scopes(
                "unusable_schemas",
                index,
                &rule.group,
                &rule.games,
                &rule.games,
            )?;
            if rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json unusable_schemas[{index}] has no reason"
                ));
            }
        }
        for (index, rule) in catalog.reference_drops.iter().enumerate() {
            validate_game_scopes(
                "reference_drops",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
            )?;
            if clean_field_key(&rule.source_path).is_empty() || rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json reference_drops[{index}] has an empty path or reason"
                ));
            }
        }
        for (index, rule) in catalog.accepted_field_drops.iter().enumerate() {
            validate_game_scopes(
                "accepted_field_drops",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
            )?;
            if clean_field_key(&rule.source_path).is_empty() || rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json accepted_field_drops[{index}] has an empty path or reason"
                ));
            }
        }
        for (index, rule) in catalog.accepted_payload_drops.iter().enumerate() {
            validate_game_scopes(
                "accepted_payload_drops",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
            )?;
            if clean_field_key(&rule.source_path).is_empty() || rule.reason.trim().is_empty() {
                return Err(format!(
                    "conversion_mappings.json accepted_payload_drops[{index}] has an empty path or reason"
                ));
            }
        }
        for (index, rule) in catalog.option_aliases.iter().enumerate() {
            validate_mapping_rule_scope(
                "option_aliases",
                index,
                &rule.group,
                &rule.source_games,
                &rule.target_games,
                &rule.source,
                &rule.target,
            )?;
            if normalize_option_name(&rule.field).is_empty() {
                return Err(format!(
                    "conversion_mappings.json option_aliases[{index}] has an empty field"
                ));
            }
        }
        Ok(catalog)
    }

    fn field_names_match(&self, request: FieldMappingRequest<'_>) -> bool {
        self.field_aliases.iter().any(|rule| {
            if !rule.group.eq_ignore_ascii_case(request.group) {
                return false;
            }
            mapping_rule_direction_matches(
                &rule.source_games,
                &rule.target_games,
                request.source_game,
                request.target_game,
                &rule.source,
                &rule.target,
                request.source_name,
                request.target_name,
            ) && guid_rule_matches(
                rule.source_struct_guid.as_deref(),
                rule.target_struct_guid.as_deref(),
                request.source_guid,
                request.target_guid,
                request.source_game,
                request.target_game,
                &rule.source_games,
                &rule.target_games,
            )
        })
    }

    fn option_names_match(
        &self,
        group: &str,
        field_path: &str,
        source_game: &str,
        target_game: &str,
        source_name: &str,
        target_name: &str,
    ) -> bool {
        let field = field_path
            .rsplit('/')
            .next()
            .unwrap_or(field_path)
            .split('[')
            .next()
            .unwrap_or(field_path);
        self.option_aliases.iter().any(|rule| {
            rule.group.eq_ignore_ascii_case(group)
                && normalize_option_name(&rule.field) == normalize_option_name(field)
                && mapping_rule_direction_matches(
                    &rule.source_games,
                    &rule.target_games,
                    source_game,
                    target_game,
                    &rule.source,
                    &rule.target,
                    source_name,
                    target_name,
                )
        })
    }

    /// Whether a reviewed rule says these two `data` definitions hold the same kind
    /// of payload, so the bytes may be carried verbatim. Bidirectional, like every
    /// other section.
    fn payload_alias_allows(
        &self,
        group: &str,
        source_game: &str,
        target_game: &str,
        source_definition: &str,
        target_definition: &str,
    ) -> bool {
        self.payload_aliases.iter().any(|rule| {
            if !rule.group.eq_ignore_ascii_case(group) {
                return false;
            }
            let forward = game_scope_matches(&rule.source_games, source_game)
                && game_scope_matches(&rule.target_games, target_game)
                && rule.source_definition == source_definition
                && rule.target_definition == target_definition;
            let reverse = game_scope_matches(&rule.source_games, target_game)
                && game_scope_matches(&rule.target_games, source_game)
                && rule.source_definition == target_definition
                && rule.target_definition == source_definition;
            forward || reverse
        })
    }

    fn struct_mapping<'a>(
        &'a self,
        group: &str,
        source_game: &str,
        target_game: &str,
    ) -> Option<(&'a str, &'a str)> {
        self.struct_mappings.iter().find_map(|rule| {
            if !rule.group.eq_ignore_ascii_case(group) {
                return None;
            }
            if game_scope_matches(&rule.source_games, source_game)
                && game_scope_matches(&rule.target_games, target_game)
            {
                Some((rule.source_path.as_str(), rule.target_path.as_str()))
            } else if rule.bidirectional
                && game_scope_matches(&rule.source_games, target_game)
                && game_scope_matches(&rule.target_games, source_game)
            {
                Some((rule.target_path.as_str(), rule.source_path.as_str()))
            } else {
                None
            }
        })
    }

    fn incompatibility_reason<'a>(
        &'a self,
        group: &str,
        source_game: &str,
        target_game: &str,
    ) -> Option<&'a str> {
        self.incompatible_pairs.iter().find_map(|rule| {
            (rule.group.eq_ignore_ascii_case(group)
                && ((game_scope_matches(&rule.source_games, source_game)
                    && game_scope_matches(&rule.target_games, target_game))
                    || (game_scope_matches(&rule.source_games, target_game)
                        && game_scope_matches(&rule.target_games, source_game))))
            .then_some(rule.reason.as_str())
        })
    }

    fn unusable_schema_reason<'a>(&'a self, group: &str, game: &str) -> Option<&'a str> {
        self.unusable_schemas.iter().find_map(|rule| {
            (rule.group.eq_ignore_ascii_case(group) && game_scope_matches(&rule.games, game))
                .then_some(rule.reason.as_str())
        })
    }

    fn reference_drop_reason<'a>(
        &'a self,
        group: &str,
        source_game: &str,
        target_game: &str,
        source_path: &str,
    ) -> Option<&'a str> {
        self.reference_drops.iter().find_map(|rule| {
            (rule.group.eq_ignore_ascii_case(group)
                && game_scope_matches(&rule.source_games, source_game)
                && game_scope_matches(&rule.target_games, target_game)
                && clean_field_key(&rule.source_path) == clean_field_key(source_path))
            .then_some(rule.reason.as_str())
        })
    }

    /// Why a source field with no target counterpart is an accepted loss.
    ///
    /// Checks both sections: a reference and an ordinary field are the same
    /// question to a caller asking "was this drop reviewed?", and splitting the
    /// lookup would let a rule filed in the wrong one silently stop working.
    fn accepted_drop_reason<'a>(
        &'a self,
        group: &str,
        source_game: &str,
        target_game: &str,
        source_path: &str,
    ) -> Option<&'a str> {
        // A rule covers its own path *and* everything beneath it. When the
        // target has no `facial wrinkle events` block at all, it has none of
        // the fields inside one either, and the converter reports those
        // children individually — 85 of HREK's cinematic head graphs refused on
        // `.../facial wrinkle events[0]/wrinkle name` while the rule named the
        // block. Matching by ancestry is what a dropped container means.
        //
        // `is_ancestor_of` compares segment by segment and ignores element
        // indices, so this cannot match a differently-named sibling and one
        // rule covers every element of a repeated block.
        let matches = |rule: &'a ReferenceDropRule| {
            if !rule.group.eq_ignore_ascii_case(group)
                || !game_scope_matches(&rule.source_games, source_game)
                || !game_scope_matches(&rule.target_games, target_game)
            {
                return None;
            }
            let covered = crate::TagFieldPath::parse(&clean_field_key(&rule.source_path));
            let reported = crate::TagFieldPath::parse(&clean_field_key(source_path));
            covered.is_ancestor_of(&reported).then_some(rule.reason.as_str())
        };
        self.accepted_field_drops
            .iter()
            .find_map(matches)
            .or_else(|| self.reference_drops.iter().find_map(matches))
    }

    /// Why an inline `data` blob is reviewed as safe to lose.
    ///
    /// Only consulted by the payload check. A blob the target declares but
    /// cannot be handed is a different situation from a field it does not have,
    /// and answering both from one list would mean neither could be validated.
    fn accepted_payload_drop_reason<'a>(
        &'a self,
        group: &str,
        source_game: &str,
        target_game: &str,
        source_path: &str,
    ) -> Option<&'a str> {
        self.accepted_payload_drops.iter().find_map(|rule| {
            if !rule.group.eq_ignore_ascii_case(group)
                || !game_scope_matches(&rule.source_games, source_game)
                || !game_scope_matches(&rule.target_games, target_game)
            {
                return None;
            }
            let covered = crate::TagFieldPath::parse(&clean_field_key(&rule.source_path));
            let reported = crate::TagFieldPath::parse(&clean_field_key(source_path));
            covered.is_ancestor_of(&reported).then_some(rule.reason.as_str())
        })
    }
}

fn validate_game_scopes(
    section: &str,
    index: usize,
    group: &str,
    source_games: &[String],
    target_games: &[String],
) -> Result<(), String> {
    if group.trim().is_empty() || source_games.is_empty() || target_games.is_empty() {
        return Err(format!(
            "conversion_mappings.json {section}[{index}] has an empty group or game scope"
        ));
    }
    for game in source_games.iter().chain(target_games) {
        if !CONVERSION_PROFILES.contains(&game.as_str()) {
            return Err(format!(
                "conversion_mappings.json {section}[{index}] uses unsupported game {game}"
            ));
        }
    }
    Ok(())
}

fn validate_mapping_rule_scope(
    section: &str,
    index: usize,
    group: &str,
    source_games: &[String],
    target_games: &[String],
    source: &str,
    target: &str,
) -> Result<(), String> {
    if group.trim().is_empty()
        || normalize_option_name(source).is_empty()
        || normalize_option_name(target).is_empty()
    {
        return Err(format!(
            "conversion_mappings.json {section}[{index}] has an empty group or name"
        ));
    }
    for game in source_games.iter().chain(target_games) {
        if !CONVERSION_PROFILES.contains(&game.as_str()) {
            return Err(format!(
                "conversion_mappings.json {section}[{index}] uses unsupported game {game}"
            ));
        }
    }
    Ok(())
}

struct FieldMappingRequest<'a> {
    group: &'a str,
    source_game: &'a str,
    target_game: &'a str,
    source_guid: [u8; 16],
    target_guid: [u8; 16],
    source_name: &'a str,
    target_name: &'a str,
}

fn mapping_rule_direction_matches(
    source_games: &[String],
    target_games: &[String],
    source_game: &str,
    target_game: &str,
    rule_source: &str,
    rule_target: &str,
    source_name: &str,
    target_name: &str,
) -> bool {
    let forward = game_scope_matches(source_games, source_game)
        && game_scope_matches(target_games, target_game)
        && normalized_names_equal(rule_source, source_name)
        && normalized_names_equal(rule_target, target_name);
    let reverse = game_scope_matches(source_games, target_game)
        && game_scope_matches(target_games, source_game)
        && normalized_names_equal(rule_source, target_name)
        && normalized_names_equal(rule_target, source_name);
    forward || reverse
}

fn game_scope_matches(games: &[String], game: &str) -> bool {
    games.is_empty() || games.iter().any(|candidate| candidate == game)
}

fn normalized_names_equal(left: &str, right: &str) -> bool {
    normalize_option_name(left) == normalize_option_name(right)
}

fn guid_rule_matches(
    source_rule: Option<&str>,
    target_rule: Option<&str>,
    source_guid: [u8; 16],
    target_guid: [u8; 16],
    source_game: &str,
    target_game: &str,
    source_games: &[String],
    target_games: &[String],
) -> bool {
    let source_rule = source_rule.and_then(parse_schema_guid);
    let target_rule = target_rule.and_then(parse_schema_guid);
    let forward = game_scope_matches(source_games, source_game)
        && game_scope_matches(target_games, target_game)
        && source_rule.is_none_or(|guid| guid == source_guid)
        && target_rule.is_none_or(|guid| guid == target_guid);
    let reverse = game_scope_matches(source_games, target_game)
        && game_scope_matches(target_games, source_game)
        && source_rule.is_none_or(|guid| guid == target_guid)
        && target_rule.is_none_or(|guid| guid == source_guid);
    forward || reverse
}

/// How a struct is addressed in an alias table.
///
/// A GUID is the right key where there is one: it survives a struct rename, so
/// `weapon_group` and `weapon_block_struct` share an entry. But **every struct in
/// `haloce_mcc` (752) and `halo2_mcc` (2,323) has an all-zero GUID** — they were
/// dumped from HABT classic XML layouts — so keying on it alone collapses a whole
/// classic game onto one bucket and leaks aliases between unrelated structs. Fall
/// back to the struct's own name there, which is a JSON object key and therefore
/// unique within the group.
fn schema_struct_key(guid: [u8; 16], struct_name: &str) -> String {
    if guid == [0u8; 16] {
        format!("name:{}", struct_name.to_ascii_lowercase())
    } else {
        guid.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

#[derive(Default)]
struct SchemaFieldAliases {
    by_struct: HashMap<String, HashMap<String, HashSet<String>>>,
    /// Cleaned field key -> the `:units` annotation the *schema* gives it.
    ///
    /// A tag's own layout stores the display name only — `clean_blay_field_name`
    /// cuts at `:` and `#` — so by the time the converter sees a field, its unit
    /// is gone. The schema is the only place left that says whether `grenade
    /// angle` is degrees, and that is what decides an `angle`/`real` rescale.
    /// Keyed group-wide rather than per struct because the answer is a property
    /// of the field's name, and a struct GUID is unavailable for the classic
    /// profiles anyway.
    units: HashMap<String, String>,
    /// Cleaned keys whose *schema* name carries `*` — the editor-owned marker.
    ///
    /// Stripped from a tag's own layout just like `:units`, so it has to be read
    /// off the definition. Used to tell an opaque blob the author wrote from one
    /// the toolchain wrote and will overwrite.
    editor_owned: HashSet<String>,
    /// Cleaned keys whose *schema* name carries `!` — engine-managed / not in
    /// cache. Stripped from a tag's own layout, so it too must come from here.
    engine_managed: HashSet<String>,
}

impl SchemaFieldAliases {
    fn load(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not parse {}: {error}", path.display()))?;
        let mut result = Self::default();
        // A group's JSON carries only its *own* registry. Anything it inherits
        // — `biped` -> `unit` -> `object`, which is where `grenade angle:degrees`
        // actually lives — sits in the ancestor files, so a lookup that reads one
        // file sees neither their aliases nor their units. Walk the chain the way
        // the engine's `merge_parent_schemas` does, tolerating a missing
        // `_meta.json` or parent file as "no parent".
        for value in std::iter::once(value)
            .chain(ancestor_schemas(path).into_iter())
            .collect::<Vec<_>>()
        {
            result.absorb(&value);
        }
        Ok(result)
    }

    fn absorb(&mut self, value: &Value) {
        let result = self;
        let Some(structs) = value.get("structs").and_then(Value::as_object) else {
            return;
        };
        for (struct_name, structure) in structs {
            let (Some(guid), Some(fields)) = (
                structure
                    .get("guid")
                    .and_then(Value::as_str)
                    .and_then(parse_schema_guid),
                structure.get("fields").and_then(Value::as_array),
            ) else {
                continue;
            };
            let aliases = result
                .by_struct
                .entry(schema_struct_key(guid, struct_name))
                .or_default();
            for field in fields {
                let Some(name) = field.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let base = name.split(['#', ':']).next().unwrap_or(name);
                if base.contains('*') {
                    result.editor_owned.insert(clean_field_key(base));
                }
                if base.contains('!') || base.trim_start().starts_with("runtime ") {
                    result.engine_managed.insert(clean_field_key(base));
                }
                if let Some(unit) = field_unit_annotation(name) {
                    // Keyed by the base name, because that is all a tag's own
                    // layout keeps and so all the converter can look up with.
                    result.units.insert(clean_field_key(base), unit);
                }
                // Keyed with `clean_field_key`, the same function every lookup uses.
                //
                // `option_name_aliases` normalizes for *enum option* comparison —
                // it strips `*!^`, turns `_`/`-` into spaces and collapses runs,
                // but does not lowercase. `matches` is called with
                // `clean_field_key`, which lowercases via `TagFieldPath`. Keying
                // with one and looking up with the other silently lost every
                // `{former name}` alias whose name carries a capital, an underscore
                // or a hyphen — and the schema declares hundreds of them, which is
                // how Reach's `root offset max scale idle{root offset max scale}`
                // failed to pair with Halo 3's `root offset max scale` even though
                // the schema states the rename outright.
                let names = option_name_aliases(base);
                for name in &names {
                    aliases
                        .entry(clean_field_key(name))
                        .or_default()
                        .extend(
                            names
                                .iter()
                                .filter(|alias| *alias != name)
                                .map(|alias| clean_field_key(alias)),
                        );
                }
            }
        }
    }

    fn matches(&self, guid: [u8; 16], struct_name: &str, left: &str, right: &str) -> bool {
        self.by_struct
            .get(&schema_struct_key(guid, struct_name))
            .and_then(|fields| fields.get(left))
            .is_some_and(|aliases| aliases.contains(right))
    }

    fn unit_of(&self, key: &str) -> Option<&str> {
        self.units.get(key).map(String::as_str)
    }

    fn is_editor_owned(&self, key: &str) -> bool {
        self.editor_owned.contains(key)
    }

    fn is_engine_managed(&self, key: &str) -> bool {
        self.engine_managed.contains(key)
    }
}

/// Every ancestor group's schema JSON for `path`, nearest parent first.
///
/// `parent_tag` names a four-CC that `_meta.json`'s `tag_index` maps to the
/// sibling file name. Anything unresolvable is treated as "no parent", matching
/// the engine's `merge_parent_schemas`.
fn ancestor_schemas(path: &Path) -> Vec<Value> {
    let Some(dir) = path.parent() else { return Vec::new() };
    let Ok(meta_bytes) = fs::read(dir.join("_meta.json")) else { return Vec::new() };
    let Ok(meta) = serde_json::from_slice::<Value>(&meta_bytes) else { return Vec::new() };
    let Some(index) = meta.get("tag_index").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut current = fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| {
            value.get("parent_tag").and_then(Value::as_str).map(str::to_owned)
        });
    for _ in 0..32 {
        let Some(parent) = current.take() else { break };
        let Some(name) = index.get(&parent).and_then(Value::as_str) else { break };
        let Ok(bytes) = fs::read(dir.join(format!("{name}.json"))) else { break };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else { break };
        current = value.get("parent_tag").and_then(Value::as_str).map(str::to_owned);
        out.push(value);
    }
    out
}

impl ConversionMappingCatalog {
    /// What `group` is called in `target_game`, if a reviewed rule renames it.
    ///
    /// Reversible like every other rule: a rule written `contrail` ->
    /// `contrail_system` also answers `contrail_system` -> `contrail` when the
    /// direction is reversed.
    fn group_alias(&self, group: &str, source_game: &str, target_game: &str) -> Option<&str> {
        self.group_aliases.iter().find_map(|rule| {
            let forward = game_scope_matches(&rule.source_games, source_game)
                && game_scope_matches(&rule.target_games, target_game);
            let reverse = game_scope_matches(&rule.source_games, target_game)
                && game_scope_matches(&rule.target_games, source_game);
            if forward && normalized_names_equal(&rule.source, group) {
                Some(rule.target.as_str())
            } else if reverse && normalized_names_equal(&rule.target, group) {
                Some(rule.source.as_str())
            } else {
                None
            }
        })
    }
}

/// The group tag `group_name` lands on in `target_groups`, by canonical name and
/// then by reviewed alias.
///
/// The one place group identity is decided. Three call sites used to do this
/// lookup independently — the conversion root, reference-fidelity validation, and
/// the per-field reference remap — which is how a renamed class could be resolved
/// in one and dropped in another.
fn resolve_target_group(
    group_name: &str,
    target_groups: &GameTagIndex,
    catalog: &ConversionMappingCatalog,
    source_game: &str,
    target_game: &str,
) -> Option<(u32, String)> {
    // A reviewed alias outranks a same-name group, because the case that needs an
    // alias most is the one where the target *has* the name and never uses it.
    // Halo 4 and H2A still declare `shader` — and ship zero `.shader` tags, against
    // 7,140 and 1,917 `.material` — so resolving `shader` to `shader` produced a
    // class the game does not load, and the alias saying "use `material` here" was
    // dead code behind the direct lookup.
    //
    // Safe for the renames already in the catalog: measured across every profile,
    // no game declares both halves of a pair (`contrail` exists only in H1/H2,
    // `contrail_system` only in H3 onward, and likewise for `decal`/`decal_system`,
    // `light_volume`/`light_volume_system`, `model_animations`/
    // `model_animation_graph`). `shader`/`material` is the sole overlap, which is
    // exactly what this ordering exists to resolve.
    if let Some(aliased) = catalog.group_alias(group_name, source_game, target_game)
        && let Some(tag) = target_groups
            .by_name
            .get(&aliased.to_ascii_lowercase())
            .copied()
    {
        let name = target_groups
            .by_tag
            .get(&tag)
            .cloned()
            .unwrap_or_else(|| aliased.to_owned());
        return Some((tag, name));
    }
    let tag = target_groups
        .by_name
        .get(&group_name.to_ascii_lowercase())
        .copied()?;
    let name = target_groups.by_tag.get(&tag).cloned();
    Some((tag, name.unwrap_or_else(|| group_name.to_owned())))
}

fn parse_schema_guid(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32 {
        return None;
    }
    let mut result = [0u8; 16];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(result)
}

#[derive(Clone)]
struct TargetFieldInfo {
    ordinal: usize,
    name: String,
    key: String,
    field_type: TagFieldType,
}

/// Where each hop of a route gets its kit-authored layout templates.
///
/// A route's intermediate profiles need templates too — the hop into Halo 3 is a
/// conversion into Halo 3 like any other, and starting it from a kit tag is what
/// makes its output the shape Halo 3 actually ships. A profile with no entry
/// still converts, from a schema-built tag, so a missing intermediate kit costs
/// fidelity rather than the whole route. Classic profiles are the exception and
/// say so: one cannot be built from a schema at all.
pub trait TemplateSource {
    fn templates_for(&self, game: &str) -> Option<&NativeTemplateIndex>;
}

impl TemplateSource for () {
    fn templates_for(&self, _game: &str) -> Option<&NativeTemplateIndex> {
        None
    }
}

impl TemplateSource for HashMap<String, NativeTemplateIndex> {
    fn templates_for(&self, game: &str) -> Option<&NativeTemplateIndex> {
        self.get(game)
    }
}

/// Convert `source` into `target_game`, routing through intermediate engines when
/// the direct pair cannot carry it.
///
/// Tries the direct conversion first and returns it untouched when it works, so
/// nothing that converts today starts taking a detour. Only on refusal does it
/// walk [`conversion_routes`], shortest first, hop by hop.
///
/// A hop hands the next one *serialized bytes*, not the draft's in-memory tag.
/// That is deliberate and it is the difference between this and a shortcut: the
/// intermediate is exactly the file the user would have got by saving into that
/// game and importing from it, so a draft that would not survive a save fails
/// here instead of two hops later. Nothing is written to disk, so there is no
/// intermediate file to clean up and no half-finished tag left in a kit if a
/// later hop fails.
///
/// Every hop's issues are kept, tagged with the hop that raised them. A routed
/// conversion loses more than a direct one and the report has to show where.
pub fn analyze_conversion_routed(
    source: &TagFile,
    source_game: &str,
    target_game: &str,
    definitions_root: &Path,
    templates: &dyn TemplateSource,
) -> Result<TagConversionDraft, String> {
    let routes = conversion_routes(source_game, target_game);
    if routes.is_empty() {
        return Err(unsupported_pair_message(source_game, target_game));
    }
    let mut refusals = Vec::new();
    for route in &routes {
        match run_conversion_route(source, route, definitions_root, templates) {
            Ok(draft) => return Ok(draft),
            Err(error) => refusals.push(format!("{}: {error}", route.join(" \u{2192} "))),
        }
    }
    // Every route refused. The direct one's reason is the one that answers the
    // user's question, so it leads; the rest say what else was tried, because
    // "no route works" and "we did not look" are different answers.
    Err(format!(
        "Could not convert {source_game} to {target_game}. Tried {} route(s):\n  {}",
        refusals.len(),
        refusals.join("\n  ")
    ))
}

/// Run one route end to end, or fail saying which hop broke.
fn run_conversion_route(
    source: &TagFile,
    route: &[String],
    definitions_root: &Path,
    templates: &dyn TemplateSource,
) -> Result<TagConversionDraft, String> {
    let mut carried: Option<TagFile> = None;
    let mut earlier_issues: Vec<ConversionIssue> = Vec::new();
    for (index, hop) in route.windows(2).enumerate() {
        let (from, to) = (hop[0].as_str(), hop[1].as_str());
        let input = carried.as_ref().unwrap_or(source);
        let mut draft = analyze_conversion_with_templates(
            input,
            from,
            to,
            definitions_root,
            templates.templates_for(to),
        )
        .map_err(|error| format!("{from} \u{2192} {to} failed: {error}"))?;

        let final_hop = index + 2 == route.len();
        if final_hop {
            // The route's loss, oldest first, ahead of this hop's own.
            earlier_issues.append(&mut draft.report.issues);
            draft.report.issues = earlier_issues;
            draft.route = if route.len() > 2 {
                route.to_vec()
            } else {
                Vec::new()
            };
            return Ok(draft);
        }

        // A companion tag synthesized mid-route has nowhere reviewed to go: the
        // rules that would carry it onward are written for the group it was
        // extracted from, not for a tag that only exists because of an earlier
        // hop. Refusing the route beats inventing a path for it, and the next
        // route may not need one.
        if !draft.companion_tags.is_empty() {
            return Err(format!(
                "{from} \u{2192} {to} produced {} companion tag(s), which cannot be carried \
                 through a further conversion",
                draft.companion_tags.len()
            ));
        }
        for issue in draft.report.issues.drain(..) {
            earlier_issues.push(ConversionIssue {
                kind: issue.kind,
                path: issue.path,
                message: format!("[{from} \u{2192} {to}] {}", issue.message),
            });
        }
        // Serialize and reparse, so the next hop reads what a saved tag would be.
        let bytes = draft
            .tag
            .write_to_bytes()
            .map_err(|error| format!("{from} \u{2192} {to} produced an unwritable tag: {error}"))?;
        carried = Some(reparse_intermediate(&bytes, to, definitions_root).map_err(|error| {
            format!("{from} \u{2192} {to} produced a tag that will not reopen: {error}")
        })?);
    }
    Err("A conversion route needs at least two profiles".to_owned())
}

/// Reparse a hop's output from its own bytes.
///
/// Classic containers take the JSON-layout path for the same reason
/// [`read_tag_for_conversion`] does: they carry no embedded layout, so
/// `read_from_bytes` cannot parse them and fails in a way that points anywhere
/// but at the reader.
fn reparse_intermediate(
    bytes: &[u8],
    game: &str,
    definitions_root: &Path,
) -> Result<TagFile, String> {
    if ClassicHeader::parse(bytes).is_some() {
        let (header, _) = ClassicHeader::parse(bytes).expect("checked above");
        let group_tag = u32::from_be_bytes(header.group_tag);
        let group_name =
            group_tag_to_extension(group_tag).ok_or("unknown group for classic intermediate")?;
        let definition = definitions_root.join(game).join(format!("{group_name}.json"));
        let layout = TagLayout::from_json(&definition)
            .map_err(|error| format!("failed to load {}: {error}", definition.display()))?;
        return read_classic_tag_file(bytes, layout).map_err(|error| error.to_string());
    }
    TagFile::read_from_bytes(bytes).map_err(|error| error.to_string())
}

pub fn analyze_conversion(
    source: &TagFile,
    source_game: &str,
    target_game: &str,
    definitions_root: &Path,
    target_tags_root: Option<&Path>,
) -> Result<TagConversionDraft, String> {
    let target_groups = GameTagIndex::load(definitions_root, target_game)?;
    let native_templates =
        target_tags_root.map(|root| NativeTemplateIndex::build(root, &target_groups));
    analyze_conversion_with_templates(
        source,
        source_game,
        target_game,
        definitions_root,
        native_templates.as_ref(),
    )
}

pub fn analyze_conversion_with_templates(
    source: &TagFile,
    source_game: &str,
    target_game: &str,
    definitions_root: &Path,
    native_templates: Option<&NativeTemplateIndex>,
) -> Result<TagConversionDraft, String> {
    // A classic source is fine in either byte order — reading is endian-aware and
    // Halo CE bodies are big-endian by design. A big-endian *MCC* tag is not: it
    // is an Xbox 360 or legacy debug build with no round trip, the same carve-out
    // `unsaveable_reason` makes.
    if source.classic_engine().is_none() && source.endian != Endian::Le {
        return Err("Only little-endian MCC tags can be converted".to_owned());
    }
    if source.classic_engine().is_some() && !CLASSIC_CONVERSION_GAMES.contains(&source_game) {
        return Err(format!(
            "{source_game} is not a classic profile, but this tag is a classic \
             {:?} container",
            source.classic_engine()
        ));
    }
    if !conversion_pair_supported(source_game, target_game) {
        return Err(unsupported_pair_message(source_game, target_game));
    }

    let source_groups = GameTagIndex::load(definitions_root, source_game)?;
    let target_groups = GameTagIndex::load(definitions_root, target_game)?;
    let source_group_name = source_groups
        .by_tag
        .get(&source.group().tag)
        .ok_or_else(|| {
            format!(
                "{} does not identify group {}",
                source_game,
                format_group_tag(source.group().tag)
            )
        })?;
    let mapping_catalog = ConversionMappingCatalog::load()?;
    let (target_group_tag, target_group_name) = resolve_target_group(
        source_group_name,
        &target_groups,
        &mapping_catalog,
        source_game,
        target_game,
    )
    .ok_or_else(|| format!("{target_game} has no {source_group_name} tag group"))?;
    let source_schema_path = definitions_root
        .join(source_game)
        .join(format!("{source_group_name}.json"));
    let schema_path = definitions_root
        .join(target_game)
        .join(format!("{target_group_name}.json"));
    let source_field_aliases = SchemaFieldAliases::load(&source_schema_path)?;
    let target_field_aliases = SchemaFieldAliases::load(&schema_path)?;
    let native_target = native_templates
        .map(|templates| {
            find_native_target_template(
                templates,
                target_group_tag,
                target_game,
                Some(&target_field_aliases),
                definitions_root,
            )
        })
        .transpose()?
        .flatten();
    for game in [source_game, target_game] {
        if let Some(reason) = mapping_catalog.unusable_schema_reason(source_group_name, game) {
            let native_layout_avoids_schema_construction = game == target_game
                && native_target.is_some()
                && source_group_name.eq_ignore_ascii_case("contrail_system");
            if native_layout_avoids_schema_construction || game == source_game {
                continue;
            }
            return Err(format!(
                "{game} {source_group_name} schema cannot be converted safely: {reason}"
            ));
        }
    }
    if let Some(reason) =
        mapping_catalog.incompatibility_reason(source_group_name, source_game, target_game)
    {
        let native_contrail_layout =
            native_target.is_some() && source_group_name.eq_ignore_ascii_case("contrail_system");
        if !native_contrail_layout {
            return Err(format!(
                "{source_game} and {target_game} {source_group_name} layouts are explicitly incompatible: {reason}"
            ));
        }
    }
    let (mut target, target_template) = if let Some((template, template_path)) = native_target {
        (template, Some(template_path))
    } else if CLASSIC_CONVERSION_GAMES.contains(&target_game) {
        // `TagFile::new` can only build an MCC container: it hard-codes
        // `TagContainer::Mcc` and `Endian::Le`, there is no `ClassicHeader`
        // writer, and Halo 2's root block header is never synthesized. So a
        // classic target has to start from a tag the kit authored.
        return Err(format!(
            "Converting to {target_game} needs a {target_group_name} tag from its \
             editing kit to start from, because a classic tag cannot be built \
             from a schema alone. Configure the {target_game} kit and make sure \
             it ships at least one {target_group_name}."
        ));
    } else {
        let mut target = TagFile::new(&schema_path).map_err(|error| {
            format!(
                "Could not create target tag from {}: {error}",
                schema_path.display()
            )
        })?;
        initialize_block_index_defaults(target.root_mut());
        (target, None)
    };
    apply_editing_kit_mcc_header(&mut target, target_game)?;

    let mut context = ConversionContext {
        source_groups: &source_groups,
        target_groups: &target_groups,
        source_field_aliases: &source_field_aliases,
        target_field_aliases: &target_field_aliases,
        mapping_catalog: &mapping_catalog,
        definitions_root,
        native_templates,
        source_game,
        target_game,
        group_name: source_group_name,
        report: TagConversionReport::default(),
        companion_tags: Vec::new(),
        fatal_error: None,
        root_matches: 0,
        wire_identical: HashMap::new(),
        payloads_left_behind: Vec::new(),
        resources_left_behind: Vec::new(),
    };
    // Groups whose substance is compiled geometry, not authored fields.
    //
    // A converted one is structurally valid and the numbers all carry, but the
    // meshes, collision hulls and rigid bodies were built by the *source* game's
    // tool from source art, and each generation's importer produces different
    // vertex formats, compression and node layouts. So the honest advice is to
    // reimport from the original assets rather than trust the upgrade. Stated as a
    // warning rather than a refusal because the conversion is still useful for
    // reading the settings across, which is what the user asked for: "cool and
    // somewhat useful", but reimport is what they should actually do.
    //
    // `bitmap` is deliberately absent — pixel data carried forward avoids a
    // recompression pass, which is the one case where the upgrade beats reimporting.
    const REIMPORT_INSTEAD: &[&str] = &["render_model", "physics_model", "collision_model"];
    if REIMPORT_INSTEAD
        .iter()
        .any(|group| group.eq_ignore_ascii_case(&target_group_name))
    {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: target_group_name.clone(),
            message: format!(
                "{target_group_name} holds geometry compiled by {source_game}'s own importer. \
                 The settings convert, but the meshes, collision and rigid-body data were \
                 built for a different engine's vertex formats and compression — reimport \
                 from the source art with {target_game}'s tool rather than relying on this."
            ),
        });
    }
    if let Some(template_path) = target_template.as_ref() {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: "target layout".to_owned(),
            message: format!(
                "Used native {target_group_name} layout template {} and cleared its values before conversion",
                template_path.display()
            ),
        });
    } else {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: "target layout".to_owned(),
            message: format!(
                "Used generated {target_group_name} layout; Baboon round-trip verification cannot prove native editing-kit stream compatibility"
            ),
        });
    }
    if let Some((source_path, target_path)) =
        mapping_catalog.struct_mapping(source_group_name, source_game, target_game)
    {
        let source_struct = struct_at_path(source.root(), source_path).ok_or_else(|| {
            format!(
                "Configured source struct path '{source_path}' was not found in {source_game} {source_group_name}"
            )
        })?;
        // A kit-authored template carries its *own* layout, which does not always
        // agree with the dumped JSON the rule was reviewed against — the same
        // divergence the particle work turned up. Fall back to root-to-root
        // rather than refusing a tag that converts fine without the reparent.
        if !convert_to_struct_path(
            source_struct,
            target.root_mut(),
            target_path,
            source_path,
            &mut context,
        ) {
            context.report.issues.push(ConversionIssue {
                kind: ConversionIssueKind::Warning,
                path: target_path.to_owned(),
                message: format!(
                    "Reviewed reparent target '{target_path}' is absent from this                      {target_game} {target_group_name} layout; converted root to root instead"
                ),
            });
            convert_struct(source.root(), target.root_mut(), "", true, &mut context);
        }
    } else {
        convert_struct(source.root(), target.root_mut(), "", true, &mut context);
    }
    if let Some(error) = context.fatal_error.take() {
        return Err(error);
    }
    if context.root_matches == 0 {
        return Err(format!(
            "{} and {} do not share a compatible root structure for {}",
            source_game, target_game, source_group_name
        ));
    }

    let dependency_schema = definitions_root
        .join(target_game)
        .join("tag_dependency_list.json");
    if dependency_schema.is_file() {
        target
            .rebuild_dependency_list(&dependency_schema)
            .map_err(|error| format!("Could not rebuild target dependencies: {error}"))?;
    } else {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: "dependency list".to_owned(),
            message: format!(
                "Target dependency schema is missing: {}",
                dependency_schema.display()
            ),
        });
    }

    validate_reference_fidelity(
        source,
        &target,
        &source_groups,
        &target_groups,
        source_group_name,
        source_game,
        target_game,
        &mapping_catalog,
        &mut context.report,
    )?;
    strip_cross_engine_scripts(&mut target, &mut context);
    validate_critical_runtime_safety(source, &context)?;

    let target_extension = group_tag_to_extension(target_group_tag)
        .unwrap_or(&target_group_name)
        .to_owned();
    Ok(TagConversionDraft {
        tag: target,
        companion_tags: context.companion_tags,
        report: context.report,
        target_group_name,
        target_extension,
        native_layout_template: target_template,
        route: Vec::new(),
    })
}

/// A scenario's compiled HaloScript, which never survives an engine change.
///
/// The string table, the syntax datums that index into it, and the script and
/// global declarations that index those. All or nothing: they are one artefact
/// spread across five fields.
const COMPILED_SCRIPT_FIELDS: &[&str] = &[
    "script string data",
    "script syntax data",
    "hs syntax datums",
    "scripts",
    "globals",
];

/// Empty a converted scenario's compiled scripts, and say why.
///
/// A compiled script node stores an *index* into its own game's function table,
/// and those tables are renumbered at every engine boundary. Measured against
/// Baboon's own per-game script documentation: Halo 2 declares 887 functions and
/// Halo 3 1,377; the two agree position-for-position for the first 20 entries and
/// then diverge, and of the 628 names both games have, **608 sit at a different
/// index**. Even the closest pair, Halo 3 and ODST, moves 1,113 of 1,293.
///
/// So carrying the bytecode across is not a lossy copy, it is a wrong one: the
/// scenario would load and then call whatever function now occupies each slot.
/// That is worse than arriving with no scripts, because it looks like it worked.
///
/// The `.hsc` **source** is a different matter and is deliberately left alone —
/// it is text, it carries fine, and 47 of the 68 scenarios Halo 2's kit ships
/// bring it along. Recompiling from it in the destination game's tools is the
/// route that produces correct bytecode, and the warning says so.
fn strip_cross_engine_scripts(target: &mut TagFile, context: &mut ConversionContext<'_>) {
    if !context.group_name.eq_ignore_ascii_case("scenario")
        || context.source_game == context.target_game
    {
        return;
    }
    let mut emptied = Vec::new();
    {
        let mut root = target.root_mut();
        let ordinals: Vec<(usize, String)> = root
            .as_ref()
            .fields()
            .enumerate()
            .filter(|(_, field)| {
                let key = clean_field_key(field.name());
                COMPILED_SCRIPT_FIELDS.iter().any(|name| key == *name)
            })
            .map(|(ordinal, field)| (ordinal, field.name().to_owned()))
            .collect();
        for (ordinal, name) in ordinals {
            let Some(mut field) = root.field_at_mut(ordinal) else {
                continue;
            };
            if let Some(mut block) = field.as_block_mut() {
                if block.len() > 0 {
                    block.clear();
                    emptied.push(name);
                }
                continue;
            }
            let filled = matches!(field.as_ref().value(), Some(TagFieldData::Data(bytes)) if !bytes.is_empty());
            if filled && field.set(TagFieldData::Data(Vec::new())).is_ok() {
                emptied.push(name);
            }
        }
    }
    // The payload check must stop treating the string table as lost data: it was
    // not dropped for want of a home, it was removed on purpose.
    context
        .payloads_left_behind
        .retain(|path| !COMPILED_SCRIPT_FIELDS.iter().any(|name| path == name));
    if emptied.is_empty() {
        return;
    }
    let sources = target
        .root()
        .fields()
        .find(|field| clean_field_key(field.name()) == "source files")
        .and_then(|field| field.as_block())
        .map(|block| block.len())
        .unwrap_or(0);
    let advice = if sources > 0 {
        format!(
            "The {sources} .hsc source file(s) came across with the tag \u{2014} recompile them \
             with {}'s tools to get working scripts.",
            context.target_game
        )
    } else {
        "This scenario carried no .hsc source, so the scripts have to be reauthored.".to_owned()
    };
    context.report.issues.push(ConversionIssue {
        kind: ConversionIssueKind::Warning,
        path: emptied.join(", "),
        message: format!(
            "Compiled scripts were cleared rather than carried. A script node indexes its own \
             game's function table, and {} renumbers {}'s \u{2014} the two tables diverge within \
             the first two dozen entries, so carried bytecode would call the wrong functions. {advice}",
            context.target_game, context.source_game
        ),
    });
}

fn validate_critical_runtime_safety(
    source: &TagFile,
    context: &ConversionContext<'_>,
) -> Result<(), String> {
    // Not "does the source have resources?" but "did any resource fail to
    // cross?". The old question refused every animation graph, because an
    // animation graph's payload *is* a pageable resource — it is why a loose
    // HREK jmad runs to a hundred megabytes and more.
    if !context.resources_left_behind.is_empty() {
        return Err(format!(
            "{} carries {} pageable resource(s) that could not be translated from {} to {} ({}); \
             the tag was not written",
            context.group_name,
            context.resources_left_behind.len(),
            context.source_game,
            context.target_game,
            context
                .resources_left_behind
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    // Same question as the resource check, for inline `data` blobs. A reviewed
    // `accepted_field_drops` rule can still say a particular blob is fine to
    // lose; anything else refuses, because the alternative is a tag that opens
    // to nothing or takes the kit down with it.
    let unreviewed_payloads: Vec<&String> = context
        .payloads_left_behind
        .iter()
        .filter(|path| {
            context
                .mapping_catalog
                .accepted_drop_reason(
                    context.group_name,
                    context.source_game,
                    context.target_game,
                    path,
                )
                .is_none()
                && context
                    .mapping_catalog
                    .accepted_payload_drop_reason(
                        context.group_name,
                        context.source_game,
                        context.target_game,
                        path,
                    )
                    .is_none()
        })
        .collect();
    if !unreviewed_payloads.is_empty() {
        return Err(format!(
            "{} carries {} data blob(s) that {} stores differently from {} ({}); the bytes are \
             the substance of the tag, so it was not written rather than written empty",
            context.group_name,
            unreviewed_payloads.len(),
            context.source_game,
            context.target_game,
            unreviewed_payloads
                .iter()
                .take(3)
                .map(|path| path.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        ));
    }
    let _ = source;
    const FAIL_CLOSED_GROUPS: &[&str] = &[
        "model_animation_graph",
        "damage_effect",
        "effect",
        "lens_flare",
        "light",
        "particle",
    ];
    let critical_issues = context
        .report
        .issues
        .iter()
        .filter(|issue| issue.kind == ConversionIssueKind::Unsupported)
        .filter(|issue| {
            context
                .mapping_catalog
                .accepted_drop_reason(
                    context.group_name,
                    context.source_game,
                    context.target_game,
                    &issue.path,
                )
                .is_none()
        })
        .filter(|issue| {
            !(context
                .group_name
                .eq_ignore_ascii_case("model_animation_graph")
                && ["desired compression", "current compression"]
                    .iter()
                    .any(|field| clean_field_key(&issue.path).ends_with(field)))
        })
        .collect::<Vec<_>>();
    let animation_graph = context
        .group_name
        .eq_ignore_ascii_case("model_animation_graph");
    let audited_h3_to_reach = ["halo3_mcc", "halo3odst_mcc"].contains(&context.source_game)
        && context.target_game == "haloreach_mcc";
    if (animation_graph
        || (audited_h3_to_reach
            && FAIL_CLOSED_GROUPS
                .iter()
                .any(|group| context.group_name.eq_ignore_ascii_case(group))))
        && !critical_issues.is_empty()
    {
        let examples = critical_issues
            .iter()
            .take(4)
            .map(|issue| issue.path.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "{} conversion would lose {} meaningful runtime or authored field(s) ({examples}); the tag was not written",
            context.group_name,
            critical_issues.len()
        ));
    }
    Ok(())
}


fn struct_at_path<'a>(mut structure: TagStruct<'a>, path: &str) -> Option<TagStruct<'a>> {
    for component in path.split('/').filter(|component| !component.is_empty()) {
        structure = structure
            .fields()
            .find(|field| clean_field_key(field.name()) == clean_field_key(component))?
            .as_struct()?;
    }
    Some(structure)
}

/// Convert `source` into the struct at `path` inside `target`.
///
/// `source_prefix` is where `source` sits in the *source* tag, and it becomes the
/// path every issue inside is reported at. That matters because the
/// reference-fidelity check explains a dropped reference by looking for an issue at
/// (or above) the field's absolute source path: with a reparent whose source is
/// nested — `shader`'s `render_method` is the first — reporting `definition`
/// instead of `render_method/definition` left every dropped reference looking
/// "unexplained", i.e. like a field-matching bug, and turned a reviewed loss into a
/// fatal error.
fn convert_to_struct_path(
    source: TagStruct<'_>,
    mut target: TagStructMut<'_>,
    path: &str,
    source_prefix: &str,
    context: &mut ConversionContext<'_>,
) -> bool {
    let mut components = path.split('/').filter(|component| !component.is_empty());
    let Some(component) = components.next() else {
        convert_struct(source, target, source_prefix, true, context);
        return true;
    };
    let remainder = components.collect::<Vec<_>>().join("/");
    let ordinal = target
        .as_ref()
        .fields()
        .enumerate()
        .find(|(_, field)| clean_field_key(field.name()) == clean_field_key(component))
        .map(|(ordinal, _)| ordinal);
    let Some(ordinal) = ordinal else {
        return false;
    };
    let Some(mut field) = target.field_at_mut(ordinal) else {
        return false;
    };
    let Some(nested) = field.as_struct_mut() else {
        return false;
    };
    convert_to_struct_path(source, nested, &remainder, source_prefix, context)
}

/// Whether a candidate on disk is an acceptable native layout template for
/// `target_game`.
///
/// The rule everywhere else is "not one of our own conversion drafts", and a
/// draft is recognized by `version == -1` where an editing-kit tag carries a
/// real one. That rule *inverts* for Campaign Evolved, whose shipped generation
/// is exactly `1 / 2 / 0xffffffff` — all 12,289 of its tag blobs read it. Left
/// unqualified the check rejects the entire game, and silently: the converter
/// would simply fall back to a generated layout every time with nothing saying
/// why.
/// How many of a group's shipped tags to consider before concluding the kit has
/// no usable template for it.
///
/// A bound is needed because *proving there is none* is the expensive case, and
/// it is a real case: Halo Reach ships 10,675 bitmaps and not one carries a
/// source revision, so the search rejects every last one. Opening a file costs
/// far more than reading its header — 10,675 opens is ~15 seconds here even at
/// 64 bytes each — so the count, not the bytes, is what has to be capped.
///
/// 256 is chosen against measurement rather than taste. In every Halo Reach
/// group that has an acceptable tag at all, the first one sits at index 1 or 2,
/// and between 15% and 90% of the group qualifies. The cap is two orders of
/// magnitude past where the answer has ever been found.
///
/// The cost of being wrong is bounded and visible: the conversion falls back to
/// a generated layout and the report says so, which is exactly what a group with
/// no acceptable tag already gets.
const NATIVE_TEMPLATE_SCAN_LIMIT: usize = 256;

/// Whether a candidate's header marks it as a kit-authored tag worth starting
/// from.
///
/// Takes the header rather than the tag so the test can be applied to a
/// candidate that has not been parsed — which is the whole point, since a group
/// may ship thousands of candidates and the answer is 64 bytes in. See
/// [`TagFileHeader::peek`].
fn accepts_native_header(header: &TagFileHeader, target_game: &str) -> bool {
    if target_game == CAMPAIGN_EVOLVED_GAME {
        let (build_version, build_number, version) = CAMPAIGN_EVOLVED_GENERATION;
        return header.build_version == build_version
            && header.build_number == build_number
            && header.version == version;
    }
    header.version != u32::MAX
}

fn find_native_target_template(
    templates: &NativeTemplateIndex,
    target_group_tag: u32,
    target_game: &str,
    engine_managed: Option<&SchemaFieldAliases>,
    definitions_root: &Path,
) -> Result<Option<(TagFile, PathBuf)>, String> {
    // A classic target needs a kit-authored tag to start from — `TagFile::new`
    // only builds MCC containers — but a classic tag cannot be read by
    // `TagFile::read` (no embedded layout) nor cached as bytes and re-read
    // (`read_from_bytes` would not parse it either). So it takes its own pass:
    // read through the JSON layout, and skip the byte cache.
    if CLASSIC_CONVERSION_GAMES.contains(&target_game) {
        let Some(paths) = templates.by_group.get(&target_group_tag) else {
            return Ok(None);
        };
        for path in paths.iter().take(NATIVE_TEMPLATE_SCAN_LIMIT) {
            let Ok(mut tag) = read_tag_for_conversion(
                path,
                Some(target_game),
                Some(definitions_root),
                target_group_tag,
            ) else {
                continue;
            };
            if tag.group().tag == target_group_tag
                && tag.classic_engine().is_some()
                && reset_tag_to_defaults(&mut tag, engine_managed).is_ok()
            {
                return Ok(Some((tag, path.to_path_buf())));
            }
        }
        return Ok(None);
    }
    {
        let cached = templates.cached.borrow();
        if let Some(value) = cached.get(&target_group_tag) {
            return match value {
                Some((bytes, path)) => TagFile::read_from_bytes(bytes)
                    .map(|tag| Some((tag, path.clone())))
                    .map_err(|error| format!("Could not restore cached native template: {error}")),
                None => Ok(None),
            };
        }
    }
    let Some(paths) = templates.by_group.get(&target_group_tag) else {
        templates.cached.borrow_mut().insert(target_group_tag, None);
        return Ok(None);
    };
    for path in paths.iter().take(NATIVE_TEMPLATE_SCAN_LIMIT) {
        // Sift on the 64-byte header first. Every candidate has to be *ruled
        // out* somehow, and for a group the kit ships in bulk the ruled-out ones
        // are nearly all of them: Halo Reach ships 10,675 bitmaps, 6.4 GB, and
        // not one carries a source revision — so a scan that parsed each to read
        // its header spent half a minute proving there was nothing to find. The
        // full checks below still run on whatever survives; this only decides
        // what is worth opening.
        let Ok((header, endian)) = TagFileHeader::peek(path) else {
            continue;
        };
        if endian != Endian::Le
            || header.group_tag != target_group_tag
            || !accepts_native_header(&header, target_game)
        {
            continue;
        }
        let Ok(mut tag) = TagFile::read(path) else {
            continue;
        };
        // Prefer a game-authored tag so its embedded layout carries the
        // expansions a dumped JSON schema need not describe.
        if tag.group().tag == target_group_tag
            && tag.classic_engine().is_none()
            && tag.endian == Endian::Le
            && accepts_native_header(&tag.header, target_game)
            && reset_tag_to_defaults(&mut tag, engine_managed).is_ok()
        {
            let bytes = tag.write_to_bytes().map_err(|error| {
                format!(
                    "Could not cache native template {}: {error}",
                    path.display()
                )
            })?;
            templates
                .cached
                .borrow_mut()
                .insert(target_group_tag, Some((bytes, path.clone())));
            return Ok(Some((tag, path.to_path_buf())));
        }
    }
    templates.cached.borrow_mut().insert(target_group_tag, None);
    Ok(None)
}

fn create_companion_tag(
    key: &str,
    file_suffix: &str,
    group_name: &str,
    context: &ConversionContext<'_>,
) -> Result<CompanionTagDraft, String> {
    let group_tag = context
        .target_groups
        .by_name
        .get(&group_name.to_ascii_lowercase())
        .copied()
        .ok_or_else(|| format!("{} has no {group_name} tag group", context.target_game))?;
    let native_target = context
        .native_templates
        .map(|templates| {
            find_native_target_template(
                templates,
                group_tag,
                context.target_game,
                Some(context.target_field_aliases),
                context.definitions_root,
            )
        })
        .transpose()?
        .flatten();
    let schema = context
        .definitions_root
        .join(context.target_game)
        .join(format!("{group_name}.json"));
    let (mut tag, native_layout_template) = if let Some((template, template_path)) = native_target {
        (template, Some(template_path))
    } else {
        let mut tag = TagFile::new(&schema).map_err(|error| {
            format!(
                "Could not create companion {group_name} tag from {}: {error}",
                schema.display()
            )
        })?;
        initialize_block_index_defaults(tag.root_mut());
        (tag, None)
    };
    apply_editing_kit_mcc_header(&mut tag, context.target_game)?;
    let extension = group_tag_to_extension(group_tag)
        .unwrap_or(group_name)
        .to_owned();
    Ok(CompanionTagDraft {
        key: key.to_owned(),
        file_suffix: file_suffix.to_owned(),
        group_name: group_name.to_owned(),
        extension,
        tag,
        native_layout_template,
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ReferenceValue {
    pub field_path: String,
    pub group_tag: u32,
    pub tag_path: String,
}

pub fn collect_reference_values(
    structure: TagStruct<'_>,
    parent_path: &str,
    values: &mut Vec<ReferenceValue>,
) {
    for field in structure.fields() {
        let key = clean_field_key(field.name());
        let field_path = join_path(
            parent_path,
            if key.is_empty() {
                field.type_name()
            } else {
                &key
            },
        );
        if let Some(TagFieldData::TagReference(reference)) = field.value()
            && let Some((group_tag, path)) = reference.group_tag_and_name
            && !path.is_empty()
            && !path.eq_ignore_ascii_case("none")
        {
            values.push(ReferenceValue {
                field_path,
                group_tag,
                tag_path: path,
            });
            continue;
        }
        if let Some(nested) = field.as_struct() {
            collect_reference_values(nested, &field_path, values);
        } else if let Some(block) = field.as_block() {
            for (index, element) in block.iter().enumerate() {
                collect_reference_values(element, &format!("{field_path}[{index}]"), values);
            }
        } else if let Some(array) = field.as_array() {
            for (index, element) in array.iter().enumerate() {
                collect_reference_values(element, &format!("{field_path}[{index}]"), values);
            }
        }
    }
}

fn validate_reference_fidelity(
    source: &TagFile,
    target: &TagFile,
    source_groups: &GameTagIndex,
    target_groups: &GameTagIndex,
    group_name: &str,
    source_game: &str,
    target_game: &str,
    mapping_catalog: &ConversionMappingCatalog,
    report: &mut TagConversionReport,
) -> Result<(), String> {
    let mut source_values = Vec::new();
    collect_reference_values(source.root(), "", &mut source_values);
    let mut expected = HashSet::<(u32, String, String)>::new();
    for reference in source_values {
        if let Some(reason) = mapping_catalog.reference_drop_reason(
            group_name,
            source_game,
            target_game,
            &reference.field_path,
        ) {
            report.issues.push(ConversionIssue {
                kind: ConversionIssueKind::Warning,
                path: reference.field_path,
                message: format!(
                    "Target schema has no safe slot for reference {}: {reason}",
                    reference.tag_path
                ),
            });
            continue;
        }
        // The source profile's `_meta.json` does not name this group, so there is
        // nothing to look the target up by. Nothing correct can be done with it,
        // and refusing the whole tag over one unnameable reference is worse than
        // reporting it — the same reasoning as a target that has no such class.
        let Some(source_group_name) = source_groups.by_tag.get(&reference.group_tag) else {
            report.issues.push(ConversionIssue {
                kind: ConversionIssueKind::Warning,
                path: reference.field_path.clone(),
                message: format!(
                    "{source_game} does not name group {}, so the reference to {} was                      left empty — reconnect it by hand",
                    format_group_tag(reference.group_tag),
                    reference.tag_path,
                ),
            });
            report.dropped_references += 1;
            continue;
        };
        let Some((target_group, _)) = resolve_target_group(
            source_group_name,
            target_groups,
            mapping_catalog,
            source_game,
            target_game,
        ) else {
            // The target game has no such tag class at all. No correct
            // implementation could preserve this reference, so refusing the
            // whole tag over it says nothing about the conversion's quality —
            // it just makes the tag unconvertible.
            //
            // Halo Reach's `model` points at a `render_model`; Campaign Evolved
            // replaced Halo's render geometry with Unreal skeletal meshes and
            // defines no such group. Every Reach model refuses on that, and
            // will keep refusing however good the field matching gets.
            //
            // So report it, precisely enough to act on: the field to fill in
            // and the tag path that used to be there.
            report.issues.push(ConversionIssue {
                kind: ConversionIssueKind::Warning,
                path: reference.field_path,
                message: format!(
                    "{target_game} has no {source_group_name} group, so this reference to {} \
                     was left empty — reconnect it by hand if the target needs one",
                    reference.tag_path,
                ),
            });
            report.dropped_references += 1;
            continue;
        };
        expected.insert((target_group, reference.tag_path, reference.field_path));
    }

    let mut actual_values = Vec::new();
    collect_reference_values(target.root(), "", &mut actual_values);
    let actual = actual_values
        .into_iter()
        .map(|value| (value.group_tag, value.tag_path))
        .collect::<HashSet<_>>();

    // A reference that did not arrive is one of two very different things, and
    // this check is only worth having if it can tell them apart.
    //
    // If the conversion already reported a problem at that field, the target
    // simply has no home for it -- Campaign Evolved's biped declares no
    // fireteam name, Reach's does. That is a fact about the games, and the
    // author reconnects what they need.
    //
    // If nothing was reported, the field matched and the value vanished
    // anyway. That is a bug in field matching, and it is what this check
    // exists to catch, so it stays fatal.
    let mut unexplained = Vec::new();
    for (group, tag_path, field_path) in expected {
        if actual.contains(&(group, tag_path.clone())) {
            continue;
        }
        let explained = report.issues.iter().any(|issue| {
            crate::TagFieldPath::parse(&clean_field_key(&issue.path))
                .is_ancestor_of(&crate::TagFieldPath::parse(&clean_field_key(&field_path)))
        });
        if explained {
            report.dropped_references += 1;
            report.issues.push(ConversionIssue {
                kind: ConversionIssueKind::Warning,
                path: field_path,
                message: format!(
                    "Reference to {}:{tag_path} was left empty — reconnect it by hand if the                      target needs one",
                    format_group_tag(group),
                ),
            });
            continue;
        }
        unexplained.push(format!("{}:{tag_path} (at {field_path})", format_group_tag(group)));
    }

    if unexplained.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Conversion would lose {} tag reference(s) the target does have a field for,              which means field matching went wrong rather than the games differing: {}",
            unexplained.len(),
            unexplained.join(", ")
        ))
    }
}

fn reset_tag_to_defaults(
    tag: &mut TagFile,
    engine_managed: Option<&SchemaFieldAliases>,
) -> Result<(), String> {
    reset_struct_to_defaults(tag.root_mut(), "", engine_managed)?;
    tag.remove_dependency_list();
    tag.remove_import_info();
    tag.remove_asset_depot_storage();
    Ok(())
}

/// Is this struct an inlined render method?
///
/// `options`, `parameters` and `postprocess` appear together in the render
/// method and nowhere else, which makes the trio a reliable signature where the
/// struct's own name is not: Reach hides the whole body in an unnamed `tmpl`
/// hole, and a shipped tag's expanded layout gives the fields no distinguishing
/// parent to test against.
fn is_render_method_struct(value: TagStruct<'_>) -> bool {
    let mut options = false;
    let mut parameters = false;
    let mut postprocess = false;
    for field in value.fields() {
        match clean_field_key(field.name()).as_str() {
            "options" => options = true,
            "parameters" => parameters = true,
            "postprocess" | "postprocess definition" => postprocess = true,
            _ => {}
        }
    }
    options && parameters && postprocess
}

fn reset_struct_to_defaults(
    mut value: TagStructMut<'_>,
    path: &str,
    engine_managed: Option<&SchemaFieldAliases>,
) -> Result<(), String> {
    let field_count = value.as_ref().fields().count();
    // A render method's own `definition` reference survives the reset.
    //
    // The fx groups inline a render method into the tag, and which one it is
    // rides on a single `rmdf` reference. A Reach particle with that reference
    // empty crashes the mod tools, and the schema cannot protect it: Reach keeps
    // the whole render method inside a `tmpl` hole, so no fx schema declares a
    // `definition` field at all and neither the `*` nor the `!` annotation is
    // available. A kit-authored template names it only because its *shipped*
    // layout expands the hole.
    //
    // Recognised by shape, not by group or by name alone — `definition` is far
    // too common to blanket-preserve, but a struct carrying `options`,
    // `parameters` and `postprocess` together is a render method and nothing
    // else. A source with its own `definition` still overwrites this during
    // conversion, so it only decides what happens when none can be supplied.
    let render_method = is_render_method_struct(value.as_ref());
    for ordinal in 0..field_count {
        let Some(mut field) = value.field_at_mut(ordinal) else {
            continue;
        };
        if render_method
            && field.as_ref().field_type() == TagFieldType::TagReference
            && clean_field_key(field.as_ref().name()) == "definition"
        {
            continue;
        }
        // Engine-managed fields keep whatever the kit-authored template put
        // there. Zeroing them writes a value nothing shipped: every Halo Reach
        // particle carries `version!` 2 (one carries 1, none carries 0), and a
        // converted particle with 0 crashed the Reach mod tools. The template is
        // a real tag the kit wrote, so its own answer is the best one available —
        // and a source field of the same name still overwrites it during
        // conversion, so this only affects fields the source does not have.
        //
        // The same reasoning `apply_editing_kit_mcc_header` already records for
        // the file header: `TagFile::new` zeroes it, which the library parses
        // happily and the native tools reject.
        let key = clean_field_key(field.as_ref().name());
        // Also skipped: `*` editor-owned fields. A shipped Reach particle's
        // `actual shader?` carries `definition*` -> `shaders\particle
        // .render_method_definition`, and a particle with no render-method
        // definition crashes the mod tools. Halo 3 cannot supply that value —
        // its own render method is inlined under different field names — so the
        // kit template's is the only correct one available. `*` means the editor
        // derives this, not the author, which is precisely when the template
        // beats a zero.
        if engine_managed.is_some_and(|table| {
            table.is_engine_managed(&key) || table.is_editor_owned(&key)
        }) {
            continue;
        }
        let field_path = join_path(
            path,
            if key.is_empty() {
                field.as_ref().type_name()
            } else {
                &key
            },
        );
        match field.as_ref().field_type() {
            TagFieldType::Struct => {
                if let Some(nested) = field.as_struct_mut() {
                    reset_struct_to_defaults(nested, &field_path, engine_managed)?;
                }
            }
            TagFieldType::Block => {
                if let Some(mut block) = field.as_block_mut() {
                    block.clear();
                }
            }
            TagFieldType::Array => {
                if let Some(mut array) = field.as_array_mut() {
                    for index in 0..array.len() {
                        if let Some(element) = array.element_mut(index) {
                            reset_struct_to_defaults(element, &format!("{field_path}[{index}]"), engine_managed)?;
                        }
                    }
                }
            }
            TagFieldType::PageableResource => {
                if field
                    .as_ref()
                    .as_resource()
                    .is_some_and(|resource| !matches!(resource.kind(), TagResourceKind::Null))
                {
                    return Err(format!(
                        "Native template has a non-null pageable resource at {field_path}"
                    ));
                }
            }
            TagFieldType::ApiInterop => {
                field
                    .set(TagFieldData::ApiInterop(ApiInteropData::reset()))
                    .map_err(|error| format!("Could not reset {field_path}: {error:?}"))?;
            }
            _ => {
                let Some(current) = field.as_ref().value() else {
                    continue;
                };
                if let Some(default) = default_field_value(current) {
                    field
                        .set(default)
                        .map_err(|error| format!("Could not reset {field_path}: {error:?}"))?;
                }
            }
        }
    }
    Ok(())
}

/// A newly allocated schema struct is byte-zeroed, but a block index uses -1
/// as its null value. Native post-processing treats zero as a real index, so
/// leaving a target-only index at the allocator default can make an otherwise
/// valid converted tag assert while loading.
fn initialize_block_index_defaults(mut value: TagStructMut<'_>) {
    let field_count = value.as_ref().fields().count();
    for ordinal in 0..field_count {
        let Some(mut field) = value.field_at_mut(ordinal) else {
            continue;
        };
        match field.as_ref().field_type() {
            TagFieldType::Struct => {
                if let Some(nested) = field.as_struct_mut() {
                    initialize_block_index_defaults(nested);
                }
            }
            TagFieldType::Array => {
                if let Some(mut array) = field.as_array_mut() {
                    for index in 0..array.len() {
                        if let Some(element) = array.element_mut(index) {
                            initialize_block_index_defaults(element);
                        }
                    }
                }
            }
            TagFieldType::CharBlockIndex => {
                let _ = field.set(TagFieldData::CharBlockIndex(-1));
            }
            TagFieldType::CustomCharBlockIndex => {
                let _ = field.set(TagFieldData::CustomCharBlockIndex(-1));
            }
            TagFieldType::ShortBlockIndex => {
                let _ = field.set(TagFieldData::ShortBlockIndex(-1));
            }
            TagFieldType::CustomShortBlockIndex => {
                let _ = field.set(TagFieldData::CustomShortBlockIndex(-1));
            }
            TagFieldType::LongBlockIndex => {
                let _ = field.set(TagFieldData::LongBlockIndex(-1));
            }
            TagFieldType::CustomLongBlockIndex => {
                let _ = field.set(TagFieldData::CustomLongBlockIndex(-1));
            }
            _ => {}
        }
    }
}

fn default_field_value(value: TagFieldData) -> Option<TagFieldData> {
    Some(match value {
        TagFieldData::String(_) => TagFieldData::String(String::new()),
        TagFieldData::LongString(_) => TagFieldData::LongString(String::new()),
        TagFieldData::StringId(_) => TagFieldData::StringId(StringIdData {
            string: String::new(),
        }),
        TagFieldData::OldStringId(_) => TagFieldData::OldStringId(StringIdData {
            string: String::new(),
        }),
        TagFieldData::TagReference(_) => TagFieldData::TagReference(TagReferenceData {
            group_tag_and_name: None,
        }),
        TagFieldData::Data(_) => TagFieldData::Data(Vec::new()),
        TagFieldData::ApiInterop(_) => TagFieldData::ApiInterop(ApiInteropData::reset()),
        TagFieldData::CharInteger(_) => TagFieldData::CharInteger(0),
        TagFieldData::ShortInteger(_) => TagFieldData::ShortInteger(0),
        TagFieldData::LongInteger(_) => TagFieldData::LongInteger(0),
        TagFieldData::Int64Integer(_) => TagFieldData::Int64Integer(0),
        TagFieldData::ByteInteger(_) => TagFieldData::ByteInteger(0),
        TagFieldData::WordInteger(_) => TagFieldData::WordInteger(0),
        TagFieldData::DwordInteger(_) => TagFieldData::DwordInteger(0),
        TagFieldData::QwordInteger(_) => TagFieldData::QwordInteger(0),
        TagFieldData::Tag(_) => TagFieldData::Tag(0),
        TagFieldData::CharEnum { .. } => TagFieldData::CharEnum {
            value: 0,
            name: None,
        },
        TagFieldData::ShortEnum { .. } => TagFieldData::ShortEnum {
            value: 0,
            name: None,
        },
        TagFieldData::LongEnum { .. } => TagFieldData::LongEnum {
            value: 0,
            name: None,
        },
        TagFieldData::ByteFlags { .. } => TagFieldData::ByteFlags {
            value: 0,
            names: Vec::new(),
        },
        TagFieldData::WordFlags { .. } => TagFieldData::WordFlags {
            value: 0,
            names: Vec::new(),
        },
        TagFieldData::LongFlags { .. } => TagFieldData::LongFlags {
            value: 0,
            names: Vec::new(),
        },
        TagFieldData::ByteBlockFlags(_) => TagFieldData::ByteBlockFlags(0),
        TagFieldData::WordBlockFlags(_) => TagFieldData::WordBlockFlags(0),
        TagFieldData::LongBlockFlags(_) => TagFieldData::LongBlockFlags(0),
        TagFieldData::CharBlockIndex(_) => TagFieldData::CharBlockIndex(-1),
        TagFieldData::CustomCharBlockIndex(_) => TagFieldData::CustomCharBlockIndex(-1),
        TagFieldData::ShortBlockIndex(_) => TagFieldData::ShortBlockIndex(-1),
        TagFieldData::CustomShortBlockIndex(_) => TagFieldData::CustomShortBlockIndex(-1),
        TagFieldData::LongBlockIndex(_) => TagFieldData::LongBlockIndex(-1),
        TagFieldData::CustomLongBlockIndex(_) => TagFieldData::CustomLongBlockIndex(-1),
        TagFieldData::Angle(_) => TagFieldData::Angle(0.0),
        TagFieldData::Real(_) => TagFieldData::Real(0.0),
        TagFieldData::RealSlider(_) => TagFieldData::RealSlider(0.0),
        TagFieldData::RealFraction(_) => TagFieldData::RealFraction(0.0),
        TagFieldData::Point2d(_) => TagFieldData::Point2d(Default::default()),
        TagFieldData::Rectangle2d(_) => TagFieldData::Rectangle2d(Default::default()),
        TagFieldData::RealPoint2d(_) => TagFieldData::RealPoint2d(Default::default()),
        TagFieldData::RealPoint3d(_) => TagFieldData::RealPoint3d(Default::default()),
        TagFieldData::RealVector2d(_) => TagFieldData::RealVector2d(Default::default()),
        TagFieldData::RealVector3d(_) => TagFieldData::RealVector3d(Default::default()),
        TagFieldData::RealQuaternion(_) => TagFieldData::RealQuaternion(Default::default()),
        TagFieldData::RealEulerAngles2d(_) => TagFieldData::RealEulerAngles2d(Default::default()),
        TagFieldData::RealEulerAngles3d(_) => TagFieldData::RealEulerAngles3d(Default::default()),
        TagFieldData::RealPlane2d(_) => TagFieldData::RealPlane2d(Default::default()),
        TagFieldData::RealPlane3d(_) => TagFieldData::RealPlane3d(Default::default()),
        TagFieldData::RgbColor(_) => TagFieldData::RgbColor(Default::default()),
        TagFieldData::ArgbColor(_) => TagFieldData::ArgbColor(Default::default()),
        TagFieldData::RealRgbColor(_) => TagFieldData::RealRgbColor(Default::default()),
        TagFieldData::RealArgbColor(_) => TagFieldData::RealArgbColor(Default::default()),
        TagFieldData::RealHsvColor(_) => TagFieldData::RealHsvColor(Default::default()),
        TagFieldData::RealAhsvColor(_) => TagFieldData::RealAhsvColor(Default::default()),
        TagFieldData::ShortIntegerBounds(_) => TagFieldData::ShortIntegerBounds(Default::default()),
        TagFieldData::AngleBounds(_) => TagFieldData::AngleBounds(Default::default()),
        TagFieldData::RealBounds(_) => TagFieldData::RealBounds(Default::default()),
        TagFieldData::FractionBounds(_) => TagFieldData::FractionBounds(Default::default()),
        TagFieldData::Custom(bytes) => TagFieldData::Custom(vec![0; bytes.len()]),
    })
}

/// One cleaned field name to trace through the matcher, from `BLAM_DEBUG_FIELD`.
///
/// A field that fails to map is the converter's characteristic failure, and its
/// four match clauses can each look satisfied in isolation while the whole
/// condition does not hold. This prints what the matcher sees instead.
static DEBUG_FIELD: std::sync::LazyLock<Option<String>> =
    std::sync::LazyLock::new(|| std::env::var("BLAM_DEBUG_FIELD").ok());

fn convert_struct(
    source: TagStruct<'_>,
    mut target: TagStructMut<'_>,
    path: &str,
    root: bool,
    context: &mut ConversionContext<'_>,
) {
    let source_guid = source.definition().guid();
    let target_guid = target.as_ref().definition().guid();
    let source_struct_name = source.definition().name().to_owned();
    let target_struct_name = target.as_ref().definition().name().to_owned();
    // Two all-zero GUIDs are not evidence of anything: every classic struct has
    // one. Require a real GUID before treating the pair as the same type, which
    // is what unlocks empty-name matching and verbatim `Data`/`Custom` copies.
    let same_guid = source_guid == target_guid && source_guid != [0u8; 16];
    // A second identity key, for the profiles that have no GUIDs at all.
    //
    // Every Halo 1 and Halo 2 struct carries an all-zero GUID, so `same_guid` is
    // permanently false for a classic pair and the verbatim `Data`/`Custom` copy
    // path is unreachable — which is why a Halo 1 bitmap's `processed pixel data`
    // was refused on the way to Halo 2 even though both games store the same
    // bytes in the same field. Wire-identical struct trees prove the pair
    // describes the same thing without needing a GUID to say so.
    //
    // Used *only* to unlock the opaque-copy path. Empty-name matching still
    // requires a real GUID, because congruence is a weaker claim: it says the
    // shapes agree, not that the structs share a lineage.
    let structurally_identical = !same_guid && {
        let key = (source.definition().index() as u32, target.as_ref().definition().index() as u32);
        match context.wire_identical.get(&key) {
            Some(known) => *known,
            None => {
                let identical = crate::struct_trees_are_wire_identical(
                    source.definition(),
                    target.as_ref().definition(),
                )
                .is_ok();
                context.wire_identical.insert(key, identical);
                identical
            }
        }
    };
    let mut reparented_fields = if context.group_name == "model_animation_graph"
        && path.contains("animations")
        && source
            .fields()
            .all(|field| !clean_field_key(field.name()).starts_with("shared animation data"))
    {
        convert_local_animation_payload(source, &mut target, path, context)
    } else {
        HashSet::new()
    };
    if root && context.group_name.eq_ignore_ascii_case("weapon") {
        reparented_fields.extend(convert_weapon_melee_layout(source, &mut target, context));
    }
    if root && context.group_name.eq_ignore_ascii_case("effect") {
        reparented_fields.extend(convert_effect_looping_sound_layout(
            source,
            &mut target,
            context,
        ));
    }
    reparented_fields.extend(report_legacy_explicit_function(source, &target, path, context));
    if root && context.group_name.eq_ignore_ascii_case("vehicle") {
        reparented_fields.extend(convert_vehicle_physics_types(source, &mut target, context));
    }
    if root && context.group_name.eq_ignore_ascii_case("damage_effect") {
        match convert_h3_player_responses_to_reach_companions(source, &mut target, context) {
            Ok(fields) => reparented_fields.extend(fields),
            Err(error) => context.fatal_error = Some(error),
        }
    }
    let target_fields = target
        .as_ref()
        .fields()
        .enumerate()
        .map(|(ordinal, field)| TargetFieldInfo {
            ordinal,
            name: field.name().to_owned(),
            key: clean_field_key(field.name()),
            field_type: field.field_type(),
        })
        .collect::<Vec<_>>();
    let mut used = vec![false; target_fields.len()];

    for source_field in source.fields() {
        let key = clean_field_key(source_field.name());
        // `BLAM_DEBUG_FIELD=<cleaned source name>` dumps every target candidate for
        // that one field with each half of the match condition evaluated separately.
        // Reasoning about the clauses in isolation proved every precondition true
        // while the match still did not fire, so print what the matcher itself sees
        // rather than what the schemas say it should. Read once — this is the
        // matcher's inner loop, once per source field per struct element.
        if DEBUG_FIELD.as_deref() == Some(key.as_str()) {
            eprintln!(
                "DEBUG {key:?}: source struct {source_struct_name:?} -> target struct \
                 {target_struct_name:?} (target guid {}), {} candidate(s)",
                target_guid
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
                target_fields.len(),
            );
            for (index, candidate) in target_fields.iter().enumerate() {
                eprintln!(
                    "   cand[{index}] used={} name={:?} key={:?} name_match={} alias_target={} \
                     alias_source={} shape={}",
                    used[index],
                    candidate.name,
                    candidate.key,
                    field_names_match(source_field.name(), &candidate.name),
                    context.target_field_aliases.matches(
                        target_guid,
                        &target_struct_name,
                        &candidate.key,
                        &key,
                    ),
                    context.source_field_aliases.matches(
                        source_guid,
                        &source_struct_name,
                        &key,
                        &candidate.key,
                    ),
                    compatible_field_shapes(source_field.field_type(), candidate.field_type),
                );
            }
        }
        let matched = target_fields.iter().enumerate().find(|(index, candidate)| {
            !used[*index]
                && (field_names_match(source_field.name(), &candidate.name)
                    || context
                        .target_field_aliases
                        .matches(target_guid, &target_struct_name, &candidate.key, &key)
                    || context
                        .source_field_aliases
                        .matches(source_guid, &source_struct_name, &key, &candidate.key)
                    || context
                        .mapping_catalog
                        .field_names_match(FieldMappingRequest {
                            group: context.group_name,
                            source_game: context.source_game,
                            target_game: context.target_game,
                            source_guid,
                            target_guid,
                            source_name: &key,
                            target_name: &candidate.key,
                        }))
                && (compatible_field_shapes(source_field.field_type(), candidate.field_type)
                    || compatible_semantic_field(
                        context.group_name,
                        &key,
                        source_field.field_type(),
                        candidate.field_type,
                    )
                    || compatible_function_curve_field(
                        &key,
                        &candidate.key,
                        source_field.field_type(),
                        candidate.field_type,
                    ))
                && (!key.is_empty() || same_guid)
        });
        let field_path = join_path(
            path,
            if key.is_empty() {
                source_field.type_name()
            } else {
                &key
            },
        );
        let Some((target_index, target_info)) = matched else {
            if !reparented_fields.contains(&key) {
                record_unmatched_field_values(source_field, &field_path, context);
            }
            continue;
        };
        used[target_index] = true;
        if key != target_info.key {
            context.report.mapped_aliases += 1;
        }
        if root {
            context.root_matches += 1;
        }
        let Some(target_field) = target.field_at_mut(target_info.ordinal) else {
            continue;
        };
        convert_field(
            source_field,
            target_field,
            &field_path,
            same_guid || structurally_identical,
            context,
        );
    }

    let defaulted = used
        .iter()
        .zip(&target_fields)
        .filter(|(used, field)| !**used && is_reportable_target_default(field.field_type))
        .count();
    context.report.defaulted_target += defaulted;
}

fn convert_weapon_melee_layout(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    const LEGACY_GAMES: &[&str] = &["halo3_mcc", "halo3odst_mcc"];
    const BLOCK_GAMES: &[&str] = &["haloreach_mcc", "halo4_mcc", "halo2amp_mcc"];
    let legacy_to_block =
        LEGACY_GAMES.contains(&context.source_game) && BLOCK_GAMES.contains(&context.target_game);
    let block_to_legacy =
        BLOCK_GAMES.contains(&context.source_game) && LEGACY_GAMES.contains(&context.target_game);
    if !legacy_to_block && !block_to_legacy {
        return HashSet::new();
    }

    if legacy_to_block {
        convert_legacy_melee_to_block(source, target, context)
    } else {
        convert_block_melee_to_legacy(source, target, context)
    }
}

type ReferencePair = (Option<(u32, String)>, Option<(u32, String)>);

fn field_ordinal_by_key(structure: TagStruct<'_>, key: &str) -> Option<usize> {
    structure
        .fields()
        .enumerate()
        .find(|(_, field)| clean_field_key(field.name()) == clean_field_key(key))
        .map(|(ordinal, _)| ordinal)
}

fn struct_field_by_key<'a>(structure: TagStruct<'a>, key: &str) -> Option<TagStruct<'a>> {
    structure
        .fields()
        .find(|field| clean_field_key(field.name()) == clean_field_key(key))?
        .as_struct()
}

fn reference_by_key(structure: TagStruct<'_>, key: &str) -> Option<(u32, String)> {
    let field = structure
        .fields()
        .find(|field| clean_field_key(field.name()) == clean_field_key(key))?;
    let TagFieldData::TagReference(reference) = field.value()? else {
        return None;
    };
    reference
        .group_tag_and_name
        .filter(|(_, path)| !path.is_empty() && !path.eq_ignore_ascii_case("none"))
}

fn push_unique_reference_pair(pairs: &mut Vec<ReferencePair>, pair: ReferencePair) {
    if (pair.0.is_some() || pair.1.is_some()) && !pairs.contains(&pair) {
        pairs.push(pair);
    }
}

fn set_mapped_reference(
    target: &mut TagStructMut<'_>,
    target_key: &str,
    value: Option<(u32, String)>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    let Some((source_group, name)) = value else {
        return;
    };
    let Some(group_name) = context.source_groups.by_tag.get(&source_group) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!(
                "Source reference group {} is unknown",
                format_group_tag(source_group)
            ),
        );
        return;
    };
    let Some((target_group, _)) = resolve_target_group(
        group_name,
        context.target_groups,
        context.mapping_catalog,
        context.source_game,
        context.target_game,
    ) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Target profile has no {group_name} reference group"),
        );
        return;
    };
    let Some(ordinal) = field_ordinal_by_key(target.as_ref(), target_key) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Target melee layout has no {target_key} field"),
        );
        return;
    };
    let Some(mut field) = target.field_at_mut(ordinal) else {
        return;
    };
    set_converted(
        &mut field,
        TagFieldData::TagReference(TagReferenceData {
            group_tag_and_name: Some((target_group, name)),
        }),
        path,
        source_group == target_group,
        context,
    );
}

fn convert_legacy_melee_to_block(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let Some(source_melee) = struct_field_by_key(source, "melee damage parameters") else {
        return HashSet::new();
    };
    let Some(target_ordinal) = field_ordinal_by_key(target.as_ref(), "melee damage parameters")
    else {
        return HashSet::new();
    };
    let mut pairs = Vec::new();
    push_unique_reference_pair(
        &mut pairs,
        (
            reference_by_key(source, "player melee damage"),
            reference_by_key(source, "player melee response"),
        ),
    );
    for prefix in ["1st hit", "2nd hit", "3rd hit"] {
        push_unique_reference_pair(
            &mut pairs,
            (
                reference_by_key(source_melee, &format!("{prefix} melee damage")),
                reference_by_key(source_melee, &format!("{prefix} melee response")),
            ),
        );
    }
    let unique_pair_count = pairs.len();
    if pairs.is_empty() && struct_has_meaningful_value(source_melee) {
        pairs.push((None, None));
    }

    let Some(mut target_field) = target.field_at_mut(target_ordinal) else {
        return HashSet::new();
    };
    let Some(mut target_block) = target_field.as_block_mut() else {
        return HashSet::new();
    };
    target_block.clear();
    let maximum = target_block.definition().max_count() as usize;
    let count = pairs.len().min(maximum);
    for (index, pair) in pairs.iter().take(count).cloned().enumerate() {
        let target_index = target_block.add_element();
        if let Some(element) = target_block.element_mut(target_index) {
            initialize_block_index_defaults(element);
        }
        if let Some(element) = target_block.element_mut(target_index) {
            convert_struct(
                source_melee,
                element,
                &format!("melee damage parameters[{index}]"),
                false,
                context,
            );
        }
        let mut removed_unsupported = 0;
        context.report.issues.retain(|issue| {
            let transferred_hit = issue
                .path
                .starts_with(&format!("melee damage parameters[{index}]"))
                && ["1st hit melee", "2nd hit melee", "3rd hit melee"]
                    .iter()
                    .any(|name| issue.path.contains(name));
            if transferred_hit && issue.kind == ConversionIssueKind::Unsupported {
                removed_unsupported += 1;
            }
            !transferred_hit
        });
        context.report.unsupported_source = context
            .report
            .unsupported_source
            .saturating_sub(removed_unsupported);
        let Some(mut element) = target_block.element_mut(target_index) else {
            continue;
        };
        set_mapped_reference(
            &mut element,
            "melee damage",
            pair.0,
            &format!("melee damage parameters[{index}]/melee damage"),
            context,
        );
        set_mapped_reference(
            &mut element,
            "melee response",
            pair.1,
            &format!("melee damage parameters[{index}]/melee response"),
            context,
        );
    }
    if unique_pair_count > maximum {
        let omitted = unique_pair_count - maximum;
        context.report.truncated += omitted;
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Truncated,
            path: "melee damage parameters".to_owned(),
            message: format!("Target melee block limit omitted {omitted} unique damage pair(s)"),
        });
    }
    HashSet::from([
        "player melee damage".to_owned(),
        "player melee response".to_owned(),
        "melee damage parameters".to_owned(),
    ])
}

fn convert_block_melee_to_legacy(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let Some(source_field) = source
        .fields()
        .find(|field| clean_field_key(field.name()) == "melee damage parameters")
    else {
        return HashSet::new();
    };
    let Some(source_block) = source_field.as_block() else {
        return HashSet::new();
    };
    let pairs = source_block
        .iter()
        .map(|element| {
            (
                reference_by_key(element, "melee damage"),
                reference_by_key(element, "melee response"),
            )
        })
        .collect::<Vec<_>>();
    let Some(target_ordinal) = field_ordinal_by_key(target.as_ref(), "melee damage parameters")
    else {
        return HashSet::new();
    };
    if let Some(first) = source_block.element(0) {
        let Some(mut target_field) = target.field_at_mut(target_ordinal) else {
            return HashSet::new();
        };
        let Some(target_melee) = target_field.as_struct_mut() else {
            return HashSet::new();
        };
        convert_struct(
            first,
            target_melee,
            "melee damage parameters",
            false,
            context,
        );
    }
    for (index, pair) in pairs.into_iter().take(3).enumerate() {
        let Some(mut target_field) = target.field_at_mut(target_ordinal) else {
            continue;
        };
        let Some(mut target_melee) = target_field.as_struct_mut() else {
            continue;
        };
        let prefix = ["1st hit", "2nd hit", "3rd hit"][index];
        set_mapped_reference(
            &mut target_melee,
            &format!("{prefix} melee damage"),
            pair.0,
            &format!("melee damage parameters/{prefix} melee damage"),
            context,
        );
        set_mapped_reference(
            &mut target_melee,
            &format!("{prefix} melee response"),
            pair.1,
            &format!("melee damage parameters/{prefix} melee response"),
            context,
        );
    }
    HashSet::from(["melee damage parameters".to_owned()])
}

fn field_by_key<'a>(structure: TagStruct<'a>, key: &str) -> Option<TagField<'a>> {
    structure
        .fields()
        .find(|field| clean_field_key(field.name()) == clean_field_key(key))
}

/// Copy every value `target` can hold from a *flat* `source` struct, descending
/// into the target's nested structs.
///
/// The ordinary matcher pairs fields within one struct level. This is for the case
/// where the target has grouped into sub-structs what the source keeps flat — Halo
/// 3 files `overdampen cusp angle` under `steering control` and `turn rate` under
/// `turning control`, both of which sit bare on a Halo 2 vehicle root. Matching is
/// still strictly by cleaned field name, so nothing is paired positionally.
///
/// Every key it consumes is recorded, so the caller can tell the main matcher not
/// to report those source fields as unmatched.
fn fill_nested_target_from_flat_source(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    path: &str,
    consumed: &mut HashSet<String>,
    context: &mut ConversionContext<'_>,
) {
    let targets = target
        .as_ref()
        .fields()
        .enumerate()
        .map(|(ordinal, field)| (ordinal, clean_field_key(field.name()), field.field_type()))
        .collect::<Vec<_>>();
    for (ordinal, key, field_type) in targets {
        if field_type == TagFieldType::Struct {
            let child_path = join_path(path, &key);
            let Some(mut field) = target.field_at_mut(ordinal) else {
                continue;
            };
            let Some(mut child) = field.as_struct_mut() else {
                continue;
            };
            fill_nested_target_from_flat_source(source, &mut child, &child_path, consumed, context);
            continue;
        }
        if key.is_empty() {
            continue;
        }
        let Some(source_field) = field_by_key(source, &key) else {
            continue;
        };
        if !compatible_field_shapes(source_field.field_type(), field_type) {
            continue;
        }
        let field_path = join_path(path, &key);
        if let Some(target_field) = target.field_at_mut(ordinal) {
            convert_field(source_field, target_field, &field_path, false, context);
            consumed.insert(key);
        }
    }
}

/// Report a Halo 2 version-0 `mapping_function` that the target can only hold as a
/// serialized blob, and say so instead of guessing at one.
///
/// H2's `mapping_function` is a versioned struct: v1 holds the serialized
/// `c_function_definition` (which [`convert_function_mapping`] carries across, and
/// which is 9,193 of the 10,027 mappings in the H2EK corpus), while **v0 spells the
/// curve out as explicit fields** — `function type`, `flags`, four colours and a
/// `values` block of reals. Halo 3 onward have only the blob form, so a v0 curve
/// would have to be synthesized.
///
/// Deliberately not synthesized. Surveying the 834 v0 mappings the kit ships, the
/// `function type` byte takes values of **40 and 24**, which are not valid
/// `FunctionType` discriminants at all (the enum runs 0..=10) — so v0's type byte
/// cannot be assumed to be the modern enum, and the flat `values` list does not
/// have a derivable mapping onto the per-type compact structures either (a
/// `Constant` appears with 0, 2 and 12 values). Anything built from that would be a
/// guess, and a curve that parses but describes the wrong shape silently changes
/// how a particle looks — worse than one the user is told to reauthor.
///
/// Returns the source keys it consumed so the ordinary matcher does not also report
/// each field individually; one issue per curve is the useful granularity.
fn report_legacy_explicit_function(
    source: TagStruct<'_>,
    target: &TagStructMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let mut consumed = HashSet::new();
    let Some(function_type) = field_by_key(source, "function type") else {
        return consumed;
    };
    let Some(values) = field_by_key(source, "values").and_then(|field| field.as_block()) else {
        return consumed;
    };
    // The target must be the blob form, i.e. hold function data and nothing the
    // explicit fields could match.
    let wants_blob = target
        .as_ref()
        .fields()
        .any(|field| field.is_function_data() || clean_field_key(field.name()) == "data");
    if !wants_blob || field_ordinal_by_key(target.as_ref(), "function type").is_some() {
        return consumed;
    }
    let kind = function_type.value().and_then(integer_value).unwrap_or(-1);
    record_unsupported(
        context,
        path.to_owned(),
        format!(
            "This curve is stored in {}'s older explicit form (function type {kind}, \
             {} value(s)); {} stores curves only as a serialized function definition, \
             and the older form's type byte does not map onto the modern function \
             types, so the curve was left unset rather than guessed at. Reauthor it \
             in the target editor.",
            context.source_game,
            values.len(),
            context.target_game,
        ),
    );
    for field in source.fields() {
        let key = clean_field_key(field.name());
        if matches!(
            key.as_str(),
            "function type" | "flags" | "function 1" | "function 2" | "values"
        ) || key.starts_with("color ")
        {
            consumed.insert(key);
        }
    }
    consumed
}

/// Route a legacy vehicle's flat physics fields into the per-type block the modern
/// layout expects.
///
/// Halo 2 keeps one flat set of physics values on the vehicle root and a `type`
/// enum saying which of them the engine reads. Halo 3 replaced that with
/// `physics types`, a struct of ten blocks — `type-human_tank`, `type-human_jeep`,
/// `type-human_plane`, … — whose own schema comment says "define one of the
/// following blocks for the type of physics you wish this vehicle to have". So the
/// values did not change name or meaning; they moved into a block chosen by the
/// enum. Until this ran, every one of them was dropped: a Halo 2 warthog arrived
/// in Halo 3 with its speeds, slides, turn rates and thrust all at zero.
///
/// Driven by the shapes present rather than by a game list, so it applies to any
/// pair where the source is flat and the target has `physics types`.
///
/// `human boat` is deliberately unhandled — Halo 3 ships no boat block, and there
/// is no honest destination for those values, so it is reported.
fn convert_vehicle_physics_types(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let mut consumed = HashSet::new();
    let Some(types_ordinal) = field_ordinal_by_key(target.as_ref(), "physics types") else {
        return consumed;
    };
    // A source that already has `physics types` is the modern layout; the ordinary
    // struct match handles it.
    if field_by_key(source, "physics types").is_some() {
        return consumed;
    }
    let Some(type_field) = field_by_key(source, "type") else {
        return consumed;
    };
    let type_name = match type_field.value() {
        Some(TagFieldData::CharEnum { name, .. })
        | Some(TagFieldData::ShortEnum { name, .. })
        | Some(TagFieldData::LongEnum { name, .. }) => name,
        _ => None,
    };
    let Some(type_name) = type_name else {
        record_unsupported(
            context,
            "physics types".to_owned(),
            "Source vehicle type has no name, so its physics block could not be chosen".to_owned(),
        );
        return consumed;
    };
    // `human plane` -> `type-human_plane`. The enum option names and the block
    // names are the same words, so no lookup table is needed — and a type the
    // target dropped simply finds no block.
    let block_key = format!("type-{}", type_name.trim().to_ascii_lowercase().replace(' ', "_"));

    let Some(mut types_field) = target.field_at_mut(types_ordinal) else {
        return consumed;
    };
    let Some(mut types_struct) = types_field.as_struct_mut() else {
        return consumed;
    };
    let Some(block_ordinal) = field_ordinal_by_key(types_struct.as_ref(), &block_key) else {
        record_unsupported(
            context,
            format!("physics types/{block_key}"),
            format!(
                "{} has no {block_key} physics block, so this vehicle's physics \
                 values have no destination and need reauthoring",
                context.target_game
            ),
        );
        return consumed;
    };
    let Some(mut block_field) = types_struct.field_at_mut(block_ordinal) else {
        return consumed;
    };
    let Some(mut block) = block_field.as_block_mut() else {
        return consumed;
    };
    // Exactly one element: the schema's instruction is to define one block.
    if block.is_empty() {
        block.add_element();
    }
    let Some(mut element) = block.element_mut(0) else {
        return consumed;
    };
    let path = format!("physics types/{block_key}[0]");
    fill_nested_target_from_flat_source(source, &mut element, &path, &mut consumed, context);
    // `type` itself is spent selecting the block, not copied.
    consumed.insert("type".to_owned());
    consumed
}

fn convert_effect_looping_sound_layout(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    const LEGACY_GAMES: &[&str] = &["halo3_mcc", "halo3odst_mcc"];
    const BLOCK_GAMES: &[&str] = &["haloreach_mcc", "halo4_mcc", "halo2amp_mcc"];
    if LEGACY_GAMES.contains(&context.source_game) && BLOCK_GAMES.contains(&context.target_game) {
        convert_legacy_effect_looping_sound_to_block(source, target, context)
    } else if BLOCK_GAMES.contains(&context.source_game)
        && LEGACY_GAMES.contains(&context.target_game)
    {
        convert_effect_looping_sound_block_to_legacy(source, target, context)
    } else {
        HashSet::new()
    }
}

fn convert_legacy_effect_looping_sound_to_block(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let Some(source_sound) = field_by_key(source, "looping sound") else {
        return HashSet::new();
    };
    let Some(target_ordinal) = field_ordinal_by_key(target.as_ref(), "looping sounds") else {
        return HashSet::new();
    };
    let has_sound = matches!(
        source_sound.value(),
        Some(TagFieldData::TagReference(TagReferenceData {
            group_tag_and_name: Some((_, ref path)),
        })) if !path.is_empty() && !path.eq_ignore_ascii_case("none")
    );
    let Some(mut target_field) = target.field_at_mut(target_ordinal) else {
        return HashSet::new();
    };
    let Some(mut target_block) = target_field.as_block_mut() else {
        return HashSet::new();
    };
    target_block.clear();
    if has_sound {
        let index = target_block.add_element();
        if let Some(element) = target_block.element_mut(index) {
            initialize_block_index_defaults(element);
        }
        if let Some(mut element) = target_block.element_mut(index) {
            for key in ["looping sound", "location", "bind scale to event"] {
                let (Some(source_field), Some(target_ordinal)) = (
                    field_by_key(source, key),
                    field_ordinal_by_key(element.as_ref(), key),
                ) else {
                    continue;
                };
                if let Some(target_field) = element.field_at_mut(target_ordinal) {
                    convert_field(
                        source_field,
                        target_field,
                        &format!("looping sounds[0]/{key}"),
                        false,
                        context,
                    );
                }
            }
        }
    }
    HashSet::from([
        "looping sound".to_owned(),
        "location".to_owned(),
        "bind scale to event".to_owned(),
    ])
}

fn convert_effect_looping_sound_block_to_legacy(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let Some(source_block) =
        field_by_key(source, "looping sounds").and_then(|field| field.as_block())
    else {
        return HashSet::new();
    };
    if let Some(element) = source_block.element(0) {
        for key in ["looping sound", "location", "bind scale to event"] {
            let (Some(source_field), Some(target_ordinal)) = (
                field_by_key(element, key),
                field_ordinal_by_key(target.as_ref(), key),
            ) else {
                continue;
            };
            if let Some(target_field) = target.field_at_mut(target_ordinal) {
                convert_field(source_field, target_field, key, false, context);
            }
        }
    }
    if source_block.len() > 1 {
        record_unsupported(
            context,
            "looping sounds".to_owned(),
            format!(
                "Legacy target supports one looping sound but source has {}",
                source_block.len()
            ),
        );
    }
    HashSet::from(["looping sounds".to_owned()])
}

/// Reach-family animation entries moved the H3 inline payload into a
/// single-element `shared animation data` block. This is a structural move,
/// not a rename, so map the compatible source fields into that nested element
/// without reporting the already-copied entry metadata as unmatched.
fn convert_local_animation_payload(
    source: TagStruct<'_>,
    target: &mut TagStructMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) -> HashSet<String> {
    let mut transferred = HashSet::new();
    let payload_ordinal = target
        .as_ref()
        .fields()
        .enumerate()
        .find(|(_, field)| {
            field.field_type() == TagFieldType::Block
                && clean_field_key(field.name()).starts_with("shared animation data")
        })
        .map(|(ordinal, _)| ordinal);
    let Some(payload_ordinal) = payload_ordinal else {
        return transferred;
    };
    let Some(mut payload_field) = target.field_at_mut(payload_ordinal) else {
        return transferred;
    };
    let Some(mut payload_block) = payload_field.as_block_mut() else {
        return transferred;
    };
    payload_block.clear();
    let payload_index = payload_block.add_element();
    let Some(payload) = payload_block.element_mut(payload_index) else {
        return transferred;
    };
    initialize_block_index_defaults(payload);
    let Some(mut payload) = payload_block.element_mut(payload_index) else {
        return transferred;
    };
    let target_guid = payload.as_ref().definition().guid();
    let target_struct_name = payload.as_ref().definition().name().to_owned();
    let target_fields = payload
        .as_ref()
        .fields()
        .enumerate()
        .map(|(ordinal, field)| TargetFieldInfo {
            ordinal,
            name: field.name().to_owned(),
            key: clean_field_key(field.name()),
            field_type: field.field_type(),
        })
        .collect::<Vec<_>>();
    let mut used = vec![false; target_fields.len()];
    let source_guid = source.definition().guid();
    let source_struct_name = source.definition().name().to_owned();
    for source_field in source.fields() {
        let key = clean_field_key(source_field.name());
        let matched = target_fields.iter().enumerate().find(|(index, candidate)| {
            !used[*index]
                && (field_names_match(source_field.name(), &candidate.name)
                    || context
                        .target_field_aliases
                        .matches(target_guid, &target_struct_name, &candidate.key, &key)
                    || context
                        .source_field_aliases
                        .matches(source_guid, &source_struct_name, &key, &candidate.key)
                    || context
                        .mapping_catalog
                        .field_names_match(FieldMappingRequest {
                            group: context.group_name,
                            source_game: context.source_game,
                            target_game: context.target_game,
                            source_guid,
                            target_guid,
                            source_name: &key,
                            target_name: &candidate.key,
                        }))
                && (compatible_field_shapes(source_field.field_type(), candidate.field_type)
                    || compatible_function_curve_field(
                        &key,
                        &candidate.key,
                        source_field.field_type(),
                        candidate.field_type,
                    ))
        });
        let Some((target_index, target_info)) = matched else {
            continue;
        };
        used[target_index] = true;
        transferred.insert(key.clone());
        if key != target_info.key {
            context.report.mapped_aliases += 1;
        }
        if let Some(target_field) = payload.field_at_mut(target_info.ordinal) {
            convert_field(
                source_field,
                target_field,
                &join_path(
                    path,
                    &format!("shared animation data[0]/{}", target_info.key),
                ),
                source_guid == target_guid,
                context,
            );
        }
    }
    transferred
}

fn is_reportable_target_default(field_type: TagFieldType) -> bool {
    !matches!(
        field_type,
        TagFieldType::Terminator
            | TagFieldType::Explanation
            | TagFieldType::Pad
            | TagFieldType::UselessPad
            | TagFieldType::Skip
            | TagFieldType::Custom
            | TagFieldType::ApiInterop
            | TagFieldType::PageableResource
    )
}

fn record_unmatched_field_values(
    field: TagField<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    if !field_has_meaningful_value(field) {
        return;
    }
    if field.name().contains('!') || clean_field_key(field.name()).starts_with("runtime ") {
        context.report.issues.push(ConversionIssue {
            kind: ConversionIssueKind::Warning,
            path: path.to_owned(),
            message: "Engine-managed source value was reset for the target engine".to_owned(),
        });
        return;
    }
    match field.field_type() {
        TagFieldType::Struct => {
            if let Some(structure) = field.as_struct() {
                for child in structure.fields() {
                    let key = clean_field_key(child.name());
                    let child_path = join_path(
                        path,
                        if key.is_empty() {
                            child.type_name()
                        } else {
                            &key
                        },
                    );
                    record_unmatched_field_values(child, &child_path, context);
                }
            }
        }
        TagFieldType::Block => {
            if let Some(block) = field.as_block() {
                for (index, element) in block.iter().enumerate() {
                    for child in element.fields() {
                        let key = clean_field_key(child.name());
                        let child_path = join_path(
                            &format!("{path}[{index}]"),
                            if key.is_empty() {
                                child.type_name()
                            } else {
                                &key
                            },
                        );
                        record_unmatched_field_values(child, &child_path, context);
                    }
                }
            }
        }
        TagFieldType::Array => {
            if let Some(array) = field.as_array() {
                for (index, element) in array.iter().enumerate() {
                    for child in element.fields() {
                        let key = clean_field_key(child.name());
                        let child_path = join_path(
                            &format!("{path}[{index}]"),
                            if key.is_empty() {
                                child.type_name()
                            } else {
                                &key
                            },
                        );
                        record_unmatched_field_values(child, &child_path, context);
                    }
                }
            }
        }
        _ => record_unsupported(
            context,
            path.to_owned(),
            format!("No compatible target field for {}", field.type_name()),
        ),
    }
}

fn field_names_match(left: &str, right: &str) -> bool {
    let left_key = clean_field_key(left);
    let right_key = clean_field_key(right);
    if left_key == right_key {
        return true;
    }
    if left_key.is_empty() || right_key.is_empty() {
        return false;
    }
    // `|ABCDCC` and similar suffixes are editor presentation/order metadata,
    // not alternate field names. Including them made unrelated blocks with
    // the same suffix appear compatible.
    let aliases =
        |name: &str| option_name_aliases(name.split(['#', ':', '|']).next().unwrap_or(name));
    let left = aliases(left);
    let right = aliases(right);
    left.iter()
        .any(|left| right.iter().any(|right| left == right))
}

fn compatible_field_shapes(source: TagFieldType, target: TagFieldType) -> bool {
    source == target
        || (is_integer_type(source) && is_integer_type(target))
        || (is_real_scalar(source) && is_real_scalar(target))
        || (is_enum_type(source) && is_enum_type(target))
        || (is_flags_type(source) && is_flags_type(target))
        || (is_string_type(source) && is_string_type(target))
        || (is_string_id_type(source) && is_string_id_type(target))
}

/// Whether a `mapping_function` curve is being matched across its two spellings.
///
/// Halo 2 and earlier hold the serialized curve in `block 'data' byte_block`;
/// Halo 3 onward hold it in `data 'data' function_definition_data`. The field name
/// is `data` on both sides, so the pair is unambiguous — but the *shapes* are a
/// block and a blob, which every ordinary shape rule rejects. Without this the
/// matcher never proposes the pair and [`convert_function_mapping`] never gets a
/// chance to look at it.
///
/// Deliberately permissive about what the block contains: this only decides
/// whether the pair is worth *offering*. `convert_function_mapping` still checks
/// that the target really is function data, that the block really is one byte
/// wide, and that the bytes parse as a function before writing anything.
fn compatible_function_curve_field(
    source_key: &str,
    target_key: &str,
    source: TagFieldType,
    target: TagFieldType,
) -> bool {
    source_key == "data"
        && target_key == "data"
        && matches!(
            (source, target),
            (TagFieldType::Block, TagFieldType::Data) | (TagFieldType::Data, TagFieldType::Block)
        )
}

fn compatible_semantic_field(
    group: &str,
    field: &str,
    source: TagFieldType,
    target: TagFieldType,
) -> bool {
    group.eq_ignore_ascii_case("lens_flare")
        && clean_field_key(field) == "occlusion inner radius scale"
        && ((is_enum_type(source) && target == TagFieldType::Real)
            || (source == TagFieldType::Real && is_enum_type(target)))
}

fn convert_lens_flare_occlusion_scale(
    source: TagField<'_>,
    target: &mut TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) -> bool {
    if !context.group_name.eq_ignore_ascii_case("lens_flare")
        || clean_field_key(path.split('/').next_back().unwrap_or(path))
            != "occlusion inner radius scale"
    {
        return false;
    }

    const SCALES: [(&str, f32); 7] = [
        ("none", 0.0),
        ("1/2", 0.5),
        ("1/4", 0.25),
        ("1/8", 0.125),
        ("1/16", 0.0625),
        ("1/32", 0.03125),
        ("1/64", 0.015625),
    ];
    if is_enum_type(source.field_type()) && target.as_ref().field_type() == TagFieldType::Real {
        let name = match source.value() {
            Some(TagFieldData::CharEnum { name, .. })
            | Some(TagFieldData::ShortEnum { name, .. })
            | Some(TagFieldData::LongEnum { name, .. }) => name,
            _ => None,
        };
        let Some((_, scale)) = name.as_deref().and_then(|name| {
            SCALES
                .iter()
                .find(|(candidate, _)| option_names_match(candidate, name))
        }) else {
            if field_has_meaningful_value(source) {
                record_unsupported(
                    context,
                    path.to_owned(),
                    "Unresolved lens-flare occlusion scale enum".to_owned(),
                );
            }
            return true;
        };
        set_converted(target, TagFieldData::Real(*scale), path, false, context);
        return true;
    }

    if source.field_type() == TagFieldType::Real && is_enum_type(target.as_ref().field_type()) {
        let Some(TagFieldData::Real(scale)) = source.value() else {
            return true;
        };
        let Some((index, (name, _))) = SCALES
            .iter()
            .enumerate()
            .find(|(_, (_, candidate))| (scale - *candidate).abs() <= f32::EPSILON)
        else {
            if scale != 0.0 {
                record_unsupported(
                    context,
                    path.to_owned(),
                    format!("Legacy lens-flare schema cannot represent occlusion scale {scale}"),
                );
            }
            return true;
        };
        let value = match target.as_ref().field_type() {
            TagFieldType::CharEnum => TagFieldData::CharEnum {
                value: index as i8,
                name: Some((*name).to_owned()),
            },
            TagFieldType::ShortEnum => TagFieldData::ShortEnum {
                value: index as i16,
                name: Some((*name).to_owned()),
            },
            TagFieldType::LongEnum => TagFieldData::LongEnum {
                value: index as i32,
                name: Some((*name).to_owned()),
            },
            _ => return false,
        };
        set_converted(target, value, path, false, context);
        return true;
    }
    false
}

fn convert_field(
    source: TagField<'_>,
    mut target: TagFieldMut<'_>,
    path: &str,
    same_struct_guid: bool,
    context: &mut ConversionContext<'_>,
) {
    let source_type = source.field_type();
    let target_type = target.as_ref().field_type();
    if convert_lens_flare_occlusion_scale(source, &mut target, path, context) {
        return;
    }
    if convert_function_mapping(source, &mut target, path, context) {
        return;
    }
    match source_type {
        TagFieldType::Struct => {
            let (Some(source_struct), Some(target_struct)) =
                (source.as_struct(), target.as_struct_mut())
            else {
                record_unsupported(
                    context,
                    path.to_owned(),
                    "Missing nested struct data".to_owned(),
                );
                return;
            };
            convert_struct(source_struct, target_struct, path, false, context);
        }
        TagFieldType::Block => {
            let (Some(source_block), Some(mut target_block)) =
                (source.as_block(), target.as_block_mut())
            else {
                record_unsupported(
                    context,
                    path.to_owned(),
                    "Missing tag block data".to_owned(),
                );
                return;
            };
            target_block.clear();
            let maximum = target_block.definition().max_count() as usize;
            let count = source_block.len().min(maximum);
            for index in 0..count {
                let target_index = target_block.add_element();
                if let Some(target_element) = target_block.element_mut(target_index) {
                    initialize_block_index_defaults(target_element);
                }
                if let (Some(source_element), Some(target_element)) = (
                    source_block.element(index),
                    target_block.element_mut(target_index),
                ) {
                    convert_struct(
                        source_element,
                        target_element,
                        &format!("{path}[{index}]"),
                        false,
                        context,
                    );
                }
            }
            if source_block.len() > count {
                let omitted = source_block.len() - count;
                context.report.truncated += omitted;
                context.report.issues.push(ConversionIssue {
                    kind: ConversionIssueKind::Truncated,
                    path: path.to_owned(),
                    message: format!("Target block limit omitted {omitted} element(s)"),
                });
            }
        }
        TagFieldType::Array => {
            let (Some(source_array), Some(mut target_array)) =
                (source.as_array(), target.as_array_mut())
            else {
                record_unsupported(
                    context,
                    path.to_owned(),
                    "Missing fixed-array data".to_owned(),
                );
                return;
            };
            let count = source_array.len().min(target_array.len());
            for index in 0..count {
                if let (Some(source_element), Some(target_element)) =
                    (source_array.element(index), target_array.element_mut(index))
                {
                    convert_struct(
                        source_element,
                        target_element,
                        &format!("{path}[{index}]"),
                        false,
                        context,
                    );
                }
            }
            if source_array.len() > count {
                let omitted = source_array.len() - count;
                context.report.truncated += omitted;
                context.report.issues.push(ConversionIssue {
                    kind: ConversionIssueKind::Truncated,
                    path: path.to_owned(),
                    message: format!("Target array omitted {omitted} element(s)"),
                });
            }
        }
        TagFieldType::PageableResource => {
            transfer_resource(source, &mut target, path, context);
        }
        TagFieldType::ApiInterop => {
            if field_has_meaningful_value(source) {
                record_unsupported(
                    context,
                    path.to_owned(),
                    "API interop runtime data is not transferred".to_owned(),
                );
            }
        }
        TagFieldType::TagReference => convert_reference(source, target, path, context),
        TagFieldType::CharEnum | TagFieldType::ShortEnum | TagFieldType::LongEnum => {
            convert_enum(source, target, path, context)
        }
        TagFieldType::ByteFlags | TagFieldType::WordFlags | TagFieldType::LongFlags => {
            convert_flags(source, target, path, context)
        }
        TagFieldType::StringId | TagFieldType::OldStringId => {
            let Some(value) = source.value() else { return };
            let string = match value {
                TagFieldData::StringId(value) | TagFieldData::OldStringId(value) => value.string,
                _ => return,
            };
            let value = if target_type == TagFieldType::StringId {
                TagFieldData::StringId(StringIdData { string })
            } else {
                TagFieldData::OldStringId(StringIdData { string })
            };
            set_converted(
                &mut target,
                value,
                path,
                source_type == target_type,
                context,
            );
        }
        TagFieldType::String | TagFieldType::LongString => {
            let Some(value) = source.value() else { return };
            let string = match value {
                TagFieldData::String(value) | TagFieldData::LongString(value) => value,
                _ => return,
            };
            let limit = if target_type == TagFieldType::String {
                31
            } else {
                255
            };
            if string.len() > limit {
                record_unsupported(
                    context,
                    path.to_owned(),
                    format!(
                        "String is {} bytes but target limit is {limit}",
                        string.len()
                    ),
                );
                return;
            }
            let value = if target_type == TagFieldType::String {
                TagFieldData::String(string)
            } else {
                TagFieldData::LongString(string)
            };
            set_converted(
                &mut target,
                value,
                path,
                source_type == target_type,
                context,
            );
        }
        TagFieldType::Data | TagFieldType::Custom => {
            // A third identity key, for the blob that *is* the tag.
            //
            // The struct GUID and wire congruence both describe the enclosing
            // struct, and neither holds for a Halo 1 bitmap against a Halo 2 one:
            // Halo 1 groups its root into `processing`/`color plate` sub-structs
            // where Halo 2 flattens them, so the roots differ in size and field
            // count. But both declare the pixels with the *same data definition
            // name* — `processed_pixel_data_data` — and the schema's own name for
            // what a blob holds is exactly the claim needed here: these two fields
            // hold the same kind of payload.
            //
            // Measured before relying on it: Halo 1's per-bitmap `format_enum` and
            // Halo 2's `format_enum_2` agree entry-for-entry for indices 0..=16
            // (A8 through DXT5), and the enum is carried by *name*, so Halo 1's
            // `P8` at 17 lands on Halo 2's `p8` at 18 rather than on `p8-bump`.
            // The bytes therefore mean the same thing on both sides. Halo 1's
            // `BC7` has no Halo 2 option and is reported by the enum conversion.
            let same_payload_kind = source_type == target_type
                && source
                    .data_definition_name()
                    .zip(target.as_ref().data_definition_name())
                    .is_some_and(|(source_name, target_name)| {
                        source_name == target_name
                            || context.mapping_catalog.payload_alias_allows(
                                context.group_name,
                                context.source_game,
                                context.target_game,
                                source_name,
                                target_name,
                            )
                    });
            if (!same_struct_guid && !same_payload_kind) || source_type != target_type {
                if field_has_meaningful_value(source) {
                    record_unsupported(
                        context,
                        path.to_owned(),
                        "Opaque bytes require an identical struct GUID and field type".to_owned(),
                    );
                    // A `data` blob is usually not one field among many — it is
                    // what the tag *is*. A bitmap's `processed pixel data`, a
                    // sound's samples, a mesh's `vertices`. Leaving one behind
                    // produces a tag whose metadata promises bytes that are not
                    // there, which reads past the end and crashes the editing kit
                    // rather than merely failing to open. Record it so the write
                    // refuses.
                    //
                    // Unless the target marks the field `*`. That is the schema's
                    // own "the editor owns this, not the author" marker, and the
                    // one blob in the corpus that carries it — an animation
                    // graph's `last import results*`, the log `tool` writes and
                    // overwrites on the next import — is exactly the kind of
                    // bookkeeping a conversion has no business refusing over.
                    let editor_owned = context
                        .target_field_aliases
                        .is_editor_owned(&clean_field_key(target.as_ref().name()))
                        || context
                            .source_field_aliases
                            .is_editor_owned(&clean_field_key(source.name()));
                    if source_type == TagFieldType::Data && !editor_owned {
                        context.payloads_left_behind.push(path.to_owned());
                    }
                }
                return;
            }
            if let Some(value) = source.value() {
                set_converted(&mut target, value, path, true, context);
            }
        }
        _ if is_integer_type(source_type) && is_integer_type(target_type) => {
            convert_integer(source, target, path, context)
        }
        _ if is_real_scalar(source_type) && is_real_scalar(target_type) => {
            let Some(value) = source.value().and_then(real_value) else {
                return;
            };
            // A NaN or an infinity is not a number an author typed, and writing one
            // into the target is worse than keeping the template's default — the
            // destination game's own tools have to read it, and a non-finite float
            // is how a tag becomes one that will not open. Halo 2 stamps junk into
            // the unused `object` collision-damage slots, and a shipped H2
            // projectile carries a NaN `material responses[0]/angular noise` that
            // this converter copied straight through into Halo 3.
            //
            // Scoped to real scalars because that is what was measured; bounds and
            // vectors travel through the same-type copy below and are not guarded.
            if !value.is_finite() {
                context.report.issues.push(ConversionIssue {
                    kind: ConversionIssueKind::Warning,
                    path: path.to_owned(),
                    message: format!(
                        "Source value is not a finite number ({value}), so the target \
                         keeps its default"
                    ),
                });
                return;
            }
            let target_key = clean_field_key(target.as_ref().name());
            let value = match real_scalar_unit_change(
                source_type,
                context.source_field_aliases.unit_of(&clean_field_key(source.name())),
                target_type,
                context.target_field_aliases.unit_of(&target_key),
            ) {
                RealUnitChange::Copy => value,
                RealUnitChange::RadiansToDegrees => value.to_degrees(),
                RealUnitChange::DegreesToRadians => value.to_radians(),
            };
            let converted = real_field_value(target_type, value);
            set_converted(
                &mut target,
                converted,
                path,
                source_type == target_type,
                context,
            );
        }
        _ if source_type == target_type => {
            if let Some(value) = source.value() {
                set_converted(&mut target, value, path, true, context);
            }
        }
        _ => {
            if field_has_meaningful_value(source) {
                record_unsupported(
                    context,
                    path.to_owned(),
                    format!("Cannot convert {source_type:?} to {target_type:?}"),
                );
            }
        }
    }
}

fn convert_reference(
    source: TagField<'_>,
    mut target: TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    let Some(TagFieldData::TagReference(reference)) = source.value() else {
        return;
    };
    let Some((source_group, name)) = reference.group_tag_and_name else {
        return;
    };
    let Some(group_name) = context.source_groups.by_tag.get(&source_group) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!(
                "Source reference group {} is unknown",
                format_group_tag(source_group)
            ),
        );
        return;
    };
    let Some((target_group, _)) = resolve_target_group(
        group_name,
        context.target_groups,
        context.mapping_catalog,
        context.source_game,
        context.target_game,
    ) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Target profile has no {group_name} reference group"),
        );
        return;
    };
    set_converted(
        &mut target,
        TagFieldData::TagReference(TagReferenceData {
            group_tag_and_name: Some((target_group, name)),
        }),
        path,
        source_group == target_group,
        context,
    );
}

/// The bytes of a `mapping_function` held as a block of one-byte elements.
///
/// Halo 2 and earlier spell a function curve out as `block 'data' byte_block`,
/// one element per byte; Halo 3 onward store the same serialized
/// `c_function_definition` in a single `data 'data' function_definition_data`
/// field. Same name, same content, different container — so an ordinary
/// field-shape match rejects the pair and the whole curve is lost. One real Halo 2
/// `effect` carries 1,180 bytes of curve this way, which is why an effect arrived
/// in Halo 3 with 544 of its numbers sitting at zero.
fn function_bytes_from_block(block: TagBlock<'_>) -> Option<Vec<u8>> {
    let mut bytes = Vec::with_capacity(block.len());
    for element in block.iter() {
        let mut fields = element.fields().filter(|field| field.value().is_some());
        let only = fields.next()?;
        if fields.next().is_some() {
            // More than one value per element means this is not a byte block.
            return None;
        }
        bytes.push(u8::try_from(integer_value(only.value()?)? & 0xff).ok()?);
    }
    Some(bytes)
}

/// The byte offset at which Halo 3 inserted `compact_size` into the
/// `c_function_definition` header.
///
/// A Halo 3 header is 32 bytes: `function_type | flags | color_graph_type | pad`,
/// then 16 bytes of clamp range (or four packed colors), then
/// `exclusion_min`/`exclusion_max`, then a 4-byte `compact_size` giving the length
/// of the per-type compact block that follows. Halo 2 stores the same fields in
/// the same order but stops at 28 bytes — it has no `compact_size`, because the
/// enclosing byte block's own element count already gives the total length.
const FUNCTION_COMPACT_SIZE_OFFSET: usize = 28;

/// Move a serialized `c_function_definition` between the Halo 2 and Halo 3 header
/// layouts.
///
/// Halo 3 gained a 4-byte `compact_size` at [`FUNCTION_COMPACT_SIZE_OFFSET`]; the
/// bytes on either side of it are unchanged. So promoting is "splice the length
/// in" and demoting is "cut it out" — no reinterpretation of any field.
///
/// This is what a real Halo 2 effect needs: all 30 of one effect's emitter curves
/// are 28-byte headers describing constant functions, and without the splice every
/// one is rejected as too short and the emitter loses its authored value.
/// Which header layout a curve is written in cannot be told from its length — a
/// 36-byte legacy curve (28-byte header plus 8 bytes of compact data) is the same
/// size as a 36-byte modern one. The *container* settles it: a block of bytes is
/// always the legacy spelling and a `data` blob always the modern one, because no
/// game uses both.
fn retarget_function_bytes(
    bytes: &[u8],
    source_is_modern: bool,
    target_is_modern: bool,
) -> Option<Vec<u8>> {
    if source_is_modern == target_is_modern {
        return Some(bytes.to_vec());
    }
    if target_is_modern {
        if bytes.len() < FUNCTION_COMPACT_SIZE_OFFSET {
            return None;
        }
        let compact = bytes.len() - FUNCTION_COMPACT_SIZE_OFFSET;
        let mut out = Vec::with_capacity(bytes.len() + 4);
        out.extend_from_slice(&bytes[..FUNCTION_COMPACT_SIZE_OFFSET]);
        out.extend_from_slice(&u32::try_from(compact).ok()?.to_le_bytes());
        out.extend_from_slice(&bytes[FUNCTION_COMPACT_SIZE_OFFSET..]);
        Some(out)
    } else {
        if bytes.len() < FUNCTION_COMPACT_SIZE_OFFSET + 4 {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() - 4);
        out.extend_from_slice(&bytes[..FUNCTION_COMPACT_SIZE_OFFSET]);
        out.extend_from_slice(&bytes[FUNCTION_COMPACT_SIZE_OFFSET + 4..]);
        Some(out)
    }
}

/// Carry a function curve between the block-of-bytes and data-blob spellings.
///
/// Returns `true` when the pair was recognised and handled, so the caller stops.
/// The bytes are only moved when they parse as a `c_function_definition` on the
/// way out — [`TagFunction::parse`] is the engine's own decoder, so a format the
/// target generation could not read is reported rather than written. That check
/// is the whole safety argument for copying these bytes verbatim.
fn convert_function_mapping(
    source: TagField<'_>,
    target: &mut TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) -> bool {
    let source_type = source.field_type();
    let target_type = target.as_ref().field_type();
    let bytes = match (source_type, target_type) {
        (TagFieldType::Block, TagFieldType::Data) => {
            if !target.as_ref().is_function_data() {
                return false;
            }
            let Some(block) = source.as_block() else {
                return false;
            };
            match function_bytes_from_block(block) {
                Some(bytes) => bytes,
                None => return false,
            }
        }
        (TagFieldType::Data, TagFieldType::Block) => {
            if !source.is_function_data() {
                return false;
            }
            // Only claim the pair if the target really is a byte block. An empty
            // block cannot prove it, so require a definition of width one.
            let Some(target_block) = target.as_block_mut() else {
                return false;
            };
            if target_block.definition().struct_definition().size() != 1 {
                return false;
            }
            source.as_data().unwrap_or_default().to_vec()
        }
        _ => return false,
    };

    if bytes.is_empty() {
        // Nothing authored. Leaving the target's default alone is correct.
        return true;
    }
    // A data blob is the modern spelling and a byte block the legacy one, so the
    // target's own shape says which header layout it wants.
    let source_len = bytes.len();
    let Some(bytes) = retarget_function_bytes(
        &bytes,
        source_type == TagFieldType::Data,
        target_type == TagFieldType::Data,
    ) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("{source_len} bytes is too short to be a function definition"),
        );
        return true;
    };
    // The engine's own decoder is the arbiter. A curve that will not parse is
    // reported rather than written, because a malformed function is worse for the
    // target game's tools than an unset one.
    if crate::TagFunction::parse(&bytes).is_err() {
        record_unsupported(
            context,
            path.to_owned(),
            format!(
                "{source_len} bytes of function curve did not parse as a function \
                 definition, so they were not carried across"
            ),
        );
        return true;
    }

    if target_type == TagFieldType::Data {
        set_converted(target, TagFieldData::Data(bytes), path, false, context);
        return true;
    }

    let Some(mut block) = target.as_block_mut() else {
        return true;
    };
    block.clear();
    for (index, byte) in bytes.iter().enumerate() {
        let element_index = block.add_element();
        let Some(mut element) = block.element_mut(element_index) else {
            record_unsupported(
                context,
                path.to_owned(),
                "Could not grow the target function block".to_owned(),
            );
            return true;
        };
        let Some(mut field) = element.field_at_mut(0) else {
            return true;
        };
        let field_type = field.as_ref().field_type();
        if let Some(value) = integer_field_value(field_type, i128::from(*byte))
            && field.set(value).is_err()
        {
            record_unsupported(
                context,
                path.to_owned(),
                format!("Could not write function byte {index}"),
            );
            return true;
        }
    }
    context.report.copied_exact += 1;
    true
}

fn convert_enum(
    source: TagField<'_>,
    mut target: TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    let source_name = match source.value() {
        Some(TagFieldData::CharEnum { name, .. })
        | Some(TagFieldData::ShortEnum { name, .. })
        | Some(TagFieldData::LongEnum { name, .. }) => name,
        _ => None,
    };
    let Some(source_name) = source_name else {
        if field_has_meaningful_value(source) {
            record_unsupported(
                context,
                path.to_owned(),
                "Unresolved source enum value".to_owned(),
            );
        }
        return;
    };
    let Some(TagOptions::Enum { names, .. }) = target.as_ref().options() else {
        return;
    };
    let Some((index, mapped_by_catalog)) = names.iter().enumerate().find_map(|(index, name)| {
        if option_names_match(name, &source_name) {
            Some((index, false))
        } else if context.mapping_catalog.option_names_match(
            context.group_name,
            path,
            context.source_game,
            context.target_game,
            &source_name,
            name,
        ) {
            Some((index, true))
        } else {
            None
        }
    }) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Target enum has no {source_name:?} option"),
        );
        return;
    };
    if mapped_by_catalog {
        context.report.mapped_aliases += 1;
    }
    let value = match target.as_ref().field_type() {
        TagFieldType::CharEnum => TagFieldData::CharEnum {
            value: index as i8,
            name: Some(source_name),
        },
        TagFieldType::ShortEnum => TagFieldData::ShortEnum {
            value: index as i16,
            name: Some(source_name),
        },
        TagFieldType::LongEnum => TagFieldData::LongEnum {
            value: index as i32,
            name: Some(source_name),
        },
        _ => return,
    };
    set_converted(&mut target, value, path, false, context);
}

fn convert_flags(
    source: TagField<'_>,
    mut target: TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    let names = match source.value() {
        Some(TagFieldData::ByteFlags { value, names }) => (value as u64, names),
        Some(TagFieldData::WordFlags { value, names }) => (value as u64, names),
        Some(TagFieldData::LongFlags { value, names }) => (value as u32 as u64, names),
        _ => return,
    };
    if names.0 != 0 && names.1.is_empty() {
        record_unsupported(
            context,
            path.to_owned(),
            "Set source flag bits have no names".to_owned(),
        );
        return;
    }
    let Some(TagOptions::Flags(target_options)) = target.as_ref().options() else {
        return;
    };
    let mut raw = 0u64;
    for (_, source_name) in names.1 {
        let Some((option, mapped_by_catalog)) = target_options.iter().find_map(|option| {
            if option_names_match(&option.name, &source_name) {
                Some((option, false))
            } else if context.mapping_catalog.option_names_match(
                context.group_name,
                path,
                context.source_game,
                context.target_game,
                &source_name,
                &option.name,
            ) {
                Some((option, true))
            } else {
                None
            }
        }) else {
            record_unsupported(
                context,
                path.to_owned(),
                format!("Target flags have no {source_name:?} bit"),
            );
            continue;
        };
        if mapped_by_catalog {
            context.report.mapped_aliases += 1;
        }
        raw |= 1u64 << option.bit;
    }
    let value = match target.as_ref().field_type() {
        TagFieldType::ByteFlags => TagFieldData::ByteFlags {
            value: raw as u8,
            names: Vec::new(),
        },
        TagFieldType::WordFlags => TagFieldData::WordFlags {
            value: raw as u16,
            names: Vec::new(),
        },
        TagFieldType::LongFlags => TagFieldData::LongFlags {
            value: raw as u32 as i32,
            names: Vec::new(),
        },
        _ => return,
    };
    set_converted(&mut target, value, path, false, context);
}

fn option_names_match(left: &str, right: &str) -> bool {
    if left.trim().is_empty() || right.trim().is_empty() {
        return left.trim().is_empty() && right.trim().is_empty();
    }
    let left = option_name_aliases(left);
    let right = option_name_aliases(right);
    left.iter()
        .any(|left| right.iter().any(|right| left == right))
}

fn option_name_aliases(name: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    let mut current = String::new();
    for character in name.chars() {
        match character {
            '{' | '}' | '|' => {
                let normalized = normalize_option_name(&current);
                if !normalized.is_empty() && !aliases.contains(&normalized) {
                    aliases.push(normalized);
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let normalized = normalize_option_name(&current);
    if !normalized.is_empty() && !aliases.contains(&normalized) {
        aliases.push(normalized);
    }
    aliases
}

fn normalize_option_name(name: &str) -> String {
    name.split('#')
        .next()
        .unwrap_or(name)
        .replace(['*', '!', '^'], "")
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Byte width of an integer field type, or `None` if it is not one.
fn integer_width(field_type: TagFieldType) -> Option<u32> {
    Some(match field_type {
        TagFieldType::CharInteger | TagFieldType::ByteInteger => 1,
        TagFieldType::ShortInteger | TagFieldType::WordInteger => 2,
        TagFieldType::LongInteger | TagFieldType::DwordInteger => 4,
        TagFieldType::Int64Integer | TagFieldType::QwordInteger => 8,
        _ => return None,
    })
}

fn is_signed_integer(field_type: TagFieldType) -> bool {
    matches!(
        field_type,
        TagFieldType::CharInteger
            | TagFieldType::ShortInteger
            | TagFieldType::LongInteger
            | TagFieldType::Int64Integer
    )
}

/// Reinterpret `value` between two same-width integer types of different
/// signedness, leaving every other pair alone.
///
/// The two declare the same bytes; only how an editor prints them differs. A
/// signed -1 and an unsigned 255 in a one-byte field are the same 0xFF, and the
/// engine reads the byte. Passing the mathematical value through a range check
/// instead would reject the most common sentinel in the format.
///
/// Widening and narrowing are deliberately untouched: those really can lose a
/// value, and the range check is what catches it.
fn reinterpret_same_width_integer(
    source_type: TagFieldType,
    target_type: TagFieldType,
    value: i128,
) -> i128 {
    let (Some(source_width), Some(target_width)) =
        (integer_width(source_type), integer_width(target_type))
    else {
        return value;
    };
    if source_width != target_width || is_signed_integer(source_type) == is_signed_integer(target_type)
    {
        return value;
    }
    let bits = source_width * 8;
    let mask = if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 };
    let stored = value as u128 & mask;
    if is_signed_integer(target_type) {
        // Unsigned to signed: sign-extend from the top bit of the stored width.
        let shift = 128 - bits;
        ((stored << shift) as i128) >> shift
    } else {
        // Signed to unsigned: the masked bits are already the value.
        stored as i128
    }
}

fn convert_integer(
    source: TagField<'_>,
    mut target: TagFieldMut<'_>,
    path: &str,
    context: &mut ConversionContext<'_>,
) {
    let Some(value) = source.value().and_then(integer_value) else {
        return;
    };
    let target_type = target.as_ref().field_type();
    // Two integers of the same width under different signedness are the same
    // bytes, so carry the bits rather than the mathematical value. Halo Reach
    // declares an animation graph's IK `chain index` as `char_integer` and
    // Campaign Evolved as `byte_integer`; both write 0xFF for "none", but a
    // range check sees -1 arriving at a u8 and calls it a loss. It reported
    // 2,124 of them on one character graph, none of them real.
    let value = reinterpret_same_width_integer(source.field_type(), target_type, value);
    let Some(converted) = integer_field_value(target_type, value) else {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Value {value} does not fit target {target_type:?}"),
        );
        return;
    };
    set_converted(
        &mut target,
        converted,
        path,
        source.field_type() == target_type,
        context,
    );
}

fn set_converted(
    target: &mut TagFieldMut<'_>,
    value: TagFieldData,
    path: &str,
    exact: bool,
    context: &mut ConversionContext<'_>,
) {
    if let Err(error) = target.set(value) {
        record_unsupported(
            context,
            path.to_owned(),
            format!("Could not assign target value: {error:?}"),
        );
    } else if exact {
        context.report.copied_exact += 1;
    } else {
        context.report.converted_semantic += 1;
    }
}

fn record_unsupported(context: &mut ConversionContext<'_>, path: String, message: String) {
    context.report.unsupported_source += 1;
    context.report.issues.push(ConversionIssue {
        kind: ConversionIssueKind::Unsupported,
        path,
        message,
    });
}

fn join_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}/{child}")
    }
}

fn field_has_meaningful_value(field: TagField<'_>) -> bool {
    match field.field_type() {
        TagFieldType::Struct => field.as_struct().is_some_and(struct_has_meaningful_value),
        TagFieldType::Array => field
            .as_array()
            .is_some_and(|array| array.iter().any(struct_has_meaningful_value)),
        TagFieldType::Block => field.as_block().is_some_and(|block| !block.is_empty()),
        TagFieldType::PageableResource => field
            .as_resource()
            .is_some_and(|resource| !matches!(resource.kind(), TagResourceKind::Null)),
        _ => field.value().is_some_and(value_is_meaningful),
    }
}

fn struct_has_meaningful_value(value: TagStruct<'_>) -> bool {
    value.fields().any(field_has_meaningful_value)
}

fn value_is_meaningful(value: TagFieldData) -> bool {
    match value {
        TagFieldData::String(value) | TagFieldData::LongString(value) => !value.is_empty(),
        TagFieldData::StringId(value) | TagFieldData::OldStringId(value) => {
            !value.string.is_empty()
        }
        TagFieldData::TagReference(value) => value.group_tag_and_name.is_some(),
        TagFieldData::Data(value) | TagFieldData::Custom(value) => {
            value.iter().any(|byte| *byte != 0)
        }
        TagFieldData::CharInteger(value) => value != 0,
        TagFieldData::ShortInteger(value) => value != 0,
        TagFieldData::LongInteger(value) => value != 0,
        TagFieldData::Int64Integer(value) => value != 0,
        TagFieldData::ByteInteger(value) => value != 0,
        TagFieldData::WordInteger(value) => value != 0,
        TagFieldData::DwordInteger(value) | TagFieldData::Tag(value) => value != 0,
        TagFieldData::QwordInteger(value) => value != 0,
        TagFieldData::CharEnum { value, .. } => value != 0,
        TagFieldData::ShortEnum { value, .. } => value != 0,
        TagFieldData::LongEnum { value, .. } => value != 0,
        TagFieldData::ByteFlags { value, .. } | TagFieldData::ByteBlockFlags(value) => value != 0,
        TagFieldData::WordFlags { value, .. } | TagFieldData::WordBlockFlags(value) => value != 0,
        TagFieldData::LongFlags { value, .. } | TagFieldData::LongBlockFlags(value) => value != 0,
        TagFieldData::CharBlockIndex(value) | TagFieldData::CustomCharBlockIndex(value) => {
            value != 0
        }
        TagFieldData::ShortBlockIndex(value) | TagFieldData::CustomShortBlockIndex(value) => {
            value != 0
        }
        TagFieldData::LongBlockIndex(value) | TagFieldData::CustomLongBlockIndex(value) => {
            value != 0
        }
        TagFieldData::Angle(value)
        | TagFieldData::Real(value)
        | TagFieldData::RealSlider(value)
        | TagFieldData::RealFraction(value) => value != 0.0,
        TagFieldData::Point2d(value) => value != Default::default(),
        TagFieldData::Rectangle2d(value) => value != Default::default(),
        TagFieldData::RealPoint2d(value) => value != Default::default(),
        TagFieldData::RealPoint3d(value) => value != Default::default(),
        TagFieldData::RealVector2d(value) => value != Default::default(),
        TagFieldData::RealVector3d(value) => value != Default::default(),
        TagFieldData::RealQuaternion(value) => value != Default::default(),
        TagFieldData::RealEulerAngles2d(value) => value != Default::default(),
        TagFieldData::RealEulerAngles3d(value) => value != Default::default(),
        TagFieldData::RealPlane2d(value) => value != Default::default(),
        TagFieldData::RealPlane3d(value) => value != Default::default(),
        TagFieldData::RgbColor(value) => value != Default::default(),
        TagFieldData::ArgbColor(value) => value != Default::default(),
        TagFieldData::RealRgbColor(value) => value != Default::default(),
        TagFieldData::RealArgbColor(value) => value != Default::default(),
        TagFieldData::RealHsvColor(value) => value != Default::default(),
        TagFieldData::RealAhsvColor(value) => value != Default::default(),
        TagFieldData::ShortIntegerBounds(value) => value != Default::default(),
        TagFieldData::AngleBounds(value) => value != Default::default(),
        TagFieldData::RealBounds(value) => value != Default::default(),
        TagFieldData::FractionBounds(value) => value != Default::default(),
        TagFieldData::ApiInterop(value) => value.raw.iter().any(|byte| *byte != 0),
    }
}

fn is_integer_type(value: TagFieldType) -> bool {
    matches!(
        value,
        TagFieldType::CharInteger
            | TagFieldType::ShortInteger
            | TagFieldType::LongInteger
            | TagFieldType::Int64Integer
            | TagFieldType::ByteInteger
            | TagFieldType::WordInteger
            | TagFieldType::DwordInteger
            | TagFieldType::QwordInteger
            | TagFieldType::CharBlockIndex
            | TagFieldType::CustomCharBlockIndex
            | TagFieldType::ShortBlockIndex
            | TagFieldType::CustomShortBlockIndex
            | TagFieldType::LongBlockIndex
            | TagFieldType::CustomLongBlockIndex
    )
}

pub fn is_real_scalar(value: TagFieldType) -> bool {
    matches!(
        value,
        TagFieldType::Angle
            | TagFieldType::Real
            | TagFieldType::RealSlider
            | TagFieldType::RealFraction
    )
}

/// What must happen to a real scalar's *number* when it crosses an
/// `angle`/`real` boundary.
///
/// An `angle` field stores radians and is authored in degrees — that contract is
/// pinned by `angle_fields_are_edited_in_degrees_and_stored_in_radians`, which
/// exists because Baboon once wrote 0.15 radians where Guerilla read 8.59437
/// degrees. A plain `real` stores whatever its name says. So the same quantity
/// can be typed `angle` in one game and `real` in the next, and copying the bits
/// across is wrong by 180/pi.
///
/// Six fields do exactly this across the shipped definitions: `unit`'s three
/// `grenade angle …:degrees` (ODST `angle` -> Reach/H4/H2A/CE `real`),
/// `vehicle`'s `fixed gun pitch`/`fixed gun yaw` (H3/ODST `real` -> Reach+
/// `angle`), and `scenario`'s `local north` (H1 `real` -> H2/CE `angle`).
/// Only a matching `:units` annotation on both sides justifies rescaling. Where
/// the schemas say nothing, the bits move — which is what the shipped tags show
/// is right: `vehicle`'s `fixed gun pitch` is `real` 0.25 in H3EK/H3ODSTEK and
/// `angle` 0.24993114 in HREK/H4EK. That is the same number, not one 180/pi from
/// the other, so the type changed and the stored quantity did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RealUnitChange {
    /// Both sides agree, or nothing proves otherwise: move the bits.
    Copy,
    /// Source stores radians, target stores degrees.
    RadiansToDegrees,
    /// Source stores degrees, target stores radians.
    DegreesToRadians,
}

/// The `:units` annotation a field name carries, if any — the `secs` in
/// `strike delay bounds:secs`. Cut the `#help` tail first so a colon inside the
/// help text cannot be mistaken for a unit.
fn field_unit_annotation(name: &str) -> Option<String> {
    let name = name.split('#').next().unwrap_or(name);
    let unit = name.split_once(':')?.1.trim();
    (!unit.is_empty()).then(|| unit.to_ascii_lowercase())
}

/// `source_unit`/`target_unit` come from each side's *schema*, via
/// [`SchemaFieldAliases::unit_of`] — a tag's layout has already discarded them.
fn real_scalar_unit_change(
    source_type: TagFieldType,
    source_unit: Option<&str>,
    target_type: TagFieldType,
    target_unit: Option<&str>,
) -> RealUnitChange {
    let source_angle = source_type == TagFieldType::Angle;
    let target_angle = target_type == TagFieldType::Angle;
    if source_angle == target_angle {
        return RealUnitChange::Copy;
    }
    // Only the annotation proves the authored quantity is the same on both
    // sides. `grenade angle:degrees` says `degrees` in every game that declares
    // it, so only the storage differs and the factor is provable. An
    // unannotated pair like `fixed gun pitch` says nothing, and guessing there
    // would be the same silent corruption in the other direction.
    if source_unit.is_none() || source_unit != target_unit {
        return RealUnitChange::Copy;
    }
    if source_angle {
        RealUnitChange::RadiansToDegrees
    } else {
        RealUnitChange::DegreesToRadians
    }
}

pub fn is_enum_type(value: TagFieldType) -> bool {
    matches!(
        value,
        TagFieldType::CharEnum | TagFieldType::ShortEnum | TagFieldType::LongEnum
    )
}

pub fn is_flags_type(value: TagFieldType) -> bool {
    matches!(
        value,
        TagFieldType::ByteFlags | TagFieldType::WordFlags | TagFieldType::LongFlags
    )
}

fn is_string_type(value: TagFieldType) -> bool {
    matches!(value, TagFieldType::String | TagFieldType::LongString)
}

pub fn is_string_id_type(value: TagFieldType) -> bool {
    matches!(value, TagFieldType::StringId | TagFieldType::OldStringId)
}

fn integer_value(value: TagFieldData) -> Option<i128> {
    match value {
        TagFieldData::CharInteger(value) => Some(value as i128),
        TagFieldData::ShortInteger(value) => Some(value as i128),
        TagFieldData::LongInteger(value) => Some(value as i128),
        TagFieldData::Int64Integer(value) => Some(value as i128),
        TagFieldData::ByteInteger(value) => Some(value as i128),
        TagFieldData::WordInteger(value) => Some(value as i128),
        TagFieldData::DwordInteger(value) => Some(value as i128),
        TagFieldData::QwordInteger(value) => Some(value as i128),
        TagFieldData::CharBlockIndex(value) | TagFieldData::CustomCharBlockIndex(value) => {
            Some(value as i128)
        }
        TagFieldData::ShortBlockIndex(value) | TagFieldData::CustomShortBlockIndex(value) => {
            Some(value as i128)
        }
        TagFieldData::LongBlockIndex(value) | TagFieldData::CustomLongBlockIndex(value) => {
            Some(value as i128)
        }
        _ => None,
    }
}

fn integer_field_value(field_type: TagFieldType, value: i128) -> Option<TagFieldData> {
    Some(match field_type {
        TagFieldType::CharInteger => TagFieldData::CharInteger(i8::try_from(value).ok()?),
        TagFieldType::ShortInteger => TagFieldData::ShortInteger(i16::try_from(value).ok()?),
        TagFieldType::LongInteger => TagFieldData::LongInteger(i32::try_from(value).ok()?),
        TagFieldType::Int64Integer => TagFieldData::Int64Integer(i64::try_from(value).ok()?),
        TagFieldType::ByteInteger => TagFieldData::ByteInteger(u8::try_from(value).ok()?),
        TagFieldType::WordInteger => TagFieldData::WordInteger(u16::try_from(value).ok()?),
        TagFieldType::DwordInteger => TagFieldData::DwordInteger(u32::try_from(value).ok()?),
        TagFieldType::QwordInteger => TagFieldData::QwordInteger(u64::try_from(value).ok()?),
        TagFieldType::CharBlockIndex => TagFieldData::CharBlockIndex(i8::try_from(value).ok()?),
        TagFieldType::CustomCharBlockIndex => {
            TagFieldData::CustomCharBlockIndex(i8::try_from(value).ok()?)
        }
        TagFieldType::ShortBlockIndex => TagFieldData::ShortBlockIndex(i16::try_from(value).ok()?),
        TagFieldType::CustomShortBlockIndex => {
            TagFieldData::CustomShortBlockIndex(i16::try_from(value).ok()?)
        }
        TagFieldType::LongBlockIndex => TagFieldData::LongBlockIndex(i32::try_from(value).ok()?),
        TagFieldType::CustomLongBlockIndex => {
            TagFieldData::CustomLongBlockIndex(i32::try_from(value).ok()?)
        }
        _ => return None,
    })
}

fn real_value(value: TagFieldData) -> Option<f32> {
    match value {
        TagFieldData::Angle(value)
        | TagFieldData::Real(value)
        | TagFieldData::RealSlider(value)
        | TagFieldData::RealFraction(value) => Some(value),
        _ => None,
    }
}

pub fn real_field_value(field_type: TagFieldType, value: f32) -> TagFieldData {
    match field_type {
        TagFieldType::Angle => TagFieldData::Angle(value),
        TagFieldType::RealSlider => TagFieldData::RealSlider(value),
        TagFieldType::RealFraction => TagFieldData::RealFraction(value),
        _ => TagFieldData::Real(value),
    }
}

pub fn normalize_conversion_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where a scratch kit for a test lives. Named by process and clock so
    /// parallel test threads cannot collide.
    fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "blam_convert_{label}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// Routes are shortest first, walk the chain, and never double back.
    #[test]
    fn routes_walk_the_generation_chain_shortest_first() {
        let routes = conversion_routes("halo2_mcc", "haloreach_mcc");
        assert_eq!(
            routes[0],
            vec!["halo2_mcc", "haloreach_mcc"],
            "the direct pair is always tried first, so a working conversion never detours"
        );
        assert_eq!(
            routes[1],
            vec!["halo2_mcc", "halo3_mcc", "haloreach_mcc"],
            "one intermediate before two"
        );
        // Every route is a walk along the chain in travel order, and stays
        // between the endpoints.
        let index = |game: &str| CONVERSION_CHAIN.iter().position(|e| *e == game).unwrap();
        for route in &routes {
            let positions: Vec<usize> = route.iter().map(|game| index(game)).collect();
            assert!(
                positions.windows(2).all(|pair| pair[0] < pair[1]),
                "{route:?} doubles back"
            );
            assert!(
                positions.iter().all(|p| *p >= index("halo2_mcc") && *p <= index("haloreach_mcc")),
                "{route:?} leaves the span between its endpoints"
            );
        }

        // Backwards routes travel backwards.
        let down = conversion_routes("halo4_mcc", "halo2_mcc");
        assert_eq!(down[0], vec!["halo4_mcc", "halo2_mcc"]);
        assert!(
            down.iter().any(|route| route
                == &vec![
                    "halo4_mcc".to_owned(),
                    "haloreach_mcc".to_owned(),
                    "halo3_mcc".to_owned(),
                    "halo2_mcc".to_owned()
                ]),
            "{down:?}"
        );

        // Adjacent profiles have nothing to route through.
        assert_eq!(
            conversion_routes("halo3_mcc", "halo3odst_mcc"),
            vec![vec!["halo3_mcc", "halo3odst_mcc"]]
        );

        // Campaign Evolved has exactly one partner and is never a waypoint.
        assert_eq!(
            conversion_routes(CAMPAIGN_EVOLVED_GAME, CAMPAIGN_EVOLVED_PARENT),
            vec![vec![CAMPAIGN_EVOLVED_GAME, CAMPAIGN_EVOLVED_PARENT]]
        );
        assert!(
            conversion_routes("halo3_mcc", CAMPAIGN_EVOLVED_GAME).is_empty(),
            "Halo 3 must not reach Campaign Evolved by way of Reach"
        );
        assert!(
            conversion_routes("haloce_mcc", "halo2amp_mcc")
                .iter()
                .all(|route| !route.iter().any(|game| game == CAMPAIGN_EVOLVED_GAME))
        );
    }

    /// A bitmap Halo 2 cannot hand to Reach directly arrives by way of Halo 3.
    ///
    /// This is the case the routing exists for, and it is a real refusal rather
    /// than a contrived one: Reach's bitmap block has no `pixels offset` field at
    /// all, so pixel data carried straight there has nothing indexing it, and the
    /// catalog refuses the pair by name. Halo 3's does, and Halo 3 to Reach works
    /// — so the tag gets there in two hops.
    #[test]
    fn a_halo_2_bitmap_reaches_reach_through_halo_3() {
        let definitions = locate_definitions_root();
        let source = TagFile::new(definitions.join("halo2_mcc/bitmap.json")).unwrap();

        let direct = analyze_conversion_with_templates(
            &source,
            "halo2_mcc",
            "haloreach_mcc",
            &definitions,
            None,
        );
        let Err(refusal) = direct else {
            panic!("the direct pair should be refused by the catalog");
        };
        assert!(
            refusal.to_ascii_lowercase().contains("halo 3")
                || refusal.to_ascii_lowercase().contains("halo3"),
            "the refusal should already name the way round: {refusal}"
        );

        let routed = analyze_conversion_routed(
            &source,
            "halo2_mcc",
            "haloreach_mcc",
            &definitions,
            &(),
        )
        .expect("routing through Halo 3 carries it");
        assert_eq!(
            routed.route,
            vec!["halo2_mcc", "halo3_mcc", "haloreach_mcc"],
            "and it records how it got there"
        );
        assert_eq!(routed.target_extension, "bitmap");
        // The tag that comes out is a real Reach tag: it writes, and it reopens.
        let bytes = routed.tag.write_to_bytes().unwrap();
        let reopened = TagFile::read_from_bytes(&bytes).unwrap();
        assert_eq!(reopened.header.group_tag, u32::from_be_bytes(*b"bitm"));
    }

    /// The same route, with a real Halo 2 bitmap and real kit templates.
    ///
    /// The schema-built version above proves the routing *machinery*; this proves
    /// the thing the user asked for. It needs real tags because a `TagFile::new`
    /// Halo 2 bitmap is an MCC container wearing Halo 2's schema, not a classic
    /// container — so it never exercises the classic read path, and it carries no
    /// pixels, which are the whole reason this pair is refused.
    ///
    /// Self-skips without the kits.
    #[test]
    fn a_real_halo_2_bitmap_carries_its_pixels_to_reach_through_halo_3() {
        let (Some(h2), Some(h3), Some(reach)) = (
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
            kit_tags("BLAM_TEST_HREK", "HREK"),
        ) else {
            eprintln!("skipping: needs H2EK, H3EK and HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let group_tag = u32::from_be_bytes(*b"bitm");

        // A bitmap with pixels in it, so "did the payload survive?" is a question
        // with an answer. Bounded scan: the first few stock bitmaps will do.
        let source = tags_with_extension(&h2, "bitmap")
            .into_iter()
            .take(40)
            .find_map(|path| {
                let tag =
                    read_tag_for_conversion(&path, Some("halo2_mcc"), Some(&definitions), group_tag)
                        .ok()?;
                (blob_bytes(&tag) > 0).then_some((path, tag))
            });
        let Some((source_path, source)) = source else {
            eprintln!("skipping: no H2EK bitmap with pixel data in the first 40");
            return;
        };
        let source_pixels = blob_bytes(&source);

        let mut templates: HashMap<String, NativeTemplateIndex> = HashMap::new();
        for (game, root) in [("halo3_mcc", &h3), ("haloreach_mcc", &reach)] {
            let groups = GameTagIndex::load(&definitions, game).unwrap();
            templates.insert(game.to_owned(), NativeTemplateIndex::build(root, &groups));
        }

        let draft = analyze_conversion_routed(
            &source,
            "halo2_mcc",
            "haloreach_mcc",
            &definitions,
            &templates,
        )
        .unwrap_or_else(|error| panic!("{}: {error}", source_path.display()));

        assert_eq!(draft.route, vec!["halo2_mcc", "halo3_mcc", "haloreach_mcc"]);
        // The point of the detour: the pixels are still there at the far end.
        let landed = blob_bytes(&draft.tag);
        assert!(
            landed > 0,
            "{}: {source_pixels} bytes of pixel data went in and none came out",
            source_path.display()
        );
        // And the result is a tag Reach's tools can open at all.
        let bytes = draft.tag.write_to_bytes().unwrap();
        let reopened = TagFile::read_from_bytes(&bytes).unwrap();
        assert_eq!(reopened.header.group_tag, group_tag);
        // Attribute any change in payload size to the hop that caused it, rather
        // than reporting a number with no owner. The Halo 2 to Halo 3 blob carry
        // is reviewed; a change appearing only at the second hop would not be.
        let midpoint = analyze_conversion_with_templates(
            &source,
            "halo2_mcc",
            "halo3_mcc",
            &definitions,
            templates.templates_for("halo3_mcc"),
        )
        .map(|hop| blob_bytes(&hop.tag))
        .unwrap_or(0);
        eprintln!(
            "{} : {source_pixels} -> {midpoint} (halo3) -> {landed} (reach) bytes of pixel data",
            source_path.display()
        );
        assert_eq!(
            midpoint, landed,
            "the Halo 3 to Reach hop changed the payload size; only the classic              blob carry into Halo 3 is reviewed for that"
        );
    }

    /// A Halo 2 scenario converts, and arrives with no compiled scripts but with
    /// its script *source* intact.
    ///
    /// This is the case the user hit: the conversion refused outright because the
    /// string table is a `data` blob Halo 3 declares under a different data
    /// definition, and a blob that cannot cross fails the whole tag rather than
    /// being written empty. The blob genuinely cannot cross, but refusing the
    /// scenario over it was the wrong answer — and carrying it would have been a
    /// worse one, because the syntax datums that index it renumber between
    /// engines.
    ///
    /// Three things are asserted together on purpose. Emptied scripts without the
    /// source surviving would be data loss; emptied strings with the datums still
    /// present would be datums indexing nothing; and either without the warning
    /// would be a silent change to what the scenario does.
    ///
    /// Self-skips without the kits.
    #[test]
    fn a_halo_2_scenario_converts_without_its_compiled_scripts() {
        let (Some(h2), Some(h3)) = (
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
        ) else {
            eprintln!("skipping: needs H2EK and H3EK");
            return;
        };
        let definitions = locate_definitions_root();
        let group_tag = u32::from_be_bytes(*b"scnr");
        let block_len = |tag: &TagFile, name: &str| -> usize {
            tag.root()
                .fields()
                .find(|field| clean_field_key(field.name()) == name)
                .and_then(|field| field.as_block())
                .map(|block| block.len())
                .unwrap_or(0)
        };

        // A scenario that actually has scripts *and* their source, so every
        // assertion below has something to bite on.
        let picked = tags_with_extension(&h2, "scenario")
            .into_iter()
            .take(40)
            .find_map(|path| {
                let tag =
                    read_tag_for_conversion(&path, Some("halo2_mcc"), Some(&definitions), group_tag)
                        .ok()?;
                let datums = block_len(&tag, "hs syntax datums");
                let sources = block_len(&tag, "source files");
                (datums > 0 && sources > 0).then_some((path, tag, datums, sources))
            });
        let Some((path, source, source_datums, source_files)) = picked else {
            eprintln!("skipping: no H2EK scenario with both compiled scripts and .hsc source");
            return;
        };

        let groups = GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
        let templates = NativeTemplateIndex::build(&h3, &groups);
        let draft = analyze_conversion_with_templates(
            &source,
            "halo2_mcc",
            "halo3_mcc",
            &definitions,
            Some(&templates),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

        // Nothing compiled came across, and nothing came across half.
        for field in ["hs syntax datums", "scripts", "globals"] {
            assert_eq!(
                block_len(&draft.tag, field),
                0,
                "{}: {field} still carries Halo 2 bytecode",
                path.display()
            );
        }
        let strings = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "script string data")
            .and_then(|field| field.value())
            .and_then(|value| match value {
                TagFieldData::Data(bytes) => Some(bytes.len()),
                _ => None,
            })
            .unwrap_or(0);
        assert_eq!(strings, 0, "{}: the string table survived", path.display());

        // The source text did come across — that is what makes the scripts
        // recoverable rather than lost.
        assert_eq!(
            block_len(&draft.tag, "source files"),
            source_files,
            "{}: the .hsc source did not carry",
            path.display()
        );

        // And it says so, naming the source as the way back.
        let warning = draft
            .report
            .issues
            .iter()
            .find(|issue| issue.message.contains("Compiled scripts were cleared"))
            .unwrap_or_else(|| panic!("{}: no warning raised", path.display()));
        assert_eq!(warning.kind, ConversionIssueKind::Warning);
        assert!(warning.message.contains(".hsc"), "{}", warning.message);

        // The result is a Halo 3 tag that writes and reopens.
        let bytes = draft.tag.write_to_bytes().unwrap();
        let reopened = TagFile::read_from_bytes(&bytes).unwrap();
        assert_eq!(reopened.header.group_tag, group_tag);
        eprintln!(
            "{}: {source_datums} datums dropped, {source_files} source file(s) kept",
            path.display()
        );
    }

    /// A Halo CE scenario reaches Halo 2, editor blob and all.
    ///
    /// The same failure shape as the reported one, on a different blob: Sapien's
    /// `editor scenario data` is declared under a different data definition in
    /// every profile, so the opaque copy path refused the whole scenario over a
    /// scratch buffer. Reviewed as a drop rather than aliased, because one game's
    /// editor state means nothing to another's.
    ///
    /// Self-skips without the kits.
    #[test]
    fn a_halo_ce_scenario_reaches_halo_2_despite_its_editor_blob() {
        let (Some(h1), Some(h2)) = (
            kit_tags("BLAM_TEST_HCEEK", "HCEEK"),
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
        ) else {
            eprintln!("skipping: needs HCEEK and H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let group_tag = u32::from_be_bytes(*b"scnr");
        // A scenario that actually carries the blob, so the drop is exercised
        // rather than trivially satisfied by an empty field.
        let picked = tags_with_extension(&h1, "scenario")
            .into_iter()
            .take(30)
            .find_map(|path| {
                let tag =
                    read_tag_for_conversion(&path, Some("haloce_mcc"), Some(&definitions), group_tag)
                        .ok()?;
                let filled = tag.root().fields().any(|field| {
                    clean_field_key(field.name()) == "editor scenario data"
                        && matches!(field.value(), Some(TagFieldData::Data(bytes)) if !bytes.is_empty())
                });
                filled.then_some((path, tag))
            });
        let Some((path, source)) = picked else {
            eprintln!("skipping: no HCEEK scenario carries editor scenario data");
            return;
        };

        let groups = GameTagIndex::load(&definitions, "halo2_mcc").unwrap();
        let templates = NativeTemplateIndex::build(&h2, &groups);
        let draft = analyze_conversion_with_templates(
            &source,
            "haloce_mcc",
            "halo2_mcc",
            &definitions,
            Some(&templates),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        // A classic target has to come from a kit tag; if that stopped working
        // this would silently fall back and the assertion says so.
        assert!(
            draft.native_layout_template.is_some(),
            "{}: a Halo 2 target needs a kit-authored template",
            path.display()
        );
        assert!(draft.tag.classic_engine().is_some(), "{}", path.display());
    }

    /// A same-game conversion leaves the scripts alone.
    ///
    /// The whole justification is that function tables renumber *between*
    /// engines, so a rule that fired within one would be destroying data for no
    /// reason. Cheap to assert and it pins the scope.
    #[test]
    fn scripts_are_only_stripped_when_the_engine_changes() {
        assert!(
            !conversion_pair_supported("halo3_mcc", "halo3_mcc"),
            "a same-game pair is refused before any of this is reached, which is              what makes the guard in strip_cross_engine_scripts a belt-and-braces              check rather than the only thing standing between a scenario and its              own scripts"
        );
    }

    /// A direct conversion is left exactly as it was — no route, no detour.
    #[test]
    fn a_direct_conversion_is_not_routed() {
        let definitions = locate_definitions_root();
        let source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        let draft = analyze_conversion_routed(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            &(),
        )
        .unwrap();
        assert!(
            draft.route.is_empty(),
            "a pair that works directly must not report a route: {:?}",
            draft.route
        );
    }

    /// When no route works, the error says what was tried rather than just "no".
    #[test]
    fn an_unroutable_pair_reports_every_route_it_tried() {
        let definitions = locate_definitions_root();
        let source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        // Campaign Evolved pairs only with Reach, so this has no route at all.
        let Err(error) = analyze_conversion_routed(
            &source,
            "halo3_mcc",
            CAMPAIGN_EVOLVED_GAME,
            &definitions,
            &(),
        ) else {
            panic!("Halo 3 cannot reach Campaign Evolved");
        };
        assert!(
            error.contains(CAMPAIGN_EVOLVED_PARENT),
            "the refusal should say what to do instead: {error}"
        );
    }

    /// The header peek agrees with a full parse, and refuses what is not a tag.
    ///
    /// This is the primitive the template search sifts thousands of candidates
    /// with, so a peek that disagreed with the parse would pick a different
    /// template than the checks below it would accept — a silent divergence
    /// rather than a failure.
    #[test]
    fn a_peeked_header_says_what_a_parsed_one_does() {
        let root = scratch("peek");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("sample.weapon");
        let mut tag =
            TagFile::new(locate_definitions_root().join("halo3_mcc/weapon.json")).unwrap();
        tag.header.build_version = 1;
        tag.header.build_number = 2;
        tag.header.version = 7;
        tag.write_atomic(&path).unwrap();

        let (peeked, endian) = TagFileHeader::peek(&path).expect("a written tag peeks");
        let parsed = TagFile::read(&path).unwrap();
        assert_eq!(endian, parsed.endian);
        assert_eq!(peeked.group_tag, parsed.header.group_tag);
        assert_eq!(peeked.version, parsed.header.version);
        assert_eq!(peeked.build_version, parsed.header.build_version);
        assert_eq!(peeked.build_number, parsed.header.build_number);
        assert_eq!(peeked.group_tag, u32::from_be_bytes(*b"weap"));

        // Not a tag at all, and too short to be one: both are ordinary things to
        // meet when walking a kit, and neither may panic.
        fs::write(root.join("notes.txt"), b"this is not a tag file at all ok").unwrap();
        assert!(TagFileHeader::peek(root.join("notes.txt")).is_err());
        fs::write(root.join("stub.weapon"), b"short").unwrap();
        assert!(TagFileHeader::peek(root.join("stub.weapon")).is_err());
        assert!(TagFileHeader::peek(root.join("absent.weapon")).is_err());

        fs::remove_dir_all(root).unwrap();
    }

    /// The template search gives up after a bounded number of candidates.
    ///
    /// Both halves matter. Finding one inside the bound is the ordinary case and
    /// proves the search still works; not finding one past the bound is the
    /// deliberate limit, and without a test it would be indistinguishable from
    /// the search being broken. What it buys: proving a group has *no* usable
    /// template used to mean opening every tag in it, and Halo Reach ships
    /// 10,675 bitmaps — 6.4 GB, none acceptable, half a minute to say so.
    #[test]
    fn the_native_template_search_stops_after_a_bounded_number_of_candidates() {
        let definitions = locate_definitions_root();
        let root = scratch("scan_limit");
        let tags = root.join("tags/objects");
        fs::create_dir_all(&tags).unwrap();

        // Rejected: `version == u32::MAX` is what a tag with no recorded source
        // revision carries, which is also what Baboon stamps on its own output.
        let mut reject =
            TagFile::new(definitions.join("haloreach_mcc/weapon.json")).unwrap();
        apply_editing_kit_mcc_header(&mut reject, "haloreach_mcc").unwrap();
        assert_eq!(reject.header.version, u32::MAX, "the reject must be rejected");
        // Accepted: any recorded revision will do.
        let mut accept =
            TagFile::new(definitions.join("haloreach_mcc/weapon.json")).unwrap();
        apply_editing_kit_mcc_header(&mut accept, "haloreach_mcc").unwrap();
        accept.header.version = 3;

        // Sorted order is what the search walks, so zero-padded names put the
        // acceptable tag at an exact, chosen index.
        let place = |index: usize, tag: &TagFile| {
            tag.write_atomic(tags.join(format!("tag_{index:05}.weapon")))
                .unwrap();
        };
        let past_the_bound = NATIVE_TEMPLATE_SCAN_LIMIT + 20;
        for index in 0..past_the_bound {
            place(index, &reject);
        }
        place(past_the_bound, &accept);

        let groups = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        let analyze = |tags_root: &Path| {
            let templates = NativeTemplateIndex::build(tags_root, &groups);
            analyze_conversion_with_templates(
                &source,
                "halo3_mcc",
                "haloreach_mcc",
                &definitions,
                Some(&templates),
            )
            .unwrap()
            .native_layout_template
        };

        assert!(
            analyze(&root.join("tags")).is_none(),
            "an acceptable tag {past_the_bound} deep is past the bound and must not be found"
        );

        // Move it inside the bound and the same search finds it, so the miss
        // above is the bound rather than the search failing outright.
        place(1, &accept);
        assert!(
            analyze(&root.join("tags")).is_some(),
            "an acceptable tag at index 1 is well inside the bound"
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Clone)]
    struct LeafSeed {
        ordinal: usize,
        field_type: TagFieldType,
        option: Option<String>,
    }

    fn first_direct_leaf(tag: &TagFile, wanted: impl Fn(TagFieldType) -> bool) -> LeafSeed {
        tag.root()
            .fields()
            .enumerate()
            .find_map(|(ordinal, field)| {
                wanted(field.field_type()).then(|| {
                    let option = match field.options() {
                        Some(TagOptions::Enum { names, .. }) => {
                            names.get(1).or(names.first()).map(|s| (*s).to_owned())
                        }
                        Some(TagOptions::Flags(options)) => {
                            options.first().map(|option| option.name.to_owned())
                        }
                        None => None,
                    };
                    LeafSeed {
                        ordinal,
                        field_type: field.field_type(),
                        option,
                    }
                })
            })
            .expect("expected direct field type")
    }

    fn seed_weapon_fields(tag: &mut TagFile) {
        let reference =
            first_direct_leaf(tag, |field_type| field_type == TagFieldType::TagReference);
        tag.root_mut()
            .field_at_mut(reference.ordinal)
            .unwrap()
            .set(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((
                    u32::from_be_bytes(*b"bitm"),
                    "objects\\test\\icon".to_owned(),
                )),
            }))
            .unwrap();

        let real = first_direct_leaf(tag, is_real_scalar);
        tag.root_mut()
            .field_at_mut(real.ordinal)
            .unwrap()
            .set(real_field_value(real.field_type, 0.625))
            .unwrap();

        let enumeration = first_direct_leaf(tag, is_enum_type);
        let enum_name = enumeration.option.unwrap();
        let enum_value = match enumeration.field_type {
            TagFieldType::CharEnum => TagFieldData::CharEnum {
                value: 1,
                name: Some(enum_name),
            },
            TagFieldType::ShortEnum => TagFieldData::ShortEnum {
                value: 1,
                name: Some(enum_name),
            },
            TagFieldType::LongEnum => TagFieldData::LongEnum {
                value: 1,
                name: Some(enum_name),
            },
            _ => unreachable!(),
        };
        tag.root_mut()
            .field_at_mut(enumeration.ordinal)
            .unwrap()
            .set(enum_value)
            .unwrap();

        let flags = first_direct_leaf(tag, is_flags_type);
        let flag_name = flags.option.unwrap();
        let flag_value = match flags.field_type {
            TagFieldType::ByteFlags => TagFieldData::ByteFlags {
                value: 1,
                names: vec![(0, flag_name)],
            },
            TagFieldType::WordFlags => TagFieldData::WordFlags {
                value: 1,
                names: vec![(0, flag_name)],
            },
            TagFieldType::LongFlags => TagFieldData::LongFlags {
                value: 1,
                names: vec![(0, flag_name)],
            },
            _ => unreachable!(),
        };
        tag.root_mut()
            .field_at_mut(flags.ordinal)
            .unwrap()
            .set(flag_value)
            .unwrap();

        let string_id = first_direct_leaf(tag, is_string_id_type);
        let string_value = if string_id.field_type == TagFieldType::StringId {
            TagFieldData::StringId(StringIdData {
                string: "converted-label".to_owned(),
            })
        } else {
            TagFieldData::OldStringId(StringIdData {
                string: "converted-label".to_owned(),
            })
        };
        tag.root_mut()
            .field_at_mut(string_id.ordinal)
            .unwrap()
            .set(string_value)
            .unwrap();

        let magazines = tag
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| {
                field.field_type() == TagFieldType::Block
                    && clean_field_key(field.name()) == "magazines"
            })
            .map(|(ordinal, _)| ordinal)
            .expect("weapon has magazines block");
        let mut root = tag.root_mut();
        let mut field = root.field_at_mut(magazines).unwrap();
        let mut block = field.as_block_mut().unwrap();
        block.add_element();
    }

    #[test]
    fn halo3_weapon_converts_to_odst_and_reopens() {
        let root = locate_definitions_root();
        let mut source = TagFile::new(root.join("halo3_mcc/weapon.json")).unwrap();
        seed_weapon_fields(&mut source);

        let draft = analyze_conversion(&source, "halo3_mcc", "halo3odst_mcc", &root, None).unwrap();
        assert!(draft.native_layout_template.is_none());
        assert!(draft.report.issues.iter().any(|issue| {
            issue.path == "target layout" && issue.message.contains("native editing-kit")
        }));
        assert_eq!(draft.tag.group().tag, u32::from_be_bytes(*b"weap"));
        assert_eq!(draft.tag.header.build_version, 1);
        assert_eq!(draft.tag.header.build_number, 1);
        assert_eq!(draft.tag.header.version, u32::MAX);
        assert!(draft.report.copied_exact > 0);
        assert!(draft.report.converted_semantic > 0);
        assert!(draft
            .tag
            .root()
            .fields()
            .filter_map(|field| field.value())
            .any(|value| matches!(value, TagFieldData::TagReference(reference) if reference.group_tag_and_name.as_ref().is_some_and(|(group, path)| *group == u32::from_be_bytes(*b"bitm") && path == "objects\\test\\icon"))));
        assert_eq!(
            draft
                .tag
                .root()
                .field("magazines")
                .and_then(|field| field.as_block())
                .map(|block| block.len()),
            Some(1)
        );

        let mut path = std::env::temp_dir();
        path.push(format!(
            "baboon_conversion_weapon_{}_{}.weapon",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        draft.tag.write_atomic(&path).unwrap();
        let reopened = TagFile::read(&path).unwrap();
        assert_eq!(reopened.group().tag, u32::from_be_bytes(*b"weap"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn compute_shader_group_fourcc_remaps_for_reach() {
        let root = locate_definitions_root();
        let source = GameTagIndex::load(&root, "halo3_mcc").unwrap();
        let target = GameTagIndex::load(&root, "haloreach_mcc").unwrap();
        let name = source.by_tag.get(&u32::from_be_bytes(*b"cmpu")).unwrap();
        assert_eq!(
            target.by_name.get(name),
            Some(&u32::from_be_bytes(*b"cmps"))
        );
    }

    #[test]
    fn halo3_weapon_analyzes_for_every_supported_later_profile() {
        let root = locate_definitions_root();
        let source = TagFile::new(root.join("halo3_mcc/weapon.json")).unwrap();
        for target in ["haloreach_mcc", "halo4_mcc", "halo2amp_mcc"] {
            let draft = analyze_conversion(&source, "halo3_mcc", target, &root, None)
                .unwrap_or_else(|error| panic!("{target}: {error}"));
            assert_eq!(draft.tag.group().tag, u32::from_be_bytes(*b"weap"));
            assert_eq!(draft.tag.header.build_version, 1);
            assert_eq!(
                draft.tag.header.build_number,
                if target == "haloreach_mcc" || target == "halo4_mcc" || target == "halo2amp_mcc" {
                    2
                } else {
                    1
                }
            );
            assert_eq!(draft.tag.header.version, u32::MAX);
            draft.tag.write_to_bytes().unwrap();
        }
    }

    #[test]
    fn editing_kit_header_defaults_match_profile_generations() {
        let root = locate_definitions_root();
        for (game, build_number) in [
            ("halo3_mcc", 1),
            ("halo3odst_mcc", 1),
            ("haloreach_mcc", 2),
            ("halo4_mcc", 2),
            ("halo2amp_mcc", 2),
        ] {
            let mut tag = TagFile::new(root.join(game).join("globals.json")).unwrap();
            apply_editing_kit_mcc_header(&mut tag, game).unwrap();
            let bytes = tag.write_to_bytes().unwrap();
            assert_eq!(i32::from_le_bytes(bytes[36..40].try_into().unwrap()), 1);
            assert_eq!(
                i32::from_le_bytes(bytes[40..44].try_into().unwrap()),
                build_number
            );
            assert_eq!(
                u32::from_le_bytes(bytes[44..48].try_into().unwrap()),
                u32::MAX
            );
        }
    }

    #[test]
    fn missing_target_group_is_rejected() {
        let root = locate_definitions_root();
        let source = TagFile::new(root.join("halo3_mcc/gui_button_key_definition.json")).unwrap();
        let error = analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &root, None)
            .err()
            .expect("group should be absent");
        assert!(error.contains("has no gui_button_key_definition tag group"));
    }

    // `fingerprint_changes_with_source_edits` stayed in the editor with
    // `tag_fingerprint`: hashing a tag to notice a stale preview is a UI staleness
    // concern, not something conversion decides, and it is the only reason the
    // converter ever pulled in a hashing crate.

    #[test]
    fn integer_conversion_rejects_overflow() {
        assert!(integer_field_value(TagFieldType::ByteInteger, 256).is_none());
        assert!(integer_field_value(TagFieldType::CharInteger, -129).is_none());
        assert!(matches!(
            integer_field_value(TagFieldType::WordInteger, 65_535),
            Some(TagFieldData::WordInteger(65_535))
        ));
    }

    #[test]
    fn enum_and_flag_option_aliases_match_semantically() {
        assert!(option_names_match(
            "particle correlation 1{particle random 1}",
            "particle correlation 1"
        ));
        assert!(option_names_match(
            "resolved manually{resolved in postprocess|required by game}",
            "required by game"
        ));
        assert!(!option_names_match(
            "particle correlation 1",
            "particle correlation 2"
        ));
        assert!(option_names_match(" ", " "));
        assert!(option_names_match(
            "spew#fires its primary action barrel whenever the trigger is down",
            "spew"
        ));
        assert!(field_names_match(
            "coefficient*!",
            "spherical harmonic{coefficient}*!"
        ));
        assert!(!field_names_match("acceleration", "deceleration"));
    }

    #[test]
    fn mapping_catalog_is_scoped_and_reversible() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        let coefficient_guid = parse_schema_guid("411d27e578471259100c498a81d58751").unwrap();
        assert!(catalog.field_names_match(FieldMappingRequest {
            group: "render_model",
            source_game: "halo3_mcc",
            target_game: "haloreach_mcc",
            source_guid: coefficient_guid,
            target_guid: coefficient_guid,
            source_name: "coefficient",
            target_name: "spherical harmonic",
        }));
        assert!(catalog.field_names_match(FieldMappingRequest {
            group: "render_model",
            source_game: "haloreach_mcc",
            target_game: "halo3_mcc",
            source_guid: coefficient_guid,
            target_guid: coefficient_guid,
            source_name: "spherical harmonic",
            target_name: "coefficient",
        }));
        assert!(catalog.option_names_match(
            "particle",
            "main flags",
            "halo3_mcc",
            "haloreach_mcc",
            "dies in media",
            "dies in water"
        ));
        assert!(catalog.option_names_match(
            "particle",
            "main flags",
            "haloreach_mcc",
            "halo3_mcc",
            "dies in water",
            "dies in media"
        ));
        assert!(catalog.option_names_match(
            "effect",
            "systems[0]/emitters[0]/movement/flags",
            "halo3_mcc",
            "halo4_mcc",
            "collide with media",
            "collide with water"
        ));
        assert!(catalog.option_names_match(
            "bitmap",
            "bitmap curve",
            "halo3_mcc",
            "haloreach_mcc",
            "sRGB",
            "sRGB (gamma 2.2)"
        ));
        assert!(catalog.option_names_match(
            "weapon",
            "secondary flags",
            "halo3_mcc",
            "halo2amp_mcc",
            "magnitizes only when zoomed",
            "magnetizes only when zoomed"
        ));
        assert!(!catalog.option_names_match(
            "weapon",
            "main flags",
            "halo3_mcc",
            "haloreach_mcc",
            "dies in media",
            "dies in water"
        ));
    }

    #[test]
    fn mapping_catalog_covers_complete_common_tag_base() {
        let root = locate_definitions_root();
        let catalog = ConversionMappingCatalog::load().unwrap();
        let indexes = CONVERSION_GAMES
            .iter()
            .map(|game| GameTagIndex::load(&root, game).unwrap())
            .collect::<Vec<_>>();
        let common_groups = indexes[0]
            .by_name
            .keys()
            .filter(|group| {
                indexes[1..]
                    .iter()
                    .all(|index| index.by_name.contains_key(*group))
            })
            .cloned()
            .collect::<HashSet<_>>();
        let covered_groups = catalog
            .covered_groups
            .iter()
            .map(|group| group.to_ascii_lowercase())
            .collect::<HashSet<_>>();

        assert_eq!(
            common_groups.len(),
            125,
            "common tag-base denominator changed"
        );
        assert!(
            covered_groups.is_subset(&common_groups),
            "covered groups must exist in every supported profile: {:?}",
            covered_groups
                .difference(&common_groups)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            covered_groups, common_groups,
            "the mapping catalog must cover the complete common tag base"
        );
    }

    #[test]
    fn every_covered_group_pair_is_compatible_or_explicitly_rejected() {
        let root = locate_definitions_root();
        let catalog = ConversionMappingCatalog::load().unwrap();
        let mut failures = Vec::new();
        for group in &catalog.covered_groups {
            // Layout-sensitive output intentionally requires a native
            // editing-kit template. Its path has dedicated tests below.
            if requires_native_layout_template(group) {
                continue;
            }
            for source_game in CONVERSION_GAMES {
                let source = match std::panic::catch_unwind(|| {
                    TagFile::new(root.join(source_game).join(format!("{group}.json")))
                }) {
                    Ok(Ok(source)) => source,
                    Ok(Err(error)) => {
                        failures.push(format!("{source_game}/{group}: {error}"));
                        continue;
                    }
                    Err(_) => {
                        failures.push(format!(
                            "{source_game}/{group}: schema construction panicked"
                        ));
                        continue;
                    }
                };
                for target_game in CONVERSION_GAMES {
                    if source_game == target_game {
                        continue;
                    }
                    let analysis = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        analyze_conversion(&source, source_game, target_game, &root, None)
                    }));
                    if analysis.is_err() {
                        failures.push(format!(
                            "{group}: {source_game} -> {target_game}: conversion panicked"
                        ));
                    } else if let Ok(Err(error)) = analysis {
                        if catalog
                            .incompatibility_reason(group, source_game, target_game)
                            .is_none()
                        {
                            failures
                                .push(format!("{group}: {source_game} -> {target_game}: {error}"));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn every_available_tag_group_pair_is_compatible_or_explicitly_rejected() {
        let root = locate_definitions_root();
        let catalog = ConversionMappingCatalog::load().unwrap();
        let indexes = CONVERSION_GAMES
            .iter()
            .map(|game| GameTagIndex::load(&root, game).unwrap())
            .collect::<Vec<_>>();
        let mut all_groups = indexes
            .iter()
            .flat_map(|index| index.by_name.keys().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        all_groups.sort();
        assert_eq!(all_groups.len(), 340, "supported tag-group union changed");

        let mut failures = Vec::new();
        for group in &all_groups {
            if requires_native_layout_template(group) {
                continue;
            }
            for (source_index, source_game) in CONVERSION_GAMES.iter().enumerate() {
                if !indexes[source_index].by_name.contains_key(group) {
                    continue;
                }
                if catalog.unusable_schema_reason(group, source_game).is_some() {
                    continue;
                }
                // Resolve the schema by its real (cased) filename via `by_tag`.
                // `group` is a lowercased `by_name` key, so `{group}.json` only
                // matches the file on case-insensitive filesystems (macOS/Windows)
                // — a few schema files are mixed-case (e.g.
                // `GameEngineFirefightVariantTag.json`) and would be missed on
                // Linux. The runtime loader uses the `by_tag` casing, so match it.
                let index = &indexes[source_index];
                let name = index
                    .by_tag
                    .get(&index.by_name[group])
                    .map_or(group.as_str(), String::as_str);
                let source = TagFile::new(root.join(source_game).join(format!("{name}.json")))
                    .unwrap_or_else(|error| panic!("{source_game}/{name}: {error}"));
                for (target_index, target_game) in CONVERSION_GAMES.iter().enumerate() {
                    if source_game == target_game
                        || !indexes[target_index].by_name.contains_key(group)
                    {
                        continue;
                    }
                    if catalog.unusable_schema_reason(group, target_game).is_some() {
                        continue;
                    }
                    if let Err(error) =
                        analyze_conversion(&source, source_game, target_game, &root, None)
                    {
                        if catalog
                            .incompatibility_reason(group, source_game, target_game)
                            .is_none()
                        {
                            failures
                                .push(format!("{group}: {source_game} -> {target_game}: {error}"));
                        }
                    }
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn schema_alias_table_maps_renamed_render_model_coefficient() {
        let definitions = locate_definitions_root();
        let aliases =
            SchemaFieldAliases::load(&definitions.join("haloreach_mcc/render_model.json")).unwrap();
        let guid = parse_schema_guid("411d27e578471259100c498a81d58751").unwrap();
        assert!(aliases.matches(guid, "default_lightprobe", "spherical harmonic", "coefficient"));
    }

    /// A classic game's aliases must not leak between its structs.
    ///
    /// Every struct in `haloce_mcc` and `halo2_mcc` carries an all-zero GUID, so
    /// a GUID-keyed table collapses the whole group into one bucket and starts
    /// answering "yes, those two field names are aliases" for arbitrary pairs
    /// from unrelated structs. Keying falls back to the struct's own name — a
    /// JSON object key, unique within the group — precisely for these profiles.
    #[test]
    fn a_zero_guid_struct_is_keyed_by_name_so_classic_aliases_do_not_collide() {
        let zero = [0u8; 16];
        let real = parse_schema_guid("411d27e578471259100c498a81d58751").unwrap();
        // Two different zero-GUID structs get two different keys...
        assert_ne!(
            schema_struct_key(zero, "weapon_block_struct"),
            schema_struct_key(zero, "unit_block_struct")
        );
        // ...while a real GUID keys on itself, so a rename still shares an entry.
        assert_eq!(
            schema_struct_key(real, "weapon_group"),
            schema_struct_key(real, "weapon_block_struct")
        );

        // And on a real classic schema, an alias found in one struct must not
        // answer for a field name that lives in another.
        let definitions = locate_definitions_root();
        let path = definitions.join("halo2_mcc/weapon.json");
        if !path.is_file() {
            eprintln!("skipping: no halo2_mcc definitions");
            return;
        }
        let aliases = SchemaFieldAliases::load(&path).unwrap();
        let buckets = aliases.by_struct.len();
        assert!(
            buckets > 1,
            "a zero-GUID classic group collapsed into {buckets} alias bucket(s)"
        );
    }

    #[test]
    fn light_values_reparent_into_halo4_midnight_parameters() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/light.json")).unwrap();
        let ordinal = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "destroy light after")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        source
            .root_mut()
            .field_at_mut(ordinal)
            .unwrap()
            .set(TagFieldData::Real(7.5))
            .unwrap();

        let draft =
            analyze_conversion(&source, "halo3_mcc", "halo4_mcc", &definitions, None).unwrap();
        let midnight = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "midnight_light_parameters")
            .and_then(|field| field.as_struct())
            .unwrap();
        assert!(matches!(
            midnight
                .fields()
                .find(|field| clean_field_key(field.name()) == "destroy light after")
                .and_then(|field| field.value()),
            Some(TagFieldData::Real(value)) if value == 7.5
        ));
    }

    #[test]
    fn halo3_reverb_values_reparent_into_reach_settings() {
        let definitions = locate_definitions_root();
        let mut source =
            TagFile::new(definitions.join("halo3_mcc/sound_environment.json")).unwrap();
        let ordinal = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "room intensity")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        source
            .root_mut()
            .field_at_mut(ordinal)
            .unwrap()
            .set(TagFieldData::Real(-4.25))
            .unwrap();

        let draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        let reverb = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "reverb settings")
            .and_then(|field| field.as_struct())
            .unwrap();
        assert!(reverb.fields().any(|field| {
            clean_field_key(field.name()) == "room intensity"
                && matches!(field.value(), Some(TagFieldData::Real(value)) if value == -4.25)
        }));
    }

    #[test]
    fn model_and_biped_use_native_target_layout_templates() {
        let definitions = locate_definitions_root();
        for group in ["model", "biped"] {
            let tags_root = std::env::temp_dir().join(format!(
                "baboon_{group}_template_{}_{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(&tags_root).unwrap();

            let mut template = TagFile::new(
                definitions
                    .join("haloreach_mcc")
                    .join(format!("{group}.json")),
            )
            .unwrap();
            apply_editing_kit_mcc_header(&mut template, "haloreach_mcc").unwrap();
            template.header.version = 42;
            template
                .write_atomic(tags_root.join(format!("template.{group}")))
                .unwrap();

            let source =
                TagFile::new(definitions.join("halo3_mcc").join(format!("{group}.json"))).unwrap();
            let draft = analyze_conversion(
                &source,
                "halo3_mcc",
                "haloreach_mcc",
                &definitions,
                Some(&tags_root),
            )
            .unwrap();
            assert!(draft.native_layout_template.is_some());
            assert!(draft.report.issues.iter().any(|issue| {
                issue.path == "target layout"
                    && issue.message.contains(&format!("native {group} layout"))
            }));
            let output = tags_root.join(format!("converted.{group}"));
            draft.tag.write_atomic(&output).unwrap();
            assert_eq!(
                TagFile::read(output).unwrap().group().tag,
                source.group().tag
            );
            let _ = fs::remove_dir_all(tags_root);
        }
    }

    #[test]
    fn editor_order_annotations_do_not_alias_unrelated_fields() {
        assert!(field_names_match(
            "animations*|ABCDCC",
            "animations*|ABCDCC"
        ));
        assert!(!field_names_match(
            "animations*|ABCDCC",
            "sound references|ABCDCC!*#Legacy field"
        ));
    }

    #[test]
    fn reach_animation_entries_default_unmapped_shared_reference_to_none() {
        let definitions = locate_definitions_root();
        let mut source =
            TagFile::new(definitions.join("halo3_mcc/model_animation_graph.json")).unwrap();
        let definitions_ordinal = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "definitions")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        let mut root = source.root_mut();
        let mut definitions_field = root.field_at_mut(definitions_ordinal).unwrap();
        let mut definitions_struct = definitions_field.as_struct_mut().unwrap();
        let animations_ordinal = definitions_struct
            .as_ref()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()).starts_with("animations"))
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        let mut animations_field = definitions_struct.field_at_mut(animations_ordinal).unwrap();
        let mut animations = animations_field.as_block_mut().unwrap();
        let animation_index = animations.add_element();
        let mut animation = animations.element_mut(animation_index).unwrap();
        let node_count_ordinal = animation
            .as_ref()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()).starts_with("node count"))
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        animation
            .field_at_mut(node_count_ordinal)
            .unwrap()
            .set(TagFieldData::CharInteger(7))
            .unwrap();
        drop(animation);
        drop(animations);
        drop(animations_field);
        drop(definitions_struct);
        drop(definitions_field);
        drop(root);
        assert_eq!(
            struct_at_path(source.root(), "definitions")
                .unwrap()
                .fields()
                .find(|field| clean_field_key(field.name()).starts_with("animations"))
                .and_then(|field| field.as_block())
                .map(|block| block.len()),
            Some(1)
        );

        let draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        let target_definitions = struct_at_path(draft.tag.root(), "definitions").unwrap();
        let target_animations = target_definitions
            .fields()
            .find(|field| clean_field_key(field.name()).starts_with("animations"))
            .and_then(|field| field.as_block())
            .unwrap();
        assert_eq!(
            target_animations.len(),
            1,
            "animation block was not transferred"
        );
        let animation = target_animations.element(0).unwrap();
        let shared_reference = animation
            .fields()
            .find(|field| clean_field_key(field.name()).starts_with("shared animation reference"))
            .and_then(|field| field.as_struct())
            .unwrap();
        assert!(matches!(
            shared_reference
                .fields()
                .find(|field| clean_field_key(field.name()).starts_with("graph reference"))
                .and_then(|field| field.value()),
            Some(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: None
            }))
        ));
        assert!(matches!(
            shared_reference
                .fields()
                .find(|field| clean_field_key(field.name()).starts_with("shared animation index"))
                .and_then(|field| field.value()),
            Some(TagFieldData::ShortBlockIndex(-1))
        ));
        let shared_data = animation
            .fields()
            .find(|field| clean_field_key(field.name()).starts_with("shared animation data"))
            .and_then(|field| field.as_block())
            .unwrap();
        assert_eq!(shared_data.len(), 1);
        assert!(matches!(
            shared_data
                .element(0)
                .and_then(|payload| {
                    payload
                        .fields()
                        .find(|field| clean_field_key(field.name()).starts_with("node count"))
                })
                .and_then(|field| field.value()),
            Some(TagFieldData::CharInteger(7))
        ));
        assert_eq!(
            target_definitions
                .fields()
                .find(|field| clean_field_key(field.name()).starts_with("sound references"))
                .and_then(|field| field.as_block())
                .map(|block| block.len()),
            Some(0),
            "animation payload must not be routed through editor annotation aliases"
        );
    }

    #[test]
    fn particle_template_is_cleared_before_conversion() {
        let definitions = locate_definitions_root();
        let unique = format!(
            "baboon_particle_template_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let tags_root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&tags_root).unwrap();

        let mut template = TagFile::new(definitions.join("haloreach_mcc/particle.json")).unwrap();
        apply_editing_kit_mcc_header(&mut template, "haloreach_mcc").unwrap();
        template.header.version = 42;
        let low_res = template
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "low res switch distance")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        template
            .root_mut()
            .field_at_mut(low_res)
            .unwrap()
            .set(TagFieldData::Real(123.0))
            .unwrap();
        template
            .add_import_info(definitions.join("haloreach_mcc/tag_import_information.json"))
            .unwrap();
        template
            .add_asset_depot_storage(definitions.join("haloreach_mcc/asset_depot_storage.json"))
            .unwrap();
        template
            .write_atomic(tags_root.join("template.particle"))
            .unwrap();

        let mut source = TagFile::new(definitions.join("halo3_mcc/particle.json")).unwrap();
        let main_flags = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "main flags")
            .and_then(|(ordinal, field)| match field.options() {
                Some(TagOptions::Flags(options)) => options
                    .iter()
                    .find(|option| option.name == "dies in media")
                    .map(|option| (ordinal, option.bit, option.name.to_owned())),
                _ => None,
            })
            .unwrap();
        source
            .root_mut()
            .field_at_mut(main_flags.0)
            .unwrap()
            .set(TagFieldData::LongFlags {
                value: (1u32 << main_flags.1) as i32,
                names: vec![(main_flags.1, main_flags.2)],
            })
            .unwrap();
        let billboard = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "particle billboard style")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        source
            .root_mut()
            .field_at_mut(billboard)
            .unwrap()
            .set(TagFieldData::ShortEnum {
                value: 6,
                name: Some("local vertical".to_owned()),
            })
            .unwrap();
        let attachments = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "attachments")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        let mut root = source.root_mut();
        let mut attachments_field = root.field_at_mut(attachments).unwrap();
        let mut attachments_block = attachments_field.as_block_mut().unwrap();
        let attachment_index = attachments_block.add_element();
        let mut attachment = attachments_block.element_mut(attachment_index).unwrap();
        let type_ordinal = attachment
            .as_ref()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "type")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        attachment
            .field_at_mut(type_ordinal)
            .unwrap()
            .set(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((
                    u32::from_be_bytes(*b"effe"),
                    "effects\\particles\\spark_attachment".to_owned(),
                )),
            }))
            .unwrap();
        drop(attachment);
        drop(attachments_block);
        drop(attachments_field);
        drop(root);
        let draft = analyze_conversion(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(&tags_root),
        )
        .unwrap();
        assert!(draft.report.issues.iter().any(|issue| {
            issue.path == "target layout" && issue.message.contains("native particle layout")
        }));
        assert!(draft.report.mapped_aliases > 0);
        assert!(draft.tag.root().fields().any(|field| {
            clean_field_key(field.name()) == "main flags"
                && matches!(field.value(), Some(TagFieldData::LongFlags { names, .. }) if names.iter().any(|(_, name)| name == "dies in water"))
        }));
        assert!(draft.tag.root().fields().any(|field| {
            clean_field_key(field.name()) == "particle billboard style"
                && matches!(field.value(), Some(TagFieldData::ShortEnum { name: Some(name), .. }) if name == "local vertical")
        }));
        let target_attachments = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "attachments")
            .and_then(|field| field.as_block())
            .unwrap();
        assert_eq!(target_attachments.len(), 1);
        assert!(target_attachments.element(0).unwrap().fields().any(|field| {
            clean_field_key(field.name()) == "type"
                && matches!(field.value(), Some(TagFieldData::TagReference(reference)) if reference.group_tag_and_name == Some((u32::from_be_bytes(*b"effe"), "effects\\particles\\spark_attachment".to_owned())))
        }));
        assert!(draft.tag.root().fields().any(|field| {
            clean_field_key(field.name()) == "low res switch distance"
                && matches!(field.value(), Some(TagFieldData::Real(value)) if value == 0.0)
        }));
        assert!(draft.tag.import_info().is_none());
        assert!(draft.tag.asset_depot_storage().is_none());

        fs::remove_dir_all(tags_root).unwrap();
    }

    #[test]
    fn particle_downport_rejects_unmatched_material_reference() {
        let definitions = locate_definitions_root();
        let unique = format!(
            "baboon_particle_downport_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let tags_root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&tags_root).unwrap();
        let mut template = TagFile::new(definitions.join("halo3_mcc/particle.json")).unwrap();
        apply_editing_kit_mcc_header(&mut template, "halo3_mcc").unwrap();
        template.header.version = 42;
        template
            .write_atomic(tags_root.join("template.particle"))
            .unwrap();

        let mut source = TagFile::new(definitions.join("halo4_mcc/particle.json")).unwrap();
        let material_ordinal = source
            .root()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "actual material?")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        let mut root = source.root_mut();
        let mut material_field = root.field_at_mut(material_ordinal).unwrap();
        let mut material = material_field.as_struct_mut().unwrap();
        let shader_ordinal = material
            .as_ref()
            .fields()
            .enumerate()
            .find(|(_, field)| clean_field_key(field.name()) == "material shader")
            .map(|(ordinal, _)| ordinal)
            .unwrap();
        material
            .field_at_mut(shader_ordinal)
            .unwrap()
            .set(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((
                    u32::from_be_bytes(*b"mats"),
                    "materials\\particles\\energy".to_owned(),
                )),
            }))
            .unwrap();
        drop(material);
        drop(material_field);
        drop(root);

        let draft = analyze_conversion(
            &source,
            "halo4_mcc",
            "halo3_mcc",
            &definitions,
            Some(&tags_root),
        )
        .expect("a down-port reports what it cannot carry rather than refusing");

        // Halo 3 has no `material` group at all, so no implementation could
        // preserve this reference. What matters is that it is named precisely,
        // so the author can rebuild that part with the older render-method
        // system — which is exactly what `docs/tag-conversion-mappings.md`
        // promises a down-port report will do.
        assert!(draft.report.dropped_references > 0);
        assert!(
            draft
                .report
                .issues
                .iter()
                .any(|issue| issue.message.contains("materials\\particles\\energy")),
            "the report has to name the material that needs recreating: {:?}",
            draft.report.issues.iter().map(|i| &i.message).collect::<Vec<_>>(),
        );

        // The other half of that promise: nothing was forced into an unrelated
        // older field to make the reference appear to survive.
        let mut carried = Vec::new();
        collect_reference_values(draft.tag.root(), "", &mut carried);
        assert!(
            !carried
                .iter()
                .any(|value| value.tag_path.contains("particles\\energy")),
            "the material must not be re-pointed at some unrelated Halo 3 field",
        );

        fs::remove_dir_all(tags_root).unwrap();
    }

    #[test]
    fn target_default_count_excludes_layout_and_runtime_storage() {
        assert!(!is_reportable_target_default(TagFieldType::Custom));
        assert!(!is_reportable_target_default(TagFieldType::Pad));
        assert!(!is_reportable_target_default(
            TagFieldType::PageableResource
        ));
        assert!(is_reportable_target_default(TagFieldType::Real));
        assert!(is_reportable_target_default(TagFieldType::Block));
    }

    #[test]
    fn default_values_are_not_reported_as_meaningful() {
        assert!(!value_is_meaningful(TagFieldData::Real(0.0)));
        assert!(!value_is_meaningful(TagFieldData::TagReference(
            TagReferenceData {
                group_tag_and_name: None,
            }
        )));
        assert!(!value_is_meaningful(TagFieldData::Data(Vec::new())));
    }

    #[test]
    fn reference_fidelity_rejects_missing_non_empty_reference() {
        let definitions = locate_definitions_root();
        let source_groups = GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
        let target_groups = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let mut source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        seed_weapon_fields(&mut source);
        let target = TagFile::new(definitions.join("haloreach_mcc/weapon.json")).unwrap();
        let catalog = ConversionMappingCatalog::load().unwrap();
        let mut report = TagConversionReport::default();
        let error = validate_reference_fidelity(
            &source,
            &target,
            &source_groups,
            &target_groups,
            "weapon",
            "halo3_mcc",
            "haloreach_mcc",
            &catalog,
            &mut report,
        )
        .unwrap_err();
        assert!(error.contains("objects\\test\\icon"));
    }

    fn set_struct_reference(
        structure: &mut TagStructMut<'_>,
        key: &str,
        group_tag: u32,
        path: &str,
    ) {
        let ordinal = field_ordinal_by_key(structure.as_ref(), key).unwrap();
        structure
            .field_at_mut(ordinal)
            .unwrap()
            .set(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((group_tag, path.to_owned())),
            }))
            .unwrap();
    }

    #[test]
    fn halo3_melee_hit_references_map_to_unique_reach_block_entries() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        let melee_ordinal = field_ordinal_by_key(source.root(), "melee damage parameters").unwrap();
        let mut root = source.root_mut();
        let mut melee_field = root.field_at_mut(melee_ordinal).unwrap();
        let mut melee = melee_field.as_struct_mut().unwrap();
        let damage_group = u32::from_be_bytes(*b"jpt!");
        for (prefix, damage) in [
            ("1st hit", "objects\\weapons\\damage_effects\\strike_melee"),
            ("2nd hit", "objects\\weapons\\damage_effects\\strike_melee"),
            ("3rd hit", "objects\\weapons\\damage_effects\\smash_melee"),
        ] {
            set_struct_reference(
                &mut melee,
                &format!("{prefix} melee damage"),
                damage_group,
                damage,
            );
            set_struct_reference(
                &mut melee,
                &format!("{prefix} melee response"),
                damage_group,
                "globals\\trigger_melee",
            );
        }
        drop(melee);
        drop(melee_field);
        drop(root);

        let draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        let melee_block = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "melee damage parameters")
            .and_then(|field| field.as_block())
            .unwrap();
        assert_eq!(melee_block.len(), 2);
        let mut references = Vec::new();
        collect_reference_values(draft.tag.root(), "", &mut references);
        for path in [
            "objects\\weapons\\damage_effects\\strike_melee",
            "globals\\trigger_melee",
            "objects\\weapons\\damage_effects\\smash_melee",
        ] {
            assert!(
                references
                    .iter()
                    .any(|reference| reference.tag_path == path)
            );
        }
    }

    #[test]
    fn halo3_effect_looping_sound_maps_into_reach_block() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/effect.json")).unwrap();
        let mut root = source.root_mut();
        set_struct_reference(
            &mut root,
            "looping sound",
            u32::from_be_bytes(*b"lsnd"),
            "sound\\visual_fx\\fire_large\\fire_large",
        );
        for (key, value) in [("location", 3), ("bind scale to event", 2)] {
            let ordinal = field_ordinal_by_key(root.as_ref(), key).unwrap();
            root.field_at_mut(ordinal)
                .unwrap()
                .set(TagFieldData::CharBlockIndex(value))
                .unwrap();
        }
        drop(root);

        let draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        let looping = field_by_key(draft.tag.root(), "looping sounds")
            .and_then(|field| field.as_block())
            .unwrap();
        assert_eq!(looping.len(), 1);
        let element = looping.element(0).unwrap();
        assert!(matches!(
            field_by_key(element, "looping sound").and_then(|field| field.value()),
            Some(TagFieldData::TagReference(TagReferenceData {
                group_tag_and_name: Some((group, ref path)),
            })) if group == u32::from_be_bytes(*b"lsnd")
                && path == "sound\\visual_fx\\fire_large\\fire_large"
        ));
        assert!(matches!(
            field_by_key(element, "location").and_then(|field| field.value()),
            Some(TagFieldData::ShortBlockIndex(3))
        ));
        assert!(matches!(
            field_by_key(element, "bind scale to event").and_then(|field| field.value()),
            Some(TagFieldData::ShortBlockIndex(2))
        ));
    }

    #[test]
    fn halo3_lens_flare_occlusion_enum_maps_to_reach_scale() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/lens_flare.json")).unwrap();
        let mut root = source.root_mut();
        let ordinal = field_ordinal_by_key(root.as_ref(), "occlusion inner radius scale").unwrap();
        root.field_at_mut(ordinal)
            .unwrap()
            .set(TagFieldData::ShortEnum {
                value: 3,
                name: Some("1/8".to_owned()),
            })
            .unwrap();
        drop(root);

        let draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        assert!(matches!(
            field_by_key(draft.tag.root(), "occlusion inner radius scale")
                .and_then(|field| field.value()),
            Some(TagFieldData::Real(value)) if value == 0.125
        ));
    }

    #[test]
    fn runtime_sensitive_groups_fail_instead_of_dropping_authored_fields() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/light.json")).unwrap();
        let mut root = source.root_mut();
        let ordinal = field_ordinal_by_key(root.as_ref(), "percent spherical").unwrap();
        root.field_at_mut(ordinal)
            .unwrap()
            .set(TagFieldData::Real(0.75))
            .unwrap();
        drop(root);

        let error = analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None)
            .err()
            .unwrap();
        assert!(error.contains("light conversion would lose 1 meaningful"));
        assert!(error.contains("percent spherical"));
    }

    #[test]
    fn halo3_player_response_generates_reach_companion_tags() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/damage_effect.json")).unwrap();
        let responses_ordinal = field_ordinal_by_key(source.root(), "player responses").unwrap();
        let mut root = source.root_mut();
        let mut responses_field = root.field_at_mut(responses_ordinal).unwrap();
        let mut responses = responses_field.as_block_mut().unwrap();
        let response_index = responses.add_element();
        let response = responses.element_mut(response_index).unwrap();
        initialize_block_index_defaults(response);
        let mut response = responses.element_mut(response_index).unwrap();
        let response_type = field_ordinal_by_key(response.as_ref(), "response type").unwrap();
        response
            .field_at_mut(response_type)
            .unwrap()
            .set(TagFieldData::ShortEnum {
                value: 1,
                name: Some("unshielded".to_owned()),
            })
            .unwrap();
        let rumble_ordinal = field_ordinal_by_key(response.as_ref(), "rumble").unwrap();
        let mut rumble_field = response.field_at_mut(rumble_ordinal).unwrap();
        let mut rumble = rumble_field.as_struct_mut().unwrap();
        let low_ordinal = field_ordinal_by_key(rumble.as_ref(), "low frequency rumble").unwrap();
        let mut low_field = rumble.field_at_mut(low_ordinal).unwrap();
        let mut low = low_field.as_struct_mut().unwrap();
        let duration = field_ordinal_by_key(low.as_ref(), "duration").unwrap();
        low.field_at_mut(duration)
            .unwrap()
            .set(TagFieldData::Real(0.4))
            .unwrap();
        drop(low);
        drop(low_field);
        drop(rumble);
        drop(rumble_field);
        drop(response);
        drop(responses);
        drop(responses_field);
        drop(root);

        let mut draft =
            analyze_conversion(&source, "halo3_mcc", "haloreach_mcc", &definitions, None).unwrap();
        assert_eq!(draft.companion_tags.len(), 2);
        assert!(
            draft
                .companion_tags
                .iter()
                .any(|companion| companion.group_name == "damage_response_definition")
        );
        assert!(
            draft
                .companion_tags
                .iter()
                .any(|companion| companion.group_name == "rumble")
        );

        let tags_root = std::env::temp_dir().join(format!(
            "baboon_response_companions_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(tags_root.join("objects/test")).unwrap();
        let output = tags_root.join("objects/test/impact.damage_effect");
        let companion_outputs = prepare_companion_outputs(
            &mut draft,
            &output,
            &tags_root,
            &definitions.join("haloreach_mcc/tag_dependency_list.json"),
        )
        .unwrap();
        assert_eq!(companion_outputs.len(), 2);
        assert!(companion_outputs.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name == "impact__damage_response.damage_response_definition")
        }));
        let (_, response_path) = reference_by_key(draft.tag.root(), "damage response").unwrap();
        assert_eq!(response_path, "objects\\test\\impact__damage_response");
        fs::remove_dir_all(tags_root).unwrap();
    }

    #[test]
    fn native_reach_contrail_fixed_arrays_open_without_panicking() {
        let path = Path::new(
            "D:/SteamLibrary/steamapps/common/HREK/tags/cinematics/020lb_halsey/fx/010/mac_projectile.contrail_system",
        );
        if !path.is_file() {
            return;
        }
        let result = std::panic::catch_unwind(|| TagFile::read(path));
        assert!(result.is_ok(), "native Reach contrail read panicked");
        assert!(
            result.unwrap().is_ok(),
            "native Reach contrail is unreadable"
        );
    }

    #[test]
    fn h3_contrail_uses_native_reach_layout_when_kits_are_available() {
        let source_path = Path::new(
            "D:/SteamLibrary/steamapps/common/H3EK/tags/fx/cinematics/010la_jungle_intro/01/hatch.contrail_system",
        );
        let target_root = Path::new("D:/SteamLibrary/steamapps/common/HREK/tags");
        if !source_path.is_file() || !target_root.is_dir() {
            return;
        }
        let definitions = locate_definitions_root();
        let source = TagFile::read(source_path).unwrap();
        let draft = analyze_conversion(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(target_root),
        )
        .unwrap();
        assert!(draft.native_layout_template.is_some());
        let bytes = draft.tag.write_to_bytes().unwrap();
        TagFile::read_from_bytes(&bytes).unwrap();
    }

    /// Campaign Evolved pairs only with Halo Reach, and says so rather than
    /// quietly offering a conversion nobody reviewed.
    #[test]
    fn campaign_evolved_converts_only_with_reach() {
        assert!(conversion_pair_supported("haloreach_mcc", CAMPAIGN_EVOLVED_GAME));
        assert!(conversion_pair_supported(CAMPAIGN_EVOLVED_GAME, "haloreach_mcc"));
        for other in ["halo3_mcc", "halo3odst_mcc", "halo4_mcc", "halo2amp_mcc"] {
            assert!(
                !conversion_pair_supported(other, CAMPAIGN_EVOLVED_GAME),
                "{other} must not convert straight into Campaign Evolved",
            );
            assert!(!conversion_pair_supported(CAMPAIGN_EVOLVED_GAME, other));
        }
        assert!(!conversion_pair_supported(CAMPAIGN_EVOLVED_GAME, CAMPAIGN_EVOLVED_GAME));

        // The refusal has to say what to do instead, or it is a dead end.
        let message = unsupported_pair_message("halo3_mcc", CAMPAIGN_EVOLVED_GAME);
        assert!(message.contains("haloreach_mcc"), "{message}");

        // The five MCC profiles are untouched.
        assert!(conversion_pair_supported("halo3_mcc", "haloreach_mcc"));
        assert!(conversion_pair_supported("halo4_mcc", "halo2amp_mcc"));
    }

    /// A Halo Reach animation graph converts into Campaign Evolved.
    ///
    /// This is the whole point of the exercise, and it used to fail twice over:
    /// Campaign Evolved was not a conversion target at all, and the safety
    /// check refused any tag carrying a pageable resource — which every
    /// animation graph does, because its payload *is* one.
    ///
    /// Built from the schemas rather than from a kit, so it runs on CI. That
    /// means null resources; the engine's `resource_copy_tests` cover a
    /// populated one crossing byte-for-byte, and the fixture test below covers
    /// a real file when HREK is installed.
    #[test]
    fn a_reach_animation_graph_converts_into_campaign_evolved() {
        let definitions = locate_definitions_root();
        let source = TagFile::new(
            definitions.join("haloreach_mcc/model_animation_graph.json"),
        )
        .expect("build a Reach animation graph from its schema");

        let draft = analyze_conversion(
            &source,
            "haloreach_mcc",
            CAMPAIGN_EVOLVED_GAME,
            &definitions,
            None,
        )
        .expect("a Reach animation graph must convert into Campaign Evolved");

        // The target's layout won, not the source's — that is the safety
        // argument, and the four structs that changed size are where it shows.
        let root = draft.tag.root();
        let pool = root
            .field_path("definitions/animations")
            .and_then(|field| field.as_block())
            .expect("the converted graph declares an animation pool")
            .definition()
            .struct_definition();
        let shared = pool
            .fields()
            .find(|field| field.name() == "shared animation data")
            .and_then(|field| field.as_block())
            .expect("each pool entry holds shared animation data")
            .struct_definition();
        assert_eq!(shared.name(), "shared_model_animation_block");
        assert_eq!(
            shared.size(),
            200,
            "Campaign Evolved's 200-byte element, not Reach's 212-byte one",
        );

        // The stamp belongs to the destination.
        assert_eq!(
            (
                draft.tag.header.build_version,
                draft.tag.header.build_number,
                draft.tag.header.version,
            ),
            CAMPAIGN_EVOLVED_GENERATION,
        );

        // And it round-trips.
        let bytes = draft.tag.write_to_bytes().expect("serialize the converted graph");
        TagFile::read_from_bytes(&bytes).expect("read the converted graph back");
    }

    /// The reviewed drops are what let the graph through, so an unreviewed loss
    /// must still stop it. Removing one from the catalog has to bring the
    /// refusal back — otherwise the fail-closed rule has quietly stopped
    /// guarding anything.
    #[test]
    fn an_uncatalogued_animation_graph_loss_still_refuses() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        // The paths the converter actually reports, indices and all.
        for field in [
            "definitions/skeleton nodes[0]/node joint flags",
            "definitions/skeleton nodes[7]/additional flags",
            "definitions/animations[0]/shared animation data[0]/facial wrinkle events[0]/region",
        ] {
            assert!(
                catalog
                    .accepted_drop_reason(
                        "model_animation_graph",
                        "haloreach_mcc",
                        CAMPAIGN_EVOLVED_GAME,
                        field,
                    )
                    .is_some(),
                "{field} must be a reviewed drop, or every Reach graph refuses",
            );
        }
        // A field nobody reviewed is not accepted just because it is in the
        // same group.
        assert!(
            catalog
                .accepted_drop_reason(
                    "model_animation_graph",
                    "haloreach_mcc",
                    CAMPAIGN_EVOLVED_GAME,
                    "definitions/some field nobody reviewed",
                )
                .is_none(),
        );
    }

    /// A rule that drops a container drops what is inside it.
    ///
    /// Campaign Evolved has no `facial wrinkle events` block, so it has none of
    /// the fields within one either — but the converter reports those children
    /// individually, one per element. 85 of HREK's cinematic head graphs
    /// refused on `.../facial wrinkle events[0]/wrinkle name` while the rule
    /// named the block itself.
    #[test]
    fn a_dropped_container_covers_the_fields_inside_it() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        let reason = |path: &str| {
            catalog.accepted_drop_reason(
                "model_animation_graph",
                "haloreach_mcc",
                CAMPAIGN_EVOLVED_GAME,
                path,
            )
        };

        // The block itself, and the children the converter actually reports.
        for path in [
            "definitions/animations[0]/shared animation data[0]/facial wrinkle events",
            "definitions/animations[0]/shared animation data[0]/facial wrinkle events[0]/wrinkle name",
            "definitions/animations[12]/shared animation data[0]/facial wrinkle events[3]/region",
        ] {
            assert!(reason(path).is_some(), "`{path}` should be covered");
        }

        // Ancestry, not string prefix: a differently-named sibling is not
        // covered just because it starts the same way.
        assert!(reason("definitions/animations[0]/shared animation data[0]/facial wrinkle eventsX").is_none());
        // And an unrelated field in the same struct is still a real loss.
        assert!(reason("definitions/animations[0]/shared animation data[0]/some other field").is_none());
    }

    /// A signedness rename carries the bits, not the arithmetic.
    ///
    /// `-1` is the format's "no index" sentinel and appears everywhere. Halo
    /// Reach declares an animation graph's IK `chain index` signed and Campaign
    /// Evolved declares it unsigned; both store 0xFF. Range-checking the
    /// mathematical value instead reported 2,124 losses on one character graph,
    /// none of them real.
    #[test]
    fn a_signedness_rename_carries_the_bits() {
        use TagFieldType as T;
        // One byte, signed to unsigned and back.
        assert_eq!(reinterpret_same_width_integer(T::CharInteger, T::ByteInteger, -1), 255);
        assert_eq!(reinterpret_same_width_integer(T::ByteInteger, T::CharInteger, 255), -1);
        assert_eq!(reinterpret_same_width_integer(T::CharInteger, T::ByteInteger, 7), 7);
        // Wider pairs behave the same way.
        assert_eq!(reinterpret_same_width_integer(T::ShortInteger, T::WordInteger, -1), 65535);
        assert_eq!(reinterpret_same_width_integer(T::LongInteger, T::DwordInteger, -1), 4294967295);
        assert_eq!(reinterpret_same_width_integer(T::DwordInteger, T::LongInteger, 4294967295), -1);

        // Same signedness is left alone.
        assert_eq!(reinterpret_same_width_integer(T::CharInteger, T::CharInteger, -1), -1);
        // And so is a real width change, where the range check is the point.
        assert_eq!(reinterpret_same_width_integer(T::LongInteger, T::ByteInteger, -1), -1);
        assert_eq!(reinterpret_same_width_integer(T::CharInteger, T::LongInteger, -1), -1);
    }

    /// What Halo 2 actually stores in a version-0 `mapping_function`.
    ///
    /// H2's `mapping_function` is versioned: v0 spells a curve out as explicit
    /// fields (`Function Type`, `Flags`, four colours, a `Values` block of reals)
    /// while v1 holds the serialized blob. Halo 3 has only the blob, so a v0 curve
    /// has to be *synthesized* rather than copied — and whether that is safe
    /// depends on whether the flat `Values` list can be turned into the right
    /// per-type compact structure. Measures the distribution before anything is
    /// written, because a plausible-but-wrong curve silently changes how a
    /// particle looks and is worse than an unset one.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_halo2_v0_mapping_function_shapes() {
        let Some(h2) = kit_tags("BLAM_TEST_H2EK", "H2EK") else {
            eprintln!("skipping: no H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "halo2_mcc").unwrap();
        let by_ext = super::chain_sweep::extension_to_group_tag(&index);
        // `(function type, value count)` -> how many mappings look like that.
        let mut shapes: HashMap<(i128, usize), usize> = HashMap::new();
        let mut blobs = 0usize;
        let mut explicit = 0usize;
        for group in ["particle", "effect", "contrail", "light_volume", "decal"] {
            let Some(&group_tag) = by_ext.get(group) else {
                continue;
            };
            for path in tags_with_extension(&h2, group).iter().take(300) {
                let Ok(tag) = read_tag_for_conversion(
                    path,
                    Some("halo2_mcc"),
                    Some(definitions.as_path()),
                    group_tag,
                ) else {
                    continue;
                };
                walk_v0_mappings(tag.root(), &mut shapes, &mut blobs, &mut explicit);
            }
        }
        eprintln!("blob-form mappings: {blobs}; explicit v0 mappings: {explicit}");
        let mut shapes: Vec<_> = shapes.into_iter().collect();
        shapes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        for ((function_type, values), count) in shapes.iter().take(20) {
            let name = match function_type {
                0 => "Identity",
                1 => "Constant",
                2 => "Transition",
                3 => "Periodic",
                4 => "Linear",
                5 => "LinearKey",
                6 => "MultiLinearKey",
                7 => "Spline",
                8 => "MultiSpline",
                9 => "Exponent",
                10 => "Spline2",
                _ => "?",
            };
            eprintln!("   type {function_type} ({name:14}) with {values:2} values: x{count}");
        }
    }

    fn walk_v0_mappings(
        value: TagStruct<'_>,
        shapes: &mut HashMap<(i128, usize), usize>,
        blobs: &mut usize,
        explicit: &mut usize,
    ) {
        // A v0 mapping is recognisable by having `function type` as a field at all;
        // the blob form has only `data`.
        // Require *both* halves of the v0 shape. Matching on `function type` alone
        // picked up unrelated fields of that name elsewhere in the tag and
        // reported impossible types like 40 and 24.
        if let Some(function_type) = field_by_key(value, "function type")
            .and_then(|field| field.value())
            .and_then(integer_value)
            && let Some(values) = field_by_key(value, "values").and_then(|field| field.as_block())
        {
            *explicit += 1;
            *shapes.entry((function_type, values.len())).or_default() += 1;
        }
        for field in value.fields() {
            if field.is_function_data() {
                *blobs += 1;
            }
            if clean_field_key(field.name()) == "data"
                && let Some(block) = field.as_block()
                && block.definition().struct_definition().size() == 1
            {
                *blobs += 1;
                continue;
            }
            if let Some(child) = field.as_struct() {
                walk_v0_mappings(child, shapes, blobs, explicit);
            }
            if let Some(block) = field.as_block() {
                for element in block.iter() {
                    walk_v0_mappings(element, shapes, blobs, explicit);
                }
            }
            if let Some(array) = field.as_array() {
                for element in array.iter() {
                    walk_v0_mappings(element, shapes, blobs, explicit);
                }
            }
        }
    }

    /// Does a kit tag's embedded layout agree with the dumped schema about a
    /// struct's GUID and name?
    ///
    /// `SchemaFieldAliases` is keyed by `schema_struct_key(guid, name)`, built from
    /// the JSON, but looked up with the GUID and name off the *runtime* struct. If a
    /// kit's own layout disagrees, every `{former name}` alias in that struct is
    /// unreachable — which would explain why Reach's
    /// `root offset max scale idle{root offset max scale}` never pairs with Halo 3's
    /// `root offset max scale` despite the table being provably correct.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_runtime_versus_schema_struct_identity() {
        let Some(reach) = kit_tags("BLAM_TEST_HREK", "HREK") else {
            eprintln!("skipping: no HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let Some(&group_tag) = super::chain_sweep::extension_to_group_tag(&index).get("biped")
        else {
            return;
        };
        let schema: Value = serde_json::from_slice(
            &fs::read(definitions.join("haloreach_mcc").join("biped.json")).unwrap(),
        )
        .unwrap();
        let declared = schema
            .get("structs")
            .and_then(Value::as_object)
            .map(|structs| {
                structs
                    .iter()
                    .filter_map(|(name, value)| {
                        Some((
                            name.to_ascii_lowercase(),
                            value.get("guid").and_then(Value::as_str)?.to_owned(),
                        ))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        for path in tags_with_extension(&reach, "biped").iter().take(3) {
            let Ok(tag) = read_tag_for_conversion(
                path,
                Some("haloreach_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            eprintln!("== {}", path.file_name().unwrap().to_string_lossy());
            let mut mismatches = 0usize;
            let mut checked = 0usize;
            compare_struct_identity(tag.root(), &declared, &mut checked, &mut mismatches);
            // What the kit's own layout calls the fields in the struct at issue.
            if let Some(fitting) = struct_at_path(tag.root(), "ground fitting data") {
                eprintln!(
                    "   runtime `ground fitting data` struct = {:?} guid {}",
                    fitting.definition().name(),
                    fitting
                        .definition()
                        .guid()
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                );
                for field in fitting.fields() {
                    if !field.name().is_empty() {
                        eprintln!("      field {:?}", field.name());
                    }
                }
            } else {
                eprintln!("   this biped has no `ground fitting data` struct at the root");
            }
            eprintln!("   {checked} structs checked, {mismatches} disagreed with the schema");
            break;
        }
    }

    fn compare_struct_identity(
        value: TagStruct<'_>,
        declared: &HashMap<String, String>,
        checked: &mut usize,
        mismatches: &mut usize,
    ) {
        let definition = value.definition();
        let name = definition.name().to_ascii_lowercase();
        let runtime = definition
            .guid()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if let Some(schema_guid) = declared.get(&name) {
            *checked += 1;
            if !schema_guid.eq_ignore_ascii_case(&runtime) {
                *mismatches += 1;
                eprintln!("   MISMATCH {name}: runtime {runtime} vs schema {schema_guid}");
            }
        } else if name.contains("ground_fitting") || name.contains("ground fitting") {
            eprintln!("   runtime struct {name:?} is not named in the schema at all");
        }
        for field in value.fields() {
            if let Some(child) = field.as_struct() {
                compare_struct_identity(child, declared, checked, mismatches);
            }
            if let Some(block) = field.as_block() {
                for element in block.iter().take(1) {
                    compare_struct_identity(element, declared, checked, mismatches);
                }
            }
            if let Some(array) = field.as_array() {
                for element in array.iter().take(1) {
                    compare_struct_identity(element, declared, checked, mismatches);
                }
            }
        }
    }

    /// Run a real Halo 3 biped into Reach with `BLAM_DEBUG_FIELD` set, so the
    /// matcher prints why `root offset max scale` finds no home.
    ///
    /// Every precondition for the `{former name}` alias was verified separately and
    /// all of them hold, so the remaining possibilities are about the matcher's
    /// context rather than the table: a candidate already consumed, a struct pair
    /// that is never visited, or a struct identity that differs from the one the
    /// alias table was keyed with.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_why_the_ground_fitting_alias_does_not_fire() {
        let (Some(h3), Some(reach)) = (
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
            kit_tags("BLAM_TEST_HREK", "HREK"),
        ) else {
            eprintln!("skipping: needs H3EK and HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
        let target_index = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let Some(&group_tag) =
            super::chain_sweep::extension_to_group_tag(&source_index).get("biped")
        else {
            return;
        };
        let templates = NativeTemplateIndex::build(&reach, &target_index);
        // The same picker the numeric sweep uses, so the two diagnostics are
        // talking about the same tag. Reading "a" biped instead of "the" biped is
        // exactly how a fix can look proven while the ranking disagrees.
        let sampled = super::chain_sweep::first_tag_by_extension(&h3);
        let candidates = sampled
            .get("biped")
            .cloned()
            .into_iter()
            .chain(tags_with_extension(&h3, "biped").into_iter().take(4))
            .collect::<Vec<_>>();
        for path in &candidates {
            let Ok(source) = read_tag_for_conversion(
                path,
                Some("halo3_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Some(fitting) = struct_at_path(source.root(), "ground fitting data") else {
                eprintln!("-- {} has no ground fitting data", path.display());
                continue;
            };
            eprintln!("== {}", path.display());
            eprintln!(
                "   source struct {:?} guid {}",
                fitting.definition().name(),
                fitting
                    .definition()
                    .guid()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            for field in fitting.fields() {
                if !field.name().is_empty() {
                    eprintln!("      source field {:?}", field.name());
                }
            }
            match analyze_conversion_with_templates(
                &source,
                "halo3_mcc",
                "haloreach_mcc",
                &definitions,
                Some(&templates),
            ) {
                Ok(draft) => {
                    for issue in &draft.report.issues {
                        if issue.path.contains("ground fitting") || issue.path.contains("root offset")
                        {
                            eprintln!("   issue at {:?}: {}", issue.path, issue.message);
                        }
                    }
                    if let Some(fitting) = struct_at_path(draft.tag.root(), "ground fitting data") {
                        for field in fitting.fields() {
                            if !field.name().is_empty() {
                                eprintln!("      target field {:?}", field.name());
                            }
                        }
                    } else {
                        eprintln!("   converted tag has no ground fitting data struct");
                    }
                }
                Err(error) => eprintln!("   refused: {error}"),
            }
            return;
        }
        eprintln!("no H3 biped with a ground fitting data struct was found");
    }

    /// A non-finite source number never reaches the target tag.
    ///
    /// A NaN in a written tag is exactly what makes the destination game's tools
    /// refuse to open it, so the converter refuses the value and keeps the
    /// target's default.
    ///
    /// **This used to be anchored to a real Halo 2 projectile** whose
    /// `material responses[0]/angular noise` read as NaN. It is not any more, and
    /// the reason is worth recording: that NaN was never in the tag. Halo 2's
    /// legacy-string engines store an `old_string_id` inline, so every field after
    /// a name was being read 28 bytes early, and the "NaN" was the middle of some
    /// neighbouring value. `adjust_layout_for_engine` fixed the read, and a scan
    /// of 445 Halo 2 tags across six groups now finds **no** non-finite number
    /// anywhere.
    ///
    /// So the sample is synthetic now, which is the better test regardless: it
    /// exercises the guard rather than the corpus, needs no kit, and cannot be
    /// invalidated by a kit-reading fix the way the last one was.
    #[test]
    fn a_nan_in_the_source_does_not_reach_the_converted_tag() {
        let definitions = locate_definitions_root();
        let mut source = TagFile::new(definitions.join("halo3_mcc/weapon.json")).unwrap();
        let leaf = first_direct_leaf(&source, is_real_scalar);
        source
            .root_mut()
            .field_at_mut(leaf.ordinal)
            .unwrap()
            .set(real_field_value(leaf.field_type, f32::NAN))
            .unwrap();
        // The premise, asserted rather than assumed: a NaN really is in there to
        // be caught.
        let mut before = Vec::new();
        collect_numbers(source.root(), &mut before);
        assert!(
            before.iter().any(|(_, value, _)| !value.is_finite()),
            "the synthetic source should carry a NaN"
        );

        let draft = analyze_conversion_with_templates(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            None,
        )
        .expect("halo3 -> reach weapon converts");
        let mut after = Vec::new();
        collect_numbers(draft.tag.root(), &mut after);
        let carried: Vec<&str> = after
            .iter()
            .filter(|(_, value, _)| !value.is_finite())
            .map(|(path, _, _)| path.as_str())
            .collect();
        assert!(
            carried.is_empty(),
            "non-finite numbers reached the converted tag at {carried:?}"
        );
        // Refused, not silently defaulted: a value dropped without a word is the
        // failure mode this whole check exists to avoid.
        assert!(
            draft
                .report
                .issues
                .iter()
                .any(|issue| issue.message.contains("not a finite number")),
            "the refusal should be reported"
        );
    }

    /// A converted Reach particle keeps a render-method definition even when the
    /// source cannot supply one.
    ///
    /// A Reach particle with an empty `rmdf` reference crashes the mod tools, and
    /// nothing in the Reach schema protects that field: the whole render method
    /// lives in a `tmpl` hole, so no fx schema declares `definition` and neither the
    /// `*` nor the `!` annotation applies. It survives because a kit-authored
    /// template supplies it and the reset now leaves a render method's own
    /// definition alone.
    ///
    /// The source's reference is deliberately cleared first. Halo 3 usually carries
    /// the same `shaders\particle` value, which would make this pass whether or not
    /// the fallback works — so the test removes it and checks the template still
    /// fills the gap.
    #[test]
    fn a_converted_reach_particle_keeps_a_render_method_definition() {
        let (Some(h3), Some(reach)) = (
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
            kit_tags("BLAM_TEST_HREK", "HREK"),
        ) else {
            eprintln!("skipping: needs H3EK and HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
        let target_index = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let Some(&group_tag) =
            super::chain_sweep::extension_to_group_tag(&source_index).get("particle")
        else {
            eprintln!("skipping: halo3_mcc declares no particle");
            return;
        };
        let templates = NativeTemplateIndex::build(&reach, &target_index);
        let found = super::chain_sweep::first_tag_by_extension(&h3);
        let Some(path) = found.get("particle") else {
            eprintln!("skipping: H3EK ships no .particle");
            return;
        };
        let Ok(mut source) = read_tag_for_conversion(
            path,
            Some("halo3_mcc"),
            Some(definitions.as_path()),
            group_tag,
        ) else {
            eprintln!("skipping: {} is unreadable", path.display());
            return;
        };

        // Clear the source's own render-method definition, so only the template
        // can account for a value in the output.
        let mut cleared = false;
        let located = struct_at_path(source.root(), "actual shader?").and_then(|shader| {
            let definition = shader.fields().position(|field| {
                field.field_type() == TagFieldType::TagReference
                    && clean_field_key(field.name()) == "definition"
            })?;
            let outer = field_ordinal_by_key(source.root(), "actual shader?")?;
            is_render_method_struct(shader).then_some((outer, definition))
        });
        if let Some((outer, definition)) = located
            && let Some(mut outer_field) = source.root_mut().field_at_mut(outer)
            && let Some(mut shader) = outer_field.as_struct_mut()
            && let Some(mut field) = shader.field_at_mut(definition)
        {
            field
                .set(TagFieldData::TagReference(TagReferenceData {
                    group_tag_and_name: None,
                }))
                .unwrap();
            cleared = true;
        }
        assert!(
            cleared,
            "could not clear the source definition; the sample changed and this test              no longer proves the fallback"
        );

        let draft = analyze_conversion_with_templates(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(&templates),
        )
        .expect("halo3 -> reach particle converts");
        let shader = struct_at_path(draft.tag.root(), "actual shader?")
            .expect("the converted particle has an `actual shader?` struct");
        let definition = shader
            .fields()
            .find(|field| clean_field_key(field.name()) == "definition")
            .and_then(|field| match field.value() {
                Some(TagFieldData::TagReference(reference)) => reference.group_tag_and_name,
                _ => None,
            });
        let (group, name) = definition
            .expect("the converted particle kept a render-method definition reference");
        assert_eq!(format_group_tag(group), "rmdf");
        assert!(!name.is_empty(), "the render-method reference points at nothing");
    }

    /// What does a converted Reach particle actually hold where the render method
    /// should be?
    ///
    /// A shipped Reach particle expands the render method inline inside
    /// `actual shader?` with a `definition` reference to a render_method_definition
    /// tag, and a particle without one crashes the mod tools. Halo 3 cannot supply
    /// that value. Print the native template and the converted output side by side
    /// rather than trusting an earlier note about which fields survive.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_the_converted_reach_particle_render_method() {
        let (Some(h3), Some(reach)) = (
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
            kit_tags("BLAM_TEST_HREK", "HREK"),
        ) else {
            eprintln!("skipping: needs H3EK and HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "halo3_mcc").unwrap();
        let target_index = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let Some(&group_tag) =
            super::chain_sweep::extension_to_group_tag(&source_index).get("particle")
        else {
            return;
        };
        let templates = NativeTemplateIndex::build(&reach, &target_index);
        let dump = |label: &str, value: TagStruct<'_>| {
            eprintln!("-- {label}");
            for field in value.fields() {
                if field.name().is_empty() {
                    eprintln!("      (unnamed {:?})", field.field_type());
                    continue;
                }
                let extra = match field.value() {
                    Some(TagFieldData::TagReference(reference)) => match reference.group_tag_and_name {
                        Some((group, name)) => format!(" -> {} {name:?}", format_group_tag(group)),
                        None => " -> (empty)".to_owned(),
                    },
                    _ => String::new(),
                };
                eprintln!("      {:?} {:?}{extra}", field.name(), field.field_type());
            }
        };
        let native = super::chain_sweep::first_tag_by_extension(&reach);
        if let Some(path) = native.get("particle")
            && let Ok(tag) = TagFile::read(path)
        {
            eprintln!("== native HREK {}", path.display());
            if let Some(shader) = struct_at_path(tag.root(), "actual shader?") {
                dump("native actual shader?", shader);
            } else {
                eprintln!("   no `actual shader?` struct at the root");
                for field in tag.root().fields() {
                    if field.as_struct().is_some() {
                        eprintln!("      root struct field {:?}", field.name());
                    }
                }
            }
        }
        let found = super::chain_sweep::first_tag_by_extension(&h3);
        let Some(path) = found.get("particle") else { return };
        let Ok(source) = crate::convert::read_tag_for_conversion(
            path,
            Some("halo3_mcc"),
            Some(definitions.as_path()),
            group_tag,
        ) else {
            return;
        };
        eprintln!("== converted from {}", path.display());
        match analyze_conversion_with_templates(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(&templates),
        ) {
            Ok(draft) => {
                eprintln!("   template: {:?}", draft.native_layout_template);
                if let Some(shader) = struct_at_path(draft.tag.root(), "actual shader?") {
                    dump("converted actual shader?", shader);
                } else {
                    eprintln!("   converted tag has no `actual shader?` struct");
                }
            }
            Err(error) => eprintln!("   refused: {error}"),
        }
    }

    /// The schema's own `{former name}` marker resolves a rename.
    ///
    /// Reach declares `root offset max scale idle{root offset max scale}` in
    /// `biped_ground_fitting_data_struct`, and Halo 3 declares `root offset max
    /// scale` in the struct with the *same* GUID — so the schema states the rename
    /// outright and no reviewed rule should be needed. Hundreds of fields carry such
    /// a marker, so this pins the mechanism rather than the one field.
    #[test]
    fn a_schema_former_name_marker_resolves_a_renamed_field() {
        let definitions = locate_definitions_root();
        let reach = definitions.join("haloreach_mcc").join("biped.json");
        if !reach.is_file() {
            eprintln!("skipping: no haloreach_mcc biped schema");
            return;
        }
        let aliases = SchemaFieldAliases::load(&reach).expect("reach biped schema loads");
        let guid = parse_schema_guid("3849958ee7443781036a85bab008781b")
            .expect("the ground-fitting struct guid");
        assert!(
            aliases.matches(
                guid,
                "biped_ground_fitting_data_struct",
                "root offset max scale idle",
                "root offset max scale",
            ),
            "the `{{root offset max scale}}` former-name marker did not register"
        );
        // And the reverse lookup, which is the direction the matcher uses when the
        // *source* schema is the one carrying the marker.
        assert!(aliases.matches(
            guid,
            "biped_ground_fitting_data_struct",
            "root offset max scale",
            "root offset max scale idle",
        ));
    }

    /// A Halo 2 bitmap's pixels reach Halo 3, so the chain onward to Reach carries
    /// an image instead of empty metadata.
    ///
    /// The pixels are a `data` blob in both, under the same field name, but Halo 3
    /// renamed the data *definition* from `processed_pixel_data_data` to
    /// `bitmap_group_pixel_data_def` — so the derived "same payload kind" rule could
    /// not see it and a reviewed `payload_aliases` entry declares the rename.
    /// Asserts the bytes arrive whole and that the per-bitmap offsets indexing them
    /// come too, since a blob with no offsets is the version that crashed the tools.
    #[test]
    fn a_halo2_bitmaps_pixels_arrive_in_halo3() {
        let Some(h2) = kit_tags("BLAM_TEST_H2EK", "H2EK") else {
            eprintln!("skipping: needs H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "halo2_mcc").unwrap();
        let Some(&group_tag) = super::chain_sweep::extension_to_group_tag(&index).get("bitmap")
        else {
            return;
        };
        let mut checked = 0usize;
        for path in tags_with_extension(&h2, "bitmap").iter().take(80) {
            let Ok(source) = read_tag_for_conversion(
                path,
                Some("halo2_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Some(pixels) = field_by_key(source.root(), "processed pixel data")
                .and_then(|field| field.as_data())
                .map(<[u8]>::to_vec)
            else {
                continue;
            };
            let entries = field_by_key(source.root(), "bitmaps")
                .and_then(|field| field.as_block())
                .map(|block| block.len())
                .unwrap_or(0);
            if pixels.len() < 1024 || entries < 2 {
                continue;
            }
            // Generated layout on purpose: scanning H3EK for a native template
            // walks 97k files and costs two minutes, and the payload carry does not
            // depend on the template. The template path is exercised by the
            // adjacent-pair sweep.
            let draft = match analyze_conversion(
                &source,
                "halo2_mcc",
                "halo3_mcc",
                &definitions,
                None,
            ) {
                Ok(draft) => draft,
                Err(error) => panic!("{} refused: {error}", path.display()),
            };
            let landed = field_by_key(draft.tag.root(), "processed pixel data")
                .and_then(|field| field.as_data())
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            assert_eq!(
                landed,
                pixels,
                "{} lost or altered its pixels ({} bytes in, {} out)",
                path.display(),
                pixels.len(),
                landed.len()
            );
            let offsets: Vec<i128> = field_by_key(draft.tag.root(), "bitmaps")
                .and_then(|field| field.as_block())
                .map(|block| {
                    block
                        .iter()
                        .filter_map(|element| {
                            field_by_key(element, "pixels offset")?.value().and_then(integer_value)
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                offsets.iter().any(|offset| *offset != 0),
                "{} carried {entries} bitmaps and {} bytes of pixels, but every \
                 `pixels offset` is 0 — the blob has nothing indexing it, which is \
                 the shape that crashed the mod tools",
                path.display(),
                pixels.len()
            );
            checked += 1;
            break;
        }
        assert_eq!(checked, 1, "no multi-entry Halo 2 bitmap with pixel data was converted");
    }

    /// What a native Halo 3 bitmap actually keeps in its pageable resource, and how
    /// that compares with the Halo 2 blob that would have to become one.
    ///
    /// Carrying classic pixels into Halo 3 onward means *authoring* a
    /// `bitmap_texture_interop_resource`, and `copy_resource_from` can only copy an
    /// existing one. Whether that is a small job or a large one depends on what the
    /// resource holds: if the exploded payload is the pixel bytes behind a short
    /// header, it is tractable; if it is a paged structure with its own tables, it
    /// is not. Measures rather than guesses, because a wrong resource is exactly
    /// what crashed the Reach mod tools.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_native_bitmap_resource_shape() {
        let definitions = locate_definitions_root();
        for (kit, game) in [("H3EK", "halo3_mcc"), ("HREK", "haloreach_mcc")] {
            let Some(tags) = kit_tags(&format!("BLAM_TEST_{kit}"), kit) else {
                continue;
            };
            let index = GameTagIndex::load(&definitions, game).unwrap();
            let Some(&group_tag) = super::chain_sweep::extension_to_group_tag(&index).get("bitmap")
            else {
                continue;
            };
            let mut shown = 0usize;
            for path in tags_with_extension(&tags, "bitmap").iter().take(400) {
                let Ok(tag) = read_tag_for_conversion(
                    path,
                    Some(game),
                    Some(definitions.as_path()),
                    group_tag,
                ) else {
                    continue;
                };
                let mut lines = Vec::new();
                describe_resources(tag.root(), "", &mut lines);
                if lines.is_empty() {
                    continue;
                }
                eprintln!("== {game} {}", path.file_name().unwrap().to_string_lossy());
                for line in &lines {
                    eprintln!("   {line}");
                }
                shown += 1;
                if shown == 2 {
                    break;
                }
            }
            if shown == 0 {
                eprintln!("== {game}: no bitmap with a populated resource in the first 400");
            }
        }
    }

    fn describe_resources(value: TagStruct<'_>, prefix: &str, into: &mut Vec<String>) {
        for field in value.fields() {
            let key = clean_field_key(field.name());
            let path =
                if prefix.is_empty() { key.clone() } else { format!("{prefix}/{key}") };
            if field.field_type() == TagFieldType::PageableResource
                && let Some(resource) = field.as_resource()
            {
                let kind = format!("{:?}", resource.kind());
                let definition = resource.definition().struct_definition();
                into.push(format!(
                    "{path}: kind {kind}, struct {} ({} bytes declared)",
                    definition.name(),
                    definition.size(),
                ));
            }
            if field.is_function_data() {
                continue;
            }
            if field.field_type() == TagFieldType::Data
                && let Some(bytes) = field.as_data()
                && !bytes.is_empty()
            {
                into.push(format!("{path}: data blob {} bytes", bytes.len()));
            }
            if let Some(child) = field.as_struct() {
                describe_resources(child, &path, into);
            }
            if let Some(block) = field.as_block() {
                for (index, element) in block.iter().enumerate().take(2) {
                    describe_resources(element, &format!("{path}[{index}]"), into);
                }
            }
        }
    }

    /// A Reach `.shader` becomes a Halo 4 `.material`, carrying its parameters.
    ///
    /// Halo 4 still declares a `shader` group but ships none — 0 `.shader` against
    /// 7,140 `.material` in H4EK — so a shader-to-shader conversion produced a class
    /// the game never loads. Three things had to line up, and each failed silently
    /// on its own: the group alias has to outrank the same-name `shader` group, the
    /// reparent has to lift Reach's `render_method` body onto the flat `material`
    /// root, and the reparent must *not* apply in reverse.
    #[test]
    fn every_reach_shader_family_group_converts_into_a_halo4_material() {
        let (Some(reach), Some(h4)) = (
            kit_tags("BLAM_TEST_HREK", "HREK"),
            kit_tags("BLAM_TEST_H4EK", "H4EK"),
        ) else {
            eprintln!("skipping: needs HREK and H4EK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "haloreach_mcc").unwrap();
        let by_ext = super::chain_sweep::extension_to_group_tag(&index);
        // The whole family, not just `shader`: H4EK ships 0 of every one of these
        // against 7,140 `.material`, so each needed the same three rules.
        // Every member HREK actually ships. Enumerated from the kits, not guessed:
        // Reach declares 19 shader-family groups and H4EK/H2AMPEK ship zero of all
        // 19, so the whole family is vestigial there. The seven this list started
        // with missed five that ship real content, and the sweep caught them.
        const FAMILY: &[&str] = &[
            "shader",
            "shader_terrain",
            "shader_custom",
            "shader_water",
            "shader_halogram",
            "shader_foliage",
            "shader_decal",
            "shader_glass",
            "shader_screen",
            "shader_fur",
            "shader_fur_stencil",
            "shader_mux",
        ];
        let mut checked = 0usize;
        for group in FAMILY {
        let Some(&group_tag) = by_ext.get(*group) else {
            continue;
        };
        for path in tags_with_extension(&reach, group).iter().take(30) {
            let Ok(source) = read_tag_for_conversion(
                path,
                Some("haloreach_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            // A shader with authored parameters is the one that proves anything.
            let parameters = struct_at_path(source.root(), "render_method")
                .and_then(|render_method| field_by_key(render_method, "parameters"))
                .and_then(|field| field.as_block())
                .map(|block| block.len())
                .unwrap_or(0);
            if parameters == 0 {
                continue;
            }
            let draft = match analyze_conversion(
                &source,
                "haloreach_mcc",
                "halo4_mcc",
                &definitions,
                Some(h4.as_path()),
            ) {
                Ok(draft) => draft,
                Err(error) => panic!("{} refused: {error}", path.display()),
            };
            assert_eq!(
                draft.target_extension, "material",
                "{} must land as a material, not a shader Halo 4 never loads",
                path.display()
            );
            let landed = field_by_key(draft.tag.root(), "material parameters")
                .and_then(|field| field.as_block())
                .map(|block| block.len())
                .unwrap_or(0);
            assert_eq!(
                landed,
                parameters,
                "{} had {parameters} render-method parameters but {landed} arrived",
                path.display()
            );
            checked += 1;
            break;
        }
        }
        assert!(
            checked >= 6,
            "only {checked} of the shader family converted with parameters intact"
        );
    }

    /// A Halo 1 bitmap's pixels reach Halo 2 intact.
    ///
    /// The user asked for this specifically: everything else in the geometry
    /// family is better reimported, but pixel data carried forward avoids a
    /// recompression pass. It was refused because the opaque-copy path required a
    /// matching struct GUID and every classic struct's GUID is all-zero — so a
    /// converted bitmap arrived with all its metadata and no image.
    ///
    /// Asserts the bytes are carried whole, not merely non-empty, and that the
    /// per-bitmap offset into the shared blob survives the `pixel data offset` ->
    /// `pixels offset` rename. Without the rename every entry points at offset 0
    /// and shows the first image's pixels for the whole group.
    #[test]
    fn a_halo1_bitmaps_pixels_arrive_in_halo2() {
        let (Some(h1), Some(h2)) = (
            kit_tags("BLAM_TEST_HCEEK", "HCEEK"),
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
        ) else {
            eprintln!("skipping: needs HCEEK and H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "haloce_mcc").unwrap();
        let Some(&group_tag) =
            super::chain_sweep::extension_to_group_tag(&index).get("bitmap")
        else {
            return;
        };
        let mut checked = 0usize;
        for path in tags_with_extension(&h1, "bitmap").iter().take(60) {
            let Ok(source) = read_tag_for_conversion(
                path,
                Some("haloce_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Some(pixels) = field_by_key(source.root(), "processed pixel data")
                .and_then(|field| field.as_data())
                .map(<[u8]>::to_vec)
            else {
                continue;
            };
            // A multi-entry group is the one that proves the offset rename: with a
            // single bitmap the offset is 0 either way.
            let entries = field_by_key(source.root(), "bitmaps")
                .and_then(|field| field.as_block())
                .map(|block| block.len())
                .unwrap_or(0);
            if pixels.len() < 64 || entries < 2 {
                continue;
            }
            let draft = match analyze_conversion(
                &source,
                "haloce_mcc",
                "halo2_mcc",
                &definitions,
                Some(h2.as_path()),
            ) {
                Ok(draft) => draft,
                Err(error) => panic!("{} refused: {error}", path.display()),
            };
            let landed = field_by_key(draft.tag.root(), "processed pixel data")
                .and_then(|field| field.as_data())
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            assert_eq!(
                landed,
                pixels,
                "{} lost or altered its pixel data ({} bytes in, {} out)",
                path.display(),
                pixels.len(),
                landed.len()
            );
            // At least one entry past the first must carry a non-zero offset,
            // which only happens if the rename resolved.
            let offsets: Vec<i128> = field_by_key(draft.tag.root(), "bitmaps")
                .and_then(|field| field.as_block())
                .map(|block| {
                    block
                        .iter()
                        .filter_map(|element| {
                            field_by_key(element, "pixels offset")?.value().and_then(integer_value)
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                offsets.iter().any(|offset| *offset != 0),
                "{} carried {entries} bitmaps but every pixels offset is 0, so the \
                 `pixel data offset` rename did not resolve",
                path.display()
            );
            checked += 1;
            break;
        }
        assert_eq!(checked, 1, "no multi-entry Halo 1 bitmap with pixel data was converted");
    }

    /// Every Halo CE -> Halo 2 conversion the kits can produce must survive the
    /// classic encoder's own decode-verify.
    ///
    /// This is the regression guard for the empty-path tag reference: a reference
    /// converted to `Some((group, ""))` used to be written as `group + NUL` with
    /// an inline length of 0, and the decoder reads length 0 as "no path" and
    /// consumes nothing — so each one shifted the rest of the body by a byte and
    /// the next block header was read off by one. It took out `sky` (1 stray
    /// byte), `weapon` (2) and `scenario`, and it is exactly the class of damage
    /// that makes a written tag fail to open in the target game's tools while
    /// still looking plausible.
    #[test]
    fn every_classic_conversion_the_kits_can_make_survives_write_verification() {
        let (Some(h1), Some(h2)) = (
            kit_tags("BLAM_TEST_HCEEK", "HCEEK"),
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
        ) else {
            eprintln!("skipping: needs HCEEK and H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "haloce_mcc").unwrap();
        let by_ext = super::chain_sweep::extension_to_group_tag(&source_index);
        let scratch = std::env::temp_dir().join("baboon-classic-verify");
        let mut written = 0usize;
        let mut failures = Vec::new();
        for group in ["sky", "weapon", "scenario", "projectile", "camera_track", "biped"] {
            let (Some(&group_tag), Some(path)) = (
                by_ext.get(group),
                tags_with_extension(&h1, group).into_iter().next(),
            ) else {
                continue;
            };
            let Ok(source) = read_tag_for_conversion(
                &path,
                Some("haloce_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Ok(mut draft) = analyze_conversion(
                &source,
                "haloce_mcc",
                "halo2_mcc",
                &definitions,
                Some(h2.as_path()),
            ) else {
                continue;
            };
            let output = scratch.join(format!("{group}.{}", draft.target_extension));
            if let Some(parent) = output.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // `write_atomic` decode-verifies a classic container before it
            // commits, so this is the same check the real save path performs.
            match draft.tag.write_atomic(&output) {
                Ok(()) => written += 1,
                Err(error) => failures.push(format!("{group}: {error}")),
            }
            let _ = fs::remove_file(&output);
        }
        assert!(written > 0, "no Halo CE tag converted; are the kits installed?");
        assert!(failures.is_empty(), "classic writes failed verification: {failures:#?}");
    }

    /// Walk a converted classic tag's root fields and predict where each
    /// trailing payload lands, so a reported block offset can be attributed to a
    /// field.
    ///
    /// The decoder reports which block ran off the end and at what body offset.
    /// That is only actionable next to the offsets the *model* implies: the field
    /// whose predicted end disagrees with the reported header position is the one
    /// whose encoded length is wrong.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_predicted_classic_trailing_offsets() {
        let (Some(h1), Some(h2)) = (
            kit_tags("BLAM_TEST_HCEEK", "HCEEK"),
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
        ) else {
            eprintln!("skipping: needs HCEEK and H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "haloce_mcc").unwrap();
        let by_ext = super::chain_sweep::extension_to_group_tag(&source_index);
        for group in ["sky", "weapon"] {
            let (Some(&group_tag), Some(path)) = (
                by_ext.get(group),
                tags_with_extension(&h1, group).into_iter().next(),
            ) else {
                continue;
            };
            let Ok(source) = read_tag_for_conversion(
                &path,
                Some("haloce_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Ok(draft) = analyze_conversion(
                &source,
                "haloce_mcc",
                "halo2_mcc",
                &definitions,
                Some(h2.as_path()),
            ) else {
                eprintln!("== {group}: refused");
                continue;
            };
            let root = draft.tag.root();
            eprintln!("== {group} root fixed size {}", root.definition().size());
            let mut at = root.definition().size();
            for field in root.fields() {
                let key = clean_field_key(field.name());
                let before = at;
                let note = match field.field_type() {
                    TagFieldType::TagReference => match field.value() {
                        Some(TagFieldData::TagReference(reference)) => {
                            match &reference.group_tag_and_name {
                                Some((_, name)) => {
                                    at += name.len() + 1;
                                    format!("ref {name:?} -> {} trailing", name.len() + 1)
                                }
                                None => "ref null -> 0 trailing".to_owned(),
                            }
                        }
                        _ => "ref (unreadable)".to_owned(),
                    },
                    TagFieldType::Data => {
                        let len = field.as_data().map(<[u8]>::len).unwrap_or(0);
                        at += len;
                        format!("data -> {len} trailing")
                    }
                    TagFieldType::Block => {
                        let Some(block) = field.as_block() else {
                            continue;
                        };
                        let count = block.len();
                        if count == 0 {
                            "block empty -> 0 trailing".to_owned()
                        } else {
                            let size = block.definition().struct_definition().size();
                            at += 16 + count * size;
                            format!("block {count} x {size} -> {} trailing", 16 + count * size)
                        }
                    }
                    _ => continue,
                };
                eprintln!("   at {before:>6}  {key:34} {note}");
            }
            eprintln!("   predicted end of root trailing: {at}");
        }
    }

    /// Which stage breaks a classic tag's byte layout: reading, resetting, or
    /// converting.
    ///
    /// Three Halo CE -> Halo 2 conversions fail write verification with the block
    /// header read 1-2 bytes early, i.e. the encoder emitted too few bytes
    /// somewhere before it. Read-then-write is supposed to be byte-exact, so this
    /// narrows the desync to a stage rather than guessing at a field.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_which_stage_desyncs_a_classic_tag() {
        let Some(h2) = kit_tags("BLAM_TEST_H2EK", "H2EK") else {
            eprintln!("skipping: no H2EK");
            return;
        };
        let definitions = locate_definitions_root();
        let index = GameTagIndex::load(&definitions, "halo2_mcc").unwrap();
        let by_ext = super::chain_sweep::extension_to_group_tag(&index);
        for group in ["weapon", "sky", "scenario"] {
            let Some(&group_tag) = by_ext.get(group) else {
                continue;
            };
            let Some(path) = tags_with_extension(&h2, group).into_iter().next() else {
                eprintln!("   {group:10} no kit tag");
                continue;
            };
            let Ok(tag) = read_tag_for_conversion(
                &path,
                Some("halo2_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                eprintln!("   {group:10} unreadable");
                continue;
            };
            let original = std::fs::read(&path).unwrap_or_default();
            let rewritten = tag.write_to_bytes();
            let round_trip = match &rewritten {
                Ok(bytes) if *bytes == original => "byte-exact".to_owned(),
                Ok(bytes) => format!("differs ({} vs {} bytes)", bytes.len(), original.len()),
                Err(error) => format!("encode failed: {error}"),
            };

            // Now reset only, no conversion, and see whether the result still
            // decodes. If it does not, the desync is in the reset rather than in
            // any field mapping.
            let mut reset = read_tag_for_conversion(
                &path,
                Some("halo2_mcc"),
                Some(definitions.as_path()),
                group_tag,
            )
            .expect("re-read");
            let aliases =
                SchemaFieldAliases::load(&definitions.join("halo2_mcc").join(format!("{group}.json")))
                    .unwrap_or_default();
            let reset_state = match reset_tag_to_defaults(&mut reset, Some(&aliases)) {
                Err(error) => format!("reset refused: {error}"),
                // `write_atomic` decode-verifies a classic container before it
                // commits, so writing to a scratch path is the real check.
                Ok(()) => {
                    let scratch = std::env::temp_dir().join(format!("baboon-desync.{group}"));
                    match reset.write_atomic(&scratch) {
                        Ok(()) => "decodes".to_owned(),
                        Err(error) => format!("DESYNC: {error}"),
                    }
                }
            };
            eprintln!("   {group:10} round-trip {round_trip}; after reset {reset_state}");
        }
    }

    /// A real Halo 2 vehicle's physics values land in Halo 3's per-type block.
    ///
    /// Halo 2 keeps them flat on the root with a `type` enum; Halo 3 moved them
    /// into `physics types/type-<name>`. Before the routing existed every one of
    /// them was dropped, so a converted warthog or banshee had zero speed. This
    /// pins the whole path on a kit-authored tag: the block gets chosen from the
    /// enum, gains exactly one element, and carries the source's numbers.
    #[test]
    fn a_real_halo2_vehicles_speed_reaches_halo3s_physics_block() {
        let (Some(h2), Some(h3)) = (
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
        ) else {
            eprintln!("skipping: needs both H2EK and H3EK");
            return;
        };
        let definitions = locate_definitions_root();
        let source_index = GameTagIndex::load(&definitions, "halo2_mcc").unwrap();
        let group_tag = *super::chain_sweep::extension_to_group_tag(&source_index)
            .get("vehicle")
            .expect("halo2_mcc defines vehicle");

        // Any vehicle whose forward speed is authored will do; take the first so
        // the test does not depend on one tag surviving a kit update.
        let mut checked = 0usize;
        for path in tags_with_extension(&h2, "vehicle").iter().take(40) {
            let Ok(source) = read_tag_for_conversion(
                path,
                Some("halo2_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Some((speed, _)) = find_real_by_key(source.root(), "maximum forward speed") else {
                continue;
            };
            if speed == 0.0 {
                continue;
            }
            let Ok(draft) = analyze_conversion(
                &source,
                "halo2_mcc",
                "halo3_mcc",
                &definitions,
                Some(h3.as_path()),
            ) else {
                continue;
            };
            // The value must be reachable under `physics types`, not merely
            // present somewhere in the tag.
            let types = struct_at_path(draft.tag.root(), "physics types")
                .expect("halo3_mcc vehicle has a physics types struct");
            let populated = types
                .fields()
                .filter_map(|field| field.as_block())
                .find(|block| !block.is_empty())
                .expect("exactly one physics block is defined");
            assert_eq!(populated.len(), 1, "only one physics block element is defined");
            let element = populated.iter().next().expect("the block has an element");
            // Not every type can hold a speed: Halo 3's `human_tank_block` and
            // `human_jeep_block` carry differentials and an engine struct instead,
            // so a Halo 2 tank's `maximum forward speed` has no destination and is
            // reported as unmatched rather than forced somewhere wrong. Keep
            // looking for a type that does have the field.
            let Some((landed, _)) = find_real_by_key(element, "maximum forward speed") else {
                continue;
            };
            assert_eq!(
                landed, speed,
                "{} forward speed changed on the way into Halo 3",
                path.display()
            );
            checked += 1;
            break;
        }
        assert_eq!(checked, 1, "no Halo 2 vehicle with an authored forward speed was converted");
    }

    /// The Halo 2 -> Halo 3 function header splice, and that it is reversible.
    ///
    /// A Halo 2 curve is 28 bytes and a Halo 3 one is 32; the difference is the
    /// `compact_size` word Halo 3 inserted at offset 28. Promoting has to leave
    /// every other byte where it was, or the curve decodes as a different
    /// function — which is exactly the corruption the parse gate exists to catch.
    #[test]
    fn a_legacy_function_curve_gains_the_compact_size_word_and_nothing_else() {
        // A constant function of 60.1 with a 1.0/1.0 exclusion range, laid out
        // the way a real Halo 2 emitter curve is.
        let mut legacy = vec![0u8; 28];
        legacy[0] = 1;
        legacy[1] = 1;
        legacy[4..8].copy_from_slice(&60.1f32.to_le_bytes());
        legacy[20..24].copy_from_slice(&1.0f32.to_le_bytes());
        legacy[24..28].copy_from_slice(&1.0f32.to_le_bytes());

        let modern = retarget_function_bytes(&legacy, false, true).expect("28 bytes promotes");
        assert_eq!(modern.len(), 32);
        assert_eq!(&modern[..28], &legacy[..], "the original bytes must not move");
        assert_eq!(
            u32::from_le_bytes(modern[28..32].try_into().unwrap()),
            0,
            "a header-only curve has no compact block"
        );
        // The engine's decoder is the arbiter, so the promoted form must satisfy it.
        assert!(crate::TagFunction::parse(&modern).is_ok());

        // And back again, losslessly.
        assert_eq!(
            retarget_function_bytes(&modern, true, false).expect("32 bytes demotes"),
            legacy
        );

        // Trailing compact data keeps its length in the spliced word.
        let mut with_compact = legacy.clone();
        with_compact.extend_from_slice(&[7u8; 8]);
        let promoted = retarget_function_bytes(&with_compact, false, true).expect("promotes");
        assert_eq!(promoted.len(), 40);
        assert_eq!(u32::from_le_bytes(promoted[28..32].try_into().unwrap()), 8);
        assert_eq!(&promoted[32..], &[7u8; 8]);

        // Too short to be a header at all is refused rather than padded.
        assert!(retarget_function_bytes(&[0u8; 20], false, true).is_none());
    }

    /// Total bytes across every `data` blob in a tag.
    ///
    /// A bitmap's pixels live in one of these, so this is "is the image still
    /// here?" reduced to a number that survives the field being renamed between
    /// engines.
    pub fn blob_bytes(tag: &TagFile) -> usize {
        fn walk(value: TagStruct<'_>, total: &mut usize) {
            for field in value.fields() {
                match field.value() {
                    Some(TagFieldData::Data(bytes)) => *total += bytes.len(),
                    _ => {}
                }
                if let Some(nested) = field.as_struct() {
                    walk(nested, total);
                }
                if let Some(block) = field.as_block() {
                    for index in 0..block.len() {
                        if let Some(element) = block.element(index) {
                            walk(element, total);
                        }
                    }
                }
            }
        }
        let mut total = 0;
        walk(tag.root(), &mut total);
        total
    }

    /// An editing kit's `tags` directory, via `env_var` or the Steam library
    /// this repo is developed against.
    pub fn kit_tags(env_var: &str, kit: &str) -> Option<PathBuf> {
        if let Ok(path) = std::env::var(env_var) {
            let path = PathBuf::from(path);
            return path.is_dir().then_some(path);
        }
        [
            "D:/SteamLibrary/steamapps/common",
            "C:/Program Files (x86)/Steam/steamapps/common",
            "C:/Program Files/Steam/steamapps/common",
            "E:/SteamLibrary/steamapps/common",
        ]
        .iter()
        .map(|root| PathBuf::from(root).join(kit).join("tags"))
        .find(|path| path.is_dir())
    }

    /// Tags this harness wrote itself.
    ///
    /// `baboon_converted` lives inside the kit's own `tags` tree and sorts early,
    /// so an unfiltered alphabetical scan picks a *converted* tag as the "stock"
    /// source and silently measures a double conversion. That is exactly what
    /// happened: scanning H3EK for `.particle` returned
    /// `baboon_converted/halo2_mcc/burst_large.particle`.
    pub fn is_generated_output(path: &Path) -> bool {
        path.components()
            .any(|component| component.as_os_str().eq_ignore_ascii_case("baboon_converted"))
    }

    /// Every file with `extension` under `root`, sorted so a scan is repeatable.
    /// Does any struct in the group declare `left` and `right` as one field under a
    /// `{former name}` marker?
    ///
    /// The matcher scopes this by struct identity, correctly. The numeric diff is
    /// keyed by field name and has no struct to scope with, so it asks the question
    /// group-wide; the value-equality test at the call site is what keeps that from
    /// being loose.
    fn declares_alias(aliases: &SchemaFieldAliases, left: &str, right: &str) -> bool {
        aliases
            .by_struct
            .values()
            .any(|fields| fields.get(left).is_some_and(|set| set.contains(right)))
    }

    pub fn tags_with_extension(root: &Path, extension: &str) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = walk_files(root)
            .into_iter()
            .filter(|path| path.extension().and_then(|e| e.to_str()) == Some(extension))
            .filter(|path| !is_generated_output(path))
            .collect();
        out.sort();
        out
    }

    /// Every scalar number under `value`, keyed by cleaned field name, as
    /// `(rendered value, comparable number, type name)`.
    ///
    /// Keyed by name rather than path on purpose. Source and target paths differ
    /// wherever a struct was renamed or reparented, so a path-keyed diff reports
    /// the whole tag as changed and tells us nothing. A name that occurs exactly
    /// once on each side is unambiguous, and that subset is large enough to catch
    /// a value landing in the wrong place.
    fn collect_numbers(value: TagStruct<'_>, into: &mut Vec<(String, f64, &'static str)>) {
        collect_numbers_at(value, "", into);
    }

    /// Walk every number under `value`, keyed by an index-qualified path of
    /// cleaned field names.
    ///
    /// Struct *names* are deliberately not part of the key — only field names —
    /// because a reparented or renamed struct changes the name chain without
    /// changing where the value belongs. Block indices *are* part of it, since a
    /// value landing in the wrong element is exactly the failure being hunted.
    fn collect_numbers_at(
        value: TagStruct<'_>,
        prefix: &str,
        into: &mut Vec<(String, f64, &'static str)>,
    ) {
        for field in value.fields() {
            let key = clean_field_key(field.name());
            let path = if key.is_empty() {
                prefix.to_owned()
            } else if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}/{key}")
            };
            if let Some(data) = field.value() {
                let number = match &data {
                    TagFieldData::Real(v) | TagFieldData::Angle(v) => Some((*v as f64, "real")),
                    _ => integer_value(data).map(|v| (v as f64, "integer")),
                };
                if let Some((number, kind)) = number
                    && !key.is_empty()
                {
                    into.push((path.clone(), number, kind));
                }
            }
            if let Some(child) = field.as_struct() {
                collect_numbers_at(child, &path, into);
            }
            if let Some(block) = field.as_block() {
                // A curve held as a block of bytes is measured by `curves_carried`,
                // not here. Counting its bytes as numbers makes a perfect curve
                // transfer look like total loss, because the target holds the same
                // bytes in one opaque blob with no numeric fields at all.
                if key == "data" && block.definition().struct_definition().size() == 1 {
                    continue;
                }
                for (index, element) in block.iter().enumerate() {
                    collect_numbers_at(element, &format!("{path}[{index}]"), into);
                }
            }
            if let Some(array) = field.as_array() {
                for (index, element) in array.iter().enumerate() {
                    collect_numbers_at(element, &format!("{path}[{index}]"), into);
                }
            }
        }
    }

    /// Total bytes of `mapping_function` curve under `value`, counting both
    /// spellings: a `data` blob declared as function data, and a block of
    /// one-byte elements named `data`.
    ///
    /// Needed because [`collect_numbers`] cannot see across the two spellings —
    /// Halo 2's curve is 1,180 individually-numbered bytes and Halo 3's is one
    /// opaque blob, so a number-count diff reports a *perfect* curve transfer as
    /// total loss. Counting bytes is the comparison that means something.
    fn curve_blobs(value: TagStruct<'_>, into: &mut Vec<Vec<u8>>) {
        for field in value.fields() {
            if field.is_function_data()
                && let Some(bytes) = field.as_data()
                && !bytes.is_empty()
            {
                into.push(bytes.to_vec());
            }
            if let Some(child) = field.as_struct() {
                curve_blobs(child, into);
            }
            if let Some(block) = field.as_block() {
                if clean_field_key(field.name()) == "data"
                    && block.definition().struct_definition().size() == 1
                {
                    let mut bytes = Vec::with_capacity(block.len());
                    for element in block.iter() {
                        let byte = element
                            .fields()
                            .find_map(|f| f.value())
                            .and_then(integer_value)
                            .unwrap_or(0);
                        bytes.push(byte as u8);
                    }
                    if !bytes.is_empty() {
                        into.push(bytes);
                    }
                    continue;
                }
                for element in block.iter() {
                    curve_blobs(element, into);
                }
            }
            if let Some(array) = field.as_array() {
                for element in array.iter() {
                    curve_blobs(element, into);
                }
            }
        }
    }

    /// `(source curves, of those that arrived verbatim in the target)`.
    ///
    /// A byte count alone cannot answer this: the engine seeds every empty
    /// `function_definition_data` field with a 32-byte default curve, so a tag
    /// that transferred nothing still reports plenty of curve bytes. Matching the
    /// blobs is what distinguishes a carried curve from a default one.
    fn curves_carried(source: TagStruct<'_>, target: TagStruct<'_>) -> (usize, usize) {
        let mut before = Vec::new();
        curve_blobs(source, &mut before);
        let mut after = Vec::new();
        curve_blobs(target, &mut after);
        // A curve that crossed a header-layout boundary is 4 bytes longer or
        // shorter than it started, so match against both spellings.
        let carried = before
            .iter()
            .filter(|bytes| {
                let promoted = retarget_function_bytes(bytes, false, true);
                let demoted = retarget_function_bytes(bytes, true, false);
                after.iter().any(|candidate| {
                    candidate == *bytes
                        || Some(candidate.clone()) == promoted
                        || Some(candidate.clone()) == demoted
                })
            })
            .count();
        (before.len(), carried)
    }

    /// Names that occur exactly once in `numbers`, so a diff against them is
    /// unambiguous.
    fn unique_numbers(
        numbers: &[(String, f64, &'static str)],
    ) -> HashMap<String, (f64, &'static str)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (key, _, _) in numbers {
            *counts.entry(key.as_str()).or_default() += 1;
        }
        numbers
            .iter()
            .filter(|(key, _, _)| counts.get(key.as_str()) == Some(&1))
            .map(|(key, number, kind)| (key.clone(), (*number, *kind)))
            .collect()
    }

    /// Report every unambiguously-named number whose value changed across a
    /// conversion.
    ///
    /// The user found "a lot of the number values are not coming over correctly"
    /// in Halo 2 -> Halo 3 output. This measures it rather than reasoning about
    /// it, and prints both sides so a wild *source* number (which would mean the
    /// read is misaligned, not the conversion) is distinguishable from a wild
    /// target one.
    #[test]
    #[ignore = "diagnostic; needs the editing kits"]
    fn report_numbers_that_change_across_a_conversion() {
        let definitions = locate_definitions_root();
        // Every adjacent pair, over the groups a user is most likely to carry
        // forward. Ranked output at the end, so the worst offenders are the ones
        // to fix next rather than whichever happened to be measured first.
        const GROUPS: &[&str] = &[
            "biped",
            "vehicle",
            "weapon",
            "projectile",
            "equipment",
            "scenery",
            "crate",
            "effect",
            "particle",
            "light",
            "damage_effect",
            "material_effects",
            "sound_looping",
            "physics_model",
            "model",
            "device_machine",
            "device_control",
            "lens_flare",
            "decal_system",
            "contrail_system",
            "character",
            "creature",
            "giant",
            "cinematic",
            "globals",
        ];
        let mut ranked: Vec<(usize, String)> = Vec::new();
        // Narrowing filters, because closing the ranked list means re-running this
        // one leg and one group at a time and the full sweep is six legs x 25
        // groups of kit walking. `BLAM_TEST_ONLY_PAIR` matches the source profile,
        // `BLAM_TEST_ONLY_GROUP` the group name.
        let only_pair = std::env::var("BLAM_TEST_ONLY_PAIR").ok();
        let only_group = std::env::var("BLAM_TEST_ONLY_GROUP").ok();
        // Deciding whether a group's losses are genuine engine changes means reading
        // every one of them, and the default cap hid two thirds of the biggest
        // entries. `BLAM_TEST_LOST_LIMIT=0` prints them all.
        let lost_limit = std::env::var("BLAM_TEST_LOST_LIMIT")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .map_or(8, |value| if value == 0 { usize::MAX } else { value });
        for (source_kit, source_game, target_kit, target_game) in super::chain_sweep::CHAIN {
            if only_pair.as_deref().is_some_and(|want| want != *source_game) {
                continue;
            }
            let (Some(source_tags), Some(target_tags)) = (
                kit_tags(&format!("BLAM_TEST_{source_kit}"), source_kit),
                kit_tags(&format!("BLAM_TEST_{target_kit}"), target_kit),
            ) else {
                eprintln!("skipping {source_game} -> {target_game}: kit missing");
                continue;
            };
            let source_index = GameTagIndex::load(&definitions, source_game).unwrap();
            let target_index = GameTagIndex::load(&definitions, target_game).unwrap();
            let by_ext = super::chain_sweep::extension_to_group_tag(&source_index);
            let found = super::chain_sweep::first_tag_by_extension(&source_tags);
            let templates = NativeTemplateIndex::build(&target_tags, &target_index);
            eprintln!("== {source_game} -> {target_game}");
            for group in GROUPS {
                if only_group.as_deref().is_some_and(|want| want != *group) {
                    continue;
                }
                if !source_index.by_name.contains_key(*group)
                    || !target_index.by_name.contains_key(*group)
                {
                    continue;
                }
                let (Some(&group_tag), Some(path)) = (by_ext.get(*group), found.get(*group)) else {
                    continue;
                };
                let Ok(source) = read_tag_for_conversion(
                    path,
                    Some(source_game),
                    Some(definitions.as_path()),
                    group_tag,
                ) else {
                    eprintln!("   {group:24} source unreadable");
                    continue;
                };
                let draft = match analyze_conversion_with_templates(
                    &source,
                    source_game,
                    target_game,
                    &definitions,
                    Some(&templates),
                ) {
                    Ok(draft) => draft,
                    Err(error) => {
                        eprintln!("   {group:24} refused: {error}");
                        continue;
                    }
                };
                let mut before = Vec::new();
                collect_numbers(source.root(), &mut before);
                let mut after = Vec::new();
                collect_numbers(draft.tag.root(), &mut after);
                // A companion tag is part of the conversion's output, not something
                // outside it: H3's `player responses` become separate Reach tags, so
                // measuring `draft.tag` alone reported nine `damage_effect` numbers
                // as vanished with no issue raised — the signature of a silent loss,
                // which is exactly what this diagnostic exists to catch. Counted in
                // its own bucket so a value that left the tag is still visible as
                // having moved rather than being folded into "comparable".
                let mut companion_numbers = Vec::new();
                for companion in &draft.companion_tags {
                    collect_numbers(companion.tag.root(), &mut companion_numbers);
                }
                let source_total = before.len();
                let target_total = after.len();
                let after_all = after.clone();
                // A NaN or infinity written into a target tag is a crash risk in the
                // target game's own tools, and the source kits demonstrably contain
                // them, so check that a conversion never carries one across a field
                // that *did* match. NaN compares unequal to itself, so the changed/
                // comparable test above cannot see this.
                let bad_in = before
                    .iter()
                    .filter(|(_, value, _)| !value.is_finite())
                    .map(|(path, _, _)| path.clone())
                    .collect::<Vec<_>>();
                let bad_in = if bad_in.is_empty() {
                    String::from("none")
                } else {
                    bad_in.join(", ")
                };
                let bad_out = after_all
                    .iter()
                    .filter(|(_, value, _)| !value.is_finite())
                    .map(|(path, _, _)| path.clone())
                    .collect::<Vec<_>>();
                if !bad_out.is_empty() {
                    eprintln!(
                        "   {:24} NON-FINITE in source: {bad_in}\n   {:24} NON-FINITE in output: {}",
                        "",
                        "",
                        bad_out.join(", ")
                    );
                }
                let before = unique_numbers(&before);
                let after = unique_numbers(&after);
                let mut shared = 0usize;
                let mut changed: Vec<String> = Vec::new();
                let mut missing: Vec<String> = Vec::new();
                let mut moved = 0usize;
                let mut unset = 0usize;
                let mut renamed = 0usize;
                let mut to_companion = 0usize;
                let mut renamed_lines: Vec<String> = Vec::new();
                // The schema's own `{former name}` markers, for the rename bucket
                // below. Loaded per group because that is the file that declares
                // them, and `load` walks the `parent_tag` chain for inherited ones.
                let target_aliases =
                    SchemaFieldAliases::load(&definitions.join(target_game).join(format!(
                        "{group}.json"
                    )))
                    .ok();
                for (key, (from, from_kind)) in &before {
                    let Some((to, to_kind)) = after.get(key) else {
                        if *from != 0.0 {
                            // A reshape moves a value to a different path under the
                            // same field name — Halo 2's flat `maximum forward
                            // speed` becomes `physics types/type-human_plane[0]/
                            // maximum forward speed`. That is a success, not a
                            // loss, so match on trailing name plus value before
                            // calling anything lost.
                            let name = key.rsplit('/').next().unwrap_or(key);
                            let landed = after_all.iter().any(|(path, value, _)| {
                                path.rsplit('/').next().unwrap_or(path) == name
                                    && (value - from).abs()
                                        <= f64::from(f32::EPSILON) * from.abs().max(1.0)
                            });
                            // A `{former name}` rename is neither the same name nor
                            // the same path, so the trailing-name test above cannot
                            // see it: Reach renamed Halo 3's `root offset max scale`
                            // to `root offset max scale idle` and the value carries
                            // intact. Requiring the value to be *present* under the
                            // aliased name keeps this a verification rather than a
                            // restatement of the converter's own decision.
                            let renamed_to = target_aliases.as_ref().and_then(|aliases| {
                                after_all
                                    .iter()
                                    .find(|(path, value, _)| {
                                        (value - from).abs()
                                            <= f64::from(f32::EPSILON) * from.abs().max(1.0)
                                            && declares_alias(
                                                aliases,
                                                path.rsplit('/').next().unwrap_or(path),
                                                name,
                                            )
                                    })
                                    .map(|(path, _, _)| path.clone())
                            });
                            let companioned = companion_numbers.iter().any(|(path, value, _)| {
                                path.rsplit('/').next().unwrap_or(path) == name
                                    && (value - from).abs()
                                        <= f64::from(f32::EPSILON) * from.abs().max(1.0)
                            });
                            if landed {
                                moved += 1;
                            } else if companioned {
                                to_companion += 1;
                            } else if let Some(to) = renamed_to {
                                renamed += 1;
                                renamed_lines.push(format!("{key} = {from} -> {to}"));
                            } else if !from.is_finite()
                                || from.abs() < f64::from(f32::MIN_POSITIVE)
                            {
                                // NaN, an infinity, or a subnormal float is not a
                                // number an author typed. Halo 2 stamps its unused
                                // `object` collision-damage slots with fixed junk
                                // patterns — the same bits appear in unrelated tags
                                // — and counting those as lost data put `projectile`
                                // and `crate` on the ranking on nothing at all.
                                unset += 1;
                            } else if *from == -1.0 && *from_kind == "integer" {
                                // -1 is Halo's universal "none" for an index or
                                // option slot, so a dropped one is an unset field
                                // rather than authored data. Counted apart because
                                // otherwise it dominates: all 36 of one Reach
                                // decal_system's apparent losses were -1 option
                                // indices, which put it third in the ranking on
                                // nothing at all.
                                unset += 1;
                            } else {
                                missing.push(format!("{key} = {from} ({from_kind})"));
                            }
                        }
                        continue;
                    };
                    shared += 1;
                    if (from - to).abs() > f64::from(f32::EPSILON) * from.abs().max(1.0) {
                        // A field present under the same name on both sides that
                        // ends up holding a different number is either a genuine
                        // mistranslation or a refusal whose template default happens
                        // to differ — opposite verdicts. Say which, because only the
                        // first is a bug and the second is already reported.
                        let reported = draft
                            .report
                            .issues
                            .iter()
                            .any(|issue| issue.path == *key || issue.path.ends_with(key.as_str()));
                        changed.push(format!(
                            "{key}: {from} ({from_kind}) -> {to} ({to_kind}) [{}]",
                            if reported { "reported" } else { "SILENT" }
                        ));
                    }
                }
                eprintln!(
                    "   {group:24} {source_total} numbers in -> {target_total} out; \
                     {shared} comparable, {} changed, {moved} moved, {renamed} renamed, \
                     {to_companion} to companions, {unset} unset, {} lost",
                    changed.len(),
                    missing.len(),
                );
                if draft.report.truncated > 0 || draft.report.unsupported_source > 0 {
                    eprintln!(
                        "   {:24} report: {} truncated, {} unsupported",
                        "",
                        draft.report.truncated,
                        draft.report.unsupported_source
                    );
                }
                let (curves, carried) = curves_carried(source.root(), draft.tag.root());
                if curves > 0 {
                    eprintln!("   {:24} curves {carried}/{curves} carried", "");
                    let mut reasons: HashMap<String, usize> = HashMap::new();
                    for issue in &draft.report.issues {
                        if issue.message.contains("function") || issue.path.contains("mapping") {
                            *reasons.entry(issue.message.clone()).or_default() += 1;
                        }
                    }
                    let mut reasons: Vec<_> = reasons.into_iter().collect();
                    reasons.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
                    for (message, count) in reasons.iter().take(3) {
                        eprintln!("   {:24}   x{count} {message}", "");
                    }
                }
                changed.sort();
                for line in changed.iter().take(8) {
                    eprintln!("      CHANGED {line}");
                }
                if changed.len() > 8 {
                    eprintln!("      ... and {} more changed", changed.len() - 8);
                }
                renamed_lines.sort();
                for line in renamed_lines.iter().take(8) {
                    eprintln!("      RENAMED {line}");
                }
                missing.sort();
                for line in missing.iter().take(lost_limit) {
                    eprintln!("      LOST    {line}");
                }
                if missing.len() > lost_limit {
                    eprintln!("      ... and {} more lost", missing.len() - 4);
                }
                if !missing.is_empty() || !changed.is_empty() {
                    ranked.push((
                        missing.len() + changed.len(),
                        format!(
                            "{source_game} -> {target_game} {group}:                              {} lost, {} changed",
                            missing.len(),
                            changed.len()
                        ),
                    ));
                }
            }
        }
        ranked.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        eprintln!("
== worst numeric losses, most to least");
        for (_, line) in ranked.iter().take(30) {
            eprintln!("   {line}");
        }
    }

    /// The first real-scalar field anywhere under `value` whose cleaned name is
    /// `key`, as `(f32, TagFieldType)`.
    fn find_real_by_key(value: TagStruct<'_>, key: &str) -> Option<(f32, TagFieldType)> {
        for field in value.fields() {
            if clean_field_key(field.name()) == key
                && is_real_scalar(field.field_type())
                && let Some(number) = field.value().and_then(real_value)
            {
                return Some((number, field.field_type()));
            }
            if let Some(child) = field.as_struct()
                && let Some(found) = find_real_by_key(child, key)
            {
                return Some(found);
            }
            if let Some(block) = field.as_block() {
                for element in block.iter() {
                    if let Some(found) = find_real_by_key(element, key) {
                        return Some(found);
                    }
                }
            }
            if let Some(array) = field.as_array() {
                for element in array.iter() {
                    if let Some(found) = find_real_by_key(element, key) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// The radians-to-degrees rescale, proved on a tag the kit authored.
    ///
    /// `grenade angle:degrees` is typed `angle` in ODST — radians on the wire —
    /// and `real` in Reach, which stores the degrees its name promises. Before
    /// this was handled, a 30-degree ODST angle arrived in Reach as 0.5236
    /// degrees. The unit test pins the decision; this pins the number, on a real
    /// biped, through the real conversion, which is the only place the two halves
    /// meet.
    #[test]
    fn a_real_odst_grenade_angle_arrives_in_reach_as_degrees() {
        let Some(odst) = kit_tags("BLAM_TEST_H3ODSTEK", "H3ODSTEK") else {
            eprintln!("skipping: no ODST kit (set BLAM_TEST_H3ODSTEK to its `tags` directory)");
            return;
        };
        let Some(reach) = kit_tags("BLAM_TEST_HREK", "HREK") else {
            eprintln!("skipping: no HREK kit (set BLAM_TEST_HREK to its `tags` directory)");
            return;
        };
        let definitions = locate_definitions_root();

        // Find a biped that actually authored the field; a zero proves nothing.
        let mut chosen = None;
        for path in tags_with_extension(&odst, "biped") {
            let Ok(tag) = TagFile::read(&path) else { continue };
            let Some((radians, field_type)) = find_real_by_key(tag.root(), "grenade angle") else {
                continue;
            };
            if field_type == TagFieldType::Angle && radians.abs() > 1e-4 {
                chosen = Some((path, tag, radians));
                break;
            }
        }
        let Some((path, source, radians)) = chosen else {
            eprintln!("skipping: no ODST biped authored a non-zero `grenade angle`");
            return;
        };

        let draft = analyze_conversion(
            &source,
            "halo3odst_mcc",
            "haloreach_mcc",
            &definitions,
            Some(reach.as_path()),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

        let (landed, landed_type) = find_real_by_key(draft.tag.root(), "grenade angle")
            .expect("Reach declares `grenade angle`");
        assert_eq!(
            landed_type,
            TagFieldType::Real,
            "Reach types this field `real`; the fixture assumption changed"
        );
        let expected = radians.to_degrees();
        assert!(
            (landed - expected).abs() < 1e-3,
            "{}: {radians} radians should land as {expected} degrees, got {landed}",
            path.display()
        );
        // And the bug this guards against: the raw bits copied across.
        assert!(
            (landed - radians).abs() > 1e-3,
            "{}: the value crossed unchanged, so the rescale did not run",
            path.display()
        );
    }

    /// An `angle` and a `real` holding the same quantity are not the same bits.
    ///
    /// `angle` stores radians and is authored in degrees — pinned by
    /// `angle_fields_are_edited_in_degrees_and_stored_in_radians`. Six fields in
    /// the shipped definitions change between `angle` and `real` across a pair,
    /// so copying the float straight across is wrong by 180/pi. Where both names
    /// carry the same `:units` the factor is provable and applied; where neither
    /// does, the converter must say so rather than guess.
    #[test]
    fn an_angle_that_becomes_a_real_is_rescaled_only_when_the_schema_proves_it() {
        use TagFieldType as T;

        let deg = Some("degrees");

        // `unit`'s `grenade angle:degrees` is `angle` in ODST and `real` in
        // Reach/H4/H2A/CE. Both schemas say degrees, so only storage differs.
        assert_eq!(
            real_scalar_unit_change(T::Angle, deg, T::Real, deg),
            RealUnitChange::RadiansToDegrees
        );
        assert_eq!(
            real_scalar_unit_change(T::Real, deg, T::Angle, deg),
            RealUnitChange::DegreesToRadians
        );

        // `vehicle`'s `fixed gun pitch` is `real` in H3/ODST and `angle` in
        // Reach+, with no unit on either side — and the shipped tags store the
        // same number on both (0.25 vs 0.24993114), so the bits move.
        assert_eq!(real_scalar_unit_change(T::Real, None, T::Angle, None), RealUnitChange::Copy);
        // A unit on one side only, or two different units, proves nothing, so
        // the pre-existing behaviour stands rather than a guessed rescale.
        assert_eq!(real_scalar_unit_change(T::Angle, deg, T::Real, None), RealUnitChange::Copy);
        assert_eq!(
            real_scalar_unit_change(T::Angle, deg, T::Real, Some("world units")),
            RealUnitChange::Copy
        );

        // Neither side an angle, or both: the bits move untouched, unit or not.
        assert_eq!(
            real_scalar_unit_change(T::Real, None, T::RealFraction, None),
            RealUnitChange::Copy
        );
        assert_eq!(
            real_scalar_unit_change(T::Angle, deg, T::Angle, None),
            RealUnitChange::Copy
        );

        // The annotation is read off the schema name, where it still exists.
        // The `#help` tail must not be mistaken for a unit.
        assert_eq!(field_unit_annotation("grenade angle:degrees"), Some("degrees".to_owned()));
        assert_eq!(field_unit_annotation("delay:secs#how long to wait"), Some("secs".to_owned()));
        assert_eq!(field_unit_annotation("ratio#a:b comparison"), None);
        assert_eq!(field_unit_annotation("plain name"), None);
        // And a tag's own layout has already lost it, which is why the schema is
        // consulted rather than the field the converter is holding.
        assert_eq!(field_unit_annotation("grenade angle"), None);
    }

    /// Every reviewed drop has to resolve to a real field, by the same path the
    /// converter will report it under.
    ///
    /// This is the test the first attempt needed and did not have. The rules
    /// were written with bare field names — `additional flags` — while the
    /// converter reports `definitions/skeleton nodes[0]/additional flags`, so
    /// nothing matched and every animation graph with a non-default node flag
    /// refused. A real 167 MB Halo Reach graph found that; a schema walk finds
    /// it in milliseconds, on CI, without a kit.
    ///
    /// Element indices are stripped before comparison, which is what lets one
    /// rule cover a field inside a block of 2,136 elements.
    #[test]
    fn every_accepted_drop_resolves_along_the_path_the_converter_reports() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        let definitions = locate_definitions_root();
        assert!(
            !catalog.accepted_field_drops.is_empty(),
            "the animation graph rules should be here",
        );

        for rule in &catalog.accepted_field_drops {
            for game in &rule.source_games {
                let path = definitions.join(game).join(format!("{}.json", rule.group));
                if !path.is_file() {
                    continue;
                }
                let tag = TagFile::new(&path)
                    .unwrap_or_else(|error| panic!("build {game}/{}: {error}", rule.group));
                assert!(
                    schema_path_resolves(tag.definitions().root_struct(), &rule.source_path),
                    "{game}/{}: `{}` does not resolve; the converter would report a                      different path and this rule would never fire",
                    rule.group,
                    rule.source_path,
                );
            }
            // And it must genuinely be absent on the far side, or the rule is
            // hiding a conversion failure rather than recording a known loss.
            for game in &rule.target_games {
                let path = definitions.join(game).join(format!("{}.json", rule.group));
                if !path.is_file() {
                    continue;
                }
                let tag = TagFile::new(&path).unwrap();
                assert!(
                    !schema_path_resolves(tag.definitions().root_struct(), &rule.source_path),
                    "{game}/{} does declare `{}`, so it is not a drop",
                    rule.group,
                    rule.source_path,
                );
            }
        }
    }

    /// An accepted *payload* drop must name a blob both sides really declare.
    ///
    /// The mirror image of the check above, and the reason the two sections are
    /// separate. If the target does not declare the field, the loss is an
    /// ordinary field drop and belongs in `accepted_field_drops` where the
    /// absence is verified; if it does, the blob is being dropped despite having
    /// somewhere to go, and that is the claim this section makes. A rule in the
    /// wrong list would be checked against the wrong invariant and pass.
    #[test]
    fn every_accepted_payload_drop_names_a_blob_both_sides_declare() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        let definitions = locate_definitions_root();
        for rule in &catalog.accepted_payload_drops {
            for (role, games) in [
                ("source", &rule.source_games),
                ("target", &rule.target_games),
            ] {
                for game in games {
                    let path = definitions.join(game).join(format!("{}.json", rule.group));
                    if !path.is_file() {
                        continue;
                    }
                    let tag = TagFile::new(&path)
                        .unwrap_or_else(|error| panic!("build {game}/{}: {error}", rule.group));
                    assert!(
                        schema_path_resolves(tag.definitions().root_struct(), &rule.source_path),
                        "{game}/{} ({role}) does not declare `{}` \u{2014} that is an ordinary \
                         field drop, so the rule belongs in accepted_field_drops where its \
                         absence gets checked",
                        rule.group,
                        rule.source_path,
                    );
                }
            }
        }
    }

    /// Walk a `/`-separated field path through a schema, descending containers.
    #[cfg(test)]
    fn schema_path_resolves(root: crate::TagStructDefinition<'_>, path: &str) -> bool {
        let mut current = root;
        let segments: Vec<String> = crate::TagFieldPath::parse(path)
            .strip_node_indices()
            .to_string()
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .collect();
        let Some((last, parents)) = segments.split_last() else {
            return false;
        };
        for segment in parents {
            let Some(next) = current.fields().find_map(|field| {
                (clean_field_key(field.name()) == clean_field_key(segment))
                    .then(|| {
                        field
                            .as_struct()
                            .or_else(|| field.as_block().map(|b| b.struct_definition()))
                            .or_else(|| field.as_array().map(|a| a.struct_definition()))
                            .or_else(|| field.as_resource().map(|r| r.struct_definition()))
                    })
                    .flatten()
            }) else {
                return false;
            };
            current = next;
        }
        current
            .fields()
            .any(|field| clean_field_key(field.name()) == clean_field_key(last))
    }

    /// A real Halo Reach animation graph, off disk, with its animation payload
    /// in it. Self-skips without HREK, so a green run here proves less than it
    /// looks — check for the skip line.
    ///
    /// The schema-built test above cannot cover this: `TagFile::new` produces
    /// null resources, and the entire question is whether a *populated* one
    /// crosses.
    #[test]
    fn a_real_hrek_animation_graph_carries_its_payload_into_campaign_evolved() {
        let source_path = Path::new(
            "D:/SteamLibrary/steamapps/common/HREK/tags/cinematics/052lb_reflection/objects/052lb_reflection_030/elevator_1.model_animation_graph",
        );
        if !source_path.is_file() {
            eprintln!("skipping: HREK is not installed at the expected path");
            return;
        }
        let definitions = locate_definitions_root();
        let source = TagFile::read(source_path).expect("read the HREK animation graph");

        let resources_in = count_non_null_resources(source.root());
        assert!(
            resources_in > 0,
            "this fixture must actually carry a payload, or the assertions below \
             are 0 == 0 and prove nothing",
        );
        let draft = analyze_conversion(
            &source,
            "haloreach_mcc",
            CAMPAIGN_EVOLVED_GAME,
            &definitions,
            None,
        )
        .unwrap_or_else(|error| panic!("a real Reach animation graph must convert: {error}"));

        assert_eq!(
            draft.report.transferred_resources, resources_in,
            "every pageable resource the source carried has to arrive; the payload \
             is most of what an animation graph is",
        );
        let bytes = draft.tag.write_to_bytes().expect("serialize the converted graph");
        let reopened = TagFile::read_from_bytes(&bytes).expect("read it back");
        assert_eq!(
            count_non_null_resources(reopened.root()),
            resources_in,
            "the resources survived the write",
        );
    }

    #[cfg(test)]
    fn count_non_null_resources(structure: TagStruct<'_>) -> usize {
        structure
            .fields()
            .map(|field| {
                let here = field
                    .as_resource()
                    .is_some_and(|resource| !matches!(resource.kind(), TagResourceKind::Null))
                    as usize;
                let nested = field.as_struct().map(count_non_null_resources).unwrap_or(0)
                    + field
                        .as_block()
                        .map(|block| block.iter().map(count_non_null_resources).sum())
                        .unwrap_or(0)
                    + field
                        .as_array()
                        .map(|array| array.iter().map(count_non_null_resources).sum())
                        .unwrap_or(0);
                here + nested
            })
            .sum()
    }

    #[test]
    fn real_h3_animation_payload_is_rejected_instead_of_written_incomplete() {
        let source_path = Path::new(
            "D:/SteamLibrary/steamapps/common/H3EK/tags/fx/null_object/null_up/null_up.model_animation_graph",
        );
        let target_root = Path::new("D:/SteamLibrary/steamapps/common/HREK/tags");
        if !source_path.is_file() || !target_root.is_dir() {
            return;
        }
        let definitions = locate_definitions_root();
        let source = TagFile::read(source_path).unwrap();
        let error = analyze_conversion(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(target_root),
        )
        .err()
        .expect("unsafe animation graph must not produce a draft");
        assert!(
            error.contains("pageable runtime resources")
                || error.contains("model_animation_graph conversion would lose"),
            "unexpected animation safety error: {error}"
        );
    }

    #[test]
    fn catalogued_legacy_model_and_particle_reference_drops_are_one_way() {
        let catalog = ConversionMappingCatalog::load().unwrap();
        for (group, field) in [("model", "lod_render_model"), ("particle", "shader")] {
            assert!(
                catalog
                    .reference_drop_reason(group, "halo3_mcc", "haloreach_mcc", field,)
                    .is_some()
            );
            assert!(
                catalog
                    .reference_drop_reason(group, "haloreach_mcc", "halo3_mcc", field,)
                    .is_none()
            );
        }
    }

    // The folder-conversion worker test stayed in the editor alongside
    // run_folder_conversion_job: walking a folder, deciding destinations and
    // reporting progress to the UI thread is workflow, not conversion.
}


#[cfg(test)]
mod chain_sweep {
    use super::tests::kit_tags;
    use super::*;

    /// Kit folder, source profile, kit folder, target profile.
    pub const CHAIN: &[(&str, &str, &str, &str)] = &[
        ("HCEEK", "haloce_mcc", "H2EK", "halo2_mcc"),
        ("H2EK", "halo2_mcc", "H3EK", "halo3_mcc"),
        ("H3EK", "halo3_mcc", "H3ODSTEK", "halo3odst_mcc"),
        ("H3EK", "halo3_mcc", "HREK", "haloreach_mcc"),
        ("HREK", "haloreach_mcc", "H4EK", "halo4_mcc"),
        ("H4EK", "halo4_mcc", "H2AMPEK", "halo2amp_mcc"),
    ];

    /// Every group the source profile defines, sorted so a run is repeatable.
    ///
    /// Deliberately not a hand-picked list: checking that one tag of *each* type
    /// carries its data is the whole point, and a shortlist hides the groups
    /// nobody thought to name.
    fn source_groups(index: &GameTagIndex) -> Vec<String> {
        let mut names: Vec<String> = index.by_name.keys().cloned().collect();
        names.sort();
        names
    }

    /// One walk of a kit, bucketed by extension — rescanning per group turned a
    /// measurement into a nine-minute one.
    pub fn first_tag_by_extension(root: &Path) -> HashMap<String, PathBuf> {
        let mut out: HashMap<String, PathBuf> = HashMap::new();
        for path in walk_files(root) {
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
            let ext = ext.to_ascii_lowercase();
            if super::tests::is_generated_output(&path) {
                continue;
            }
            out.entry(ext)
                .and_modify(|best| {
                    if stock_rank(&path) < stock_rank(best) {
                        *best = path.clone();
                    }
                })
                .or_insert(path);
        }
        out
    }

    /// Sort key that prefers stock game content over editor scratch.
    ///
    /// The picker used to take whatever sorted first, which in H2EK means
    /// `digsite/...` — recovered work-in-progress rather than shipped content, and
    /// a poor thing to judge a conversion by. Rank `objects/` first (the object
    /// tags anyone would test), then `levels/`, then the rest, and push anything
    /// that looks like scratch to the back. Ties break alphabetically so a run is
    /// still repeatable.
    fn stock_rank(path: &Path) -> (u8, String) {
        let text = path.to_string_lossy().replace('\\', "/").to_ascii_lowercase();
        let scratch = ["digsite", "/test", "test/", "temp", "scratch", "_old", "unused"]
            .iter()
            .any(|needle| text.contains(needle));
        let tier = if scratch {
            3
        } else if text.contains("/objects/") {
            0
        } else if text.contains("/levels/") {
            1
        } else {
            2
        };
        (tier, text)
    }

    pub fn extension_to_group_tag(index: &GameTagIndex) -> HashMap<String, u32> {
        let mut out = HashMap::new();
        for (tag, name) in &index.by_tag {
            let ext = crate::paths::group_tag_to_extension(*tag).unwrap_or(name.as_str());
            out.insert(ext.to_ascii_lowercase(), *tag);
        }
        out
    }

    /// Convert one tag per group per pair and write it into the target kit, so
    /// the result can be opened in that kit's own tools.
    ///
    /// Everything lands under `<kit>/tags/baboon_converted/<source profile>/`, one
    /// folder to inspect and one folder to delete. Nothing existing is
    /// overwritten. Uses the same save path as the UI —
    /// `prepare_companion_outputs` then `write_atomic` — so companions are named,
    /// their references resolved, dependency lists rebuilt, and every tag
    /// round-trip verified before a byte is written.
    ///
    /// `#[ignore]`d on purpose: this writes into an installed editing kit and
    /// must never happen as a side effect of `cargo test`.
    #[test]
    #[ignore = "writes converted tags into the installed editing kits"]
    fn write_converted_samples_into_kits() {
        let definitions = locate_definitions_root();
        let mut written = 0usize;
        let mut failed = 0usize;
        // `BLAM_TEST_ONLY_PAIR=haloce_mcc` narrows the sweep to legs whose source
        // profile matches, so one leg can be re-run without paying for all six.
        let only_pair = std::env::var("BLAM_TEST_ONLY_PAIR").ok();
        for (source_kit, source_game, target_kit, target_game) in CHAIN {
            if only_pair.as_deref().is_some_and(|want| want != *source_game) {
                continue;
            }
            let (Some(source_tags), Some(target_tags)) = (
                kit_tags(&format!("BLAM_TEST_{source_kit}"), source_kit),
                kit_tags(&format!("BLAM_TEST_{target_kit}"), target_kit),
            ) else {
                eprintln!("== {source_game} -> {target_game}: kit missing, skipped");
                continue;
            };
            let source_index = GameTagIndex::load(&definitions, source_game).unwrap();
            let target_index = GameTagIndex::load(&definitions, target_game).unwrap();
            let by_ext = extension_to_group_tag(&source_index);
            let found = first_tag_by_extension(&source_tags);
            // Built once. `analyze_conversion` rebuilds this on every call,
            // which means walking the whole target kit per group.
            let templates = NativeTemplateIndex::build(&target_tags, &target_index);
            let dependency_schema = definitions.join(target_game).join("tag_dependency_list.json");
            let out_dir = target_tags.join("baboon_converted").join(source_game);
            eprintln!("== {source_game} -> {target_game}  ->  {}", out_dir.display());
            for group in source_groups(&source_index) {
                let lower = group.clone();
                if !source_index.by_name.contains_key(&lower)
                    || !target_index.by_name.contains_key(&lower)
                {
                    continue;
                }
                let (Some(&group_tag), Some(path)) = (by_ext.get(&lower), found.get(&lower)) else {
                    continue;
                };
                let read = std::panic::catch_unwind(|| {
                    read_tag_for_conversion(
                        path,
                        Some(source_game),
                        Some(definitions.as_path()),
                        group_tag,
                    )
                });
                let Ok(Ok(source)) = read else {
                    eprintln!("   {group:34} source unreadable");
                    failed += 1;
                    continue;
                };
                let analyzed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyze_conversion_with_templates(
                        &source,
                        source_game,
                        target_game,
                        &definitions,
                        Some(&templates),
                    )
                }));
                let mut draft = match analyzed {
                    Ok(Ok(draft)) => draft,
                    Ok(Err(error)) => {
                        // Printed in full on purpose. The reason *is* the finding
                        // this sweep exists to produce; truncating it here made the
                        // per-group breakdown unrecoverable from the log.
                        eprintln!("   {group:34} refused: {error}");
                        failed += 1;
                        continue;
                    }
                    Err(_) => {
                        eprintln!("   {group:34} PANICKED");
                        failed += 1;
                        continue;
                    }
                };
                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(&lower);
                let output =
                    out_dir.join(format!("{stem}.{}", draft.target_extension));
                if output.exists() {
                    eprintln!("   {group:34} already present, left alone");
                    continue;
                }
                let saved = (|| -> Result<usize, String> {
                    let companions = prepare_companion_outputs(
                        &mut draft,
                        &output,
                        &target_tags,
                        &dependency_schema,
                    )?;
                    for path in companions.iter().chain(std::iter::once(&output)) {
                        if let Some(parent) = path.parent() {
                            fs::create_dir_all(parent).map_err(|error| {
                                format!("Could not create {}: {error}", parent.display())
                            })?;
                        }
                    }
                    for (companion, path) in draft.companion_tags.iter().zip(&companions) {
                        companion.tag.write_atomic(path).map_err(|error| {
                            format!("Could not save {}: {error}", path.display())
                        })?;
                    }
                    draft.tag.write_atomic(&output).map_err(|error| {
                        format!("Could not save {}: {error}", output.display())
                    })?;
                    Ok(companions.len())
                })();
                match saved {
                    Ok(companions) => {
                        written += 1;
                        let extra = if companions == 0 {
                            String::new()
                        } else {
                            format!(" (+{companions} companion)")
                        };
                        eprintln!("   {group:34} wrote {}{extra}", output.display());
                    }
                    Err(error) => {
                        failed += 1;
                        eprintln!("   {group:34} SAVE FAILED {error}");
                    }
                }
            }
        }
        eprintln!("
{written} tag(s) written, {failed} not converted");
        assert!(written > 0, "nothing was written; are the kits installed?");
    }

    /// Follow one tag all the way along the chain, feeding each conversion's
    /// output in as the next hop's source.
    ///
    /// The adjacent-pair sweep only ever converts kit-authored tags, so it cannot
    /// see loss that compounds: a field dropped at H2 -> H3 is simply absent by
    /// the time Reach is reached, and nothing reports it twice. This is also the
    /// shape a user actually wants — carry a Halo 2 asset forward to Reach — and
    /// it is where a double conversion showed up as a crash once already.
    #[test]
    #[ignore = "measurement; needs the editing kits"]
    fn sweep_the_whole_chain_hop_by_hop() {
        const HOPS: &[(&str, &str, &str)] = &[
            ("H2EK", "halo2_mcc", "biped"),
        ];
        const CHAIN: &[(&str, &str)] = &[
            ("H3EK", "halo3_mcc"),
            ("H3ODSTEK", "halo3odst_mcc"),
            ("HREK", "haloreach_mcc"),
            ("H4EK", "halo4_mcc"),
            ("H2AMPEK", "halo2amp_mcc"),
        ];
        let definitions = locate_definitions_root();
        for (start_kit, start_game, group) in HOPS {
            let Some(start_tags) = kit_tags(&format!("BLAM_TEST_{start_kit}"), start_kit) else {
                eprintln!("skipping: no {start_kit}");
                continue;
            };
            let found = first_tag_by_extension(&start_tags);
            let Some(path) = found.get(*group) else {
                eprintln!("skipping: {start_kit} ships no .{group}");
                continue;
            };
            let index = GameTagIndex::load(&definitions, start_game).unwrap();
            let Some(&group_tag) = index.by_name.get(*group) else { continue };
            let Ok(mut carried) = read_tag_for_conversion(
                path,
                Some(start_game),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                eprintln!("skipping: {} is unreadable", path.display());
                continue;
            };
            eprintln!("== chain for .{group}, from {}", path.display());
            let mut game = *start_game;
            for (kit, next_game) in CHAIN {
                let Some(tags) = kit_tags(&format!("BLAM_TEST_{kit}"), kit) else {
                    eprintln!("   {game} -> {next_game}: no {kit}, chain stops");
                    break;
                };
                let target_index = GameTagIndex::load(&definitions, next_game).unwrap();
                let templates = NativeTemplateIndex::build(&tags, &target_index);
                let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyze_conversion_with_templates(
                        &carried,
                        game,
                        next_game,
                        &definitions,
                        Some(&templates),
                    )
                }));
                match converted {
                    Ok(Ok(draft)) => {
                        eprintln!(
                            "   {game} -> {next_game}  exact={} semantic={} default={} unsupported={} dropped_refs={}",
                            draft.report.copied_exact,
                            draft.report.converted_semantic,
                            draft.report.defaulted_target,
                            draft.report.unsupported_source,
                            draft.report.dropped_references,
                        );
                        carried = draft.tag;
                        game = next_game;
                    }
                    Ok(Err(error)) => {
                        let brief: String = error.chars().take(96).collect();
                        eprintln!("   {game} -> {next_game}  STOPPED {brief}");
                        break;
                    }
                    Err(_) => {
                        eprintln!("   {game} -> {next_game}  PANICKED");
                        break;
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "measurement sweep; needs the editing kits"]
    fn sweep_adjacent_pair_conversions() {
        let definitions = locate_definitions_root();
        for (source_kit, source_game, target_kit, target_game) in CHAIN {
            let (Some(source_tags), Some(target_tags)) = (
                kit_tags(&format!("BLAM_TEST_{source_kit}"), source_kit),
                kit_tags(&format!("BLAM_TEST_{target_kit}"), target_kit),
            ) else {
                eprintln!("== {source_game} -> {target_game}: kit missing, skipped");
                continue;
            };
            let source_index = GameTagIndex::load(&definitions, source_game).unwrap();
            let target_index = GameTagIndex::load(&definitions, target_game).unwrap();
            let by_ext = extension_to_group_tag(&source_index);
            let found = first_tag_by_extension(&source_tags);
            // Built once. `analyze_conversion` rebuilds this on every call,
            // which means walking the whole target kit per group.
            let templates = NativeTemplateIndex::build(&target_tags, &target_index);
            eprintln!("== {source_game} -> {target_game}");
            for group in source_groups(&source_index) {
                let lower = group.clone();
                if !source_index.by_name.contains_key(&lower) {
                    eprintln!("   {group:34} -  source has no such group");
                    continue;
                }
                if !target_index.by_name.contains_key(&lower) {
                    eprintln!("   {group:34} -  TARGET has no such group");
                    continue;
                }
                let (Some(&group_tag), Some(path)) = (by_ext.get(&lower), found.get(&lower)) else {
                    eprintln!("   {group:34} -  kit ships none");
                    continue;
                };
                let read = std::panic::catch_unwind(|| {
                    read_tag_for_conversion(
                        path,
                        Some(source_game),
                        Some(definitions.as_path()),
                        group_tag,
                    )
                });
                let source = match read {
                    Ok(Ok(tag)) => tag,
                    Ok(Err(error)) => {
                        eprintln!("   {group:34} READ FAILED: {error}");
                        continue;
                    }
                    Err(_) => {
                        eprintln!("   {group:34} READ PANICKED");
                        continue;
                    }
                };
                let converted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyze_conversion_with_templates(
                        &source,
                        source_game,
                        target_game,
                        &definitions,
                        Some(&templates),
                    )
                    .map(|draft| {
                        (
                            draft.report.copied_exact,
                            draft.report.converted_semantic,
                            draft.report.defaulted_target,
                            draft.report.unsupported_source,
                            draft.report.dropped_references,
                        )
                    })
                }));
                match converted {
                    Ok(Ok((exact, semantic, default, unsupported, dropped))) => eprintln!(
                        "   {group:34} ok exact={exact} semantic={semantic} default={default} unsupported={unsupported} dropped_refs={dropped}"
                    ),
                    Ok(Err(error)) => {
                        let brief: String = error.chars().take(110).collect();
                        eprintln!("   {group:34} FAILED {brief}");
                    }
                    Err(_) => eprintln!("   {group:34} PANICKED in conversion"),
                }
            }
        }
    }
}

#[cfg(test)]
mod group_alias_regression {
    use super::tests::kit_tags;
    use super::*;

    /// A converted particle must carry the format version the kit writes.
    ///
    /// Halo Reach's particle root has `version!`, which Halo 3 has no counterpart
    /// for. Engine-managed fields were zeroed along with everything else when the
    /// kit-authored template was cleared, so a converted particle claimed version
    /// 0 — a value nothing ships (24 of 25 shipped HREK particles carry 2, one
    /// carries 1) — and the Reach mod tools crashed on it.
    ///
    /// The template is a tag the kit itself wrote, so for a field the source
    /// cannot speak to, its answer is the best one available. Same reasoning
    /// `apply_editing_kit_mcc_header` already records for the file header.
    #[test]
    fn a_particle_converted_into_reach_keeps_an_initialised_version() {
        let (Some(h3), Some(reach)) = (
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
            kit_tags("BLAM_TEST_HREK", "HREK"),
        ) else {
            eprintln!("skipping: needs H3EK and HREK");
            return;
        };
        let definitions = locate_definitions_root();
        let Some(path) = super::tests::tags_with_extension(&h3, "particle").into_iter().next()
        else {
            eprintln!("skipping: H3EK ships no particle");
            return;
        };
        let source = TagFile::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let draft = analyze_conversion(
            &source,
            "halo3_mcc",
            "haloreach_mcc",
            &definitions,
            Some(reach.as_path()),
        )
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));

        let version = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()) == "version")
            .and_then(|field| field.value())
            .and_then(|value| match value {
                TagFieldData::CharInteger(value) => Some(value),
                _ => None,
            })
            .expect("Reach's particle root declares `version`");
        assert_ne!(
            version, 0,
            "{}: a converted particle claims version 0, which no shipped Reach              particle uses and which crashes the mod tools",
            path.display()
        );

        // And the render method has to be there. A shipped Reach particle's
        // `actual shader?` points `definition*` at a render_method_definition
        // tag; Halo 3 keeps its render method inline under other names and
        // cannot supply that reference, so it comes from the kit template.
        // Without it the tag has no shader at all and the mod tools crash.
        let shader = draft
            .tag
            .root()
            .fields()
            .find(|field| clean_field_key(field.name()).starts_with("actual shader"))
            .and_then(|field| field.as_struct())
            .expect("Reach's particle root declares `actual shader?`");
        let definition = shader
            .fields()
            .find(|field| clean_field_key(field.name()) == "definition")
            .and_then(|field| field.value());
        assert!(
            matches!(
                definition,
                Some(TagFieldData::TagReference(ref reference))
                    if reference.group_tag_and_name.is_some()
            ),
            "{}: the converted particle has no render-method definition ({definition:?})",
            path.display()
        );
    }

    /// A tag whose payload cannot cross must not be written at all.
    ///
    /// All three of these were found by converting real Halo 2 tags and opening
    /// the results in the target kit:
    ///
    /// - `.shader` opened in Guerilla as "not a valid tag". Halo 2 authors a
    ///   shader as a template plus a parameter/postprocess table; Halo 3 replaced
    ///   the system with a render method, and the two roots share exactly one
    ///   field name out of 16 and 2. The converted tag carried a name and nothing
    ///   else.
    /// - `.bitmap` came across with no image data, and the same bitmap **crashed**
    ///   the Reach mod tools, because metadata promising mipmaps with no pixels
    ///   behind them reads off the end.
    ///
    /// The shader is still refused: nothing in a field-level mapping can rebuild a
    /// render method from a template reference.
    ///
    /// The bitmap is no longer refused *into Halo 3*, and this test now pins the
    /// better guarantee. The original diagnosis — "opaque bytes only copy when both
    /// struct GUIDs match, and every Halo 2 GUID is zero" — was the mechanism, but
    /// the conclusion drawn from it was wrong: a native H3EK bitmap keeps its pixels
    /// in the same `processed pixel data` blob (699,052 bytes in one), so the bytes
    /// *can* cross once the payload-definition rename is declared. What must still
    /// be refused is Halo 2 straight to Reach, whose `bitmap_data_block_def` has no
    /// `pixels offset` field at all — that is the version that crashed, and going
    /// through Halo 3 is the route that works.
    #[test]
    fn a_halo_2_bitmap_or_shader_is_refused_rather_than_written_unusable() {
        let Some(h2) = kit_tags("BLAM_TEST_H2EK", "H2EK") else {
            eprintln!("skipping: needs H2EK (set BLAM_TEST_H2EK)");
            return;
        };
        let definitions = locate_definitions_root();
        for (group, fourcc, expected, targets) in [
            // Into Halo 3 a bitmap now carries its pixels, so only the direct hop
            // to Reach is refused.
            ("bitmap", "bitm", "explicitly incompatible", &["haloreach_mcc"][..]),
            ("shader", "shad", "explicitly incompatible", &["halo3_mcc", "haloreach_mcc"][..]),
        ] {
            let group_tag = crate::parse_group_tag(fourcc).expect("a group tag");
            // A bitmap with an empty `processed pixel data` has no payload to
            // lose, so it proves nothing — find one that actually carries bytes.
            let mut chosen = None;
            for candidate in super::tests::tags_with_extension(&h2, group) {
                let Ok(tag) = read_tag_for_conversion(
                    &candidate,
                    Some("halo2_mcc"),
                    Some(definitions.as_path()),
                    group_tag,
                ) else {
                    continue;
                };
                if group != "bitmap" || has_nonempty_data(tag.root()) {
                    chosen = Some((candidate, tag));
                    break;
                }
            }
            let Some((path, source)) = chosen else {
                eprintln!("skipping {group}: no H2EK {group} carries a payload");
                continue;
            };
            for target in targets {
                let error = analyze_conversion(
                    &source,
                    "halo2_mcc",
                    target,
                    &definitions,
                    None,
                )
                .err()
                .unwrap_or_else(|| {
                    panic!(
                        "{}: halo2_mcc -> {target} {group} produced a draft; it would not load",
                        path.display()
                    )
                });
                assert!(
                    error.contains(expected),
                    "{group} -> {target}: expected a {expected:?} refusal, got {error}"
                );
            }
        }
    }

    /// A renamed tag class must not take the reference to it down with it.
    ///
    /// Halo 2 calls the class `contrail`; Halo 3 renamed it to `contrail_system`.
    /// An H2 projectile's attachment points at one, and before `group_aliases`
    /// existed the canonical-name lookup found nothing and the reference was
    /// dropped — a real loss on a real tag, reported only as `dropped_refs=1`.
    /// Found by inspecting a converted `battle_rifle_bullet.projectile`.
    #[test]
    fn an_h2_attachment_keeps_its_contrail_reference_as_a_contrail_system() {
        let (Some(h2), Some(h3)) = (
            kit_tags("BLAM_TEST_H2EK", "H2EK"),
            kit_tags("BLAM_TEST_H3EK", "H3EK"),
        ) else {
            eprintln!("skipping: needs H2EK and H3EK (set BLAM_TEST_H2EK / BLAM_TEST_H3EK)");
            return;
        };
        let definitions = locate_definitions_root();
        let group_tag = crate::parse_group_tag("proj").expect("proj is a group tag");

        // Any H2 projectile whose attachment names a contrail will do.
        let mut checked = 0usize;
        for path in super::tests::tags_with_extension(&h2, "projectile") {
            let Ok(source) = read_tag_for_conversion(
                &path,
                Some("halo2_mcc"),
                Some(definitions.as_path()),
                group_tag,
            ) else {
                continue;
            };
            let Some(expected) = attachment_reference(source.root()) else { continue };
            if expected.0 != crate::parse_group_tag("cont").unwrap() {
                continue;
            }
            let draft = analyze_conversion(
                &source,
                "halo2_mcc",
                "halo3_mcc",
                &definitions,
                Some(h3.as_path()),
            )
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            let landed = attachment_reference(draft.tag.root())
                .unwrap_or_else(|| panic!("{}: the attachment reference was dropped", path.display()));
            assert_eq!(
                landed.0,
                crate::parse_group_tag("cntl").unwrap(),
                "{}: contrail should land as contrail_system, got {}",
                path.display(),
                format_group_tag(landed.0)
            );
            assert_eq!(landed.1, expected.1, "{}: the tag path changed", path.display());
            checked += 1;
            break;
        }
        if checked == 0 {
            eprintln!("skipping: no H2EK projectile attaches a contrail");
        }
    }

    /// Whether any `data` field anywhere in the tag holds bytes.
    fn has_nonempty_data(value: TagStruct<'_>) -> bool {
        for field in value.fields() {
            if field.field_type() == TagFieldType::Data
                && matches!(field.value(), Some(TagFieldData::Data(bytes)) if !bytes.is_empty())
            {
                return true;
            }
            if let Some(child) = field.as_struct()
                && has_nonempty_data(child)
            {
                return true;
            }
            if let Some(block) = field.as_block()
                && block.iter().any(has_nonempty_data)
            {
                return true;
            }
        }
        false
    }

    /// `object/attachments[0]/type`, if it is set.
    fn attachment_reference(root: TagStruct<'_>) -> Option<(u32, String)> {
        let object = root
            .fields()
            .find(|field| clean_field_key(field.name()) == "object")
            .and_then(|field| field.as_struct())?;
        let block = object
            .fields()
            .find(|field| clean_field_key(field.name()) == "attachments")
            .and_then(|field| field.as_block())?;
        let element = block.iter().next()?;
        let field = element
            .fields()
            .find(|field| clean_field_key(field.name()) == "type")?;
        match field.value() {
            Some(TagFieldData::TagReference(reference)) => reference.group_tag_and_name,
            _ => None,
        }
    }
}
