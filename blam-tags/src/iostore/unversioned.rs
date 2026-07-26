//! Cooked *unversioned* property serialization reader, driven by the embedded
//! `.usmap` schema — enough of it to recover the authoritative Campaign
//! Evolved region→permutation→mesh mapping from a cooked
//! `BlamMeshSynchronizationComponent` export.
//!
//! Cooked IoStore packages serialize object properties without per-property
//! tags: an `FUnversionedHeader` (a run of `skip`/`value` fragments plus an
//! optional zero-mask) says *which* schema properties are present, and the
//! values follow back-to-back. The schema (property order + types) comes from
//! the `.usmap`. Property order within a class is **derived→base**, matching
//! the engine's `UStruct::PropertyLink` walk — see
//! [`Usmap::flattened_properties`](super::usmap::Usmap::flattened_properties).
//!
//! Nested reflected structs (e.g. `FBlamMeshSynchronizationRuntimeRegion`)
//! serialize the same way recursively; a handful of engine structs
//! (`FTransform`, `FVector`, …) instead serialize as fixed-size native blobs
//! and are skipped by byte size.

use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;

use super::usmap::{PropertyType, Usmap, UsmapProperty};

/// A decoded property value. Only the shapes this reader needs are modeled;
/// everything else is consumed for correct positioning and discarded.
#[derive(Debug, Clone)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    /// An `FName` resolved to its display string.
    Name(String),
    Str(String),
    /// An `FPackageIndex` (import if negative, export if positive).
    Object(i32),
    /// An `FSoftObjectPath`: `(PackageName, AssetName, SubPath)`.
    SoftObject(SoftObjectPath),
    Array(Vec<PropValue>),
    /// A `TMap`, preserving insertion order.
    Map(Vec<(PropValue, PropValue)>),
    /// A nested reflected struct: property name → value.
    Struct(BTreeMap<String, PropValue>),
    /// A natively-serialized struct's raw bytes (e.g. `FVector`/`FQuat`), kept
    /// so transforms can be decoded on demand.
    Native(Vec<u8>),
    /// A value consumed but not modeled (delegate, field path, …).
    Opaque,
}

impl PropValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Name(s) | PropValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(PropValue, PropValue)]> {
        match self {
            PropValue::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[PropValue]> {
        match self {
            PropValue::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_struct(&self) -> Option<&BTreeMap<String, PropValue>> {
        match self {
            PropValue::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_soft_object(&self) -> Option<&SoftObjectPath> {
        match self {
            PropValue::SoftObject(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_native(&self) -> Option<&[u8]> {
        match self {
            PropValue::Native(b) => Some(b),
            _ => None,
        }
    }
}

/// A component-relative transform (`FTransform`) attached to a bone: UE5
/// large-world-coordinate `double`s, so `FQuat`=4×f64 and `FVector`=3×f64.
#[derive(Debug, Clone, Copy)]
pub struct MeshTransform {
    /// `(x, y, z, w)` quaternion.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for MeshTransform {
    fn default() -> Self {
        Self {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl MeshTransform {
    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3]
            && self.rotation == [0.0, 0.0, 0.0, 1.0]
            && self.scale == [1.0; 3]
    }

    /// Decode from a reflected `FTransform` struct value (`Rotation`/
    /// `Translation`/`Scale3D` as native `FQuat`/`FVector` blobs).
    fn from_prop(v: &PropValue) -> Option<MeshTransform> {
        let s = v.as_struct()?;
        let f64s = |name: &str, n: usize| -> Option<Vec<f64>> {
            let b = s.get(name)?.as_native()?;
            if b.len() < n * 8 {
                return None;
            }
            Some(
                (0..n)
                    .map(|i| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()))
                    .collect(),
            )
        };
        let mut t = MeshTransform::default();
        if let Some(r) = f64s("Rotation", 4) {
            t.rotation = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
        }
        if let Some(tr) = f64s("Translation", 3) {
            t.translation = [tr[0] as f32, tr[1] as f32, tr[2] as f32];
        }
        if let Some(sc) = f64s("Scale3D", 3) {
            t.scale = [sc[0] as f32, sc[1] as f32, sc[2] as f32];
        }
        Some(t)
    }
}

/// An `FSoftObjectPath` — a `TopLevelAssetPath` plus optional sub-path.
#[derive(Debug, Clone, Default)]
pub struct SoftObjectPath {
    /// Full package name, e.g. `/Game/Characters/Marine/.../SK_Marine_Torso_01`.
    pub package: String,
    /// Object name within the package, e.g. `SK_Marine_Torso_01`.
    pub asset: String,
    pub sub_path: String,
}

impl SoftObjectPath {
    pub fn is_empty(&self) -> bool {
        self.package.is_empty() && self.asset.is_empty()
    }
}

/// Fixed serialized byte sizes for engine structs that serialize *natively*
/// (a `SerializeNative`/`Serialize` override), so unversioned serialization
/// emits their raw bytes with no inner property header. The math primitives
/// use UE5 large-world-coordinate `double`s.
///
/// Note `FTransform` is deliberately *absent*: in this build it has no native
/// serializer and instead serializes as an unversioned struct
/// (`Rotation`/`Translation`/`Scale3D`), with zero-value components masked out
/// — so it is parsed via the schema like any other reflected struct, and its
/// `FQuat`/`FVector` members fall through to the native sizes below.
fn native_struct_size(name: &str) -> Option<usize> {
    Some(match name {
        "Vector" | "Rotator" => 24,               // 3 × f64
        "Vector4" | "Quat" => 32,                 // 4 × f64
        "Vector2D" | "LinearColor" | "Guid" => 16,
        "Vector3f" | "Rotator3f" | "IntVector" => 12, // 3 × f32 / 3 × i32
        "Vector2f" | "IntPoint" => 8,
        "Color" => 4,
        // `FPerPlatform*` serialize only their `Default` scalar in a cooked
        // (non-editor) build — the `PerPlatform` override map is editor-only. A
        // mesh's `MinLOD`/`NoRefStreamingLODBias`/etc. use these, so they must be
        // consumed as fixed native scalars to reach `Static/SkeletalMaterials`.
        "PerPlatformInt" => 4,
        "PerPlatformFloat" => 4,
        "PerPlatformBool" => 1,
        _ => return None,
    })
}

/// Little-endian byte-cursor over an export's serial data.
struct Reader<'a> {
    b: &'a [u8],
    o: usize,
    names: &'a [String],
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8], names: &'a [String]) -> Self {
        Reader { b, o: 0, names }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .b
            .get(self.o..self.o + n)
            .with_context(|| format!("unversioned read past end (+{n} @ {})", self.o))?;
        self.o += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// `FName`: `i32 index` + `i32 number`, resolved against the package name
    /// map. A non-zero number appends `_{number-1}`, per UE convention.
    fn name(&mut self) -> Result<String> {
        let idx = self.i32()?;
        let number = self.i32()?;
        let base = usize::try_from(idx)
            .ok()
            .and_then(|i| self.names.get(i))
            .with_context(|| format!("FName index {idx} out of range (@ {})", self.o - 8))?;
        Ok(if number > 0 {
            format!("{base}_{}", number - 1)
        } else {
            base.clone()
        })
    }
    /// `FString`: `i32 len`; positive = UTF-8 (NUL-terminated), negative =
    /// UTF-16 (len is negated char count).
    fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let bytes = self.take(n as usize)?;
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
        } else {
            let chars = (-n) as usize;
            let bytes = self.take(chars * 2)?;
            let u16s: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            Ok(String::from_utf16_lossy(&u16s))
        }
    }
}

/// One `FUnversionedHeader` fragment.
struct Fragment {
    skip: u8,
    has_zeroes: bool,
    value_num: u8,
    is_last: bool,
}

impl Fragment {
    fn unpack(p: u16) -> Self {
        Fragment {
            skip: (p & 0x7f) as u8,
            has_zeroes: (p & 0x80) != 0,
            is_last: (p & 0x100) != 0,
            value_num: (p >> 9) as u8,
        }
    }
}

/// Read an `FUnversionedHeader`, returning `(present_schema_indices, ...)`
/// where each present index is paired with whether its value is non-zero (a
/// zero-masked property serializes no bytes — it is the zero value).
fn read_header(r: &mut Reader) -> Result<Vec<(usize, bool)>> {
    let mut frags = Vec::new();
    let mut zero_mask_num = 0usize;
    loop {
        let frag = Fragment::unpack(r.u16()?);
        if frag.has_zeroes {
            zero_mask_num += frag.value_num as usize;
        }
        let last = frag.is_last;
        frags.push(frag);
        if last {
            break;
        }
    }
    // Zero mask: one bit per value in has-zeroes fragments.
    let mut zero_mask = Vec::with_capacity(zero_mask_num);
    if zero_mask_num > 0 {
        let (words, word_bits): (Vec<u32>, usize) = if zero_mask_num <= 8 {
            (vec![r.u8()? as u32], 8)
        } else if zero_mask_num <= 16 {
            (vec![r.u16()? as u32], 16)
        } else {
            let n = zero_mask_num.div_ceil(32);
            let mut w = Vec::with_capacity(n);
            for _ in 0..n {
                w.push(r.u32()?);
            }
            (w, 32)
        };
        for i in 0..zero_mask_num {
            zero_mask.push((words[i / word_bits] >> (i % word_bits)) & 1 == 1);
        }
    }

    let mut present = Vec::new();
    let mut schema_it = 0usize;
    let mut zi = 0usize;
    for frag in &frags {
        schema_it += frag.skip as usize;
        for _ in 0..frag.value_num {
            let non_zero = if frag.has_zeroes {
                let nz = !zero_mask[zi];
                zi += 1;
                nz
            } else {
                true
            };
            present.push((schema_it, non_zero));
            schema_it += 1;
        }
    }
    Ok(present)
}

/// Read a full reflected struct/class instance (its unversioned property
/// block) named `class`, returning present property name→value.
fn read_struct(r: &mut Reader, class: &str, usmap: &Usmap, depth: usize) -> Result<BTreeMap<String, PropValue>> {
    if depth > 32 {
        bail!("unversioned struct nesting too deep at {class}");
    }
    let flat = usmap
        .flattened_properties(class)
        .with_context(|| format!("no .usmap schema for struct {class}"))?;
    let present = read_header(r)?;
    let mut out = BTreeMap::new();
    for (idx, non_zero) in present {
        let prop = flat
            .get(idx)
            .with_context(|| format!("{class}: present schema index {idx} beyond {} props", flat.len()))?;
        let value = if non_zero {
            read_value(r, &prop.ty, usmap, depth)?
        } else {
            // Zero-masked: the property is its zero value, no bytes consumed.
            zero_value(&prop.ty)
        };
        out.insert(prop.name.clone(), value);
    }
    Ok(out)
}

/// The implicit "zero" for a zero-masked property (no bytes were serialized).
fn zero_value(ty: &PropertyType) -> PropValue {
    match ty {
        PropertyType::Bool => PropValue::Bool(false),
        PropertyType::Int
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { .. } => PropValue::Int(0),
        PropertyType::Float | PropertyType::Double => PropValue::Float(0.0),
        PropertyType::Object | PropertyType::Interface => PropValue::Object(0),
        _ => PropValue::Opaque,
    }
}

fn read_value(r: &mut Reader, ty: &PropertyType, usmap: &Usmap, depth: usize) -> Result<PropValue> {
    Ok(match ty {
        PropertyType::Bool => PropValue::Bool(r.u8()? != 0),
        PropertyType::Byte { .. } | PropertyType::Int8 => PropValue::Int(r.u8()? as i64),
        PropertyType::Int => PropValue::Int(r.i32()? as i64),
        PropertyType::UInt32 => PropValue::Int(r.u32()? as i64),
        PropertyType::Int16 | PropertyType::UInt16 => PropValue::Int(r.u16()? as i64),
        PropertyType::Int64 | PropertyType::UInt64 => PropValue::Int(r.u64()? as i64),
        PropertyType::Float => PropValue::Float(r.f32()? as f64),
        PropertyType::Double => PropValue::Float(r.f64()?),
        PropertyType::Name => PropValue::Name(r.name()?),
        PropertyType::Str | PropertyType::Utf8Str | PropertyType::AnsiStr => PropValue::Str(r.fstring()?),
        PropertyType::Enum { inner, .. } => {
            // Serialized as the underlying integer/byte value.
            read_value(r, inner, usmap, depth)?
        }
        PropertyType::Object
        | PropertyType::WeakObject
        | PropertyType::LazyObject
        | PropertyType::Interface => PropValue::Object(r.i32()?),
        PropertyType::SoftObject | PropertyType::AssetObject => {
            let package = r.name()?;
            let asset = r.name()?;
            let sub_path = r.fstring()?;
            PropValue::SoftObject(SoftObjectPath { package, asset, sub_path })
        }
        PropertyType::Struct(name) => {
            if let Some(size) = native_struct_size(name) {
                PropValue::Native(r.take(size)?.to_vec())
            } else {
                PropValue::Struct(read_struct(r, name, usmap, depth + 1)?)
            }
        }
        PropertyType::Array(inner) => {
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible array count {n} @ {}", r.o - 4);
            }
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_value(r, inner, usmap, depth)?);
            }
            PropValue::Array(v)
        }
        // A `TSet` serializes like a `TMap`, not like a `TArray`: it is
        // preceded by an `NumElementsToRemove` count (the delta-serialization
        // prefix). Treating it as a bare array leaves the stream 4 bytes short
        // and silently desyncs every property that follows.
        PropertyType::Set(inner) => {
            let _num_to_remove = r.i32()?;
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible set count {n} @ {}", r.o - 4);
            }
            let mut v = Vec::with_capacity(n as usize);
            for _ in 0..n {
                v.push(read_value(r, inner, usmap, depth)?);
            }
            PropValue::Array(v)
        }
        PropertyType::Map(k, val) => {
            let _num_to_remove = r.i32()?;
            let n = r.i32()?;
            if !(0..=1_000_000).contains(&n) {
                bail!("implausible map count {n} @ {}", r.o - 4);
            }
            let mut m = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let key = read_value(r, k, usmap, depth)?;
                let value = read_value(r, val, usmap, depth)?;
                m.push((key, value));
            }
            PropValue::Map(m)
        }
        PropertyType::Delegate => {
            // FScriptDelegate: object (FPackageIndex) + function FName.
            r.i32()?;
            r.name()?;
            PropValue::Opaque
        }
        PropertyType::MulticastDelegate => {
            let n = r.i32()?;
            for _ in 0..n.max(0) {
                r.i32()?;
                r.name()?;
            }
            PropValue::Opaque
        }
        PropertyType::FieldPath => {
            // TArray<FName> path + owner object.
            let n = r.i32()?;
            for _ in 0..n.max(0) {
                r.name()?;
            }
            r.i32()?;
            PropValue::Opaque
        }
        PropertyType::Text => bail!("FText unversioned read not implemented"),
        PropertyType::Optional(inner) => {
            // Serialized as a bool "is set" then the value.
            if r.u8()? != 0 {
                read_value(r, inner, usmap, depth)?
            } else {
                PropValue::Opaque
            }
        }
        PropertyType::Unknown(t) => bail!("unknown property kind {t} in unversioned stream"),
    })
}

// ---------------------------------------------------------------------------
// Typed Campaign Evolved mesh-sync extraction
// ---------------------------------------------------------------------------

/// A single mesh reference within a permutation.
#[derive(Debug, Clone)]
pub struct MeshRef {
    /// Full package path of the mesh asset (`/Game/.../SK_Marine_Torso_01`).
    pub package: String,
    /// Object name (`SK_Marine_Torso_01`).
    pub asset: String,
    /// Component class object name (`BPC_SkeletalMesh_C`,
    /// `BPC_HumanAnatomySkeletalMesh_C`, …), if present.
    pub class: String,
    /// Bone the (static) mesh attaches to, if any.
    pub parent_bone: String,
    /// The component's transform relative to `parent_bone` (identity when
    /// absent). Static pieces (e.g. a pelican's wings) offset from their bone
    /// need this applied on top of the bone's world rest transform.
    pub rel_transform: MeshTransform,
    /// Per-slot material overrides this instance applies to its mesh, as
    /// `(MaterialSlotName, override material-instance name)`. A variant (e.g.
    /// `brute_major`) overrides the base mesh's default slot materials by slot
    /// name; the effective material for a section is its override here, else the
    /// mesh's own default slot material. Empty when the instance uses defaults.
    pub material_overrides: Vec<(String, String)>,
}

/// One material slot on a mesh asset (`FStaticMaterial`/`FSkeletalMaterial`),
/// indexed by a section's `material_index`.
#[derive(Debug, Clone)]
pub struct MaterialSlot {
    /// `MaterialSlotName` — the key material overrides bind to.
    pub slot_name: String,
    /// The default material-instance object reference (`FPackageIndex`: negative
    /// = import, positive = export, 0 = none). The caller resolves it to a
    /// package/asset name through the mesh package's import table.
    pub material_object: i32,
}

/// Decode a mesh asset's material-slot array (`SkeletalMaterials` on a
/// `USkeletalMesh` / `StaticMaterials` on a `UStaticMesh`) from its export's
/// unversioned property block — the authoritative `material_index → (slot name,
/// default material)` mapping. Returns the slots in index order.
///
/// This decodes the mesh's reflected properties (not its native geometry, which
/// follows the property block); a schema surprise surfaces as an `Err` so the
/// caller can degrade to placeholder material names rather than corrupt output.
pub fn read_material_slots(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    is_skeletal: bool,
) -> Result<Vec<MaterialSlot>> {
    let class = if is_skeletal { "SkeletalMesh" } else { "StaticMesh" };
    let mut r = Reader::new(export, names);
    let s = read_struct(&mut r, class, usmap, 0)
        .with_context(|| format!("decoding {class} material slots"))?;
    let arr_name = if is_skeletal { "SkeletalMaterials" } else { "StaticMaterials" };
    let Some(arr) = s.get(arr_name).and_then(PropValue::as_array) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for el in arr {
        let es = el.as_struct();
        let slot_name = es
            .and_then(|e| e.get("MaterialSlotName"))
            .and_then(PropValue::as_str)
            .unwrap_or_default()
            .to_string();
        // FStaticMaterial::MaterialInterface / FSkeletalMaterial::MaterialInterface
        // (older cooks name the skeletal field `Material`).
        let material_object = es
            .and_then(|e| e.get("MaterialInterface").or_else(|| e.get("Material")))
            .and_then(|v| match v {
                PropValue::Object(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        out.push(MaterialSlot { slot_name, material_object });
    }
    Ok(out)
}

/// A permutation and the meshes it activates.
#[derive(Debug, Clone)]
pub struct Permutation {
    pub name: String,
    pub skeletal_meshes: Vec<MeshRef>,
    pub static_meshes: Vec<MeshRef>,
}

/// A region and its permutations (authoritative CE region→perm→mesh mapping).
#[derive(Debug, Clone)]
pub struct Region {
    pub name: String,
    pub permutations: Vec<Permutation>,
}

/// The decoded `RuntimeRegions` map of a `BlamMeshSynchronizationComponent`.
#[derive(Debug, Clone, Default)]
pub struct MeshSyncRegions {
    pub regions: Vec<Region>,
    /// `SynchronizedActorType` (`EBlamMeshSynchronizedActorType`):
    /// `0`=WorldRepresentation, `1`=FirstPersonRepresentation. `None` when the
    /// property is unserialized (i.e. the default, WorldRepresentation). Use
    /// [`Self::is_world`] to pick the world (third-person) BP over the `FP`/CINE
    /// variants that share the same data asset.
    pub synchronized_actor_type: Option<i64>,
}

const COMPONENT_CLASS: &str = "BlamMeshSynchronizationComponent";

impl MeshSyncRegions {
    /// Whether this is the world/third-person representation (the one a preview
    /// wants) rather than a first-person or cinematic actor.
    pub fn is_world(&self) -> bool {
        self.synchronized_actor_type.unwrap_or(0) == 0
    }
}

impl MeshSyncRegions {
    /// Decode the authoritative region→permutation→mesh mapping from a cooked
    /// `BlamMeshSynchronizationComponent` export's serial bytes. `names` is the
    /// owning package's name map (`FNameMap::copy_raw_names`).
    pub fn from_component_export(export: &[u8], names: &[String], usmap: &Usmap) -> Result<Self> {
        let mut r = Reader::new(export, names);
        let comp = read_struct(&mut r, COMPONENT_CLASS, usmap, 0)
            .context("decoding BlamMeshSynchronizationComponent properties")?;
        // Anything left after the property block must be zero padding; a
        // non-zero trailing byte means the schema-driven walk desynced.
        if let Some(tail) = export.get(r.o..) {
            if let Some(off) = tail.iter().position(|&b| b != 0) {
                bail!(
                    "unversioned parse desynced: {} non-zero trailing bytes from offset {}",
                    tail.len() - off,
                    r.o + off
                );
            }
        }
        let synchronized_actor_type = comp.get("SynchronizedActorType").and_then(|v| match v {
            PropValue::Int(n) => Some(*n),
            _ => None,
        });
        let runtime_regions = comp
            .get("RuntimeRegions")
            .and_then(PropValue::as_map)
            .context("component has no serialized RuntimeRegions")?;

        let mut regions = Vec::new();
        for (region_key, region_val) in runtime_regions {
            let region_name = region_key.as_str().unwrap_or_default().to_string();
            let perms_map = region_val
                .as_struct()
                .and_then(|s| s.get("Permutations"))
                .and_then(PropValue::as_map);
            let mut permutations = Vec::new();
            if let Some(perms_map) = perms_map {
                for (perm_key, perm_val) in perms_map {
                    let perm_name = perm_key.as_str().unwrap_or_default().to_string();
                    let perm_struct = perm_val.as_struct();
                    let skeletal_meshes = perm_struct
                        .and_then(|s| s.get("SkeletalMeshes"))
                        .map(|v| collect_meshes(v))
                        .unwrap_or_default();
                    let static_meshes = perm_struct
                        .and_then(|s| s.get("StaticMeshes"))
                        .map(|v| collect_meshes(v))
                        .unwrap_or_default();
                    permutations.push(Permutation { name: perm_name, skeletal_meshes, static_meshes });
                }
            }
            regions.push(Region { name: region_name, permutations });
        }
        Ok(MeshSyncRegions { regions, synchronized_actor_type })
    }

    /// The set of skeletal meshes to render for a given `(region, permutation)`.
    /// Returns an empty slice when the region/perm has no skeletal mesh (e.g.
    /// `head`/`helmet` on characters whose head is an external MetaHuman).
    pub fn skeletal_meshes(&self, region: &str, permutation: &str) -> &[MeshRef] {
        self.regions
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(region))
            .and_then(|r| r.permutations.iter().find(|p| p.name.eq_ignore_ascii_case(permutation)))
            .map(|p| p.skeletal_meshes.as_slice())
            .unwrap_or(&[])
    }

    /// The set of rigid static meshes (each with a `parent_bone`) for a given
    /// `(region, permutation)` — vehicle/weapon parts attached to the skeleton.
    pub fn static_meshes(&self, region: &str, permutation: &str) -> &[MeshRef] {
        self.regions
            .iter()
            .find(|r| r.name.eq_ignore_ascii_case(region))
            .and_then(|r| r.permutations.iter().find(|p| p.name.eq_ignore_ascii_case(permutation)))
            .map(|p| p.static_meshes.as_slice())
            .unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// UUserDefinedStruct layout recovery + UDataTable row decoding
// ---------------------------------------------------------------------------
//
// Blueprint-generated row structs (e.g. `S_MetaHumanHeads`) are absent from the
// native-reflection `.usmap`, so a cooked `UDataTable`'s rows can't be decoded
// until we recover the row struct's property layout from its
// `UUserDefinedStruct` export and register it into the [`Usmap`].
//
// A cooked `UUserDefinedStruct` export serializes as: a UObject unversioned
// header (its own reflected `FGuid`), then native `UStruct` data — `SuperStruct`
// (i32), an empty `Children` array, a pad word, then the `FField` chain
// (`numFields: i32` followed by that many properties). Each `FField` record is:
//
//   propTypeName: FName   (e.g. "SoftObjectProperty")
//   Name: FName           (e.g. "Head_4_<guid>")
//   Flags: u32
//   ArrayDim: i32
//   ElementSize: i32
//   PropertyFlags: u64
//   RepIndex: u16
//   RepNotifyFunc: FName
//   BlueprintReplicationCondition: u8
//   <type-specific tail>
//
// with the type-specific tail carrying inner properties for containers
// (`ArrayProperty`→Inner, `MapProperty`→Key+Value, `SetProperty`→Element, each a
// nested `FField`) or a class ref for object/struct properties.

/// Strip UE's `_<index>_<32-hex-guid>` decoration (and a trailing `_Value`/`_Key`
/// map-element marker) from a `UUserDefinedStruct` field name, recovering the
/// author-facing base name (`Head_4_BFCB…` → `Head`).
fn deguid(name: &str) -> String {
    let stripped = name
        .strip_suffix("_Value")
        .or_else(|| name.strip_suffix("_Key"))
        .unwrap_or(name);
    let parts: Vec<&str> = stripped.split('_').collect();
    if parts.len() >= 3 {
        let guid = parts[parts.len() - 1];
        let idx = parts[parts.len() - 2];
        if guid.len() == 32
            && guid.bytes().all(|c| c.is_ascii_hexdigit())
            && !idx.is_empty()
            && idx.bytes().all(|c| c.is_ascii_digit())
        {
            return parts[..parts.len() - 2].join("_");
        }
    }
    stripped.to_string()
}

/// Read one `FField` (a `SerializeSingleField`): its type-name FName, then the
/// common `FProperty` header, then the type-specific tail. Returns
/// `(field_name, PropertyType, array_dim)`, or `None` for the `None`
/// terminator/null field.
fn read_single_field(r: &mut Reader) -> Result<Option<(String, PropertyType, u8)>> {
    let type_name = r.name()?;
    if type_name == "None" {
        return Ok(None);
    }
    let field_name = r.name()?;
    let _flags = r.u32()?;
    let array_dim = r.i32()?;
    let _element_size = r.i32()?;
    let _property_flags = r.u64()?;
    let _rep_index = r.u16()?;
    let _rep_notify = r.name()?;
    let _bp_rep_cond = r.u8()?;
    let ty = read_ffield_tail(r, &type_name)?;
    Ok(Some((field_name, ty, array_dim.clamp(1, 255) as u8)))
}

/// The `FProperty`-subclass-specific serialized tail, mapped to the [`PropertyType`]
/// the unversioned value reader understands.
fn read_ffield_tail(r: &mut Reader, type_name: &str) -> Result<PropertyType> {
    Ok(match type_name {
        "BoolProperty" => {
            // FieldSize, ByteOffset, ByteMask, FieldMask, BoolSize, bIsNativeBool.
            r.take(6)?;
            PropertyType::Bool
        }
        "SoftObjectProperty" | "SoftClassProperty" => {
            r.i32()?; // PropertyClass
            PropertyType::SoftObject
        }
        "AssetObjectProperty" => {
            r.i32()?;
            PropertyType::AssetObject
        }
        "ObjectProperty" | "ObjectPtrProperty" | "ClassProperty" | "ClassPtrProperty" => {
            r.i32()?;
            PropertyType::Object
        }
        "WeakObjectProperty" => {
            r.i32()?;
            PropertyType::WeakObject
        }
        "LazyObjectProperty" => {
            r.i32()?;
            PropertyType::LazyObject
        }
        "InterfaceProperty" => {
            r.i32()?;
            PropertyType::Interface
        }
        "StructProperty" => {
            r.i32()?; // Struct object ref — name unresolved (unused by our rows).
            PropertyType::Struct(String::new())
        }
        "ByteProperty" => {
            r.i32()?; // Enum object ref
            PropertyType::Byte { enum_name: None }
        }
        "EnumProperty" => {
            let inner = read_single_field(r)?
                .map(|(_, t, _)| t)
                .unwrap_or(PropertyType::Byte { enum_name: None });
            r.i32()?; // Enum object ref
            PropertyType::Enum { inner: Box::new(inner), enum_name: String::new() }
        }
        "ArrayProperty" => {
            let inner = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("ArrayProperty inner missing")?;
            PropertyType::Array(Box::new(inner))
        }
        "SetProperty" => {
            let elem = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("SetProperty element missing")?;
            PropertyType::Set(Box::new(elem))
        }
        "MapProperty" => {
            let key = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("MapProperty key missing")?;
            let val = read_single_field(r)?
                .map(|(_, t, _)| t)
                .context("MapProperty value missing")?;
            PropertyType::Map(Box::new(key), Box::new(val))
        }
        "StrProperty" => PropertyType::Str,
        "NameProperty" => PropertyType::Name,
        "TextProperty" => PropertyType::Text,
        "IntProperty" => PropertyType::Int,
        "Int8Property" => PropertyType::Int8,
        "Int16Property" => PropertyType::Int16,
        "Int64Property" => PropertyType::Int64,
        "UInt16Property" => PropertyType::UInt16,
        "UInt32Property" => PropertyType::UInt32,
        "UInt64Property" => PropertyType::UInt64,
        "FloatProperty" => PropertyType::Float,
        "DoubleProperty" => PropertyType::Double,
        "FieldPathProperty" => {
            r.name()?; // PropertyClass FName
            PropertyType::FieldPath
        }
        "DelegateProperty"
        | "MulticastInlineDelegateProperty"
        | "MulticastSparseDelegateProperty" => {
            r.i32()?; // SignatureFunction
            PropertyType::Delegate
        }
        other => bail!("unhandled FProperty class '{other}' in UserDefinedStruct layout"),
    })
}

/// Extract the serialized Kismet **bytecode blob** of a `UFunction` export. A
/// `UFunction` serializes as a `UStruct` (UObject unversioned header, then native
/// `SuperStruct` i32, two empty arrays, `numFields` + the `FField` param/local
/// chain) followed by the script: `ScriptBytecodeSize`(i32),
/// `ScriptStorageSize`(i32), then that many bytes of `SerializeExpr`-encoded
/// bytecode (object/name refs inline). Returns the raw storage bytes for a
/// disassembler to walk. `names` is the package name map.
pub fn read_ufunction_script(export: &[u8], names: &[String]) -> Result<Vec<u8>> {
    let mut r = Reader::new(export, names);
    let present = read_header(&mut r)?; // UObject block (usually empty for a UFunction)
    for (_, non_zero) in &present {
        if *non_zero {
            r.take(16)?;
        }
    }
    // Native UStruct prefix, then the script. The two i32s after SuperStruct are
    // empty arrays (Children + script/property-object refs); probe a couple of
    // offsets for the `numFields` that yields a fully-parsing FField chain.
    let base = r.o;
    let mut last_err: Option<anyhow::Error> = None;
    for pad in [3usize, 2, 4, 1, 0] {
        r.o = base + pad * 4;
        match try_read_script(&mut r) {
            Ok(blob) if !blob.is_empty() => return Ok(blob),
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no UFunction script found")))
}

fn try_read_script(r: &mut Reader) -> Result<Vec<u8>> {
    let num = r.i32()?;
    if !(0..=4096).contains(&num) {
        bail!("implausible numFields {num}");
    }
    for _ in 0..num {
        // A wrong offset yields an unknown FProperty class → error, rejecting it.
        read_single_field(r)?;
    }
    let _bytecode_size = r.i32()?;
    let storage = r.i32()?;
    if !(1..=16_000_000).contains(&storage) {
        bail!("implausible ScriptStorageSize {storage}");
    }
    Ok(r.take(storage as usize)?.to_vec())
}

/// Decode a cooked object export's unversioned property block for a known
/// native `class` (present in the `.usmap`), returning present property
/// name→value. General entry point for simple UObject exports (e.g.
/// `SkeletalMeshSocket`) whose serial data is just their reflected properties.
pub fn read_export_struct(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<BTreeMap<String, PropValue>> {
    let mut r = Reader::new(export, names);
    read_struct(&mut r, class, usmap, 0)
}

/// Recover a `UUserDefinedStruct`'s property layout (in serialization order,
/// `schema_index` assigned) from its cooked export bytes, ready to
/// [`Usmap::register_struct`]. `names` is the owning package's name map.
pub fn read_userdefined_struct_layout(export: &[u8], names: &[String]) -> Result<Vec<UsmapProperty>> {
    let mut r = Reader::new(export, names);
    // UObject unversioned header: the struct's own reflected properties. On a
    // cooked UUserDefinedStruct each present (non-zero) value is an FGuid.
    let present = read_header(&mut r)?;
    for (_, non_zero) in &present {
        if *non_zero {
            r.take(16)?; // FGuid
        }
    }
    // Native UStruct prefix (SuperStruct, empty Children, pad) precedes the
    // FField chain by a small, layout-stable number of words. Rather than
    // hard-code it, probe the next few i32 slots for a `numFields` that yields a
    // clean chain — a wrong offset almost never parses (type names must resolve
    // to known FProperty classes and field names to valid FNames).
    let base = r.o;
    let mut last_err: Option<anyhow::Error> = None;
    for pad in 0..=4usize {
        r.o = base + pad * 4;
        match try_parse_field_chain(&mut r) {
            Ok(props) if !props.is_empty() => return Ok(props),
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no FField chain found in UserDefinedStruct")))
}

fn try_parse_field_chain(r: &mut Reader) -> Result<Vec<UsmapProperty>> {
    let num = r.i32()?;
    if !(1..=1024).contains(&num) {
        bail!("implausible numFields {num}");
    }
    let mut props = Vec::with_capacity(num as usize);
    for i in 0..num {
        let (name, ty, array_dim) =
            read_single_field(r)?.with_context(|| format!("null field at index {i}"))?;
        props.push(UsmapProperty { schema_index: i as u16, array_dim, name: deguid(&name), ty });
    }
    Ok(props)
}

/// Decode a cooked `UDataTable`'s rows into `(row key, field→value)` pairs.
/// `row_struct` must already be registered in `usmap` (see
/// [`read_userdefined_struct_layout`] + [`Usmap::register_struct`]).
pub fn read_datatable(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    row_struct: &str,
) -> Result<Vec<(String, BTreeMap<String, PropValue>)>> {
    let mut r = Reader::new(export, names);
    // The DataTable's own reflected block (RowStruct ref, import flags, …).
    let _dt = read_struct(&mut r, "DataTable", usmap, 0).context("DataTable header block")?;
    // Rows follow: `NumRows: i32`, then `(FName key, row struct)` back-to-back.
    // A pad word can sit between the reflected block and NumRows; probe for the
    // offset that decodes every row cleanly with no trailing garbage.
    let base = r.o;
    let mut last_err: Option<anyhow::Error> = None;
    for pad in 0..=2usize {
        r.o = base + pad * 4;
        match try_read_rows(&mut r, usmap, row_struct, export.len()) {
            Ok(rows) if !rows.is_empty() => return Ok(rows),
            Ok(_) => {}
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("no DataTable rows decoded")))
}

fn try_read_rows(
    r: &mut Reader,
    usmap: &Usmap,
    row_struct: &str,
    export_len: usize,
) -> Result<Vec<(String, BTreeMap<String, PropValue>)>> {
    let num = r.i32()?;
    if !(1..=1_000_000).contains(&num) {
        bail!("implausible NumRows {num}");
    }
    let mut rows = Vec::with_capacity(num as usize);
    for i in 0..num {
        let key = r.name()?;
        let row = read_struct(r, row_struct, usmap, 0).with_context(|| format!("row {i} ({key})"))?;
        rows.push((key, row));
    }
    // Rows are the last payload; anything left must be zero padding, else this
    // probe locked onto the wrong NumRows offset.
    if let Some(tail) = r.b.get(r.o..export_len) {
        if tail.iter().any(|&b| b != 0) {
            bail!("{} trailing non-zero bytes after {} rows", tail.len(), num);
        }
    }
    Ok(rows)
}

/// Turn a `TArray<FBlamMeshSynchronizationRuntime{Skeletal,Static}Mesh>` value
/// into flat [`MeshRef`]s, dropping entries with an empty asset path.
fn collect_meshes(value: &PropValue) -> Vec<MeshRef> {
    let Some(items) = value.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in items {
        let Some(s) = item.as_struct() else { continue };
        let asset = s.get("Asset").and_then(PropValue::as_soft_object);
        let Some(asset) = asset else { continue };
        if asset.is_empty() {
            continue;
        }
        let class = s
            .get("Class")
            .and_then(PropValue::as_soft_object)
            .map(|c| c.asset.clone())
            .unwrap_or_default();
        let parent_bone = s
            .get("ParentBoneName")
            .and_then(PropValue::as_str)
            .unwrap_or_default()
            .to_string();
        let rel_transform = s
            .get("Transform")
            .and_then(MeshTransform::from_prop)
            .unwrap_or_default();
        let material_overrides = s
            .get("MaterialOverrides")
            .and_then(PropValue::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        let os = o.as_struct()?;
                        let slot = os.get("MaterialSlotName").and_then(PropValue::as_str)?;
                        let mi = os.get("OverrideMaterial").and_then(PropValue::as_soft_object)?;
                        if mi.is_empty() {
                            return None;
                        }
                        let name = if !mi.asset.is_empty() {
                            mi.asset.clone()
                        } else {
                            mi.package.rsplit('/').next().unwrap_or(&mi.package).to_string()
                        };
                        Some((slot.to_string(), name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(MeshRef {
            package: asset.package.clone(),
            asset: asset.asset.clone(),
            class,
            parent_bone,
            rel_transform,
            material_overrides,
        });
    }
    out
}
