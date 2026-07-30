//! Engine structs a class tail writes, as *types* rather than byte runs.
//!
//! [`super::tail_models`] originally kept these as `[u8; N]` and `FixedArray`s.
//! That round-trips, but it is not the same thing as modeling them, and for two
//! kinds of field it is actively unsafe:
//!
//!  * An **`FName` is an index into the package name map**, not text. A raw
//!    8-byte copy stays correct exactly as long as nothing edits that map — and
//!    editing it is the point of a writer. The same mistake, in a place that
//!    looked like a constant, is what made every cooked texture write the wrong
//!    format terminator.
//!  * An **`FPackageIndex` is an object reference**. A mod that retargets one
//!    has to be able to find it, and one buried inside `FStaticMaterial`'s 36
//!    bytes cannot be found.
//!
//! Byte-identical round-trip cannot catch either, because both only go wrong
//! under mutation. So these are types.
//!
//! Everything here is laid out exactly as the cooked stream writes it, and the
//! widths are the ones the corpus measured before the conversion — a struct that
//! encodes to a different length than it decoded from is a bug the gate catches
//! immediately.

use anyhow::{bail, Result};

use super::archive::Ar;
use super::value::FName;

/// `FGuid` — four `int32`s named A, B, C, D (NoExportTypes.h:528).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Guid(pub [i32; 4]);

impl Guid {
    pub const SIZE: usize = 16;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        for w in &mut self.0 {
            ar.i32(w)?;
        }
        Ok(())
    }
}

/// `FSHAHash` — twenty bytes with no interior. A hash is one value, not a
/// struct, so this is a named byte array rather than a decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaHash(pub [u8; 20]);

impl Default for ShaHash {
    fn default() -> Self {
        ShaHash([0; 20])
    }
}

impl ShaHash {
    pub const SIZE: usize = 20;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        let mut v = self.0.to_vec();
        ar.raw(&mut v, 20)?;
        self.0 = v.try_into().expect("20 bytes");
        Ok(())
    }
}

/// `FHashedName` — a `uint64` hash standing in for a name that was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HashedName(pub u64);

impl HashedName {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u64(&mut self.0)
    }
}

/// `FStripDataFlags` — the global and class-specific strip masks, always written
/// as a pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StripDataFlags {
    pub global: u8,
    pub class: u8,
}

impl StripDataFlags {
    pub const SIZE: usize = 2;

    /// `EStrippedData::Editor` — set by every client cook, and *not* a reason to
    /// suppress render data.
    pub const EDITOR: u8 = 1;
    /// `EStrippedData::AudioVisual`.
    pub const AUDIO_VISUAL: u8 = 2;

    pub fn audio_visual_stripped(&self) -> bool {
        self.global & Self::AUDIO_VISUAL != 0
    }

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.global)?;
        ar.u8(&mut self.class)
    }
}

/// `FVector` at large-world-coordinate precision.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector3d {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vector3d {
    pub const SIZE: usize = 24;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.f64(&mut self.x)?;
        ar.f64(&mut self.y)?;
        ar.f64(&mut self.z)
    }
}

/// `FVector3f`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector3f {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3f {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.f32(&mut self.x)?;
        ar.f32(&mut self.y)?;
        ar.f32(&mut self.z)
    }
}

/// `FVector4f`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector4f {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Vector4f {
    pub const SIZE: usize = 16;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.f32(&mut self.x)?;
        ar.f32(&mut self.y)?;
        ar.f32(&mut self.z)?;
        ar.f32(&mut self.w)
    }
}

/// `FBoxSphereBounds` at LWC precision.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoxSphereBounds {
    pub origin: Vector3d,
    pub box_extent: Vector3d,
    pub sphere_radius: f64,
}

impl BoxSphereBounds {
    pub const SIZE: usize = 56;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.origin.serialize(ar)?;
        self.box_extent.serialize(ar)?;
        ar.f64(&mut self.sphere_radius)
    }
}

/// `FBox` at LWC precision — 49 bytes, the `IsValid` byte deliberately unpadded.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Box3d {
    pub min: Vector3d,
    pub max: Vector3d,
    pub is_valid: u8,
}

impl Box3d {
    pub const SIZE: usize = 49;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.min.serialize(ar)?;
        self.max.serialize(ar)?;
        ar.u8(&mut self.is_valid)
    }
}

/// `FBox3f` — the float variant, 25 bytes rather than 49.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Box3f {
    pub min: Vector3f,
    pub max: Vector3f,
    pub is_valid: u8,
}

impl Box3f {
    pub const SIZE: usize = 25;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.min.serialize(ar)?;
        self.max.serialize(ar)?;
        ar.u8(&mut self.is_valid)
    }
}

/// `FPerPlatformFloat` in a cooked stream: the `bCooked` flag `FArchive` writes
/// as four bytes, then the default. The override map is editor-only.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PerPlatformFloat {
    pub cooked: u32,
    pub default: f32,
}

impl PerPlatformFloat {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.cooked)?;
        ar.f32(&mut self.default)
    }
}

/// `FMeshUVChannelInfo` — two four-byte bools and the per-channel densities.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshUvChannelInfo {
    pub initialized: u32,
    pub override_densities: u32,
    pub local_uv_densities: [f32; 4],
}

impl MeshUvChannelInfo {
    pub const SIZE: usize = 24;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.initialized)?;
        ar.u32(&mut self.override_densities)?;
        for d in &mut self.local_uv_densities {
            ar.f32(d)?;
        }
        Ok(())
    }
}

/// `FStaticMaterial` (StaticMesh.h:482).
///
/// The reason this is a type: it holds an **object reference** and a **name**.
/// As 36 opaque bytes, a tool retargeting a material or renaming a slot could
/// not see either.
// No `Eq`: the UV channel densities are floats.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StaticMaterial {
    pub material_interface: i32,
    pub material_slot_name: FName,
    pub uv_channel_data: MeshUvChannelInfo,
}

impl StaticMaterial {
    pub const SIZE: usize = 36;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.material_interface)?;
        ar.fname(&mut self.material_slot_name)?;
        self.uv_channel_data.serialize(ar)
    }
}

/// `FMeshBoneInfo` — a bone's name and its parent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeshBoneInfo {
    pub name: FName,
    pub parent_index: i32,
}

impl MeshBoneInfo {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.fname(&mut self.name)?;
        ar.i32(&mut self.parent_index)
    }
}

/// One entry of a `TMap<FName, int32>` as a plain archive writes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NameToIndex {
    pub name: FName,
    pub index: i32,
}

impl NameToIndex {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.fname(&mut self.name)?;
        ar.i32(&mut self.index)
    }
}

/// One entry of `UClass::FuncMap` — a function's name and the object that is it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FuncMapEntry {
    pub name: FName,
    pub function: i32,
}

impl FuncMapEntry {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.fname(&mut self.name)?;
        ar.i32(&mut self.function)
    }
}

/// `FImplementedInterface`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ImplementedInterface {
    pub class: i32,
    pub pointer_offset: i32,
    pub implemented_by_k2: u32,
}

impl ImplementedInterface {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.class)?;
        ar.i32(&mut self.pointer_offset)?;
        ar.u32(&mut self.implemented_by_k2)
    }
}

/// `FStaticMeshSection` (StaticMeshResources.h:198).
///
/// The field order here is the **wire** order from `operator<<`
/// (StaticMesh.cpp:300), which is *not* the declaration order: `bForceOpaque`
/// is serialized third of the flags and declared fifth. The editor-only UV
/// densities sit between `bForceOpaque` and `bVisibleInRayTracing` and are
/// absent from a cook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StaticMeshSection {
    pub material_index: i32,
    pub first_index: i32,
    pub num_triangles: i32,
    pub min_vertex_index: i32,
    pub max_vertex_index: i32,
    pub enable_collision: u32,
    pub cast_shadow: u32,
    pub force_opaque: u32,
    pub visible_in_ray_tracing: u32,
    pub affect_distance_field_lighting: u32,
}

impl StaticMeshSection {
    pub const SIZE: usize = 40;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        for v in [
            &mut self.material_index,
            &mut self.first_index,
            &mut self.num_triangles,
            &mut self.min_vertex_index,
            &mut self.max_vertex_index,
        ] {
            ar.i32(v)?;
        }
        for v in [
            &mut self.enable_collision,
            &mut self.cast_shadow,
            &mut self.force_opaque,
            &mut self.visible_in_ray_tracing,
            &mut self.affect_distance_field_lighting,
        ] {
            ar.u32(v)?;
        }
        Ok(())
    }
}

/// `FClothingSectionData` (SkeletalMeshTypes.h:105) — the clothing asset a
/// section uses, and which of its LODs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClothingSectionData {
    pub asset_guid: Guid,
    pub asset_lod_index: i32,
}

impl ClothingSectionData {
    pub const SIZE: usize = 20;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.asset_guid.serialize(ar)?;
        ar.i32(&mut self.asset_lod_index)
    }
}

/// A fixed-width engine struct that serializes the same way in both directions.
///
/// `SIZE` is not decoration: it is the width the corpus measured before the
/// conversion, and [`read_vec`] uses it to bound a count before allocating.
pub trait UeStruct: Default + Clone {
    const SIZE: usize;
    fn ser<A: Ar>(&mut self, ar: &mut A) -> Result<()>;
}

macro_rules! ue_structs {
    ($($t:ty),* $(,)?) => { $(
        impl UeStruct for $t {
            const SIZE: usize = <$t>::SIZE;
            fn ser<A: Ar>(&mut self, ar: &mut A) -> Result<()> {
                self.serialize(ar)
            }
        }
    )* };
}

/// `FPageStreamingState` (NaniteResources.h:185) — where one Nanite page lives
/// in the streaming bulk data and what it depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PageStreamingState {
    pub bulk_offset: u32,
    pub bulk_size: u32,
    pub page_size: u32,
    pub dependencies_start: u32,
    pub dependencies_num: u16,
    pub max_hierarchy_depth: u8,
    pub flags: u8,
}

impl PageStreamingState {
    pub const SIZE: usize = 20;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.bulk_offset)?;
        ar.u32(&mut self.bulk_size)?;
        ar.u32(&mut self.page_size)?;
        ar.u32(&mut self.dependencies_start)?;
        ar.u16(&mut self.dependencies_num)?;
        ar.u8(&mut self.max_hierarchy_depth)?;
        ar.u8(&mut self.flags)
    }
}

/// `FPackedHierarchyNode` (NaniteResources.h:48) — one BVH node covering up to
/// `NANITE_MAX_BVH_NODE_FANOUT` (4) children.
///
/// **Structure of arrays, not an array of structures.** All four `LODBounds`
/// come first, then all four `Misc0`, and so on. Reading it as four interleaved
/// 52-byte child records gives the same 208 bytes and the wrong values
/// everywhere — which a byte-identical round trip cannot detect, and which is
/// what this type existed as before it was checked against the header.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PackedHierarchyNode {
    pub lod_bounds: [Vector4f; 4],
    pub box_bounds_center: [Vector3f; 4],
    pub min_lod_error_max_parent_lod_error: [u32; 4],
    pub box_bounds_extent: [Vector3f; 4],
    pub child_start_reference: [u32; 4],
    pub resource_page_index_num_pages_group_part_size: [u32; 4],
}

impl PackedHierarchyNode {
    pub const SIZE: usize = 208;
    /// `NANITE_MAX_BVH_NODE_FANOUT`.
    pub const FANOUT: usize = 4;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        for v in &mut self.lod_bounds {
            v.serialize(ar)?;
        }
        for i in 0..Self::FANOUT {
            self.box_bounds_center[i].serialize(ar)?;
            ar.u32(&mut self.min_lod_error_max_parent_lod_error[i])?;
        }
        for i in 0..Self::FANOUT {
            self.box_bounds_extent[i].serialize(ar)?;
            ar.u32(&mut self.child_start_reference[i])?;
        }
        for v in &mut self.resource_page_index_num_pages_group_part_size {
            ar.u32(v)?;
        }
        Ok(())
    }
}

/// `TLumenCardOBB<float>` (MeshCardRepresentation.h:26).
///
/// The wire order is **AxisX, AxisY, AxisZ, Origin, Extent** — `Origin` is
/// declared first and serialized fourth.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LumenCardObb {
    pub axis_x: Vector3f,
    pub axis_y: Vector3f,
    pub axis_z: Vector3f,
    pub origin: Vector3f,
    pub extent: Vector3f,
}

impl LumenCardObb {
    pub const SIZE: usize = 60;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.axis_x.serialize(ar)?;
        self.axis_y.serialize(ar)?;
        self.axis_z.serialize(ar)?;
        self.origin.serialize(ar)?;
        self.extent.serialize(ar)
    }
}

/// `FLumenCardBuildData` (MeshCardBuild.h:17).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LumenCardBuildData {
    pub obb: LumenCardObb,
    pub axis_aligned_direction_index: u8,
}

impl LumenCardBuildData {
    pub const SIZE: usize = 61;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.obb.serialize(ar)?;
        ar.u8(&mut self.axis_aligned_direction_index)
    }
}

/// `FInt32Vector`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Int32Vector {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Int32Vector {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.x)?;
        ar.i32(&mut self.y)?;
        ar.i32(&mut self.z)
    }
}

/// `FVector2f`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vector2f {
    pub x: f32,
    pub y: f32,
}

impl Vector2f {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.f32(&mut self.x)?;
        ar.f32(&mut self.y)
    }
}

/// `FSparseDistanceFieldMip` (DistanceFieldAtlas.h:197). Wire order matches the
/// declaration (`operator<<` at :219).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SparseDistanceFieldMip {
    pub indirection_dimensions: Int32Vector,
    pub num_distance_field_bricks: i32,
    pub volume_to_virtual_uv_scale: Vector3f,
    pub volume_to_virtual_uv_add: Vector3f,
    pub distance_field_to_volume_scale_bias: Vector2f,
    pub bulk_offset: u32,
    pub bulk_size: u32,
}

impl SparseDistanceFieldMip {
    pub const SIZE: usize = 56;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.indirection_dimensions.serialize(ar)?;
        ar.i32(&mut self.num_distance_field_bricks)?;
        self.volume_to_virtual_uv_scale.serialize(ar)?;
        self.volume_to_virtual_uv_add.serialize(ar)?;
        self.distance_field_to_volume_scale_bias.serialize(ar)?;
        ar.u32(&mut self.bulk_offset)?;
        ar.u32(&mut self.bulk_size)
    }
}

/// `FStaticMeshLODResources::FStaticMeshBuffersSize` (StaticMeshResources.h:551).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StaticMeshBuffersSize {
    pub serialized_buffers_size: u32,
    pub depth_only_ib_size: u32,
    pub reversed_ibs_size: u32,
}

impl StaticMeshBuffersSize {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.serialized_buffers_size)?;
        ar.u32(&mut self.depth_only_ib_size)?;
        ar.u32(&mut self.reversed_ibs_size)
    }
}

/// `FMeshToMeshVertData` (SkeletalMeshTypes.h:58) — one cloth wrap-deformer
/// influence.
///
/// **64 bytes, not 80.** The walker used 80, and every cloth mapping array in
/// the corpus is empty, so nothing ever exercised it — a non-zero count with a
/// 16-byte-per-element error would have desynced the whole LOD immediately.
/// Read from `operator<<` (SkeletalMeshLODRenderData.cpp:193): three
/// `FVector4f`, four `uint16` indices, a weight and a padding word. The
/// pre-`WeightFMeshToMeshVertData` form was also 64 — two padding words instead
/// of weight-plus-padding.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshToMeshVertData {
    pub position_bary_coords_and_dist: Vector4f,
    pub normal_bary_coords_and_dist: Vector4f,
    pub tangent_bary_coords_and_dist: Vector4f,
    /// Three source-mesh triangle indices; the fourth is a flag, `0xffff`
    /// meaning "skin normally, no simulation".
    pub source_mesh_vert_indices: [u16; 4],
    pub weight: f32,
    pub padding: u32,
}

impl MeshToMeshVertData {
    pub const SIZE: usize = 64;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        self.position_bary_coords_and_dist.serialize(ar)?;
        self.normal_bary_coords_and_dist.serialize(ar)?;
        self.tangent_bary_coords_and_dist.serialize(ar)?;
        for i in &mut self.source_mesh_vert_indices {
            ar.u16(i)?;
        }
        ar.f32(&mut self.weight)?;
        ar.u32(&mut self.padding)
    }
}

/// `FLightmassPrimitiveSettings`.
///
/// Typed from the UHT declaration — four `uint8 b:1` bitfields then five floats,
/// each bool four bytes through `FArchive`, which is the measured 36. The
/// *order* is the declaration's; the `operator<<` body is in none of the
/// obvious translation units, so unlike the rest of this module it is the one
/// layout whose field order is inferred rather than read.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LightmassPrimitiveSettings {
    pub use_two_sided_lighting: u32,
    pub shadow_indirect_only: u32,
    pub use_emissive_for_static_lighting: u32,
    pub use_vertex_normal_for_hemisphere_gather: u32,
    pub emissive_light_falloff_exponent: f32,
    pub emissive_light_explicit_influence_radius: f32,
    pub emissive_boost: f32,
    pub diffuse_boost: f32,
    pub fully_occluded_samples_fraction: f32,
}

impl LightmassPrimitiveSettings {
    pub const SIZE: usize = 36;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        for b in [
            &mut self.use_two_sided_lighting,
            &mut self.shadow_indirect_only,
            &mut self.use_emissive_for_static_lighting,
            &mut self.use_vertex_normal_for_hemisphere_gather,
        ] {
            ar.u32(b)?;
        }
        for f in [
            &mut self.emissive_light_falloff_exponent,
            &mut self.emissive_light_explicit_influence_radius,
            &mut self.emissive_boost,
            &mut self.diffuse_boost,
            &mut self.fully_occluded_samples_fraction,
        ] {
            ar.f32(f)?;
        }
        Ok(())
    }
}

/// One entry of `UActorComponent::UCSModifiedProperties` — which object, which
/// property, and the construction-script instance that set it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UcsModifiedProperty {
    pub object: i32,
    pub property_name: FName,
    pub guid: Guid,
}

impl UcsModifiedProperty {
    pub const SIZE: usize = 28;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.object)?;
        ar.fname(&mut self.property_name)?;
        self.guid.serialize(ar)
    }
}

/// `FClothBufferIndexMapping` (SkeletalMeshTypes.h:91).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClothBufferIndexMapping {
    pub base_vertex_index: u32,
    pub mapping_offset: u32,
    pub lod_bias_stride: u32,
}

impl ClothBufferIndexMapping {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.base_vertex_index)?;
        ar.u32(&mut self.mapping_offset)?;
        ar.u32(&mut self.lod_bias_stride)
    }
}

/// One `FMemoryImageVTablePatch`-adjacent type dependency in a shader map's
/// pointer table: the type's name, the size its layout had when the image was
/// frozen, and the hash of that layout.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryImageTypeDependency {
    pub name: FName,
    pub layout_size: u32,
    pub layout_hash: ShaHash,
}

impl MemoryImageTypeDependency {
    pub const SIZE: usize = 32;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.fname(&mut self.name)?;
        ar.u32(&mut self.layout_size)?;
        self.layout_hash.serialize(ar)
    }
}

/// `FPlatformTypeLayoutParameters` — the alignment and flags a frozen memory
/// image was built with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PlatformTypeLayoutParameters {
    pub max_field_alignment: u32,
    pub flags: u32,
}

impl PlatformTypeLayoutParameters {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.max_field_alignment)?;
        ar.u32(&mut self.flags)
    }
}

/// One vtable patch: where in the frozen image a pointer lives and what offset
/// to write there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VTablePatch {
    pub vtable_offset: u32,
    pub offset: u32,
}

impl VTablePatch {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.vtable_offset)?;
        ar.u32(&mut self.offset)
    }
}

/// One `TMap<FGuid, int32>`-style grass weight offset: the object it belongs to
/// and where its weights start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GrassWeightOffset {
    pub grass_type: i32,
    pub offset: i32,
}

impl GrassWeightOffset {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.grass_type)?;
        ar.i32(&mut self.offset)
    }
}

/// One entry of a duplicated-vertex index buffer: where a vertex's duplicates
/// start and how many there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DuplicatedVertexIndex {
    pub num_duplicates: u32,
    pub index: u32,
}

impl DuplicatedVertexIndex {
    pub const SIZE: usize = 8;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.num_duplicates)?;
        ar.u32(&mut self.index)
    }
}

/// One `FPCGMetadataAttributeBase` entry-to-value mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntryToValueKey {
    pub entry_key: i64,
    pub value_key: i32,
}

impl EntryToValueKey {
    pub const SIZE: usize = 12;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        let mut lo = self.entry_key as u64;
        ar.u64(&mut lo)?;
        self.entry_key = lo as i64;
        ar.i32(&mut self.value_key)
    }
}

ue_structs!(
    UcsModifiedProperty,
    ClothBufferIndexMapping,
    MemoryImageTypeDependency,
    PlatformTypeLayoutParameters,
    VTablePatch,
    GrassWeightOffset,
    DuplicatedVertexIndex,
    EntryToValueKey,
    Int32Vector,
    Vector2f,
    SparseDistanceFieldMip,
    StaticMeshBuffersSize,
    MeshToMeshVertData,
    LightmassPrimitiveSettings,
    PageStreamingState,
    PackedHierarchyNode,
    LumenCardObb,
    LumenCardBuildData,
    Guid,
    ShaHash,
    HashedName,
    StripDataFlags,
    Vector3d,
    Vector3f,
    Vector4f,
    BoxSphereBounds,
    Box3d,
    Box3f,
    PerPlatformFloat,
    MeshUvChannelInfo,
    StaticMaterial,
    MeshBoneInfo,
    NameToIndex,
    FuncMapEntry,
    ImplementedInterface,
    StaticMeshSection,
    ClothingSectionData,
);

/// Read a `TArray<T>` written with a bare count.
pub fn read_vec<T: UeStruct, A: Ar>(ar: &mut A, what: &str, n: usize) -> Result<Vec<T>> {
    // `SIZE` bounds the allocation: a count is untrusted input, and reserving
    // from it is how a corrupt file turns into an out-of-memory abort.
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let mut t = T::default();
        t.ser(ar).map_err(|e| e.context(format!("{what} element")))?;
        out.push(t);
    }
    Ok(out)
}

/// Write a `TArray<T>`, count first.
pub fn write_vec<T: UeStruct, A: Ar>(ar: &mut A, v: &[T]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for t in v {
        t.clone().ser(ar)?;
    }
    Ok(())
}

/// Write a run of `T` with no count of its own — the caller already wrote one,
/// or the length is implied.
pub fn write_run<T: UeStruct, A: Ar>(ar: &mut A, v: &[T]) -> Result<()> {
    for t in v {
        t.clone().ser(ar)?;
    }
    Ok(())
}

/// Guard a count read off the wire before it is used to allocate.
pub fn bounded_count(n: i32, what: &str, at: usize) -> Result<usize> {
    if !(0..=super::limits::MAX_NATIVE_COUNT).contains(&n) {
        bail!("implausible {what} count {n} @ {at}");
    }
    Ok(n as usize)
}
