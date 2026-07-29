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
//! Worse for a writer: some of it is conditional on *property values*.
//! `USceneComponent` writes its baked bounds only when `bComputeBoundsOnceForGame`
//! is set, so the tail cannot be written without the property block — the two
//! are not independent, and any tail model has to take the block as an input.

use anyhow::{bail, Context, Result};

use super::archive::{Ar, Reader};
use super::block::{flattened_schema, read_struct, write_block};
use super::common::read_bulk_array;
use super::limits::MAX_NATIVE_COUNT;
use super::usmap::Usmap;
use super::value::{FName, FStr, PropValue, PropertyBlock};

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
}

/// A texture's CPU-side copy (`FSharedImage`, an `FImage`; ImageCore.h:412).
///
/// `RawData` is a `TArray64<uint8>`, so its count is 64-bit — the one place in
/// the texture tail that is not a 32-bit count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextureCpuCopy {
    pub size_x: i32,
    pub size_y: i32,
    pub num_slices: i32,
    pub format: u8,
    pub gamma_space: u8,
    pub raw_data: Vec<u8>,
}

/// `FOptTexturePlatformData` (Texture.h:801, 5.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub strip_flags: [u8; 2],
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
        let strip_flags = [r.u8()?, r.u8()?];
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
        ar.u8(&mut self.strip_flags[0].to_owned())?;
        ar.u8(&mut self.strip_flags[1].to_owned())?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualTextureDataChunk {
    pub bulk_data_hash: [u8; 20],
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
            let bulk_data_hash: [u8; 20] = r.take(20)?.try_into().expect("20 bytes");
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
            ar.raw(&mut c.bulk_data_hash.to_vec(), 20)?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticMeshBuffers {
    pub global_strip: u8,
    pub class_strip: u8,
    pub position_stride: i32,
    pub position_num_vertices: i32,
    pub positions: BulkArray,
    pub vertex_strip: [u8; 2],
    pub num_tex_coords: i32,
    pub num_vertices: i32,
    pub use_full_precision_uvs: u32,
    pub use_high_precision_tangent_basis: u32,
    /// Tangents and UVs, present unless the vertex buffer's own strip flags say
    /// otherwise.
    pub tangents_and_uvs: Option<(BulkArray, BulkArray)>,
    pub color_strip: [u8; 2],
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
        let vertex_strip = [r.u8()?, r.u8()?];
        let num_tex_coords = r.i32()?;
        let num_vertices = r.i32()?;
        let use_full_precision_uvs = r.u32()?;
        let use_high_precision_tangent_basis = r.u32()?;
        let tangents_and_uvs = if vertex_strip[0] & 2 == 0 {
            Some((BulkArray::read(r, "tangents")?, BulkArray::read(r, "UVs")?))
        } else {
            None
        };
        let color_strip = [r.u8()?, r.u8()?];
        let color_stride = r.i32()?;
        let color_num_vertices = r.i32()?;
        let colors = (color_strip[0] & 2 == 0 && color_num_vertices > 0)
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
        ar.u8(&mut self.vertex_strip[0].to_owned())?;
        ar.u8(&mut self.vertex_strip[1].to_owned())?;
        ar.i32(&mut self.num_tex_coords.to_owned())?;
        ar.i32(&mut self.num_vertices.to_owned())?;
        ar.u32(&mut self.use_full_precision_uvs.to_owned())?;
        ar.u32(&mut self.use_high_precision_tangent_basis.to_owned())?;
        match (&self.tangents_and_uvs, self.vertex_strip[0] & 2 == 0) {
            (Some((t, u)), true) => {
                t.write(ar)?;
                u.write(ar)?;
            }
            (None, false) => {}
            _ => bail!("tangent/UV presence disagrees with the vertex strip flags"),
        }
        ar.u8(&mut self.color_strip[0].to_owned())?;
        ar.u8(&mut self.color_strip[1].to_owned())?;
        ar.i32(&mut self.color_stride.to_owned())?;
        ar.i32(&mut self.color_num_vertices.to_owned())?;
        match (&self.colors, self.color_strip[0] & 2 == 0 && self.color_num_vertices > 0) {
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
    /// `FStaticMeshSection` — five `int32`s then five four-byte flags.
    pub sections: Vec<[u8; 40]>,
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
    /// `FStaticMeshBuffersSize`: three `uint32` totals.
    pub buffers_size: [u8; 12],
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
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "mesh sections", r.o - 4)?
        };
        let mut sections = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            sections.push(r.take(40)?.try_into().expect("40 bytes"));
        }
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
                buffers_size: r.take(12)?.try_into().expect("12 bytes"),
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
        ar.i32(&mut (self.sections.len() as i32))?;
        for s in &self.sections {
            ar.raw(&mut s.to_vec(), 40)?;
        }
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
                ar.raw(&mut rd.buffers_size.to_vec(), 12)?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaniteResources {
    pub strip_flags: [u8; 2],
    pub resource_flags: u32,
    /// `StreamablePages`: a bulk-data handle, never inline in this corpus.
    pub streamable_pages_index: i32,
    pub root_data: Vec<u8>,
    /// 20 bytes each.
    pub page_streaming_states: Vec<[u8; 20]>,
    /// `FPackedHierarchyNode` — four BVH slices of 52 bytes.
    pub hierarchy_nodes: Vec<[u8; 208]>,
    pub hierarchy_root_offsets: Vec<u32>,
    pub page_dependencies: Vec<u32>,
    /// Two bytes per entry.
    pub imposter_atlas: Vec<u8>,
    /// `NumRootPages`, `PositionPrecision`, `NormalPrecision`,
    /// `NumInputTriangles`, then `NumInputVertices`, the two `uint16` counts and
    /// `NumClusters`.
    pub stats: [u8; 28],
}

impl NaniteResources {
    fn read(r: &mut Reader) -> Result<Self> {
        let strip_flags = [r.u8()?, r.u8()?];
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
        let mut page_streaming_states = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            page_streaming_states.push(r.take(20)?.try_into().expect("20 bytes"));
        }
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "HierarchyNodes", r.o - 4)?
        };
        let mut hierarchy_nodes = Vec::with_capacity(n.min(256));
        for _ in 0..n {
            hierarchy_nodes.push(r.take(208)?.try_into().expect("208 bytes"));
        }
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
            stats: r.take(28)?.try_into().expect("28 bytes"),
        })
    }

    fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.strip_flags[0].to_owned())?;
        ar.u8(&mut self.strip_flags[1].to_owned())?;
        ar.u32(&mut self.resource_flags.to_owned())?;
        ar.i32(&mut self.streamable_pages_index.to_owned())?;
        ar.i32(&mut (self.root_data.len() as i32))?;
        let n = self.root_data.len();
        ar.raw(&mut self.root_data.clone(), n)?;
        ar.i32(&mut (self.page_streaming_states.len() as i32))?;
        for s in &self.page_streaming_states {
            ar.raw(&mut s.to_vec(), 20)?;
        }
        ar.i32(&mut (self.hierarchy_nodes.len() as i32))?;
        for s in &self.hierarchy_nodes {
            ar.raw(&mut s.to_vec(), 208)?;
        }
        write_u32_array(ar, &self.hierarchy_root_offsets)?;
        write_u32_array(ar, &self.page_dependencies)?;
        if self.imposter_atlas.len() % 2 != 0 {
            bail!("imposter atlas has an odd byte count");
        }
        ar.i32(&mut ((self.imposter_atlas.len() / 2) as i32))?;
        let n = self.imposter_atlas.len();
        ar.raw(&mut self.imposter_atlas.clone(), n)?;
        ar.raw(&mut self.stats.to_vec(), 28)
    }
}

/// One LOD's ray-tracing proxy entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RayTracingProxyLod {
    /// `bOwnsBuffers`, and the 40-byte sections it owns when set.
    pub sections: Option<Vec<[u8; 40]>>,
    pub owns_ray_tracing_geometry: u32,
    pub bulk_index: i32,
    /// The streamable payload, present only when the bulk map puts it here.
    pub payload: Option<Vec<u8>>,
}

/// `FStaticMeshRayTracingProxy`, written only when `bHasRayTracingProxy`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RayTracingProxy {
    pub strip_flags: [u8; 2],
    pub using_rendering_lods: u32,
    pub lods: Vec<RayTracingProxyLod>,
}

/// One LOD's Lumen card representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRepresentation {
    /// `Bounds` as an `FBox` — three doubles each way plus `IsValid`.
    pub bounds: [u8; 49],
    pub mostly_two_sided: u32,
    /// `FLumenCardBuildData` — an `FLumenCardOBB` of five `FVector3f` plus the
    /// axis-aligned direction index, 61 bytes.
    pub cards: Vec<[u8; 61]>,
}

/// One LOD's distance-field volume (`FDistanceFieldVolumeData5`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistanceFieldVolume {
    /// `LocalSpaceMeshBounds` is an `FBox3f` — six floats and `IsValid`, 25
    /// bytes, not the 49-byte double-width `FBox`.
    pub local_space_mesh_bounds: [u8; 25],
    pub mostly_two_sided: u32,
    /// Three `FSparseDistanceFieldMip` of 56 bytes.
    pub mips: [u8; 168],
    pub always_loaded_mip: Vec<u8>,
    /// `StreamableMips`: a bulk-data handle.
    pub streamable_mips_index: i32,
}

/// The whole tail of a `UStaticMesh` export: 15,231 exports and 1,310 MiB, the
/// largest tail population in the corpus.
// No `Eq`: LODs carry `f32` deviations and sampler weights.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticMeshTail {
    pub strip_flags: [u8; 2],
    pub cooked: u32,
    pub body_setup: i32,
    pub nav_collision: i32,
    pub lighting_guid: [u8; 16],
    pub sockets: Vec<i32>,
    pub lods: Vec<StaticMeshLod>,
    pub num_inlined_lods: u8,
    pub nanite: NaniteResources,
    pub ray_tracing_proxy: Option<RayTracingProxy>,
    pub card_strip: [u8; 2],
    /// Per LOD, `None` where the validity flag was zero. Absent entirely when
    /// the strip flags dropped the whole section.
    pub card_representations: Option<Vec<Option<CardRepresentation>>>,
    pub distance_field_strip: [u8; 2],
    pub distance_fields: Option<Vec<Option<DistanceFieldVolume>>>,
    /// `Bounds`: an `FBoxSphereBounds`.
    pub bounds: [u8; 56],
    pub lods_share_static_lighting: u32,
    /// `ScreenSize[MAX_STATIC_LODS_UE4]`, each an `FPerPlatformFloat`.
    pub screen_sizes: [u8; 64],
    pub render_data_strip: [u8; 2],
    pub has_speed_tree_wind: u32,
    /// `FStaticMaterial` — 36 bytes each.
    pub materials: Vec<[u8; 36]>,
}

impl StaticMeshTail {
    pub fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let strip_flags = [r.u8()?, r.u8()?];
        let cooked = r.u32()?;
        let body_setup = r.i32()?;
        let nav_collision = r.i32()?;
        let lighting_guid: [u8; 16] = r.take(16)?.try_into().expect("16 bytes");
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
                let strip_flags = [r.u8()?, r.u8()?];
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

        let card_strip = [r.u8()?, r.u8()?];
        let card_representations = (card_strip[0] & 2 == 0 && card_strip[1] & 2 == 0)
            .then(|| -> Result<Vec<Option<CardRepresentation>>> {
                (0..lods.len())
                    .map(|_| {
                        if r.u32()? == 0 {
                            return Ok(None);
                        }
                        let bounds: [u8; 49] = r.take(49)?.try_into().expect("49 bytes");
                        let mostly_two_sided = r.u32()?;
                        let n = {
                            let n = r.i32()?;
                            super::limits::bounded(n, MAX_NATIVE_COUNT, "CardBuildData", r.o - 4)?
                        };
                        let mut cards = Vec::with_capacity(n.min(64));
                        for _ in 0..n {
                            cards.push(r.take(61)?.try_into().expect("61 bytes"));
                        }
                        Ok(Some(CardRepresentation { bounds, mostly_two_sided, cards }))
                    })
                    .collect()
            })
            .transpose()?;

        let distance_field_strip = [r.u8()?, r.u8()?];
        let distance_fields = (distance_field_strip[0] & 2 == 0 && distance_field_strip[1] & 1 == 0)
            .then(|| -> Result<Vec<Option<DistanceFieldVolume>>> {
                (0..lods.len())
                    .map(|_| {
                        if r.u32()? == 0 {
                            return Ok(None);
                        }
                        let local_space_mesh_bounds = r.take(25)?.try_into().expect("25 bytes");
                        let mostly_two_sided = r.u32()?;
                        let mips = r.take(168)?.try_into().expect("168 bytes");
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

        let bounds = r.take(56)?.try_into().expect("56 bytes");
        let lods_share_static_lighting = r.u32()?;
        let screen_sizes = r.take(64)?.try_into().expect("64 bytes");
        let render_data_strip = [r.u8()?, r.u8()?];
        let has_speed_tree_wind = r.u32()?;
        let n = {
            let n = r.i32()?;
            super::limits::bounded(n, MAX_NATIVE_COUNT, "StaticMaterials", r.o - 4)?
        };
        let mut materials = Vec::with_capacity(n.min(64));
        for _ in 0..n {
            materials.push(r.take(36)?.try_into().expect("36 bytes"));
        }

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
        ar.u8(&mut self.strip_flags[0].to_owned())?;
        ar.u8(&mut self.strip_flags[1].to_owned())?;
        ar.u32(&mut self.cooked.to_owned())?;
        ar.i32(&mut self.body_setup.to_owned())?;
        ar.i32(&mut self.nav_collision.to_owned())?;
        ar.raw(&mut self.lighting_guid.to_vec(), 16)?;
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
                ar.u8(&mut p.strip_flags[0].to_owned())?;
                ar.u8(&mut p.strip_flags[1].to_owned())?;
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

        ar.u8(&mut self.card_strip[0].to_owned())?;
        ar.u8(&mut self.card_strip[1].to_owned())?;
        match (&self.card_representations, self.card_strip[0] & 2 == 0 && self.card_strip[1] & 2 == 0)
        {
            (Some(v), true) => {
                if v.len() != self.lods.len() {
                    bail!("{} card representations for {} LODs", v.len(), self.lods.len());
                }
                for c in v {
                    match c {
                        Some(c) => {
                            ar.u32(&mut 1)?;
                            ar.raw(&mut c.bounds.to_vec(), 49)?;
                            ar.u32(&mut c.mostly_two_sided.to_owned())?;
                            ar.i32(&mut (c.cards.len() as i32))?;
                            for card in &c.cards {
                                ar.raw(&mut card.to_vec(), 61)?;
                            }
                        }
                        None => ar.u32(&mut 0)?,
                    }
                }
            }
            (None, false) => {}
            _ => bail!("card representation presence disagrees with the strip flags"),
        }

        ar.u8(&mut self.distance_field_strip[0].to_owned())?;
        ar.u8(&mut self.distance_field_strip[1].to_owned())?;
        match (
            &self.distance_fields,
            self.distance_field_strip[0] & 2 == 0 && self.distance_field_strip[1] & 1 == 0,
        ) {
            (Some(v), true) => {
                if v.len() != self.lods.len() {
                    bail!("{} distance fields for {} LODs", v.len(), self.lods.len());
                }
                for d in v {
                    match d {
                        Some(d) => {
                            ar.u32(&mut 1)?;
                            ar.raw(&mut d.local_space_mesh_bounds.to_vec(), 25)?;
                            ar.u32(&mut d.mostly_two_sided.to_owned())?;
                            ar.raw(&mut d.mips.to_vec(), 168)?;
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

        ar.raw(&mut self.bounds.to_vec(), 56)?;
        ar.u32(&mut self.lods_share_static_lighting.to_owned())?;
        ar.raw(&mut self.screen_sizes.to_vec(), 64)?;
        ar.u8(&mut self.render_data_strip[0].to_owned())?;
        ar.u8(&mut self.render_data_strip[1].to_owned())?;
        ar.u32(&mut self.has_speed_tree_wind.to_owned())?;
        ar.i32(&mut (self.materials.len() as i32))?;
        for m in &self.materials {
            ar.raw(&mut m.to_vec(), 36)?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodySetupTail {
    pub guid: [u8; 16],
    pub cooked: bool,
    /// Written only when cooked, so `None` and `Some(false)` are different
    /// files, not the same one described two ways.
    pub has_cooked_data: Option<bool>,
    pub formats: Vec<CookedFormat>,
}

impl BodySetupTail {
    pub fn read(r: &mut Reader, ctx: TailContext) -> Result<Self> {
        let guid: [u8; 16] = r.take(16)?.try_into().expect("16 bytes");
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
        ar.raw(&mut self.guid.to_vec(), 16)?;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub texture_strip_flags: [u8; 2],
    pub cooked: TextureCookedData,
}

impl TextureChainTail {
    pub fn read(r: &mut Reader, ctx: TailContext, has_mip_data_flag: bool) -> Result<Self> {
        Ok(TextureChainTail {
            texture_strip_flags: [r.u8()?, r.u8()?],
            cooked: TextureCookedData::read(r, ctx, has_mip_data_flag)?,
        })
    }

    pub fn write(&self, ar: &mut impl Ar) -> Result<()> {
        ar.u8(&mut self.texture_strip_flags[0].to_owned())?;
        ar.u8(&mut self.texture_strip_flags[1].to_owned())?;
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
        // `UTexture2D` alone writes a `bSerializeMipData` flag; the other cooked
        // texture shapes call the shared serializer directly. `UTextureLightProfile`
        // *derives* from `UTexture2D`, so it writes the flag too — treating it as
        // a sibling of `UTextureCube` desynced all 7 of them.
        "Texture2D" | "TextureCube" | "VolumeTexture" | "Texture2DArray"
        | "TextureLightProfile" => Some((|| {
            let mut r = Reader::new(tail, names);
            let derives_from_texture_2d = matches!(class, "Texture2D" | "TextureLightProfile");
            let modeled = TextureChainTail::read(&mut r, ctx, derives_from_texture_2d)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        // `UMaterial` always writes inline shader maps; a `UMaterialInstance`
        // writes its own cache first and defers to the property block.
        "Material" | "MaterialInstanceConstant" | "LandscapeMaterialInstanceConstant"
        | "MaterialInstanceDynamic" => Some((|| {
            let mut r = Reader::new(tail, names);
            let is_instance = class != "Material";
            let modeled = MaterialChainTail::read(&mut r, block, ctx, is_instance)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w, block, ctx)?;
            Ok(w.into_bytes())
        })()),
        "StaticMesh" => Some((|| {
            let mut r = Reader::new(tail, names);
            let modeled = StaticMeshTail::read(&mut r, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        "BodySetup" => Some((|| {
            let mut r = Reader::new(tail, names);
            let modeled = BodySetupTail::read(&mut r, ctx)?;
            if r.o != tail.len() {
                bail!("model consumed {} of {} tail bytes", r.o, tail.len());
            }
            let mut w = super::archive::Writer::new();
            modeled.write(&mut w)?;
            Ok(w.into_bytes())
        })()),
        _ => None,
    }
}
