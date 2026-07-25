//! Reader for cooked UE5 `UStaticMesh` render geometry inside IoStore packages
//! — positions, normals, UVs, and triangle indices — for the highest LOD.
//! Campaign Evolved (UE 5.5.4) layout.
//!
//! Like [`super::skeletal_mesh`], we ANCHOR on a recognizable structure rather
//! than decode the unversioned property block: an `FStaticMeshLODResources`
//! begins its vertex data with an `FPositionVertexBuffer`, whose header is a
//! distinctive `[Stride=12][NumVertices][ElementSize=12][Num=NumVertices]` run
//! followed by `NumVertices` finite XYZ floats. From that anchor the cooked
//! buffer order is deterministic:
//! `Position → StaticMeshVertexBuffer(tangents+UVs) → Color → IndexBuffer`.

use anyhow::{bail, Result};
use half::f16;

/// One vertex of a static render mesh.
#[derive(Debug, Clone)]
pub struct StaticVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    /// First UV set (V flipped to the classic-tools convention is left to the
    /// caller; this is the raw UE UV).
    pub uv: [f32; 2],
}

/// The parsed highest-LOD static render mesh.
#[derive(Debug)]
pub struct StaticMesh {
    /// Triangle indices into `vertices`.
    pub indices: Vec<u32>,
    pub vertices: Vec<StaticVertex>,
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
        let s = self
            .b
            .get(self.p..self.p + n)
            .ok_or_else(|| anyhow::anyhow!("static mesh: read past end at {} (+{n})", self.p))?;
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
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
}

fn i32_at(b: &[u8], o: usize) -> Option<i32> {
    b.get(o..o + 4).map(|s| i32::from_le_bytes(s.try_into().unwrap()))
}

/// Find an `FPositionVertexBuffer` header: `[12][N][12][N]` with `N` a plausible
/// vertex count and the first few XYZ floats finite and in a sane range. Returns
/// the offset of the `Stride` field (buffer start). Scans from `from`.
fn find_position_buffer(b: &[u8], from: usize) -> Option<usize> {
    let mut o = from;
    while o + 16 < b.len() {
        if i32_at(b, o) == Some(12) && i32_at(b, o + 8) == Some(12) {
            if let (Some(n1), Some(n2)) = (i32_at(b, o + 4), i32_at(b, o + 12)) {
                if n1 == n2 && (1..=4_000_000).contains(&n1) {
                    let n = n1 as usize;
                    // Need N*12 bytes of data after the 16-byte header.
                    if o + 16 + n * 12 <= b.len() {
                        // Sanity: first up-to-8 floats finite and within ±1e6.
                        let sane = (0..(n.min(8) * 3)).all(|i| {
                            let f = f32::from_le_bytes(
                                b[o + 16 + i * 4..o + 20 + i * 4].try_into().unwrap(),
                            );
                            f.is_finite() && f.abs() < 1.0e6
                        });
                        if sane {
                            return Some(o);
                        }
                    }
                }
            }
        }
        o += 1;
    }
    None
}

impl StaticMesh {
    /// Parse the highest LOD from a `UStaticMesh` export body. `header_size` is
    /// `FZenPackageSummary::header_size` (export data start).
    pub fn from_package(package_bytes: &[u8], header_size: usize) -> Result<Self> {
        let body = package_bytes
            .get(header_size..)
            .ok_or_else(|| anyhow::anyhow!("header_size past end"))?;

        // Try successive position-buffer anchors until one yields a mesh whose
        // buffers cross-check (guards against a stray 12/N/12/N in properties).
        let mut from = 0usize;
        let mut last_err = anyhow::anyhow!("no FPositionVertexBuffer anchor");
        while let Some(anchor) = find_position_buffer(body, from) {
            match Self::read_from(body, anchor) {
                Ok(m) => return Ok(m),
                Err(e) => {
                    last_err = e;
                    from = anchor + 1;
                }
            }
        }
        Err(last_err)
    }

    /// Prefer the full-resolution **Nanite** geometry when the package has it
    /// and its `.ubulk` bulk data is available; otherwise fall back to the
    /// coarse `FStaticMeshLODResources` reader ([`Self::from_package`]).
    ///
    /// CE static meshes are Nanite: the classic LOD holds only a ~1% decimated
    /// fallback, so faithful extraction needs this path. Decoding is heavy
    /// (~1s and hundreds of thousands of triangles for large meshes), so
    /// callers that only need a light preview should use [`Self::from_package`].
    pub fn from_package_preferring_nanite(
        package_bytes: &[u8],
        header_size: usize,
        ubulk: Option<&[u8]>,
    ) -> Result<Self> {
        if let Some(ubulk) = ubulk {
            if let Some(res) = super::nanite::NaniteResources::parse(package_bytes, header_size) {
                let mesh = super::nanite::decode_nanite(package_bytes, ubulk, &res);
                if !mesh.triangles.is_empty() && mesh.unresolved_vertices == 0 {
                    return Ok(Self::from_nanite(&mesh));
                }
            }
        }
        Self::from_package(package_bytes, header_size)
    }

    /// Convert a decoded [`super::nanite::NaniteMesh`] into a `StaticMesh`.
    pub fn from_nanite(mesh: &super::nanite::NaniteMesh) -> Self {
        let vertices = (0..mesh.positions.len())
            .map(|i| StaticVertex {
                position: mesh.positions[i],
                normal: *mesh.normals.get(i).unwrap_or(&[0.0; 3]),
                uv: *mesh.uvs.get(i).unwrap_or(&[0.0; 2]),
            })
            .collect();
        let indices = mesh.triangles.iter().flatten().copied().collect();
        Self { indices, vertices }
    }

    fn read_from(body: &[u8], anchor: usize) -> Result<Self> {
        let mut r = R::new(body, anchor);

        // FPositionVertexBuffer: Stride(=12), NumVertices, [elem=12, count, data].
        let _stride = r.i32()?;
        let _num = r.i32()?;
        let pos_elem = r.i32()? as usize;
        let pos_count = r.i32()? as usize;
        if pos_elem != 12 {
            bail!("position element size {pos_elem} != 12");
        }
        let mut positions = Vec::with_capacity(pos_count);
        for _ in 0..pos_count {
            positions.push([r.f32()?, r.f32()?, r.f32()?]);
        }

        // FStaticMeshVertexBuffer: strip(2) + numUV + numVerts + fullUV + highTangent
        // + tangent bulk + UV bulk.
        r.skip(2)?;
        let num_uv = r.i32()? as usize;
        let _vb_numverts = r.i32()?;
        let _full_uv = r.boolean()?;
        let high_tangent = r.boolean()?;
        let tan_elem = r.i32()? as usize;
        let tan_count = r.i32()? as usize;
        if tan_count != pos_count || !(4..=16).contains(&tan_elem) {
            bail!("tangent buffer mismatch (count {tan_count} vs {pos_count}, elem {tan_elem})");
        }
        let tan_data = r.take(tan_count * tan_elem)?;
        let uv_elem = r.i32()? as usize;
        let uv_count = r.i32()? as usize;
        if uv_count != pos_count * num_uv.max(1) || !(4..=16).contains(&uv_elem) {
            bail!("uv buffer mismatch (count {uv_count}, verts {pos_count}, numUV {num_uv})");
        }
        let uv_data = r.take(uv_count * uv_elem)?;

        // FColorVertexBuffer: strip(2) + Stride + NumVertices; bulk only if non-empty.
        r.skip(2)?;
        let _col_stride = r.i32()?;
        let col_num = r.i32()?;
        if col_num > 0 {
            let ce = r.i32()? as usize;
            let cc = r.i32()? as usize;
            r.skip(ce * cc)?;
        }

        // FRawStaticIndexBuffer IndexBuffer: bool b32Bit + bulk [elem, count, data].
        let indices = read_index_buffer(&mut r, pos_count)?;

        // Assemble vertices.
        let mut vertices = Vec::with_capacity(pos_count);
        for v in 0..pos_count {
            let normal = decode_normal(tan_data, v, tan_elem, high_tangent);
            let uv = decode_uv(uv_data, v * num_uv.max(1), uv_elem);
            vertices.push(StaticVertex { position: positions[v], normal, uv });
        }
        Ok(StaticMesh { indices, vertices })
    }
}

/// `FRawStaticIndexBuffer`: `bool b32Bit` (as `int32`) then the index storage
/// as a raw byte bulk array `[ElementSize=1][Num=byteCount][bytes]`. Index width
/// (2 or 4) comes from `b32Bit`; validated against the vertex count.
fn read_index_buffer(r: &mut R, num_verts: usize) -> Result<Vec<u32>> {
    let b32 = r.boolean()?;
    let elem = r.i32()? as usize;
    let count = r.i32()? as usize;
    let total_bytes = elem
        .checked_mul(count)
        .ok_or_else(|| anyhow::anyhow!("index byte count overflow"))?;
    let width = if b32 { 4 } else { 2 };
    if total_bytes == 0 || total_bytes % (width * 3) != 0 || total_bytes > 240_000_000 {
        bail!("implausible index buffer (b32 {b32}, elem {elem}, count {count})");
    }
    let num = total_bytes / width;
    let data = r.take(total_bytes)?;
    let mut out = Vec::with_capacity(num);
    for i in 0..num {
        let idx = if width == 2 {
            u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]) as u32
        } else {
            u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap())
        };
        if idx as usize >= num_verts {
            bail!("index {idx} out of range (verts {num_verts})");
        }
        out.push(idx);
    }
    Ok(out)
}

fn packed_component(b: u8) -> f32 {
    (b as f32 / 127.5) - 1.0
}

fn decode_normal(tan: &[u8], v: usize, elem: usize, high: bool) -> [f32; 3] {
    let base = v * elem;
    if high {
        // FPackedRGBA16N: TangentX(8) + TangentZ(8), each 4×u16 → [-1,1].
        let z = base + 8;
        let c = |o: usize| (u16::from_le_bytes([tan[z + o], tan[z + o + 1]]) as f32 / 32767.5) - 1.0;
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
        let u = f16::from_le_bytes([uv[base], uv[base + 1]]).to_f32();
        let v = f16::from_le_bytes([uv[base + 2], uv[base + 3]]).to_f32();
        [u, v]
    } else {
        let u = f32::from_le_bytes(uv[base..base + 4].try_into().unwrap());
        let v = f32::from_le_bytes(uv[base + 4..base + 8].try_into().unwrap());
        [u, v]
    }
}

fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > 1e-6 {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 1.0]
    }
}
