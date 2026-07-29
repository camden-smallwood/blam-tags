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
//! `ce_tail_census` says where the bytes are. Two orderings fall out of it and
//! which matters depends on the goal: `StaticMesh` and `BodySetup` are 48% of
//! the 4.77 GiB, while `StaticMeshComponent` and `StaticMeshActor` are 196,000
//! exports whose tails have medians of 16 and 79 bytes. This starts with the
//! latter, because a cheap conversion covering a sixth of all exports proves the
//! pattern before anything expensive depends on it.
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
//! Worse for a writer: some of it is conditional on *property values*.
//! `USceneComponent` writes its baked bounds only when `bComputeBoundsOnceForGame`
//! is set, so the tail cannot be written without the property block — the two
//! are not independent, and any tail model has to take the block as an input.

use anyhow::{bail, Result};

use super::archive::{Ar, Reader};
use super::common::read_bulk_array;
use super::limits::MAX_NATIVE_COUNT;
use super::value::{PropValue, PropertyBlock};

/// A `TArray` written with `BulkSerialize`: element size, count, then
/// `count × size` blittable bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorComponentTail {
    pub ucs_modified_properties: Vec<u8>,
}

/// `USceneComponent`'s tail: baked bounds, written only when the component asked
/// for them to be computed once for game.
///
/// `None` means the property flag was clear, so *nothing* is written — not even
/// the four-byte present flag. That distinction is the whole difficulty: the
/// bytes that exist depend on a value in the property block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneComponentTail {
    pub bounds: Option<Option<[u8; 56]>>,
}

/// The whole tail of a `UStaticMeshComponent` export, chain and all.
#[derive(Debug, Clone, PartialEq, Eq)]
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

impl StaticMeshComponentChainTail {
    pub fn read(r: &mut Reader, block: &PropertyBlock) -> Result<Self> {
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "UCSModifiedProperties", r.o - 4)?
        };
        let ucs_modified_properties = r.take(n * 28)?.to_vec();

        let bounds = if scene_component_writes_bounds(block) {
            Some(if r.u32()? != 0 {
                Some(r.take(56)?.try_into().expect("56 bytes"))
            } else {
                None
            })
        } else {
            None
        };

        Ok(StaticMeshComponentChainTail {
            actor_component: ActorComponentTail { ucs_modified_properties },
            scene_component: SceneComponentTail { bounds },
            static_mesh_component: StaticMeshComponentTail::read(r)?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar, block: &PropertyBlock) -> Result<()> {
        let ucs = &self.actor_component.ucs_modified_properties;
        if ucs.len() % 28 != 0 {
            bail!("UCS modified properties is {} bytes, not a multiple of 28", ucs.len());
        }
        ar.i32(&mut ((ucs.len() / 28) as i32))?;
        let n = ucs.len();
        ar.raw(&mut ucs.clone(), n)?;

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

/// The classes whose whole tail chain this module models, for the gate to
/// enumerate.
pub const MODELED_TAILS: &[&str] = &[
    "StaticMeshComponent",
    "InstancedStaticMeshComponent",
    "FoliageInstancedStaticMeshComponent",
    "HLODInstancedStaticMeshComponent",
    "HierarchicalInstancedStaticMeshComponent",
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
) -> Option<Result<Vec<u8>>> {
    match class {
        "StaticMeshComponent" => Some((|| {
            let mut r = Reader::new(tail, names);
            let modeled = StaticMeshComponentChainTail::read(&mut r, block)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w, block)?;
            Ok(w.into_bytes())
        })()),
        "InstancedStaticMeshComponent"
        | "FoliageInstancedStaticMeshComponent"
        | "HLODInstancedStaticMeshComponent"
        | "HierarchicalInstancedStaticMeshComponent" => Some((|| {
            let mut r = Reader::new(tail, names);
            let modeled =
                InstancedStaticMeshComponentChainTail::read(&mut r, block, is_hierarchical(class))?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w, block)?;
            Ok(w.into_bytes())
        })()),
        _ => None,
    }
}
