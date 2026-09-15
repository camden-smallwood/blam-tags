//! `model_animation_graph` (jmad) animation extraction.
//!
//! `Animation::new(&tag)` walks the user-facing `definitions/animations`
//! block (or `resources/animations` for older inline-layout tags) and
//! pairs each entry with its `tag resource groups[r]/tag_resource/
//! group_members[m]` runtime payload. Each [`AnimationGroup`] carries
//! header metadata + the raw `animation_data` blob; call
//! [`AnimationGroup::decode`] to turn the blob into an
//! [`AnimationClip`] (static + animated tracks + flags + movement),
//! then [`AnimationClip::pose`] composes against a [`Skeleton`] and
//! [`Pose::write_jma`] emits a JMA-family text file.
//!
//! Inheriting jmads (zero local animations, parent reference set) are
//! a normal success: `Animation::len() == 0` with `parent()` non-null.
//!
//! See `project_jmad_extraction_shipped` in auto-memory for the
//! engine-specific blob layouts (H3 hardcoded vs Reach cumulative-sum)
//! and the binary references they were verified against.

pub mod byte_order;
pub mod classic;
pub mod resource;
pub mod codec;
pub mod graph;
pub mod jma;
pub mod name;
pub mod pose;

pub use codec::Codec;
pub use graph::{
    AnimationGraph, GraphAction, GraphActionAnimation, GraphMode, GraphSet, GraphTransition,
    GraphWeaponClass, GraphWeaponType,
};
pub use jma::JmaKind;
pub use name::{base_state_candidates, AnimationName, AnimationStateType};
pub use pose::{NodeTransform, Pose, Skeleton, SkeletonNode};

use crate::api::TagStruct;
use crate::fields::TagFieldData;
use crate::file::TagFile;
use crate::math::{RealPoint3d, RealQuaternion};

/// Errors returned by [`Animation::new`] / [`AnimationGroup::decode`].
#[derive(Debug)]
pub enum AnimationError {
    /// Root struct doesn't expose the fields we expect from a jmad
    /// (no `definitions/animations` block).
    NotAnAnimationGraph,
    /// Animation has no codec payload (empty `animation_data` blob).
    /// Either the animation is inherited or the tag is malformed.
    NoCodecPayload,
    /// First byte of the codec stream isn't a recognized
    /// `e_animation_codec_types` value (0..=11).
    UnknownCodec(u8),
    /// Codec recognized but no decoder implemented for this slot.
    UnsupportedCodec(Codec),
    /// Codec stream is shorter than the codec's required header bytes.
    TruncatedHeader { codec: Codec, want: usize, have: usize },
    /// A computed slice into the codec stream goes past the blob's
    /// end. Common cause: the codec header's offsets disagree with
    /// the engine's actual layout.
    TruncatedPayload { codec: Codec, want_end: usize, blob_size: usize },
}

impl std::fmt::Display for AnimationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnAnimationGraph => write!(f, "tag is not a recognizable model_animation_graph (missing `definitions/animations`)"),
            Self::NoCodecPayload => write!(f, "animation has no codec payload (empty blob)"),
            Self::UnknownCodec(b) => write!(f, "unknown codec byte 0x{b:02x} (expected 0..=11)"),
            Self::UnsupportedCodec(c) => write!(f, "codec {c:?} not yet supported"),
            Self::TruncatedHeader { codec, want, have } =>
                write!(f, "{codec:?} header: need {want} bytes, blob has {have}"),
            Self::TruncatedPayload { codec, want_end, blob_size } =>
                write!(f, "{codec:?} payload: slice ends at {want_end} but blob is {blob_size} bytes"),
        }
    }
}

impl std::error::Error for AnimationError {}

/// Per-animation `data sizes` breakdown — the engine's record of how
/// the `animation_data` blob is internally partitioned. Useful for
/// sanity-checking total length against the sum of subsections.
///
/// Held as a name/value list rather than a fixed struct because the
/// shape varies by engine: H3 uses `packed_data_sizes_struct` (16
/// bytes, mixed i8/i16/i32), Reach widens to a `_reach` variant with
/// 17+ i32 fields (`blend_screen_data`, `object_space_offset_data`,
/// `ik_chain_*`, `uncompressed_object_space_data`, …). All fields
/// are byte counts and decode losslessly to i64.
#[derive(Debug, Clone, Default)]
pub struct PackedDataSizes {
    pub fields: Vec<(String, i64)>,
}

impl PackedDataSizes {
    /// Sum of all subsection bytes — what the blob length should equal
    /// in a well-formed tag.
    pub fn total(&self) -> i64 {
        self.fields.iter().map(|(_, v)| *v).sum()
    }

    /// Lookup a named subsection size. Returns 0 if absent.
    pub fn get(&self, name: &str) -> i64 {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| *v).unwrap_or(0)
    }

    /// Which engine's `data sizes` struct this is. H3 uses
    /// `packed_data_sizes_struct` (0x10 bytes, 7 mixed-width fields);
    /// Reach widens to `packed_data_sizes_reach_struct` (0x44 bytes,
    /// 17 i32 fields adding `blend_screen_data`, `object_space_*`,
    /// `ik_chain_*`, `compressed_event_curve`, etc.). The presence of
    /// any of those Reach-only fields tells us we're reading a
    /// Reach-style blob, where the schema field NAMES are kept but
    /// their SEMANTICS shift (e.g. `static_node_flags` becomes the
    /// static-codec byte size, not the static-flag byte size).
    pub fn layout(&self) -> SizeLayout {
        // Halo 2 is built by `read_h2_data_sizes` with a leading
        // `h2_static_data` marker field; its sections are ordered
        // identically to Reach (codec/codec/flags/flags/movement/pill),
        // so the decoder treats it positionally like Reach.
        if self.fields.first().map(|(n, _)| n == "h2_static_data").unwrap_or(false) {
            return SizeLayout::Halo2;
        }
        let reach_only = ["blend_screen_data", "object_space_offset_data", "ik_chain_event_data"];
        if self.fields.iter().any(|(n, _)| reach_only.iter().any(|r| r == n)) {
            SizeLayout::Reach
        } else {
            SizeLayout::H3
        }
    }
}

/// Engine-version flavor of the `data sizes` struct. Determines how
/// the blob's sections are addressed: H3 packs the static codec
/// stream first then a single animated codec; Reach uses the same
/// names but with different meanings (the `static_node_flags` field
/// is repurposed to carry the static codec stream's byte size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeLayout {
    /// Pre-Reach (H3, ODST). 7 fields, mixed widths.
    H3,
    /// Reach onward. 17 fields, all i32. Field names kept the same,
    /// but several now carry codec/flag sizes rather than what the
    /// names suggest — see the Reach arms of the size accessors below.
    Reach,
    /// Halo 2 (`jmad`). The on-disk `data sizes` are stored either as
    /// a 7-field unnamed struct (pool-block v1/v2) or as separate inline
    /// fields (pool-block v0); `read_h2_data_sizes` normalizes both to
    /// the positional order `[static_codec, animated_codec, static_flags,
    /// animated_flags, movement, pill]`. That order matches Reach's, so
    /// the codec decoder addresses Halo 2 with the same positional logic
    /// as [`Self::Reach`].
    Halo2,
}

/// One animation entry — header metadata from `animations[i]` joined
/// with the matching `group_members[m]` runtime payload.
#[derive(Debug)]
pub struct AnimationGroup<'a> {
    /// Index in `definitions/animations[]`.
    pub index: usize,
    /// Resolved `name` string-id.
    pub name: Option<String>,
    /// `animation type` enum name (`base`, `overlay`, `replacement`,
    /// `world`, …).
    pub animation_type: Option<String>,
    /// `frame info type` enum name from `animations[i]`. The matching
    /// resource group_member's `movement_data_type` should agree;
    /// disagreement is surfaced via [`Self::movement_type_mismatch`].
    pub frame_info_type: Option<String>,
    /// Engine-recorded `frame count` (from `animations[i]`).
    pub frame_count: i16,
    /// Engine-recorded `node count` (from `animations[i]`).
    pub node_count: i8,
    /// `node list checksum` from `animations[i]` — used to verify the
    /// jmad targets the same skeleton as the consuming render_model.
    pub node_list_checksum: i32,
    /// `(resource_group, resource_group_member)` pair from
    /// `animations[i]`. `(-1, -1)` is a valid sentinel meaning the
    /// animation has no local payload (inherited).
    pub resource_group: i16,
    pub resource_group_member: i16,
    /// `animation_checksum` from the matching group_member. `None` if
    /// the group_member couldn't be resolved.
    pub checksum: Option<i32>,
    /// `frame count*` from the matching group_member — the codec
    /// stream's frame count, which can differ from the animation's
    /// user-visible `frame_count` (notably for slot-8 BlendScreen
    /// animations, where the codec count is the number of variants
    /// in the screen, not the playback length). Falls back to
    /// `frame_count` when the group_member isn't resolved.
    pub codec_frame_count: Option<i16>,
    /// `movement_data_type` enum name from the matching group_member.
    pub movement_type: Option<String>,
    /// `data sizes` struct from the matching group_member.
    pub data_sizes: Option<PackedDataSizes>,
    /// First byte of the `animation_data` blob — the codec enum value
    /// per `e_animation_codec_types` (0..=8 for Halo 3, 0..=11 for
    /// Reach+). `None` if blob is empty or missing.
    pub codec_byte: Option<u8>,
    /// Raw `animation_data` blob bytes. Empty slice if the
    /// group_member is missing or its data field is empty.
    pub blob: &'a [u8],
    /// `internal flags / world relative` (bit 1). When set on a
    /// `animation_type=base` animation, the export uses the JMW
    /// extension. The schema doesn't expose "world" as a type enum
    /// value; this bit is the only signal — same convention as
    /// TagTool's `GetAnimationExtension(..., worldRelative)` and
    /// Foundry's `internal_flags.TestBit("world relative")`.
    pub world_relative: bool,
    /// `object-space parent nodes` — per-overlay 3D pose-overlay data
    /// (empty for H3, common in Reach/H4). Each entry pins a node (and
    /// its descendants) to a fixed object-space orientation after overlay
    /// composition; applied by the JMA writer's object-space correction.
    /// Empty ⇒ not a 3D pose overlay (Foundry's `is_pose_overlay` false).
    pub object_space_parents: Vec<ObjectSpaceParentNode>,
    /// Halo 4 graph-level **shared static** value pool (`codec data/
    /// shared_static_codec`). The H4 static rest pose isn't in the
    /// per-animation blob — `SharedStatic` (codec 11) stores only int16
    /// indices into this graph-shared pool. Shared (cheap `Rc` clone)
    /// across all groups of one tag; `None` for non-H4 / tags without it.
    /// See [`AnimationGroup::decode`]'s shared-static path.
    pub shared_static: Option<std::rc::Rc<SharedStaticPool>>,
}

impl<'a> AnimationGroup<'a> {
    /// A group standing for nothing but a blob, so it can be decoded on its own.
    ///
    /// The converter uses this to read a 360 animation back after turning it
    /// round: a blob that decodes into the tracks its header promises is the
    /// only check on the byte order short of playing it. Every field the
    /// decoder does not consult is left at nothing.
    pub fn for_blob(
        blob: &'a [u8],
        data_sizes: Option<PackedDataSizes>,
        frame_count: i16,
        node_count: i8,
        movement_type: Option<String>,
    ) -> Self {
        Self {
            index: 0,
            name: None,
            animation_type: None,
            frame_info_type: movement_type.clone(),
            frame_count,
            node_count,
            node_list_checksum: 0,
            resource_group: -1,
            resource_group_member: -1,
            checksum: None,
            codec_frame_count: Some(frame_count),
            movement_type,
            data_sizes,
            codec_byte: blob.first().copied(),
            blob,
            world_relative: false,
            object_space_parents: Vec::new(),
            shared_static: None,
        }
    }
}

/// Halo 4's graph-level shared-static value pool — decoded once from
/// `codec data/shared_static_codec/{rotations,translations,scale}` on the
/// `model_animation_graph`. The per-animation `compressed_static_pose`
/// codec stream (codec 11) holds int16 indices into these vectors.
/// Rotations are 4×int16/0x7FFF quaternions; translations/scales are
/// raw f32. RE'd from the H4 Xbox debug build (`c_shared_static_data_codec`).
#[derive(Debug, Default)]
pub struct SharedStaticPool {
    pub rotations: Vec<RealQuaternion>,
    pub translations: Vec<RealPoint3d>,
    pub scales: Vec<f32>,
}

/// One `object-space parent nodes` entry: a node whose object-space
/// orientation is pinned to `(translation, rotation, scale)` for a pose
/// overlay. Rotation is dequantized from four int16s (`value / 0x7FFF`).
/// Translation is in world units (metres) — the JMA `×100` happens at
/// write time. Mirrors Foundry's
/// `_object_space_parent_orientation_transform`.
#[derive(Debug, Clone, Copy)]
pub struct ObjectSpaceParentNode {
    /// Skeleton node index this entry targets.
    pub node_index: i16,
    pub translation: RealPoint3d,
    pub rotation: RealQuaternion,
    pub scale: f32,
}

impl<'a> AnimationGroup<'a> {
    /// True when the per-animation `frame info type` and the
    /// group_member's `movement_data_type` disagree. Indicates the
    /// tag was authored against a different schema than what the
    /// consumer expects — usually benign but worth flagging in sweeps.
    pub fn movement_type_mismatch(&self) -> bool {
        match (&self.frame_info_type, &self.movement_type) {
            (Some(a), Some(b)) => a != b,
            _ => false,
        }
    }

    /// Codec byte for the **animated** stream that follows the static
    /// rest-pose stream. Engine-aware: H3 reads `default_data` (the
    /// static codec stream's size = animated stream offset); Reach
    /// reads positional index 0 (= `static_node_flags` field, but
    /// semantically the static codec stream's size in Reach's
    /// repurposed field naming). When the offset is 0 there's no
    /// static stream at all, so the byte at offset 0 IS the animated
    /// codec — return that.
    pub fn animated_codec_byte(&self) -> Option<u8> {
        let sizes = self.data_sizes.as_ref()?;
        let off = match sizes.layout() {
            SizeLayout::H3 => sizes.get("default_data") as usize,
            SizeLayout::Reach | SizeLayout::Halo2 =>
                sizes.fields.first().map(|(_, v)| *v as usize).unwrap_or(0),
        };
        self.blob.get(off).copied()
    }
}

/// All animations in a jmad, paired with their per-animation payload
/// from the tag's resource groups. Construct via [`Animation::new`].
pub struct Animation<'a> {
    animations: Vec<AnimationGroup<'a>>,
    parent: Option<String>,
}

/// Top-level struct name varies by jmad layout version: newer tags
/// call it `definitions`, older ones (e.g. mongoose, layout version
/// ~4601) call it `resources`. Try both.
const TOP_LEVEL_NAMES: &[&str] = &["definitions", "resources"];

impl<'a> Animation<'a> {
    /// Walk a parsed `model_animation_graph` tag and pair each
    /// per-animation entry with its `tag resource groups[]` runtime
    /// payload. Inheriting tags (zero local animations + non-null
    /// `parent animation graph`) succeed with `len() == 0` and a
    /// non-`None` [`Self::parent`].
    pub fn new(tag: &'a TagFile) -> Result<Self, AnimationError> {
        let root = tag.root();
        let game = crate::game::Game::of(tag);

        let top_prefix = TOP_LEVEL_NAMES.iter().copied()
            .find(|name| root.field_path(&format!("{name}/animations")).is_some())
            .ok_or(AnimationError::NotAnAnimationGraph)?;

        let animations_block = root
            .field_path(&format!("{top_prefix}/animations"))
            .and_then(|f| f.as_block())
            .ok_or(AnimationError::NotAnAnimationGraph)?;

        // Pre-walk all `tag resource groups[]` entries. Each one is a
        // `tag_resource` field; its `as_struct()` exposes a
        // `group_members` block whose elements carry the per-animation
        // payload. Build a (resource_group, resource_group_member) →
        // group_member-struct lookup so the per-animation join below
        // stays O(1) per lookup.
        let resource_groups_block = root
            .field_path("tag resource groups")
            .and_then(|f| f.as_block());

        let mut group_member_table: Vec<Option<Vec<TagStruct<'a>>>> = Vec::new();
        if let Some(rg) = resource_groups_block {
            for r in 0..rg.len() {
                let elem = match rg.element(r) {
                    Some(e) => e,
                    None => { group_member_table.push(None); continue; }
                };
                let resource = elem.field("tag_resource").and_then(|f| f.as_resource());
                let header = match resource.and_then(|r| r.as_struct()) {
                    Some(h) => h,
                    None => { group_member_table.push(None); continue; }
                };
                let members_block = header.field("group_members").and_then(|f| f.as_block());
                let members = match members_block {
                    Some(b) => (0..b.len()).filter_map(|i| b.element(i)).collect(),
                    None => Vec::new(),
                };
                group_member_table.push(Some(members));
            }
        }

        // Halo 4 graph-level shared-static value pool (read once; shared
        // across every animation in this tag via a cheap `Rc` clone).
        let shared_static = read_shared_static_pool(&root).map(std::rc::Rc::new);

        // Walk per-animation entries.
        let mut animations = Vec::with_capacity(animations_block.len());
        for i in 0..animations_block.len() {
            let anim = match animations_block.element(i) {
                Some(a) => a,
                None => continue,
            };
            let name = anim.read_string_id("name");

            // Reach moved most per-animation codec metadata into a
            // nested `shared animation data[0]` block. H3-era tags keep
            // it on the outer `animations[i]` struct directly. Pick
            // whichever struct actually carries the metadata fields.
            let metadata = anim
                .field("shared animation data")
                .and_then(|f| f.as_block())
                .and_then(|b| b.element(0))
                .filter(|s| s.field("resource_group").is_some()
                         || s.field("frame count").is_some())
                .unwrap_or(anim);

            // Older layouts (e.g. mongoose, version ~4601) call this
            // field `type`; newer ones name it `animation type`.
            let animation_type = metadata.read_enum_name("animation type")
                .or_else(|| metadata.read_enum_name("type"));
            let frame_info_type = metadata.read_enum_name("frame info type");
            let frame_count = metadata.read_int_any("frame count").unwrap_or(0) as i16;
            let node_count = metadata.read_int_any("node count").unwrap_or(0) as i8;
            let node_list_checksum = metadata.read_int_any("node list checksum").unwrap_or(0) as i32;
            let resource_group = metadata.read_int_any("resource_group").unwrap_or(-1) as i16;
            let resource_group_member = metadata.read_int_any("resource_group_member").unwrap_or(-1) as i16;
            // `internal flags` bit 1 = "world relative". The schema's
            // `animation type` enum has only base/overlay/replacement;
            // JMW (world-relative base) is selected here, not from a
            // type enum value. Matches Foundry's `internal_flags
            // .TestBit("world relative")` lookup and TagTool's
            // `GetAnimationExtension(type, frame_info, worldRelative)`
            // boolean parameter.
            let internal_flags = metadata.read_int_any("internal flags").unwrap_or(0) as u32;
            let world_relative = (internal_flags >> 1) & 1 == 1;

            let (mut checksum, mut codec_frame_count, mut movement_type, mut data_sizes, mut codec_byte, mut blob) =
                resolve_member(&group_member_table, resource_group, resource_group_member);

            // Inline payload — older layouts skip the tgrc resource and
            // store `animation data` / `data sizes` directly on each
            // animation block element. Try inline only when the
            // resource lookup didn't find anything.
            if blob.is_empty() && data_sizes.is_none() {
                let inline_blob = if game == crate::game::Game::Halo2 {
                    read_h2_animation_data(&metadata)
                } else {
                    read_inline_animation_data(&metadata)
                };
                if let Some(inline_blob) = inline_blob {
                    blob = inline_blob;
                    codec_byte = blob.first().copied();
                }
                // Halo 2 stores the section sizes either as an unnamed
                // 7-field `data sizes` struct (pool-block v1/v2) or as
                // separate inline fields (v0); both need positional/
                // explicit-name handling rather than the H3 named lookup.
                data_sizes = if game == crate::game::Game::Halo2 {
                    read_h2_data_sizes(&metadata)
                } else {
                    read_packed_data_sizes(&metadata)
                };
                if movement_type.is_none() {
                    movement_type = frame_info_type.clone();
                }
                if checksum.is_none() {
                    checksum = metadata.read_int_any("production checksum").map(|v| v as i32);
                }
                if codec_frame_count.is_none() {
                    codec_frame_count = Some(frame_count);
                }
            }

            let object_space_parents = read_object_space_parents(&metadata);

            animations.push(AnimationGroup {
                index: i,
                name,
                animation_type,
                frame_info_type,
                frame_count,
                node_count,
                node_list_checksum,
                resource_group,
                resource_group_member,
                checksum,
                codec_frame_count,
                movement_type,
                data_sizes,
                codec_byte,
                blob,
                world_relative,
                object_space_parents,
                shared_static: shared_static.clone(),
            });
        }

        // Parent reference for the inheritance case. Same prefix as
        // the animations block.
        let parent = root
            .field_path(&format!("{top_prefix}/parent animation graph"))
            .and_then(|f| f.value())
            .and_then(|v| match v {
                TagFieldData::TagReference(r) => r.group_tag_and_name.map(|(_, p)| p),
                _ => None,
            })
            .filter(|p| !p.is_empty());

        Ok(Self { animations, parent })
    }

    /// Number of animations in `definitions/animations[]`. Zero on
    /// inheriting tags.
    pub fn len(&self) -> usize { self.animations.len() }

    /// `true` when there are no local animations (commonly because
    /// the tag inherits from [`Self::parent`]).
    pub fn is_empty(&self) -> bool { self.animations.is_empty() }

    /// Iterate every animation group in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = &AnimationGroup<'a>> {
        self.animations.iter()
    }

    /// Look up an animation by index into `definitions/animations[]`.
    pub fn get(&self, index: usize) -> Option<&AnimationGroup<'a>> {
        self.animations.get(index)
    }

    /// Look up an animation by its resolved string-id name.
    pub fn find(&self, name: &str) -> Option<&AnimationGroup<'a>> {
        self.animations.iter().find(|a| a.name.as_deref() == Some(name))
    }

    /// Path of `definitions/parent animation graph` if non-null.
    /// Inheriting jmads with `len() == 0` will typically have this set.
    pub fn parent(&self) -> Option<&str> { self.parent.as_deref() }

    /// Count of animations whose `(resource_group, resource_group_member)`
    /// didn't resolve. In a typical jmad this is 0; inheriting tags
    /// may have many.
    pub fn unresolved_count(&self) -> usize {
        self.animations.iter().filter(|a| a.checksum.is_none()).count()
    }

    /// Resolve the composition base pose for an overlay/replacement
    /// `group` — the first frame of the matching base animation the
    /// overlay's deltas were authored against. Returns `None` to mean
    /// "fall back to the rest/bind pose" (no scope, custom name, damage/
    /// transition state, `aim_spine` pose overlay, or no matching base
    /// found).
    ///
    /// Mirrors Foundry's `_get_base_animation_candidates` +
    /// `_build_animation(base).first_frame()`: parse the overlay's
    /// `(mode, weapon_class, weapon_type, state)` scope, walk
    /// [`base_state_candidates`] in priority order, and resolve each
    /// against the [`AnimationGraph`]'s `actions` (which already does
    /// Halo's per-level `any` fallback). The first base/none-type
    /// animation found is decoded and its frame 0 returned. Both TagTool
    /// and Foundry compose overlays/replacements onto this base, not the
    /// bind pose — composing onto the bind pose is what makes extracted
    /// overlays explode.
    pub fn overlay_base_pose(
        &self,
        graph: &AnimationGraph,
        group: &AnimationGroup<'a>,
        skeleton: &Skeleton,
        defaults: &[NodeTransform],
    ) -> Option<Vec<NodeTransform>> {
        let name = AnimationName::parse(group.name.as_deref()?);
        if !name.valid || name.custom || name.state_type != AnimationStateType::Action {
            return None;
        }
        // POSE_OVERLAY_REST_BASE_STATES — `aim_spine` pose overlays
        // compose against rest, not a base animation. (Foundry also
        // gates this on `is_pose_overlay`; `aim_spine` overlays are
        // always pose overlays in practice.)
        if name.state == "aim_spine" {
            return None;
        }

        for state in base_state_candidates(&name.state) {
            let Some(act) = graph.find_action(
                &name.mode,
                &name.weapon_class,
                &name.weapon_type,
                &name.set,
                &state,
            ) else {
                continue;
            };
            if !act.is_local() || act.animation_index < 0 {
                continue;
            }
            let idx = act.animation_index as usize;
            if idx == group.index {
                continue;
            }
            let Some(base_group) = self.get(idx) else { continue };
            // Only base/none-type animations are valid composition bases
            // (Foundry's `_resolved_base_candidates` filter).
            if matches!(base_group.animation_type.as_deref(), Some("overlay") | Some("replacement")) {
                continue;
            }
            let Ok(base_clip) = base_group.decode() else { continue };
            // Frame 0 of the base, posed against the rest defaults.
            // Movement is irrelevant to frame 0 (it accumulates from 0).
            if let Some(first) = base_clip.pose(skeleton, Some(defaults)).frames.into_iter().next() {
                return Some(first);
            }
        }
        None
    }
}

fn resolve_member<'a>(
    table: &[Option<Vec<TagStruct<'a>>>],
    rg: i16,
    rgm: i16,
) -> (Option<i32>, Option<i16>, Option<String>, Option<PackedDataSizes>, Option<u8>, &'a [u8]) {
    if rg < 0 || rgm < 0 {
        return (None, None, None, None, None, &[]);
    }
    let Some(Some(members)) = table.get(rg as usize) else {
        return (None, None, None, None, None, &[]);
    };
    let Some(member) = members.get(rgm as usize) else {
        return (None, None, None, None, None, &[]);
    };

    let checksum = member.read_int_any("animation_checksum").map(|v| v as i32);
    let codec_frame_count = member.read_int_any("frame count").map(|v| v as i16);
    let movement_type = member.read_enum_name("movement_data_type");
    let data_sizes = read_packed_data_sizes(member);
    let blob = member.field("animation_data").and_then(|f| f.as_data()).unwrap_or(&[]);
    let codec_byte = blob.first().copied();

    (checksum, codec_frame_count, movement_type, data_sizes, codec_byte, blob)
}

/// Inline `animation data` field on an `animations[i]` element —
/// the older layout's storage spot. Tries underscore and space
/// variants of the field name.
fn read_inline_animation_data<'a>(anim: &TagStruct<'a>) -> Option<&'a [u8]> {
    anim.field("animation data").and_then(|f| f.as_data())
        .or_else(|| anim.field("animation_data").and_then(|f| f.as_data()))
}

/// Parse the `object-space parent nodes` block from an animation's
/// metadata struct (empty for H3; populated for Reach/H4 pose overlays).
/// Each element has a node index, component flags (ignored — we always
/// apply the full orientation), and a quantized orientation
/// (int16 rotation x/y/z/w `/ 0x7FFF`, real translation, real scale).
fn read_object_space_parents(metadata: &TagStruct<'_>) -> Vec<ObjectSpaceParentNode> {
    let Some(block) = metadata
        .field("object-space parent nodes")
        .and_then(|f| f.as_block())
    else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(block.len());
    for i in 0..block.len() {
        let Some(elem) = block.element(i) else { continue };
        let node_index = elem
            .read_int_any("node_index")
            .or_else(|| elem.read_int_any("node index"))
            .unwrap_or(-1) as i16;
        let Some(orient) = elem
            .field("parent orientation")
            .or_else(|| elem.field("orientation"))
            .and_then(|f| f.as_struct())
        else {
            continue;
        };
        let q = |name: &str| orient.read_int_any(name).unwrap_or(0) as f32 / 32767.0;
        let mut rotation = RealQuaternion {
            i: q("rotation x"),
            j: q("rotation y"),
            k: q("rotation z"),
            w: q("rotation w"),
        };
        rotation = if rotation.length() <= 1e-6 {
            RealQuaternion::IDENTITY
        } else {
            rotation.normalized()
        };
        out.push(ObjectSpaceParentNode {
            node_index,
            translation: orient.read_point3d("default translation"),
            rotation,
            scale: orient.read_real("default scale").unwrap_or(1.0),
        });
    }
    out
}

fn read_packed_data_sizes(member: &TagStruct<'_>) -> Option<PackedDataSizes> {
    let s = member.field("data sizes").and_then(|f| f.as_struct())?;
    let mut fields = Vec::new();
    for f in s.fields() {
        let name = f.name().to_string();
        if let Some(v) = s.read_int_any(&name) {
            fields.push((name, v as i64));
        }
    }
    Some(PackedDataSizes { fields })
}

/// Build the codec decoder's positional [`PackedDataSizes`] from a Halo 2
/// `animation_pool_block` element, normalizing the two on-disk shapes:
///
/// - **pool-block v1/v2** carry an unnamed 7-field `data sizes` struct in
///   the order `StaticNodeFlags(b) AnimatedNodeFlags(b) MovementData(s)
///   PillOffsetData(s) StaticDataSize(s) UncompressedDataSize(i)
///   CompressedDataSize(i)` (TagTool `Gen2.ModelAnimationGraph.
///   PackedDataSizesStructBlock`);
/// - **pool-block v0** stores the same values as separate inline fields
///   (`static node flag data size`, `animated node flag data size`,
///   `movement_data size`, `default_data size`, `uncompressed_data size`,
///   `compressed_data size`).
///
/// Both are emitted in the positional order the codec decoder expects for
/// [`SizeLayout::Halo2`]: `[static_codec, animated_codec, static_flags,
/// animated_flags, movement, pill]`, where `animated_codec = uncompressed
/// + compressed` (the animated stream is one codec, either form). The
/// leading `h2_static_data` name is the marker [`PackedDataSizes::layout`]
/// keys on. Blob section order verified against TagTool's
/// `AnimationResourceData.Read` (static codec → animated codec → static
/// flags → animated flags → movement).
/// Read a Halo 2 `animation_pool_block` element's `animation data` blob.
/// Tries the named field first, then falls back to the element's sole
/// `data`-typed field. The named lookup succeeds against the current
/// definitions, but the `find_map` fallback guards against a re-dump:
/// the versioned-layout generator emits the base `animation_pool_block`
/// (pool-block v5) with this field's name NULL, and it is the only
/// `data` field on the element. (The shipped def has the name patched
/// back in — see the matching `data sizes` note in `read_h2_data_sizes`.)
fn read_h2_animation_data<'a>(anim: &TagStruct<'a>) -> Option<&'a [u8]> {
    anim.field("animation data").and_then(|f| f.as_data())
        .or_else(|| anim.field("animation_data").and_then(|f| f.as_data()))
        .or_else(|| anim.fields().find_map(|f| f.as_data()))
}

/// Read Halo 4's graph-level shared-static value pool from
/// `codec data/shared_static_codec/{rotations,translations,scale}`.
/// Returns `None` when the field is absent (non-H4 / no shared-static).
/// Rotations are 4×int16/0x7FFF quaternions (`c_quantized_quaternion_8byte`),
/// translations are `(x,y,z)` f32, scales are f32.
fn read_shared_static_pool(root: &TagStruct<'_>) -> Option<SharedStaticPool> {
    let base = "codec data/shared_static_codec";
    let rotations = root
        .field_path(&format!("{base}/rotations"))
        .and_then(|f| f.as_block())?;
    let mut pool = SharedStaticPool::default();
    for i in 0..rotations.len() {
        let Some(e) = rotations.element(i) else { continue };
        let g = |n: &str| e.read_int_any(n).unwrap_or(0) as f32 / 32767.0;
        let q = RealQuaternion { i: g("i"), j: g("j"), k: g("k"), w: g("w") };
        pool.rotations.push(if q.length() <= 1e-6 { RealQuaternion::IDENTITY } else { q.normalized() });
    }
    if let Some(b) = root.field_path(&format!("{base}/translations")).and_then(|f| f.as_block()) {
        for i in 0..b.len() {
            let Some(e) = b.element(i) else { continue };
            pool.translations.push(RealPoint3d {
                x: e.read_real("x").unwrap_or(0.0),
                y: e.read_real("y").unwrap_or(0.0),
                z: e.read_real("z").unwrap_or(0.0),
            });
        }
    }
    if let Some(b) = root.field_path(&format!("{base}/scale")).and_then(|f| f.as_block()) {
        for i in 0..b.len() {
            if let Some(e) = b.element(i) {
                pool.scales.push(e.read_real("scale").unwrap_or(1.0));
            }
        }
    }
    Some(pool)
}

fn read_h2_data_sizes(anim: &TagStruct<'_>) -> Option<PackedDataSizes> {
    let build = |static_data: i64, animated: i64, static_flags: i64,
                 animated_flags: i64, movement: i64, pill: i64| {
        PackedDataSizes {
            fields: vec![
                ("h2_static_data".into(), static_data),
                ("h2_animated_data".into(), animated),
                ("h2_static_flags".into(), static_flags),
                ("h2_animated_flags".into(), animated_flags),
                ("h2_movement".into(), movement),
                ("h2_pill".into(), pill),
            ],
        }
    };

    // The animated codec stream that the engine plays (and TagTool
    // extracts) is the COMPRESSED block, laid out right after the static
    // block: `[static][compressed][static_flags][animated_flags][movement]
    // [pill][uncompressed]`. The trailing `uncompressed` block is an
    // unplayed lossless mirror — NOT part of the animated codec stream and
    // NOT counted toward the flag offset. So the positional "animated
    // data" size we hand the codec decoder is `compressed` alone (falling
    // back to `uncompressed` for the rare compressed-less animation, where
    // the uncompressed block takes the compressed block's slot). Summing
    // the two was the bug that put the flag offset 14 KB into the trailing
    // mirror, scrambling every node's transform.
    let animated_stream = |uncompressed: i64, compressed: i64| {
        if compressed > 0 { compressed } else { uncompressed }
    };

    // v1-v5: the `data sizes` struct, read positionally. The named
    // lookup works against the current def; the `find_map` fallback
    // guards against a re-dump — the versioned-layout generator emits
    // the base `animation_pool_block` (pool-block v5) with this struct's
    // name NULL (we patched the name back into the shipped def), and it
    // is the element's sole `struct`-typed field. (v0 has no struct
    // here; its sizes are separate inline fields, so the `find_map`
    // returns `None` and we drop to the v0 branch below.)
    if let Some(s) = anim.field("data sizes").and_then(|f| f.as_struct())
        .or_else(|| anim.fields().find_map(|f| f.as_struct()))
    {
        let vals: Vec<i64> = s.fields().filter_map(|f| int_value(f.value()?)).collect();
        if vals.len() >= 7 {
            let (static_flags, animated_flags, movement, pill, static_data, uncompressed, compressed) =
                (vals[0], vals[1], vals[2], vals[3], vals[4], vals[5], vals[6]);
            return Some(build(static_data, animated_stream(uncompressed, compressed),
                static_flags, animated_flags, movement, pill));
        }
    }

    // v0: separate inline size fields.
    let g = |n: &str| anim.read_int_any(n).map(|v| v as i64);
    let static_flags = g("static node flag data size")?;
    let animated_flags = g("animated node flag data size")?;
    let movement = g("movement_data size").unwrap_or(0);
    let static_data = g("default_data size").unwrap_or(0);
    let uncompressed = g("uncompressed_data size").unwrap_or(0);
    let compressed = g("compressed_data size").unwrap_or(0);
    Some(build(static_data, animated_stream(uncompressed, compressed),
        static_flags, animated_flags, movement, 0))
}

/// Extract an integer from any integer-shaped [`TagFieldData`] variant.
/// Used to read the Halo 2 `data sizes` struct's *unnamed* fields by
/// position (where [`TagStruct::read_int_any`]'s name lookup can't help).
pub(crate) fn int_value(v: TagFieldData) -> Option<i64> {
    Some(match v {
        TagFieldData::CharInteger(x) => x as i64,
        TagFieldData::ShortInteger(x) => x as i64,
        TagFieldData::LongInteger(x) => x as i64,
        TagFieldData::Int64Integer(x) => x,
        TagFieldData::ByteInteger(x) => x as i64,
        TagFieldData::WordInteger(x) => x as i64,
        TagFieldData::DwordInteger(x) => x as i64,
        TagFieldData::QwordInteger(x) => x as i64,
        _ => return None,
    })
}


//================================================================================
// Decoded data types
//
// Produced by the codec module, consumed by the pose composer and the
// JMA writer. Public API surface, so they live here at the parent
// module rather than inside `codec`.
//================================================================================

/// One codec stream's worth of decoded transforms, indexed by
/// `[codec_node_index][frame_index]`. The outer index is the codec's
/// own node enumeration (as counted by `total_*_nodes` in the codec
/// header) — NOT the global skeleton node index. Composition with the
/// per-bone flag bitarrays maps back to skeleton nodes; that step
/// happens in [`AnimationClip::pose`].
///
/// Translations are in **codec-native units** (raw `real_point3d`
/// floats — no `×100` JMA convention applied here). Rotations are
/// normalized quaternions. Scales are raw f32. `frame_count` is `1`
/// for static codecs; it's the animation's frame count for animated
/// codecs.
#[derive(Debug, Clone)]
pub struct AnimationTracks {
    pub codec: Codec,
    pub frame_count: u16,
    pub rotations: Vec<Vec<RealQuaternion>>,
    pub translations: Vec<Vec<RealPoint3d>>,
    pub scales: Vec<Vec<f32>>,
}

/// Animated-stream decode outcome attached to [`AnimationClip`] when
/// the animated codec wasn't (yet) decoded. `Unsupported` carries the
/// codec we recognized but can't yet decode (slot 4/5/6/7/8 etc.);
/// `Unknown` carries the raw byte when it didn't map to a known slot.
#[derive(Debug, Clone)]
pub enum AnimatedStreamStatus {
    /// `data sizes/default_data == 0` — animation is static-only.
    NoAnimatedStream,
    /// Decoded successfully — values live in `clip.animated_tracks`.
    Decoded,
    /// Recognized codec but no decoder yet.
    Unsupported(Codec),
    /// Codec byte ∉ 0..=11.
    Unknown(u8),
}

/// A fully-decoded animation. The blob's two codec streams come back
/// separately:
///
/// - [`static_tracks`](Self::static_tracks): the rest-pose / default
///   section, addressed by the *static* node-flag bitarrays (which
///   bones use this fixed transform). For most MCC animations this is
///   [`Codec::UncompressedStatic`].
/// - [`animated_tracks`](Self::animated_tracks): the per-frame
///   section, addressed by the *animated* node-flag bitarrays. `None`
///   when `data sizes/default_data == 0` (the animation is purely a
///   rest pose with no per-frame motion). The animated codec varies:
///   slot 3 (8-byte fullframe), slot 4/5/6/7 (keyframe), slot 8
///   (blend-screen) all common.
///
/// Combining the two via the bitarray flags into a single `(node,
/// frame) → transform` table happens in [`AnimationClip::pose`]; the
/// data model here keeps them separate so the JMA exporter, JSON
/// dump, and any future GUI can compose them differently.
#[derive(Debug, Clone)]
pub struct AnimationClip {
    pub frame_count: u16,
    pub static_tracks: AnimationTracks,
    pub animated_tracks: Option<AnimationTracks>,
    /// Why `animated_tracks` is what it is. `Decoded` when the animated
    /// codec was recognized and read; `NoAnimatedStream` for purely
    /// static animations; `Unsupported(_)` / `Unknown(_)` when the
    /// animated stream exists but couldn't be decoded — the static
    /// tracks are still valid in those cases (rest pose).
    pub animated_status: AnimatedStreamStatus,
    /// Per-component node-flag bitarrays for static and animated
    /// streams. `None` when the blob doesn't carry them (older inline
    /// layouts) — composition then falls back to "all bones use
    /// static_tracks" for static-only animations.
    pub node_flags: Option<NodeFlags>,
    /// Per-frame root-bone movement (dx/dy/dz/dyaw deltas). Empty
    /// when the animation has no movement (`frame info type = none`).
    pub movement: MovementData,
}

/// Per-frame root-bone movement deltas, keyed by `frame_info_type`.
/// All values are **local-space** as stored on disk — JMA's
/// world-space convention is applied at export time only.
///
/// - [`DxDy`](MovementKind::DxDy): 2 f32 per frame (dx, dy).
/// - [`DxDyDyaw`](MovementKind::DxDyDyaw): 3 f32 per frame (dx, dy, dyaw radians).
/// - [`DxDyDzDyaw`](MovementKind::DxDyDzDyaw): 4 f32 (dx, dy, dz, dyaw radians).
///
/// Layout matches TagTool's `MovementData.Read` (per-frame, packed
/// little-endian floats).
#[derive(Debug, Clone, Default)]
pub struct MovementData {
    pub kind: MovementKind,
    pub frames: Vec<MovementFrame>,
}

/// Kind of per-frame root movement encoded in the animation —
/// matches the schema's `frame_info_type_enum`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MovementKind {
    #[default]
    None,
    DxDy,
    DxDyDyaw,
    DxDyDzDyaw,
    /// `DxDyDz + angle_axis` — Reach+ addition. The rotation is the full
    /// angle-axis 3-vector (magnitude = angle), decoded to a quaternion.
    DxDyDzDangleAxis,
    /// `xyz_absolute` — Reach+ addition. The translation is an
    /// **absolute** root position per frame (not accumulated), no
    /// rotation. Used by some cinematic/scripted root paths.
    XyzAbsolute,
}

impl MovementKind {
    /// Bytes per frame on disk for this kind.
    pub fn bytes_per_frame(self) -> usize {
        match self {
            Self::None => 0,
            Self::DxDy => 8,
            Self::DxDyDyaw => 12,
            Self::DxDyDzDyaw => 16,
            Self::DxDyDzDangleAxis => 24,
            Self::XyzAbsolute => 12,
        }
    }

    /// `true` when this kind drives the root bone's *position*
    /// absolutely (replacing the accumulator) rather than as a
    /// per-frame delta.
    pub fn is_absolute(self) -> bool {
        matches!(self, Self::XyzAbsolute)
    }

    /// Resolve the `frame_info_type_enum` schema option name to a
    /// [`MovementKind`]. Unknown / missing names map to [`Self::None`].
    pub fn from_schema_name(name: &str) -> Self {
        match name {
            "dx,dy" => Self::DxDy,
            "dx,dy,dyaw" => Self::DxDyDyaw,
            "dx,dy,dz,dyaw" => Self::DxDyDzDyaw,
            "dx,dy,dz,dangle_axis" => Self::DxDyDzDangleAxis,
            "xyz,absolute" | "xyz_absolute" | "x,y,z,absolute" => Self::XyzAbsolute,
            _ => Self::None,
        }
    }
}

/// One frame of root-bone movement. `(dx, dy, dz)` is a local-space
/// translation delta (or an absolute position for
/// [`MovementKind::XyzAbsolute`]); `rotation` is the per-frame rotation
/// **delta** (identity when the kind carries no rotation). All in
/// local space — JMA's world-space convention is applied at export.
#[derive(Debug, Clone, Copy)]
pub struct MovementFrame {
    pub dx: f32,
    pub dy: f32,
    pub dz: f32,
    /// Per-frame rotation delta. `DxDyDyaw`/`DxDyDzDyaw` carry a
    /// yaw-only quaternion; `DxDyDzDangleAxis` the full angle-axis
    /// rotation; the rest leave this at identity.
    pub rotation: RealQuaternion,
}

impl Default for MovementFrame {
    fn default() -> Self {
        Self { dx: 0.0, dy: 0.0, dz: 0.0, rotation: RealQuaternion::IDENTITY }
    }
}

/// Per-component node-flag bitarrays for static + animated codec
/// streams. Six BitArrays total (rotation, translation, scale × static,
/// animated). Bone N is "static-rotated" iff
/// `static_rotation.bit(N)` is set; in that case its codec_node_index
/// in `static_tracks.rotations` is `popcount(static_rotation[0..N])`.
#[derive(Debug, Clone, Default)]
pub struct NodeFlags {
    pub static_rotation: BitArray,
    pub static_translation: BitArray,
    pub static_scale: BitArray,
    pub animated_rotation: BitArray,
    pub animated_translation: BitArray,
    pub animated_scale: BitArray,
}

/// Tightly-packed bit array used by the animation flag tables. Stored
/// as `u32` words (matching how the engine writes them).
#[derive(Debug, Clone, Default)]
pub struct BitArray {
    /// Underlying u32 words, little-endian on disk.
    pub words: Vec<u32>,
}

impl BitArray {
    /// Build from a 64-bit mask (low word = bits 0–31, high = 32–63).
    /// Used by the Halo CE antr path, whose node-flag masks are two
    /// `long_integer`s.
    pub fn from_u64(mask: u64) -> Self {
        Self { words: vec![mask as u32, (mask >> 32) as u32] }
    }

    /// Parse a tightly packed flag bitarray from little-endian u32
    /// words. Trailing bytes that don't fill a full u32 are dropped.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let words = bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Self { words }
    }

    /// Read a single bit. Returns `false` for indices past the end.
    pub fn bit(&self, index: usize) -> bool {
        let (w, b) = (index / 32, index % 32);
        self.words.get(w).copied().unwrap_or(0) & (1u32 << b) != 0
    }

    /// Number of set bits in `[0..bound)`. Used to translate a
    /// skeleton-bone index into the codec's own flat node enumeration.
    pub fn popcount_below(&self, bound: usize) -> usize {
        let full_words = bound / 32;
        let mut count = 0u32;
        for w in self.words.iter().take(full_words) {
            count += w.count_ones();
        }
        if let Some(&w) = self.words.get(full_words) {
            let trailing_bits = bound % 32;
            if trailing_bits > 0 {
                let mask = (1u32 << trailing_bits) - 1;
                count += (w & mask).count_ones();
            }
        }
        count as usize
    }
}
