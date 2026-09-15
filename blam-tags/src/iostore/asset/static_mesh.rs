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

use std::collections::{HashMap, HashSet};

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
        let mut vertices: Vec<StaticVertex> = (0..mesh.positions.len())
            .map(|i| StaticVertex {
                position: mesh.positions[i],
                normal: *mesh.normals.get(i).unwrap_or(&[0.0; 3]),
                uv: *mesh.uvs.get(i).unwrap_or(&[0.0; 2]),
            })
            .collect();
        let mut indices = mesh.triangles.iter().flatten().copied().collect();
        repair_nanite_triangular_holes(&vertices, &mut indices);
        split_nanite_negative_uv_wraps(&mut vertices, &mut indices);
        for triangle in indices.chunks_exact_mut(3) {
            let oriented = orient_nanite_triangle(
                &vertices,
                [triangle[0], triangle[1], triangle[2]],
            );
            triangle.copy_from_slice(&oriented);
        }
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

/// Nanite's strip decoder can yield a small number of triangles with the
/// opposite winding from the rest of a cluster. Conventional mesh viewers and
/// exporters then cull those isolated faces as holes. Restore Unreal's
/// clockwise front-face convention by comparing each geometric face normal
/// with its decoded vertex normals.
fn orient_nanite_triangle(
    vertices: &[StaticVertex],
    triangle: [u32; 3],
) -> [u32; 3] {
    let (Some(a), Some(b), Some(c)) = (
        vertices.get(triangle[0] as usize),
        vertices.get(triangle[1] as usize),
        vertices.get(triangle[2] as usize),
    ) else {
        return triangle;
    };
    let ab = [
        b.position[0] - a.position[0],
        b.position[1] - a.position[1],
        b.position[2] - a.position[2],
    ];
    let ac = [
        c.position[0] - a.position[0],
        c.position[1] - a.position[1],
        c.position[2] - a.position[2],
    ];
    let face = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    let normal = triangle.iter().fold([0.0; 3], |mut total, index| {
        if let Some(vertex) = vertices.get(*index as usize) {
            total[0] += vertex.normal[0];
            total[1] += vertex.normal[1];
            total[2] += vertex.normal[2];
        }
        total
    });
    let alignment = face[0] * normal[0] + face[1] * normal[1] + face[2] * normal[2];
    if alignment.is_finite() && alignment > 0.0 {
        [triangle[0], triangle[2], triangle[1]]
    } else {
        triangle
    }
}

#[derive(Clone, Copy)]
struct BoundaryEdge {
    key: u64,
    vertices: [u32; 2],
}

#[inline]
fn nanite_edge_key(a: u32, b: u32) -> u64 {
    let [lo, hi] = if a < b { [a, b] } else { [b, a] };
    (u64::from(lo) << 32) | u64::from(hi)
}

#[inline]
fn nanite_triangle_area_squared(vertices: &[StaticVertex], triangle: &[u32]) -> f32 {
    let [a, b, c] = [
        vertices[triangle[0] as usize].position,
        vertices[triangle[1] as usize].position,
        vertices[triangle[2] as usize].position,
    ];
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ac = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
    let cross = [
        ab[1] * ac[2] - ab[2] * ac[1],
        ab[2] * ac[0] - ab[0] * ac[2],
        ab[0] * ac[1] - ab[1] * ac[0],
    ];
    cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]
}

#[inline]
fn nanite_uv_distance_squared(vertices: &[StaticVertex], a: u32, b: u32) -> f32 {
    let a = vertices[a as usize].uv;
    let b = vertices[b as usize].uv;
    let du = a[0] - b[0];
    let dv = a[1] - b[1];
    du * du + dv * dv
}

fn interpolate_nanite_vertex(a: &StaticVertex, b: &StaticVertex, t: f32) -> StaticVertex {
    let position =
        std::array::from_fn(|axis| a.position[axis] + (b.position[axis] - a.position[axis]) * t);
    let mut normal =
        std::array::from_fn(|axis| a.normal[axis] + (b.normal[axis] - a.normal[axis]) * t);
    let normal_length = normal
        .iter()
        .map(|component| component * component)
        .sum::<f32>()
        .sqrt();
    if normal_length > 0.0 {
        for component in &mut normal {
            *component /= normal_length;
        }
    }
    let uv = std::array::from_fn(|axis| a.uv[axis] + (b.uv[axis] - a.uv[axis]) * t);
    StaticVertex {
        position,
        normal,
        uv,
    }
}

fn clip_nanite_uv_polygon(
    polygon: &[StaticVertex],
    axis: usize,
    boundary: f32,
    keep_greater: bool,
) -> Vec<StaticVertex> {
    let Some(mut previous) = polygon.last() else {
        return Vec::new();
    };
    let mut previous_inside = if keep_greater {
        previous.uv[axis] >= boundary
    } else {
        previous.uv[axis] <= boundary
    };
    let mut clipped = Vec::with_capacity(polygon.len() + 1);
    for current in polygon {
        let current_inside = if keep_greater {
            current.uv[axis] >= boundary
        } else {
            current.uv[axis] <= boundary
        };
        if current_inside != previous_inside {
            let denominator = current.uv[axis] - previous.uv[axis];
            if denominator != 0.0 {
                let t = (boundary - previous.uv[axis]) / denominator;
                clipped.push(interpolate_nanite_vertex(previous, current, t));
            }
        }
        if current_inside {
            clipped.push(current.clone());
        }
        previous = current;
        previous_inside = current_inside;
    }
    clipped
}

fn split_nanite_negative_uv_wraps(
    vertices: &mut Vec<StaticVertex>,
    indices: &mut Vec<u32>,
) -> usize {
    let original_indices = std::mem::take(indices);
    indices.reserve(original_indices.len());
    let mut split_triangles = 0usize;

    for triangle in original_indices.chunks_exact(3) {
        let source =
            [triangle[0], triangle[1], triangle[2]].map(|index| vertices[index as usize].clone());
        if nanite_triangle_area_squared(vertices, triangle) <= 1.0e-12 {
            indices.extend_from_slice(triangle);
            continue;
        }
        let mut polygons = vec![source.to_vec()];
        let mut split = false;

        for axis in 0..2 {
            let minimum = source
                .iter()
                .map(|vertex| vertex.uv[axis])
                .fold(f32::INFINITY, f32::min);
            let maximum = source
                .iter()
                .map(|vertex| vertex.uv[axis])
                .fold(f32::NEG_INFINITY, f32::max);
            // Negative coordinates cannot name a standard UDIM tile. When a
            // face spans more than one whole negative tile, UE's wrap sampler
            // renders it correctly but DCC UV editors draw a long spoke. Split
            // at each integer boundary and move every piece into 0..1. This is
            // sampling-equivalent for the repeated negative range while real
            // non-negative UDIM coordinates remain completely untouched.
            if !minimum.is_finite()
                || !maximum.is_finite()
                || minimum >= -1.0
                || maximum >= 0.0
                || maximum - minimum <= 1.0
            {
                continue;
            }

            let first_band = minimum.floor() as i32;
            let last_band = maximum.floor() as i32;
            let mut banded = Vec::new();
            for polygon in polygons {
                for band in first_band..=last_band {
                    let lower = band as f32;
                    let upper = lower + 1.0;
                    let clipped = clip_nanite_uv_polygon(&polygon, axis, lower, true);
                    let mut clipped = clip_nanite_uv_polygon(&clipped, axis, upper, false);
                    if clipped.len() < 3 {
                        continue;
                    }
                    for vertex in &mut clipped {
                        vertex.uv[axis] -= lower;
                    }
                    banded.push(clipped);
                }
            }
            polygons = banded;
            split = true;
        }

        if !split {
            indices.extend_from_slice(triangle);
            continue;
        }

        split_triangles += 1;
        for polygon in polygons {
            let base = vertices.len() as u32;
            let polygon_len = polygon.len();
            vertices.extend(polygon);
            for corner in 1..polygon_len - 1 {
                indices.extend_from_slice(&[base, base + corner as u32, base + corner as u32 + 1]);
            }
        }
    }
    split_triangles
}

fn select_nanite_hole_vertices(
    vertices: &[StaticVertex],
    canonical_vertices: &[u32],
    hole: [u32; 3],
    boundary_edges: &HashMap<u64, [u32; 2]>,
    placeholder: Option<&[u32]>,
) -> [u32; 3] {
    let mut candidates = [Vec::<u32>::new(), Vec::new(), Vec::new()];
    let hole_index = |canonical: u32| hole.iter().position(|&value| value == canonical);

    for [a, b] in [[hole[0], hole[1]], [hole[1], hole[2]], [hole[2], hole[0]]] {
        let edge = boundary_edges[&nanite_edge_key(a, b)];
        for vertex in edge {
            let canonical = canonical_vertices[vertex as usize];
            if let Some(index) = hole_index(canonical) {
                candidates[index].push(vertex);
            }
        }
    }
    if let Some(placeholder) = placeholder {
        for &vertex in placeholder {
            let canonical = canonical_vertices[vertex as usize];
            if let Some(index) = hole_index(canonical) {
                candidates[index].push(vertex);
            }
        }
    }
    for candidates in &mut candidates {
        candidates.sort_unstable();
        candidates.dedup();
    }

    let mut best = [candidates[0][0], candidates[1][0], candidates[2][0]];
    let mut best_score = (f32::INFINITY, usize::MAX);
    for &a in &candidates[0] {
        for &b in &candidates[1] {
            for &c in &candidates[2] {
                let selected = [a, b, c];
                let mut uv_mismatch = 0.0;
                for [left, right] in [[0usize, 1usize], [1, 2], [2, 0]] {
                    let edge = boundary_edges[&nanite_edge_key(hole[left], hole[right])];
                    let expected_left = if canonical_vertices[edge[0] as usize] == hole[left] {
                        edge[0]
                    } else {
                        edge[1]
                    };
                    let expected_right = if canonical_vertices[edge[0] as usize] == hole[right] {
                        edge[0]
                    } else {
                        edge[1]
                    };
                    uv_mismatch +=
                        nanite_uv_distance_squared(vertices, selected[left], expected_left);
                    uv_mismatch +=
                        nanite_uv_distance_squared(vertices, selected[right], expected_right);
                }
                let placeholder_changes = placeholder.map_or(0, |placeholder| {
                    selected
                        .iter()
                        .filter(|&&vertex| !placeholder.contains(&vertex))
                        .count()
                });
                let score = (uv_mismatch, placeholder_changes);
                if score.0 < best_score.0 || (score.0 == best_score.0 && score.1 < best_score.1) {
                    best = selected;
                    best_score = score;
                }
            }
        }
    }
    best
}

/// Repair the isolated one-triangle boundary loops produced when the Nanite
/// strip decoder emits a zero-area placeholder instead of the cluster's real
/// face. Only closed three-edge loops are considered: larger authored openings
/// remain untouched. A matching zero-area triangle is replaced where possible.
/// Unmatched loops are filled only when a large mesh overwhelmingly exhibits
/// the same decoder failure, which avoids closing an isolated authored opening.
fn repair_nanite_triangular_holes(vertices: &[StaticVertex], indices: &mut Vec<u32>) -> usize {
    if vertices.len() < 3 || indices.len() < 9 {
        return 0;
    }

    let mut position_ids = HashMap::<[u32; 3], u32>::new();
    let mut canonical_vertices = Vec::with_capacity(vertices.len());
    for vertex in vertices {
        let key = vertex
            .position
            .map(|value| if value == 0.0 { 0 } else { value.to_bits() });
        let canonical = match position_ids.get(&key) {
            Some(&canonical) => canonical,
            None => {
                let canonical = position_ids.len() as u32;
                position_ids.insert(key, canonical);
                canonical
            }
        };
        canonical_vertices.push(canonical);
    }

    let mut edges = Vec::<BoundaryEdge>::with_capacity(indices.len());
    for triangle in indices.chunks_exact(3) {
        let ids = [
            canonical_vertices[triangle[0] as usize],
            canonical_vertices[triangle[1] as usize],
            canonical_vertices[triangle[2] as usize],
        ];
        if ids[0] == ids[1]
            || ids[1] == ids[2]
            || ids[0] == ids[2]
            || nanite_triangle_area_squared(vertices, triangle) <= 1.0e-12
        {
            continue;
        }
        for [a, b, va, vb] in [
            [ids[0], ids[1], triangle[0], triangle[1]],
            [ids[1], ids[2], triangle[1], triangle[2]],
            [ids[2], ids[0], triangle[2], triangle[0]],
        ] {
            edges.push(BoundaryEdge {
                key: nanite_edge_key(a, b),
                vertices: [va, vb],
            });
        }
    }
    edges.sort_unstable_by_key(|edge| edge.key);

    let mut boundary_adjacency = HashMap::<u32, Vec<u32>>::new();
    let mut boundary_edge_vertices = HashMap::<u64, [u32; 2]>::new();
    let mut cursor = 0usize;
    while cursor < edges.len() {
        let edge = edges[cursor].key;
        let mut end = cursor + 1;
        while end < edges.len() && edges[end].key == edge {
            end += 1;
        }
        if end - cursor == 1 {
            let a = (edge >> 32) as u32;
            let b = edge as u32;
            boundary_adjacency.entry(a).or_default().push(b);
            boundary_adjacency.entry(b).or_default().push(a);
            boundary_edge_vertices.insert(edge, edges[cursor].vertices);
        }
        cursor = end;
    }

    let mut visited = HashSet::<u32>::new();
    let mut holes = Vec::<[u32; 3]>::new();
    for &start in boundary_adjacency.keys() {
        if !visited.insert(start) {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        let mut degree_sum = 0usize;
        while let Some(vertex) = stack.pop() {
            component.push(vertex);
            let neighbours = &boundary_adjacency[&vertex];
            degree_sum += neighbours.len();
            for &neighbour in neighbours {
                if visited.insert(neighbour) {
                    stack.push(neighbour);
                }
            }
        }
        if component.len() == 3 && degree_sum == 6 {
            component.sort_unstable();
            holes.push([component[0], component[1], component[2]]);
        }
    }
    if holes.is_empty() {
        return 0;
    }

    let mut hole_by_edge = HashMap::<u64, usize>::new();
    for (hole_index, [a, b, c]) in holes.iter().copied().enumerate() {
        hole_by_edge.insert(nanite_edge_key(a, b), hole_index);
        hole_by_edge.insert(nanite_edge_key(b, c), hole_index);
        hole_by_edge.insert(nanite_edge_key(c, a), hole_index);
    }
    let mut repaired = vec![false; holes.len()];
    for triangle in indices.chunks_exact_mut(3) {
        if nanite_triangle_area_squared(vertices, triangle) > 1.0e-12 {
            continue;
        }
        let ids = [
            canonical_vertices[triangle[0] as usize],
            canonical_vertices[triangle[1] as usize],
            canonical_vertices[triangle[2] as usize],
        ];
        let candidate = [[ids[0], ids[1]], [ids[1], ids[2]], [ids[2], ids[0]]]
            .into_iter()
            .filter(|[a, b]| a != b)
            .find_map(|[a, b]| hole_by_edge.get(&nanite_edge_key(a, b)).copied())
            .filter(|hole_index| !repaired[*hole_index]);
        let Some(hole_index) = candidate else {
            continue;
        };
        let hole = holes[hole_index];
        let replacement = select_nanite_hole_vertices(
            vertices,
            &canonical_vertices,
            hole,
            &boundary_edge_vertices,
            Some(triangle),
        );
        triangle.copy_from_slice(&replacement);
        repaired[hole_index] = true;
    }

    let directly_repaired = repaired.iter().filter(|&&repaired| repaired).count();
    let recover_unmatched = holes.len() >= 16 && directly_repaired * 10 >= holes.len() * 9;
    if recover_unmatched {
        for (hole_index, hole) in holes.iter().enumerate() {
            if repaired[hole_index] {
                continue;
            }
            indices.extend(select_nanite_hole_vertices(
                vertices,
                &canonical_vertices,
                *hole,
                &boundary_edge_vertices,
                None,
            ));
            repaired[hole_index] = true;
        }
    }
    repaired.iter().filter(|&&repaired| repaired).count()
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
    // Modern UE stores the SNORM bit pattern; rebias the sign bit before
    // unpacking it as UNORM (RenderingObjectVersion::IncreaseNormalPrecision).
    ((b ^ 0x80) as f32 / 127.5) - 1.0
}

fn decode_normal(tan: &[u8], v: usize, elem: usize, high: bool) -> [f32; 3] {
    let base = v * elem;
    if high {
        // FPackedRGBA16N: TangentX(8) + TangentZ(8), each 4×u16 → [-1,1].
        let z = base + 8;
        let c = |o: usize| {
            ((u16::from_le_bytes([tan[z + o], tan[z + o + 1]]) ^ 0x8000) as f32 / 32767.5) - 1.0
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modern_packed_normals_rebase_the_signed_bit_pattern() {
        let low_positive_x = [0, 0, 0, 0, 0x7f, 0, 0, 0];
        let normal = decode_normal(&low_positive_x, 0, 8, false);
        assert!(normal[0] > 0.999);
        assert!(normal[1].abs() < 0.01 && normal[2].abs() < 0.01);

        let mut high_positive_x = [0u8; 16];
        high_positive_x[8..10].copy_from_slice(&0x7fffu16.to_le_bytes());
        let normal = decode_normal(&high_positive_x, 0, 16, true);
        assert!(normal[0] > 0.999);
        assert!(normal[1].abs() < 0.001 && normal[2].abs() < 0.001);
    }

    #[test]
    fn nanite_triangles_are_oriented_to_unreal_clockwise_winding() {
        let mesh = super::super::nanite::NaniteMesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![[0.0; 2]; 3],
            triangles: vec![[0, 1, 2]],
            unresolved_vertices: 0,
        };
        let converted = StaticMesh::from_nanite(&mesh);
        assert_eq!(converted.indices, [0, 2, 1]);

        let already_clockwise = super::super::nanite::NaniteMesh {
            triangles: vec![[0, 2, 1]],
            ..mesh
        };
        let converted = StaticMesh::from_nanite(&already_clockwise);
        assert_eq!(converted.indices, [0, 2, 1]);
    }

    #[test]
    fn nanite_zero_area_placeholder_is_replaced_by_its_triangular_hole() {
        let mesh = super::super::nanite::NaniteMesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0; 2]; 4],
            triangles: vec![[0, 1, 3], [1, 2, 3], [2, 0, 3], [0, 1, 1]],
            unresolved_vertices: 0,
        };
        let converted = StaticMesh::from_nanite(&mesh);
        assert_eq!(converted.indices.len(), 12);
        let repaired = &converted.indices[9..12];
        assert!(repaired.contains(&0));
        assert!(repaired.contains(&1));
        assert!(repaired.contains(&2));
    }

    #[test]
    fn nanite_hole_repair_preserves_the_matching_uv_seam() {
        let mesh = super::super::nanite::NaniteMesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 5],
            uvs: vec![[-4.0, 0.0], [0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.5, 0.5]],
            triangles: vec![[1, 2, 4], [2, 3, 4], [3, 1, 4], [1, 2, 2]],
            unresolved_vertices: 0,
        };
        let converted = StaticMesh::from_nanite(&mesh);
        let repaired = &converted.indices[9..12];
        assert!(repaired.contains(&1));
        assert!(!repaired.contains(&0));
        let repaired_uvs = [repaired[0], repaired[1], repaired[2]]
            .map(|index| converted.vertices[index as usize].uv);
        for [a, b] in [[0usize, 1usize], [1, 2], [2, 0]] {
            let du = repaired_uvs[a][0] - repaired_uvs[b][0];
            let dv = repaired_uvs[a][1] - repaired_uvs[b][1];
            assert!(du * du + dv * dv <= 2.0);
        }
    }

    #[test]
    fn nanite_negative_wrap_spans_are_split_into_local_uv_islands() {
        let mut vertices = vec![
            StaticVertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [-0.48, 0.82],
            },
            StaticVertex {
                position: [0.0, 46.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [-2.52, 0.82],
            },
            StaticVertex {
                position: [1.0, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [-0.55, 0.88],
            },
        ];
        let mut indices = vec![0, 1, 2];

        assert_eq!(
            split_nanite_negative_uv_wraps(&mut vertices, &mut indices),
            1
        );
        assert!(indices.len() > 3);
        for triangle in indices.chunks_exact(3) {
            let u =
                [triangle[0], triangle[1], triangle[2]].map(|index| vertices[index as usize].uv[0]);
            let minimum = u.iter().copied().fold(f32::INFINITY, f32::min);
            let maximum = u.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            assert!(minimum >= -1.0e-6);
            assert!(maximum <= 1.0 + 1.0e-6);
            assert!(maximum - minimum <= 1.0 + 1.0e-6);
            assert!(nanite_triangle_area_squared(&vertices, triangle) > 1.0e-12);
        }
    }

    #[test]
    fn nanite_nonnegative_udim_spans_are_not_changed() {
        let mut vertices = vec![
            StaticVertex {
                position: [0.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.2, 0.2],
            },
            StaticVertex {
                position: [1.0, 0.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [2.3, 0.2],
            },
            StaticVertex {
                position: [0.0, 1.0, 0.0],
                normal: [0.0, 0.0, 1.0],
                uv: [0.2, 0.8],
            },
        ];
        let mut indices = vec![0, 1, 2];

        assert_eq!(
            split_nanite_negative_uv_wraps(&mut vertices, &mut indices),
            0
        );
        assert_eq!(vertices.len(), 3);
        assert_eq!(indices, [0, 1, 2]);
    }

    #[test]
    fn nanite_repair_does_not_fill_a_larger_authored_opening() {
        let mesh = super::super::nanite::NaniteMesh {
            positions: vec![
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 5],
            uvs: vec![[0.0; 2]; 5],
            triangles: vec![[0, 1, 4], [1, 2, 4], [2, 3, 4], [3, 0, 4]],
            unresolved_vertices: 0,
        };
        let converted = StaticMesh::from_nanite(&mesh);
        assert_eq!(converted.indices.len(), 12);
    }

    #[test]
    fn nanite_repair_does_not_fill_an_unmatched_triangular_opening() {
        let mesh = super::super::nanite::NaniteMesh {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 0.0, 1.0],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 4],
            uvs: vec![[0.0; 2]; 4],
            triangles: vec![[0, 1, 3], [1, 2, 3], [2, 0, 3]],
            unresolved_vertices: 0,
        };
        let converted = StaticMesh::from_nanite(&mesh);
        assert_eq!(converted.indices.len(), 9);
    }
}
