//! Export the s3d_turf bunker collision geometry to two OBJ files by
//! WORLD-SPACE AABB (independent of how the bunker is split into
//! instances):
//!
//!   1. `bunker_instance_collision.obj` — every instanced-geometry
//!      `collision info` surface (world-transformed) whose centroid is
//!      inside the AABB (the detailed/grooved physics meshes).
//!   2. `bunker_mainbsp_collision.obj` — every MAIN-BSP collision
//!      surface whose centroid is inside the AABB (the proxy the decal
//!      primaries land on).
//!
//! Usage:
//!   cargo run --example dump_bunker_collision_obj -- <sbsp> \
//!       minx miny minz maxx maxy maxz

use std::io::Write;
use std::path::PathBuf;

use blam_tags::TagFile;
use blam_tags::math::RealPoint3d;
use blam_tags::structure_bsp::{Bsp3d, StructureBsp};

fn surface_ring(bsp: &Bsp3d, surface_index: i32) -> Vec<RealPoint3d> {
    let mut out = Vec::new();
    let Some(surf) = bsp.surfaces.get(surface_index as usize) else {
        return out;
    };
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

fn centroid(ring: &[RealPoint3d]) -> RealPoint3d {
    let n = ring.len() as f32;
    RealPoint3d {
        x: ring.iter().map(|p| p.x).sum::<f32>() / n,
        y: ring.iter().map(|p| p.y).sum::<f32>() / n,
        z: ring.iter().map(|p| p.z).sum::<f32>() / n,
    }
}

fn in_box(p: &RealPoint3d, mn: &[f32; 3], mx: &[f32; 3]) -> bool {
    p.x >= mn[0] && p.x <= mx[0] && p.y >= mn[1] && p.y <= mx[1] && p.z >= mn[2] && p.z <= mx[2]
}

fn write_obj(path: &PathBuf, polys: &[Vec<RealPoint3d>]) -> std::io::Result<usize> {
    let mut f = std::fs::File::create(path)?;
    let mut vbase = 1usize;
    let mut nsurf = 0;
    for poly in polys {
        if poly.len() < 3 {
            continue;
        }
        for p in poly {
            writeln!(f, "v {:.5} {:.5} {:.5}", p.x, p.y, p.z)?;
        }
        for k in 1..poly.len() - 1 {
            writeln!(f, "f {} {} {}", vbase, vbase + k, vbase + k + 1)?;
        }
        vbase += poly.len();
        nsurf += 1;
    }
    Ok(nsurf)
}

fn main() {
    let a: Vec<f32> = std::env::args().skip(2).filter_map(|s| s.parse().ok()).collect();
    let path = PathBuf::from(std::env::args().nth(1).expect("usage: <sbsp> minx miny minz maxx maxy maxz"));
    let (mn, mx) = if a.len() >= 6 {
        ([a[0], a[1], a[2]], [a[3], a[4], a[5]])
    } else {
        // Default: s3d_turf bunker footprint.
        ([-10.6, 6.6, 0.8], [-3.8, 9.2, 3.0])
    };
    eprintln!("AABB min=({:?}) max=({:?})", mn, mx);

    let tag = TagFile::read(&path).expect("read sbsp");
    let sbsp = StructureBsp::from_tag(&tag).expect("StructureBsp::from_tag");
    let out_dir = PathBuf::from(std::env::var("HOME").unwrap()).join("Downloads");

    // ---- Instance collision surfaces in the AABB (world space) ----
    let mut inst_polys: Vec<Vec<RealPoint3d>> = Vec::new();
    let mut inst_hits: std::collections::BTreeMap<i16, usize> = Default::default();
    for inst in &sbsp.instanced_geometry_instances {
        let Some(def) = sbsp.instance_definitions.get(inst.definition_index as usize) else { continue };
        let Some(ibsp) = def.bsp.as_ref() else { continue };
        // Cheap reject: instance bounding sphere vs AABB center.
        let c = inst.world_bounding_sphere_center;
        let r = inst.world_bounding_sphere_radius;
        let cx = (mn[0] + mx[0]) * 0.5;
        let cy = (mn[1] + mx[1]) * 0.5;
        let cz = (mn[2] + mx[2]) * 0.5;
        let half = ((mx[0]-mn[0]).powi(2)+(mx[1]-mn[1]).powi(2)+(mx[2]-mn[2]).powi(2)).sqrt()*0.5;
        if (c.x-cx).powi(2)+(c.y-cy).powi(2)+(c.z-cz).powi(2) > (r+half).powi(2) { continue; }
        let to_world = |p: &RealPoint3d| RealPoint3d {
            x: inst.scale*(p.x*inst.forward.i+p.y*inst.left.i+p.z*inst.up.i)+inst.position.x,
            y: inst.scale*(p.x*inst.forward.j+p.y*inst.left.j+p.z*inst.up.j)+inst.position.y,
            z: inst.scale*(p.x*inst.forward.k+p.y*inst.left.k+p.z*inst.up.k)+inst.position.z,
        };
        for si in 0..ibsp.surfaces.len() as i32 {
            let ring: Vec<RealPoint3d> = surface_ring(ibsp, si).iter().map(to_world).collect();
            if ring.len() < 3 { continue; }
            if in_box(&centroid(&ring), &mn, &mx) {
                inst_polys.push(ring);
                *inst_hits.entry(inst.definition_index).or_insert(0) += 1;
            }
        }
    }
    let inst_out = out_dir.join("bunker_instance_collision.obj");
    let n1 = write_obj(&inst_out, &inst_polys).unwrap();
    eprintln!("wrote {} ({n1} surfaces from {} instance-defs)", inst_out.display(), inst_hits.len());

    // ---- Main-BSP collision surfaces in the AABB ----
    let mbsp = sbsp.collision_bsp.as_ref().expect("no main collision bsp");
    let main_polys: Vec<Vec<RealPoint3d>> = (0..mbsp.surfaces.len() as i32)
        .filter_map(|si| {
            let ring = surface_ring(mbsp, si);
            if ring.len() < 3 { return None; }
            if in_box(&centroid(&ring), &mn, &mx) { Some(ring) } else { None }
        })
        .collect();
    let main_out = out_dir.join("bunker_mainbsp_collision.obj");
    let n2 = write_obj(&main_out, &main_polys).unwrap();
    eprintln!("wrote {} ({n2} surfaces)", main_out.display());
}
