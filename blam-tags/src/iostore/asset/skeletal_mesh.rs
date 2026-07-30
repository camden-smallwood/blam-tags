//! Reader for cooked UE5 `USkeletalMesh` render geometry inside IoStore
//! packages — positions, indices, UVs, normals, and skin weights — for the
//! highest LOD. Campaign Evolved (UE 5.5.4) layout, validated byte-exact
//! against real meshes.
//!
//! We reach the native `FSkeletalMeshRenderData` by ANCHORING on the
//! `FReferenceSkeleton` (a recognizable `numBones` + valid `FMeshBoneInfo`
//! run) rather than decoding the unversioned property block that precedes
//! it — so no `.usmap` is required for geometry, and it's resilient to
//! property-schema changes across patches.

use anyhow::{bail, Result};
use half::f16;

/// One bone of the mesh's reference skeleton.
#[derive(Debug, Clone)]
pub struct SkelBone {
    pub name: String,
    /// Parent bone index, or `-1` for the root.
    pub parent: i32,
    /// Reference (bind) pose, parent-local: `FQuat` (x,y,z,w) + `FVector`.
    /// This is the UE skeleton's bind pose the static components attach to,
    /// which can differ from the classic skeleton_model tag's bind pose.
    pub rest_rotation: [f32; 4],
    pub rest_translation: [f32; 3],
}

/// A render section: a contiguous vertex/triangle range bound to one
/// material and a section-local bone map.
#[derive(Debug, Clone)]
pub struct SkelSection {
    pub material_index: u16,
    pub base_index: u32,
    pub num_triangles: u32,
    pub base_vertex: u32,
    pub num_vertices: u32,
    /// Section-local slot → global reference-skeleton bone index.
    pub bone_map: Vec<u16>,
}

/// A skin influence: a global bone index and its normalized weight.
#[derive(Debug, Clone, Copy)]
pub struct Influence {
    pub bone: u16,
    pub weight: f32,
}

/// One vertex of the render mesh.
#[derive(Debug, Clone)]
pub struct SkelVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// First UV set (V flipped to the classic-tools convention).
    pub uv: [f32; 2],
    /// Non-zero influences, bones as global reference-skeleton indices.
    pub influences: Vec<Influence>,
}

/// The parsed highest-LOD render mesh.
#[derive(Debug)]
pub struct SkeletalMesh {
    pub bones: Vec<SkelBone>,
    pub sections: Vec<SkelSection>,
    /// Triangle indices into `vertices`.
    pub indices: Vec<u32>,
    pub vertices: Vec<SkelVertex>,
}

struct R<'a> {
    b: &'a [u8],
    p: usize,
}
impl<'a> R<'a> {
    fn new(b: &'a [u8], p: usize) -> Self {
        Self { b, p }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self.b.get(self.p..self.p + n).ok_or_else(|| {
            anyhow::anyhow!("skeletal mesh: read past end at {} (+{n})", self.p)
        })?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
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
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// `FArchive` serializes `bool` as `int32`.
    fn boolean(&mut self) -> Result<bool> {
        Ok(self.i32()? != 0)
    }
    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n)?;
        Ok(())
    }
    fn peek_i32(&self, off: usize) -> Option<i32> {
        self.b
            .get(self.p + off..self.p + off + 4)
            .map(|s| i32::from_le_bytes(s.try_into().unwrap()))
    }
}

/// Locate the `FReferenceSkeleton` in an export body: `i32 numBones` then a
/// fully-valid run of `FMeshBoneInfo` = FName(8) + `i32 parent`.
fn find_ref_skeleton(body: &[u8], names_len: usize) -> Option<usize> {
    let i32_at = |o: usize| body.get(o..o + 4).map(|s| i32::from_le_bytes(s.try_into().unwrap()));
    let mut o = 0;
    while o + 8 < body.len() {
        if let Some(n) = i32_at(o) {
            if (8..=4096).contains(&n) {
                let n = n as usize;
                let ok = (0..n).all(|i| {
                    let e = o + 4 + i * 12;
                    match (i32_at(e), i32_at(e + 8)) {
                        (Some(ni), Some(par)) => {
                            ni >= 0
                                && (ni as usize) < names_len
                                && if i == 0 { par == -1 } else { par >= 0 && (par as usize) < n }
                        }
                        _ => false,
                    }
                });
                if ok {
                    return Some(o);
                }
            }
        }
        o += 1;
    }
    None
}

/// Decode an `FPackedNormal` component (`u8`) to `[-1, 1]`.
fn packed_component(b: u8) -> f32 {
    (b as f32 / 127.5) - 1.0
}

impl SkeletalMesh {
    /// Parse the highest LOD from a `USkeletalMesh` export body. `names` is
    /// the package name map (`FZenPackageHeader::name_map.copy_raw_names()`);
    /// `header_size` is `FZenPackageSummary::header_size` (export data start).
    pub fn from_package(package_bytes: &[u8], names: &[String], header_size: usize) -> Result<Self> {
        let body = package_bytes
            .get(header_size..)
            .ok_or_else(|| anyhow::anyhow!("header_size past end"))?;
        let anchor =
            find_ref_skeleton(body, names.len()).ok_or_else(|| anyhow::anyhow!("no FReferenceSkeleton anchor"))?;
        let mut r = R::new(body, anchor);

        // FReferenceSkeleton: FMeshBoneInfo[], FTransform[] refpose, TMap.
        let nbones = r.i32()? as usize;
        let mut bones = Vec::with_capacity(nbones);
        for _ in 0..nbones {
            let name_idx = r.i32()? as usize;
            let _number = r.i32()?;
            let parent = r.i32()?;
            bones.push(SkelBone {
                name: names.get(name_idx).cloned().unwrap_or_default(),
                parent,
                rest_rotation: [0.0, 0.0, 0.0, 1.0],
                rest_translation: [0.0; 3],
            });
        }
        let refpose_n = r.i32()? as usize;
        // FTransform size: LWC double (80) vs float (40), by which makes the
        // following map count read back nbones.
        let tsize = [80usize, 40]
            .into_iter()
            .find(|&ts| r.peek_i32(refpose_n * ts) == Some(nbones as i32))
            .ok_or_else(|| anyhow::anyhow!("could not determine FTransform size"))?;
        // FTransform = FQuat rotation + FVector translation + FVector scale.
        for i in 0..refpose_n {
            let (rot, trans) = if tsize == 80 {
                let rot = [r.f64()? as f32, r.f64()? as f32, r.f64()? as f32, r.f64()? as f32];
                let trans = [r.f64()? as f32, r.f64()? as f32, r.f64()? as f32];
                let _scale = (r.f64()?, r.f64()?, r.f64()?);
                (rot, trans)
            } else {
                let rot = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                let trans = [r.f32()?, r.f32()?, r.f32()?];
                let _scale = (r.f32()?, r.f32()?, r.f32()?);
                (rot, trans)
            };
            if let Some(b) = bones.get_mut(i) {
                b.rest_rotation = rot;
                b.rest_translation = trans;
            }
        }
        let map_n = r.i32()? as usize;
        r.skip(map_n * 12)?; // FName(8) + i32(4)

        let _b_cooked = r.boolean()?;
        let num_lods = r.i32()?;
        if !(1..=16).contains(&num_lods) {
            bail!("implausible NumLODs {num_lods} — layout drift");
        }

        // LOD 0 (highest detail).
        r.skip(2)?; // FStripDataFlags
        let lod_cooked_out = r.boolean()?;
        let inlined = r.boolean()?;
        if lod_cooked_out {
            bail!("LOD0 is cooked out");
        }
        let req = r.i32()? as usize;
        r.skip(req * 2)?; // RequiredBones (u16[])

        let nsec = r.i32()?;
        if !(0..=256).contains(&nsec) {
            bail!("implausible section count {nsec}");
        }
        let mut sections = Vec::with_capacity(nsec as usize);
        for _ in 0..nsec {
            let _sg = r.u8()?;
            let sclass = r.u8()?;
            let material_index = r.i16()? as u16;
            let base_index = r.u32()?;
            let num_triangles = r.u32()?;
            let _brecompute = r.boolean()?;
            let _recompute_mask = r.u8()?; // RecomputeTangentsVertexMaskChannel
            let _bcast = r.boolean()?;
            let _bvisrt = r.boolean()?;
            let base_vertex = r.u32()?;
            let cloth_outer = r.i32()?;
            for _ in 0..cloth_outer {
                let inner = r.i32()? as usize;
                r.skip(inner * FMESH_TO_MESH_VERTDATA)?;
            }
            let bonemap_n = r.i32()? as usize;
            let mut bone_map = Vec::with_capacity(bonemap_n);
            for _ in 0..bonemap_n {
                bone_map.push(r.u16()?);
            }
            let num_vertices = r.u32()?;
            let _max_influences = r.i32()?;
            let _corr_cloth = r.i16()?;
            r.skip(20)?; // FClothingSectionData: FGuid(16) + i32
            if sclass & 1 == 0 {
                let dv = r.i32()? as usize;
                r.skip(dv * 4)?; // DupVertData
                let dvi = r.i32()? as usize;
                r.skip(dvi * 8)?; // DupVertIndexData
            }
            let _b_disabled = r.boolean()?;
            sections.push(SkelSection {
                material_index,
                base_index,
                num_triangles,
                base_vertex,
                num_vertices,
                bone_map,
            });
        }

        let active_n = r.i32()? as usize;
        r.skip(active_n * 2)?; // ActiveBoneIndices
        let _buffers_size = r.u32()?;
        if !inlined {
            bail!("LOD0 is not inlined (streamed to .ubulk) — not yet supported");
        }

        // SerializeStreamedData
        r.skip(2)?; // FStripDataFlags
        // FMultisizeIndexContainer
        let data_size = r.u8()?;
        let idx_elem = r.i32()? as usize;
        let idx_count = r.i32()? as usize;
        let mut indices = Vec::with_capacity(idx_count);
        for _ in 0..idx_count {
            match data_size {
                2 => indices.push(r.u16()? as u32),
                4 => indices.push(r.u32()?),
                n => bail!("unexpected index DataSize {n}"),
            }
        }
        let _ = idx_elem;

        // FPositionVertexBuffer
        let _pos_stride = r.i32()?;
        let _pos_numverts = r.i32()?;
        let pos_elem = r.i32()? as usize;
        let pos_count = r.i32()? as usize;
        if pos_elem != 12 {
            bail!("unexpected position element size {pos_elem}");
        }
        let mut positions = Vec::with_capacity(pos_count);
        for _ in 0..pos_count {
            positions.push([r.f32()?, r.f32()?, r.f32()?]);
        }

        // FStaticMeshVertexBuffer (tangents + UVs)
        r.skip(2)?; // FStripDataFlags
        let num_uv = r.i32()? as usize;
        let _vb_numverts = r.i32()?;
        let _full_uv = r.boolean()?;
        let high_tangent = r.boolean()?;
        let tan_elem = r.i32()? as usize;
        let tan_count = r.i32()? as usize;
        let tan_data = r.take(tan_count * tan_elem)?;
        let uv_elem = r.i32()? as usize;
        let uv_count = r.i32()? as usize;
        let uv_data = r.take(uv_count * uv_elem)?;

        // FSkinWeightVertexBuffer
        r.skip(2)?; // FStripDataFlags
        let _var_bones = r.boolean()?;
        let sw_maxinfl = r.u32()? as usize;
        let _sw_numbones = r.u32()?;
        let _sw_numverts = r.u32()?;
        let use16_idx = r.boolean()?;
        let use16_wt = r.boolean()?;
        let sw_elem = r.i32()? as usize;
        let sw_count = r.i32()? as usize;
        let sw_data = r.take(sw_count * sw_elem)?;

        // Assemble vertices.
        let nverts = pos_count;
        let idx_bytes = if use16_idx { 2 } else { 1 };
        let wt_bytes = if use16_wt { 2 } else { 1 };
        let stride = sw_maxinfl * (idx_bytes + wt_bytes);
        let mut vertices = Vec::with_capacity(nverts);
        for v in 0..nverts {
            // Normal from tangent Z (second FPackedNormal / FPackedRGBA16N).
            let normal = decode_normal(tan_data, v, tan_elem, high_tangent);
            // UV: per-vertex contiguous, first set.
            let uv = decode_uv(uv_data, v * num_uv, uv_elem);
            // Skin influences via the owning section's bone map.
            let sec = section_of(&sections, v as u32);
            let influences = decode_influences(
                sw_data, v, stride, sw_maxinfl, idx_bytes, wt_bytes, sec.map(|s| &s.bone_map),
            );
            vertices.push(SkelVertex { position: positions[v], normal, uv, influences });
        }

        Ok(SkeletalMesh { bones, sections, indices, vertices })
    }
}

const FMESH_TO_MESH_VERTDATA: usize = 80;

fn section_of(sections: &[SkelSection], v: u32) -> Option<&SkelSection> {
    sections
        .iter()
        .find(|s| v >= s.base_vertex && v < s.base_vertex + s.num_vertices)
        .or_else(|| sections.first())
}

fn decode_normal(tan: &[u8], v: usize, elem: usize, high: bool) -> [f32; 3] {
    let base = v * elem;
    if high {
        // FPackedRGBA16N: TangentX(8) + TangentZ(8), each 4×u16 mapped [-1,1].
        let z = base + 8;
        let c = |o: usize| {
            let raw = u16::from_le_bytes([tan[z + o], tan[z + o + 1]]);
            (raw as f32 / 32767.5) - 1.0
        };
        normalize([c(0), c(2), c(4)])
    } else {
        // FPackedNormal: TangentX(4) + TangentZ(4), each 4×u8.
        let z = base + 4;
        normalize([
            packed_component(tan[z]),
            packed_component(tan[z + 1]),
            packed_component(tan[z + 2]),
        ])
    }
}

fn decode_uv(uv: &[u8], item: usize, elem: usize) -> [f32; 2] {
    let base = item * elem;
    if elem == 4 {
        // FMeshUVHalf: 2×f16
        let u = f16::from_le_bytes([uv[base], uv[base + 1]]).to_f32();
        let v = f16::from_le_bytes([uv[base + 2], uv[base + 3]]).to_f32();
        [u, v]
    } else {
        // FMeshUVFloat: 2×f32
        let u = f32::from_le_bytes(uv[base..base + 4].try_into().unwrap());
        let v = f32::from_le_bytes(uv[base + 4..base + 8].try_into().unwrap());
        [u, v]
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_influences(
    sw: &[u8],
    v: usize,
    stride: usize,
    maxinfl: usize,
    idx_bytes: usize,
    wt_bytes: usize,
    bone_map: Option<&Vec<u16>>,
) -> Vec<Influence> {
    let base = v * stride;
    let mut out = Vec::new();
    for i in 0..maxinfl {
        let idx_off = base + i * idx_bytes;
        let wt_off = base + maxinfl * idx_bytes + i * wt_bytes;
        if wt_off + wt_bytes > sw.len() || idx_off + idx_bytes > sw.len() {
            break;
        }
        let local = if idx_bytes == 2 {
            u16::from_le_bytes([sw[idx_off], sw[idx_off + 1]])
        } else {
            sw[idx_off] as u16
        };
        let raw_w = if wt_bytes == 2 {
            u16::from_le_bytes([sw[wt_off], sw[wt_off + 1]]) as f32 / 65535.0
        } else {
            sw[wt_off] as f32 / 255.0
        };
        if raw_w <= 0.0 {
            continue;
        }
        let bone = bone_map.and_then(|m| m.get(local as usize).copied()).unwrap_or(local);
        out.push(Influence { bone, weight: raw_w });
    }
    out
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}
