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
    let mut bytes = None;
    'o: for u in &utocs { let Ok(a)=IoStoreArchive::open(u) else {continue};
        for e in a.entries() { if e.path.to_ascii_lowercase().ends_with("pelican-skeleton_model.ubulk") { bytes=a.read(&e.path).ok(); break 'o; } } }
    let tag = TagFile::read_from_bytes(&bytes.unwrap()).unwrap();
    let root = tag.root();
    let nb = root.field_path("nodes").and_then(|f| f.as_block()).unwrap();
    let n = nb.len();
    let (mut par, mut lr, mut lt) = (vec![], vec![], vec![]);
    for i in 0..n { let e = nb.element(i).unwrap();
        par.push(e.read_int_any("parent node").unwrap_or(-1) as i32);
        lr.push(e.read_quat("default rotation").normalized());
        let t = e.read_point3d("default translation");
        lt.push(RealVector3d { i: t.x, j: t.y, k: t.z }); }
    let mut wr = vec![RealQuaternion{i:0.0,j:0.0,k:0.0,w:1.0}; n];
    let mut wt = vec![RealVector3d{i:0.0,j:0.0,k:0.0}; n];
    for i in 0..n { let p = par[i];
        if p>=0 && (p as usize)<i { let p=p as usize; wr[i]=(wr[p]*lr[i]).normalized(); wt[i]=wt[p]+wr[p].rotate(lt[i]); }
        else { wr[i]=lr[i]; wt[i]=lt[i]; } }
    let x=RealVector3d{i:1.0,j:0.0,k:0.0}; let y=RealVector3d{i:0.0,j:1.0,k:0.0}; let z=RealVector3d{i:0.0,j:0.0,k:1.0};
    let p3=|v:RealVector3d| format!("({:7.4},{:7.4},{:7.4})", v.i, v.j, v.k);
    for i in [0usize, 4, 8, 16, 32] {
        if i>=n { continue }
        let e = nb.element(i).unwrap();
        let ci = conj(wr[i]);
        println!("[{i}] {}  worldq=({:.3},{:.3},{:.3},{:.3})", e.read_string_id("name").unwrap_or_default(), wr[i].i, wr[i].j, wr[i].k, wr[i].w);
        println!("   stored fwd={} left={} up={}", p3(e.read_vec3("inverse forward")), p3(e.read_vec3("inverse left")), p3(e.read_vec3("inverse up")));
        println!("   conj*axis  ={} {} {}", p3(ci.rotate(x)), p3(ci.rotate(y)), p3(ci.rotate(z)));
        println!("   wrot*axis  ={} {} {}", p3(wr[i].rotate(x)), p3(wr[i].rotate(y)), p3(wr[i].rotate(z)));
        println!("   inv scale={:?}", e.read_real("inverse scale"));
    }
}
