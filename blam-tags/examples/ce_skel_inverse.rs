//! Are the node `inverse forward/left/up/position` fields the inverse of the
//! ABSOLUTE (world) bind transform composed from `default translation/rotation`?
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::{RealQuaternion, RealVector3d};

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn main() {
    let suffix = std::env::args().nth(1)
        .unwrap_or_else(|| "pelican-skeleton_model.ubulk".to_string());
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();
    let mut bytes = None;
    'o: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix.to_ascii_lowercase()) {
                bytes = a.read(&e.path).ok(); break 'o;
            }
        }
    }
    let tag = TagFile::read_from_bytes(&bytes.expect("not found")).unwrap();
    let root = tag.root();
    let nb = root.field_path("nodes").and_then(|f| f.as_block()).expect("nodes");
    let n = nb.len();
    let (mut par, mut lr, mut lt) = (vec![], vec![], vec![]);
    for i in 0..n {
        let e = nb.element(i).unwrap();
        par.push(e.read_int_any("parent node").unwrap_or(-1) as i32);
        lr.push(e.read_quat("default rotation"));
        lt.push(e.read_point3d("default translation"));
    }
    // world transforms
    let mut wr = vec![RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 }; n];
    let mut wt = vec![RealVector3d { i: 0.0, j: 0.0, k: 0.0 }; n];
    for i in 0..n {
        let lq = lr[i].normalized();
        let ltv = RealVector3d { i: lt[i].x, j: lt[i].y, k: lt[i].z };
        let p = par[i];
        if p >= 0 && (p as usize) < i {
            let p = p as usize;
            wr[i] = (wr[p] * lq).normalized();
            wt[i] = wt[p] + wr[p].rotate(ltv);
        } else { wr[i] = lq; wt[i] = ltv; }
    }
    let mut worst = 0.0f32;
    println!("{:<26} {:>34} {:>34}  err", "node", "inverse position (tag)", "-(R^-1 * world_t) (computed)");
    for i in 0..n {
        let e = nb.element(i).unwrap();
        let inv_p = e.read_point3d("inverse position");
        // inverse of world transform: p' = -R^-1 * t
        let conj = RealQuaternion { i: -wr[i].i, j: -wr[i].j, k: -wr[i].k, w: wr[i].w };
        let c = conj.rotate(wt[i]);
        let exp = (-c.i, -c.j, -c.k);
        let err = ((inv_p.x - exp.0).abs()).max((inv_p.y - exp.1).abs()).max((inv_p.z - exp.2).abs());
        if err > worst { worst = err; }
        if i < 8 || err > 1e-3 {
            let nm = e.read_string_id("name").unwrap_or_default();
            println!("{nm:<26} ({:8.3},{:8.3},{:8.3})   ({:8.3},{:8.3},{:8.3})  {err:.5}",
                inv_p.x, inv_p.y, inv_p.z, exp.0, exp.1, exp.2);
        }
    }
    println!("\nworst |error| over {n} nodes = {worst:.6}");
}
