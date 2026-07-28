//! Probe: is `default translation/rotation` on skeleton_model nodes the live
//! rest pose, or does the hidden `runtime node orientations` block shadow it?
//! Run: cargo run -p blam-tags --features iostore --example ce_skel_pose -- [suffix]
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let suffix = std::env::args().nth(1)
        .unwrap_or_else(|| "pelican-skeleton_model.ubulk".to_string()).to_ascii_lowercase();
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut hits: Vec<(String, Vec<u8>)> = Vec::new();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase().replace('\\', "/");
            if p.ends_with(&suffix) {
                if let Ok(b) = a.read(&e.path) { hits.push((e.path.clone(), b)); }
            }
        }
    }
    if hits.is_empty() { eprintln!("no match for {suffix}"); return; }
    for (path, bytes) in hits.iter().take(3) {
        println!("\n########## {path}  ({} bytes)", bytes.len());
        let Ok(tag) = TagFile::read_from_bytes(bytes) else { println!("  parse failed"); continue };
        let root = tag.root();
        let nb = root.field_path("nodes").and_then(|f| f.as_block());
        let ob = root.field_path("runtime node orientations").and_then(|f| f.as_block());
        let nc = nb.as_ref().map(|b| b.len()).unwrap_or(0);
        let oc = ob.as_ref().map(|b| b.len()).unwrap_or(0);
        println!("  nodes = {nc}   runtime node orientations = {oc}");
        let Some(nb) = nb else { continue };
        for i in 0..nc {
            let Some(n) = nb.element(i) else { continue };
            let name = n.read_string_id("name").unwrap_or_default();
            let par = n.read_int_any("parent node").unwrap_or(-1);
            let tp = n.read_point3d("default translation");
            let q = n.read_quat("default rotation");
            let t = format!("({:8.3},{:8.3},{:8.3}) q({:6.3},{:6.3},{:6.3},{:6.3})", tp.x, tp.y, tp.z, q.i, q.j, q.k, q.w);
            let mut line = format!("  [{i:3}] {name:<28} parent={par:<4} nodeT={t}");
            if let Some(ob) = &ob {
                if let Some(o) = ob.element(i) {
                    let op = o.read_point3d("translation");
                    let oq = o.read_quat("rotation");
                    let ot = format!("({:8.3},{:8.3},{:8.3}) q({:6.3},{:6.3},{:6.3},{:6.3})", op.x, op.y, op.z, oq.i, oq.j, oq.k, oq.w);
                    line.push_str(&format!("  | orientT={ot}"));
                }
            }
            println!("{line}");
        }
    }
}
