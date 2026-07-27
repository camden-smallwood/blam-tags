//! Rebaking the derived data that hangs off a skeleton's node rest pose.
//!
//! A `skeleton_model` (and a `render_model`, which shares the block layout)
//! stores each node's rest pose **three** times:
//!
//! 1. `nodes[i].default translation` / `default rotation` — the authored values;
//! 2. `runtime node orientations[i]` — a hidden tool.exe-baked mirror of them.
//!    Measured across every Campaign Evolved skeleton: a byte-exact duplicate,
//!    1:1 by index, in all 450 that have nodes;
//! 3. `nodes[i].inverse forward / left / up / position` — the inverse of the
//!    node's **absolute** (world) bind transform, composed by chaining the rest
//!    pose down the hierarchy.
//!
//! Editing (1) on its own therefore leaves (2) and (3) describing a skeleton
//! that no longer exists — and because (3) is absolute, moving one node
//! invalidates it for that node *and its whole subtree*. Nothing in the engine
//! validates this, so it fails silently.
//!
//! # What is derivable
//!
//! Verified over all 450 CE skeletons / 4,486 nodes:
//!
//! - `inverse position` = `-(conj(world_rot) * world_translation)` — matches
//!   **every** node to 4e-6, with no per-skeleton variation.
//! - `inverse up` = `conj(world_rot) * +Z` — likewise universal.
//! - `inverse forward` / `inverse left` sit at a per-skeleton yaw offset within
//!   the XY plane (328 of 450 skeletons disagree with any single convention),
//!   so they are *not* regenerated from scratch here. They depend only on the
//!   node's world **rotation**, which means a translation-only edit leaves them
//!   correct as they stand.
//!
//! So a translation edit rebakes exactly. A rotation edit is detected — via the
//! universal up-axis identity — and reported in
//! [`RebakeReport::rotation_changed`] rather than being silently half-baked.

use crate::file::TagFile;
use crate::fields::TagFieldData;
use crate::math::{RealPoint3d, RealQuaternion, RealVector3d};

/// What a [`rebake_derived_node_data`] pass changed.
#[derive(Debug, Default, Clone)]
pub struct RebakeReport {
    /// Nodes examined.
    pub nodes: usize,
    /// `runtime node orientations` rows brought back in line with the nodes.
    pub orientations_updated: usize,
    /// Nodes whose `inverse position` was stale and has been recomputed.
    pub positions_updated: usize,
    /// Set when the tag carries no baked derived data at all (no matching
    /// `runtime node orientations` mirror) — an editing-kit source tag that
    /// tool.exe bakes at build time. Nothing was touched.
    pub unbaked: bool,
    /// Names of nodes whose stored basis disagrees with their current rotation,
    /// i.e. whose `default rotation` was edited. Their `inverse up` is fixed but
    /// `inverse forward` / `inverse left` carry a per-skeleton convention that
    /// cannot be reconstructed after the fact — surface these to the user.
    pub rotation_changed: Vec<String>,
}

impl RebakeReport {
    /// Whether anything at all was out of date.
    pub fn changed(&self) -> bool {
        self.orientations_updated > 0 || self.positions_updated > 0
    }
}

fn conj(q: RealQuaternion) -> RealQuaternion {
    RealQuaternion { i: -q.i, j: -q.j, k: -q.k, w: q.w }
}

fn close(a: RealVector3d, b: RealVector3d, eps: f32) -> bool {
    (a.i - b.i).abs() <= eps && (a.j - b.j).abs() <= eps && (a.k - b.k).abs() <= eps
}

/// Bring a skeleton's derived node data back in line with its rest pose.
///
/// Safe to run on a tag that is already consistent — it reports zero changes
/// and touches nothing. Does nothing to a tag with no `nodes` block.
///
/// See the module docs for exactly which fields are regenerated and why
/// `inverse forward` / `inverse left` are deliberately left alone.
pub fn rebake_derived_node_data(tag: &mut TagFile) -> RebakeReport {
    // Tolerance sits above the ~4e-6 spread the shipped bakes already show, so
    // an untouched skeleton reports clean instead of rewriting itself.
    const EPS: f32 = 1e-4;
    let mut report = RebakeReport::default();

    // Read the rest pose and the stored basis first; the writes come after.
    let (parents, local_rot, local_trans, names, stored_up) = {
        let root = tag.root();
        let Some(nodes) = root.field_path("nodes").and_then(|f| f.as_block()) else {
            return report;
        };
        let count = nodes.len();
        let mut parents = Vec::with_capacity(count);
        let mut local_rot = Vec::with_capacity(count);
        let mut local_trans = Vec::with_capacity(count);
        let mut names = Vec::with_capacity(count);
        let mut stored_up = Vec::with_capacity(count);
        for i in 0..count {
            let Some(node) = nodes.element(i) else { continue };
            parents.push(node.read_int_any("parent node").unwrap_or(-1) as i32);
            local_rot.push(node.read_quat("default rotation").normalized());
            let t = node.read_point3d("default translation");
            local_trans.push(RealVector3d { i: t.x, j: t.y, k: t.z });
            names.push(node.read_string_id("name").unwrap_or_default());
            stored_up.push(node.read_vec3("inverse up"));
        }
        (parents, local_rot, local_trans, names, stored_up)
    };
    let count = parents.len();
    report.nodes = count;
    if count == 0 {
        return report;
    }

    // The orientations mirror is tool.exe's own output, so its presence is the
    // signal that this tag has been baked at all. Editing-kit *source* tags
    // (e.g. every Reach `render_model` in an HREK tree) ship with the mirror
    // absent and the inverse matrices unwritten — tool.exe fills both in at
    // build time. Rebaking those would author derived data the toolchain is
    // supposed to own, so leave them completely alone.
    let baked = {
        let root = tag.root();
        root.field_path("runtime node orientations")
            .and_then(|f| f.as_block())
            .is_some_and(|b| b.len() == count)
    };
    if !baked {
        report.unbaked = true;
        return report;
    }

    // Compose world transforms. A parent always precedes its children in these
    // blocks; anything that doesn't is treated as a root rather than trusted.
    let mut world_rot = vec![RealQuaternion { i: 0.0, j: 0.0, k: 0.0, w: 1.0 }; count];
    let mut world_trans = vec![RealVector3d { i: 0.0, j: 0.0, k: 0.0 }; count];
    for i in 0..count {
        let parent = parents[i];
        if parent >= 0 && (parent as usize) < i {
            let p = parent as usize;
            world_rot[i] = (world_rot[p] * local_rot[i]).normalized();
            world_trans[i] = world_trans[p] + world_rot[p].rotate(local_trans[i]);
        } else {
            world_rot[i] = local_rot[i];
            world_trans[i] = local_trans[i];
        }
    }

    let up_axis = RealVector3d { i: 0.0, j: 0.0, k: 1.0 };
    let mut new_position = Vec::with_capacity(count);
    let mut new_up = Vec::with_capacity(count);
    for i in 0..count {
        let inverse = conj(world_rot[i]);
        let c = inverse.rotate(world_trans[i]);
        new_position.push(RealPoint3d { x: -c.i, y: -c.j, z: -c.k });
        let expected_up = inverse.rotate(up_axis);
        if !close(stored_up[i], expected_up, EPS) {
            report.rotation_changed.push(names[i].clone());
        }
        new_up.push(expected_up);
    }

    // Write the recomputed inverse bind data back onto the nodes.
    {
        let mut root = tag.root_mut();
        if let Some(mut field) = root.field_path_mut("nodes") {
            if let Some(mut nodes) = field.as_block_mut() {
                for i in 0..count {
                    let Some(mut node) = nodes.element_mut(i) else { continue };
                    let current = node.as_ref().read_point3d("inverse position");
                    let want = new_position[i];
                    let stale = (current.x - want.x).abs() > EPS
                        || (current.y - want.y).abs() > EPS
                        || (current.z - want.z).abs() > EPS;
                    if stale {
                        if let Some(mut f) = node.field_mut("inverse position") {
                            let _ = f.set(TagFieldData::RealPoint3d(want));
                        }
                        report.positions_updated += 1;
                    }
                    if !close(stored_up[i], new_up[i], EPS) {
                        if let Some(mut f) = node.field_mut("inverse up") {
                            let _ = f.set(TagFieldData::RealVector3d(new_up[i]));
                        }
                    }
                }
            }
        }
    }

    // Mirror the rest pose into the hidden runtime orientations block. It is a
    // straight duplicate, so it is rewritten rather than derived.
    {
        let mut root = tag.root_mut();
        let Some(mut field) = root.field_path_mut("runtime node orientations") else {
            return report;
        };
        let Some(mut orientations) = field.as_block_mut() else {
            return report;
        };
        // Length was already checked before anything was written.
        if orientations.len() != count {
            return report;
        }
        for i in 0..count {
            let Some(mut row) = orientations.element_mut(i) else { continue };
            let have_rot = row.as_ref().read_quat("rotation");
            let have_trans = row.as_ref().read_point3d("translation");
            let want_trans = RealPoint3d {
                x: local_trans[i].i,
                y: local_trans[i].j,
                z: local_trans[i].k,
            };
            let stale = (have_rot.i - local_rot[i].i).abs() > EPS
                || (have_rot.j - local_rot[i].j).abs() > EPS
                || (have_rot.k - local_rot[i].k).abs() > EPS
                || (have_rot.w - local_rot[i].w).abs() > EPS
                || (have_trans.x - want_trans.x).abs() > EPS
                || (have_trans.y - want_trans.y).abs() > EPS
                || (have_trans.z - want_trans.z).abs() > EPS;
            if !stale {
                continue;
            }
            if let Some(mut f) = row.field_mut("rotation") {
                let _ = f.set(TagFieldData::RealQuaternion(local_rot[i]));
            }
            if let Some(mut f) = row.field_mut("translation") {
                let _ = f.set(TagFieldData::RealPoint3d(want_trans));
            }
            report.orientations_updated += 1;
        }
    }

    report
}
