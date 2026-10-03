//! Halo CE gbxmodel → JMS 8200 node records, checked against the tag's own
//! `nodes` block. Halo CE tool.exe reads JMS 8200 nodes parent-relative and
//! copies each quaternion and (×0.01) translation straight into the gbxmodel,
//! so a correct export writes back exactly what the tag holds.
//!
//!   cargo run -q --release -p blam-tags --example ce_jms8200_node_roundtrip -- /Users/camden/Halo/haloce_mcc/tags

use blam_tags::classic::{read_classic_tag_file, ClassicHeader};
use blam_tags::{JmsFile, TagLayout};
use std::path::{Path, PathBuf};

fn collect(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "gbxmodel") {
            out.push(p);
        }
    }
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("tags root"));
    let probe = std::env::args().nth(2).is_some();
    let defs = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../definitions/haloce_mcc");
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    let (mut checked, mut unreadable, mut matched, mut mismatched, mut reordered) = (0, 0, 0, 0, 0);
    let mut worst = (0.0f32, String::new());
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        let layout = TagLayout::from_json(&defs.join("gbxmodel.json")).unwrap();
        let tag = match ClassicHeader::parse(&bytes).map(|_| read_classic_tag_file(&bytes, layout)) {
            Some(Ok(tag)) => tag,
            _ => {
                unreadable += 1;
                continue;
            }
        };
        let root_struct = tag.root();
        let nodes = root_struct.field("nodes").and_then(|f| f.as_block()).unwrap();
        if probe {
            let n = nodes.element(0).unwrap();
            for f in n.fields() {
                println!("{:?} = {:?}", f.name(), f.value());
            }
            return;
        }
        let jms = match JmsFile::from_gbxmodel(&tag) {
            Ok(jms) => jms,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let mut out = Vec::new();
        jms.write(&mut out, 8200).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let floats = |l: &str| l.split('\t').map(|v| v.parse::<f32>().unwrap()).collect::<Vec<_>>();
        checked += 1;
        let mut file_bad = 0.0f32;
        let mut links_reordered = false;
        let mut parents_from_links = vec![(-1i32, -1i32); nodes.len()];
        for i in 0..nodes.len() {
            let n = nodes.element(i).unwrap();
            let base = 3 + i * 5;
            let name = lines[base];
            let child: i32 = lines[base + 1].parse().unwrap();
            let sibling: i32 = lines[base + 2].parse().unwrap();
            let q = floats(lines[base + 3]);
            let t = floats(lines[base + 4]);
            let tag_name = n.read_string("name").unwrap_or_default();
            let tag_child = n.read_int_any("first child node index").unwrap_or(-2) as i32;
            let tag_sibling = n.read_int_any("next sibling node index").unwrap_or(-2) as i32;
            let tq = n.read_quat("default rotation");
            let tt = match n.field("default translation").and_then(|f| f.value()) {
                Some(blam_tags::TagFieldData::RealPoint3d(p)) => blam_tags::math::RealVector3d { i: p.x, j: p.y, k: p.z },
                Some(blam_tags::TagFieldData::RealVector3d(v)) => v,
                other => panic!("default translation: {other:?}"),
            };
            let same_sign = (q[0] - tq.i).abs() + (q[1] - tq.j).abs() + (q[2] - tq.k).abs() + (q[3] - tq.w).abs();
            let flipped = (q[0] + tq.i).abs() + (q[1] + tq.j).abs() + (q[2] + tq.k).abs() + (q[3] + tq.w).abs();
            let rot_err = same_sign.min(flipped);
            let tr_err = (t[0] - tt.i * 100.0).abs() + (t[1] - tt.j * 100.0).abs() + (t[2] - tt.k * 100.0).abs();
            let err = rot_err.max(tr_err / 100.0);
            // Sibling order is a free choice: what matters to tool.exe is the
            // parent each node ends up with, so compare those.
            let tag_parent = n.read_int_any("parent node index").unwrap_or(-2) as i32;
            if name != tag_name || jms.nodes[i].parent as i32 != tag_parent {
                file_bad = f32::INFINITY;
            }
            if child != tag_child || sibling != tag_sibling {
                links_reordered = true;
            }
            parents_from_links[i] = (child, sibling);
            file_bad = file_bad.max(err);
        }
        // The links written must give every node the tag's parent.
        let mut derived = vec![-1i32; nodes.len()];
        for (p, &(child, _)) in parents_from_links.iter().enumerate() {
            let mut c = child;
            while c >= 0 {
                derived[c as usize] = p as i32;
                c = parents_from_links[c as usize].1;
            }
        }
        for i in 0..nodes.len() {
            let tag_parent = nodes.element(i).unwrap().read_int_any("parent node index").unwrap_or(-2) as i32;
            if derived[i] != tag_parent {
                file_bad = f32::INFINITY;
            }
        }
        if links_reordered {
            reordered += 1;
        }
        if file_bad < 1e-3 {
            matched += 1;
        } else {
            mismatched += 1;
        }
        if file_bad > worst.0 {
            worst = (file_bad, path.display().to_string());
        }
    }
    println!("gbxmodels: {} found, {checked} checked, {unreadable} unreadable", files.len());
    println!("node names, parents and transforms equal to the tag: {matched}; different: {mismatched}");
    println!("sibling chains in a different (equivalent) order: {reordered}");
    println!("worst error {:.6} in {}", worst.0, worst.1);
}
