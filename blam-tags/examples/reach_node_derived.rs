//! Does Reach's render_model carry the same three-copy rest pose as CE's
//! skeleton_model, and do the same derivations hold? Also checks Reach's extra
//! `distance from parent` field, which CE does not have.
use std::path::Path;
use blam_tags::file::TagFile;
use blam_tags::math::{RealQuaternion, RealVector3d};
use blam_tags::skeleton::rebake_derived_node_data;

const TAGS: &str = "/Users/camden/Halo/haloreach_mcc/tags";
fn conj(q: RealQuaternion) -> RealQuaternion { RealQuaternion { i: -q.i, j: -q.j, k: -q.k, w: q.w } }

fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() { walk(&p, out); }
        else if p.extension().is_some_and(|x| x == "render_model") { out.push(p); }
    }
}

fn main() {
    let limit: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(400);
    let mut files = Vec::new();
    walk(Path::new(TAGS), &mut files);
    files.sort();
    println!("{} render_model tags, checking {}\n", files.len(), files.len().min(limit));

    let (mut ok, mut with_orient, mut orient_exact, mut nodes_total) = (0usize, 0usize, 0usize, 0usize);
    let (mut e_pos, mut e_up, mut e_dist) = (0f32, 0f32, 0f32);
    let mut rebake_dirty = 0usize;
    let up_axis = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };

    for path in files.iter() {
        if ok >= limit { break }
        let Ok(mut tag) = TagFile::read(path) else { continue };
        let has_nodes = {
            let root = tag.root();
            root.field_path("nodes").and_then(|f| f.as_block()).map(|b| b.len()).unwrap_or(0)
        };
        if has_nodes < 8 { continue }
        ok += 1;
        {
            let root = tag.root();
            let nb = root.field_path("nodes").and_then(|f| f.as_block()).unwrap();
            let ob = root.field_path("runtime node orientations").and_then(|f| f.as_block());
            let n = nb.len();
            nodes_total += n;
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
            if let Some(ob) = &ob {
                if ob.len() == n {
                    with_orient += 1;
                    let mut exact = true;
                    for i in 0..n {
                        let o = ob.element(i).unwrap();
                        let ot = o.read_point3d("translation");
                        let oq = o.read_quat("rotation");
                        if (ot.x - lt[i].i).abs() > 1e-6 || (ot.y - lt[i].j).abs() > 1e-6
                            || (ot.z - lt[i].k).abs() > 1e-6
                            || (oq.i - lr[i].i).abs() > 1e-4 || (oq.w - lr[i].w).abs() > 1e-4
                        { exact = false; break }
                    }
                    if exact { orient_exact += 1; }
                }
            }
            for i in 0..n {
                let e = nb.element(i).unwrap();
                let ci = conj(wr[i]);
                let c = ci.rotate(wt[i]);
                let tp = e.read_point3d("inverse position");
                e_pos = e_pos.max((tp.x + c.i).abs().max((tp.y + c.j).abs()).max((tp.z + c.k).abs()));
                let eu = ci.rotate(up_axis);
                let tu = e.read_vec3("inverse up");
                e_up = e_up.max((tu.i - eu.i).abs().max((tu.j - eu.j).abs()).max((tu.k - eu.k).abs()));
                if i == 0 && ok <= 3 {
                    println!("  sample {} node[0] inv_pos=({:.3},{:.3},{:.3}) inv_up=({:.3},{:.3},{:.3}) inv_scale={:?}",
                        path.file_name().unwrap().to_string_lossy(), tp.x, tp.y, tp.z, tu.i, tu.j, tu.k,
                        e.read_real("inverse scale"));
                }
                // Reach-only: distance from parent
                if let Some(d) = e.read_real("distance from parent") {
                    let len = (lt[i].i * lt[i].i + lt[i].j * lt[i].j + lt[i].k * lt[i].k).sqrt();
                    e_dist = e_dist.max((d - len).abs());
                }
            }
        }
        let r = rebake_derived_node_data(&mut tag);
        if r.changed() { rebake_dirty += 1; }
    }
    println!("render_models with nodes: {ok}   nodes: {nodes_total}");
    println!("  have a matching-length 'runtime node orientations': {with_orient}/{ok}");
    println!("    of those, a byte-exact mirror of the node rest pose: {orient_exact}/{with_orient}");
    println!("\nworst |error| vs the CE-derived identities:");
    println!("  inverse position  = -(conj(world_rot)*world_t) : {e_pos:.6}");
    println!("  inverse up        =  conj(world_rot)*+Z        : {e_up:.6}");
    println!("  distance from parent = |default translation|   : {e_dist:.6}   (Reach-only field)");
    println!("\nCE rebake run on shipped Reach tags — reporting changes: {rebake_dirty}/{ok}");
}
