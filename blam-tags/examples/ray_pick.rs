//! Ray-cast from a world point along a direction against ALL collision
//! geometry (main BSP + every instance, world-transformed) and report
//! the nearest hits — used to identify exactly what the camera center
//! is pointing at (e.g. the s3d_turf bunker).
//!
//! Usage:
//!   cargo run --example ray_pick -- <sbsp> ox oy oz dx dy dz

use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::math::RealPoint3d;
use blam_tags::structure_bsp::{Bsp3d, StructureBsp};

fn surface_ring(bsp: &Bsp3d, surface_index: i32) -> Vec<RealPoint3d> {
    let mut out = Vec::new();
    let Some(surf) = bsp.surfaces.get(surface_index as usize) else { return out };
    let first_edge = surf.first_edge;
    let mut idx = first_edge;
    let mut guard = 0;
    while idx != -1 && guard < 4096 {
        guard += 1;
        let Some(edge) = bsp.edges.get(idx as usize).copied() else { break };
        let on_right = edge.right_surface == surface_index;
        let vidx = if on_right { edge.end_vertex } else { edge.start_vertex };
        let Some(v) = bsp.vertices.get(vidx as usize) else { break };
        out.push(v.point);
        let next_edge = if on_right { edge.reverse_edge } else { edge.forward_edge };
        idx = if next_edge == first_edge { -1 } else { next_edge };
    }
    out
}

// Möller–Trumbore, returns t if hit (front or back face).
fn ray_tri(o: [f32; 3], d: [f32; 3], a: &RealPoint3d, b: &RealPoint3d, c: &RealPoint3d) -> Option<f32> {
    let e1 = [b.x - a.x, b.y - a.y, b.z - a.z];
    let e2 = [c.x - a.x, c.y - a.y, c.z - a.z];
    let p = [d[1]*e2[2]-d[2]*e2[1], d[2]*e2[0]-d[0]*e2[2], d[0]*e2[1]-d[1]*e2[0]];
    let det = e1[0]*p[0]+e1[1]*p[1]+e1[2]*p[2];
    if det.abs() < 1e-8 { return None; }
    let inv = 1.0/det;
    let tv = [o[0]-a.x, o[1]-a.y, o[2]-a.z];
    let u = (tv[0]*p[0]+tv[1]*p[1]+tv[2]*p[2])*inv;
    if u < -1e-4 || u > 1.0001 { return None; }
    let q = [tv[1]*e1[2]-tv[2]*e1[1], tv[2]*e1[0]-tv[0]*e1[2], tv[0]*e1[1]-tv[1]*e1[0]];
    let v = (d[0]*q[0]+d[1]*q[1]+d[2]*q[2])*inv;
    if v < -1e-4 || u+v > 1.0001 { return None; }
    let t = (e2[0]*q[0]+e2[1]*q[1]+e2[2]*q[2])*inv;
    if t > 1e-3 { Some(t) } else { None }
}

fn cast_poly(o: [f32;3], d: [f32;3], ring: &[RealPoint3d]) -> Option<f32> {
    let mut best: Option<f32> = None;
    for k in 1..ring.len().saturating_sub(1) {
        if let Some(t) = ray_tri(o, d, &ring[0], &ring[k], &ring[k+1]) {
            best = Some(best.map_or(t, |b| b.min(t)));
        }
    }
    best
}

fn main() {
    let a: Vec<f32> = std::env::args().skip(2).filter_map(|s| s.parse().ok()).collect();
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: <sbsp> ox oy oz dx dy dz"));
    let o = [a[0], a[1], a[2]];
    let dl = (a[3]*a[3]+a[4]*a[4]+a[5]*a[5]).sqrt();
    let d = [a[3]/dl, a[4]/dl, a[5]/dl];

    let tag = TagFile::read(&path).expect("read sbsp");
    let sbsp = StructureBsp::from_tag(&tag).expect("StructureBsp::from_tag");

    let mut hits: Vec<(f32, String, RealPoint3d, i32)> = Vec::new();

    // Main BSP.
    if let Some(mbsp) = sbsp.collision_bsp.as_ref() {
        for si in 0..mbsp.surfaces.len() as i32 {
            let ring = surface_ring(mbsp, si);
            if ring.len() < 3 { continue; }
            if let Some(t) = cast_poly(o, d, &ring) {
                let p = RealPoint3d { x: o[0]+t*d[0], y: o[1]+t*d[1], z: o[2]+t*d[2] };
                hits.push((t, "MAIN_BSP".into(), p, si));
            }
        }
    }
    // Instances.
    for (ii, inst) in sbsp.instanced_geometry_instances.iter().enumerate() {
        let Some(def) = sbsp.instance_definitions.get(inst.definition_index as usize) else { continue };
        let Some(ibsp) = def.bsp.as_ref() else { continue };
        let to_world = |p: &RealPoint3d| RealPoint3d {
            x: inst.scale*(p.x*inst.forward.i+p.y*inst.left.i+p.z*inst.up.i)+inst.position.x,
            y: inst.scale*(p.x*inst.forward.j+p.y*inst.left.j+p.z*inst.up.j)+inst.position.y,
            z: inst.scale*(p.x*inst.forward.k+p.y*inst.left.k+p.z*inst.up.k)+inst.position.z,
        };
        for si in 0..ibsp.surfaces.len() as i32 {
            let ring: Vec<RealPoint3d> = surface_ring(ibsp, si).iter().map(to_world).collect();
            if ring.len() < 3 { continue; }
            if let Some(t) = cast_poly(o, d, &ring) {
                let p = RealPoint3d { x: o[0]+t*d[0], y: o[1]+t*d[1], z: o[2]+t*d[2] };
                hits.push((t, format!("inst {ii} def={} '{}'", inst.definition_index, inst.name), p, si));
            }
        }
    }

    hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    println!("ray o=({:.2},{:.2},{:.2}) d=({:.3},{:.3},{:.3}) — nearest {} hits:", o[0],o[1],o[2],d[0],d[1],d[2], hits.len().min(15));
    for (t, name, p, si) in hits.iter().take(15) {
        println!("  t={:>6.2} hit=({:>7.2},{:>6.2},{:>6.2}) surf={:>5} {}", t, p.x, p.y, p.z, si, name);
    }
}
