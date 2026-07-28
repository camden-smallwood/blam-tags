//! Campaign Evolved's authoritative region→permutation→mesh mapping, read out
//! of a cooked `BlamMeshSynchronizationComponent` export.

use anyhow::{bail, Context, Result};

use crate::iostore::object::unversioned::{
    read_export_struct, read_export_struct_len, MeshTransform, PropValue,
};
use crate::iostore::object::usmap::Usmap;

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
    let s = read_export_struct(export, names, usmap, class)
        .with_context(|| format!("decoding {class} material slots"))?;
    // `UStaticMesh` names the array `StaticMaterials`; `USkeletalMesh` names it
    // plain `Materials` in this engine version (`SkeletalMaterials` is the older
    // spelling). Missing the live name silently yields zero slots, which reads as
    // "this mesh has no materials" rather than as a decode failure.
    let arr_names: &[&str] =
        if is_skeletal { &["Materials", "SkeletalMaterials"] } else { &["StaticMaterials", "Materials"] };
    let Some(arr) = arr_names.iter().find_map(|n| s.get(*n)).and_then(PropValue::as_array) else {
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
        let (comp, used) = read_export_struct_len(export, names, usmap, COMPONENT_CLASS)
            .context("decoding BlamMeshSynchronizationComponent properties")?;
        // Anything left after the property block must be zero padding; a
        // non-zero trailing byte means the schema-driven walk desynced.
        if let Some(tail) = export.get(used..) {
            if let Some(off) = tail.iter().position(|&b| b != 0) {
                bail!(
                    "unversioned parse desynced: {} non-zero trailing bytes from offset {}",
                    tail.len() - off,
                    used + off
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
                        let name = if !mi.asset.as_str().is_empty() {
                            mi.asset.as_str().to_string()
                        } else {
                            mi.package.rsplit('/').next().unwrap_or(&mi.package).to_string()
                        };
                        Some((slot.to_string(), name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(MeshRef {
            package: asset.package.as_str().to_string(),
            asset: asset.asset.as_str().to_string(),
            class: class.as_str().to_string(),
            parent_bone,
            rel_transform,
            material_overrides,
        });
    }
    out
}
