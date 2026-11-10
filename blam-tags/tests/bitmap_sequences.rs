//! A bitmap's sequences and sprites as [`BitmapDefinition`] reads them: both
//! blocks kept apart, the frame rule the game uses, and the GPU particle frame
//! list. Expected values are the tags' own fields.
//!
//! The kit tests need `BLAM_TEST_HCEEK` and `BLAM_TEST_H3EK` set to the kits'
//! roots.

mod common;

use blam_tags::bitmap::{Bitmap, BitmapDefinition, BitmapSequence, BitmapSprite, GPU_PARTICLE_MOST_FRAMES};
use blam_tags::classic::read_classic_tag_file;
use blam_tags::file::TagFile;
use blam_tags::layout::TagLayout;

fn load_halo_ce(rel: &str) -> Option<TagFile> {
    let tags = common::kit("BLAM_TEST_HCEEK")?.tags();
    let bytes = std::fs::read(tags.join(rel)).unwrap();
    let layout = TagLayout::from_json(common::definitions("haloce_mcc").join("bitmap.json")).unwrap();
    Some(read_classic_tag_file(&bytes, layout).unwrap())
}

fn load_halo_3(rel: &str) -> Option<TagFile> {
    Some(TagFile::read(common::kit("BLAM_TEST_H3EK")?.tags().join(rel)).unwrap())
}

fn sprite(bitmap_index: i16) -> BitmapSprite {
    BitmapSprite { bitmap_index, ..Default::default() }
}

fn sequence(first_bitmap_index: i16, bitmap_count: i16, sprites: Vec<BitmapSprite>) -> BitmapSequence {
    BitmapSequence { first_bitmap_index, bitmap_count, sprites, ..Default::default() }
}

#[test]
fn a_run_of_images_steps_from_its_first_and_wraps() {
    let run = sequence(3, 4, vec![sprite(9)]);
    assert_eq!(run.frame_bitmap(0), 3);
    assert_eq!(run.frame_bitmap(2), 5);
    assert_eq!(run.frame_bitmap(5), 4);
}

#[test]
fn without_a_run_a_frame_is_its_sprites_image() {
    let sprites = sequence(0, 0, vec![sprite(2), sprite(7)]);
    assert_eq!(sprites.frame_bitmap(1), 7);
    assert_eq!(sprites.frame_bitmap(2), 2);
    assert_eq!(sprites.frame_count(), 2);
}

#[test]
fn an_empty_sequence_shows_the_frame_itself() {
    let empty = sequence(-1, 0, Vec::new());
    assert_eq!(empty.frame_bitmap(0), 0);
    assert_eq!(empty.frame_bitmap(6), 6);
    assert_eq!(empty.frame_count(), 0);
    assert_eq!(sequence(0, 21, Vec::new()).frame_count(), 21);
}

#[test]
fn only_a_real_name_is_shown() {
    let named = |name: &str| BitmapSequence { name: name.to_owned(), ..Default::default() };
    assert_eq!(named("small").display_name(), Some("small"));
    assert_eq!(named("import #8").display_name(), None);
    assert_eq!(named("").display_name(), None);
    assert_eq!(named("import #fire").display_name(), Some("import #fire"));
}

#[test]
fn a_sprites_registration_hangs_from_its_top_left() {
    let sprite = BitmapSprite {
        bitmap_index: 0,
        left: 0.25,
        right: 0.5,
        top: 0.5,
        bottom: 1.0,
        registration_point: [0.125, 0.375],
    };
    assert_eq!(sprite.rect(), [0.25, 0.5, 0.5, 1.0]);
    assert_eq!(sprite.registration_uv(), [0.375, 0.875]);
    assert_eq!(sprite.pixel_rect(256, 128), [64.0, 128.0, 64.0, 128.0]);
}

#[test]
fn the_particle_frame_list_takes_manual_sequences_first_at_the_next_index() {
    let named = |name: &str| BitmapSequence { name: name.to_owned(), ..Default::default() };
    let mut definition = BitmapDefinition {
        sequences: vec![named("a"), named("b"), named("c")],
        ..Default::default()
    };
    assert_eq!(definition.gpu_frame_sequence(0).unwrap().name, "b");
    assert_eq!(definition.gpu_frame_sequence(2).unwrap().name, "a");
    definition.manual_sequences = vec![named("m0"), named("m1")];
    assert_eq!(definition.gpu_frame_sequence(0).unwrap().name, "m1");
    assert_eq!(definition.gpu_frame_sequence(1).unwrap().name, "m0");
    assert!(BitmapDefinition::default().gpu_frame_sequence(0).is_none());
    assert_eq!(GPU_PARTICLE_MOST_FRAMES, 15);
}

/// 25 one-sprite sequences over four pages; sequence 7's sprite is on the
/// second, and from sequence 8 on tool named them `import #N`.
#[test]
fn halo_ce_sprite_sheet_reads_every_sequence() {
    let Some(tag) = load_halo_ce("ui/hud/bitmaps/combined/hud_msg_icons.bitmap") else {
        return;
    };
    let definition = BitmapDefinition::from_tag(&tag);
    assert_eq!(definition.sprite_spacing, 4);
    assert_eq!(definition.sequences.len(), 25);
    assert!(definition.manual_sequences.is_empty());
    assert_eq!(definition.images.len(), 4);
    assert_eq!((definition.images[3].width, definition.images[3].height), (512, 256));
    let seventh = &definition.sequences[7];
    assert_eq!(
        seventh.sprites,
        [BitmapSprite {
            bitmap_index: 1,
            left: 0.0,
            right: 0.738_281_25,
            top: 0.0,
            bottom: 0.324_218_75,
            registration_point: [0.369_140_625, 0.212_890_625],
        }]
    );
    assert_eq!(seventh.frame_bitmap(0), 1);
    assert_eq!(definition.sequences[3].frame_bitmap(0), 0);
    assert_eq!(definition.sequences[8].name, "import #8");
    assert_eq!(definition.sequences[8].display_name(), None);
    assert_eq!(Bitmap::new(&tag).unwrap().sequences(), definition.sequences);
}

/// One sequence whose eight sprites are each on their own page.
#[test]
fn halo_ce_sprites_can_spread_over_pages() {
    let Some(tag) = load_halo_ce("effects/particles/energy/bitmaps/shield c generic.bitmap") else {
        return;
    };
    let definition = BitmapDefinition::from_tag(&tag);
    let only = &definition.sequences[0];
    assert_eq!((only.first_bitmap_index, only.bitmap_count), (0, 0));
    assert_eq!((0..8).map(|frame| only.frame_bitmap(frame)).collect::<Vec<_>>(), [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(only.frame_bitmap(9), 1);
}

/// Sequence 62 of the medals is empty (`first -1`, no count, no sprites).
#[test]
fn halo_3_empty_sequence_has_no_frames() {
    let Some(tag) = load_halo_3("ui/chud/bitmaps/medals/medals.bitmap") else {
        return;
    };
    let definition = BitmapDefinition::from_tag(&tag);
    let empty = &definition.sequences[62];
    assert_eq!((empty.first_bitmap_index, empty.bitmap_count), (-1, 0));
    assert_eq!(empty.frame_count(), 0);
    assert_eq!(definition.sequences[0].frame_count(), 1);
}

/// A plate with only manual sequences: the processed block stays empty, even
/// through [`Bitmap::sequences`], and the particle frames come from the manual
/// one.
#[test]
fn halo_3_manual_sequences_stay_apart() {
    let Some(tag) = load_halo_3("fx/particles/atmospheric/_bitmaps/rolling_smoke_plate.bitmap") else {
        return;
    };
    let definition = BitmapDefinition::from_tag(&tag);
    assert!(definition.sequences.is_empty());
    assert_eq!(definition.sprite_spacing, -1);
    assert_eq!(definition.manual_sequences.len(), 1);
    let small = &definition.manual_sequences[0];
    assert_eq!(small.display_name(), Some("small"));
    assert_eq!(small.sprites.len(), 4);
    assert_eq!(small.sprites[1].rect(), [6.145_025_730_133_057, 5.795_025_825_500_488, 6.239_089_965_820_312_5, 5.889_090_061_187_744]);
    assert_eq!(definition.gpu_frame_sequence(0), Some(small));
    assert!(Bitmap::new(&tag).unwrap().sequences().is_empty());
}
