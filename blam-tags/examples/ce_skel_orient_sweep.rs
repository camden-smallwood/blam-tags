//! Sweep every CE skeleton_model: does the hidden `runtime node orientations`
//! block duplicate the visible node `default translation/rotation`?
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let (mut n_tags, mut n_missing, mut n_countmismatch, mut n_valdiff, mut n_exact) = (0, 0, 0, 0, 0);
    let mut examples: Vec<String> = Vec::new();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            let p = e.path.to_ascii_lowercase().replace('\\', "/");
            if !p.ends_with("-skeleton_model.ubulk") { continue }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(tag) = TagFile::read_from_bytes(&bytes) else { continue };
            n_tags += 1;
            let root = tag.root();
            let nb = root.field_path("nodes").and_then(|f| f.as_block());
            let ob = root.field_path("runtime node orientations").and_then(|f| f.as_block());
            let nc = nb.as_ref().map(|b| b.len()).unwrap_or(0);
            let oc = ob.as_ref().map(|b| b.len()).unwrap_or(0);
            if oc == 0 { n_missing += 1; examples.push(format!("EMPTY  {nc:3} nodes  {}", e.path)); continue }
            if oc != nc { n_countmismatch += 1; examples.push(format!("COUNT  {nc} vs {oc}  {}", e.path)); continue }
            let (Some(nb), Some(ob)) = (nb, ob) else { continue };
            let mut diff = 0;
            for i in 0..nc {
                let (Some(n), Some(o)) = (nb.element(i), ob.element(i)) else { continue };
                let (a1, b1) = (n.read_point3d("default translation"), o.read_point3d("translation"));
                let (a2, b2) = (n.read_quat("default rotation"), o.read_quat("rotation"));
                if (a1.x - b1.x).abs() > 1e-6 || (a1.y - b1.y).abs() > 1e-6 || (a1.z - b1.z).abs() > 1e-6
                    || (a2.i - b2.i).abs() > 1e-6 || (a2.j - b2.j).abs() > 1e-6
                    || (a2.k - b2.k).abs() > 1e-6 || (a2.w - b2.w).abs() > 1e-6 { diff += 1; }
            }
            if diff > 0 { n_valdiff += 1; examples.push(format!("VALUES {diff}/{nc} rows differ  {}", e.path)); }
            else { n_exact += 1; }
        }
    }
    println!("skeleton_model tags: {n_tags}");
    println!("  orientations block EMPTY:          {n_missing}");
    println!("  count != node count:               {n_countmismatch}");
    println!("  count ok but VALUES differ:        {n_valdiff}");
    println!("  exact duplicate of node defaults:  {n_exact}");
    for e in examples.iter().take(25) { println!("    {e}"); }
}
