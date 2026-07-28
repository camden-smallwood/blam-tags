//! Pin down the exact relationship between a skeleton_model node's rest pose
//! and its derived `inverse forward/left/up/position`, `inverse scale`, and the
//! `runtime node orientations` mirror — so a rebake can reproduce them.
use blam_tags::file::TagFile;
use blam_tags::iostore::IoStoreArchive;
use blam_tags::math::{RealQuaternion, RealVector3d};

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";

fn conj(q: RealQuaternion) -> RealQuaternion {
    RealQuaternion { i: -q.i, j: -q.j, k: -q.k, w: q.w }
}

fn main() {
    let suffix = std::env::args().nth(1).unwrap_or_else(|| "pelican-skeleton_model.ubulk".into());
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
    let nb = root.field_path("nodes").and_then(|f| f.as_block()).unwrap();
    let ob = root.field_path("runtime node orientations").and_then(|f| f.as_block());
    let n = nb.len();

    let (mut par, mut lr, mut lt) = (vec![], vec![], vec![]);
    for i in 0..n {
        let e = nb.element(i).unwrap();
        par.push(e.read_int_any("parent node").unwrap_or(-1) as i32);
        lr.push(e.read_quat("default rotation").normalized());
        let t = e.read_point3d("default translation");
        lt.push(RealVector3d { i: t.x, j: t.y, k: t.z });
    }
    let mut wr = vec![RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 }; n];
    let mut wt = vec![RealVector3d { i: 0.0, j: 0.0, k: 0.0 }; n];
    for i in 0..n {
        let p = par[i];
        if p >= 0 && (p as usize) < i {
            let p = p as usize;
            wr[i] = (wr[p] * lr[i]).normalized();
            wt[i] = wt[p] + wr[p].rotate(lt[i]);
        } else { wr[i] = lr[i]; wt[i] = lt[i]; }
    }

    let (mut e_fwd, mut e_left, mut e_up, mut e_pos, mut e_scale, mut e_orient) =
        (0f32, 0f32, 0f32, 0f32, 0f32, 0f32);
    let (mut b_fwd, mut b_left, mut b_up) = (0f32, 0f32, 0f32);
    let x = RealVector3d { i: 1.0, j: 0.0, k: 0.0 };
    let y = RealVector3d { i: 0.0, j: 1.0, k: 0.0 };
    let z = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };
    for i in 0..n {
        let e = nb.element(i).unwrap();
        let ci = conj(wr[i]);
        // inverse basis = the world rotation's inverse applied to each axis
        // Candidate A: columns of R^-1 (inverse applied to each world axis).
        let (fa, la, ua) = (ci.rotate(x), ci.rotate(y), ci.rotate(z));
        // Candidate B: rows of R^-1 == columns of R (the node's own axes in world space).
        let (fb, lb, ub) = (wr[i].rotate(x), wr[i].rotate(y), wr[i].rotate(z));
        let (f, l, u) = (fa, la, ua);
        let _ = (fb, lb, ub);
        let c = ci.rotate(wt[i]);
        let pos = RealVector3d { i: -c.i, j: -c.j, k: -c.k };
        let m = |a: RealVector3d, b: RealVector3d| {
            (a.i - b.i).abs().max((a.j - b.j).abs()).max((a.k - b.k).abs())
        };
        let tf = e.read_vec3("inverse forward");
        let tl = e.read_vec3("inverse left");
        let tu = e.read_vec3("inverse up");
        let tp = e.read_point3d("inverse position");
        e_fwd = e_fwd.max(m(tf, f));
        e_left = e_left.max(m(tl, l));
        e_up = e_up.max(m(tu, u));
        b_fwd = b_fwd.max(m(tf, fb));
        b_left = b_left.max(m(tl, lb));
        b_up = b_up.max(m(tu, ub));
        e_pos = e_pos.max(m(RealVector3d { i: tp.x, j: tp.y, k: tp.z }, pos));
        e_scale = e_scale.max((e.read_real("inverse scale").unwrap_or(1.0) - 1.0).abs());
        if let Some(ob) = &ob {
            if let Some(o) = ob.element(i) {
                e_orient = e_orient.max((o.read_real("scale").unwrap_or(0.0) - 1.0).abs());
            }
        }
    }
    println!("over {n} nodes, worst |error| vs recomputed-from-rest-pose:");
    println!("  inverse forward  : {e_fwd:.6}   (= conj(world_rot) * +X)");
    println!("  inverse left     : {e_left:.6}   (= conj(world_rot) * +Y)");
    println!("  inverse up       : {e_up:.6}   (= conj(world_rot) * +Z)");
    println!("  inverse position : {e_pos:.6}   (= -(conj(world_rot) * world_t))");
    println!("  inverse scale    : {e_scale:.6}   (vs constant 1.0)");
    println!("  orientations.scale: {e_orient:.6}  (vs constant 1.0)");
    println!("candidate B — rows of R^-1 == the node's own axes in world space:");
    println!("  inverse forward  : {b_fwd:.6}   (= world_rot * +X)");
    println!("  inverse left     : {b_left:.6}   (= world_rot * +Y)");
    println!("  inverse up       : {b_up:.6}   (= world_rot * +Z)");
}
