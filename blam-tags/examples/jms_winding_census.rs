//! Do the JMS builders wind triangles the same way relative to their vertex
//! normals? A JMS converted between Halo CE and Halo 2/3 keeps its triangles
//! as they are, which is right only if every builder agrees: Halo CE
//! tool.exe reverses winding on read and its tags wind the other way, so the
//! two reversals cancel only when the in-memory winding means the same thing.
//!
//!   cargo run -q --release -p blam-tags --example jms_winding_census -- /Users/camden/Halo

use blam_tags::classic::read_classic_tag_file;
use blam_tags::{JmsFile, TagFile, TagLayout};
use std::path::{Path, PathBuf};

fn collect(root: &Path, ext: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, ext, out);
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

/// (triangles whose face normal agrees with their vertex normals, disagree).
fn census(jms: &JmsFile) -> (usize, usize) {
    let (mut agree, mut disagree) = (0, 0);
    for t in &jms.triangles {
        let [a, b, c] = t.v.map(|i| &jms.vertices[i as usize]);
        let e1 = b.position - a.position;
        let e2 = c.position - a.position;
        let face = (e1.j * e2.k - e1.k * e2.j, e1.k * e2.i - e1.i * e2.k, e1.i * e2.j - e1.j * e2.i);
        let n = (
            a.normal.i + b.normal.i + c.normal.i,
            a.normal.j + b.normal.j + c.normal.j,
            a.normal.k + b.normal.k + c.normal.k,
        );
        let dot = face.0 * n.0 + face.1 * n.1 + face.2 * n.2;
        if dot > 1e-9 {
            agree += 1;
        } else if dot < -1e-9 {
            disagree += 1;
        }
    }
    (agree, disagree)
}

fn main() {
    let halo = PathBuf::from(std::env::args().nth(1).expect("~/Halo"));
    let defs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions");
    for (game, ext) in [("haloce_mcc", "gbxmodel"), ("halo2_mcc", "render_model"), ("halo3_mcc", "render_model")] {
        let mut files = Vec::new();
        collect(&halo.join(game).join("tags"), ext, &mut files);
        let (mut built, mut failed, mut agree, mut disagree) = (0, 0, 0usize, 0usize);
        let (mut models_mostly_agree, mut models_mostly_disagree) = (0, 0);
        for path in &files {
            let Ok(bytes) = std::fs::read(path) else { failed += 1; continue };
            let tag = if game == "halo3_mcc" {
                TagFile::read(path).ok()
            } else {
                TagLayout::from_json(defs.join(game).join(format!("{ext}.json")))
                    .ok()
                    .and_then(|layout| read_classic_tag_file(&bytes, layout).ok())
            };
            let Some(tag) = tag else { failed += 1; continue };
            let jms = match game {
                "haloce_mcc" => JmsFile::from_gbxmodel(&tag),
                "halo2_mcc" => JmsFile::from_h2_render_model(&tag),
                _ => JmsFile::from_render_model(&tag),
            };
            let Ok(jms) = jms else { failed += 1; continue };
            built += 1;
            let (a, d) = census(&jms);
            agree += a;
            disagree += d;
            if a > d {
                models_mostly_agree += 1;
            } else if d > a {
                models_mostly_disagree += 1;
            }
        }
        println!(
            "{game}: {} files, {built} built, {failed} unreadable/unbuildable; triangles agreeing with normals {agree}, disagreeing {disagree} ({:.2}%); models mostly agreeing {models_mostly_agree}, mostly disagreeing {models_mostly_disagree}",
            files.len(),
            100.0 * agree as f64 / (agree + disagree).max(1) as f64,
        );
    }
}
