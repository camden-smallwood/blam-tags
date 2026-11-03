//! Reach's extended block sections (`ik_chain_*`) found by their `data sizes`
//! and decoded through the curve codec's streams. `BLAM_TEST_HREK` to the
//! kit's root.

mod common;

use blam_tags::animation::{decode_extended_block, Animation, ExtendedChannel};
use blam_tags::TagFile;

/// The Spartan's `combat:rifle:idle` holds its hands to the gun's markers:
/// the left by identity, the right by the offset its own clip has from the
/// grip, both at full weight, every frame alike.
#[test]
fn the_spartans_rifle_idle_reaches_for_the_gun() {
    let Some(kit) = common::kit("BLAM_TEST_HREK") else {
        eprintln!("skipping: BLAM_TEST_HREK is not set");
        return;
    };
    let path = kit.tags().join("objects/characters/spartans/spartans.model_animation_graph");
    let tag = TagFile::read(path).unwrap();
    let graph = Animation::new(&tag).unwrap();
    let idle = graph.find("combat:rifle:idle").expect("the rifle idle");
    let events = decode_extended_block(idle.data_section("ik_chain_event_data").expect("event data")).unwrap();
    assert_eq!(events.len(), 6, "two orientations of three entries");
    let close = |a: f32, b: f32| (a - b).abs() < 1e-3;
    match (&events[0], &events[3], &events[4], &events[5]) {
        (
            ExtendedChannel::Rotation(left),
            ExtendedChannel::Rotation(rotation),
            ExtendedChannel::Translation(translation),
            ExtendedChannel::Scalar(scale),
        ) => {
            assert_eq!(rotation.len(), 156);
            let identity = left[0];
            assert!(close(identity.w.abs(), 1.0), "{identity:?}");
            let q = rotation[0];
            assert!(close(q.i, -0.68565) && close(q.j, 0.00965) && close(q.k, 0.10404) && close(q.w, -0.72039), "{q:?}");
            let t = translation[0];
            assert!(close(t.x, -0.039946) && close(t.y, -0.004624) && close(t.z, 0.010731), "{t:?}");
            assert!(close(scale[0], 1.0) && close(scale[155], 1.0));
            assert_eq!(rotation[0], rotation[155], "constant");
        }
        other => panic!("{other:?}"),
    }
    let weights = decode_extended_block(idle.data_section("ik_chain_control_data").expect("weights")).unwrap();
    assert_eq!(weights.len(), 2);
    for weight in &weights {
        let ExtendedChannel::Scalar(values) = weight else { panic!("{weight:?}") };
        assert!(values.iter().all(|value| close(*value, 1.0)));
    }
    assert!(idle.data_section("ik_chain_proxy_data").is_none(), "the Spartan has no proxies");
}
