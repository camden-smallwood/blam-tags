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

/// A four-word `FGuid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Guid(pub [u32; 4]);

impl Guid {
    pub const SIZE: usize = 16;

    pub fn serialize(&mut self, ar: &mut impl Ar) -> Result<()> {
        for w in &mut self.0 {
            ar.u32(w)?;
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

/// `FStaticMeshSection` — five indices then five four-byte flags.
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

/// `FClothingSectionData` — which clothing asset a section uses.
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

ue_structs!(
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
