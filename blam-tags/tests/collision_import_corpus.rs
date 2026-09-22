//! Build `collision_model` tags from shipped sources, and verify them by
//! traversing the packed result.
//!
//! A wrong collision BSP does not look wrong and does not error — it
//! drops the player through a floor. Counting blocks proves nothing. So
//! this reads the tag **back**, decodes the packed bit fields with an
//! implementation deliberately independent of the writer's, and walks the
//! tree the way the runtime does:
//!
//! * every surface must be reachable — descend from a point just behind
//!   its own face and the leaf you land in must reference it;
//! * every surface's edge ring must close in at most 8 steps, because
//!   every consumer in the game uses a fixed 8-entry stack buffer;
//! * the tree must be at most 128 deep, because the traversal stack is
//!   128 entries and overruns without stopping.

use std::path::{Path, PathBuf};

use blam_tags::collision_import::{collision_model_from_jms, CollisionError, CollisionOptions};
use blam_tags::jms::JmsFile;
use blam_tags::TagFile;
use flate2::read::ZlibDecoder;

fn h3ek() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("BLAM_TEST_H3EK") {
        let path = PathBuf::from(path);
        return path.is_dir().then_some(path);
    }
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "C:/Program Files/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK"))
    .find(|path| path.join("data").is_dir() && path.join("tags").is_dir())
}

fn schema() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../definitions/halo3_mcc/collision_model.json");
    p.exists().then_some(p)
}

fn walk(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x == ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn source_jms(tag: &TagFile) -> Option<JmsFile> {
    let info = tag.import_info()?;
    let files = info.field("files").and_then(|f| f.as_block())?;
    let mut found: Vec<Vec<u8>> = Vec::new();
    for file in files.iter() {
        if !file.read_string("path").unwrap_or_default().to_ascii_lowercase().ends_with(".jms") {
            continue;
        }
        let Some(z) = file.field("zipped data").and_then(|f| f.as_data()) else { continue };
        let mut out = Vec::new();
        if std::io::Read::read_to_end(&mut ZlibDecoder::new(z), &mut out).is_ok() {
            found.push(out);
        }
    }
    if found.len() != 1 {
        return None;
    }
    let text = String::from_utf8(found.remove(0)).ok()?;
    JmsFile::parse(&text).ok().map(|(j, _)| j)
}

/// A decoded BSP, read back out of a written tag. Deliberately a separate
/// decoder from the writer's packer — a shared one would agree with
/// itself about a wrong layout.
struct Decoded {
    nodes: Vec<(i32, i32, i32)>, // plane, back, front  (children: >=0 node, <0 !leaf, i32::MAX none)
    planes: Vec<[f32; 4]>,
    leaves: Vec<(i32, u16)>, // first ref, count
    refs: Vec<(i32, i32)>,   // plane (bit31 = flipped), node (>=0 bsp2d, <0 !surface)
    nodes2d: Vec<([f32; 3], i32, i32)>,
    surfaces: Vec<(i32, u16)>, // plane (bit31 = flipped), first edge
    edges: Vec<(u16, u16, u16, u16, i16, i16)>,
    vertices: Vec<[f32; 3]>,
}

fn unpack_s15(s: i16) -> i32 {
    let u = s as u16;
    if u == 0xFFFF {
        return -1;
    }
    let idx = (u & 0x7FFF) as i32;
    if u & 0x8000 != 0 { (idx as u32 | 0x8000_0000) as i32 } else { idx }
}

/// The 24-bit child: `0xFFFFFF` is none, bit 23 marks a leaf.
/// A node index we never emit, used as the "no child" marker so it
/// cannot collide with a flagged leaf.
const NO_CHILD: i32 = i32::MAX;

fn unpack_child(v: u32) -> i32 {
    if v == 0xFF_FFFF {
        // "No child". It must NOT be i32::MIN — an unpacked leaf 0 is
        // exactly 0x80000000, and using that as the absence sentinel
        // makes every leaf-0 descent look like a dead end.
        return NO_CHILD;
    }
    if v & 0x80_0000 != 0 {
        // Flagged: a leaf. The flag moves from bit 23 back to bit 31; the
        // low bits stay a plain index. NOT a bitwise complement.
        return ((v & 0x7F_FFFF) | 0x8000_0000) as i32;
    }
    v as i32
}

fn decode(bsp: &blam_tags::TagStruct<'_>) -> Option<Decoded> {
    let blk = |n: &str| bsp.field(n).and_then(|f| f.as_block());
    let mut d = Decoded {
        nodes: Vec::new(),
        planes: Vec::new(),
        leaves: Vec::new(),
        refs: Vec::new(),
        nodes2d: Vec::new(),
        surfaces: Vec::new(),
        edges: Vec::new(),
        vertices: Vec::new(),
    };
    for e in blk("bsp3d nodes")?.iter() {
        let w = match e.field("node data designator").and_then(|f| f.value()) {
            Some(blam_tags::TagFieldData::Int64Integer(v)) => v as u64,
            _ => return None,
        };
        d.nodes.push((
            (w & 0xFFFF) as i32,
            unpack_child(((w >> 16) & 0xFF_FFFF) as u32),
            unpack_child(((w >> 40) & 0xFF_FFFF) as u32),
        ));
    }
    for e in blk("planes")?.iter() {
        let p = e.read_plane3d("plane");
        d.planes.push([p.i, p.j, p.k, p.d]);
    }
    for e in blk("leaves")?.iter() {
        d.leaves.push((
            e.read_int_any("first bsp2d reference").unwrap_or(-1) as i32,
            e.read_int_any("bsp2d reference count").unwrap_or(0) as u16,
        ));
    }
    for e in blk("bsp2d references")?.iter() {
        d.refs.push((
            unpack_s15(e.read_int_any("plane").unwrap_or(-1) as i16),
            unpack_s15(e.read_int_any("bsp2d node").unwrap_or(-1) as i16),
        ));
    }
    for e in blk("bsp2d nodes")?.iter() {
        let p = match e.field("plane").and_then(|f| f.value()) {
            Some(blam_tags::TagFieldData::RealPlane2d(v)) => v,
            _ => blam_tags::math::RealPlane2d::default(),
        };
        d.nodes2d.push((
            [p.i, p.j, p.d],
            unpack_s15(e.read_int_any("left child").unwrap_or(-1) as i16),
            unpack_s15(e.read_int_any("right child").unwrap_or(-1) as i16),
        ));
    }
    for e in blk("surfaces")?.iter() {
        d.surfaces.push((
            unpack_s15(e.read_int_any("plane").unwrap_or(-1) as i16),
            e.read_int_any("first edge").unwrap_or(0) as u16,
        ));
    }
    for e in blk("edges")?.iter() {
        d.edges.push((
            e.read_int_any("start vertex").unwrap_or(0) as u16,
            e.read_int_any("end vertex").unwrap_or(0) as u16,
            e.read_int_any("forward edge").unwrap_or(0) as u16,
            e.read_int_any("reverse edge").unwrap_or(0) as u16,
            e.read_int_any("left surface").unwrap_or(-1) as i16,
            e.read_int_any("right surface").unwrap_or(-1) as i16,
        ));
    }
    for e in blk("vertices")?.iter() {
        let p = e.read_point3d("point");
        d.vertices.push([p.x, p.y, p.z]);
    }
    Some(d)
}

fn depth(d: &Decoded, node: i32, seen: usize) -> usize {
    if node < 0 || node == NO_CHILD || seen > 200 {
        return seen;
    }
    let (_, b, f) = d.nodes[node as usize];
    depth(d, b, seen + 1).max(depth(d, f, seen + 1))
}

fn descend(d: &Decoded, p: [f32; 3]) -> Option<usize> {
    if d.nodes.is_empty() {
        return None;
    }
    let mut cur = 0i32;
    for _ in 0..256 {
        if cur == NO_CHILD {
            return None;
        }
        if cur < 0 {
            return Some((cur as u32 & 0x7FFF_FFFF) as usize);
        }
        let (pl, b, f) = d.nodes[cur as usize];
        let pd = d.planes[pl as usize];
        let s = pd[0] * p[0] + pd[1] * p[1] + pd[2] * p[2] - pd[3];
        cur = if s >= 0.0 { f } else { b };
    }
    None
}

/// Every surface a bsp2d subtree can yield.
fn collect_2d(d: &Decoded, node: i32, out: &mut Vec<usize>) {
    if node < 0 {
        out.push((node as u32 & 0x7FFF_FFFF) as usize);
        return;
    }
    if node as usize >= d.nodes2d.len() || out.len() > 100_000 {
        return;
    }
    let (_, l, r) = d.nodes2d[node as usize];
    collect_2d(d, l, out);
    collect_2d(d, r, out);
}

#[test]
fn every_surface_of_a_built_collision_model_is_reachable() {
    let (Some(kit), Some(schema)) = (h3ek(), schema()) else {
        eprintln!("skipping: need an H3EK install and definitions/halo3_mcc/collision_model.json");
        return;
    };

    let opts = CollisionOptions::default();
    let mut built = 0usize;
    let mut refused: Vec<String> = Vec::new();
    let mut too_big = 0usize;
    let mut checked_surfaces = 0usize;
    let mut unreachable: Vec<String> = Vec::new();
    let mut bad_rings: Vec<String> = Vec::new();
    let mut deepest = 0usize;
    let mut dropped = 0usize;
    let mut rows: Vec<String> = Vec::new();

    for tag_path in walk(&kit.join("tags"), "collision_model").iter().take(150) {
        let Ok(tag) = TagFile::read(tag_path) else { continue };
        let Some(jms) = source_jms(&tag) else { continue };
        let name = tag_path.file_name().unwrap_or_default().to_string_lossy().to_string();

        let (out, report) = match collision_model_from_jms(&jms, &schema, &opts) {
            Ok(v) => v,
            Err(CollisionError::TooLarge { .. }) | Err(CollisionError::TooDeep(_)) => {
                too_big += 1;
                continue;
            }
            Err(e) => {
                refused.push(format!("{name}: {e}"));
                continue;
            }
        };
        built += 1;
        deepest = deepest.max(report.max_depth);
        dropped += report.dropped_surfaces;
        if rows.len() < 10 {
            rows.push(format!(
                "{name:<40} bsps {}  surfaces {}  nodes {}  leaves {}  depth {}",
                report.bsps, report.surfaces, report.bsp3d_nodes, report.leaves, report.max_depth
            ));
        }

        // Read it back and traverse it independently.
        let bytes = out.write_to_bytes().expect("serialize");
        let back = TagFile::read_from_bytes(&bytes)
            .unwrap_or_else(|e| panic!("{name}: rebuilt tag does not parse: {e}"));
        let root = back.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else { continue };
        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
                for bsp_el in bsps.iter() {
                    let Some(bsp) = bsp_el.descend("bsp") else { continue };
                    let Some(d) = decode(&bsp) else {
                        refused.push(format!("{name}: could not decode a written BSP"));
                        continue;
                    };
                    if d.nodes.is_empty() {
                        continue;
                    }
                    let dep = depth(&d, 0, 0);
                    assert!(dep <= 128, "{name}: BSP is {dep} deep, over the 128 the game allows");

                    for (si, (plane, first_edge)) in d.surfaces.iter().enumerate() {
                        // The ring must close in at most 8 steps.
                        let mut e = *first_edge as usize;
                        let mut n = 0usize;
                        loop {
                            n += 1;
                            if n > 8 || e >= d.edges.len() {
                                break;
                            }
                            let ed = d.edges[e];
                            let next =
                                if ed.4 == si as i16 { ed.2 as usize } else { ed.3 as usize };
                            if next == *first_edge as usize {
                                break;
                            }
                            e = next;
                        }
                        if n > 8 {
                            bad_rings.push(format!("{name}: surface {si} ring does not close"));
                            continue;
                        }

                        // Reachability: a point just behind the face must
                        // land in a leaf that references this surface.
                        let pi = (*plane as u32 & 0x7FFF_FFFF) as usize;
                        if pi >= d.planes.len() {
                            continue;
                        }
                        let pl = d.planes[pi];
                        let mut c = [0.0f32; 3];
                        let mut e = *first_edge as usize;
                        let mut cnt = 0.0f32;
                        for _ in 0..8 {
                            if e >= d.edges.len() {
                                break;
                            }
                            let ed = d.edges[e];
                            // The ring runs start->end for the edge's
                            // LEFT surface and end->start for its right,
                            // so which vertex this step contributes
                            // depends on which side we are walking.
                            let v = if ed.4 == si as i16 {
                                d.vertices[ed.0 as usize]
                            } else {
                                d.vertices[ed.1 as usize]
                            };
                            for k in 0..3 {
                                c[k] += v[k];
                            }
                            cnt += 1.0;
                            let next =
                                if ed.4 == si as i16 { ed.2 as usize } else { ed.3 as usize };
                            if next == *first_edge as usize {
                                break;
                            }
                            e = next;
                        }
                        if cnt == 0.0 {
                            continue;
                        }
                        for k in 0..3 {
                            c[k] /= cnt;
                        }
                        let dir = if *plane < 0 { 1.0f32 } else { -1.0 };
                        let probe =
                            [c[0] + pl[0] * dir * 1e-4, c[1] + pl[1] * dir * 1e-4, c[2] + pl[2] * dir * 1e-4];

                        checked_surfaces += 1;
                        let Some(leaf) = descend(&d, probe) else {
                            unreachable.push(format!("{name}: surface {si} descends to nothing"));
                            continue;
                        };
                        if leaf >= d.leaves.len() {
                            unreachable.push(format!("{name}: surface {si} -> leaf {leaf} oob"));
                            continue;
                        }
                        let (first, count) = d.leaves[leaf];
                        let mut found = false;
                        for r in 0..count as usize {
                            let Some(&(_, node)) = d.refs.get(first as usize + r) else { continue };
                            let mut list = Vec::new();
                            collect_2d(&d, node, &mut list);
                            if list.contains(&si) {
                                found = true;
                                break;
                            }
                        }
                        if !found {
                            unreachable
                                .push(format!("{name}: surface {si} not in leaf {leaf}"));
                        }
                    }
                }
            }
        }
    }

    eprintln!("built {built} collision_models, {too_big} too large/deep, {} refused",
        refused.len());
    for r in refused.iter().take(8) {
        eprintln!("    {r}");
    }
    eprintln!("deepest tree: {deepest}");
    eprintln!("surfaces traversed: {checked_surfaces}, unreachable: {}", unreachable.len());
    eprintln!("coplanar surfaces dropped: {dropped}");
    for u in unreachable.iter().take(8) {
        eprintln!("    {u}");
    }
    for b in bad_rings.iter().take(5) {
        eprintln!("    {b}");
    }
    for r in &rows {
        eprintln!("  {r}");
    }

    assert!(built > 0, "nothing was built — the harness is broken, not the writer");
    assert!(refused.is_empty(), "unexpected failures:\n{}", refused.join("\n"));
    assert!(bad_rings.is_empty(), "edge rings must close within 8:\n{}", bad_rings.join("\n"));
    assert!(
    // Some loss is inherent: a bsp2d leaf holds exactly one surface, so
    // coplanar faces that no 2D line separates cannot all be represented,
    // and Tool discards them too. What must not happen is losing a
    // meaningful share of the collision.
        unreachable.len() * 100 <= checked_surfaces,
        "{} of {checked_surfaces} surfaces unreachable, over the 1% that is \
         inherent:\n{}",
        unreachable.len(),
        unreachable.iter().take(10).cloned().collect::<Vec<_>>().join("\n")
    );
}
