//! Confirm the exact derivation of every skeleton_model node's `inverse *`
//! fields from its rest pose, across every CE skeleton:
//!   inverse forward  = conj(world_rot) * (+Y)
//!   inverse left     = conj(world_rot) * (-X)
//!   inverse up       = conj(world_rot) * (+Z)
//!   inverse position = -(conj(world_rot) * world_translation)
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::{RealQuaternion, RealVector3d};

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
fn conj(q: RealQuaternion) -> RealQuaternion { RealQuaternion { i: -q.i, j: -q.j, k: -q.k, w: q.w } }

fn main() {
    let mut utocs: Vec<_> = std::fs::read_dir(PAKS).unwrap().filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))).collect();
    utocs.sort();
    let fwd_axis = RealVector3d { i: 0.0, j: 1.0, k: 0.0 };
    let left_axis = RealVector3d { i: -1.0, j: 0.0, k: 0.0 };
    let up_axis = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };
    let (mut tags, mut nodes) = (0usize, 0usize);
    let (mut e_f, mut e_l, mut e_u, mut e_p) = (0f32, 0f32, 0f32, 0f32);
    let mut bad_basis_tags = std::collections::BTreeSet::new();
    let mut bad_pos_tags = std::collections::BTreeSet::new();
    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        for e in a.entries() {
            if !e.path.to_ascii_lowercase().replace('\\',"/").ends_with("-skeleton_model.ubulk") { continue }
            let Ok(bytes) = a.read(&e.path) else { continue };
            let Ok(tag) = TagFile::read_from_bytes(&bytes) else { continue };
            let root = tag.root();
            let Some(nb) = root.field_path("nodes").and_then(|f| f.as_block()) else { continue };
            let n = nb.len();
            if n == 0 { continue }
            tags += 1;
            let (mut par, mut lr, mut lt) = (vec![], vec![], vec![]);
            for i in 0..n { let el = nb.element(i).unwrap();
                par.push(el.read_int_any("parent node").unwrap_or(-1) as i32);
                lr.push(el.read_quat("default rotation").normalized());
                let t = el.read_point3d("default translation");
                lt.push(RealVector3d { i: t.x, j: t.y, k: t.z }); }
            let mut wr = vec![RealQuaternion{i:0.0,j:0.0,k:0.0,w:1.0}; n];
            let mut wt = vec![RealVector3d{i:0.0,j:0.0,k:0.0}; n];
            for i in 0..n { let p = par[i];
                if p >= 0 && (p as usize) < i { let p = p as usize;
                    wr[i] = (wr[p] * lr[i]).normalized(); wt[i] = wt[p] + wr[p].rotate(lt[i]); }
                else { wr[i] = lr[i]; wt[i] = lt[i]; } }
            for i in 0..n {
                let el = nb.element(i).unwrap();
                let ci = conj(wr[i]);
                let c = ci.rotate(wt[i]);
                let m = |a: RealVector3d, b: RealVector3d|
                    (a.i-b.i).abs().max((a.j-b.j).abs()).max((a.k-b.k).abs());
                let tp = el.read_point3d("inverse position");
                let (df, dl, du) = (
                    m(el.read_vec3("inverse forward"), ci.rotate(fwd_axis)),
                    m(el.read_vec3("inverse left"), ci.rotate(left_axis)),
                    m(el.read_vec3("inverse up"), ci.rotate(up_axis)));
                let dp = m(RealVector3d{i:tp.x,j:tp.y,k:tp.z}, RealVector3d{i:-c.i,j:-c.j,k:-c.k});
                e_f = e_f.max(df); e_l = e_l.max(dl); e_u = e_u.max(du); e_p = e_p.max(dp);
                if df.max(dl).max(du) > 1e-3 { bad_basis_tags.insert(e.path.clone()); }
                if dp > 1e-3 { bad_pos_tags.insert(e.path.clone()); }
                nodes += 1;
            }
        }
    }
    println!("skeletons: {tags}   nodes: {nodes}");
    println!("worst |error| per field:");
    println!("  inverse forward : {e_f:.6}");
    println!("  inverse left    : {e_l:.6}");
    println!("  inverse up      : {e_u:.6}");
    println!("  inverse position: {e_p:.6}   <-- convention-independent");
    println!("skeletons with a basis mismatch   : {}/{tags}", bad_basis_tags.len());
    println!("skeletons with a POSITION mismatch: {}/{tags}", bad_pos_tags.len());
    for t in bad_pos_tags.iter().take(5) { println!("    pos-bad: {t}"); }
}
