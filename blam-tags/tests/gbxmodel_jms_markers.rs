//! A gbxmodel keeps its markers on its permutations, and a JMS built from one
//! has to carry them or `tool` rebuilds a model with no grips or hands.
//!
//! Needs the kit: set `BLAM_TEST_HCEEK` to its root.

mod common;

use blam_tags::classic::read_classic_tag_file;
use blam_tags::jms::JmsFile;
use blam_tags::layout::TagLayout;

#[test]
fn a_gbxmodel_jms_keeps_its_markers_in_the_jms_convention() {
    let Some(kit) = common::kit("BLAM_TEST_HCEEK") else {
        eprintln!("skipping: BLAM_TEST_HCEEK is not set");
        return;
    };
    let bytes = std::fs::read(kit.tags().join("characters/cyborg/cyborg.gbxmodel")).unwrap();
    let layout = TagLayout::from_json(common::definitions("haloce_mcc").join("gbxmodel.json")).unwrap();
    let tag = read_classic_tag_file(&bytes, layout).unwrap();
    let jms = JmsFile::from_gbxmodel(&tag).unwrap();

    let names: Vec<_> = jms.markers.iter().map(|marker| marker.name.as_str()).collect();
    assert!(names.contains(&"left hand"), "{names:?}");
    assert!(names.contains(&"right hand"), "{names:?}");

    // The tag's own value, to tell the convention from a copy.
    let root = tag.root();
    let regions = root.field("regions").and_then(|field| field.as_block()).unwrap();
    let stored = (0..regions.len())
        .flat_map(|region| {
            let region = regions.element(region).unwrap();
            let permutations = region.field("permutations").and_then(|f| f.as_block()).unwrap();
            let markers = (0..permutations.len()).flat_map(|permutation| {
                let permutation = permutations.element(permutation).unwrap();
                let block = permutation.field("markers").and_then(|f| f.as_block()).unwrap();
                (0..block.len()).map(|marker| block.element(marker).unwrap()).collect::<Vec<_>>()
            });
            markers.collect::<Vec<_>>()
        })
        .find(|marker| marker.read_string("name").as_deref() == Some("left hand"))
        .unwrap();
    let stored = stored.read_quat("rotation");
    let marker = jms.markers.iter().find(|marker| marker.name == "left hand").unwrap();
    assert_eq!(marker.rotation, stored.conjugate());
    assert_ne!(marker.rotation, stored);

    // `tool` takes the 8200 value as is, so that one is what the tag stores.
    let mut text = Vec::new();
    jms.write(&mut text, 8200).unwrap();
    let text = String::from_utf8(text).unwrap();
    let lines: Vec<_> = text.lines().collect();
    let at = lines.iter().position(|line| *line == "left hand").unwrap();
    let written: Vec<f32> =
        lines[at + 3].split_whitespace().map(|value| value.parse().unwrap()).collect();
    let want = stored.to_array();
    assert!(written.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5), "{written:?} vs {want:?}");
}
