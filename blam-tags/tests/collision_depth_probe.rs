//! What depth do Tool's own collision trees actually reach?
//!
//! `collision_import` refuses a tree deeper than `MAX_DEPTH`. That
//! constant is worth nothing unless it matches what shipped tags do, and
//! the builder's own depth is worth nothing unless it is in the same
//! range as Tool's on the same model. This measures both, from the
//! shipped `coll` tags and the JMS baked into their `info` stream.
//!
//! Diagnostic, not an assertion — run it with `--nocapture`.

use std::path::{Path, PathBuf};

use blam_tags::TagFile;

fn h3ek() -> Option<PathBuf> {
    [
        "D:/SteamLibrary/steamapps/common",
        "C:/Program Files (x86)/Steam/steamapps/common",
        "E:/SteamLibrary/steamapps/common",
    ]
    .iter()
    .map(|root| PathBuf::from(root).join("H3EK"))
    .find(|path| path.join("tags").is_dir())
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

/// A shipped `bsp3d_node`: plane in bits 0..16, back in 16..40, front in
/// 40..64, each child flagged in its top bit.
fn children(packed: i64) -> (i64, i64) {
    let v = packed as u64;
    let back = (v >> 16) & 0xFF_FFFF;
    let front = (v >> 40) & 0xFF_FFFF;
    let un = |c: u64| -> i64 {
        if c == 0xFF_FFFF {
            -1 // no child
        } else if c & 0x80_0000 != 0 {
            -2 // a leaf: depth stops here
        } else {
            c as i64
        }
    };
    (un(back), un(front))
}

fn depth_of(nodes: &[i64]) -> usize {
    let mut memo: Vec<u32> = vec![u32::MAX; nodes.len()];
    let mut stack: Vec<(usize, bool)> = vec![(0, false)];
    while let Some((at, expanded)) = stack.pop() {
        if at >= nodes.len() {
            continue;
        }
        let (b, f) = children(nodes[at]);
        if !expanded {
            if memo[at] != u32::MAX {
                continue;
            }
            memo[at] = 0; // marks "in progress", so a cycle cannot loop
            stack.push((at, true));
            for c in [b, f] {
                if c >= 0 && (c as usize) < nodes.len() && memo[c as usize] == u32::MAX {
                    stack.push((c as usize, false));
                }
            }
        } else {
            let d = |c: i64| -> u32 {
                if c >= 0 && (c as usize) < nodes.len() {
                    memo[c as usize]
                } else {
                    0
                }
            };
            memo[at] = 1 + d(b).max(d(f));
        }
    }
    memo.first().copied().unwrap_or(0) as usize
}

#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn how_deep_are_tools_own_collision_trees() {
    let Some(ek) = h3ek() else {
        eprintln!("no H3EK; skipping");
        return;
    };

    let mut rows: Vec<(usize, usize, String)> = Vec::new();
    let mut over_128 = 0usize;

    for path in walk(&ek.join("tags"), "collision_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
            continue;
        };

        let mut deepest = 0usize;
        let mut surfaces = 0usize;
        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
                for bsp_el in bsps.iter() {
                    let Some(bsp) = bsp_el.descend("bsp") else { continue };
                    let Some(n3) = bsp.field("bsp3d nodes").and_then(|f| f.as_block()) else {
                        continue;
                    };
                    let nodes: Vec<i64> =
                        n3.iter().filter_map(|n| n.read_int_any("node data designator").map(|v| v as i64)).collect();
                    if let Some(s) = bsp.field("surfaces").and_then(|f| f.as_block()) {
                        surfaces += s.len();
                    }
                    if !nodes.is_empty() {
                        deepest = deepest.max(depth_of(&nodes));
                    }
                }
            }
        }
        if deepest > 0 {
            if deepest > 128 {
                over_128 += 1;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            rows.push((deepest, surfaces, name));
        }
    }

    rows.sort_unstable();
    rows.reverse();
    println!("{} shipped collision_models with a bsp3d tree", rows.len());
    println!("{over_128} of them are deeper than 128");
    println!("deepest 25:");
    for (d, s, name) in rows.iter().take(25) {
        println!("  depth {d:4}  surfaces {s:6}  {name}");
    }
    if let Some(med) = rows.get(rows.len() / 2) {
        println!("median depth {}", med.0);
    }
}

/// Are Tool's collision surfaces triangles, or merged polygons?
///
/// A `surfaces_block` entry names one `first edge`; the ring is closed by
/// following each edge's forward or reverse link depending on which side
/// this surface sits. If Tool merged coplanar neighbours the rings will
/// be longer than three.
#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn are_tools_collision_surfaces_triangles() {
    let Some(ek) = h3ek() else {
        eprintln!("no H3EK; skipping");
        return;
    };

    let mut hist = [0usize; 12]; // ring length, 0..11, 11 meaning "longer"
    let mut surfaces_total = 0usize;
    let mut guardian: Option<(usize, [usize; 12])> = None;

    for path in walk(&ek.join("tags"), "collision_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
            continue;
        };
        let mut here = [0usize; 12];
        let mut here_total = 0usize;

        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
                for bsp_el in bsps.iter() {
                    let Some(bsp) = bsp_el.descend("bsp") else { continue };
                    let (Some(surfaces), Some(edges)) = (
                        bsp.field("surfaces").and_then(|f| f.as_block()),
                        bsp.field("edges").and_then(|f| f.as_block()),
                    ) else {
                        continue;
                    };
                    // (forward, reverse, left surface)
                    let e: Vec<(i64, i64, i64)> = edges
                        .iter()
                        .map(|x| {
                            (
                                x.read_int_any("forward edge").unwrap_or(-1) as i64,
                                x.read_int_any("reverse edge").unwrap_or(-1) as i64,
                                x.read_int_any("left surface").unwrap_or(-1) as i64,
                            )
                        })
                        .collect();
                    for (si, sf) in surfaces.iter().enumerate() {
                        let Some(first) = sf.read_int_any("first edge") else { continue };
                        let first = first as i64;
                        if first < 0 || first as usize >= e.len() {
                            continue;
                        }
                        let mut at = first;
                        let mut len = 0usize;
                        loop {
                            len += 1;
                            if len > 32 || at < 0 || at as usize >= e.len() {
                                break;
                            }
                            let (fwd, rev, left) = e[at as usize];
                            at = if left == si as i64 { fwd } else { rev };
                            if at == first {
                                break;
                            }
                        }
                        here_total += 1;
                        here[len.min(11)] += 1;
                    }
                }
            }
        }
        surfaces_total += here_total;
        for i in 0..12 {
            hist[i] += here[i];
        }
        if path.file_name().is_some_and(|f| f == "guardian.collision_model") {
            guardian = Some((here_total, here));
        }
    }

    println!("{surfaces_total} shipped collision surfaces");
    for (n, c) in hist.iter().enumerate() {
        if *c > 0 {
            let label = if n == 11 { ">10".to_string() } else { n.to_string() };
            println!("  ring {label:>3}: {c:8}  ({:.1}%)", 100.0 * *c as f64 / surfaces_total as f64);
        }
    }
    if let Some((total, h)) = guardian {
        println!("guardian.collision_model: {total} surfaces");
        for (n, c) in h.iter().enumerate() {
            if *c > 0 {
                println!("  ring {n:>3}: {c}");
            }
        }
    }
}

/// How many distinct planes does Tool keep per surface?
///
/// Depth is driven by planes, not surfaces: a node consumes one plane, so
/// a path can be no longer than the planes below it. If Tool's plane
/// count per surface is well under one, its dedup is looser than
/// matching normals to 0.9999 and offsets to 1e-5.
#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn how_many_planes_does_tool_keep() {
    let Some(ek) = h3ek() else {
        eprintln!("no H3EK; skipping");
        return;
    };
    let mut rows: Vec<(String, usize, usize, usize)> = Vec::new();
    for path in walk(&ek.join("tags"), "collision_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
            continue;
        };
        let (mut planes, mut surfaces, mut nodes) = (0usize, 0usize, 0usize);
        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
                for bsp_el in bsps.iter() {
                    let Some(bsp) = bsp_el.descend("bsp") else { continue };
                    let n = |f: &str| {
                        bsp.field(f).and_then(|x| x.as_block()).map(|b| b.len()).unwrap_or(0)
                    };
                    planes += n("planes");
                    surfaces += n("surfaces");
                    nodes += n("bsp3d nodes");
                }
            }
        }
        if surfaces > 0 {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            rows.push((name, planes, surfaces, nodes));
        }
    }
    let tp: usize = rows.iter().map(|r| r.1).sum();
    let ts: usize = rows.iter().map(|r| r.2).sum();
    println!("corpus: {tp} planes for {ts} surfaces = {:.3} planes/surface", tp as f64 / ts as f64);
    rows.sort_by_key(|r| std::cmp::Reverse(r.2));
    println!("largest by surface count:");
    for (name, p, s, n) in rows.iter().take(10) {
        println!("  {name:<44} planes {p:6}  surfaces {s:6}  nodes {n:6}  ({:.3} p/s)",
                 *p as f64 / *s as f64);
    }
    for (name, p, s, n) in rows.iter() {
        if name.starts_with("guardian") || name.starts_with("soccer") {
            println!("  {name:<44} planes {p:6}  surfaces {s:6}  nodes {n:6}  ({:.3} p/s)",
                     *p as f64 / *s as f64);
        }
    }
}

/// How many BSPs does Tool put in one permutation?
///
/// `collision_import` keys a BSP on (region, permutation, node), so a
/// skinned character gets one per bone. If Tool instead writes a single
/// BSP per permutation then that grouping is wrong, and the thin
/// scattered bands it produces are what the tree is choking on.
#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn how_many_bsps_per_permutation() {
    let Some(ek) = h3ek() else {
        eprintln!("no H3EK; skipping");
        return;
    };
    let mut hist: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut perms_total = 0usize;
    for path in walk(&ek.join("tags"), "collision_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
            continue;
        };
        let mut here: Vec<usize> = Vec::new();
        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let n = perm.field("bsps").and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0);
                *hist.entry(n).or_default() += 1;
                perms_total += 1;
                here.push(n);
            }
        }
        if path.file_name().is_some_and(|f| f == "guardian.collision_model") {
            println!("guardian.collision_model: {} permutations, bsps each {:?}", here.len(), here);
        }
    }
    println!("{perms_total} shipped permutations, bsps per permutation:");
    for (n, c) in &hist {
        println!("  {n:>3} bsps: {c:6}  ({:.1}%)", 100.0 * *c as f64 / perms_total as f64);
    }
}

/// How many bsp2d references does Tool put in one leaf?
///
/// A convex body has no face plane that separates its other faces, so an
/// autopartition over face planes alone degenerates into a list as long
/// as the face count — which is what `collision_import` does to
/// guardian's 180-triangle shield. Stopping at a convex cell and letting
/// one leaf hold the lot fixes the depth, but only if leaves that size
/// are something the format and the runtime actually see.
#[test]
#[ignore = "measures the shipped corpus; run with --ignored"]
fn how_big_do_tools_leaves_get() {
    let Some(ek) = h3ek() else {
        eprintln!("no H3EK; skipping");
        return;
    };
    let mut hist: std::collections::BTreeMap<usize, usize> = std::collections::BTreeMap::new();
    let mut leaves_total = 0usize;
    let mut biggest: Vec<(usize, String)> = Vec::new();

    for path in walk(&ek.join("tags"), "collision_model") {
        let Ok(tag) = TagFile::read(&path) else { continue };
        let root = tag.root();
        let Some(regions) = root.field_path("regions").and_then(|f| f.as_block()) else {
            continue;
        };
        let mut here = 0usize;
        for region in regions.iter() {
            let Some(perms) = region.field("permutations").and_then(|f| f.as_block()) else {
                continue;
            };
            for perm in perms.iter() {
                let Some(bsps) = perm.field("bsps").and_then(|f| f.as_block()) else { continue };
                for bsp_el in bsps.iter() {
                    let Some(bsp) = bsp_el.descend("bsp") else { continue };
                    let Some(leaves) = bsp.field("leaves").and_then(|f| f.as_block()) else {
                        continue;
                    };
                    for leaf in leaves.iter() {
                        let n = leaf
                            .read_int_any("bsp2d reference count")
                            .unwrap_or(0)
                            .max(0) as usize;
                        // Bucket so the tail stays readable.
                        let bucket = match n {
                            0..=8 => n,
                            9..=16 => 16,
                            17..=32 => 32,
                            33..=64 => 64,
                            65..=128 => 128,
                            _ => 999,
                        };
                        *hist.entry(bucket).or_default() += 1;
                        leaves_total += 1;
                        here = here.max(n);
                    }
                }
            }
        }
        if here > 0 {
            biggest.push((here, path.file_name().unwrap().to_string_lossy().into_owned()));
        }
    }

    println!("{leaves_total} shipped leaves, references per leaf:");
    for (b, c) in &hist {
        let label = match b {
            999 => ">128".to_string(),
            n if *n > 8 => format!("<={n}"),
            n => n.to_string(),
        };
        println!("  {label:>5}: {c:8}  ({:.2}%)", 100.0 * *c as f64 / leaves_total as f64);
    }
    biggest.sort_unstable();
    biggest.reverse();
    println!("models with the biggest leaf:");
    for (n, name) in biggest.iter().take(10) {
        println!("  {n:>5} references  {name}");
    }
}
