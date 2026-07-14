//! Opt-in regression for pasting a block element into a previously-EMPTY
//! Halo 2 classic block.
//!
//! A block that is empty *on disk* carries no `dfbt` block header (empty H2
//! blocks are headerless) and its `classic_block_header` is `None`. When an
//! element is pasted in, the block becomes non-empty and the encoder needs a
//! header to stamp the authoritative count/element_size. `paste_element` used
//! to skip `ensure_h2_classic_header_for_nonempty` (which `add_element`/
//! `insert_element` call), so the saved tag held the element bytes with no
//! leading header. On the next open the reader desynced and hit a nested
//! `dfbt` signature where a count belonged, reporting
//! `corrupt block header: count=1952605796 (== "dfbt") element_size=0`.
//!
//! Repro (matches the reported jackal.character -> jackal_ultra.character
//! "ready properties" paste): snapshot an element from a character whose
//! "ready properties" block is populated, paste it into a *different*
//! character whose "ready properties" block is empty, then write+reread. The
//! reread previously failed verification; after the fix the element survives.
//!
//! Skips silently when the local game tags aren't present on this machine.

use blam_tags::classic::read_classic_tag_file;
use blam_tags::layout::TagLayout;
use std::path::Path;

const TAGS: &str = "/Users/camden/Halo/halo2_mcc/tags";
const DEFS: &str = "../definitions/halo2_mcc";
// A character with a populated "ready properties" block (the copy source).
const SOURCE: &str = "objects/characters/jackal/ai/jackal.character";
// A character with an empty "ready properties" block (the paste target).
const TARGET: &str = "objects/characters/elite/ai/elite_major.character";
const BLOCK: &str = "ready properties";

fn layout(group: &str) -> Option<TagLayout> {
    TagLayout::from_json(Path::new(DEFS).join(format!("{group}.json"))).ok()
}

fn load_h2(rel: &str, group: &str) -> Option<blam_tags::file::TagFile> {
    let bytes = std::fs::read(Path::new(TAGS).join(rel)).ok()?;
    read_classic_tag_file(&bytes, layout(group)?).ok()
}

#[test]
fn paste_into_empty_on_disk_h2_block_roundtrips() {
    let (Some(source), Some(mut target)) =
        (load_h2(SOURCE, "character"), load_h2(TARGET, "character"))
    else {
        eprintln!("skip: halo2_mcc jackal/elite_major characters not present");
        return;
    };

    // Snapshot element 0 of the SOURCE's populated "ready properties" block.
    let snapshot = {
        let root = source.root();
        let Some(field) = root.field_path(BLOCK) else {
            eprintln!("skip: '{BLOCK}' did not resolve on the source character");
            return;
        };
        let block = field.as_block().expect("'ready properties' is a block");
        let Some(snapshot) = block.element_snapshot(0) else {
            eprintln!("skip: source character has an empty '{BLOCK}' block");
            return;
        };
        snapshot
    };

    // Confirm the TARGET's block really is empty on disk (the buggy state:
    // no elements and no `classic_block_header`), then paste into it.
    {
        let mut root = target.root_mut();
        let mut field = root.field_path_mut(BLOCK).expect("'ready properties' resolves");
        let mut block = field.as_block_mut().expect("'ready properties' is a block");
        assert_eq!(block.len(), 0, "target '{BLOCK}' must start empty for this repro");
        block.paste_element(0, &snapshot).expect("paste into empty-on-disk block");
    }

    // The write+reread must not fail verification with "corrupt block header".
    let bytes = target.write_to_bytes().expect("serialize classic tag");
    let rt = read_classic_tag_file(&bytes, layout("character").expect("layout"))
        .expect("reread after paste into empty-on-disk block must not corrupt");

    // And the pasted element must still be there.
    let count = rt
        .root()
        .field_path(BLOCK)
        .and_then(|f| f.as_block().map(|b| b.len()))
        .expect("'ready properties' resolves after reread");
    assert_eq!(count, 1, "the pasted element must survive the round-trip");
}
