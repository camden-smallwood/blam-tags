//! `particle_model` → JMI + per-object JMS reconstruction holds up on
//! shipped tags.
//!
//! The reconstruction rests on two claims that the tag data does not
//! state outright, so both are pinned here by a measurement that would
//! move if either were wrong:
//!
//! 1. **The gen3 index buffer is a triangle strip**, even though 224 of
//!    the 266 shipped `pmdf` tags declare `index buffer type = DEFAULT`
//!    rather than `triangle strip`.
//! 2. **The strip must be cut at `m_gpu_data/m_variants` boundaries.**
//!    There is no `0xFFFF` restart between objects, so decoding the
//!    whole buffer as one strip invents triangles that bridge unrelated
//!    objects.
//!
//! The discriminator is the mean dot product between each triangle's
//! geometric face normal and the average of its three stored vertex
//! normals. A correct decode lands near +1; a wrong winding, a wrong
//! primitive type, or an uncut strip lands near 0. Measured over the
//! shipped corpus the three interpretations separate cleanly — e.g. on
//! Reach's 7-object `glass_fragments`: per-variant +0.994, whole-buffer
//! +0.015, triangle-list +0.108.
//!
//! Halo 2's `PRTM` is a different tag and a different decode (a
//! triangle **list**, uncompressed vertices) and additionally carries
//! the original object names, so it is asserted separately.
//!
//! Skips silently when the corresponding tag set is absent.

use std::path::{Path, PathBuf};

use blam_tags::particle_model::read_particle_model;
use blam_tags::{JmsFile, TagFile};

/// Root of an extracted MCC tag set, via `BLAM_TEST_<KIT>_TAGS` or the
/// conventional local layout.
fn kit_tags(kit: &str) -> Option<PathBuf> {
    let var = format!("BLAM_TEST_{}_TAGS", kit.to_uppercase());
    if let Ok(p) = std::env::var(&var) {
        let p = PathBuf::from(p);
        return p.is_dir().then_some(p);
    }
    let home = std::env::var("HOME").ok()?;
    let p = PathBuf::from(home).join("Halo").join(format!("{kit}_mcc")).join("tags");
    p.is_dir().then_some(p)
}

/// Mean dot(face normal, averaged vertex normal) over a JMS's
/// triangles. `None` when nothing measurable survived.
fn face_normal_agreement(jms: &JmsFile) -> Option<f32> {
    let mut total = 0.0f64;
    let mut n = 0usize;
    for t in &jms.triangles {
        let [a, b, c] = t.v;
        let (va, vb, vc) = (
            jms.vertices.get(a as usize)?,
            jms.vertices.get(b as usize)?,
            jms.vertices.get(c as usize)?,
        );
        let (pa, pb, pc) = (va.position, vb.position, vc.position);
        let u = [pb.x - pa.x, pb.y - pa.y, pb.z - pa.z];
        let v = [pc.x - pa.x, pc.y - pa.y, pc.z - pa.z];
        let fnl = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        let fl = (fnl[0] * fnl[0] + fnl[1] * fnl[1] + fnl[2] * fnl[2]).sqrt();
        if fl < 1e-12 {
            continue;
        }
        let vn = [
            (va.normal.i + vb.normal.i + vc.normal.i) / 3.0,
            (va.normal.j + vb.normal.j + vc.normal.j) / 3.0,
            (va.normal.k + vb.normal.k + vc.normal.k) / 3.0,
        ];
        let vl = (vn[0] * vn[0] + vn[1] * vn[1] + vn[2] * vn[2]).sqrt();
        if vl < 1e-12 {
            continue;
        }
        total += (0..3).map(|k| (fnl[k] / fl) * (vn[k] / vl)).sum::<f32>() as f64;
        n += 1;
    }
    (n > 0).then(|| (total / n as f64) as f32)
}

/// Read a tag, routing Halo 2's classic format through the JSON
/// definition. Classic tags carry no embedded `blay`, so `TagFile::read`
/// rejects them ("expected BLAM, got BMAL" — the byte-swapped signature
/// is the tell); they need a layout synthesized from
/// `definitions/<game>/<group>.json`.
fn read(path: &Path, game: &str) -> TagFile {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    if blam_tags::classic::ClassicHeader::parse(&bytes).is_some() {
        let def = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("definitions")
            .join(game)
            .join("particle_model.json");
        let layout = blam_tags::layout::TagLayout::from_json(&def)
            .unwrap_or_else(|e| panic!("load {}: {e}", def.display()));
        return blam_tags::classic::read_classic_tag_file(&bytes, layout)
            .unwrap_or_else(|e| panic!("decode classic {}: {e}", path.display()));
    }
    TagFile::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn stem(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().into_owned()
}

/// Reach's `glass_fragments` is the sharpest gen3 case: 7 objects
/// packed into one strip, so an uncut decode collapses to noise.
#[test]
fn gen3_multi_object_strip_splits_at_variant_boundaries() {
    let Some(tags) = kit_tags("haloreach") else { return };
    let path = tags.join("fx/particles/models/debris/glass_fragments/glass_fragments.particle_model");
    if !path.is_file() {
        return;
    }
    let tag = read(&path, "haloreach_mcc");
    let src = read_particle_model(&tag, &stem(&path)).expect("reconstruct");

    assert_eq!(src.objects.len(), 7, "glass_fragments ships 7 objects");
    assert_eq!(src.jmi.objects.len(), src.objects.len(), "manifest indexes every object");
    assert!(!src.names_are_authentic(), "gen3 stores no object names");

    for o in &src.objects {
        assert!(!o.jms.triangles.is_empty(), "object `{}` decoded empty", o.name);
        let score = face_normal_agreement(&o.jms).expect("measurable");
        assert!(
            score > 0.6,
            "object `{}` scored {score:.3} — an uncut strip or a list decode \
             scores near 0 here, so this is a decode regression",
            o.name,
        );
    }
}

/// The gen3 shape that declares `index buffer type = DEFAULT` (the
/// majority) must still decode as a strip.
#[test]
fn gen3_default_index_type_still_decodes_as_strip() {
    let Some(tags) = kit_tags("halo3") else { return };
    let path = tags.join("fx/particles/models/weapons/carbine_clip/carbine_clip.particle_model");
    if !path.is_file() {
        return;
    }
    let tag = read(&path, "halo3_mcc");
    let src = read_particle_model(&tag, &stem(&path)).expect("reconstruct");
    let jms = &src.objects.first().expect("one object").jms;
    let score = face_normal_agreement(jms).expect("measurable");
    assert!(score > 0.6, "carbine_clip scored {score:.3} — expected strip decode");
    // A strip of N indices yields ~N-2 triangles; a list would yield N/3.
    assert!(
        jms.triangles.len() > 100,
        "only {} triangles — that is list-decode magnitude, not strip",
        jms.triangles.len(),
    );
}

/// Halo 2's `PRTM` is the only engine that keeps the source object
/// names, and its indices are a triangle list rather than a strip.
#[test]
fn halo2_recovers_object_names_and_decodes_as_list() {
    let Some(tags) = kit_tags("halo2") else { return };
    let path = tags.join("effects/particle_models/urban_debris/urban_debris.particle_model");
    if !path.is_file() {
        return;
    }
    let tag = read(&path, "halo2_mcc");
    let src = read_particle_model(&tag, &stem(&path)).expect("reconstruct");

    assert!(src.names_are_authentic(), "Halo 2 stores `models[].model name`");
    assert_eq!(
        src.jmi.objects,
        vec![
            "can_1", "can_2", "can_3", "can_4", "can_5",
            "paper_1", "paper_2", "paper_3", "butt_1", "butt_2",
        ],
        "the shipped names, in `models[]` order",
    );
    for o in &src.objects {
        let score = face_normal_agreement(&o.jms).expect("measurable");
        assert!(score > 0.6, "object `{}` scored {score:.3}", o.name);
    }
}

/// Every object listed in the manifest must have geometry behind it —
/// a JMI line with no JMS is an import failure, not a warning.
#[test]
fn manifest_and_objects_stay_in_step_across_kits() {
    let cases = [
        ("halo3", "fx/particles/models/debris/ice_shards/ice_shards.particle_model"),
        ("haloreach", "fx/particles/models/debris/generic_shards/generic_shards.particle_model"),
        ("halo4", "fx/particles/models/fx_mesh_plane/fx_mesh_plane.particle_model"),
        ("halo2", "effects/particle_models/leaves/alder_leaves.particle_model"),
    ];
    let mut checked = 0;
    for (kit, rel) in cases {
        let Some(tags) = kit_tags(kit) else { continue };
        let path = tags.join(rel);
        if !path.is_file() {
            continue;
        }
        let tag = read(&path, &format!("{kit}_mcc"));
        let src = read_particle_model(&tag, &stem(&path)).expect("reconstruct");
        assert_eq!(
            src.jmi.objects,
            src.objects.iter().map(|o| o.name.clone()).collect::<Vec<_>>(),
            "{kit}/{rel}: manifest lines must match emitted objects exactly",
        );
        for (i, o) in src.objects.iter().enumerate() {
            assert_eq!(
                src.jmi.object_jms_path(i).unwrap(),
                format!("{}/render/{}.jms", o.name, o.name),
                "{kit}/{rel}: manifest path must match Tool's layout",
            );
        }
        checked += 1;
    }
    let _ = checked;
}
