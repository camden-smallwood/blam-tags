//! Class tails as *models* rather than retained spans.
//!
//! [`super::tails`] already walks every tail exactly — that is what makes the
//! coverage matrix account for all 1,153,838 exports — but it walks them as a
//! *skipper*, keeping nothing. So an export round-trips only because its tail is
//! copied through verbatim. This module is the other half: the same layout,
//! decoded into values and written back out of them.
//!
//! It is the same transformation Phase 1 applied to property values, and it is
//! done **one class at a time, on demand**. Each conversion is verifiable in a
//! way a new format model normally is not: the bytes it must reproduce are
//! already there, so `ce_tail_model_roundtrip` checks the model against the span
//! it would replace rather than against anyone's reading of the engine.
//!
//! `ce_tail_census` says where the bytes are, and two orderings fall out of it:
//! a few classes hold most of the 4.77 GiB, while `StaticMeshComponent` and
//! `StaticMeshActor` are 196,000 exports with medians of 16 and 79 bytes. This
//! started with the latter, because a cheap conversion covering a sixth of all
//! exports proves the pattern before anything expensive depends on it, and then
//! took the heavy classes in order of bytes.
//!
//! Converted so far — 351,018 tails, 2.51 GiB, all byte-exact: the
//! static-mesh-component family, the instanced-static-mesh family, every cooked
//! texture shape, every material shape, and `UBodySetup`. What is left is
//! `StaticMesh` (Nanite), `SkeletalMesh`, `AnimSequence` (ACL) and
//! `GeometryCollection`.
//!
//! # Payloads another serializer owns
//!
//! Several of these tails end in a blob a *different* serializer produced: a
//! texture mip's block-compressed pixels, a material's compiled shader map,
//! `UBodySetup`'s Chaos physics data. Those stay byte strings here, and that is
//! not the model quietly retaining a span — they are leaf data, not an encoding
//! of some richer value this layer could recover and re-emit. What the model
//! owes them is *addressing*: which format, which bulk-data index, how long.
//! Decoding them is its own work item.
//!
//! # A tail is a chain, and parts of it are property-dependent
//!
//! An export's tail is not one class's data — it is every class in the
//! inheritance chain appending its own, base to derived. A `UStaticMeshComponent`
//! export's 16 bytes are `UActorComponent`'s 4, `USceneComponent`'s 4 and
//! `UStaticMeshComponent`'s 8. Modeling "the StaticMeshComponent tail" in
//! isolation reads the wrong 8 bytes and reports success, which is exactly what
//! the first version of this module did before the gate caught it.
//!
//! No model here derives `Eq`. Nearly all of them reach a float sooner or later
//! — bounds, weights, barycentric coordinates — so equality is `PartialEq` and
//! byte-level comparison is the gate's job, not the type's.
//!
//! Worse for a writer: some of it is conditional on *property values*.
//! `USceneComponent` writes its baked bounds only when `bComputeBoundsOnceForGame`
//! is set, so the tail cannot be written without the property block — the two
//! are not independent, and any tail model has to take the block as an input.

use anyhow::{bail, Context, Result};

use super::archive::{Ar, Reader};
use super::block::{flattened_schema, read_struct, write_block};
use super::common::read_bulk_array;
use super::limits::MAX_NATIVE_COUNT;
use super::ue_struct::{
    bounded_count, GeometryCollectionMeshElement, GeometryCollectionSection, Int32Vector,
    LinearColor, Vector2d, Vector2f, Vector3f, PrecomputedVisibilityCell, Transform, Vector4f, BspSurf, HashedName, MeshUvChannelInfo, ModelVertex, MorphTargetDelta, ClothBufferIndexMapping, DuplicatedVertexIndex, EntryToValueKey,
    GrassWeightOffset, MemoryImageTypeDependency, PlatformTypeLayoutParameters,
    UcsModifiedProperty, VTablePatch, Box3d, Box3f, LightmassPrimitiveSettings, MeshToMeshVertData, PerPlatformFloat,
    SparseDistanceFieldMip, StaticMeshBuffersSize, LumenCardBuildData, PackedHierarchyNode, PageStreamingState, read_vec, write_run, write_vec, BoxSphereBounds, ClothingSectionData, FuncMapEntry,
    Guid, ImplementedInterface, MeshBoneInfo, NameToIndex, ShaHash, StaticMaterial,
    StaticMeshSection, StripDataFlags,
};
use super::usmap::Usmap;
use super::value::{FName, FStr, PropValue, PropertyBlock};

/// A `TArray` written with `BulkSerialize`: element size, count, then
/// `count × size` blittable bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct BulkArray {
    pub element_size: i32,
    pub data: Vec<u8>,
}

impl BulkArray {
    fn read(r: &mut Reader, what: &str) -> Result<Self> {
        let at = r.o;
        let element_size = r.b.get(at..at + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap()));
        let count = read_bulk_array(r, what)?;
        let element_size = element_size.unwrap_or(0);
        // `read_bulk_array` has already validated and consumed the whole run;
        // take the payload back out of the span it covered.
        let data_start = at + 8;
        let data_len = count * element_size.max(0) as usize;
        Ok(BulkArray {
            element_size,
            data: r.b[data_start..data_start + data_len].to_vec(),
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        if self.element_size > 0 && self.data.len() % self.element_size as usize != 0 {
            bail!(
                "bulk array of {}-byte elements has {} bytes, not a whole number of elements",
                self.element_size,
                self.data.len()
            );
        }
        ar.i32(&mut self.element_size.to_owned())?;
        let count = if self.element_size > 0 {
            (self.data.len() / self.element_size as usize) as i32
        } else {
            0
        };
        ar.i32(&mut count.to_owned())?;
        let n = self.data.len();
        ar.raw(&mut self.data.clone(), n)
    }
}

/// An `FColorVertexBuffer` inside a component's per-LOD override colours.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorVertexBuffer {
    pub global_strip: u8,
    pub class_strip: u8,
    pub stride: i32,
    pub num_vertices: i32,
    /// The colours, present only when there are vertices *and* audio-visual
    /// data survived stripping.
    pub colors: Option<BulkArray>,
}

/// One entry of `UStaticMeshComponent::LODData`.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshComponentLodInfo {
    pub global_strip: u8,
    pub class_strip: u8,
    /// `MapBuildDataId` then `OriginalMapBuildDataId`, written only when
    /// audio-visual data was not stripped (global strip bit 1).
    pub map_build_data: Option<[u8; 32]>,
    /// Three states, and they are genuinely distinct on the wire:
    /// `None` — the class strip flags say override colours are gone, so not
    /// even a flag byte is written; `Some(None)` — the flag is written and
    /// zero; `Some(Some(..))` — the flag is one and a buffer follows.
    pub vertex_colors: Option<Option<ColorVertexBuffer>>,
}

/// The `UStaticMeshComponent` tail: 126,158 exports, median 16 bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshComponentTail {
    pub lod_data: Vec<StaticMeshComponentLodInfo>,
    /// `MeshPaintTextureCooked`, behind its own four-byte present flag.
    pub mesh_paint_texture: Option<i32>,
}

impl StaticMeshComponentTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "LODData", r.o - 4)?
        };
        let mut lod_data = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            lod_data.push(read_lod_info(r)?);
        }
        let mesh_paint_texture = if r.u32()? != 0 { Some(r.i32()?) } else { None };
        Ok(StaticMeshComponentTail { lod_data, mesh_paint_texture })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut (self.lod_data.len() as i32))?;
        for lod in &self.lod_data {
            write_lod_info(ar, lod)?;
        }
        match self.mesh_paint_texture {
            Some(v) => {
                ar.u32(&mut 1)?;
                ar.i32(&mut v.to_owned())
            }
            None => ar.u32(&mut 0),
        }
    }
}

fn read_lod_info(r: &mut Reader) -> Result<StaticMeshComponentLodInfo> {
    let global_strip = r.u8()?;
    let class_strip = r.u8()?;
    // Bit 1 = audio/visual data stripped.
    let map_build_data = if global_strip & 2 == 0 {
        Some(r.take(32)?.try_into().expect("32 bytes"))
    } else {
        None
    };
    let vertex_colors = if class_strip & 1 != 0 {
        None
    } else if r.u8()? != 1 {
        Some(None)
    } else {
        let cb_global = r.u8()?;
        let cb_class = r.u8()?;
        let stride = r.i32()?;
        let num_vertices = r.i32()?;
        let colors = if num_vertices > 0 && cb_global & 2 == 0 {
            Some(BulkArray::read(r, "OverrideVertexColors")?)
        } else {
            None
        };
        Some(Some(ColorVertexBuffer {
            global_strip: cb_global,
            class_strip: cb_class,
            stride,
            num_vertices,
            colors,
        }))
    };
    Ok(StaticMeshComponentLodInfo { global_strip, class_strip, map_build_data, vertex_colors })
}

fn write_lod_info(ar: &mut impl Ar, lod: &StaticMeshComponentLodInfo) -> Result<()> {
    ar.u8(&mut lod.global_strip.to_owned())?;
    ar.u8(&mut lod.class_strip.to_owned())?;
    match (&lod.map_build_data, lod.global_strip & 2 == 0) {
        (Some(id), true) => ar.raw(&mut id.to_vec(), 32)?,
        (None, false) => {}
        // The strip flags decide whether these bytes exist, so a model that
        // disagrees with its own flags would write a different length than it
        // read. Fail rather than silently pick one.
        _ => bail!("map build data presence disagrees with the strip flags"),
    }
    match (&lod.vertex_colors, lod.class_strip & 1 != 0) {
        (None, true) => {}
        (Some(v), false) => {
            match v {
                None => ar.u8(&mut 0)?,
                Some(cb) => {
                    ar.u8(&mut 1)?;
                    ar.u8(&mut cb.global_strip.to_owned())?;
                    ar.u8(&mut cb.class_strip.to_owned())?;
                    ar.i32(&mut cb.stride.to_owned())?;
                    ar.i32(&mut cb.num_vertices.to_owned())?;
                    match (&cb.colors, cb.num_vertices > 0 && cb.global_strip & 2 == 0) {
                        (Some(a), true) => a.write(ar)?,
                        (None, false) => {}
                        _ => bail!("override colour presence disagrees with the buffer's flags"),
                    }
                }
            }
        }
        _ => bail!("vertex colour presence disagrees with the strip flags"),
    }
    Ok(())
}

/// `UActorComponent`'s tail: the sparse UCS-modified-property list, each entry
/// an `FPackageIndex`, an `FName` and an `FGuid` — 28 bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorComponentTail {
    pub ucs_modified_properties: Vec<UcsModifiedProperty>,
}

/// `USceneComponent`'s tail: baked bounds, written only when the component asked
/// for them to be computed once for game.
///
/// `None` means the property flag was clear, so *nothing* is written — not even
/// the four-byte present flag. That distinction is the whole difficulty: the
/// bytes that exist depend on a value in the property block.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneComponentTail {
    pub bounds: Option<Option<[u8; 56]>>,
}

/// The whole tail of a `UStaticMeshComponent` export, chain and all.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshComponentChainTail {
    pub actor_component: ActorComponentTail,
    pub scene_component: SceneComponentTail,
    pub static_mesh_component: StaticMeshComponentTail,
}

/// Whether `USceneComponent` writes anything, which only the property block
/// knows. Mirrors the condition in [`super::tails::read_class_native_tail`].
fn scene_component_writes_bounds(block: &PropertyBlock) -> bool {
    let flag = |name: &str| matches!(block.get(name), Some(PropValue::Bool(true)));
    flag("bComputeBoundsOnceForGame") || flag("bComputedBoundsOnceForGame")
}

/// `UActorComponent` + `USceneComponent` — the tail of any scene component that
/// appends nothing of its own.
///
/// That is most of them: 42 classes and 151,249 exports in this corpus, from
/// `USpotLightComponent` to `UHaloAudioPlacementComponent`, whose whole tail is
/// these two layers.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneComponentChainTail {
    pub actor_component: ActorComponentTail,
    pub scene_component: SceneComponentTail,
}

impl SceneComponentChainTail {
    pub fn read(r: &mut Reader, block: &PropertyBlock) -> Result<Self> {
        let n = bounded_count(r.i32()?, "UCSModifiedProperties", r.o - 4)?;
        let ucs_modified_properties: Vec<UcsModifiedProperty> =
            read_vec(r, "UCSModifiedProperties", n)?;
        let bounds = if scene_component_writes_bounds(block) {
            Some(if r.u32()? != 0 {
                Some(r.take(56)?.try_into().expect("56 bytes"))
            } else {
                None
            })
        } else {
            None
        };
        Ok(SceneComponentChainTail {
            actor_component: ActorComponentTail { ucs_modified_properties },
            scene_component: SceneComponentTail { bounds },
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock) -> Result<()> {
        write_vec(ar, &self.actor_component.ucs_modified_properties)?;
        // The property block decides whether these bytes exist, so a model that
        // disagrees with it would change the tail's length.
        match (&self.scene_component.bounds, scene_component_writes_bounds(block)) {
            (Some(b), true) => match b {
                Some(bounds) => {
                    ar.u32(&mut 1)?;
                    ar.raw(&mut bounds.to_vec(), 56)?;
                }
                None => ar.u32(&mut 0)?,
            },
            (None, false) => {}
            _ => bail!("scene component bounds disagree with the property block"),
        }
        Ok(())
    }
}

impl StaticMeshComponentChainTail {
    pub fn read(r: &mut Reader, block: &PropertyBlock) -> Result<Self> {
        let base = SceneComponentChainTail::read(r, block)?;
        Ok(StaticMeshComponentChainTail {
            actor_component: base.actor_component,
            scene_component: base.scene_component,
            static_mesh_component: StaticMeshComponentTail::read(r)?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock) -> Result<()> {
        write_vec(ar, &self.actor_component.ucs_modified_properties)?;

        // The property block decides whether these bytes exist at all, so a
        // model that disagrees with it would change the tail's length.
        match (&self.scene_component.bounds, scene_component_writes_bounds(block)) {
            (Some(b), true) => match b {
                Some(bounds) => {
                    ar.u32(&mut 1)?;
                    ar.raw(&mut bounds.to_vec(), 56)?;
                }
                None => ar.u32(&mut 0)?,
            },
            (None, false) => {}
            _ => bail!("scene component bounds disagree with the property block"),
        }

        self.static_mesh_component.write(ar)
    }
}

/// A `TArray` written with `BulkSerialize` whose element type is *known*, so it
/// decodes into values instead of a span.
///
/// The declared element size is kept as a field rather than re-derived on write.
/// It is real data in the stream, and an empty array still carries one — a
/// cooked component with no instances writes a size the model has to reproduce,
/// and deriving it from `items` would be guessing at a width nothing observed.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedBulkArray<T> {
    pub element_size: i32,
    pub items: Vec<T>,
}

impl<T> TypedBulkArray<T> {
    /// Read `count × element_size` bytes, decoding each element with `f`.
    ///
    /// `expect` is the element width the decoder produces. A mismatch is an
    /// error rather than a fallback: the stream is self-describing here, so a
    /// different width means the element is not what this model thinks it is,
    /// and decoding it anyway would silently produce wrong values.
    fn read(
        r: &mut Reader,
        what: &str,
        expect: i32,
        f: impl Fn(&[u8]) -> T,
    ) -> Result<Self> {
        let at = r.o;
        let element_size =
            r.b.get(at..at + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap())).unwrap_or(0);
        let count = read_bulk_array(r, what)?;
        if count > 0 && element_size != expect {
            bail!("{what} has {count} elements of {element_size} bytes, expected {expect}");
        }
        let start = at + 8;
        let items = (0..count)
            .map(|i| {
                let o = start + i * element_size.max(0) as usize;
                f(&r.b[o..o + element_size.max(0) as usize])
            })
            .collect();
        Ok(TypedBulkArray { element_size, items })
    }

    fn write(&self, ar: &mut impl Ar, f: impl Fn(&T) -> Vec<u8>) -> Result<()> {
        ar.i32(&mut self.element_size.to_owned())?;
        ar.i32(&mut (self.items.len() as i32))?;
        for it in &self.items {
            let mut b = f(it);
            if b.len() != self.element_size.max(0) as usize {
                bail!(
                    "element encoded to {} bytes but the array declares {}",
                    b.len(),
                    self.element_size
                );
            }
            let n = b.len();
            ar.raw(&mut b, n)?;
        }
        Ok(())
    }
}

/// `FInstancedStaticMeshInstanceData::Transform` — an `FMatrix`, which at UE5
/// large-world-coordinate precision is 16 `double`s, hence the 128-byte elements
/// every cooked component in the corpus declares.
pub type InstanceTransform = [f64; 16];

fn read_matrix(b: &[u8]) -> InstanceTransform {
    let mut m = [0f64; 16];
    for (i, s) in m.iter_mut().enumerate() {
        *s = f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().expect("8 bytes"));
    }
    m
}

fn write_matrix(m: &InstanceTransform) -> Vec<u8> {
    m.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// `UInstancedStaticMeshComponent`'s own tail: the per-instance transforms and
/// custom float data, plus the cooked render buffers.
///
/// Measured over all 150,779 exports of the three ISMC classes: the transforms
/// are 128-byte `FMatrix`es (3,425,245 of them) and the custom data is
/// 4-byte floats (4,588,932). See `ce_ismc_probe`.
#[derive(Debug, Clone, PartialEq)]
pub struct InstancedStaticMeshComponentTail {
    pub cooked: bool,
    /// The authoring instance buffers, written when the component did not skip
    /// serializing them.
    pub instances: Option<InstanceBuffers>,
    /// The cooked render buffers. Empty in every export measured, so the element
    /// type is not yet known and these stay `BulkArray` — the one place in this
    /// model that would retain bytes, and it has never had any to retain.
    pub render: Option<Option<(BulkArray, BulkArray)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstanceBuffers {
    pub transforms: TypedBulkArray<InstanceTransform>,
    pub custom_data: TypedBulkArray<f32>,
}

impl InstancedStaticMeshComponentTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let cooked = r.u32()? != 0;
        let instances = if r.u32()? != 0 {
            Some(InstanceBuffers {
                transforms: TypedBulkArray::read(r, "PerInstanceSMData", 128, read_matrix)?,
                custom_data: TypedBulkArray::read(r, "PerInstanceSMCustomData", 4, |b| {
                    f32::from_le_bytes(b.try_into().expect("4 bytes"))
                })?,
            })
        } else {
            None
        };
        let render = if cooked {
            Some(if r.u32()? != 0 {
                Some((
                    BulkArray::read(r, "instance render data")?,
                    BulkArray::read(r, "instance render data")?,
                ))
            } else {
                None
            })
        } else {
            None
        };
        Ok(InstancedStaticMeshComponentTail { cooked, instances, render })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut u32::from(self.cooked))?;
        match &self.instances {
            Some(b) => {
                ar.u32(&mut 1)?;
                b.transforms.write(ar, write_matrix)?;
                b.custom_data.write(ar, |v| v.to_le_bytes().to_vec())?;
            }
            None => ar.u32(&mut 0)?,
        }
        // `cooked` decides whether the render flag exists at all, so a model
        // that disagrees with itself would write a different length.
        match (&self.render, self.cooked) {
            (Some(r), true) => match r {
                Some((a, b)) => {
                    ar.u32(&mut 1)?;
                    a.write(ar)?;
                    b.write(ar)?;
                }
                None => ar.u32(&mut 0)?,
            },
            (None, false) => {}
            _ => bail!("render buffer presence disagrees with the cooked flag"),
        }
        Ok(())
    }
}

/// One `FClusterNode` of a hierarchical component's cluster tree
/// (HierarchicalInstancedStaticMeshComponent.h:71, 5.5.4). Bulk-serialized as a
/// memory dump, so the 64 bytes are exactly these fields in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClusterNode {
    pub bound_min: [f32; 3],
    pub first_child: i32,
    pub bound_max: [f32; 3],
    pub last_child: i32,
    pub first_instance: i32,
    pub last_instance: i32,
    pub min_instance_scale: [f32; 3],
    pub max_instance_scale: [f32; 3],
}

impl ClusterNode {
    pub const SIZE: i32 = 64;

    fn read(b: &[u8]) -> Self {
        let f = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"));
        let i = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().expect("4 bytes"));
        let v = |o: usize| [f(o), f(o + 4), f(o + 8)];
        ClusterNode {
            bound_min: v(0),
            first_child: i(12),
            bound_max: v(16),
            last_child: i(28),
            first_instance: i(32),
            last_instance: i(36),
            min_instance_scale: v(40),
            max_instance_scale: v(52),
        }
    }

    fn write(&self) -> Vec<u8> {
        let mut o = Vec::with_capacity(64);
        let mut v = |a: &[f32; 3]| a.iter().for_each(|x| o.extend_from_slice(&x.to_le_bytes()));
        v(&self.bound_min);
        o.extend_from_slice(&self.first_child.to_le_bytes());
        let mut v = |a: &[f32; 3]| a.iter().for_each(|x| o.extend_from_slice(&x.to_le_bytes()));
        v(&self.bound_max);
        o.extend_from_slice(&self.last_child.to_le_bytes());
        o.extend_from_slice(&self.first_instance.to_le_bytes());
        o.extend_from_slice(&self.last_instance.to_le_bytes());
        let mut v = |a: &[f32; 3]| a.iter().for_each(|x| o.extend_from_slice(&x.to_le_bytes()));
        v(&self.min_instance_scale);
        v(&self.max_instance_scale);
        o
    }
}

/// `UHierarchicalInstancedStaticMeshComponent`'s tail: the cluster tree.
///
/// It sits *between* `UInstancedStaticMeshComponent` and
/// `UFoliageInstancedStaticMeshComponent` in the chain, which is why a model
/// built from the ISMC layer alone came up exactly 8 bytes — one empty bulk
/// array — short on every foliage export.
#[derive(Debug, Clone, PartialEq)]
pub struct HierarchicalInstancedStaticMeshComponentTail {
    pub cluster_tree: TypedBulkArray<ClusterNode>,
}

/// The whole tail of an `UInstancedStaticMeshComponent` export.
///
/// `UPrimitiveComponent` and `UMeshComponent` write nothing. `hierarchical` is
/// present only for the classes that descend through
/// `UHierarchicalInstancedStaticMeshComponent`.
#[derive(Debug, Clone, PartialEq)]
pub struct InstancedStaticMeshComponentChainTail {
    pub actor_component: ActorComponentTail,
    pub scene_component: SceneComponentTail,
    pub static_mesh_component: StaticMeshComponentTail,
    pub instanced: InstancedStaticMeshComponentTail,
    pub hierarchical: Option<HierarchicalInstancedStaticMeshComponentTail>,
}

impl InstancedStaticMeshComponentChainTail {
    pub fn read(r: &mut Reader, block: &PropertyBlock, hierarchical: bool) -> Result<Self> {
        let base = StaticMeshComponentChainTail::read(r, block)?;
        let instanced = InstancedStaticMeshComponentTail::read(r)?;
        let hierarchical = hierarchical
            .then(|| {
                TypedBulkArray::read(r, "ClusterTree", ClusterNode::SIZE, ClusterNode::read)
                    .map(|cluster_tree| HierarchicalInstancedStaticMeshComponentTail {
                        cluster_tree,
                    })
            })
            .transpose()?;
        Ok(InstancedStaticMeshComponentChainTail {
            actor_component: base.actor_component,
            scene_component: base.scene_component,
            static_mesh_component: base.static_mesh_component,
            instanced,
            hierarchical,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock) -> Result<()> {
        StaticMeshComponentChainTail {
            actor_component: self.actor_component.clone(),
            scene_component: self.scene_component.clone(),
            static_mesh_component: self.static_mesh_component.clone(),
        }
        .write(ar, block)?;
        self.instanced.write(ar)?;
        if let Some(h) = &self.hierarchical {
            h.cluster_tree.write(ar, |n| n.write())?;
        }
        Ok(())
    }
}

/// Whether a class descends through `UHierarchicalInstancedStaticMeshComponent`,
/// and so writes a cluster tree after the ISMC layer.
fn is_hierarchical(class: &str) -> bool {
    matches!(
        class,
        "HierarchicalInstancedStaticMeshComponent" | "FoliageInstancedStaticMeshComponent"
    )
}

/// What a tail model needs beyond its own bytes.
///
/// Bulk payloads are referenced by an index into the *package's* bulk-data map,
/// and whether one is inline is decided by comparing that entry's offset against
/// the reader's position — which is a position in the whole export, not in the
/// tail. So a model that touches bulk data needs both the map and where its
/// slice starts.
#[derive(Clone, Copy)]
pub struct TailContext<'a> {
    /// `(serial offset, serial size)` per bulk-data index.
    pub bulk_data: &'a [(i64, i64)],
    /// The tail's offset within the export payload.
    pub origin: usize,
    /// Needed by any tail that embeds a reflected struct — the material caches
    /// are property blocks, so they cannot be read or written without a schema.
    pub usmap: &'a Usmap,
    /// How to find a layout the `.usmap` does not carry: a `UDataTable`'s row
    /// struct, or a user-defined struct nested inside another package's block.
    /// Both directions need it — regenerating a nested block's header needs the
    /// nested schema just as much as reading it did.
    pub resolver: Option<&'a dyn super::archive::PackageResolver>,
}

/// A texture's CPU-side copy (`FSharedImage`, an `FImage`; ImageCore.h:412).
///
/// `RawData` is a `TArray64<uint8>`, so its count is 64-bit — the one place in
/// the texture tail that is not a 32-bit count.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureCpuCopy {
    pub size_x: i32,
    pub size_y: i32,
    pub num_slices: i32,
    pub format: u8,
    pub gamma_space: u8,
    pub raw_data: Vec<u8>,
}

/// `FOptTexturePlatformData` (Texture.h:801, 5.5.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OptTexturePlatformData {
    pub ext_data: u32,
    pub num_mips_in_tail: u32,
}

/// One mip of a cooked texture (`FTexture2DMipMap::Serialize`,
/// Texture2D.cpp:150): the bulk-data handle, then the three dimensions.
///
/// `payload` holds the mip's actual pixel bytes when the cook inlined them. For
/// a streaming mip the payload lives in the container's separate bulk chunk and
/// only the index is written here, so there is nothing in the tail to hold.
///
/// The bytes are block-compressed and stay that way. They *are* the stored
/// representation — decoding them to RGBA is a lossy interpretation that belongs
/// in an API on top, not in the codec, and re-encoding could not reproduce them.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureMip {
    pub bulk_index: i32,
    pub payload: Option<Vec<u8>>,
    pub size_x: i32,
    pub size_y: i32,
    pub size_z: i32,
}

/// One entry of `UTexture::SerializeCookedPlatformData`'s format list: a pixel
/// format name, a skip offset, and the `FTexturePlatformData` behind it.
///
/// The skip offset is *not* stored. It is a delta from its own position to the
/// end of the platform data, so it is a function of what follows and is
/// recomputed on write — retaining it would let the model disagree with its own
/// contents.
#[derive(Debug, Clone, PartialEq)]
pub struct TexturePlatformData {
    pub format_name: FName,
    /// The cook can emit a derived-data *reference* instead of the data. Not
    /// observed in this corpus; the model reports it rather than guessing.
    pub using_derived_data: bool,
    pub size_x: i32,
    pub size_y: i32,
    /// Packed slice count and the cube-map / opt-data / cpu-copy bits
    /// (Texture.h:973).
    pub packed_data: u32,
    pub pixel_format: FStr,
    pub opt_data: Option<OptTexturePlatformData>,
    pub cpu_copy: Option<TextureCpuCopy>,
    pub first_mip_to_serialize: i32,
    pub mips: Vec<TextureMip>,
    /// A virtual texture writes no mips at all; its data is the built-data block
    /// that follows the flag.
    pub virtual_data: Option<VirtualTextureBuiltData>,
}

impl TexturePlatformData {
    const BIT_HAS_OPT_DATA: u32 = 1 << 30;
    const BIT_HAS_CPU_COPY: u32 = 1 << 29;

    /// The 15 zero bytes the engine writes and then `check()`s are all zero
    /// (TextureDerivedData.cpp). Regenerated rather than kept — a constant is
    /// not data.
    const PLACEHOLDER: usize = 15;
}

/// The cooked platform-data list shared by every texture class.
///
/// `UTexture2D` alone writes a `bSerializeMipData` flag between the cooked flag
/// and the list; the other classes call the shared serializer directly.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureCookedData {
    pub strip_flags: StripDataFlags,
    pub cooked: bool,
    pub serialize_mip_data: Option<bool>,
    pub formats: Vec<TexturePlatformData>,
    /// The `NAME_None` that ends the format list, kept as the name it actually
    /// was rather than rebuilt.
    ///
    /// Its *text* is "None" but its identity is not `FName::none()` — this
    /// package's name map puts "None" at index 3444, and writing index 0
    /// produced the right length with the wrong four bytes on 8,937 textures.
    /// An `FName` is an index and a number, not a string.
    pub terminator: FName,
}

impl TextureCookedData {
    pub fn read(r: &mut Reader, ctx: TailContext, has_mip_data_flag: bool) -> Result<Self> {
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let cooked = r.u32()? != 0;
        if !cooked {
            return Ok(TextureCookedData {
                strip_flags,
                cooked,
                serialize_mip_data: None,
                formats: Vec::new(),
                terminator: FName::none(),
            });
        }
        let serialize_mip_data = has_mip_data_flag.then(|| r.u32()).transpose()?.map(|v| v != 0);
        let mip_data = serialize_mip_data.unwrap_or(true);
        let mut formats = Vec::new();
        loop {
            let format_name = r.fname()?;
            if format_name.as_str() == "None" {
                return Ok(TextureCookedData {
                    strip_flags,
                    cooked,
                    serialize_mip_data,
                    formats,
                    terminator: format_name,
                });
            }
            let loc = r.o;
            let skip = r.u64()? as i64;
            let end = loc
                .checked_add_signed(skip as isize)
                .filter(|e| *e > r.o && *e <= r.b.len())
                .with_context(|| format!("implausible texture SkipOffset {skip} @ {loc}"))?;
            let mut pd = TexturePlatformData::read(r, ctx, mip_data, end)?;
            pd.format_name = format_name;
            formats.push(pd);
            if r.o != end {
                bail!("platform data ended at {} but its SkipOffset says {end}", r.o);
            }
        }
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.strip_flags.clone().serialize(ar)?;
        ar.u32(&mut u32::from(self.cooked))?;
        if !self.cooked {
            return Ok(());
        }
        match self.serialize_mip_data {
            Some(v) => ar.u32(&mut u32::from(v))?,
            None => {}
        }
        let mip_data = self.serialize_mip_data.unwrap_or(true);
        for f in &self.formats {
            ar.fname(&mut f.format_name.clone())?;
            // The skip offset is a delta to the end of this platform data, so
            // encode the body first and measure it.
            // A scratch buffer only, to measure the body before writing the
            // skip offset — it embeds no nested schema.
            let mut body = super::archive::Writer::new();
            f.write_body(&mut body, mip_data)?;
            let body = body.into_bytes();
            ar.u64(&mut ((body.len() + 8) as u64))?;
            let n = body.len();
            ar.raw(&mut body.clone(), n)?;
        }
        ar.fname(&mut self.terminator.clone())?;
        Ok(())
    }
}

impl TexturePlatformData {
    /// The caller owns the format name — it is read before the skip offset that
    /// bounds this block — and assigns it after.
    fn read(r: &mut Reader, ctx: TailContext, mip_data: bool, end: usize) -> Result<Self> {
        let format_name = FName::none();
        let using_derived_data = r.u8()? != 0;
        if using_derived_data {
            bail!("texture cooked to a derived-data reference, which is not modeled");
        }
        let placeholder = r.take(Self::PLACEHOLDER)?;
        if placeholder.iter().any(|&b| b != 0) {
            bail!("texture placeholder derived data is not zero");
        }
        let size_x = r.i32()?;
        let size_y = r.i32()?;
        let packed_data = r.u32()?;
        let pixel_format = r.fstring()?;
        let opt_data = if packed_data & Self::BIT_HAS_OPT_DATA != 0 {
            Some(OptTexturePlatformData { ext_data: r.u32()?, num_mips_in_tail: r.u32()? })
        } else {
            None
        };
        let cpu_copy = if packed_data & Self::BIT_HAS_CPU_COPY != 0 {
            let (size_x, size_y, num_slices) = (r.i32()?, r.i32()?, r.i32()?);
            let format = r.u8()?;
            let gamma_space = r.u8()?;
            let n = r.u64()? as usize;
            Some(TextureCpuCopy {
                size_x,
                size_y,
                num_slices,
                format,
                gamma_space,
                raw_data: r.take(n)?.to_vec(),
            })
        } else {
            None
        };
        let first_mip_to_serialize = r.i32()?;
        let num_mips = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "texture mips", r.o - 4)?
        };
        let mut mips = Vec::with_capacity(num_mips.min(32));
        for _ in 0..num_mips {
            mips.push(TextureMip::read(r, ctx, mip_data)?);
        }
        let virtual_data =
            (r.u32()? != 0).then(|| VirtualTextureBuiltData::read(r, ctx)).transpose()?;
        let _ = end;
        Ok(TexturePlatformData {
            format_name,
            using_derived_data,
            size_x,
            size_y,
            packed_data,
            pixel_format,
            opt_data,
            cpu_copy,
            first_mip_to_serialize,
            mips,
            virtual_data,
        })
    }

    fn write_body(&self, ar: &mut impl Ar, mip_data: bool) -> Result<()> {
        ar.u8(&mut u8::from(self.using_derived_data))?;
        ar.raw(&mut vec![0u8; Self::PLACEHOLDER], Self::PLACEHOLDER)?;
        ar.i32(&mut self.size_x.to_owned())?;
        ar.i32(&mut self.size_y.to_owned())?;
        ar.u32(&mut self.packed_data.to_owned())?;
        ar.fstring(&mut self.pixel_format.clone())?;
        match (&self.opt_data, self.packed_data & Self::BIT_HAS_OPT_DATA != 0) {
            (Some(o), true) => {
                ar.u32(&mut o.ext_data.to_owned())?;
                ar.u32(&mut o.num_mips_in_tail.to_owned())?;
            }
            (None, false) => {}
            _ => bail!("opt data presence disagrees with the packed-data bit"),
        }
        match (&self.cpu_copy, self.packed_data & Self::BIT_HAS_CPU_COPY != 0) {
            (Some(c), true) => {
                ar.i32(&mut c.size_x.to_owned())?;
                ar.i32(&mut c.size_y.to_owned())?;
                ar.i32(&mut c.num_slices.to_owned())?;
                ar.u8(&mut c.format.to_owned())?;
                ar.u8(&mut c.gamma_space.to_owned())?;
                ar.u64(&mut (c.raw_data.len() as u64))?;
                let n = c.raw_data.len();
                ar.raw(&mut c.raw_data.clone(), n)?;
            }
            (None, false) => {}
            _ => bail!("CPU copy presence disagrees with the packed-data bit"),
        }
        ar.i32(&mut self.first_mip_to_serialize.to_owned())?;
        ar.i32(&mut (self.mips.len() as i32))?;
        for m in &self.mips {
            m.write(ar, mip_data)?;
        }
        match &self.virtual_data {
            Some(v) => {
                ar.u32(&mut 1)?;
                v.write(ar)
            }
            None => ar.u32(&mut 0),
        }
    }
}

impl TextureMip {
    fn read(r: &mut Reader, ctx: TailContext, mip_data: bool) -> Result<Self> {
        let mut bulk_index = 0;
        let mut payload = None;
        if mip_data {
            bulk_index = r.i32()?;
            let Some(&(offset, size)) = ctx.bulk_data.get(bulk_index.max(0) as usize) else {
                bail!("texture mip: bulk data index {bulk_index} out of range");
            };
            // Inline exactly when the map says this payload lives here.
            if offset as usize == ctx.origin + r.o {
                payload = Some(r.take(size.max(0) as usize)?.to_vec());
            }
        }
        Ok(TextureMip {
            bulk_index,
            payload,
            size_x: r.i32()?,
            size_y: r.i32()?,
            size_z: r.i32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar, mip_data: bool) -> Result<()> {
        if mip_data {
            ar.i32(&mut self.bulk_index.to_owned())?;
            if let Some(p) = &self.payload {
                let n = p.len();
                ar.raw(&mut p.clone(), n)?;
            }
        }
        ar.i32(&mut self.size_x.to_owned())?;
        ar.i32(&mut self.size_y.to_owned())?;
        ar.i32(&mut self.size_z.to_owned())
    }
}

/// A `TArray<uint32>` written by the *default* `TArray` operator — a count and
/// then the elements, with no element size. Distinct from [`BulkArray`], which
/// is what `BulkSerialize` writes.
fn read_u32_array(r: &mut Reader, what: &str) -> Result<Vec<u32>> {
    let n = {
        let n = r.i32()?;
        super::limits::bounded(n, MAX_NATIVE_COUNT, what, r.o - 4)?
    };
    (0..n).map(|_| r.u32()).collect()
}

fn write_u32_array(ar: &mut impl Ar, v: &[u32]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for x in v {
        ar.u32(&mut x.to_owned())?;
    }
    Ok(())
}

/// `FVirtualTextureTileOffsetData` (VirtualTextureBuiltData.h:89, 5.5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualTextureTileOffsetData {
    pub width: u32,
    pub height: u32,
    /// Upper-bound Morton address for the managed area.
    pub max_address: u32,
    /// Sorted list of contiguous tile block addresses.
    pub addresses: Vec<u32>,
    /// Offset for each block in `addresses`; an empty block is `!0`.
    pub offsets: Vec<u32>,
}

/// One streamed chunk of a virtual texture's built data.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualTextureDataChunk {
    pub bulk_data_hash: ShaHash,
    pub size_in_bytes: u32,
    pub codec_payload_size: u32,
    /// Per layer, in the order the engine writes them: the codec type then its
    /// payload offset.
    pub codecs: Vec<(u8, u32)>,
    pub bulk_index: i32,
    pub payload: Option<Vec<u8>>,
}

/// `FVirtualTextureBuiltData::Serialize` (VirtualTextureBuiltData.cpp:222).
///
/// The mip-stripping branch is a *save-side* path taken only when
/// `FirstMipToSerialize > 0`, which the engine asserts cannot happen on load, so
/// a cooked stream always carries the unstripped field order.
#[derive(Debug, Clone, PartialEq)]
pub struct VirtualTextureBuiltData {
    pub cooked: bool,
    pub num_layers: u32,
    pub width_in_blocks: u32,
    pub height_in_blocks: u32,
    pub tile_size: u32,
    pub tile_border_size: u32,
    pub tile_data_offset_per_layer: Vec<u32>,
    pub num_mips: u32,
    pub width: u32,
    pub height: u32,
    pub chunk_index_per_mip: Vec<u32>,
    pub base_offset_per_mip: Vec<u32>,
    pub tile_offset_data: Vec<VirtualTextureTileOffsetData>,
    pub tile_index_per_chunk: Vec<u32>,
    pub tile_index_per_mip: Vec<u32>,
    pub tile_offset_in_chunk: Vec<u32>,
    /// One pixel-format name per layer.
    pub layer_types: Vec<FStr>,
    /// One `FLinearColor` per layer, four floats.
    pub layer_fallback_colors: Vec<[f32; 4]>,
    pub chunks: Vec<VirtualTextureDataChunk>,
}

impl VirtualTextureBuiltData {
    fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let cooked = r.u32()? != 0;
        let num_layers = r.u32()?;
        let width_in_blocks = r.u32()?;
        let height_in_blocks = r.u32()?;
        let tile_size = r.u32()?;
        let tile_border_size = r.u32()?;
        let tile_data_offset_per_layer = read_u32_array(r, "TileDataOffsetPerLayer")?;
        let num_mips = r.u32()?;
        let width = r.u32()?;
        let height = r.u32()?;
        let chunk_index_per_mip = read_u32_array(r, "ChunkIndexPerMip")?;
        let base_offset_per_mip = read_u32_array(r, "BaseOffsetPerMip")?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "TileOffsetData", r.o - 4)?
        };
        let mut tile_offset_data = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            tile_offset_data.push(VirtualTextureTileOffsetData {
                width: r.u32()?,
                height: r.u32()?,
                max_address: r.u32()?,
                addresses: read_u32_array(r, "tile addresses")?,
                offsets: read_u32_array(r, "tile offsets")?,
            });
        }
        let tile_index_per_chunk = read_u32_array(r, "TileIndexPerChunk")?;
        let tile_index_per_mip = read_u32_array(r, "TileIndexPerMip")?;
        let tile_offset_in_chunk = read_u32_array(r, "TileOffsetInChunk")?;

        // Layer arrays are fixed-size in memory but only `num_layers` entries
        // are written, so the count comes from the header rather than the wire.
        let layers = super::limits::bounded(
            num_layers.min(i32::MAX as u32) as i32,
            64,
            "virtual texture layers",
            r.o,
        )?;
        let layer_types = (0..layers).map(|_| r.fstring()).collect::<Result<Vec<_>>>()?;
        let mut layer_fallback_colors = Vec::with_capacity(layers);
        for _ in 0..layers {
            layer_fallback_colors.push([r.f32()?, r.f32()?, r.f32()?, r.f32()?]);
        }

        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "virtual texture chunks", r.o - 4)?
        };
        let mut chunks = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let bulk_data_hash = { let mut h = ShaHash::default(); h.serialize(r)?; h };
            let size_in_bytes = r.u32()?;
            let codec_payload_size = r.u32()?;
            let mut codecs = Vec::with_capacity(layers);
            for _ in 0..layers {
                codecs.push((r.u8()?, r.u32()?));
            }
            let bulk_index = r.i32()?;
            let Some(&(offset, size)) = ctx.bulk_data.get(bulk_index.max(0) as usize) else {
                bail!("virtual texture chunk: bulk data index {bulk_index} out of range");
            };
            let payload = (offset as usize == ctx.origin + r.o)
                .then(|| r.take(size.max(0) as usize).map(<[u8]>::to_vec))
                .transpose()?;
            chunks.push(VirtualTextureDataChunk {
                bulk_data_hash,
                size_in_bytes,
                codec_payload_size,
                codecs,
                bulk_index,
                payload,
            });
        }
        Ok(VirtualTextureBuiltData {
            cooked,
            num_layers,
            width_in_blocks,
            height_in_blocks,
            tile_size,
            tile_border_size,
            tile_data_offset_per_layer,
            num_mips,
            width,
            height,
            chunk_index_per_mip,
            base_offset_per_mip,
            tile_offset_data,
            tile_index_per_chunk,
            tile_index_per_mip,
            tile_offset_in_chunk,
            layer_types,
            layer_fallback_colors,
            chunks,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut u32::from(self.cooked))?;
        ar.u32(&mut self.num_layers.to_owned())?;
        ar.u32(&mut self.width_in_blocks.to_owned())?;
        ar.u32(&mut self.height_in_blocks.to_owned())?;
        ar.u32(&mut self.tile_size.to_owned())?;
        ar.u32(&mut self.tile_border_size.to_owned())?;
        write_u32_array(ar, &self.tile_data_offset_per_layer)?;
        ar.u32(&mut self.num_mips.to_owned())?;
        ar.u32(&mut self.width.to_owned())?;
        ar.u32(&mut self.height.to_owned())?;
        write_u32_array(ar, &self.chunk_index_per_mip)?;
        write_u32_array(ar, &self.base_offset_per_mip)?;
        ar.i32(&mut (self.tile_offset_data.len() as i32))?;
        for t in &self.tile_offset_data {
            ar.u32(&mut t.width.to_owned())?;
            ar.u32(&mut t.height.to_owned())?;
            ar.u32(&mut t.max_address.to_owned())?;
            write_u32_array(ar, &t.addresses)?;
            write_u32_array(ar, &t.offsets)?;
        }
        write_u32_array(ar, &self.tile_index_per_chunk)?;
        write_u32_array(ar, &self.tile_index_per_mip)?;
        write_u32_array(ar, &self.tile_offset_in_chunk)?;
        for s in &self.layer_types {
            ar.fstring(&mut s.clone())?;
        }
        for c in &self.layer_fallback_colors {
            for v in c {
                ar.f32(&mut v.to_owned())?;
            }
        }
        ar.i32(&mut (self.chunks.len() as i32))?;
        for c in &self.chunks {
            c.bulk_data_hash.clone().serialize(ar)?;
            ar.u32(&mut c.size_in_bytes.to_owned())?;
            ar.u32(&mut c.codec_payload_size.to_owned())?;
            for (ty, off) in &c.codecs {
                ar.u8(&mut ty.to_owned())?;
                ar.u32(&mut off.to_owned())?;
            }
            ar.i32(&mut c.bulk_index.to_owned())?;
            if let Some(p) = &c.payload {
                let n = p.len();
                ar.raw(&mut p.clone(), n)?;
            }
        }
        Ok(())
    }
}

/// An `FWeightedRandomSampler`: two float arrays and a total weight.
#[derive(Debug, Clone, PartialEq)]
pub struct WeightedRandomSampler {
    pub prob: Vec<f32>,
    pub alias: Vec<i32>,
    pub total_weight: f32,
}

impl WeightedRandomSampler {
    fn read(r: &mut Reader) -> Result<Self> {
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "sampler prob", r.o - 4)?
        };
        let prob = (0..n).map(|_| r.f32()).collect::<Result<Vec<_>>>()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "sampler alias", r.o - 4)?
        };
        let alias = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
        Ok(WeightedRandomSampler { prob, alias, total_weight: r.f32()? })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut (self.prob.len() as i32))?;
        for v in &self.prob {
            ar.f32(&mut v.to_owned())?;
        }
        ar.i32(&mut (self.alias.len() as i32))?;
        for v in &self.alias {
            ar.i32(&mut v.to_owned())?;
        }
        ar.f32(&mut self.total_weight.to_owned())
    }
}

/// An `FRawStaticIndexBuffer`: a 32-bit flag, the indices as a bulk array, and
/// the "should expand to 32 bit" flag.
#[derive(Debug, Clone, PartialEq)]
pub struct RawStaticIndexBuffer {
    pub is_32_bit: u32,
    pub indices: BulkArray,
    pub should_expand_to_32_bit: u32,
}

impl RawStaticIndexBuffer {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(RawStaticIndexBuffer {
            is_32_bit: r.u32()?,
            indices: BulkArray::read(r, "index buffer")?,
            should_expand_to_32_bit: r.u32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.is_32_bit.to_owned())?;
        self.indices.write(ar)?;
        ar.u32(&mut self.should_expand_to_32_bit.to_owned())
    }
}

/// `FStaticMeshLODResources::SerializeBuffers` — the vertex and index buffers.
///
/// Every payload is a bulk array carrying its own element size, so none of the
/// vertex *formats* need modeling here: the stride and the flags that decide it
/// are all present as values, and the packed vertices behind them are leaf data.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshBuffers {
    pub global_strip: u8,
    pub class_strip: u8,
    pub position_stride: i32,
    pub position_num_vertices: i32,
    pub positions: BulkArray,
    pub vertex_strip: StripDataFlags,
    pub num_tex_coords: i32,
    pub num_vertices: i32,
    pub use_full_precision_uvs: u32,
    pub use_high_precision_tangent_basis: u32,
    /// Tangents and UVs, present unless the vertex buffer's own strip flags say
    /// otherwise.
    pub tangents_and_uvs: Option<(BulkArray, BulkArray)>,
    pub color_strip: StripDataFlags,
    pub color_stride: i32,
    pub color_num_vertices: i32,
    pub colors: Option<BulkArray>,
    pub index_buffer: RawStaticIndexBuffer,
    pub reversed_index_buffer: Option<RawStaticIndexBuffer>,
    pub depth_only_index_buffer: RawStaticIndexBuffer,
    pub reversed_depth_only_index_buffer: Option<RawStaticIndexBuffer>,
    /// Editor-only, so absent from a cooked package unless the global strip
    /// flags kept it.
    pub wireframe_index_buffer: Option<RawStaticIndexBuffer>,
    pub ray_tracing_geometry: Option<BulkArray>,
}

/// The samplers that follow the buffers: one per section, then one for the LOD.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshSamplers {
    pub per_section: Vec<WeightedRandomSampler>,
    pub area_weighted: WeightedRandomSampler,
}

impl StaticMeshBuffers {
    fn read(r: &mut Reader) -> Result<Self> {
        let global_strip = r.u8()?;
        let class_strip = r.u8()?;
        let position_stride = r.i32()?;
        let position_num_vertices = r.i32()?;
        let positions = BulkArray::read(r, "positions")?;
        let vertex_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let num_tex_coords = r.i32()?;
        let num_vertices = r.i32()?;
        let use_full_precision_uvs = r.u32()?;
        let use_high_precision_tangent_basis = r.u32()?;
        let tangents_and_uvs = if vertex_strip.global & 2 == 0 {
            Some((BulkArray::read(r, "tangents")?, BulkArray::read(r, "UVs")?))
        } else {
            None
        };
        let color_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let color_stride = r.i32()?;
        let color_num_vertices = r.i32()?;
        let colors = (color_strip.global & 2 == 0 && color_num_vertices > 0)
            .then(|| BulkArray::read(r, "vertex colours"))
            .transpose()?;
        let index_buffer = RawStaticIndexBuffer::read(r)?;
        // `CDSF_ReversedIndexBuffer` is bit 2 of the class strip flags.
        let reversed_index_buffer =
            (class_strip & 4 == 0).then(|| RawStaticIndexBuffer::read(r)).transpose()?;
        let depth_only_index_buffer = RawStaticIndexBuffer::read(r)?;
        let reversed_depth_only_index_buffer =
            (class_strip & 4 == 0).then(|| RawStaticIndexBuffer::read(r)).transpose()?;
        let wireframe_index_buffer =
            (global_strip & 1 == 0).then(|| RawStaticIndexBuffer::read(r)).transpose()?;
        let ray_tracing_geometry = (class_strip & 8 == 0)
            .then(|| BulkArray::read(r, "ray tracing geometry"))
            .transpose()?;
        Ok(StaticMeshBuffers {
            global_strip,
            class_strip,
            position_stride,
            position_num_vertices,
            positions,
            vertex_strip,
            num_tex_coords,
            num_vertices,
            use_full_precision_uvs,
            use_high_precision_tangent_basis,
            tangents_and_uvs,
            color_strip,
            color_stride,
            color_num_vertices,
            colors,
            index_buffer,
            reversed_index_buffer,
            depth_only_index_buffer,
            reversed_depth_only_index_buffer,
            wireframe_index_buffer,
            ray_tracing_geometry,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.global_strip.to_owned())?;
        ar.u8(&mut self.class_strip.to_owned())?;
        ar.i32(&mut self.position_stride.to_owned())?;
        ar.i32(&mut self.position_num_vertices.to_owned())?;
        self.positions.write(ar)?;
        self.vertex_strip.clone().serialize(ar)?;
        ar.i32(&mut self.num_tex_coords.to_owned())?;
        ar.i32(&mut self.num_vertices.to_owned())?;
        ar.u32(&mut self.use_full_precision_uvs.to_owned())?;
        ar.u32(&mut self.use_high_precision_tangent_basis.to_owned())?;
        match (&self.tangents_and_uvs, self.vertex_strip.global & 2 == 0) {
            (Some((t, u)), true) => {
                t.write(ar)?;
                u.write(ar)?;
            }
            (None, false) => {}
            _ => bail!("tangent/UV presence disagrees with the vertex strip flags"),
        }
        self.color_strip.clone().serialize(ar)?;
        ar.i32(&mut self.color_stride.to_owned())?;
        ar.i32(&mut self.color_num_vertices.to_owned())?;
        match (&self.colors, self.color_strip.global & 2 == 0 && self.color_num_vertices > 0) {
            (Some(c), true) => c.write(ar)?,
            (None, false) => {}
            _ => bail!("vertex colour presence disagrees with the colour buffer's flags"),
        }
        self.index_buffer.write(ar)?;
        write_optional_index_buffer(ar, &self.reversed_index_buffer, self.class_strip & 4 == 0)?;
        self.depth_only_index_buffer.write(ar)?;
        write_optional_index_buffer(
            ar,
            &self.reversed_depth_only_index_buffer,
            self.class_strip & 4 == 0,
        )?;
        write_optional_index_buffer(ar, &self.wireframe_index_buffer, self.global_strip & 1 == 0)?;
        match (&self.ray_tracing_geometry, self.class_strip & 8 == 0) {
            (Some(b), true) => b.write(ar)?,
            (None, false) => {}
            _ => bail!("ray tracing geometry presence disagrees with the strip flags"),
        }
        Ok(())
    }
}

fn write_optional_index_buffer(
    ar: &mut impl Ar,
    buf: &Option<RawStaticIndexBuffer>,
    expected: bool,
) -> Result<()> {
    match (buf, expected) {
        (Some(b), true) => b.write(ar),
        (None, false) => Ok(()),
        _ => bail!("index buffer presence disagrees with the strip flags"),
    }
}

/// One `FStaticMeshLODResources`.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshLod {
    pub global_strip: u8,
    pub class_strip: u8,
    pub sections: Vec<StaticMeshSection>,
    pub max_deviation: f32,
    pub is_lod_cooked_out: bool,
    pub is_inlined: bool,
    /// Everything after `bInlined`, absent for a LOD that was cooked out or had
    /// its render data stripped — such a LOD ends right there.
    pub render: Option<StaticMeshLodRender>,
}

/// The part of a LOD that only exists when it kept its render data.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshLodRender {
    pub has_ray_tracing_geometry: u32,
    /// Inline buffers, or the bulk-data handle and metadata of stripped ones.
    pub buffers: StaticMeshLodBuffers,
    pub buffers_size: StaticMeshBuffersSize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StaticMeshLodBuffers {
    Inline { buffers: Box<StaticMeshBuffers>, samplers: StaticMeshSamplers },
    /// Streamed: a bulk-data index, the depth-only triangle count and packed
    /// flags, then the metadata describing each buffer that was left out.
    Streamed { bulk_index: i32, depth_only_and_flags: [u8; 8], buffer_metadata: [u8; 72] },
}

impl StaticMeshLod {
    fn read(r: &mut Reader) -> Result<Self> {
        let global_strip = r.u8()?;
        let class_strip = r.u8()?;
        let n = bounded_count(r.i32()?, "mesh sections", r.o - 4)?;
        let sections: Vec<StaticMeshSection> = read_vec(r, "mesh sections", n)?;
        let max_deviation = r.f32()?;
        let is_lod_cooked_out = r.u32()? != 0;
        let is_inlined = r.u32()? != 0;
        let render = if global_strip & 2 == 0 && !is_lod_cooked_out {
            let has_ray_tracing_geometry = r.u32()?;
            let buffers = if is_inlined {
                let buffers = Box::new(StaticMeshBuffers::read(r)?);
                let mut per_section = Vec::with_capacity(sections.len().min(64));
                for _ in 0..sections.len() {
                    per_section.push(WeightedRandomSampler::read(r)?);
                }
                StaticMeshLodBuffers::Inline {
                    buffers,
                    samplers: StaticMeshSamplers {
                        per_section,
                        area_weighted: WeightedRandomSampler::read(r)?,
                    },
                }
            } else {
                StaticMeshLodBuffers::Streamed {
                    bulk_index: r.i32()?,
                    depth_only_and_flags: r.take(8)?.try_into().expect("8 bytes"),
                    buffer_metadata: r.take(72)?.try_into().expect("72 bytes"),
                }
            };
            Some(StaticMeshLodRender {
                has_ray_tracing_geometry,
                buffers,
                buffers_size: {
                    let mut b = StaticMeshBuffersSize::default();
                    b.serialize(r)?;
                    b
                },
            })
        } else {
            None
        };
        Ok(StaticMeshLod {
            global_strip,
            class_strip,
            sections,
            max_deviation,
            is_lod_cooked_out,
            is_inlined,
            render,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.global_strip.to_owned())?;
        ar.u8(&mut self.class_strip.to_owned())?;
        write_vec(ar, &self.sections)?;
        ar.f32(&mut self.max_deviation.to_owned())?;
        ar.u32(&mut u32::from(self.is_lod_cooked_out))?;
        ar.u32(&mut u32::from(self.is_inlined))?;
        let expected = self.global_strip & 2 == 0 && !self.is_lod_cooked_out;
        match (&self.render, expected) {
            (Some(rd), true) => {
                ar.u32(&mut rd.has_ray_tracing_geometry.to_owned())?;
                match (&rd.buffers, self.is_inlined) {
                    (StaticMeshLodBuffers::Inline { buffers, samplers }, true) => {
                        buffers.write(ar)?;
                        if samplers.per_section.len() != self.sections.len() {
                            bail!(
                                "{} per-section samplers for {} sections",
                                samplers.per_section.len(),
                                self.sections.len()
                            );
                        }
                        for s in &samplers.per_section {
                            s.write(ar)?;
                        }
                        samplers.area_weighted.write(ar)?;
                    }
                    (
                        StaticMeshLodBuffers::Streamed {
                            bulk_index,
                            depth_only_and_flags,
                            buffer_metadata,
                        },
                        false,
                    ) => {
                        ar.i32(&mut bulk_index.to_owned())?;
                        ar.raw(&mut depth_only_and_flags.to_vec(), 8)?;
                        ar.raw(&mut buffer_metadata.to_vec(), 72)?;
                    }
                    _ => bail!("buffer form disagrees with the inlined flag"),
                }
                rd.buffers_size.clone().serialize(ar)?;
            }
            (None, false) => {}
            _ => bail!("LOD render data presence disagrees with its flags"),
        }
        Ok(())
    }
}

/// `FNaniteResources` as cooked into a `UStaticMesh`.
///
/// `root_data` and the streaming pages are Nanite's own compressed cluster
/// encoding — leaf data here, and their decoder is a separate module. Everything
/// that *addresses* them is modeled: the page streaming states, the BVH
/// hierarchy, the dependencies and the mesh statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct NaniteResources {
    pub strip_flags: StripDataFlags,
    pub resource_flags: u32,
    /// `StreamablePages`: a bulk-data handle, never inline in this corpus.
    pub streamable_pages_index: i32,
    pub root_data: Vec<u8>,
    pub page_streaming_states: Vec<PageStreamingState>,
    pub hierarchy_nodes: Vec<PackedHierarchyNode>,
    pub hierarchy_root_offsets: Vec<u32>,
    pub page_dependencies: Vec<u32>,
    /// Two bytes per entry.
    pub imposter_atlas: Vec<u8>,
    /// The trailing statistics, in the order `FResources::Serialize` writes them
    /// (NaniteResources.cpp:286).
    pub num_root_pages: u32,
    pub position_precision: i32,
    pub normal_precision: i32,
    pub num_input_triangles: u32,
    pub num_input_vertices: u32,
    pub num_input_meshes: u16,
    pub num_input_tex_coords: u16,
    pub num_clusters: u32,
}

impl NaniteResources {
    fn read(r: &mut Reader) -> Result<Self> {
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let resource_flags = r.u32()?;
        let streamable_pages_index = r.i32()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "Nanite RootData", r.o - 4)?
        };
        let root_data = r.take(n)?.to_vec();
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "PageStreamingStates", r.o - 4)?
        };
        let page_streaming_states: Vec<PageStreamingState> =
            read_vec(r, "PageStreamingStates", n)?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "HierarchyNodes", r.o - 4)?
        };
        let hierarchy_nodes: Vec<PackedHierarchyNode> = read_vec(r, "HierarchyNodes", n)?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "HierarchyRootOffsets", r.o - 4)?
        };
        let hierarchy_root_offsets = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "PageDependencies", r.o - 4)?
        };
        let page_dependencies = (0..n).map(|_| r.u32()).collect::<Result<Vec<_>>>()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "ImposterAtlas", r.o - 4)?
        };
        let imposter_atlas = r.take(n * 2)?.to_vec();
        Ok(NaniteResources {
            strip_flags,
            resource_flags,
            streamable_pages_index,
            root_data,
            page_streaming_states,
            hierarchy_nodes,
            hierarchy_root_offsets,
            page_dependencies,
            imposter_atlas,
            num_root_pages: r.u32()?,
            position_precision: r.i32()?,
            normal_precision: r.i32()?,
            num_input_triangles: r.u32()?,
            num_input_vertices: r.u32()?,
            num_input_meshes: r.u16()?,
            num_input_tex_coords: r.u16()?,
            num_clusters: r.u32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.strip_flags.clone().serialize(ar)?;
        ar.u32(&mut self.resource_flags.to_owned())?;
        ar.i32(&mut self.streamable_pages_index.to_owned())?;
        ar.i32(&mut (self.root_data.len() as i32))?;
        let n = self.root_data.len();
        ar.raw(&mut self.root_data.clone(), n)?;
        write_vec(ar, &self.page_streaming_states)?;
        write_vec(ar, &self.hierarchy_nodes)?;
        write_u32_array(ar, &self.hierarchy_root_offsets)?;
        write_u32_array(ar, &self.page_dependencies)?;
        if self.imposter_atlas.len() % 2 != 0 {
            bail!("imposter atlas has an odd byte count");
        }
        ar.i32(&mut ((self.imposter_atlas.len() / 2) as i32))?;
        let n = self.imposter_atlas.len();
        ar.raw(&mut self.imposter_atlas.clone(), n)?;
        ar.u32(&mut self.num_root_pages.to_owned())?;
        ar.i32(&mut self.position_precision.to_owned())?;
        ar.i32(&mut self.normal_precision.to_owned())?;
        ar.u32(&mut self.num_input_triangles.to_owned())?;
        ar.u32(&mut self.num_input_vertices.to_owned())?;
        ar.u16(&mut self.num_input_meshes.to_owned())?;
        ar.u16(&mut self.num_input_tex_coords.to_owned())?;
        ar.u32(&mut self.num_clusters.to_owned())
    }
}

/// One LOD's ray-tracing proxy entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RayTracingProxyLod {
    /// `bOwnsBuffers`, and the 40-byte sections it owns when set.
    pub sections: Option<Vec<[u8; 40]>>,
    pub owns_ray_tracing_geometry: u32,
    pub bulk_index: i32,
    /// The streamable payload, present only when the bulk map puts it here.
    pub payload: Option<Vec<u8>>,
}

/// `FStaticMeshRayTracingProxy`, written only when `bHasRayTracingProxy`.
#[derive(Debug, Clone, PartialEq)]
pub struct RayTracingProxy {
    pub strip_flags: StripDataFlags,
    pub using_rendering_lods: u32,
    pub lods: Vec<RayTracingProxyLod>,
}

/// One LOD's Lumen card representation.
#[derive(Debug, Clone, PartialEq)]
pub struct CardRepresentation {
    pub bounds: Box3d,
    pub mostly_two_sided: u32,
    pub cards: Vec<LumenCardBuildData>,
}

/// One LOD's distance-field volume (`FDistanceFieldVolumeData5`).
#[derive(Debug, Clone, PartialEq)]
pub struct DistanceFieldVolume {
    /// An `FBox3f` — the float variant, 25 bytes, not the 49-byte `FBox`.
    pub local_space_mesh_bounds: Box3f,
    pub mostly_two_sided: u32,
    pub mips: [SparseDistanceFieldMip; 3],
    pub always_loaded_mip: Vec<u8>,
    /// `StreamableMips`: a bulk-data handle.
    pub streamable_mips_index: i32,
}

/// The whole tail of a `UStaticMesh` export: 15,231 exports and 1,310 MiB, the
/// largest tail population in the corpus.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshTail {
    pub strip_flags: StripDataFlags,
    pub cooked: u32,
    pub body_setup: i32,
    pub nav_collision: i32,
    pub lighting_guid: Guid,
    pub sockets: Vec<i32>,
    pub lods: Vec<StaticMeshLod>,
    pub num_inlined_lods: u8,
    pub nanite: NaniteResources,
    pub ray_tracing_proxy: Option<RayTracingProxy>,
    pub card_strip: StripDataFlags,
    /// Per LOD, `None` where the validity flag was zero. Absent entirely when
    /// the strip flags dropped the whole section.
    pub card_representations: Option<Vec<Option<CardRepresentation>>>,
    pub distance_field_strip: StripDataFlags,
    pub distance_fields: Option<Vec<Option<DistanceFieldVolume>>>,
    /// `Bounds`: an `FBoxSphereBounds`.
    pub bounds: BoxSphereBounds,
    pub lods_share_static_lighting: u32,
    /// `ScreenSize[MAX_STATIC_LODS_UE4]`.
    pub screen_sizes: [PerPlatformFloat; 8],
    pub render_data_strip: StripDataFlags,
    pub has_speed_tree_wind: u32,
    pub materials: Vec<StaticMaterial>,
}

impl StaticMeshTail {
    pub fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let cooked = r.u32()?;
        let body_setup = r.i32()?;
        let nav_collision = r.i32()?;
        let lighting_guid = { let mut g = Guid::default(); g.serialize(r)?; g };
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "Sockets", r.o - 4)?
        };
        let sockets = (0..n).map(|_| r.i32()).collect::<Result<Vec<_>>>()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "LODs", r.o - 4)?
        };
        let mut lods = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            lods.push(StaticMeshLod::read(r)?);
        }
        let num_inlined_lods = r.u8()?;
        let nanite = NaniteResources::read(r)?;

        let ray_tracing_proxy = (r.u32()? != 0)
            .then(|| -> Result<RayTracingProxy> {
                let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
                let using_rendering_lods = r.u32()?;
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "ray tracing proxy LODs", r.o - 4)?
                };
                let mut proxy_lods = Vec::with_capacity(n.min(16));
                for _ in 0..n {
                    let sections = (r.u32()? != 0)
                        .then(|| -> Result<Vec<[u8; 40]>> {
                            let n = {
                                let n = r.i32()?;
                                super::limits::bounded(
                                    n,
                                    MAX_NATIVE_COUNT,
                                    "proxy sections",
                                    r.o - 4,
                                )?
                            };
                            (0..n)
                                .map(|_| Ok(r.take(40)?.try_into().expect("40 bytes")))
                                .collect()
                        })
                        .transpose()?;
                    let owns_ray_tracing_geometry = r.u32()?;
                    let bulk_index = r.i32()?;
                    let payload = match ctx.bulk_data.get(bulk_index.max(0) as usize) {
                        Some(&(offset, size)) if offset as usize == ctx.origin + r.o => {
                            Some(r.take(size.max(0) as usize)?.to_vec())
                        }
                        _ => None,
                    };
                    proxy_lods.push(RayTracingProxyLod {
                        sections,
                        owns_ray_tracing_geometry,
                        bulk_index,
                        payload,
                    });
                }
                Ok(RayTracingProxy { strip_flags, using_rendering_lods, lods: proxy_lods })
            })
            .transpose()?;

        let card_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let card_representations = (card_strip.global & 2 == 0 && card_strip.class & 2 == 0)
            .then(|| -> Result<Vec<Option<CardRepresentation>>> {
                (0..lods.len())
                    .map(|_| {
                        if r.u32()? == 0 {
                            return Ok(None);
                        }
                        let bounds = { let mut b = Box3d::default(); b.serialize(r)?; b };
                        let mostly_two_sided = r.u32()?;
                        let n = {
                            let n = r.i32()?;
                            super::limits::bounded(n, MAX_NATIVE_COUNT, "CardBuildData", r.o - 4)?
                        };
                        let cards: Vec<LumenCardBuildData> = read_vec(r, "CardBuildData", n)?;
                        Ok(Some(CardRepresentation { bounds, mostly_two_sided, cards }))
                    })
                    .collect()
            })
            .transpose()?;

        let distance_field_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let distance_fields = (distance_field_strip.global & 2 == 0 && distance_field_strip.class & 1 == 0)
            .then(|| -> Result<Vec<Option<DistanceFieldVolume>>> {
                (0..lods.len())
                    .map(|_| {
                        if r.u32()? == 0 {
                            return Ok(None);
                        }
                        let local_space_mesh_bounds =
                            { let mut b = Box3f::default(); b.serialize(r)?; b };
                        let mostly_two_sided = r.u32()?;
                        let mut mips = [SparseDistanceFieldMip::default(); 3];
                        for m in &mut mips {
                            m.serialize(r)?;
                        }
                        let n = {
                            let n = r.i32()?;
                            super::limits::bounded(n, MAX_NATIVE_COUNT, "AlwaysLoadedMip", r.o - 4)?
                        };
                        Ok(Some(DistanceFieldVolume {
                            local_space_mesh_bounds,
                            mostly_two_sided,
                            mips,
                            always_loaded_mip: r.take(n)?.to_vec(),
                            streamable_mips_index: r.i32()?,
                        }))
                    })
                    .collect()
            })
            .transpose()?;

        let bounds = { let mut b = BoxSphereBounds::default(); b.serialize(r)?; b };
        let lods_share_static_lighting = r.u32()?;
        let mut screen_sizes = [PerPlatformFloat::default(); 8];
        for v in &mut screen_sizes {
            v.serialize(r)?;
        }
        let render_data_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let has_speed_tree_wind = r.u32()?;
        let n = bounded_count(r.i32()?, "StaticMaterials", r.o - 4)?;
        let materials: Vec<StaticMaterial> = read_vec(r, "StaticMaterials", n)?;

        Ok(StaticMeshTail {
            strip_flags,
            cooked,
            body_setup,
            nav_collision,
            lighting_guid,
            sockets,
            lods,
            num_inlined_lods,
            nanite,
            ray_tracing_proxy,
            card_strip,
            card_representations,
            distance_field_strip,
            distance_fields,
            bounds,
            lods_share_static_lighting,
            screen_sizes,
            render_data_strip,
            has_speed_tree_wind,
            materials,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.strip_flags.clone().serialize(ar)?;
        ar.u32(&mut self.cooked.to_owned())?;
        ar.i32(&mut self.body_setup.to_owned())?;
        ar.i32(&mut self.nav_collision.to_owned())?;
        self.lighting_guid.clone().serialize(ar)?;
        ar.i32(&mut (self.sockets.len() as i32))?;
        for s in &self.sockets {
            ar.i32(&mut s.to_owned())?;
        }
        ar.i32(&mut (self.lods.len() as i32))?;
        for l in &self.lods {
            l.write(ar)?;
        }
        ar.u8(&mut self.num_inlined_lods.to_owned())?;
        self.nanite.write(ar)?;

        match &self.ray_tracing_proxy {
            Some(p) => {
                ar.u32(&mut 1)?;
                p.strip_flags.clone().serialize(ar)?;
                ar.u32(&mut p.using_rendering_lods.to_owned())?;
                ar.i32(&mut (p.lods.len() as i32))?;
                for l in &p.lods {
                    match &l.sections {
                        Some(sec) => {
                            ar.u32(&mut 1)?;
                            ar.i32(&mut (sec.len() as i32))?;
                            for s in sec {
                                ar.raw(&mut s.to_vec(), 40)?;
                            }
                        }
                        None => ar.u32(&mut 0)?,
                    }
                    ar.u32(&mut l.owns_ray_tracing_geometry.to_owned())?;
                    ar.i32(&mut l.bulk_index.to_owned())?;
                    if let Some(p) = &l.payload {
                        let n = p.len();
                        ar.raw(&mut p.clone(), n)?;
                    }
                }
            }
            None => ar.u32(&mut 0)?,
        }

        self.card_strip.clone().serialize(ar)?;
        match (&self.card_representations, self.card_strip.global & 2 == 0 && self.card_strip.class & 2 == 0)
        {
            (Some(v), true) => {
                if v.len() != self.lods.len() {
                    bail!("{} card representations for {} LODs", v.len(), self.lods.len());
                }
                for c in v {
                    match c {
                        Some(c) => {
                            ar.u32(&mut 1)?;
                            c.bounds.clone().serialize(ar)?;
                            ar.u32(&mut c.mostly_two_sided.to_owned())?;
                            write_vec(ar, &c.cards)?;
                        }
                        None => ar.u32(&mut 0)?,
                    }
                }
            }
            (None, false) => {}
            _ => bail!("card representation presence disagrees with the strip flags"),
        }

        self.distance_field_strip.clone().serialize(ar)?;
        match (
            &self.distance_fields,
            self.distance_field_strip.global & 2 == 0 && self.distance_field_strip.class & 1 == 0,
        ) {
            (Some(v), true) => {
                if v.len() != self.lods.len() {
                    bail!("{} distance fields for {} LODs", v.len(), self.lods.len());
                }
                for d in v {
                    match d {
                        Some(d) => {
                            ar.u32(&mut 1)?;
                            d.local_space_mesh_bounds.clone().serialize(ar)?;
                            ar.u32(&mut d.mostly_two_sided.to_owned())?;
                            for m in &d.mips {
                                m.clone().serialize(ar)?;
                            }
                            ar.i32(&mut (d.always_loaded_mip.len() as i32))?;
                            let n = d.always_loaded_mip.len();
                            ar.raw(&mut d.always_loaded_mip.clone(), n)?;
                            ar.i32(&mut d.streamable_mips_index.to_owned())?;
                        }
                        None => ar.u32(&mut 0)?,
                    }
                }
            }
            (None, false) => {}
            _ => bail!("distance field presence disagrees with the strip flags"),
        }

        self.bounds.clone().serialize(ar)?;
        ar.u32(&mut self.lods_share_static_lighting.to_owned())?;
        for v in &self.screen_sizes {
            v.clone().serialize(ar)?;
        }
        self.render_data_strip.clone().serialize(ar)?;
        ar.u32(&mut self.has_speed_tree_wind.to_owned())?;
        write_vec(ar, &self.materials)?;
        Ok(())
    }
}

/// One LOD of a `UMorphTarget`.
#[derive(Debug, Clone, PartialEq)]
pub struct MorphLodModel {
    /// `true` when the vertex array was stripped and only its count is written.
    pub stripped: bool,
    /// The count when stripped, or the deltas themselves when not.
    pub vertices: Result2<i32, Vec<MorphTargetDelta>>,
    pub num_base_mesh_verts: i32,
    pub section_indices: Vec<i32>,
    pub generated_by_engine: u32,
    /// Empty in a cook, but written.
    pub source_filename: FStr,
}

/// A two-way choice that is not an error — named to avoid reading as `Result`.
#[derive(Debug, Clone, PartialEq)]
pub enum Result2<A, B> {
    A(A),
    B(B),
}

/// `UMorphTarget::Serialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct MorphTargetTail {
    pub strip_flags: u16,
    /// Absent when audio-visual data was stripped — the tail ends at the flags.
    pub lods: Option<Vec<MorphLodModel>>,
}

impl MorphTargetTail {
    fn read(r: &mut Reader) -> Result<Self> {
        let strip_flags = r.u16()?;
        if strip_flags & 0x02 != 0 {
            return Ok(MorphTargetTail { strip_flags, lods: None });
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "MorphLODModels", r.o - 4)?
        };
        let mut lods = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            let stripped = r.u32()? != 0;
            let vertices = if stripped {
                Result2::A(r.i32()?)
            } else {
                Result2::B({
                    let n = bounded_count(r.i32()?, "morph vertices", r.o - 4)?;
                    read_vec(r, "morph vertices", n)?
                })
            };
            lods.push(MorphLodModel {
                stripped,
                vertices,
                num_base_mesh_verts: r.i32()?,
                section_indices: read_i32_array(r, "SectionIndices")?,
                generated_by_engine: r.u32()?,
                source_filename: r.fstring()?,
            });
        }
        Ok(MorphTargetTail { strip_flags, lods: Some(lods) })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u16(&mut self.strip_flags.to_owned())?;
        match (&self.lods, self.strip_flags & 0x02 != 0) {
            (None, true) => return Ok(()),
            (Some(_), false) => {}
            _ => bail!("LOD presence disagrees with the strip flags"),
        }
        let lods = self.lods.as_ref().expect("checked above");
        ar.i32(&mut (lods.len() as i32))?;
        for l in lods {
            ar.u32(&mut u32::from(l.stripped))?;
            match (&l.vertices, l.stripped) {
                (Result2::A(n), true) => ar.i32(&mut n.to_owned())?,
                (Result2::B(a), false) => write_vec(ar, a)?,
                _ => bail!("morph vertex form disagrees with the stripped flag"),
            }
            ar.i32(&mut l.num_base_mesh_verts.to_owned())?;
            write_i32_array(ar, &l.section_indices)?;
            ar.u32(&mut l.generated_by_engine.to_owned())?;
            ar.fstring(&mut l.source_filename.clone())?;
        }
        Ok(())
    }
}

/// One streamed audio chunk of a `USoundWave`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    pub flags: u32,
    pub bulk: InlineBulkPayload,
    pub data_size: i32,
    pub audio_data_size: i32,
    /// Written only when the chunk's flags say it is seekable.
    pub seek_offset_in_audio_frames: Option<i32>,
}

/// `USoundWave::Serialize` in a cooked, streaming build.
#[derive(Debug, Clone)]
pub struct SoundWaveTail {
    pub flags: u32,
    pub cue_points: Vec<PropertyBlock>,
    pub compressed_data_guid: Guid,
    pub audio_format: FName,
    pub chunks: Vec<AudioChunk>,
}

impl SoundWaveTail {
    const SEEKABLE: u32 = 2;

    fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let flags = r.u32()?;
        if flags & 1 == 0 {
            bail!("uncooked SoundWave");
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "CuePoints", r.o - 4)?
        };
        let mut cue_points = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            cue_points.push(read_struct(r, "SoundWaveCuePoint", ctx.usmap, 0)?);
        }
        let compressed_data_guid = { let mut g = Guid::default(); g.serialize(r)?; g };
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "audio chunks", r.o - 4)?
        };
        // The format name comes *after* the chunk count, not before it.
        let audio_format = r.fname()?;
        let mut chunks = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let flags = r.u32()?;
            let bulk = InlineBulkPayload::read(r, ctx, "audio chunk")?;
            chunks.push(AudioChunk {
                flags,
                bulk,
                data_size: r.i32()?,
                audio_data_size: r.i32()?,
                seek_offset_in_audio_frames: (flags & Self::SEEKABLE != 0)
                    .then(|| r.i32())
                    .transpose()?,
            });
        }
        Ok(SoundWaveTail { flags, cue_points, compressed_data_guid, audio_format, chunks })
    }

    fn write(&self, ar: &mut impl Ar, ctx: TailContext) -> Result<()> {
        ar.u32(&mut self.flags.to_owned())?;
        ar.i32(&mut (self.cue_points.len() as i32))?;
        let flat = flattened_schema("SoundWaveCuePoint", ctx.usmap)?;
        for b in &self.cue_points {
            write_block(ar, b, &flat, ctx.usmap)?;
        }
        self.compressed_data_guid.clone().serialize(ar)?;
        ar.i32(&mut (self.chunks.len() as i32))?;
        ar.fname(&mut self.audio_format.clone())?;
        for c in &self.chunks {
            ar.u32(&mut c.flags.to_owned())?;
            c.bulk.write(ar)?;
            ar.i32(&mut c.data_size.to_owned())?;
            ar.i32(&mut c.audio_data_size.to_owned())?;
            match (c.seek_offset_in_audio_frames, c.flags & Self::SEEKABLE != 0) {
                (Some(v), true) => ar.i32(&mut v.to_owned())?,
                (None, false) => {}
                _ => bail!("seek offset presence disagrees with the chunk flags"),
            }
        }
        Ok(())
    }
}

/// One element of a `UModelComponent`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelElement {
    pub map_build_data_id: Guid,
    pub component: i32,
    pub material: i32,
    pub nodes: Vec<u16>,
}

/// `UModelComponent::Serialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelComponentTail {
    pub model: i32,
    pub elements: Vec<ModelElement>,
    pub component_index: u32,
    pub nodes: Vec<u16>,
}

impl ModelComponentTail {
    fn read(r: &mut Reader) -> Result<Self> {
        let model = r.i32()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "model elements", r.o - 4)?
        };
        let mut elements = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            elements.push(ModelElement {
                map_build_data_id: { let mut g = Guid::default(); g.serialize(r)?; g },
                component: r.i32()?,
                material: r.i32()?,
                nodes: read_u16_array(r, "element nodes")?,
            });
        }
        Ok(ModelComponentTail {
            model,
            elements,
            component_index: r.u32()?,
            nodes: read_u16_array(r, "component nodes")?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.model.to_owned())?;
        ar.i32(&mut (self.elements.len() as i32))?;
        for e in &self.elements {
            e.map_build_data_id.clone().serialize(ar)?;
            ar.i32(&mut e.component.to_owned())?;
            ar.i32(&mut e.material.to_owned())?;
            write_u16_array(ar, &e.nodes)?;
        }
        ar.u32(&mut self.component_index.to_owned())?;
        write_u16_array(ar, &self.nodes)
    }
}

/// One `FReferencePose` of a `USkeleton`'s retarget sources.
#[derive(Debug, Clone, PartialEq)]
pub struct RetargetSource {
    pub key: FName,
    pub pose_name: FName,
    pub reference_pose: Vec<u8>,
}

/// `USkeleton::Serialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkeletonTail {
    pub reference_skeleton: ReferenceSkeleton,
    pub retarget_sources: Vec<RetargetSource>,
    pub guid: Guid,
    pub strip_flags: StripDataFlags,
}

impl SkeletonTail {
    fn read(r: &mut Reader) -> Result<Self> {
        let reference_skeleton = ReferenceSkeleton::read(r)?;
        // Retarget poses use the same `FTransform` width the reference skeleton
        // had to discover, which is why that answer is worth keeping.
        let tsize = reference_skeleton.transform_size;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "AnimRetargetSources", r.o - 4)?
        };
        let mut retarget_sources = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let key = r.fname()?;
            let pose_name = r.fname()?;
            let m = {
                let m = r.i32()?;
                super::limits::bounded(m, MAX_NATIVE_COUNT, "FReferencePose", r.o - 4)?
            };
            retarget_sources.push(RetargetSource {
                key,
                pose_name,
                reference_pose: r.take(m * tsize)?.to_vec(),
            });
        }
        let guid = { let mut g = Guid::default(); g.serialize(r)?; g };
        let smart_names = r.i32()?;
        if smart_names != 0 {
            bail!("non-empty deprecated SmartNames container");
        }
        Ok(SkeletonTail {
            reference_skeleton,
            retarget_sources,
            guid,
            strip_flags: { let mut f = StripDataFlags::default(); f.serialize(r)?; f },
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.reference_skeleton.write(ar)?;
        let tsize = self.reference_skeleton.transform_size;
        ar.i32(&mut (self.retarget_sources.len() as i32))?;
        for s in &self.retarget_sources {
            ar.fname(&mut s.key.clone())?;
            ar.fname(&mut s.pose_name.clone())?;
            if tsize == 0 || s.reference_pose.len() % tsize != 0 {
                bail!("retarget pose is {} bytes for {tsize}-byte transforms", s.reference_pose.len());
            }
            ar.i32(&mut ((s.reference_pose.len() / tsize) as i32))?;
            let n = s.reference_pose.len();
            ar.raw(&mut s.reference_pose.clone(), n)?;
        }
        self.guid.clone().serialize(ar)?;
        ar.i32(&mut 0)?; // the deprecated SmartNames container, always empty
        ar.u8(&mut self.strip_flags.global.to_owned())?;
        ar.u8(&mut self.strip_flags.class.to_owned())
    }
}

/// `UStruct::Serialize` — the layer under every function, class and script
/// struct: 11,250 exports across seven classes.
///
/// The `FField` chain and the Kismet bytecode are both their own sub-formats with
/// no writer in this crate, so they stay spans. What the model owns is the
/// framing: the super-struct reference, the child list, and the two sizes.
#[derive(Debug, Clone, PartialEq)]
pub struct StructTail {
    pub super_struct: i32,
    /// `ChildArray` — an `FPackageIndex` per entry.
    pub children: Vec<i32>,
    /// The `FField` chain, exactly as `read_field_chain` consumed it.
    pub field_chain: Vec<u8>,
    /// Written ahead of the script and *not* equal to its length — the engine
    /// writes a bytecode size and a separate storage size.
    pub bytecode_size: i32,
    pub script: Vec<u8>,
}

impl StructTail {
    fn read(r: &mut Reader) -> Result<Self> {
        let super_struct = r.i32()?;
        let children = read_i32_array(r, "ChildArray")?;
        let at = r.o;
        r.struct_fields = Some(super::reflect::read_field_chain(r)?);
        let field_chain = r.b[at..r.o].to_vec();
        let bytecode_size = r.i32()?;
        let storage = r.i32()?;
        if !(0..=16_000_000).contains(&storage) {
            bail!("implausible ScriptStorageSize {storage}");
        }
        Ok(StructTail {
            super_struct,
            children,
            field_chain,
            bytecode_size,
            script: r.take(storage as usize)?.to_vec(),
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.super_struct.to_owned())?;
        write_i32_array(ar, &self.children)?;
        let n = self.field_chain.len();
        ar.raw(&mut self.field_chain.clone(), n)?;
        ar.i32(&mut self.bytecode_size.to_owned())?;
        ar.i32(&mut (self.script.len() as i32))?;
        let n = self.script.len();
        ar.raw(&mut self.script.clone(), n)
    }
}

/// `UFunction::Serialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionTail {
    pub function_flags: u32,
    /// `RepOffset`, written only for `FUNC_Net`.
    pub rep_offset: Option<u16>,
    pub event_graph_function: i32,
    pub event_graph_call_offset: i32,
}

impl FunctionTail {
    const FUNC_NET: u32 = 0x0040;

    fn read(r: &mut Reader) -> Result<Self> {
        let function_flags = r.u32()?;
        Ok(FunctionTail {
            function_flags,
            rep_offset: (function_flags & Self::FUNC_NET != 0).then(|| r.u16()).transpose()?,
            event_graph_function: r.i32()?,
            event_graph_call_offset: r.i32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.function_flags.to_owned())?;
        match (self.rep_offset, self.function_flags & Self::FUNC_NET != 0) {
            (Some(v), true) => ar.u16(&mut v.to_owned())?,
            (None, false) => {}
            _ => bail!("RepOffset presence disagrees with FUNC_Net"),
        }
        ar.i32(&mut self.event_graph_function.to_owned())?;
        ar.i32(&mut self.event_graph_call_offset.to_owned())
    }
}

/// `UClass::Serialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassTail {
    pub func_map: Vec<FuncMapEntry>,
    pub class_flags: u32,
    pub class_within: i32,
    pub class_config_name: FName,
    pub class_generated_by: i32,
    pub interfaces: Vec<ImplementedInterface>,
    pub deprecated_flag: u32,
    pub deprecated_name: FName,
    pub cooked: u32,
    pub class_default_object: i32,
}

impl ClassTail {
    fn read(r: &mut Reader) -> Result<Self> {
        Ok(ClassTail {
            func_map: {
                let n = bounded_count(r.i32()?, "FuncMap", r.o - 4)?;
                read_vec(r, "FuncMap", n)?
            },
            class_flags: r.u32()?,
            class_within: r.i32()?,
            class_config_name: r.fname()?,
            class_generated_by: r.i32()?,
            interfaces: {
                let n = bounded_count(r.i32()?, "Interfaces", r.o - 4)?;
                read_vec(r, "Interfaces", n)?
            },
            deprecated_flag: r.u32()?,
            deprecated_name: r.fname()?,
            cooked: r.u32()?,
            class_default_object: r.i32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        write_vec(ar, &self.func_map)?;
        ar.u32(&mut self.class_flags.to_owned())?;
        ar.i32(&mut self.class_within.to_owned())?;
        ar.fname(&mut self.class_config_name.clone())?;
        ar.i32(&mut self.class_generated_by.to_owned())?;
        write_vec(ar, &self.interfaces)?;
        ar.u32(&mut self.deprecated_flag.to_owned())?;
        ar.fname(&mut self.deprecated_name.clone())?;
        ar.u32(&mut self.cooked.to_owned())?;
        ar.i32(&mut self.class_default_object.to_owned())
    }
}

/// `UBlueprintGeneratedClass::Serialize`'s editor tags.
///
/// Only read when more than four bytes remain, which is the engine's own guard
/// against reading a short tail's last word as a count.
#[derive(Debug, Clone, PartialEq)]
pub struct BlueprintGeneratedClassTail {
    pub editor_tags: Option<Vec<(FName, FStr)>>,
}

impl BlueprintGeneratedClassTail {
    fn read(r: &mut Reader) -> Result<Self> {
        if r.b.len().saturating_sub(r.o) <= 4 {
            return Ok(BlueprintGeneratedClassTail { editor_tags: None });
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "EditorTags", r.o - 4)?
        };
        let mut tags = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            tags.push((r.fname()?, r.fstring()?));
        }
        Ok(BlueprintGeneratedClassTail { editor_tags: Some(tags) })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        if let Some(tags) = &self.editor_tags {
            ar.i32(&mut (tags.len() as i32))?;
            for (n, v) in tags {
                ar.fname(&mut n.clone())?;
                ar.fstring(&mut v.clone())?;
            }
        }
        Ok(())
    }
}

/// One element of a tail that is just a flat sequence of typed pieces.
///
/// Most of the long tail is this: 40-odd classes whose whole contribution is a
/// couple of scalars, a reflected struct, or a fixed-width array. Writing a
/// bespoke struct for each would be forty near-identical types, so the shape is
/// declared as data in [`COMPOSED_TAILS`] and decoded generically. Nothing is
/// retained — each piece becomes a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TailPiece {
    /// `UActorComponent`'s UCS-modified-property list — 28 bytes per entry.
    UcsProperties,
    /// `USceneComponent`'s baked bounds, which exist only when the property
    /// block asks for them.
    SceneBounds,
    /// A four-byte present flag and, when set, an `FString`.
    OptStr,
    /// A fixed run of bytes whose interior another serializer owns.
    Bytes(usize),
    /// An `FGuid`.
    Guid,
    I32,
    U32,
    U16,
    U8,
    Str,
    /// A reflected struct, read against the `.usmap`.
    Struct(&'static str),
    /// A `TArray<FPackageIndex>` — object references.
    ObjectArray,
    /// `UEnum::Names`: an `FName` and the `int64` value it maps to.
    EnumNameArray,
    /// `UPhysicsAsset::CollisionDisableTable`: two body indices and a flag.
    CollisionDisableArray,
    /// A `TArray<FVector2f>`.
    Vector2fArray,
    /// A `TArray` of `FString`s.
    StrArray,
    /// A `TArray` of `FString` pairs.
    StrPairArray,
    /// A four-byte present flag and, when set, an `int32`.
    OptI32,
}

/// A decoded [`TailPiece`].
#[derive(Debug, Clone)]
pub enum TailValue {
    UcsProperties(Vec<UcsModifiedProperty>),
    SceneBounds(Option<Option<[u8; 56]>>),
    OptStr(Option<FStr>),
    Bytes(Vec<u8>),
    Guid(Guid),
    I32(i32),
    U32(u32),
    U16(u16),
    U8(u8),
    Str(FStr),
    Struct(PropertyBlock),
    ObjectArray(Vec<i32>),
    EnumNameArray(Vec<(FName, i64)>),
    CollisionDisableArray(Vec<(i32, i32, u32)>),
    Vector2fArray(Vec<Vector2f>),
    StrArray(Vec<FStr>),
    StrPairArray(Vec<(FStr, FStr)>),
    OptI32(Option<i32>),
}

/// Families whose whole tail is a flat piece sequence, keyed the same way
/// [`tail_owners`] reports them.
///
/// A key naming several classes is the inheritance chain, base last, so the
/// pieces are listed base-first to match the order they are written in.
pub const COMPOSED_TAILS: &[(&str, &[TailPiece])] = &[
    // 52 classes, 19,506 exports: every component that is not a *scene*
    // component and adds nothing of its own.
    ("ActorComponent", &[TailPiece::UcsProperties]),
    (
        "SkyAtmosphereComponent+SceneComponent+ActorComponent",
        &[TailPiece::UcsProperties, TailPiece::SceneBounds, TailPiece::Guid],
    ),
    ("LevelInstance+Actor", &[TailPiece::OptStr, TailPiece::Guid, TailPiece::Guid, TailPiece::Guid]),
    // `UWorld`: the persistent level, then two object arrays.
    ("World", &[TailPiece::I32, TailPiece::ObjectArray, TailPiece::ObjectArray]),
    ("NiagaraDataInterfaceTexture", &[TailPiece::U32]),
    ("Font", &[TailPiece::U32]),
    ("FileMediaSource", &[TailPiece::Bytes(8)]),
    // `UTexture` alone writes its strip flags.
    ("Texture", &[TailPiece::Bytes(2)]),
    ("AnimationAsset", &[TailPiece::Guid]),
    ("AkRtpc", &[TailPiece::Struct("WwiseGameParameterCookedData")]),
    ("AkStateValue", &[TailPiece::Struct("WwiseGroupValueCookedData")]),
    ("AkSwitchValue", &[TailPiece::Struct("WwiseGroupValueCookedData")]),
    ("AkInitBank", &[TailPiece::Struct("WwiseInitBankCookedData")]),
    ("AkAuxBus", &[TailPiece::Struct("WwiseLocalizedAuxBusCookedData"), TailPiece::I32]),
    // `UEnum`: an `FName` and an `int64` per entry, then `CppForm`.
    ("Enum", &[TailPiece::EnumNameArray, TailPiece::U8]),
    ("FontFace", &[TailPiece::U32, TailPiece::U32]),
    ("NiagaraSpriteRendererProperties", &[TailPiece::Vector2fArray]),
    ("PhysicsAsset", &[TailPiece::CollisionDisableArray]),
    ("WorldPartition", &[TailPiece::OptI32]),
    ("SoundNode", &[TailPiece::Bytes(2)]),
    ("SoundCue", &[TailPiece::Bytes(2)]),
    ("SoundNodeWavePlayer+SoundNode", &[TailPiece::Bytes(2), TailPiece::I32]),
];

/// Read a piece sequence.
fn read_pieces(
    r: &mut Reader,
    pieces: &[TailPiece],
    block: &PropertyBlock,
    ctx: TailContext,
) -> Result<Vec<TailValue>> {
    let mut out = Vec::with_capacity(pieces.len());
    for p in pieces {
        out.push(match *p {
            TailPiece::UcsProperties => {
                let n = bounded_count(r.i32()?, "UCSModifiedProperties", r.o - 4)?;
                TailValue::UcsProperties(read_vec(r, "UCSModifiedProperties", n)?)
            }
            TailPiece::SceneBounds => TailValue::SceneBounds(
                if scene_component_writes_bounds(block) {
                    Some(if r.u32()? != 0 {
                        Some(r.take(56)?.try_into().expect("56 bytes"))
                    } else {
                        None
                    })
                } else {
                    None
                },
            ),
            TailPiece::OptStr => {
                TailValue::OptStr((r.u32()? != 0).then(|| r.fstring()).transpose()?)
            }
            TailPiece::Bytes(n) => TailValue::Bytes(r.take(n)?.to_vec()),
            TailPiece::Guid => TailValue::Guid({
                let mut g = Guid::default();
                g.serialize(r)?;
                g
            }),
            TailPiece::I32 => TailValue::I32(r.i32()?),
            TailPiece::U32 => TailValue::U32(r.u32()?),
            TailPiece::U16 => TailValue::U16(r.u16()?),
            TailPiece::U8 => TailValue::U8(r.u8()?),
            TailPiece::Str => TailValue::Str(r.fstring()?),
            TailPiece::Struct(name) => TailValue::Struct(read_struct(r, name, ctx.usmap, 0)?),
            TailPiece::ObjectArray => TailValue::ObjectArray(read_i32_array(r, "object array")?),
            TailPiece::EnumNameArray => {
                let n = bounded_count(r.i32()?, "Enum Names", r.o - 4)?;
                let mut v = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    v.push((r.fname()?, r.u64()? as i64));
                }
                TailValue::EnumNameArray(v)
            }
            TailPiece::CollisionDisableArray => {
                let n = bounded_count(r.i32()?, "CollisionDisableTable", r.o - 4)?;
                let mut v = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    v.push((r.i32()?, r.i32()?, r.u32()?));
                }
                TailValue::CollisionDisableArray(v)
            }
            TailPiece::Vector2fArray => {
                let n = bounded_count(r.i32()?, "BoundingGeometry", r.o - 4)?;
                TailValue::Vector2fArray(read_vec(r, "BoundingGeometry", n)?)
            }
            TailPiece::StrArray => {
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "tail string array", r.o - 4)?
                };
                TailValue::StrArray((0..n).map(|_| r.fstring()).collect::<Result<_>>()?)
            }
            TailPiece::OptI32 => {
                TailValue::OptI32((r.u32()? != 0).then(|| r.i32()).transpose()?)
            }
            TailPiece::StrPairArray => {
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "tail string pairs", r.o - 4)?
                };
                let mut v = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    v.push((r.fstring()?, r.fstring()?));
                }
                TailValue::StrPairArray(v)
            }
        });
    }
    Ok(out)
}

/// Write a piece sequence back. Errors rather than guessing if a value does not
/// match the piece it was decoded from.
fn write_pieces(
    ar: &mut impl Ar,
    values: &[TailValue],
    pieces: &[TailPiece],
    block: &PropertyBlock,
    ctx: TailContext,
) -> Result<()> {
    if values.len() != pieces.len() {
        bail!("{} values for {} tail pieces", values.len(), pieces.len());
    }
    for (v, p) in values.iter().zip(pieces) {
        match (v, *p) {
            (TailValue::UcsProperties(u), TailPiece::UcsProperties) => write_vec(ar, u)?,
            (TailValue::SceneBounds(b), TailPiece::SceneBounds) => {
                match (b, scene_component_writes_bounds(block)) {
                    (Some(b), true) => match b {
                        Some(bounds) => {
                            ar.u32(&mut 1)?;
                            ar.raw(&mut bounds.to_vec(), 56)?;
                        }
                        None => ar.u32(&mut 0)?,
                    },
                    (None, false) => {}
                    _ => bail!("scene component bounds disagree with the property block"),
                }
            }
            (TailValue::OptStr(s), TailPiece::OptStr) => match s {
                Some(s) => {
                    ar.u32(&mut 1)?;
                    ar.fstring(&mut s.clone())?;
                }
                None => ar.u32(&mut 0)?,
            },
            (TailValue::Bytes(b), TailPiece::Bytes(n)) => {
                if b.len() != n {
                    bail!("{} bytes for a {n}-byte piece", b.len());
                }
                ar.raw(&mut b.clone(), n)?;
            }
            (TailValue::Guid(g), TailPiece::Guid) => g.clone().serialize(ar)?,
            (TailValue::I32(x), TailPiece::I32) => ar.i32(&mut x.to_owned())?,
            (TailValue::U32(x), TailPiece::U32) => ar.u32(&mut x.to_owned())?,
            (TailValue::U16(x), TailPiece::U16) => ar.u16(&mut x.to_owned())?,
            (TailValue::U8(x), TailPiece::U8) => ar.u8(&mut x.to_owned())?,
            (TailValue::Str(s), TailPiece::Str) => ar.fstring(&mut s.clone())?,
            (TailValue::Struct(b), TailPiece::Struct(name)) => {
                let flat = flattened_schema(name, ctx.usmap)?;
                write_block(ar, b, &flat, ctx.usmap)?;
            }
            (TailValue::ObjectArray(v), TailPiece::ObjectArray) => write_i32_array(ar, v)?,
            (TailValue::EnumNameArray(v), TailPiece::EnumNameArray) => {
                ar.i32(&mut (v.len() as i32))?;
                for (n, value) in v {
                    ar.fname(&mut n.clone())?;
                    ar.u64(&mut (*value as u64))?;
                }
            }
            (TailValue::CollisionDisableArray(v), TailPiece::CollisionDisableArray) => {
                ar.i32(&mut (v.len() as i32))?;
                for (a1, b1, c1) in v {
                    ar.i32(&mut a1.to_owned())?;
                    ar.i32(&mut b1.to_owned())?;
                    ar.u32(&mut c1.to_owned())?;
                }
            }
            (TailValue::Vector2fArray(v), TailPiece::Vector2fArray) => write_vec(ar, v)?,
            (TailValue::StrArray(v), TailPiece::StrArray) => {
                ar.i32(&mut (v.len() as i32))?;
                for s in v {
                    ar.fstring(&mut s.clone())?;
                }
            }
            (TailValue::StrPairArray(v), TailPiece::StrPairArray) => {
                ar.i32(&mut (v.len() as i32))?;
                for (a, b) in v {
                    ar.fstring(&mut a.clone())?;
                    ar.fstring(&mut b.clone())?;
                }
            }
            (TailValue::OptI32(x), TailPiece::OptI32) => match x {
                Some(v) => {
                    ar.u32(&mut 1)?;
                    ar.i32(&mut v.to_owned())?;
                }
                None => ar.u32(&mut 0)?,
            },
            (v, p) => bail!("tail value {v:?} does not match piece {p:?}"),
        }
    }
    Ok(())
}

/// A Chaos shared pointer as written by `FChaosArchive`.
///
/// The tag is an object-graph identity: the *first* time a tag appears its object
/// follows, and a repeat is a back-reference with nothing behind it. So the model
/// records whether the payload was there rather than re-deriving it, which is
/// what lets a single pointer be written without replaying the whole graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ChaosPtr {
    pub present: bool,
    pub tag: Option<i32>,
    /// The object's own bytes — Chaos's recursive implicit-object or BVH
    /// encoding, kept whole. Its own serializer owns the interior.
    pub payload: Option<Vec<u8>>,
}

impl ChaosPtr {
    fn read(
        r: &mut Reader,
        seen: &mut std::collections::HashSet<i32>,
        read_body: impl FnOnce(&mut Reader) -> Result<()>,
    ) -> Result<Self> {
        if r.u32()? == 0 {
            return Ok(ChaosPtr { present: false, tag: None, payload: None });
        }
        let tag = r.i32()?;
        if !seen.insert(tag) {
            return Ok(ChaosPtr { present: true, tag: Some(tag), payload: None });
        }
        let at = r.o;
        read_body(r)?;
        Ok(ChaosPtr { present: true, tag: Some(tag), payload: Some(r.b[at..r.o].to_vec()) })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        if !self.present {
            return ar.u32(&mut 0);
        }
        ar.u32(&mut 1)?;
        let Some(tag) = self.tag else { bail!("a present Chaos pointer has no tag") };
        ar.i32(&mut tag.to_owned())?;
        if let Some(p) = &self.payload {
            let n = p.len();
            ar.raw(&mut p.clone(), n)?;
        }
        Ok(())
    }
}

/// One attribute's values inside an `FManagedArrayCollection`.
///
/// `FManagedArrayCollection` is a tagged union: the attribute's `EManagedArrayType`
/// id picks the element type, exactly as it does in the engine. So this is an
/// enum over the element types the corpus actually contains
/// (`ce_managed_array_census`), and an id outside that set is an error rather
/// than an untyped span — a new type should announce itself, not decode to
/// bytes that look fine.
///
/// The bulk kinds keep the element size the stream declares. It is real data
/// there — `TryBulkSerializeManagedArray` writes it — and deriving it from the
/// Rust type would silently paper over a disagreement.
#[derive(Debug, Clone, PartialEq)]
pub enum ManagedArrayValues {
    Vector { element_size: i32, items: Vec<Vector3f> },
    IntVector { element_size: i32, items: Vec<Int32Vector> },
    Vector2D { element_size: i32, items: Vec<Vector2f> },
    Int32 { element_size: i32, items: Vec<i32> },
    Bool { element_size: i32, items: Vec<u8> },
    Float { element_size: i32, items: Vec<f32> },
    LinearColor(Vec<LinearColor>),
    /// `Transform` is the LWC-double form, `Transform3f` the float one; both
    /// decode to the same type and remember which width to write.
    Transform { double: bool, items: Vec<Transform> },
    MeshSection(Vec<GeometryCollectionSection>),
    Box(Vec<Box3d>),
    Strings(Vec<FStr>),
    /// `IntArray` — an array of arrays.
    IntArray(Vec<Vec<i32>>),
    /// Chaos implicit objects or BVH particles, one shared pointer each.
    ChaosPointers(Vec<ChaosPtr>),
}

/// One attribute of an `FManagedArrayCollection`.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedArrayAttribute {
    pub name: FName,
    pub group: FName,
    pub value_type_version: i32,
    pub type_id: i32,
    pub group_index_dependency: FName,
    pub persistent: u32,
    pub array_version: i32,
    pub values: ManagedArrayValues,
}

/// `FManagedArrayCollection` — the attribute store a geometry collection is
/// built out of.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedArrayCollection {
    pub version: i32,
    /// An `FName` key and its `FGroupInfo` — a version and a size.
    pub groups: Vec<(FName, i32, i32)>,
    pub attributes: Vec<ManagedArrayAttribute>,
}

impl ManagedArrayCollection {
    fn read(r: &mut Reader) -> Result<Self> {
        use super::tails::{
            managed_array_elem, managed_array_is_bulk, managed_array_nested_elem,
            read_bvh_particles, read_chaos_implicit_object, MANAGED_ARRAY_TYPES,
        };
        let version = r.i32()?;
        let n = bounded_count(r.i32()?, "collection groups", r.o - 4)?;
        let mut groups = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            groups.push((r.fname()?, r.i32()?, r.i32()?));
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "collection attributes", r.o - 4)?
        };
        // One dedup set for the whole collection: a Chaos tag seen under one
        // attribute is a back-reference when it reappears under another.
        let mut seen = std::collections::HashSet::new();
        let mut attributes = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            let name = r.fname()?;
            let group = r.fname()?;
            let value_type_version = r.i32()?;
            let type_id = r.i32()?;
            let group_index_dependency = r.fname()?;
            let persistent = r.u32()?;
            let ty = MANAGED_ARRAY_TYPES.get(type_id as usize).copied().unwrap_or("?");
            let array_version = r.i32()?;
            let count = |r: &mut Reader, what: &str| -> Result<usize> {
                let n = r.i32()?;
                super::limits::bounded(n, MAX_NATIVE_COUNT, what, r.o - 4)
            };
            // Bulk kinds are self-describing: the stream writes an element
            // size and a count, and the size is kept because it is data.
            let mut bulk = |r: &mut Reader, expect: usize| -> Result<(i32, usize)> {
                let element_size = r.i32()?;
                let n = r.i32()?;
                if element_size < 0 || n < 0 {
                    bail!("implausible bulk managed array {element_size}x{n} @ {}", r.o - 8);
                }
                // The declared width is the authority. Decoding `n` elements at
                // the Rust type's width when the file says otherwise desyncs the
                // whole collection, so a disagreement is an error that names
                // both numbers rather than a silent misread.
                if n > 0 && element_size as usize != expect {
                    bail!(
                        "managed array of {ty} declares {element_size}-byte elements, \
                         the model has {expect} @ {}",
                        r.o - 8
                    );
                }
                Ok((element_size, n as usize))
            };
            let values = match ty {
                "Vector" => {
                    let (element_size, n) = bulk(r, Vector3f::SIZE)?;
                    ManagedArrayValues::Vector { element_size, items: read_vec(r, ty, n)? }
                }
                "IntVector" => {
                    let (element_size, n) = bulk(r, Int32Vector::SIZE)?;
                    ManagedArrayValues::IntVector { element_size, items: read_vec(r, ty, n)? }
                }
                "Vector2D" => {
                    let (element_size, n) = bulk(r, Vector2f::SIZE)?;
                    ManagedArrayValues::Vector2D { element_size, items: read_vec(r, ty, n)? }
                }
                "Int32" => {
                    let (element_size, n) = bulk(r, 4)?;
                    let items = (0..n).map(|_| r.i32()).collect::<Result<_>>()?;
                    ManagedArrayValues::Int32 { element_size, items }
                }
                "Bool" => {
                    let (element_size, n) = bulk(r, 1)?;
                    ManagedArrayValues::Bool { element_size, items: r.take(n)?.to_vec() }
                }
                "Float" => {
                    let (element_size, n) = bulk(r, 4)?;
                    let items = (0..n).map(|_| r.f32()).collect::<Result<_>>()?;
                    ManagedArrayValues::Float { element_size, items }
                }
                "String" => {
                    let n = bounded_count(r.i32()?, "collection strings", r.o - 4)?;
                    ManagedArrayValues::Strings((0..n).map(|_| r.fstring()).collect::<Result<_>>()?)
                }
                "LinearColor" => {
                    let n = bounded_count(r.i32()?, ty, r.o - 4)?;
                    ManagedArrayValues::LinearColor(read_vec(r, ty, n)?)
                }
                "MeshSection" => {
                    let n = bounded_count(r.i32()?, ty, r.o - 4)?;
                    ManagedArrayValues::MeshSection(read_vec(r, ty, n)?)
                }
                "Box" => {
                    let n = bounded_count(r.i32()?, ty, r.o - 4)?;
                    ManagedArrayValues::Box(read_vec(r, ty, n)?)
                }
                "Transform" | "Transform3f" => {
                    let double = ty == "Transform";
                    let width =
                        if double { Transform::SIZE_DOUBLE } else { Transform::SIZE_FLOAT };
                    let n = bounded_count(r.i32()?, ty, r.o - 4)?;
                    let mut items = Vec::with_capacity(n.min(4096));
                    for _ in 0..n {
                        let mut t = Transform::default();
                        t.serialize(r, width)?;
                        items.push(t);
                    }
                    ManagedArrayValues::Transform { double, items }
                }
                "IntArray" => {
                    let n = bounded_count(r.i32()?, "collection nested array", r.o - 4)?;
                    let mut arrays = Vec::with_capacity(n.min(256));
                    for _ in 0..n {
                        arrays.push(read_i32_array(r, "collection nested element")?);
                    }
                    ManagedArrayValues::IntArray(arrays)
                }
                "ImplicitObjectRefCountedPtr" | "ConvexRefCountedPtr" => {
                    let n = bounded_count(r.i32()?, "collection implicit objects", r.o - 4)?;
                    let mut ptrs = Vec::with_capacity(n.min(256));
                    for _ in 0..n {
                        ptrs.push(ChaosPtr::read(r, &mut seen, read_chaos_implicit_object)?);
                    }
                    ManagedArrayValues::ChaosPointers(ptrs)
                }
                "BVHParticlesFloat3UniquePointer" => {
                    let n = bounded_count(r.i32()?, "collection BVH particles", r.o - 4)?;
                    let mut ptrs = Vec::with_capacity(n.min(256));
                    for _ in 0..n {
                        ptrs.push(ChaosPtr::read(r, &mut seen, read_bvh_particles)?);
                    }
                    ManagedArrayValues::ChaosPointers(ptrs)
                }
                _ => bail!("unmodeled managed array type {ty} ({type_id}) @ {}", r.o),
            };
            attributes.push(ManagedArrayAttribute {
                name,
                group,
                value_type_version,
                type_id,
                group_index_dependency,
                persistent,
                array_version,
                values,
            });
        }
        Ok(ManagedArrayCollection { version, groups, attributes })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.version.to_owned())?;
        ar.i32(&mut (self.groups.len() as i32))?;
        for (name, version, size) in &self.groups {
            ar.fname(&mut name.clone())?;
            ar.i32(&mut version.to_owned())?;
            ar.i32(&mut size.to_owned())?;
        }
        ar.i32(&mut (self.attributes.len() as i32))?;
        for a in &self.attributes {
            ar.fname(&mut a.name.clone())?;
            ar.fname(&mut a.group.clone())?;
            ar.i32(&mut a.value_type_version.to_owned())?;
            ar.i32(&mut a.type_id.to_owned())?;
            ar.fname(&mut a.group_index_dependency.clone())?;
            ar.u32(&mut a.persistent.to_owned())?;
            ar.i32(&mut a.array_version.to_owned())?;
            match &a.values {
                ManagedArrayValues::Vector { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    write_run_counted(ar, items)?;
                }
                ManagedArrayValues::IntVector { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    write_run_counted(ar, items)?;
                }
                ManagedArrayValues::Vector2D { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    write_run_counted(ar, items)?;
                }
                ManagedArrayValues::Int32 { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    write_i32_array(ar, items)?;
                }
                ManagedArrayValues::Bool { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    write_byte_array(ar, items)?;
                }
                ManagedArrayValues::Float { element_size, items } => {
                    ar.i32(&mut element_size.to_owned())?;
                    ar.i32(&mut (items.len() as i32))?;
                    for v in items {
                        ar.f32(&mut v.to_owned())?;
                    }
                }
                ManagedArrayValues::Strings(v) => {
                    ar.i32(&mut (v.len() as i32))?;
                    for x in v {
                        ar.fstring(&mut x.clone())?;
                    }
                }
                ManagedArrayValues::LinearColor(v) => write_vec(ar, v)?,
                ManagedArrayValues::MeshSection(v) => write_vec(ar, v)?,
                ManagedArrayValues::Box(v) => write_vec(ar, v)?,
                ManagedArrayValues::Transform { double, items } => {
                    let width =
                        if *double { Transform::SIZE_DOUBLE } else { Transform::SIZE_FLOAT };
                    ar.i32(&mut (items.len() as i32))?;
                    for t in items {
                        t.clone().serialize(ar, width)?;
                    }
                }
                ManagedArrayValues::IntArray(arrays) => {
                    ar.i32(&mut (arrays.len() as i32))?;
                    for inner in arrays {
                        write_i32_array(ar, inner)?;
                    }
                }
                ManagedArrayValues::ChaosPointers(v) => {
                    ar.i32(&mut (v.len() as i32))?;
                    for p in v {
                        p.write(ar)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// `FGeometryCollectionMeshResources` plus its description.
#[derive(Debug, Clone, PartialEq)]
pub struct GeometryCollectionMesh {
    /// The index buffer comes **first** here, unlike `FStaticMeshLODResources`,
    /// and each buffer writes its own strip flags because they are serialized
    /// individually rather than under one shared set.
    pub index_buffer: RawStaticIndexBuffer,
    /// `FPositionVertexBuffer::Serialize` has **no** strip flags.
    pub position_stride: i32,
    pub position_num_vertices: i32,
    pub positions: BulkArray,
    pub vertex_strip: StripDataFlags,
    pub num_tex_coords: i32,
    pub vertex_num_vertices: i32,
    pub use_full_precision_uvs: u32,
    pub use_high_precision_tangent_basis: u32,
    pub tangents_and_uvs: Option<(BulkArray, BulkArray)>,
    pub color_strip: StripDataFlags,
    pub color_stride: i32,
    pub color_num_vertices: i32,
    pub colors: Option<BulkArray>,
    /// `FBoneMapVertexBuffer::Serialize` has no strip flags either.
    pub bone_map_num_vertices: i32,
    pub bone_map: BulkArray,
    pub description_num_vertices: i32,
    pub description_num_triangles: i32,
    pub pre_skinned_bounds: BoxSphereBounds,
    /// `Sections`, `SectionsNoInternal`, `SubSections` — 20-byte
    /// `FGeometryCollectionMeshElement` each.
    pub sections: [Vec<GeometryCollectionMeshElement>; 3],
}

/// `UGeometryCollection`'s tail: the managed array collection, then the cooked
/// render data.
#[derive(Debug, Clone, PartialEq)]
pub struct GeometryCollectionTail {
    pub is_cooked_or_cooking: u32,
    pub collection: ManagedArrayCollection,
    pub cooked: bool,
    pub mesh: Option<GeometryCollectionMesh>,
    pub nanite: Option<NaniteResources>,
}

impl GeometryCollectionTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let is_cooked_or_cooking = r.u32()?;
        let collection = ManagedArrayCollection::read(r)?;
        let cooked = r.u32()? != 0;
        let (mut mesh, mut nanite) = (None, None);
        if cooked {
            let has_mesh = r.u32()? != 0;
            let has_nanite = r.u32()? != 0;
            if has_mesh {
                let index_buffer = RawStaticIndexBuffer::read(r)?;
                let position_stride = r.i32()?;
                let position_num_vertices = r.i32()?;
                let positions = BulkArray::read(r, "collection positions")?;
                let vertex_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
                let num_tex_coords = r.i32()?;
        let vertex_num_vertices = r.i32()?;
        let use_full_precision_uvs = r.u32()?;
        let use_high_precision_tangent_basis = r.u32()?;
                let tangents_and_uvs = (vertex_strip.global & 2 == 0)
                    .then(|| -> Result<(BulkArray, BulkArray)> {
                        Ok((
                            BulkArray::read(r, "collection tangents")?,
                            BulkArray::read(r, "collection UVs")?,
                        ))
                    })
                    .transpose()?;
                let color_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
                let color_stride = r.i32()?;
                let color_num_vertices = r.i32()?;
                let colors = (color_strip.global & 2 == 0 && color_num_vertices > 0)
                    .then(|| BulkArray::read(r, "collection vertex colours"))
                    .transpose()?;
                let bone_map_num_vertices = r.i32()?;
                let bone_map = BulkArray::read(r, "collection bone map")?;
                let description_num_vertices = r.i32()?;
                let description_num_triangles = r.i32()?;
                let pre_skinned_bounds =
                    { let mut b = BoxSphereBounds::default(); b.serialize(r)?; b };
                let mut sections: [Vec<GeometryCollectionMeshElement>; 3] =
                    [Vec::new(), Vec::new(), Vec::new()];
                for (slot, what) in
                    sections.iter_mut().zip(["Sections", "SectionsNoInternal", "SubSections"])
                {
                    let n = bounded_count(r.i32()?, what, r.o - 4)?;
                    *slot = read_vec(r, what, n)?;
                }
                mesh = Some(GeometryCollectionMesh {
                    index_buffer,
                    position_stride,
                    position_num_vertices,
                    positions,
                    vertex_strip,
                    num_tex_coords,
                    vertex_num_vertices,
                    use_full_precision_uvs,
                    use_high_precision_tangent_basis,
                    tangents_and_uvs,
                    color_strip,
                    color_stride,
                    color_num_vertices,
                    colors,
                    bone_map_num_vertices,
                    bone_map,
                    description_num_vertices,
                    description_num_triangles,
                    pre_skinned_bounds,
                    sections,
                });
            }
            if has_nanite {
                nanite = Some(NaniteResources::read(r)?);
            }
        }
        Ok(GeometryCollectionTail { is_cooked_or_cooking, collection, cooked, mesh, nanite })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u32(&mut self.is_cooked_or_cooking.to_owned())?;
        self.collection.write(ar)?;
        ar.u32(&mut u32::from(self.cooked))?;
        if !self.cooked {
            if self.mesh.is_some() || self.nanite.is_some() {
                bail!("render data present on an uncooked geometry collection");
            }
            return Ok(());
        }
        ar.u32(&mut u32::from(self.mesh.is_some()))?;
        ar.u32(&mut u32::from(self.nanite.is_some()))?;
        if let Some(m) = &self.mesh {
            m.index_buffer.write(ar)?;
            ar.i32(&mut m.position_stride.to_owned())?;
            ar.i32(&mut m.position_num_vertices.to_owned())?;
            m.positions.write(ar)?;
            m.vertex_strip.clone().serialize(ar)?;
            ar.i32(&mut m.num_tex_coords.to_owned())?;
            ar.i32(&mut m.vertex_num_vertices.to_owned())?;
            ar.u32(&mut m.use_full_precision_uvs.to_owned())?;
            ar.u32(&mut m.use_high_precision_tangent_basis.to_owned())?;
            match (&m.tangents_and_uvs, m.vertex_strip.global & 2 == 0) {
                (Some((t, u)), true) => {
                    t.write(ar)?;
                    u.write(ar)?;
                }
                (None, false) => {}
                _ => bail!("tangent/UV presence disagrees with the vertex strip flags"),
            }
            m.color_strip.clone().serialize(ar)?;
            ar.i32(&mut m.color_stride.to_owned())?;
            ar.i32(&mut m.color_num_vertices.to_owned())?;
            match (&m.colors, m.color_strip.global & 2 == 0 && m.color_num_vertices > 0) {
                (Some(c), true) => c.write(ar)?,
                (None, false) => {}
                _ => bail!("colour presence disagrees with the colour buffer's flags"),
            }
            ar.i32(&mut m.bone_map_num_vertices.to_owned())?;
            m.bone_map.write(ar)?;
            ar.i32(&mut m.description_num_vertices.to_owned())?;
            ar.i32(&mut m.description_num_triangles.to_owned())?;
            m.pre_skinned_bounds.clone().serialize(ar)?;
            for s in &m.sections {
                write_vec(ar, s)?;
            }
        }
        if let Some(n) = &self.nanite {
            n.write(ar)?;
        }
        Ok(())
    }
}

/// One vtable patch table inside a shader map's pointer table.
#[derive(Debug, Clone, PartialEq)]
pub struct VTablePatchTable {
    pub type_name_hash: HashedName,
    /// `VTableOffset` and `Offset` per patch.
    pub patches: Vec<VTablePatch>,
}

/// One name patch table — script names and memory-image names share the shape.
#[derive(Debug, Clone, PartialEq)]
pub struct NamePatchTable {
    pub name: FName,
    pub offsets: Vec<u32>,
}

/// Where a shader map's bytecode lives.
#[derive(Debug, Clone, PartialEq)]
pub enum ShaderCode {
    /// In a shared shader library; only the hash is in the package.
    Shared { hash: [u8; 20] },
    /// Inlined — `FShaderMapResourceCode::Serialize`.
    Inline {
        resource_hash: [u8; 20],
        /// One `FSHAHash` per shader.
        shader_hashes: Vec<ShaHash>,
        /// Each entry is two `FSharedBuffer`s, each a `uint64` length then that
        /// many bytes. The bytecode itself is leaf data.
        resources: Vec<(Vec<u8>, Vec<u8>)>,
    },
}

/// `FMemoryImageResult::LoadFromArchive` plus the pointer and patch tables — a
/// compiled shader map as it sits in a package.
///
/// The frozen memory image and the compiled bytecode stay byte strings; the
/// tables that address and relocate them are values.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderMap {
    pub layout_params: PlatformTypeLayoutParameters,
    pub frozen_image: Vec<u8>,
    /// An `FName`, a layout size and an `FSHAHash` per dependency.
    pub type_dependencies: Vec<MemoryImageTypeDependency>,
    /// `FHashedName` per entry. The two *counts* are written adjacently and only
    /// then the combined run of hashes, so these cannot be read as two
    /// independent count-then-payload arrays — doing so put a vertex-factory
    /// count of 466,651,457 in the middle of the hash data.
    pub shader_types: Vec<HashedName>,
    pub vertex_factory_types: Vec<HashedName>,
    /// Niagara's pointer table adds the data-interface type names.
    pub data_interface_types: Option<Vec<FStr>>,
    pub vtable_patches: Vec<VTablePatchTable>,
    pub script_name_patches: Vec<NamePatchTable>,
    pub image_name_patches: Vec<NamePatchTable>,
    pub shader_platform_name: HashedName,
    pub code: ShaderCode,
}

impl ShaderMap {
    fn read(r: &mut Reader, niagara_pointer_table: bool) -> Result<Self> {
        let layout_params = {
            let mut p = PlatformTypeLayoutParameters::default();
            p.serialize(r)?;
            p
        };
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "frozen memory image", r.o - 4)?
        };
        let frozen_image = r.take(n)?.to_vec();
        let n = bounded_count(r.i32()?, "memory image type dependencies", r.o - 4)?;
        let type_dependencies: Vec<MemoryImageTypeDependency> =
            read_vec(r, "memory image type dependencies", n)?;
        let n_types = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "shader types", r.o - 4)?
        };
        let n_vf = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "vertex factory types", r.o - 4)?
        };
        let mut hashed = |r: &mut Reader, n: usize| -> Result<Vec<HashedName>> {
            (0..n)
                .map(|_| {
                    let mut h = HashedName::default();
                    h.serialize(r)?;
                    Ok(h)
                })
                .collect()
        };
        let shader_types = hashed(r, n_types)?;
        let vertex_factory_types = hashed(r, n_vf)?;
        let data_interface_types = niagara_pointer_table
            .then(|| -> Result<Vec<FStr>> {
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "data interface types", r.o - 4)?
                };
                (0..n).map(|_| r.fstring()).collect()
            })
            .transpose()?;
        // The three patch-table counts are written up front, then the tables
        // follow in order — so all three counts must be read before any table.
        let count = |r: &mut Reader, what: &str| -> Result<usize> {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, what, r.o - 4)
        };
        let n_vtable = count(r, "vtable patch tables")?;
        let n_script = count(r, "script name patch tables")?;
        let n_image = count(r, "memory image name patch tables")?;
        let mut vtable_patches = Vec::with_capacity(n_vtable.min(64));
        for _ in 0..n_vtable {
            vtable_patches.push(VTablePatchTable {
                type_name_hash: { let mut h = HashedName::default(); h.serialize(r)?; h },
                patches: { let n = bounded_count(r.i32()?, "vtable patches", r.o - 4)?; read_vec(r, "vtable patches", n)? },
            });
        }
        let mut name_table = |r: &mut Reader, n: usize| -> Result<Vec<NamePatchTable>> {
            let mut out = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                out.push(NamePatchTable {
                    name: r.fname()?,
                    offsets: {
                        let n = bounded_count(r.i32()?, "name patches", r.o - 4)?;
                        (0..n).map(|_| r.u32()).collect::<Result<_>>()?
                    },
                });
            }
            Ok(out)
        };
        let script_name_patches = name_table(r, n_script)?;
        let image_name_patches = name_table(r, n_image)?;
        let share_code = r.u32()? != 0;
        let shader_platform_name = { let mut h = HashedName::default(); h.serialize(r)?; h };
        let code = if share_code {
            ShaderCode::Shared { hash: r.take(20)?.try_into().expect("20 bytes") }
        } else {
            let resource_hash = r.take(20)?.try_into().expect("20 bytes");
            let n = bounded_count(r.i32()?, "shader hashes", r.o - 4)?;
            let shader_hashes: Vec<ShaHash> = read_vec(r, "shader hashes", n)?;
            let n = {
                let n = r.i32()?;
                super::limits::bounded(n, MAX_NATIVE_COUNT, "shader code resources", r.o - 4)?
            };
            let mut resources = Vec::with_capacity(n.min(64));
            for _ in 0..n {
                let mut buf = || -> Result<Vec<u8>> {
                    let len = usize::try_from(r.u64()?).context("implausible shader buffer")?;
                    Ok(r.take(len)?.to_vec())
                };
                let a = buf()?;
                let b = buf()?;
                resources.push((a, b));
            }
            ShaderCode::Inline { resource_hash, shader_hashes, resources }
        };
        Ok(ShaderMap {
            layout_params,
            frozen_image,
            type_dependencies,
            shader_types,
            vertex_factory_types,
            data_interface_types,
            vtable_patches,
            script_name_patches,
            image_name_patches,
            shader_platform_name,
            code,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.layout_params.clone().serialize(ar)?;
        ar.i32(&mut (self.frozen_image.len() as i32))?;
        let n = self.frozen_image.len();
        ar.raw(&mut self.frozen_image.clone(), n)?;
        write_vec(ar, &self.type_dependencies)?;
        ar.i32(&mut (self.shader_types.len() as i32))?;
        ar.i32(&mut (self.vertex_factory_types.len() as i32))?;
        for h in self.shader_types.iter().chain(&self.vertex_factory_types) {
            h.clone().serialize(ar)?;
        }
        if let Some(v) = &self.data_interface_types {
            ar.i32(&mut (v.len() as i32))?;
            for s in v {
                ar.fstring(&mut s.clone())?;
            }
        }
        ar.i32(&mut (self.vtable_patches.len() as i32))?;
        ar.i32(&mut (self.script_name_patches.len() as i32))?;
        ar.i32(&mut (self.image_name_patches.len() as i32))?;
        for t in &self.vtable_patches {
            t.type_name_hash.clone().serialize(ar)?;
            write_vec(ar, &t.patches)?;
        }
        for t in self.script_name_patches.iter().chain(&self.image_name_patches) {
            ar.fname(&mut t.name.clone())?;
            write_u32_array(ar, &t.offsets)?;
        }
        match &self.code {
            ShaderCode::Shared { hash } => {
                ar.u32(&mut 1)?;
                self.shader_platform_name.clone().serialize(ar)?;
                ar.raw(&mut hash.to_vec(), 20)?;
            }
            ShaderCode::Inline { resource_hash, shader_hashes, resources } => {
                ar.u32(&mut 0)?;
                self.shader_platform_name.clone().serialize(ar)?;
                ar.raw(&mut resource_hash.to_vec(), 20)?;
                write_vec(ar, shader_hashes)?;
                ar.i32(&mut (resources.len() as i32))?;
                for (a, b) in resources {
                    for buf in [a, b] {
                        ar.u64(&mut (buf.len() as u64))?;
                        let n = buf.len();
                        ar.raw(&mut buf.clone(), n)?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// One of a `UNiagaraScript`'s compiled shader resources.
#[derive(Debug, Clone, PartialEq)]
pub struct NiagaraShaderResource {
    pub cooked: bool,
    pub num_permutations: i32,
    pub base_compile_hash: Vec<u8>,
    /// An uncooked resource writes nothing more; a cooked one says whether a map
    /// compiled, and only then is there a map.
    pub shader_map: Option<Option<Box<ShaderMap>>>,
}

/// `UNiagaraScript`'s tail: its compiled shader maps, or nothing at all.
///
/// A script with no shader maps ends at the property block, so the tail is empty
/// — which is not the same as a script whose resource count is zero.
#[derive(Debug, Clone, PartialEq)]
pub struct NiagaraScriptTail {
    pub resources: Vec<NiagaraShaderResource>,
}

impl NiagaraScriptTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "Niagara shader resources", r.o - 4)?
        };
        let mut resources = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let cooked = r.u32()? != 0;
            let num_permutations = r.i32()?;
            let h = {
                let h = r.i32()?;
                super::limits::bounded(h, MAX_NATIVE_COUNT, "BaseCompileHash", r.o - 4)?
            };
            let base_compile_hash = r.take(h)?.to_vec();
            let shader_map = cooked
                .then(|| -> Result<Option<Box<ShaderMap>>> {
                    Ok((r.u32()? != 0).then(|| ShaderMap::read(r, true).map(Box::new)).transpose()?)
                })
                .transpose()?;
            resources.push(NiagaraShaderResource {
                cooked,
                num_permutations,
                base_compile_hash,
                shader_map,
            });
        }
        Ok(NiagaraScriptTail { resources })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut (self.resources.len() as i32))?;
        for res in &self.resources {
            ar.u32(&mut u32::from(res.cooked))?;
            ar.i32(&mut res.num_permutations.to_owned())?;
            ar.i32(&mut (res.base_compile_hash.len() as i32))?;
            let n = res.base_compile_hash.len();
            ar.raw(&mut res.base_compile_hash.clone(), n)?;
            match (&res.shader_map, res.cooked) {
                (Some(m), true) => match m {
                    Some(map) => {
                        ar.u32(&mut 1)?;
                        map.write(ar)?;
                    }
                    None => ar.u32(&mut 0)?,
                },
                (None, false) => {}
                _ => bail!("shader map presence disagrees with the cooked flag"),
            }
        }
        Ok(())
    }
}

/// `ULandscapeComponent`'s tail: the grass weight offsets and the packed
/// height/weight data.
#[derive(Debug, Clone, PartialEq)]
pub struct LandscapeComponentTail {
    pub num_elements: i32,
    /// An `FPackageIndex` and an `int32` per entry.
    pub grass_weight_offsets: Vec<GrassWeightOffset>,
    pub height_weight_data: Vec<u8>,
    pub cooked: u32,
}

/// `ULandscapeHeightfieldCollisionComponent`'s tail: the cooked Chaos
/// heightfield, behind its own present flag.
#[derive(Debug, Clone, PartialEq)]
pub struct LandscapeCollisionTail {
    pub cooked_collision_data: Option<BulkArray>,
}

/// A payload referenced through the package's bulk-data map and inlined here.
///
/// `UVectorFieldStatic`'s volume source is the only user in this corpus, but the
/// shape — an index, and the bytes when the map points at this very offset — is
/// the one every inline bulk payload has.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineBulkPayload {
    pub bulk_index: i32,
    pub payload: Option<Vec<u8>>,
}

impl InlineBulkPayload {
    fn read(r: &mut Reader, ctx: TailContext, what: &str) -> Result<Self> {
        let bulk_index = r.i32()?;
        let Some(&(offset, size)) = ctx.bulk_data.get(bulk_index.max(0) as usize) else {
            bail!("{what}: bulk data index {bulk_index} out of range");
        };
        let payload = (offset as usize == ctx.origin + r.o)
            .then(|| r.take(size.max(0) as usize).map(<[u8]>::to_vec))
            .transpose()?;
        Ok(InlineBulkPayload { bulk_index, payload })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.bulk_index.to_owned())?;
        if let Some(p) = &self.payload {
            let n = p.len();
            ar.raw(&mut p.clone(), n)?;
        }
        Ok(())
    }
}

/// `AActor`'s tail: an optional name string, then the actor and instance GUIDs.
///
/// Modeled on its own because several actor classes add nothing of their own —
/// `AStaticMeshActor` alone is 69,832 exports whose whole tail is this.
#[derive(Debug, Clone, PartialEq)]
pub struct ActorTail {
    pub name: Option<FStr>,
    /// `FActorInstanceGuid`.
    pub actor_guid: Guid,
    pub actor_instance_guid: Guid,
}

impl ActorTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let name = (r.u32()? != 0).then(|| r.fstring()).transpose()?;
        let mut actor_guid = Guid::default();
        actor_guid.serialize(r)?;
        let mut actor_instance_guid = Guid::default();
        actor_instance_guid.serialize(r)?;
        Ok(ActorTail { name, actor_guid, actor_instance_guid })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        match &self.name {
            Some(s) => {
                ar.u32(&mut 1)?;
                ar.fstring(&mut s.clone())?;
            }
            None => ar.u32(&mut 0)?,
        }
        self.actor_guid.clone().serialize(ar)?;
        self.actor_instance_guid.clone().serialize(ar)
    }
}

/// `UAkAudioEvent`'s tail: the localized cooked event data, then its durations
/// and attenuation radius.
#[derive(Debug, Clone)]
pub struct AkAudioEventTail {
    pub cooked_data: PropertyBlock,
    pub maximum_duration: f32,
    pub minimum_duration: f32,
    pub is_infinite: u32,
    pub max_attenuation_radius: f32,
}

impl AkAudioEventTail {
    pub fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        Ok(AkAudioEventTail {
            cooked_data: read_struct(r, "WwiseLocalizedEventCookedData", ctx.usmap, 0)?,
            maximum_duration: r.f32()?,
            minimum_duration: r.f32()?,
            is_infinite: r.u32()?,
            max_attenuation_radius: r.f32()?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, ctx: TailContext) -> Result<()> {
        let flat = flattened_schema("WwiseLocalizedEventCookedData", ctx.usmap)?;
        write_block(ar, &self.cooked_data, &flat, ctx.usmap)?;
        ar.f32(&mut self.maximum_duration.to_owned())?;
        ar.f32(&mut self.minimum_duration.to_owned())?;
        ar.u32(&mut self.is_infinite.to_owned())?;
        ar.f32(&mut self.max_attenuation_radius.to_owned())
    }
}

/// `UModel`'s BSP data.
///
/// `UModel` uses the **float** math variants throughout in UE5, which the stream
/// confirms: its `Vectors`/`Points` element size is 12, not 24. An `FModelVertex`
/// is 56 bytes for the same reason — a double-width reading survived 16,722
/// models because every one of them has an empty vertex buffer, and blew up on
/// the two that do not.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelTail {
    pub global_strip: u8,
    pub class_strip: u8,
    pub bounds: BoxSphereBounds,
    pub vectors: BulkArray,
    pub points: BulkArray,
    pub nodes: BulkArray,
    pub surfs: Vec<BspSurf>,
    pub verts: BulkArray,
    pub num_shared_sides: i32,
    pub root_outside: u32,
    pub linked: u32,
    pub num_unique_vertices: u32,
    /// Absent when both editor data and the class's vertex-buffer flag are
    /// stripped.
    pub vertex_buffer: Option<Vec<ModelVertex>>,
    pub lighting_guid: Guid,
    pub lightmass_settings: Vec<LightmassPrimitiveSettings>,
}

impl ModelTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let global_strip = r.u8()?;
        let class_strip = r.u8()?;
        let bounds = { let mut b = BoxSphereBounds::default(); b.serialize(r)?; b };
        let vectors = BulkArray::read(r, "Vectors")?;
        let points = BulkArray::read(r, "Points")?;
        let nodes = BulkArray::read(r, "Nodes")?;
        let n = bounded_count(r.i32()?, "Surfs", r.o - 4)?;
        let surfs: Vec<BspSurf> = read_vec(r, "Surfs", n)?;
        let verts = BulkArray::read(r, "Verts")?;
        let num_shared_sides = r.i32()?;
        let root_outside = r.u32()?;
        let linked = r.u32()?;
        let num_unique_vertices = r.u32()?;
        let vertex_buffer = (global_strip & 1 == 0 || class_strip & 1 == 0)
            .then(|| -> Result<Vec<ModelVertex>> {
                let n = bounded_count(r.i32()?, "model vertices", r.o - 4)?;
                read_vec(r, "model vertices", n)
            })
            .transpose()?;
        Ok(ModelTail {
            global_strip,
            class_strip,
            bounds,
            vectors,
            points,
            nodes,
            surfs,
            verts,
            num_shared_sides,
            root_outside,
            linked,
            num_unique_vertices,
            vertex_buffer,
            lighting_guid: { let mut g = Guid::default(); g.serialize(r)?; g },
            lightmass_settings: {
                let n = bounded_count(r.i32()?, "LightmassSettings", r.o - 4)?;
                read_vec(r, "LightmassSettings", n)?
            },
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.global_strip.to_owned())?;
        ar.u8(&mut self.class_strip.to_owned())?;
        self.bounds.clone().serialize(ar)?;
        self.vectors.write(ar)?;
        self.points.write(ar)?;
        self.nodes.write(ar)?;
        write_vec(ar, &self.surfs)?;
        self.verts.write(ar)?;
        ar.i32(&mut self.num_shared_sides.to_owned())?;
        ar.u32(&mut self.root_outside.to_owned())?;
        ar.u32(&mut self.linked.to_owned())?;
        ar.u32(&mut self.num_unique_vertices.to_owned())?;
        match (
            &self.vertex_buffer,
            self.global_strip & 1 == 0 || self.class_strip & 1 == 0,
        ) {
            (Some(v), true) => write_vec(ar, v)?,
            (None, false) => {}
            _ => bail!("vertex buffer presence disagrees with the strip flags"),
        }
        self.lighting_guid.clone().serialize(ar)?;
        write_vec(ar, &self.lightmass_settings)
    }
}

/// One bucket of a level's precomputed visibility.
#[derive(Debug, Clone, PartialEq)]
pub struct VisibilityBucket {
    pub cell_data_size: i32,
    pub cells: Vec<PrecomputedVisibilityCell>,
    pub chunks: Vec<VisibilityChunk>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VisibilityChunk {
    pub compressed: u32,
    pub uncompressed_size: i32,
    pub data: Vec<u8>,
}

/// `ULevel`'s tail: the actor list, the level's `FURL`, the model and component
/// references, and the precomputed visibility and distance-field data.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelTail {
    pub actors: Vec<i32>,
    /// Protocol, host, map and portal.
    pub url_strings: [FStr; 4],
    pub url_options: Vec<FStr>,
    pub port: i32,
    pub url_valid: u32,
    pub model: i32,
    pub model_components: Vec<i32>,
    pub level_script_actor: i32,
    pub nav_list_start: i32,
    pub nav_list_end: i32,
    /// `FPrecomputedVisibilityHandler`'s placement grid.
    pub visibility_bucket_origin_xy: Vector2d,
    pub visibility_cell_size_xy: f32,
    pub visibility_cell_size_z: f32,
    pub visibility_cell_bucket_size_xy: i32,
    pub visibility_num_cell_buckets: i32,
    pub visibility_buckets: Vec<VisibilityBucket>,
    pub volume_distance_field_scale: f32,
    pub volume_distance_field_box: Box3d,
    pub volume_size_x: i32,
    pub volume_size_y: i32,
    pub volume_size_z: i32,
    pub volume_distance_field_data: Vec<u32>,
}

impl LevelTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let actors = read_i32_array(r, "Actors")?;
        let url_strings = [r.fstring()?, r.fstring()?, r.fstring()?, r.fstring()?];
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "URL options", r.o - 4)?
        };
        let url_options = (0..n).map(|_| r.fstring()).collect::<Result<Vec<_>>>()?;
        let port = r.i32()?;
        let url_valid = r.u32()?;
        let model = r.i32()?;
        let model_components = read_i32_array(r, "ModelComponents")?;
        let level_script_actor = r.i32()?;
        let nav_list_start = r.i32()?;
        let nav_list_end = r.i32()?;
        let mut visibility_bucket_origin_xy = Vector2d::default();
        visibility_bucket_origin_xy.serialize(r)?;
        let visibility_cell_size_xy = r.f32()?;
        let visibility_cell_size_z = r.f32()?;
        let visibility_cell_bucket_size_xy = r.i32()?;
        let visibility_num_cell_buckets = r.i32()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "visibility buckets", r.o - 4)?
        };
        let mut visibility_buckets = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let cell_data_size = r.i32()?;
            let n = bounded_count(r.i32()?, "visibility cells", r.o - 4)?;
            let cells: Vec<PrecomputedVisibilityCell> = read_vec(r, "visibility cells", n)?;
            let m = {
                let m = r.i32()?;
                super::limits::bounded(m, MAX_NATIVE_COUNT, "visibility chunks", r.o - 4)?
            };
            let mut chunks = Vec::with_capacity(m.min(64));
            for _ in 0..m {
                let compressed = r.u32()?;
                let uncompressed_size = r.i32()?;
                let bytes = {
                    let b = r.i32()?;
                    super::limits::bounded(b, MAX_NATIVE_COUNT, "visibility chunk data", r.o - 4)?
                };
                chunks.push(VisibilityChunk {
                    compressed,
                    uncompressed_size,
                    data: r.take(bytes)?.to_vec(),
                });
            }
            visibility_buckets.push(VisibilityBucket { cell_data_size, cells, chunks });
        }
        Ok(LevelTail {
            actors,
            url_strings,
            url_options,
            port,
            url_valid,
            model,
            model_components,
            level_script_actor,
            nav_list_start,
            nav_list_end,
            visibility_bucket_origin_xy,
            visibility_cell_size_xy,
            visibility_cell_size_z,
            visibility_cell_bucket_size_xy,
            visibility_num_cell_buckets,
            visibility_buckets,
            volume_distance_field_scale: r.f32()?,
            volume_distance_field_box: { let mut b = Box3d::default(); b.serialize(r)?; b },
            volume_size_x: r.i32()?,
            volume_size_y: r.i32()?,
            volume_size_z: r.i32()?,
            volume_distance_field_data: read_u32_array(r, "distance field data")?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        write_i32_array(ar, &self.actors)?;
        for s in &self.url_strings {
            ar.fstring(&mut s.clone())?;
        }
        ar.i32(&mut (self.url_options.len() as i32))?;
        for s in &self.url_options {
            ar.fstring(&mut s.clone())?;
        }
        ar.i32(&mut self.port.to_owned())?;
        ar.u32(&mut self.url_valid.to_owned())?;
        ar.i32(&mut self.model.to_owned())?;
        write_i32_array(ar, &self.model_components)?;
        ar.i32(&mut self.level_script_actor.to_owned())?;
        ar.i32(&mut self.nav_list_start.to_owned())?;
        ar.i32(&mut self.nav_list_end.to_owned())?;
        self.visibility_bucket_origin_xy.clone().serialize(ar)?;
        ar.f32(&mut self.visibility_cell_size_xy.to_owned())?;
        ar.f32(&mut self.visibility_cell_size_z.to_owned())?;
        ar.i32(&mut self.visibility_cell_bucket_size_xy.to_owned())?;
        ar.i32(&mut self.visibility_num_cell_buckets.to_owned())?;
        ar.i32(&mut (self.visibility_buckets.len() as i32))?;
        for b in &self.visibility_buckets {
            ar.i32(&mut b.cell_data_size.to_owned())?;
            write_vec(ar, &b.cells)?;
            ar.i32(&mut (b.chunks.len() as i32))?;
            for c in &b.chunks {
                ar.u32(&mut c.compressed.to_owned())?;
                ar.i32(&mut c.uncompressed_size.to_owned())?;
                ar.i32(&mut (c.data.len() as i32))?;
                let n = c.data.len();
                ar.raw(&mut c.data.clone(), n)?;
            }
        }
        ar.f32(&mut self.volume_distance_field_scale.to_owned())?;
        self.volume_distance_field_box.clone().serialize(ar)?;
        ar.i32(&mut self.volume_size_x.to_owned())?;
        ar.i32(&mut self.volume_size_y.to_owned())?;
        ar.i32(&mut self.volume_size_z.to_owned())?;
        write_u32_array(ar, &self.volume_distance_field_data)
    }
}

/// The bone-compression codec's own trailing data, which differs by codec.
#[derive(Debug, Clone, PartialEq)]
pub enum BoneCodecData {
    /// `FACLCompressedAnimDataBase::SerializeCompressedData` — the base key
    /// count then `bCompressionFailed`. The compressed clip itself lives in
    /// `CompressedByteStream`, not here.
    Acl { compression_failed: u32 },
    /// `FUECompressedAnimData` — four `TEnumAsByte` formats, three
    /// `SerializeView` counts whose payloads are also in `CompressedByteStream`,
    /// and `CompressedScaleOffsets.StripSize`.
    Ue { formats: [u8; 4], view_counts: [i32; 3], strip_size: i32 },
}

/// The whole tail of a `UAnimSequence` export.
///
/// `UAnimationAsset` writes a 16-byte GUID *first*. A model that starts at the
/// compressed-data block reads that GUID's tail as a track count and reports
/// 2,039,646,153 tracks — which is what happened, on all 14,130 exports, before
/// this type existed.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimSequenceChainTail {
    pub animation_asset_guid: Guid,
    pub sequence: AnimSequenceTail,
}

impl AnimSequenceChainTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        Ok(AnimSequenceChainTail {
            animation_asset_guid: { let mut g = Guid::default(); g.serialize(r)?; g },
            sequence: AnimSequenceTail::read(r)?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.animation_asset_guid.clone().serialize(ar)?;
        self.sequence.write(ar)
    }
}

/// `UAnimSequence`'s compressed animation data: 14,130 exports, 172 MiB.
///
/// The ACL-compressed clip is in `compressed_byte_stream`, and it stays a byte
/// string here — it is ACL's own container, and decoding it is work item H.
/// Everything that describes and addresses it is a value.
#[derive(Debug, Clone, PartialEq)]
pub struct AnimSequenceTail {
    pub strip_flags: StripDataFlags,
    /// `bSerializeCompressedData`. When clear the tail ends here.
    pub serialize_compressed_data: bool,
    pub compressed_raw_data_size: i32,
    pub track_to_skeleton_map: Vec<i32>,
    /// `FAnimCompressedCurveIndexedName` serializes **only** its `CurveName`;
    /// the `CurveIndex` the struct declares is written for memory counting only,
    /// so an element is 8 bytes on the wire, not 12.
    pub indexed_curve_names: Vec<FName>,
    /// The declared length of the compressed stream. Kept because a bulk-backed
    /// stream writes the length with no payload behind it, so it cannot be
    /// derived from what follows.
    pub compressed_byte_stream_len: i32,
    pub use_bulk: bool,
    /// Present only when the stream is inline rather than bulk-backed.
    pub compressed_byte_stream: Option<Vec<u8>>,
    pub bone_codec: FStr,
    pub curve_codec: FStr,
    pub compressed_curve_byte_stream: Vec<u8>,
    /// `CompressedNumberOfKeys` from the `ICompressedAnimData` base.
    pub compressed_number_of_keys: i32,
    pub codec_data: BoneCodecData,
    /// `UAnimSequence`'s trailing flag.
    pub trailing_flag: u32,
}

impl AnimSequenceTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let serialize_compressed_data = r.u32()? != 0;
        if !serialize_compressed_data {
            return Ok(AnimSequenceTail {
                strip_flags,
                serialize_compressed_data,
                compressed_raw_data_size: 0,
                track_to_skeleton_map: Vec::new(),
                indexed_curve_names: Vec::new(),
                compressed_byte_stream_len: 0,
                use_bulk: false,
                compressed_byte_stream: None,
                bone_codec: FStr::default(),
                curve_codec: FStr::default(),
                compressed_curve_byte_stream: Vec::new(),
                compressed_number_of_keys: 0,
                codec_data: BoneCodecData::Acl { compression_failed: 0 },
                trailing_flag: 0,
            });
        }
        let compressed_raw_data_size = r.i32()?;
        let track_to_skeleton_map = read_i32_array(r, "CompressedTrackToSkeletonMapTable")?;
        let indexed_curve_names = read_name_array(r, "IndexedCurveNames")?;
        let compressed_byte_stream_len = r.i32()?;
        let n = super::limits::bounded(
            compressed_byte_stream_len,
            MAX_NATIVE_COUNT,
            "CompressedByteStream",
            r.o - 4,
        )?;
        let use_bulk = r.u32()? != 0;
        let compressed_byte_stream =
            (!use_bulk).then(|| r.take(n).map(<[u8]>::to_vec)).transpose()?;
        let bone_codec = r.fstring()?;
        let curve_codec = r.fstring()?;
        let compressed_curve_byte_stream = read_byte_array(r, "CompressedCurveByteStream")?;
        let compressed_number_of_keys = r.i32()?;
        let codec_name = bone_codec.as_str();
        let codec_data = if codec_name.starts_with("AnimBoneCompressionCodec_ACL") {
            BoneCodecData::Acl { compression_failed: r.u32()? }
        } else if codec_name.starts_with("AnimCompress_") {
            BoneCodecData::Ue {
                formats: r.take(4)?.try_into().expect("4 bytes"),
                view_counts: [r.i32()?, r.i32()?, r.i32()?],
                strip_size: r.i32()?,
            }
        } else {
            bail!("unmodeled bone compression codec {codec_name:?}");
        };
        Ok(AnimSequenceTail {
            strip_flags,
            serialize_compressed_data,
            compressed_raw_data_size,
            track_to_skeleton_map,
            indexed_curve_names,
            compressed_byte_stream_len,
            use_bulk,
            compressed_byte_stream,
            bone_codec,
            curve_codec,
            compressed_curve_byte_stream,
            compressed_number_of_keys,
            codec_data,
            trailing_flag: r.u32()?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.strip_flags.clone().serialize(ar)?;
        ar.u32(&mut u32::from(self.serialize_compressed_data))?;
        if !self.serialize_compressed_data {
            return Ok(());
        }
        ar.i32(&mut self.compressed_raw_data_size.to_owned())?;
        write_i32_array(ar, &self.track_to_skeleton_map)?;
        write_name_array(ar, &self.indexed_curve_names)?;
        ar.i32(&mut self.compressed_byte_stream_len.to_owned())?;
        ar.u32(&mut u32::from(self.use_bulk))?;
        match (&self.compressed_byte_stream, self.use_bulk) {
            (Some(s), false) => {
                if s.len() as i32 != self.compressed_byte_stream_len {
                    bail!(
                        "compressed stream is {} bytes but its length field says {}",
                        s.len(),
                        self.compressed_byte_stream_len
                    );
                }
                let n = s.len();
                ar.raw(&mut s.clone(), n)?;
            }
            (None, true) => {}
            _ => bail!("compressed stream presence disagrees with the bulk flag"),
        }
        ar.fstring(&mut self.bone_codec.clone())?;
        ar.fstring(&mut self.curve_codec.clone())?;
        write_byte_array(ar, &self.compressed_curve_byte_stream)?;
        ar.i32(&mut self.compressed_number_of_keys.to_owned())?;
        match &self.codec_data {
            BoneCodecData::Acl { compression_failed } => {
                ar.u32(&mut compression_failed.to_owned())?;
            }
            BoneCodecData::Ue { formats, view_counts, strip_size } => {
                ar.raw(&mut formats.to_vec(), 4)?;
                for v in view_counts {
                    ar.i32(&mut v.to_owned())?;
                }
                ar.i32(&mut strip_size.to_owned())?;
            }
        }
        ar.u32(&mut self.trailing_flag.to_owned())
    }
}

/// `UDNAAsset`'s two RigLogic DNA streams, back to back: the behaviour layers
/// then the geometry.
///
/// DNA is RigLogic's own container format, so the streams stay byte strings and
/// what this model owns is the *split* — which is not trivial, because the first
/// stream can be written without a size and the reader has to find where the
/// second one begins.
#[derive(Debug, Clone, PartialEq)]
pub struct DnaAssetTail {
    pub behavior: Vec<u8>,
    pub geometry: Vec<u8>,
}

impl DnaAssetTail {
    pub fn read(r: &mut Reader) -> Result<Self> {
        use super::tails::{dna_stream_end, dna_unsized_floor};
        let start = r.o;
        // The behaviour stream usually carries its own size. When it does not,
        // its end is wherever a *second* stream begins that closes the export
        // exactly — the same search the walker does.
        let split = match dna_stream_end(r.b, r.o)? {
            Some(end) => end,
            None => {
                let floor = dna_unsized_floor(r.b, r.o)?;
                (floor..r.b.len().saturating_sub(3))
                    .filter(|&i| &r.b[i..i + 3] == b"DNA")
                    .find(|&i| matches!(dna_stream_end(r.b, i), Ok(Some(e)) if e == r.b.len()))
                    .with_context(|| {
                        format!("no second DNA stream closing the export after {floor}")
                    })?
            }
        };
        let end = dna_stream_end(r.b, split)?
            .context("the second DNA stream is itself unsized")?;
        if end != r.b.len() {
            bail!("second DNA stream ends at {end}, not the export end {}", r.b.len());
        }
        let behavior = r.b[start..split].to_vec();
        let geometry = r.b[split..end].to_vec();
        r.o = end;
        Ok(DnaAssetTail { behavior, geometry })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        let n = self.behavior.len();
        ar.raw(&mut self.behavior.clone(), n)?;
        let n = self.geometry.len();
        ar.raw(&mut self.geometry.clone(), n)
    }
}


/// `FReferenceSkeleton` — the rig the renderer skins against.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceSkeleton {
    pub bone_info: Vec<MeshBoneInfo>,
    /// How wide an `FTransform` is in this cook, 80 or 40 bytes.
    ///
    /// It is not written anywhere, so the reader finds it by checking which
    /// width leaves the following bone-count where it belongs. Keeping the
    /// answer is what lets the writer reproduce the pose without probing again.
    pub transform_size: usize,
    pub bone_pose: Vec<u8>,
    pub name_to_index: Vec<NameToIndex>,
}

impl ReferenceSkeleton {
    fn read(r: &mut Reader) -> Result<Self> {
        let n = bounded_count(r.i32()?, "RawRefBoneInfo", r.o - 4)?;
        let bone_info: Vec<MeshBoneInfo> = read_vec(r, "RawRefBoneInfo", n)?;
        let nbones = bone_info.len();
        let npose = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "RawRefBonePose", r.o - 4)?
        };
        let transform_size = if npose == 0 {
            80
        } else {
            [80usize, 40]
                .into_iter()
                .find(|&ts| {
                    r.b.get(r.o + npose * ts..r.o + npose * ts + 4)
                        .and_then(|s| s.try_into().ok())
                        .map(|s| i32::from_le_bytes(s) == nbones as i32)
                        .unwrap_or(false)
                })
                .context("could not size FTransform in FReferenceSkeleton")?
        };
        Ok(ReferenceSkeleton {
            bone_info,
            transform_size,
            bone_pose: r.take(npose * transform_size)?.to_vec(),
            name_to_index: {
                let n = bounded_count(r.i32()?, "RawRefBoneNameToIndexMap", r.o - 4)?;
                read_vec(r, "RawRefBoneNameToIndexMap", n)?
            },
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        write_vec(ar, &self.bone_info)?;
        if self.transform_size == 0 || self.bone_pose.len() % self.transform_size != 0 {
            bail!(
                "bone pose is {} bytes for {}-byte transforms",
                self.bone_pose.len(),
                self.transform_size
            );
        }
        ar.i32(&mut ((self.bone_pose.len() / self.transform_size) as i32))?;
        let n = self.bone_pose.len();
        ar.raw(&mut self.bone_pose.clone(), n)?;
        write_vec(ar, &self.name_to_index)
    }
}

/// One `FSkelMeshRenderSection`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkelRenderSection {
    pub global_strip: u8,
    pub class_strip: u8,
    /// In the order `operator<<` writes them (SkeletalMeshLODRenderData.cpp:169).
    /// `RecomputeTangentsVertexMaskChannel` is an `ESkinVertexColorChannel`, a
    /// single unpadded byte between `bRecomputeTangent` and `bCastShadow` —
    /// which is *not* where it is declared.
    pub material_index: u16,
    pub base_index: u32,
    pub num_triangles: u32,
    pub recompute_tangent: u32,
    pub recompute_tangents_vertex_mask_channel: u8,
    pub cast_shadow: u32,
    pub visible_in_ray_tracing: u32,
    pub base_vertex_index: u32,
    pub cloth_mapping_lods: Vec<Vec<MeshToMeshVertData>>,
    pub bone_map: Vec<u16>,
    pub num_vertices: u32,
    pub max_bone_influences: i32,
    pub correspond_cloth_asset_index: i16,
    pub clothing_section_data: ClothingSectionData,
    /// The duplicated-vertex buffers, stripped from cooks that do not need them.
    pub dup_verts: Option<(Vec<u32>, Vec<DuplicatedVertexIndex>)>,
    pub disabled: u32,
}

impl SkelRenderSection {
    /// Whether this section carries cloth, which decides what the LOD's buffers
    /// contain further down.
    fn has_cloth(&self) -> bool {
        self.cloth_mapping_lods.iter().any(|a| !a.is_empty())
    }

    fn read(r: &mut Reader) -> Result<Self> {
        let global_strip = r.u8()?;
        let class_strip = r.u8()?;
        let material_index = r.u16()?;
        let base_index = r.u32()?;
        let num_triangles = r.u32()?;
        let recompute_tangent = r.u32()?;
        let recompute_tangents_vertex_mask_channel = r.u8()?;
        let cast_shadow = r.u32()?;
        let visible_in_ray_tracing = r.u32()?;
        let base_vertex_index = r.u32()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "ClothMappingDataLODs", r.o - 4)?
        };
        let mut cloth_mapping_lods = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            let m = bounded_count(r.i32()?, "cloth mapping data", r.o - 4)?;
            cloth_mapping_lods.push(read_vec(r, "cloth mapping data", m)?);
        }
        let bone_map = read_u16_array(r, "BoneMap")?;
        let num_vertices = r.u32()?;
        let max_bone_influences = r.i32()?;
        let correspond_cloth_asset_index = r.u16()? as i16;
        let clothing_section_data = {
            let mut c = ClothingSectionData::default();
            c.serialize(r)?;
            c
        };
        let dup_verts = (class_strip & 1 == 0)
            .then(|| -> Result<(Vec<u32>, Vec<DuplicatedVertexIndex>)> {
                let a = read_u32_array(r, "DupVertData")?;
                let n = bounded_count(r.i32()?, "DupVertIndexData", r.o - 4)?;
                Ok((a, read_vec(r, "DupVertIndexData", n)?))
            })
            .transpose()?;
        Ok(SkelRenderSection {
            global_strip,
            class_strip,
            material_index,
            base_index,
            num_triangles,
            recompute_tangent,
            recompute_tangents_vertex_mask_channel,
            cast_shadow,
            visible_in_ray_tracing,
            base_vertex_index,
            cloth_mapping_lods,
            bone_map,
            num_vertices,
            max_bone_influences,
            correspond_cloth_asset_index,
            clothing_section_data,
            dup_verts,
            disabled: r.u32()?,
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.global_strip.to_owned())?;
        ar.u8(&mut self.class_strip.to_owned())?;
        ar.u16(&mut self.material_index.to_owned())?;
        ar.u32(&mut self.base_index.to_owned())?;
        ar.u32(&mut self.num_triangles.to_owned())?;
        ar.u32(&mut self.recompute_tangent.to_owned())?;
        ar.u8(&mut self.recompute_tangents_vertex_mask_channel.to_owned())?;
        ar.u32(&mut self.cast_shadow.to_owned())?;
        ar.u32(&mut self.visible_in_ray_tracing.to_owned())?;
        ar.u32(&mut self.base_vertex_index.to_owned())?;
        ar.i32(&mut (self.cloth_mapping_lods.len() as i32))?;
        for a in &self.cloth_mapping_lods {
            write_vec(ar, a)?;
        }
        write_u16_array(ar, &self.bone_map)?;
        ar.u32(&mut self.num_vertices.to_owned())?;
        ar.i32(&mut self.max_bone_influences.to_owned())?;
        ar.u16(&mut (self.correspond_cloth_asset_index as u16))?;
        self.clothing_section_data.clone().serialize(ar)?;
        match (&self.dup_verts, self.class_strip & 1 == 0) {
            (Some((a, b)), true) => {
                write_u32_array(ar, a)?;
                write_vec(ar, b)?;
            }
            (None, false) => {}
            _ => bail!("duplicated vertex data disagrees with the strip flags"),
        }
        ar.u32(&mut self.disabled.to_owned())
    }
}

/// One skin-weight profile's override data.
#[derive(Debug, Clone, PartialEq)]
pub struct SkinWeightProfile {
    pub name: FName,
    pub bone_ids: Vec<u8>,
    pub bone_weights: Vec<u8>,
    pub num_weights_per_vertex: u8,
    pub vertex_index_to_influence_offset: Vec<EntryToValueKey>,
}

/// One named per-vertex attribute buffer.
#[derive(Debug, Clone, PartialEq)]
pub struct VertexAttributeBuffer {
    pub name: FName,
    pub component_count: i32,
    pub pixel_format: i32,
    pub component_stride: i32,
    pub values: BulkArray,
}

/// Compressed morph-target render data, present only when the cook wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct MorphTargetData {
    pub morph_data: Vec<u32>,
    pub minimum_value_per_morph: Vec<Vector4f>,
    pub maximum_value_per_morph: Vec<Vector4f>,
    pub batch_start_offset_per_morph: Vec<u32>,
    pub batches_per_morph: Vec<u32>,
    pub num_total_batches: u32,
    pub position_precision: i32,
    pub tangent_z_precision: i32,
}

/// `FSkeletalMeshLODRenderData::SerializeStreamedData` — everything a LOD keeps
/// inline.
#[derive(Debug, Clone, PartialEq)]
pub struct SkelStreamedData {
    pub strip_flags: StripDataFlags,
    pub index_data_type_size: u8,
    pub index_buffer: BulkArray,
    pub position_stride: i32,
    pub position_num_vertices: i32,
    pub positions: BulkArray,
    pub vertex_strip: StripDataFlags,
    pub num_tex_coords: i32,
    pub vertex_num_vertices: i32,
    pub use_full_precision_uvs: u32,
    pub use_high_precision_tangent_basis: u32,
    pub tangents: BulkArray,
    pub uvs: BulkArray,
    pub skin_strip: StripDataFlags,
    pub variable_bones_per_vertex: u32,
    pub max_bone_influences: u32,
    pub num_bone_weights: u32,
    pub skin_num_vertices: u32,
    pub use_16_bit_bone_index: u32,
    pub use_16_bit_bone_weight: u32,
    pub skin_weights: BulkArray,
    pub lookup_strip: StripDataFlags,
    pub lookup_num_vertices: u32,
    pub skin_weight_lookup: BulkArray,
    /// Present only when the mesh declares vertex colours; the inner option is
    /// the buffer, which serializes only when it has vertices.
    pub colors: Option<((StripDataFlags, i32, u32), Option<BulkArray>)>,
    pub cloth: Option<(StripDataFlags, BulkArray, Vec<ClothBufferIndexMapping>)>,
    pub skin_weight_profiles: Vec<SkinWeightProfile>,
    /// `FRayTracingGeometry::RawData`.
    pub source_ray_tracing_geometry: Vec<u8>,
    pub morph: Option<MorphTargetData>,
    pub vertex_attributes: Vec<VertexAttributeBuffer>,
    pub half_edge_strip: StripDataFlags,
    pub half_edge: Option<(Vec<i32>, Vec<i32>)>,
}

impl SkelStreamedData {
    fn read(r: &mut Reader, has_vertex_colors: bool, has_cloth: bool) -> Result<Self> {
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let index_data_type_size = r.u8()?;
        let index_buffer = BulkArray::read(r, "index buffer")?;
        let position_stride = r.i32()?;
        let position_num_vertices = r.i32()?;
        let positions = BulkArray::read(r, "positions")?;
        let vertex_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let num_tex_coords = r.i32()?;
        let vertex_num_vertices = r.i32()?;
        let use_full_precision_uvs = r.u32()?;
        let use_high_precision_tangent_basis = r.u32()?;
        let tangents = BulkArray::read(r, "tangents")?;
        let uvs = BulkArray::read(r, "UVs")?;
        let skin_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let variable_bones_per_vertex = r.u32()?;
        let max_bone_influences = r.u32()?;
        let num_bone_weights = r.u32()?;
        let skin_num_vertices = r.u32()?;
        let use_16_bit_bone_index = r.u32()?;
        let use_16_bit_bone_weight = r.u32()?;
        let skin_weights = BulkArray::read(r, "skin weights")?;
        let lookup_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let lookup_num_vertices = r.u32()?;
        let skin_weight_lookup = BulkArray::read(r, "skin weight lookup")?;
        let colors = has_vertex_colors
            .then(|| -> Result<_> {
                let strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
                let stride = r.i32()?;
                let n = r.u32()?;
                let buf = (n > 0).then(|| BulkArray::read(r, "vertex colors")).transpose()?;
                Ok(((strip, stride, n), buf))
            })
            .transpose()?;
        let cloth = has_cloth
            .then(|| -> Result<_> {
                let mut strip = StripDataFlags::default();
                strip.serialize(r)?;
                Ok((
                    strip,
                    BulkArray::read(r, "cloth vertices")?,
                    {
                        let n = bounded_count(r.i32()?, "ClothIndexMapping", r.o - 4)?;
                        read_vec(r, "ClothIndexMapping", n)?
                    },
                ))
            })
            .transpose()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "SkinWeightProfiles", r.o - 4)?
        };
        let mut skin_weight_profiles = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            skin_weight_profiles.push(SkinWeightProfile {
                name: r.fname()?,
                bone_ids: read_byte_array(r, "profile BoneIDs")?,
                bone_weights: read_byte_array(r, "profile BoneWeights")?,
                num_weights_per_vertex: r.u8()?,
                vertex_index_to_influence_offset: {
                    let n =
                        bounded_count(r.i32()?, "profile VertexIndexToInfluenceOffset", r.o - 4)?;
                    read_vec(r, "profile VertexIndexToInfluenceOffset", n)?
                },
            });
        }
        let source_ray_tracing_geometry = read_byte_array(r, "SourceRayTracingGeometry")?;
        let morph = (r.u32()? != 0)
            .then(|| -> Result<MorphTargetData> {
                Ok(MorphTargetData {
                    morph_data: read_u32_array(r, "MorphData")?,
                    minimum_value_per_morph: { let n = bounded_count(r.i32()?, "MinimumValuePerMorph", r.o - 4)?; read_vec(r, "MinimumValuePerMorph", n)? },
                    maximum_value_per_morph: { let n = bounded_count(r.i32()?, "MaximumValuePerMorph", r.o - 4)?; read_vec(r, "MaximumValuePerMorph", n)? },
                    batch_start_offset_per_morph: read_u32_array(r, "BatchStartOffsetPerMorph")?,
                    batches_per_morph: read_u32_array(r, "BatchesPerMorph")?,
                    num_total_batches: r.u32()?,
                    position_precision: r.i32()?,
                    tangent_z_precision: r.i32()?,
                })
            })
            .transpose()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "VertexAttributeBuffers", r.o - 4)?
        };
        let mut vertex_attributes = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            vertex_attributes.push(VertexAttributeBuffer {
                name: r.fname()?,
                component_count: r.i32()?,
                pixel_format: r.i32()?,
                component_stride: r.i32()?,
                values: BulkArray::read(r, "attribute values")?,
            });
        }
        let half_edge_strip = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let half_edge = (half_edge_strip.class & 1 == 0)
            .then(|| -> Result<(Vec<i32>, Vec<i32>)> {
                Ok((
                    read_i32_array(r, "VertexToEdgeData")?,
                    read_i32_array(r, "EdgeToTwinEdgeData")?,
                ))
            })
            .transpose()?;
        Ok(SkelStreamedData {
            strip_flags,
            index_data_type_size,
            index_buffer,
            position_stride,
            position_num_vertices,
            positions,
            vertex_strip,
            num_tex_coords,
            vertex_num_vertices,
            use_full_precision_uvs,
            use_high_precision_tangent_basis,
            tangents,
            uvs,
            skin_strip,
            variable_bones_per_vertex,
            max_bone_influences,
            num_bone_weights,
            skin_num_vertices,
            use_16_bit_bone_index,
            use_16_bit_bone_weight,
            skin_weights,
            lookup_strip,
            lookup_num_vertices,
            skin_weight_lookup,
            colors,
            cloth,
            skin_weight_profiles,
            source_ray_tracing_geometry,
            morph,
            vertex_attributes,
            half_edge_strip,
            half_edge,
        })
    }

    fn write(&self, ar: &mut impl Ar, has_vertex_colors: bool, has_cloth: bool) -> Result<()> {
        self.strip_flags.clone().serialize(ar)?;
        ar.u8(&mut self.index_data_type_size.to_owned())?;
        self.index_buffer.write(ar)?;
        ar.i32(&mut self.position_stride.to_owned())?;
        ar.i32(&mut self.position_num_vertices.to_owned())?;
        self.positions.write(ar)?;
        self.vertex_strip.clone().serialize(ar)?;
        ar.i32(&mut self.num_tex_coords.to_owned())?;
        ar.i32(&mut self.vertex_num_vertices.to_owned())?;
        ar.u32(&mut self.use_full_precision_uvs.to_owned())?;
        ar.u32(&mut self.use_high_precision_tangent_basis.to_owned())?;
        self.tangents.write(ar)?;
        self.uvs.write(ar)?;
        self.skin_strip.clone().serialize(ar)?;
        ar.u32(&mut self.variable_bones_per_vertex.to_owned())?;
        ar.u32(&mut self.max_bone_influences.to_owned())?;
        ar.u32(&mut self.num_bone_weights.to_owned())?;
        ar.u32(&mut self.skin_num_vertices.to_owned())?;
        ar.u32(&mut self.use_16_bit_bone_index.to_owned())?;
        ar.u32(&mut self.use_16_bit_bone_weight.to_owned())?;
        self.skin_weights.write(ar)?;
        self.lookup_strip.clone().serialize(ar)?;
        ar.u32(&mut self.lookup_num_vertices.to_owned())?;
        self.skin_weight_lookup.write(ar)?;
        match (&self.colors, has_vertex_colors) {
            (Some(((strip, stride, n), buf)), true) => {
                strip.clone().serialize(ar)?;
                ar.i32(&mut stride.to_owned())?;
                ar.u32(&mut n.to_owned())?;
                match (buf, *n > 0) {
                    (Some(b), true) => b.write(ar)?,
                    (None, false) => {}
                    _ => bail!("colour buffer presence disagrees with its vertex count"),
                }
            }
            (None, false) => {}
            _ => bail!("vertex colour presence disagrees with the property block"),
        }
        match (&self.cloth, has_cloth) {
            (Some((strip, verts, mapping)), true) => {
                strip.clone().serialize(ar)?;
                verts.write(ar)?;
                write_vec(ar, mapping)?;
            }
            (None, false) => {}
            _ => bail!("cloth presence disagrees with the render sections"),
        }
        ar.i32(&mut (self.skin_weight_profiles.len() as i32))?;
        for p in &self.skin_weight_profiles {
            ar.fname(&mut p.name.clone())?;
            write_byte_array(ar, &p.bone_ids)?;
            write_byte_array(ar, &p.bone_weights)?;
            ar.u8(&mut p.num_weights_per_vertex.to_owned())?;
            write_vec(ar, &p.vertex_index_to_influence_offset)?;
        }
        write_byte_array(ar, &self.source_ray_tracing_geometry)?;
        match &self.morph {
            Some(m) => {
                ar.u32(&mut 1)?;
                write_u32_array(ar, &m.morph_data)?;
                write_vec(ar, &m.minimum_value_per_morph)?;
                write_vec(ar, &m.maximum_value_per_morph)?;
                write_u32_array(ar, &m.batch_start_offset_per_morph)?;
                write_u32_array(ar, &m.batches_per_morph)?;
                ar.u32(&mut m.num_total_batches.to_owned())?;
                ar.i32(&mut m.position_precision.to_owned())?;
                ar.i32(&mut m.tangent_z_precision.to_owned())?;
            }
            None => ar.u32(&mut 0)?,
        }
        ar.i32(&mut (self.vertex_attributes.len() as i32))?;
        for a in &self.vertex_attributes {
            ar.fname(&mut a.name.clone())?;
            ar.i32(&mut a.component_count.to_owned())?;
            ar.i32(&mut a.pixel_format.to_owned())?;
            ar.i32(&mut a.component_stride.to_owned())?;
            a.values.write(ar)?;
        }
        self.half_edge_strip.clone().serialize(ar)?;
        match (&self.half_edge, self.half_edge_strip.class & 1 == 0) {
            (Some((a, b)), true) => {
                write_i32_array(ar, a)?;
                write_i32_array(ar, b)?;
            }
            (None, false) => {}
            _ => bail!("half-edge data disagrees with the strip flags"),
        }
        Ok(())
    }
}

/// The metadata a streamed-out LOD leaves behind in the export.
#[derive(Debug, Clone, PartialEq)]
pub struct SkelAvailabilityInfo {
    /// The metadata a streamed-out LOD leaves behind, in the order
    /// `SerializeAvailabilityInfo` writes it. Note the static-mesh vertex-buffer
    /// counts come *before* the position buffer's here, the opposite of
    /// `SerializeStreamedData`.
    pub index_data_type_size: u8,
    pub num_indices: i32,
    pub num_tex_coords: i32,
    pub vertex_num_vertices: i32,
    pub use_full_precision_uvs: u32,
    pub use_high_precision_tangent_basis: u32,
    pub position_stride: i32,
    pub position_num_vertices: i32,
    pub color_stride: i32,
    pub color_num_vertices: u32,
    pub variable_bones_per_vertex: u32,
    pub max_bone_influences: u32,
    pub num_bone_weights: u32,
    pub skin_num_vertices: u32,
    pub use_16_bit_bone_index: u32,
    pub use_16_bit_bone_weight: u32,
    pub lookup_num_vertices: u32,
    pub cloth: Option<(Vec<ClothBufferIndexMapping>, i32, u32)>,
    pub skin_weight_profile_names: Vec<FName>,
}

impl SkelAvailabilityInfo {
    fn read(r: &mut Reader, has_cloth: bool) -> Result<Self> {
        let index_data_type_size = r.u8()?;
        let num_indices = r.i32()?;
        let num_tex_coords = r.i32()?;
        let vertex_num_vertices = r.i32()?;
        let use_full_precision_uvs = r.u32()?;
        let use_high_precision_tangent_basis = r.u32()?;
        let position_stride = r.i32()?;
        let position_num_vertices = r.i32()?;
        let color_stride = r.i32()?;
        let color_num_vertices = r.u32()?;
        let variable_bones_per_vertex = r.u32()?;
        let max_bone_influences = r.u32()?;
        let num_bone_weights = r.u32()?;
        let skin_num_vertices = r.u32()?;
        let use_16_bit_bone_index = r.u32()?;
        let use_16_bit_bone_weight = r.u32()?;
        let lookup_num_vertices = r.u32()?;
        let cloth = has_cloth
            .then(|| -> Result<_> {
                let n = bounded_count(r.i32()?, "ClothIndexMapping", r.o - 4)?;
                Ok((read_vec(r, "ClothIndexMapping", n)?, r.i32()?, r.u32()?))
            })
            .transpose()?;
        Ok(SkelAvailabilityInfo {
            index_data_type_size,
            num_indices,
            num_tex_coords,
            vertex_num_vertices,
            use_full_precision_uvs,
            use_high_precision_tangent_basis,
            position_stride,
            position_num_vertices,
            color_stride,
            color_num_vertices,
            variable_bones_per_vertex,
            max_bone_influences,
            num_bone_weights,
            skin_num_vertices,
            use_16_bit_bone_index,
            use_16_bit_bone_weight,
            lookup_num_vertices,
            cloth,
            skin_weight_profile_names: read_name_array(r, "SkinWeightProfileNames")?,
        })
    }

    fn write(&self, ar: &mut impl Ar, has_cloth: bool) -> Result<()> {
        ar.u8(&mut self.index_data_type_size.to_owned())?;
        ar.i32(&mut self.num_indices.to_owned())?;
        ar.i32(&mut self.num_tex_coords.to_owned())?;
        ar.i32(&mut self.vertex_num_vertices.to_owned())?;
        ar.u32(&mut self.use_full_precision_uvs.to_owned())?;
        ar.u32(&mut self.use_high_precision_tangent_basis.to_owned())?;
        ar.i32(&mut self.position_stride.to_owned())?;
        ar.i32(&mut self.position_num_vertices.to_owned())?;
        ar.i32(&mut self.color_stride.to_owned())?;
        ar.u32(&mut self.color_num_vertices.to_owned())?;
        ar.u32(&mut self.variable_bones_per_vertex.to_owned())?;
        ar.u32(&mut self.max_bone_influences.to_owned())?;
        ar.u32(&mut self.num_bone_weights.to_owned())?;
        ar.u32(&mut self.skin_num_vertices.to_owned())?;
        ar.u32(&mut self.use_16_bit_bone_index.to_owned())?;
        ar.u32(&mut self.use_16_bit_bone_weight.to_owned())?;
        ar.u32(&mut self.lookup_num_vertices.to_owned())?;
        match (&self.cloth, has_cloth) {
            (Some((m, stride, n)), true) => {
                write_vec(ar, m)?;
                ar.i32(&mut stride.to_owned())?;
                ar.u32(&mut n.to_owned())?;
            }
            (None, false) => {}
            _ => bail!("cloth mapping disagrees with the render sections"),
        }
        write_name_array(ar, &self.skin_weight_profile_names)
    }
}

/// One `FSkeletalMeshLODRenderData`.
#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalMeshLod {
    pub global_strip: u8,
    pub class_strip: u8,
    pub is_lod_cooked_out: bool,
    pub is_inlined: bool,
    pub required_bones: Vec<u16>,
    /// Absent for a server cook or a LOD below the minimum — the LOD ends at
    /// `RequiredBones`.
    pub render: Option<SkeletalMeshLodRender>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalMeshLodRender {
    pub sections: Vec<SkelRenderSection>,
    pub active_bone_indices: Vec<u16>,
    pub buffers_size: u32,
    pub buffers: SkeletalMeshLodBuffers,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkeletalMeshLodBuffers {
    Inline(Box<SkelStreamedData>),
    /// Streamed to `.ubulk`. A zero-size payload means the LOD was discarded
    /// outright and not even the metadata follows.
    Streamed { bulk_index: i32, availability: Option<SkelAvailabilityInfo> },
}

impl SkeletalMeshLod {
    fn read(r: &mut Reader, ctx: TailContext, has_vertex_colors: bool) -> Result<Self> {
        let global_strip = r.u8()?;
        let class_strip = r.u8()?;
        let is_lod_cooked_out = r.u32()? != 0;
        let is_inlined = r.u32()? != 0;
        let required_bones = read_u16_array(r, "RequiredBones")?;
        // `EStrippedData::AudioVisual` is bit 1 — bit 0 is `EditorOnly`, which
        // every client cook sets and which must NOT suppress the buffers.
        if global_strip & 2 != 0 || is_lod_cooked_out {
            return Ok(SkeletalMeshLod {
                global_strip,
                class_strip,
                is_lod_cooked_out,
                is_inlined,
                required_bones,
                render: None,
            });
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "RenderSections", r.o - 4)?
        };
        let mut sections = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            sections.push(SkelRenderSection::read(r)?);
        }
        let has_cloth = sections.iter().any(SkelRenderSection::has_cloth);
        let active_bone_indices = read_u16_array(r, "ActiveBoneIndices")?;
        let buffers_size = r.u32()?;
        let buffers = if is_inlined {
            SkeletalMeshLodBuffers::Inline(Box::new(SkelStreamedData::read(
                r,
                has_vertex_colors,
                has_cloth,
            )?))
        } else {
            let bulk_index = r.i32()?;
            let size =
                ctx.bulk_data.get(bulk_index.max(0) as usize).map(|&(_, s)| s).unwrap_or(0);
            SkeletalMeshLodBuffers::Streamed {
                bulk_index,
                availability: (size != 0)
                    .then(|| SkelAvailabilityInfo::read(r, has_cloth))
                    .transpose()?,
            }
        };
        Ok(SkeletalMeshLod {
            global_strip,
            class_strip,
            is_lod_cooked_out,
            is_inlined,
            required_bones,
            render: Some(SkeletalMeshLodRender {
                sections,
                active_bone_indices,
                buffers_size,
                buffers,
            }),
        })
    }

    fn write(&self, ar: &mut impl Ar, has_vertex_colors: bool) -> Result<()> {
        ar.u8(&mut self.global_strip.to_owned())?;
        ar.u8(&mut self.class_strip.to_owned())?;
        ar.u32(&mut u32::from(self.is_lod_cooked_out))?;
        ar.u32(&mut u32::from(self.is_inlined))?;
        write_u16_array(ar, &self.required_bones)?;
        let expected = self.global_strip & 2 == 0 && !self.is_lod_cooked_out;
        let rd = match (&self.render, expected) {
            (Some(rd), true) => rd,
            (None, false) => return Ok(()),
            _ => bail!("LOD render data presence disagrees with its flags"),
        };
        ar.i32(&mut (rd.sections.len() as i32))?;
        for s in &rd.sections {
            s.write(ar)?;
        }
        let has_cloth = rd.sections.iter().any(SkelRenderSection::has_cloth);
        write_u16_array(ar, &rd.active_bone_indices)?;
        ar.u32(&mut rd.buffers_size.to_owned())?;
        match (&rd.buffers, self.is_inlined) {
            (SkeletalMeshLodBuffers::Inline(d), true) => d.write(ar, has_vertex_colors, has_cloth),
            (SkeletalMeshLodBuffers::Streamed { bulk_index, availability }, false) => {
                ar.i32(&mut bulk_index.to_owned())?;
                match availability {
                    Some(a) => a.write(ar, has_cloth),
                    None => Ok(()),
                }
            }
            _ => bail!("buffer form disagrees with the inlined flag"),
        }
    }
}

/// The whole tail of a `USkeletalMesh` export: 415 exports, 470 MiB.
#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalMeshTail {
    pub strip_flags: StripDataFlags,
    /// `ImportedBounds`, an `FBoxSphereBounds` at LWC precision.
    pub imported_bounds: BoxSphereBounds,
    pub materials: Vec<SkeletalMeshMaterial>,
    pub reference_skeleton: ReferenceSkeleton,
    pub cooked: u32,
    /// The render data, written only when cooked.
    pub render: Option<SkeletalMeshRenderData>,
    pub dummy_objs: Vec<i32>,
    /// `BodySetup`, written only when the mesh enables per-poly collision — a
    /// condition that lives in the property block.
    pub body_setup: Option<i32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalMeshMaterial {
    pub material_interface: i32,
    pub slot_name: FName,
    /// The imported slot name only survives a cook that keeps editor data.
    pub imported_slot_name: Option<FName>,
    pub uv_channel_info: MeshUvChannelInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkeletalMeshRenderData {
    pub lods: Vec<SkeletalMeshLod>,
    pub nanite: NaniteResources,
    pub num_inlined_lods: u8,
    pub num_non_optional_lods: u8,
}

impl SkeletalMeshTail {
    pub fn read(r: &mut Reader, block: &PropertyBlock, ctx: TailContext) -> Result<Self> {
        let flag = |name: &str| matches!(block.get(name), Some(PropValue::Bool(true)));
        let strip_flags = { let mut f = StripDataFlags::default(); f.serialize(r)?; f };
        let imported_bounds = { let mut b = BoxSphereBounds::default(); b.serialize(r)?; b };
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "Materials", r.o - 4)?
        };
        let mut materials = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            let material_interface = r.i32()?;
            let slot_name = r.fname()?;
            let imported_slot_name = (r.u32()? != 0).then(|| r.fname()).transpose()?;
            materials.push(SkeletalMeshMaterial {
                material_interface,
                slot_name,
                imported_slot_name,
                uv_channel_info: {
                    let mut u = MeshUvChannelInfo::default();
                    u.serialize(r)?;
                    u
                },
            });
        }
        let reference_skeleton = ReferenceSkeleton::read(r)?;
        let cooked = r.u32()?;
        let render = (cooked != 0)
            .then(|| -> Result<SkeletalMeshRenderData> {
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "LODRenderData", r.o - 4)?
                };
                let mut lods = Vec::with_capacity(n.min(16));
                for _ in 0..n {
                    lods.push(SkeletalMeshLod::read(r, ctx, flag("bHasVertexColors"))?);
                }
                Ok(SkeletalMeshRenderData {
                    lods,
                    nanite: NaniteResources::read(r)?,
                    num_inlined_lods: r.u8()?,
                    num_non_optional_lods: r.u8()?,
                })
            })
            .transpose()?;
        let dummy_objs = read_i32_array(r, "legacy DummyObjs")?;
        let body_setup = flag("bEnablePerPolyCollision").then(|| r.i32()).transpose()?;
        Ok(SkeletalMeshTail {
            strip_flags,
            imported_bounds,
            materials,
            reference_skeleton,
            cooked,
            render,
            dummy_objs,
            body_setup,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock) -> Result<()> {
        let flag = |name: &str| matches!(block.get(name), Some(PropValue::Bool(true)));
        self.strip_flags.clone().serialize(ar)?;
        self.imported_bounds.clone().serialize(ar)?;
        ar.i32(&mut (self.materials.len() as i32))?;
        for m in &self.materials {
            ar.i32(&mut m.material_interface.to_owned())?;
            ar.fname(&mut m.slot_name.clone())?;
            match &m.imported_slot_name {
                Some(n) => {
                    ar.u32(&mut 1)?;
                    ar.fname(&mut n.clone())?;
                }
                None => ar.u32(&mut 0)?,
            }
            m.uv_channel_info.clone().serialize(ar)?;
        }
        self.reference_skeleton.write(ar)?;
        ar.u32(&mut self.cooked.to_owned())?;
        match (&self.render, self.cooked != 0) {
            (Some(rd), true) => {
                ar.i32(&mut (rd.lods.len() as i32))?;
                for l in &rd.lods {
                    l.write(ar, flag("bHasVertexColors"))?;
                }
                rd.nanite.write(ar)?;
                ar.u8(&mut rd.num_inlined_lods.to_owned())?;
                ar.u8(&mut rd.num_non_optional_lods.to_owned())?;
            }
            (None, false) => {}
            _ => bail!("render data presence disagrees with the cooked flag"),
        }
        write_i32_array(ar, &self.dummy_objs)?;
        match (self.body_setup, flag("bEnablePerPolyCollision")) {
            (Some(v), true) => ar.i32(&mut v.to_owned())?,
            (None, false) => {}
            _ => bail!("body setup presence disagrees with the property block"),
        }
        Ok(())
    }
}

/// One entry of an `FFormatContainer`: a format name and the payload cooked for
/// it.
///
/// `payload` is Chaos's serialized physics geometry. It stays a byte string in
/// this layer for the same reason a texture mip does — it is a separate
/// serializer's output, and decoding it is its own work item rather than
/// something the tail model can do on the way past. What the model does supply
/// is the addressing: which format, which bulk-data index.
#[derive(Debug, Clone, PartialEq)]
pub struct CookedFormat {
    pub format: FName,
    pub bulk_index: i32,
    pub payload: Vec<u8>,
}

/// `UBodySetup::Serialize`: the setup GUID, then — when cooked — the per-format
/// cooked physics data.
///
/// 17,754 exports and 1,051 MiB, the second-largest tail population in the
/// corpus.
#[derive(Debug, Clone, PartialEq)]
pub struct BodySetupTail {
    pub guid: Guid,
    pub cooked: bool,
    /// Written only when cooked, so `None` and `Some(false)` are different
    /// files, not the same one described two ways.
    pub has_cooked_data: Option<bool>,
    pub formats: Vec<CookedFormat>,
}

impl BodySetupTail {
    pub fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let guid = { let mut g = Guid::default(); g.serialize(r)?; g };
        let cooked = r.u32()? != 0;
        if !cooked {
            return Ok(BodySetupTail { guid, cooked, has_cooked_data: None, formats: Vec::new() });
        }
        let has_cooked_data = Some(r.u32()? != 0);
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "CookedFormatData", r.o - 4)?
        };
        let mut formats = Vec::with_capacity(n.min(16));
        for _ in 0..n {
            let format = r.fname()?;
            let bulk_index = r.i32()?;
            let Some(&(offset, size)) = ctx.bulk_data.get(bulk_index.max(0) as usize) else {
                bail!("body setup: bulk data index {bulk_index} out of range");
            };
            if offset as usize != ctx.origin + r.o {
                bail!("body setup payload at {offset} is not inline at {}", ctx.origin + r.o);
            }
            formats.push(CookedFormat {
                format,
                bulk_index,
                payload: r.take(size.max(0) as usize)?.to_vec(),
            });
        }
        Ok(BodySetupTail { guid, cooked, has_cooked_data, formats })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.guid.clone().serialize(ar)?;
        ar.u32(&mut u32::from(self.cooked))?;
        match (self.has_cooked_data, self.cooked) {
            (Some(v), true) => ar.u32(&mut u32::from(v))?,
            (None, false) => return Ok(()),
            _ => bail!("cooked-data flag disagrees with the cooked flag"),
        }
        ar.i32(&mut (self.formats.len() as i32))?;
        for f in &self.formats {
            ar.fname(&mut f.format.clone())?;
            ar.i32(&mut f.bulk_index.to_owned())?;
            let n = f.payload.len();
            ar.raw(&mut f.payload.clone(), n)?;
        }
        Ok(())
    }
}

/// One `FMaterialResourceLocOnDisk`: where a resource starts and which
/// feature/quality level it was compiled for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaterialResourceLoc {
    pub offset: u32,
    pub feature_level: u8,
    pub quality_level: u8,
}

/// The inline shader maps a material writes after its cached data
/// (`FMaterialResourceProxyReader`).
///
/// `data` is the compiled shader-map payload. It stays a byte string for the
/// same reason a texture mip does: compiled shader bytecode is the leaf datum,
/// not an encoding of some richer value this codec could recover and re-emit.
/// What *is* modeled is everything that addresses it — the name table and the
/// per-resource locations, which is what a tool needs to find a resource.
#[derive(Debug, Clone, PartialEq)]
pub struct InlineShaderMaps {
    /// The resource count that leads the block. Zero or negative ends it
    /// immediately, and nothing below is written at all.
    pub resources: i32,
    /// Per shader map: its name, then the non-case-preserving and
    /// case-preserving hashes of it.
    pub names: Vec<(FStr, u16, u16)>,
    pub locs: Vec<MaterialResourceLoc>,
    pub data: Vec<u8>,
}

impl InlineShaderMaps {
    fn read(r: &mut Reader) -> Result<Self> {
        let resources = r.i32()?;
        if resources <= 0 {
            return Ok(InlineShaderMaps {
                resources,
                names: Vec::new(),
                locs: Vec::new(),
                data: Vec::new(),
            });
        }
        if resources > 1024 {
            bail!("implausible inline shader map resource count {resources}");
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "shader map names", r.o - 4)?
        };
        let mut names = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            names.push((r.fstring()?, r.u16()?, r.u16()?));
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "material resource locs", r.o - 4)?
        };
        let mut locs = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            locs.push(MaterialResourceLoc {
                offset: r.u32()?,
                feature_level: r.u8()?,
                quality_level: r.u8()?,
            });
        }
        let num_bytes = r.u32()? as usize;
        Ok(InlineShaderMaps { resources, names, locs, data: r.take(num_bytes)?.to_vec() })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.i32(&mut self.resources.to_owned())?;
        if self.resources <= 0 {
            return Ok(());
        }
        ar.i32(&mut (self.names.len() as i32))?;
        for (s, a, b) in &self.names {
            ar.fstring(&mut s.clone())?;
            ar.u16(&mut a.to_owned())?;
            ar.u16(&mut b.to_owned())?;
        }
        ar.i32(&mut (self.locs.len() as i32))?;
        for l in &self.locs {
            ar.u32(&mut l.offset.to_owned())?;
            ar.u8(&mut l.feature_level.to_owned())?;
            ar.u8(&mut l.quality_level.to_owned())?;
        }
        ar.u32(&mut (self.data.len() as u32))?;
        let n = self.data.len();
        ar.raw(&mut self.data.clone(), n)
    }
}

/// Read a reflected struct that a class appends to its tail, behind the
/// four-byte "was it saved" flag that precedes it.
fn read_flagged_struct(r: &mut Reader, name: &str, usmap: &Usmap) -> Result<Option<PropertyBlock>> {
    (r.u32()? != 0).then(|| read_struct(r, name, usmap, 0)).transpose()
}

fn write_flagged_struct(
    ar: &mut impl Ar,
    block: &Option<PropertyBlock>,
    name: &str,
    usmap: &Usmap,
) -> Result<()> {
    match block {
        Some(b) => {
            ar.u32(&mut 1)?;
            let flat = flattened_schema(name, usmap)?;
            write_block(ar, b, &flat, usmap)
        }
        None => ar.u32(&mut 0),
    }
}

/// The whole tail of a material export.
///
/// `UMaterialInterface` writes its cached expression data, then the concrete
/// class adds its own: `UMaterial` always writes inline shader maps, while a
/// `UMaterialInstance` writes its own cache and only writes shader maps when it
/// has a static permutation resource — a condition that lives in the *property
/// block*, not in the tail.
// No `PartialEq`: a `PropertyBlock` is compared with `semantic_eq`, not `==`.
#[derive(Debug, Clone)]
pub struct MaterialChainTail {
    pub cached_expression_data: Option<PropertyBlock>,
    /// Present for a `UMaterialInstance`, absent for a `UMaterial`.
    pub instance_cached_data: Option<Option<PropertyBlock>>,
    pub shader_maps: Option<InlineShaderMaps>,
    /// A `UMaterial` writes one more resource count after its shader maps, and
    /// a `UMaterialInstance` does not.
    ///
    /// `SerializeInlineShaderMaps` emits a bare `int32 NumResourcesToSave = 0`
    /// on the non-editor saving path (Material.cpp:825), which is what these
    /// four bytes are. Stored rather than assumed zero, so a material that ever
    /// carried a second resource set still round-trips.
    pub trailing_resource_count: Option<i32>,
}

/// Whether an instance writes inline shader maps, which only the property block
/// knows. Mirrors the condition in [`super::tails::read_class_native_tail`].
fn has_static_permutation(block: &PropertyBlock) -> bool {
    matches!(block.get("bHasStaticPermutationResource"), Some(PropValue::Bool(true)))
}

impl MaterialChainTail {
    pub fn read(
        r: &mut Reader,
        block: &PropertyBlock,
        ctx: TailContext,
        is_instance: bool,
    ) -> Result<Self> {
        let cached_expression_data =
            read_flagged_struct(r, "MaterialCachedExpressionData", ctx.usmap)?;
        if !is_instance {
            let shader_maps = Some(InlineShaderMaps::read(r)?);
            return Ok(MaterialChainTail {
                cached_expression_data,
                instance_cached_data: None,
                shader_maps,
                trailing_resource_count: Some(r.i32()?),
            });
        }
        let instance_cached_data =
            Some(read_flagged_struct(r, "MaterialInstanceCachedData", ctx.usmap)?);
        let shader_maps =
            has_static_permutation(block).then(|| InlineShaderMaps::read(r)).transpose()?;
        Ok(MaterialChainTail {
            cached_expression_data,
            instance_cached_data,
            shader_maps,
            trailing_resource_count: None,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock, ctx: TailContext) -> Result<()> {
        write_flagged_struct(
            ar,
            &self.cached_expression_data,
            "MaterialCachedExpressionData",
            ctx.usmap,
        )?;
        match &self.instance_cached_data {
            Some(b) => {
                write_flagged_struct(ar, b, "MaterialInstanceCachedData", ctx.usmap)?;
                // The property block decides whether shader maps exist at all.
                match (&self.shader_maps, has_static_permutation(block)) {
                    (Some(m), true) => m.write(ar)?,
                    (None, false) => {}
                    _ => bail!("shader map presence disagrees with the property block"),
                }
            }
            None => match (&self.shader_maps, self.trailing_resource_count) {
                (Some(m), Some(n)) => {
                    m.write(ar)?;
                    ar.i32(&mut n.to_owned())?;
                }
                _ => bail!("a UMaterial always writes inline shader maps and a resource count"),
            },
        }
        Ok(())
    }
}

/// The whole tail of a cooked texture export: `UTexture`'s strip flags, then the
/// concrete class's cooked platform data.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureChainTail {
    /// `UTexture::Serialize` writes only its strip flags in a cooked stream.
    pub texture_strip_flags: StripDataFlags,
    pub cooked: TextureCookedData,
}

impl TextureChainTail {
    pub fn read(r: &mut Reader, ctx: TailContext, has_mip_data_flag: bool) -> Result<Self> {
        Ok(TextureChainTail {
            texture_strip_flags: { let mut f = StripDataFlags::default(); f.serialize(r)?; f },
            cooked: TextureCookedData::read(r, ctx, has_mip_data_flag)?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        self.texture_strip_flags.clone().serialize(ar)?;
        self.cooked.write(ar)
    }
}

/// The classes whose whole tail chain this module models, for the gate to
/// enumerate.
pub const MODELED_TAILS: &[&str] = &[
    "StaticMeshComponent",
    "InstancedStaticMeshComponent",
    "FoliageInstancedStaticMeshComponent",
    "HLODInstancedStaticMeshComponent",
    "HierarchicalInstancedStaticMeshComponent",
    "Texture2D",
    "TextureCube",
    "VolumeTexture",
    "Texture2DArray",
    "TextureLightProfile",
    "Material",
    "MaterialInstanceConstant",
    "LandscapeMaterialInstanceConstant",
    "MaterialInstanceDynamic",
    "BodySetup",
    "StaticMesh",
    "SkeletalMesh",
    "AnimSequence",
    "DNAAsset",
    "StaticMeshActor",
    "AkAudioEvent",
    "Model",
    "Level",
];

/// Decode a modeled tail and re-emit it, for verification against the span it
/// would replace. `None` when the class has no model yet.
///
/// Takes the property block because the tail's *shape* depends on it.
pub fn roundtrip_tail(
    class: &str,
    tail: &[u8],
    names: &[String],
    block: &PropertyBlock,
    ctx: TailContext,
) -> Option<Result<Vec<u8>>> {
    match class {
        "StaticMeshComponent" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = StaticMeshComponentChainTail::read(&mut r, block)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w, block)?;
            Ok(w.into_bytes())
        })()),
        "InstancedStaticMeshComponent"
        | "FoliageInstancedStaticMeshComponent"
        | "HLODInstancedStaticMeshComponent"
        | "HierarchicalInstancedStaticMeshComponent" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled =
                InstancedStaticMeshComponentChainTail::read(&mut r, block, is_hierarchical(class))?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w, block)?;
            Ok(w.into_bytes())
        })()),
        // `UTexture2D` alone writes a `bSerializeMipData` flag; the other cooked
        // texture shapes call the shared serializer directly. `UTextureLightProfile`
        // *derives* from `UTexture2D`, so it writes the flag too — treating it as
        // a sibling of `UTextureCube` desynced all 7 of them.
        "Texture2D" | "TextureCube" | "VolumeTexture" | "Texture2DArray"
        | "TextureLightProfile" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let derives_from_texture_2d = matches!(class, "Texture2D" | "TextureLightProfile");
            let modeled = TextureChainTail::read(&mut r, ctx, derives_from_texture_2d)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        // `UMaterial` always writes inline shader maps; a `UMaterialInstance`
        // writes its own cache first and defers to the property block.
        "Material" | "MaterialInstanceConstant" | "LandscapeMaterialInstanceConstant"
        | "MaterialInstanceDynamic" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let is_instance = class != "Material";
            let modeled = MaterialChainTail::read(&mut r, block, ctx, is_instance)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w, block, ctx)?;
            Ok(w.into_bytes())
        })()),
        // `AStaticMeshActor` adds nothing of its own; its whole tail is `AActor`'s.
        "StaticMeshActor" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = ActorTail::read(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "AkAudioEvent" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = AkAudioEventTail::read(&mut r, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w, ctx)?;
            Ok(w.into_bytes())
        })()),
        "Model" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = ModelTail::read(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "Level" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = LevelTail::read(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "AnimSequence" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = AnimSequenceChainTail::read(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "DNAAsset" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = DnaAssetTail::read(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "SkeletalMesh" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = SkeletalMeshTail::read(&mut r, block, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w, block)?;
            Ok(w.into_bytes())
        })()),
        "StaticMesh" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = StaticMeshTail::read(&mut r, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        // `URigVM` and `URigHierarchy` override `Serialize` and deliberately do
        // *not* call up, so their export is entirely their own format with no
        // property block ahead of it. Both have a reader and neither has a
        // writer, so the run stays whole — but it is now *checked*: the reader
        // must land exactly on the end.
        "RigVM" => Some((|| {
            let mut r = reader(tail, names, ctx);
            super::tails::read_rigvm(&mut r, ctx.usmap)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            Ok(tail.to_vec())
        })()),
        "RigHierarchy" => Some((|| {
            let mut r = reader(tail, names, ctx);
            super::tails::read_rig_hierarchy(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            Ok(tail.to_vec())
        })()),
        "BodySetup" => Some((|| {
            let mut r = reader(tail, names, ctx);
            let modeled = BodySetupTail::read(&mut r, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::with_resolver(ctx.resolver);
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        // Anything else: decide by *chain* rather than by name. A class that
        // appends nothing of its own has exactly its ancestors' tail, and most
        // of the long tail is that — 49 actor subclasses and 42 scene-component
        // subclasses in this corpus, 242,647 exports between them, none of which
        // needs a model naming it.
        class => match owner_key(class, ctx.usmap).as_str() {
            // A landscape component's own data sits after the scene-component
            // layers, so the chain has to be read whole.
            "LandscapeComponent+SceneComponent+ActorComponent" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = SceneComponentChainTail::read(&mut r, block)?;
                let num_elements = r.i32()?;
                let n = bounded_count(r.i32()?, "grass weight offsets", r.o - 4)?;
                let grass_weight_offsets: Vec<GrassWeightOffset> =
                    read_vec(&mut r, "grass weight offsets", n)?;
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "HeightWeightData", r.o - 4)?
                };
                let own = LandscapeComponentTail {
                    num_elements,
                    grass_weight_offsets,
                    height_weight_data: r.take(n)?.to_vec(),
                    cooked: r.u32()?,
                };
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w, block)?;
                w.i32(&mut own.num_elements.to_owned())?;
                write_vec(&mut w, &own.grass_weight_offsets)?;
                w.i32(&mut (own.height_weight_data.len() as i32))?;
                let n = own.height_weight_data.len();
                w.raw(&mut own.height_weight_data.clone(), n)?;
                w.u32(&mut own.cooked.to_owned())?;
                Ok(w.into_bytes())
            })()),
            "LandscapeHeightfieldCollisionComponent+SceneComponent+ActorComponent" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = SceneComponentChainTail::read(&mut r, block)?;
                let own = LandscapeCollisionTail {
                    cooked_collision_data: (r.u32()? != 0)
                        .then(|| BulkArray::read(&mut r, "CookedCollisionData"))
                        .transpose()?,
                };
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w, block)?;
                match &own.cooked_collision_data {
                    Some(d) => {
                        w.u32(&mut 1)?;
                        d.write(&mut w)?;
                    }
                    None => w.u32(&mut 0)?,
                }
                Ok(w.into_bytes())
            })()),
            // `UPCGMetadata`: attributes whose value width is decided by an
            // `EPCGMetadataTypes` id, with strings and soft paths carrying their
            // own lengths. Note a soft path here goes through
            // `FSoftObjectPath::Serialize` — two `FName`s and an `FString` — not
            // the three-`FName` form the unversioned property reader uses.
            "PCGMetadata" => Some((|| {
                use super::tails::{pcg_array_element_size, pcg_value_size};
                let mut r = reader(tail, names, ctx);
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "PCG attributes", r.o - 4)?
                };
                let mut attrs = Vec::with_capacity(n.min(1024));
                for _ in 0..n {
                    let name = r.fname()?;
                    let type_id = r.i32()?;
                    let n = bounded_count(r.i32()?, "EntryToValueKeyMap", r.o - 4)?;
                    let entries: Vec<EntryToValueKey> = read_vec(&mut r, "EntryToValueKeyMap", n)?;
                    let parent = r.i32()?;
                    let name2 = r.fname()?;
                    let attribute_id = r.i32()?;
                    let count = {
                        let c = r.i32()?;
                        super::limits::bounded(c, MAX_NATIVE_COUNT, "PCG values", r.o - 4)?
                    };
                    // `Values` then a single `DefaultValue`, both of that type.
                    let values: Vec<Vec<u8>> = match pcg_value_size(type_id) {
                        Some(size) => {
                            let elem = pcg_array_element_size(type_id).unwrap_or(size);
                            let mut v = Vec::with_capacity(count.min(4096) + 1);
                            for _ in 0..count {
                                v.push(r.take(elem)?.to_vec());
                            }
                            v.push(r.take(size)?.to_vec());
                            v
                        }
                        None if type_id == 9 => {
                            let mut v = Vec::with_capacity(count.min(4096) + 1);
                            for _ in 0..=count {
                                let at = r.o;
                                r.fstring()?;
                                v.push(r.b[at..r.o].to_vec());
                            }
                            v
                        }
                        None if type_id == 13 || type_id == 14 => {
                            let mut v = Vec::with_capacity(count.min(4096) + 1);
                            for _ in 0..=count {
                                let at = r.o;
                                r.take(16)?;
                                r.fstring()?;
                                v.push(r.b[at..r.o].to_vec());
                            }
                            v
                        }
                        None => bail!("unmodeled EPCGMetadataTypes id {type_id} @ {}", r.o),
                    };
                    attrs.push((name, type_id, entries, parent, name2, attribute_id, count, values));
                }
                // `ParentKeys` closes the metadata — an `int64` per entry.
                // `FPCGMetadataEntryKey` is an `int64`.
                let n = bounded_count(r.i32()?, "ParentKeys", r.o - 4)?;
                let parent_keys: Vec<i64> =
                    (0..n).map(|_| r.u64().map(|v| v as i64)).collect::<Result<_>>()?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.i32(&mut (attrs.len() as i32))?;
                for (name, type_id, entries, parent, name2, attribute_id, count, values) in &attrs {
                    w.fname(&mut name.clone())?;
                    w.i32(&mut type_id.to_owned())?;
                    write_vec(&mut w, entries)?;
                    w.i32(&mut parent.to_owned())?;
                    w.fname(&mut name2.clone())?;
                    w.i32(&mut attribute_id.to_owned())?;
                    w.i32(&mut (*count as i32))?;
                    for v in values {
                        let n = v.len();
                        w.raw(&mut v.clone(), n)?;
                    }
                }
                w.i32(&mut (parent_keys.len() as i32))?;
                for k in &parent_keys {
                    w.u64(&mut (*k as u64))?;
                }
                Ok(w.into_bytes())
            })()),
            // A `UDataTable`'s rows are instances of a row struct named by a
            // *property*, and that struct usually lives in another package —
            // hence the resolver. Both directions need it.
            "DataTable" => Some((|| {
                let Some(resolver) = ctx.resolver else {
                    bail!("a data table's row struct needs a package resolver")
                };
                let Some(PropValue::Object(row_ref)) = block.get("RowStruct") else {
                    bail!("data table has no RowStruct property")
                };
                let row_struct = resolver
                    .struct_name(*row_ref)
                    .with_context(|| format!("cannot resolve RowStruct {row_ref}"))?;
                let mut r = Reader::with_ctx(
                    tail,
                    names,
                    &super::archive::ExportContext { bulk_data: &[], resolver: Some(resolver) },
                );
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "DataTable rows", r.o - 4)?
                };
                let mut rows = Vec::with_capacity(n.min(4096));
                for i in 0..n {
                    let key = r.fname()?;
                    let row = read_struct(&mut r, &row_struct, ctx.usmap, 0)
                        .with_context(|| format!("row {i}"))?;
                    rows.push((key, row));
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.i32(&mut (rows.len() as i32))?;
                for (key, row) in &rows {
                    w.fname(&mut key.clone())?;
                    super::property::write_value(
                        &mut w,
                        &super::usmap::PropertyType::Struct(row_struct.clone()),
                        &PropValue::Struct(row.clone()),
                        false,
                        ctx.usmap,
                    )?;
                }
                Ok(w.into_bytes())
            })()),
            "StringTable" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let namespace = r.fstring()?;
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "StringTable entries", r.o - 4)?
                };
                let mut entries = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    entries.push((r.fstring()?, r.fstring()?));
                }
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "meta-data keys", r.o - 4)?
                };
                let mut meta = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    let key = r.fstring()?;
                    let m = {
                        let m = r.i32()?;
                        super::limits::bounded(m, MAX_NATIVE_COUNT, "meta-data entries", r.o - 4)?
                    };
                    let mut vals = Vec::with_capacity(m.min(64));
                    for _ in 0..m {
                        vals.push((r.fname()?, r.fstring()?));
                    }
                    meta.push((key, vals));
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.fstring(&mut namespace.clone())?;
                w.i32(&mut (entries.len() as i32))?;
                for (k, v) in &entries {
                    w.fstring(&mut k.clone())?;
                    w.fstring(&mut v.clone())?;
                }
                w.i32(&mut (meta.len() as i32))?;
                for (k, vals) in &meta {
                    w.fstring(&mut k.clone())?;
                    w.i32(&mut (vals.len() as i32))?;
                    for (id, v) in vals {
                        w.fname(&mut id.clone())?;
                        w.fstring(&mut v.clone())?;
                    }
                }
                Ok(w.into_bytes())
            })()),
            // A `UUserDefinedStruct` writes its default values against the very
            // field chain `UStruct` just read, so the two cannot be separated.
            // A class-default object writes none at all, which shows up as the
            // tail simply ending.
            "UserDefinedStruct+ScriptStruct+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let script_struct_flag = r.u32()?;
                let fields = r.struct_fields.clone();
                let defaults = if r.o == tail.len() {
                    None
                } else {
                    let Some(fields) = fields else {
                        bail!("no field chain for a user-defined struct's defaults")
                    };
                    let schema: Vec<(&super::usmap::UsmapProperty, u8, &str)> = fields
                        .iter()
                        .flat_map(|f| {
                            (0..f.array_dim.max(1)).map(move |i| (f, i, "UserDefinedStruct"))
                        })
                        .collect();
                    Some((
                        super::block::read_struct_with_schema(
                            &mut r,
                            "UserDefinedStruct default",
                            &schema,
                            ctx.usmap,
                            0,
                        )?,
                        schema.iter().map(|(p, i, o)| ((*p).clone(), *i, o.to_string())).collect::<Vec<_>>(),
                    ))
                };
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                w.u32(&mut script_struct_flag.to_owned())?;
                if let Some((blk, schema)) = &defaults {
                    let flat: Vec<(&super::usmap::UsmapProperty, u8, &str)> =
                        schema.iter().map(|(p, i, o)| (p, *i, o.as_str())).collect();
                    write_block(&mut w, blk, &flat, ctx.usmap)?;
                }
                Ok(w.into_bytes())
            })()),
            "MorphTarget" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let m = MorphTargetTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                m.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "SoundWave" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let m = SoundWaveTail::read(&mut r, ctx)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                m.write(&mut w, ctx)?;
                Ok(w.into_bytes())
            })()),
            "ModelComponent+SceneComponent+ActorComponent" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = SceneComponentChainTail::read(&mut r, block)?;
                let m = ModelComponentTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w, block)?;
                m.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "Skeleton" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let m = SkeletonTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                m.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            // `ARecastNavMesh` writes a version then a self-sized blob whose
            // interior is Recast's own tile format.
            "RecastNavMesh+Actor" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let actor = ActorTail::read(&mut r)?;
                let version = r.u32()?;
                // The size is measured from its *own* offset, so it includes the
                // four bytes it occupies — the payload is `size - 4`.
                let size = r.u32()? as usize;
                let data = r.take(size.checked_sub(4).context("Recast blob size under 4")?)?
                    .to_vec();
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                actor.write(&mut w)?;
                w.u32(&mut version.to_owned())?;
                w.u32(&mut ((data.len() + 4) as u32))?;
                let n = data.len();
                w.raw(&mut data.clone(), n)?;
                Ok(w.into_bytes())
            })()),
            "PCGLandscapeCache" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "PCG cache entries", r.o - 4)?
                };
                let mut entries = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    let key: Vec<u8> = r.take(24)?.to_vec(); // FGuid + FIntPoint
                    let half_size: Vec<u8> = r.take(24)?.to_vec(); // FVector
                    let stride = r.i32()?;
                    let n = bounded_count(r.i32()?, "LayerDataNames", r.o - 4)?;
                    let layer_names: Vec<FName> =
                        (0..n).map(|_| r.fname()).collect::<Result<_>>()?;
                    let bulk = InlineBulkPayload::read(&mut r, ctx, "landscape cache entry")?;
                    entries.push((key, half_size, stride, layer_names, bulk));
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.i32(&mut (entries.len() as i32))?;
                for (key, half, stride, layers, bulk) in &entries {
                    w.raw(&mut key.clone(), 24)?;
                    w.raw(&mut half.clone(), 24)?;
                    w.i32(&mut stride.to_owned())?;
                    w.i32(&mut (layers.len() as i32))?;
                    for n in layers {
                        w.fname(&mut n.clone())?;
                    }
                    bulk.write(&mut w)?;
                }
                Ok(w.into_bytes())
            })()),
            "InstancedFoliageActor+Actor" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let actor = ActorTail::read(&mut r)?;
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "FoliageInfos", r.o - 4)?
                };
                let mut infos = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    let ty = r.i32()?;
                    let impl_type = r.u8()?;
                    let component = match impl_type {
                        0 => None,
                        1 => Some(r.i32()?),
                        other => bail!("unmodeled EFoliageImplType {other}"),
                    };
                    infos.push((ty, impl_type, component));
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                actor.write(&mut w)?;
                w.i32(&mut (infos.len() as i32))?;
                for (ty, impl_type, component) in &infos {
                    w.i32(&mut ty.to_owned())?;
                    w.u8(&mut impl_type.to_owned())?;
                    if let Some(c) = component {
                        w.i32(&mut c.to_owned())?;
                    }
                }
                Ok(w.into_bytes())
            })()),
            "ComputeGraph" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(n, MAX_NATIVE_COUNT, "compute kernels", r.o - 4)?
                };
                let mut kernels = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    let m = {
                        let m = r.i32()?;
                        super::limits::bounded(m, MAX_NATIVE_COUNT, "kernel resources", r.o - 4)?
                    };
                    let mut res = Vec::with_capacity(m.min(64));
                    for _ in 0..m {
                        let cooked = r.u32()? != 0;
                        let map = cooked
                            .then(|| -> Result<Option<Box<ShaderMap>>> {
                                Ok((r.u32()? != 0)
                                    .then(|| ShaderMap::read(&mut r, false).map(Box::new))
                                    .transpose()?)
                            })
                            .transpose()?;
                        res.push((cooked, map));
                    }
                    kernels.push(res);
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.i32(&mut (kernels.len() as i32))?;
                for res in &kernels {
                    w.i32(&mut (res.len() as i32))?;
                    for (cooked, map) in res {
                        w.u32(&mut u32::from(*cooked))?;
                        if *cooked {
                            match map {
                                Some(Some(m)) => {
                                    w.u32(&mut 1)?;
                                    m.write(&mut w)?;
                                }
                                _ => w.u32(&mut 0)?,
                            }
                        }
                    }
                }
                Ok(w.into_bytes())
            })()),
            // `UDynamicMesh`'s interior is its own recursive attribute-set
            // format with no writer here, so the whole run stays a span.
            "DynamicMesh" => Some((|| {
                let mut r = reader(tail, names, ctx);
                super::tails::read_dynamic_mesh(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                Ok(tail.to_vec())
            })()),
            "Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = StructTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "Function+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let own = FunctionTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                own.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "ScriptStruct+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let flag = r.u32()?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                w.u32(&mut flag.to_owned())?;
                Ok(w.into_bytes())
            })()),
            "Class+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let own = ClassTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                own.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "BlueprintGeneratedClass+Class+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let cls = ClassTail::read(&mut r)?;
                let bp = BlueprintGeneratedClassTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                cls.write(&mut w)?;
                bp.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            // `UControlRigBlueprintGeneratedClass` appends a whole `URigVM`.
            // That has its own reader and no writer, so it stays a span; the
            // `PublicFunctions` count after it is a value.
            "ControlRigBlueprintGeneratedClass+BlueprintGeneratedClass+Class+Struct" => {
                Some((|| {
                    let mut r = reader(tail, names, ctx);
                    let base = StructTail::read(&mut r)?;
                    let cls = ClassTail::read(&mut r)?;
                    let bp = BlueprintGeneratedClassTail::read(&mut r)?;
                    let at = r.o;
                    super::tails::read_rigvm(&mut r, ctx.usmap)?;
                    let rigvm = r.b[at..r.o].to_vec();
                    let public_functions = r.i32()?;
                    if r.o != tail.len() {
                        bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                    }
                    let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                    base.write(&mut w)?;
                    cls.write(&mut w)?;
                    bp.write(&mut w)?;
                    let n = rigvm.len();
                    w.raw(&mut rigvm.clone(), n)?;
                    w.i32(&mut public_functions.to_owned())?;
                    Ok(w.into_bytes())
                })())
            }
            // `URigVMMemoryStorageGeneratorClass` adds its property-path
            // descriptions and the memory type.
            "RigVMMemoryStorageGeneratorClass+Class+Struct" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let base = StructTail::read(&mut r)?;
                let cls = ClassTail::read(&mut r)?;
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(
                        n,
                        MAX_NATIVE_COUNT,
                        "PropertyPathDescriptions",
                        r.o - 4,
                    )?
                };
                let mut paths = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    paths.push((r.i32()?, r.fstring()?, r.fstring()?));
                }
                let memory_type = r.u8()?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                base.write(&mut w)?;
                cls.write(&mut w)?;
                w.i32(&mut (paths.len() as i32))?;
                for (idx, head, seg) in &paths {
                    w.i32(&mut idx.to_owned())?;
                    w.fstring(&mut head.clone())?;
                    w.fstring(&mut seg.clone())?;
                }
                w.u8(&mut memory_type.to_owned())?;
                Ok(w.into_bytes())
            })()),
            // Subclasses of `UStaticMeshComponent` that add nothing of their own.
            "StaticMeshComponent+SceneComponent+ActorComponent" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = StaticMeshComponentChainTail::read(&mut r, block)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w, block)?;
                Ok(w.into_bytes())
            })()),
            "GeometryCollection" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = GeometryCollectionTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "NiagaraScript" => Some((|| {
                // An empty tail is a script with no shader maps at all, not a
                // resource count of zero.
                if tail.is_empty() {
                    return Ok(Vec::new());
                }
                let mut r = reader(tail, names, ctx);
                let modeled = NiagaraScriptTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "NiagaraSystem" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let n = {
                    let n = r.i32()?;
                    super::limits::bounded(
                        n,
                        MAX_NATIVE_COUNT,
                        "NiagaraEmitterCompiledData",
                        r.o - 4,
                    )?
                };
                let mut compiled = Vec::with_capacity(n.min(64));
                for _ in 0..n {
                    compiled.push(read_struct(&mut r, "NiagaraEmitterCompiledData", ctx.usmap, 0)?);
                }
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.i32(&mut (compiled.len() as i32))?;
                let flat = flattened_schema("NiagaraEmitterCompiledData", ctx.usmap)?;
                for b in &compiled {
                    write_block(&mut w, b, &flat, ctx.usmap)?;
                }
                Ok(w.into_bytes())
            })()),
            "VectorFieldStatic" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled =
                    InlineBulkPayload::read(&mut r, ctx, "VectorFieldStatic SourceData")?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            // `UWorldPartitionRuntimeCellData` writes its debug name.
            "WorldPartitionRuntimeCellData" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let mut name = r.fstring()?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                w.fstring(&mut name)?;
                Ok(w.into_bytes())
            })()),
            // `USkeletalBodySetup` adds nothing over `UBodySetup`.
            // `URigVM` and `URigHierarchy` override `Serialize` and deliberately do
        // *not* call up, so their export is entirely their own format with no
        // property block ahead of it. Both have a reader and neither has a
        // writer, so the run stays whole — but it is now *checked*: the reader
        // must land exactly on the end.
        "RigVM" => Some((|| {
            let mut r = reader(tail, names, ctx);
            super::tails::read_rigvm(&mut r, ctx.usmap)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            Ok(tail.to_vec())
        })()),
        "RigHierarchy" => Some((|| {
            let mut r = reader(tail, names, ctx);
            super::tails::read_rig_hierarchy(&mut r)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            Ok(tail.to_vec())
        })()),
        "BodySetup" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = BodySetupTail::read(&mut r, ctx)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "Actor" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = ActorTail::read(&mut r)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            "SceneComponent+ActorComponent" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = SceneComponentChainTail::read(&mut r, block)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w, block)?;
                Ok(w.into_bytes())
            })()),
            "Texture2D+Texture" => Some((|| {
                let mut r = reader(tail, names, ctx);
                let modeled = TextureChainTail::read(&mut r, ctx, true)?;
                if r.o != tail.len() {
                    bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                }
                let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                modeled.write(&mut w)?;
                Ok(w.into_bytes())
            })()),
            // Families declared as a piece sequence rather than a bespoke type.
            key => COMPOSED_TAILS.iter().find(|(k, _)| *k == key).map(|(_, pieces)| {
                (|| {
                    let mut r = reader(tail, names, ctx);
                    let values = read_pieces(&mut r, pieces, block, ctx)?;
                    if r.o != tail.len() {
                        bail!("model consumed {} of {} tail bytes", r.o, tail.len());
                    }
                    let mut w = super::archive::Writer::with_resolver(ctx.resolver);
                    write_pieces(&mut w, &values, pieces, block, ctx)?;
                    Ok(w.into_bytes())
                })()
            }),
        },
    }
}

/// Read a `TArray<u16>` written with a bare count — bone indices and node
/// indices, which UE types as `uint16` rather than as an opaque pair of bytes.
fn read_u16_array(r: &mut Reader, what: &str) -> Result<Vec<u16>> {
    let n = bounded_count(r.i32()?, what, r.o - 4)?;
    (0..n).map(|_| r.u16()).collect()
}

fn write_u16_array(ar: &mut impl Ar, v: &[u16]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for x in v {
        ar.u16(&mut x.to_owned())?;
    }
    Ok(())
}

/// Read a `TArray<i32>` written with a bare count. Used for object-reference
/// arrays too — an `FPackageIndex` is an `int32`.
fn read_i32_array(r: &mut Reader, what: &str) -> Result<Vec<i32>> {
    let n = bounded_count(r.i32()?, what, r.o - 4)?;
    (0..n).map(|_| r.i32()).collect()
}

fn write_i32_array(ar: &mut impl Ar, v: &[i32]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for x in v {
        ar.i32(&mut x.to_owned())?;
    }
    Ok(())
}

/// Write a count then a run of engine structs — the bulk form, where the
/// element size has already been written by the caller.
fn write_run_counted<T: super::ue_struct::UeStruct, A: Ar>(ar: &mut A, v: &[T]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for t in v {
        t.clone().ser(ar)?;
    }
    Ok(())
}

/// Read a `TArray<uint8>` written with a bare count — a genuine byte array in
/// the engine too, not a run this model declined to interpret.
fn read_byte_array(r: &mut Reader, what: &str) -> Result<Vec<u8>> {
    let n = bounded_count(r.i32()?, what, r.o - 4)?;
    Ok(r.take(n)?.to_vec())
}

fn write_byte_array(ar: &mut impl Ar, v: &[u8]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    let n = v.len();
    ar.raw(&mut v.to_vec(), n)
}

/// Read a `TArray<FName>` written with a bare count.
fn read_name_array(r: &mut Reader, what: &str) -> Result<Vec<FName>> {
    let n = bounded_count(r.i32()?, what, r.o - 4)?;
    (0..n).map(|_| r.fname()).collect()
}

fn write_name_array(ar: &mut impl Ar, v: &[FName]) -> Result<()> {
    ar.i32(&mut (v.len() as i32))?;
    for x in v {
        ar.fname(&mut x.clone())?;
    }
    Ok(())
}

/// A reader over a tail that carries the package context.
///
/// Every tail model wants this: a nested `UUserDefinedStruct` property needs the
/// resolver to find its layout, and a bulk payload needs the map. Building a
/// bare `Reader` silently drops both, which reads as "unmodeled" when it is
/// really "unasked".
fn reader<'a>(tail: &'a [u8], names: &'a [String], ctx: TailContext<'a>) -> Reader<'a> {
    Reader::with_ctx(
        tail,
        names,
        &super::archive::ExportContext { bulk_data: ctx.bulk_data, resolver: ctx.resolver },
    )
}

/// The ancestors of `class` — itself included — that append a tail of their own,
/// most-derived first.
///
/// This is what says whether a class needs a model naming it or is simply its
/// base classes' tail under a new name. It reads the `.usmap` super chain and
/// keeps the entries [`CLASSES_WITH_OWN_TAIL`](super::tails::CLASSES_WITH_OWN_TAIL)
/// lists, so it stays correct as arms are added.
pub fn tail_owners(class: &str, usmap: &Usmap) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = class.to_string();
    for _ in 0..64 {
        if super::tails::CLASSES_WITH_OWN_TAIL.contains(&cur.as_str()) {
            out.push(cur.clone());
        }
        match usmap.get(&cur).and_then(|s| s.super_name.clone()) {
            Some(s) => cur = s,
            None => break,
        }
    }
    out
}

/// [`tail_owners`] joined with `+`, so a whole family can be matched as one key.
fn owner_key(class: &str, usmap: &Usmap) -> String {
    tail_owners(class, usmap).join("+")
}
