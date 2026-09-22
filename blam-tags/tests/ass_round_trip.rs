//! Export a shipped structure BSP to ASS, read it back, and require the
//! two to agree.
//!
//! This is the strongest oracle available for the reader: the corpus
//! supplies real scenes — thousands of materials, hundreds of thousands
//! of vertices, quoted names with spaces in them, lights, spheres — and
//! `ass.rs` supplies the expected answer. Neither side is grading its own
//! work, and a hand-written fixture could not cover the same ground.
//!
//! Floats go through text at ten decimal places, so positions are
//! compared with a tolerance rather than for equality. Counts, indices,
//! names and material assignments are compared exactly: those are the
//! things a mangled import would get wrong.

use std::path::{Path, PathBuf};

use blam_tags::ass::{AssFile, AssObjectPayload};
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

fn close(a: f32, b: f32, scale: f32) -> bool {
    (a - b).abs() <= 1e-4 * scale.max(1.0)
}

/// Compare two scenes, returning the first difference.
fn differ(a: &AssFile, b: &AssFile) -> Option<String> {
    if a.header_tool != b.header_tool || a.header_machine != b.header_machine {
        return Some("header strings".into());
    }
    if a.materials.len() != b.materials.len() {
        return Some(format!("{} materials against {}", a.materials.len(), b.materials.len()));
    }
    for (i, (x, y)) in a.materials.iter().zip(&b.materials).enumerate() {
        if x.name != y.name {
            return Some(format!("material {i} name {:?} against {:?}", x.name, y.name));
        }
        if x.lightmap_variant != y.lightmap_variant {
            return Some(format!("material {i} lightmap variant"));
        }
        if x.bm_strings != y.bm_strings {
            return Some(format!("material {i} BM strings"));
        }
    }
    if a.objects.len() != b.objects.len() {
        return Some(format!("{} objects against {}", a.objects.len(), b.objects.len()));
    }
    for (i, (x, y)) in a.objects.iter().zip(&b.objects).enumerate() {
        if x.xref_filepath != y.xref_filepath || x.xref_objectname != y.xref_objectname {
            return Some(format!("object {i} xref"));
        }
        match (&x.payload, &y.payload) {
            (
                AssObjectPayload::Mesh { vertices: v1, triangles: t1 },
                AssObjectPayload::Mesh { vertices: v2, triangles: t2 },
            ) => {
                if v1.len() != v2.len() {
                    return Some(format!(
                        "object {i} has {} vertices against {}",
                        v1.len(),
                        v2.len()
                    ));
                }
                if t1.len() != t2.len() {
                    return Some(format!(
                        "object {i} has {} triangles against {}",
                        t1.len(),
                        t2.len()
                    ));
                }
                for (k, (p, q)) in v1.iter().zip(v2).enumerate() {
                    let s = p.position.x.abs().max(p.position.y.abs()).max(p.position.z.abs());
                    if !close(p.position.x, q.position.x, s)
                        || !close(p.position.y, q.position.y, s)
                        || !close(p.position.z, q.position.z, s)
                    {
                        return Some(format!("object {i} vertex {k} position"));
                    }
                    if !close(p.normal.i, q.normal.i, 1.0)
                        || !close(p.normal.j, q.normal.j, 1.0)
                        || !close(p.normal.k, q.normal.k, 1.0)
                    {
                        return Some(format!("object {i} vertex {k} normal"));
                    }
                    if p.uvs.len() != q.uvs.len() {
                        return Some(format!("object {i} vertex {k} uv count"));
                    }
                    if p.node_set.len() != q.node_set.len() {
                        return Some(format!("object {i} vertex {k} node count"));
                    }
                }
                // Exact: a triangle's material and winding are what a
                // mangled import gets wrong, and they are integers.
                for (k, (p, q)) in t1.iter().zip(t2).enumerate() {
                    if p.material != q.material || p.v != q.v {
                        return Some(format!(
                            "object {i} triangle {k}: material {} v {:?} against material {} v {:?}",
                            p.material, p.v, q.material, q.v
                        ));
                    }
                }
            }
            (AssObjectPayload::Sphere { material: m1, radius: r1 }, AssObjectPayload::Sphere { material: m2, radius: r2 }) => {
                if m1 != m2 || !close(*r1, *r2, r1.abs()) {
                    return Some(format!("object {i} sphere"));
                }
            }
            (AssObjectPayload::GenericLight(l1), AssObjectPayload::GenericLight(l2)) => {
                if l1.kind != l2.kind || !close(l1.intensity, l2.intensity, l1.intensity.abs()) {
                    return Some(format!("object {i} light"));
                }
            }
            _ => return Some(format!("object {i} class")),
        }
    }
    if a.instances.len() != b.instances.len() {
        return Some(format!("{} instances against {}", a.instances.len(), b.instances.len()));
    }
    for (i, (x, y)) in a.instances.iter().zip(&b.instances).enumerate() {
        if x.object_index != y.object_index || x.name != y.name {
            return Some(format!("instance {i} identity"));
        }
        if x.unique_id != y.unique_id || x.parent_id != y.parent_id {
            return Some(format!("instance {i} ids"));
        }
        let s = x.local_translation.x.abs().max(x.local_translation.y.abs());
        if !close(x.local_translation.x, y.local_translation.x, s)
            || !close(x.local_translation.z, y.local_translation.z, s)
            || !close(x.local_scale, y.local_scale, 1.0)
        {
            return Some(format!("instance {i} placement"));
        }
    }
    None
}

#[test]
fn shipped_structure_bsps_survive_a_write_and_read() {
    let Some(kit) = h3ek() else {
        eprintln!("skipping: no H3EK install");
        return;
    };

    let limit: usize = std::env::var("BLAM_ASS_MODELS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12);

    let (mut checked, mut skipped) = (0usize, 0usize);
    let mut bad: Vec<String> = Vec::new();
    let (mut objects, mut vertices, mut triangles) = (0usize, 0usize, 0usize);

    for path in walk(&kit.join("tags"), "scenario_structure_bsp").iter().take(limit) {
        let Ok(tag) = TagFile::read(path) else {
            skipped += 1;
            continue;
        };
        let Ok(original) = AssFile::from_scenario_structure_bsp(&tag) else {
            skipped += 1;
            continue;
        };
        let mut text: Vec<u8> = Vec::new();
        if original.write(&mut text).is_err() {
            skipped += 1;
            continue;
        }
        let Ok(src) = String::from_utf8(text) else {
            skipped += 1;
            continue;
        };
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();

        match blam_tags::ass_parse::parse(&src) {
            Ok((back, version)) => {
                checked += 1;
                assert_eq!(version, 7, "{name}: the writer emits version 7");
                objects += original.objects.len();
                for o in &original.objects {
                    if let AssObjectPayload::Mesh { vertices: v, triangles: t } = &o.payload {
                        vertices += v.len();
                        triangles += t.len();
                    }
                }
                if let Some(d) = differ(&original, &back) {
                    bad.push(format!("{name}: {d}"));
                }
            }
            Err(e) => bad.push(format!("{name}: {e}")),
        }
    }

    eprintln!("{checked} structure BSPs exported and read back ({skipped} skipped)");
    eprintln!("  {objects} objects, {vertices} vertices, {triangles} triangles");
    for b in bad.iter().take(8) {
        eprintln!("    {b}");
    }

    assert!(checked > 0, "no structure BSPs were available to check");
    assert!(
        bad.is_empty(),
        "the reader does not reproduce what the writer emitted:\n{}",
        bad.join("\n")
    );
}
