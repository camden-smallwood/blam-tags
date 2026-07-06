//! Opt-in regression check for the `RealPoint2d` interpolant arm in
//! `effects_properties::read_interpolant`. Contrail `profile offset` is a
//! `real_point_2d` typed property; before the fix the shared reader only
//! handled point3d/vector3d/spherical, so every 2D offset interpolant
//! silently collapsed to `None`. This scans real H3 contrail tags and
//! asserts at least one carries a populated 2D offset interpolant.
//!
//! Skips silently when the tag tree isn't present on this machine.

use blam_tags::contrail_system::ContrailSystem;
use blam_tags::file::TagFile;

const H3_CONTRAIL_DIR: &str = "/Users/camden/Halo/halo3_mcc/tags";

fn scan_dir(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_dir(&p, out);
        } else if p.extension().and_then(|x| x.to_str()) == Some("contrail_system") {
            out.push(p);
        }
    }
}

#[test]
fn contrail_profile_offset_2d_interpolant_populates() {
    let root = std::path::Path::new(H3_CONTRAIL_DIR);
    if !root.exists() {
        eprintln!("skip: {H3_CONTRAIL_DIR} not present");
        return;
    }

    let mut tags = Vec::new();
    scan_dir(root, &mut tags);
    if tags.is_empty() {
        eprintln!("skip: no .contrail_system tags found");
        return;
    }

    let mut typed_offsets = 0usize;
    let mut sample = None;
    for path in &tags {
        let Ok(tag) = TagFile::read(path) else { continue };
        let Ok(cs) = ContrailSystem::from_tag(&tag) else { continue };
        for def in &cs.definitions {
            if let Some(start) = def.profile_offset.starting_interpolant {
                typed_offsets += 1;
                if sample.is_none() {
                    sample = Some((path.clone(), start, def.profile_offset.ending_interpolant));
                }
            }
        }
    }

    eprintln!(
        "scanned {} contrail tags; {} had a populated 2D profile-offset interpolant",
        tags.len(),
        typed_offsets
    );
    if let Some((p, start, end)) = &sample {
        eprintln!("sample: {}", p.display());
        eprintln!("  start interpolant = ({}, {}, {})", start.i, start.j, start.k);
        if let Some(e) = end {
            eprintln!("  end   interpolant = ({}, {}, {})", e.i, e.j, e.k);
        }
        // 2D offset stores into i/j with k pinned to 0.
        assert_eq!(start.k, 0.0, "2D interpolant must pin k=0");
    }

    assert!(
        typed_offsets > 0,
        "expected at least one contrail with a typed 2D profile-offset interpolant \
         (RealPoint2d arm regressed?)"
    );
}
