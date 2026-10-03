//! Malformed-input tests for [`TagFile::read_from_bytes`]: a layout that
//! refers to itself or past its own tables, sizes and counts far larger than
//! the file, blocks nested past any real tag, and a root block with no
//! element. Each must come back as an `Err` — not a panic, not a stack
//! overflow, not an allocation the size of the lie.
//!
//! These live in their own test binary on purpose. A stack overflow or a
//! failed allocation aborts the process rather than unwinding, which no
//! `catch_unwind` can turn into a test failure; here it kills this binary,
//! and cargo reports the binary as failed. A counting global allocator
//! records the largest single allocation each read asks for, so a size field
//! that is trusted shows up even on a system that would lazily grant it.
//!
//! Every input is built from a definition-generated tag (no shipped tag
//! files): a Halo 3 GUI model widget, whose root struct holds a string id,
//! a tag reference and the `camera settings` block.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::{BufReader, Cursor};

use blam_tags::{TagFieldType, TagFile, TagLayout, TagReadError};

struct Counting;

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(layout.size())));
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(layout.size())));
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(new_size)));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const SCHEMA: &str = "../definitions/halo3_mcc/gui_model_widget_definition.json";

/// Where the `blay` chunk starts: after the 64-byte file header and the
/// 12-byte `tag!` chunk header.
const BLAY: usize = 76;

/// Read `bytes`, returning the result and the largest single allocation the
/// read asked for.
fn read(bytes: &[u8]) -> (Result<TagFile, TagReadError>, usize) {
    LARGEST.with(|largest| largest.set(0));
    let result = TagFile::read_from_bytes(bytes);
    (result, LARGEST.with(Cell::get))
}

fn baseline() -> Vec<u8> {
    TagFile::new(SCHEMA).expect("build widget").write_to_bytes().expect("write widget")
}

/// Source-literal signature to its on-disk (little-endian) bytes.
fn on_disk(signature: &[u8; 4]) -> [u8; 4] {
    [signature[3], signature[2], signature[1], signature[0]]
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn put_u32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// The end of the chunk whose header starts at `at`.
fn chunk_end(bytes: &[u8], at: usize) -> usize {
    at + 12 + u32_at(bytes, at + 8) as usize
}

/// The tag's own layout, as read back from its `blay` chunk.
fn layout_of(bytes: &[u8]) -> TagLayout {
    let mut reader = BufReader::new(Cursor::new(&bytes[BLAY..]));
    TagLayout::read(&mut reader, blam_tags::Endian::Le).expect("baseline layout")
}

/// `bytes` with its `blay` chunk replaced by `layout`, written out.
fn with_layout(bytes: &[u8], layout: &TagLayout) -> Vec<u8> {
    let mut blay = Vec::new();
    layout.write(&mut blay).unwrap();
    let mut out = bytes[..BLAY].to_vec();
    out.extend_from_slice(&blay);
    out.extend_from_slice(&bytes[chunk_end(bytes, BLAY)..]);
    out
}

fn root_struct(layout: &TagLayout) -> usize {
    layout.block_layouts[layout.header.tag_group_block_index as usize].struct_index as usize
}

/// The field indices of `struct_index`, up to its terminator.
fn fields_of(layout: &TagLayout, struct_index: usize) -> std::ops::Range<usize> {
    let first = layout.struct_layouts[struct_index].first_field_index as usize;
    let mut end = first;
    while layout.fields[end].field_type != TagFieldType::Terminator {
        end += 1;
    }
    first..end
}

fn field_of_type(layout: &TagLayout, struct_index: usize, field_type: TagFieldType) -> usize {
    fields_of(layout, struct_index)
        .find(|&i| layout.fields[i].field_type == field_type)
        .unwrap_or_else(|| panic!("root has no {field_type:?} field"))
}

#[test]
fn the_baseline_reads() {
    let (result, largest) = read(&baseline());
    let tag = result.expect("baseline");
    assert!(tag.root().field("camera settings").is_some());
    assert!(largest < 1 << 20, "baseline allocated {largest}");
}

/// A struct field naming the struct it sits in. Laying it out recursed
/// forever and overflowed the stack, which aborts the process.
#[test]
fn a_struct_that_contains_itself_is_refused() {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    let root = root_struct(&layout);
    let struct_type = layout
        .field_types
        .iter()
        .position(|t| layout.get_string(t.name_offset) == Some("struct"))
        .expect("a struct field type") as u32;
    let first = layout.struct_layouts[root].first_field_index as usize;
    layout.fields[first].type_index = struct_type;
    layout.fields[first].definition = root as u32;

    let (result, _) = read(&with_layout(&bytes, &layout));
    assert!(matches!(result, Err(TagReadError::InvalidLayout { .. })), "{:?}", result.err());
}

/// A block field naming a block layout that does not exist. The data reader
/// indexed the table with it and panicked.
#[test]
fn a_block_past_the_end_of_its_table_is_refused() {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    let block = field_of_type(&layout, root_struct(&layout), TagFieldType::Block);
    layout.fields[block].definition = 9999;

    let (result, _) = read(&with_layout(&bytes, &layout));
    assert!(matches!(result, Err(TagReadError::InvalidLayout { .. })), "{:?}", result.err());
}

#[test]
fn a_root_block_past_the_end_of_its_table_is_refused() {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    layout.header.tag_group_block_index = 9999;

    let (result, _) = read(&with_layout(&bytes, &layout));
    assert!(matches!(result, Err(TagReadError::InvalidLayout { .. })), "{:?}", result.err());
}

#[test]
fn a_struct_without_a_terminator_is_refused() {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    let pad_type = layout
        .field_types
        .iter()
        .position(|t| layout.get_string(t.name_offset) == Some("pad"))
        .unwrap_or(0) as u32;
    for field in &mut layout.fields {
        if field.field_type == TagFieldType::Terminator {
            field.type_index = pad_type;
            field.definition = 0;
        }
    }

    let (result, _) = read(&with_layout(&bytes, &layout));
    assert!(matches!(result, Err(TagReadError::InvalidLayout { .. })), "{:?}", result.err());
}

/// A field type index past the field-type table.
#[test]
fn a_field_type_past_the_end_of_its_table_is_refused() {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    let first = layout.struct_layouts[root_struct(&layout)].first_field_index as usize;
    layout.fields[first].type_index = 9999;

    let (result, _) = read(&with_layout(&bytes, &layout));
    assert!(matches!(result, Err(TagReadError::InvalidLayout { .. })), "{:?}", result.err());
}

/// The root block claiming four billion elements in a file of a few hundred
/// bytes. The element bytes were allocated before being read.
#[test]
fn a_block_count_larger_than_the_file_allocates_nothing_for_it() {
    let mut bytes = baseline();
    let bdat = chunk_end(&bytes, BLAY);
    let tgbl = bdat + 12;
    assert_eq!(bytes[tgbl..tgbl + 4], on_disk(b"tgbl"));
    put_u32(&mut bytes, tgbl + 12, u32::MAX);

    let (result, largest) = read(&bytes);
    assert!(matches!(result, Err(TagReadError::SizeExceedsInput { .. })), "{:?}", result.err());
    assert!(largest < 1 << 20, "a {}-byte file asked for {largest} bytes", bytes.len());
}

/// A leaf chunk (the root's string id) claiming four gigabytes.
#[test]
fn a_leaf_chunk_larger_than_the_file_allocates_nothing_for_it() {
    let mut bytes = baseline();
    let tgsi = bytes.windows(4).position(|w| w == on_disk(b"tgsi")).expect("a string id chunk");
    put_u32(&mut bytes, tgsi + 8, u32::MAX - 64);

    let (result, largest) = read(&bytes);
    assert!(matches!(result, Err(TagReadError::SizeExceedsInput { .. })), "{:?}", result.err());
    assert!(largest < 1 << 20, "a {}-byte file asked for {largest} bytes", bytes.len());
}

/// The layout's string table claiming four gigabytes (the `str*` chunk agrees,
/// as the reader checks).
#[test]
fn a_string_table_larger_than_the_file_allocates_nothing_for_it() {
    let mut bytes = baseline();
    // Payload header: root data size, guid, version; then the layout header,
    // whose second word (after the root block index) is the string data size.
    let string_data_size = BLAY + 12 + 24 + 4;
    let original = u32_at(&bytes, string_data_size);
    let str_chunk = bytes.windows(4).position(|w| w == on_disk(b"str*")).expect("str*");
    assert_eq!(u32_at(&bytes, str_chunk + 8), original);
    put_u32(&mut bytes, string_data_size, u32::MAX - 64);
    put_u32(&mut bytes, str_chunk + 8, u32::MAX - 64);

    let (result, largest) = read(&bytes);
    assert!(matches!(result, Err(TagReadError::SizeExceedsInput { .. })), "{:?}", result.err());
    assert!(largest < 1 << 20, "a {}-byte file asked for {largest} bytes", bytes.len());
}

/// A table count far past the layout's bytes, with its chunk size to match.
#[test]
fn a_field_count_larger_than_the_file_allocates_nothing_for_it() {
    let mut bytes = baseline();
    let field_count = BLAY + 12 + 24 + 4 * 8;
    let gras = bytes.windows(4).position(|w| w == on_disk(b"gras")).expect("gras");
    assert_eq!(u32_at(&bytes, gras + 8), 12 * u32_at(&bytes, field_count));
    put_u32(&mut bytes, field_count, 0x1000_0000);
    put_u32(&mut bytes, gras + 8, 0xC000_0000);

    let (result, largest) = read(&bytes);
    assert!(matches!(result, Err(TagReadError::SizeExceedsInput { .. })), "{:?}", result.err());
    assert!(largest < 1 << 20, "a {}-byte file asked for {largest} bytes", bytes.len());
}

/// A root block with no element. It read, and `root()` then panicked.
#[test]
fn an_empty_root_block_is_refused() {
    let bytes = baseline();
    let bdat = chunk_end(&bytes, BLAY);
    let tgbl = bdat + 12;
    let flags = u32_at(&bytes, tgbl + 16);

    let mut out = bytes[..bdat].to_vec();
    // bdat (version 1) holding a tgbl of no elements: count and flags only.
    out.extend_from_slice(&on_disk(b"bdat"));
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&on_disk(b"tgbl"));
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&flags.to_le_bytes());
    out.extend_from_slice(&bytes[chunk_end(&bytes, bdat)..]);

    let (result, _) = read(&out);
    assert!(matches!(result, Err(TagReadError::EmptyRootBlock)), "{:?}", result.err());
}

/// The root's `camera settings` block pointed back at the root struct, and
/// data nesting it `levels` deep — legal in the layout (a block holds its
/// elements by reference), and each level costs real bytes.
fn nested_blocks(levels: usize) -> Vec<u8> {
    let bytes = baseline();
    let mut layout = layout_of(&bytes);
    let root = root_struct(&layout);
    let block_field = field_of_type(&layout, root, TagFieldType::Block);
    let camera = layout.fields[block_field].definition as usize;
    layout.block_layouts[camera].struct_index = root as u32;
    let bytes = with_layout(&bytes, &layout);

    // The root tgbl: header, count, flags, the element's raw bytes, then its
    // tgst, whose last sub-chunk is the empty camera settings tgbl.
    let bdat = chunk_end(&bytes, BLAY);
    let tgbl = bdat + 12;
    let tgbl_end = chunk_end(&bytes, tgbl);
    let element_size = layout.struct_layouts[root].size;
    let tgst = tgbl + 20 + element_size;
    assert_eq!(bytes[tgst..tgst + 4], on_disk(b"tgst"));
    let inner = tgbl_end - 20; // the empty camera settings tgbl: 12 + count + flags
    assert_eq!(bytes[inner..inner + 4], on_disk(b"tgbl"));
    let leaves = &bytes[tgst + 12..inner];
    let head = &bytes[tgbl + 12..tgst]; // count, flags, raw

    let mut level = bytes[inner..tgbl_end].to_vec();
    for _ in 0..levels {
        let tgst_size = (leaves.len() + level.len()) as u32;
        let mut next = Vec::with_capacity(level.len() + 64 + element_size);
        next.extend_from_slice(&on_disk(b"tgbl"));
        next.extend_from_slice(&0u32.to_le_bytes());
        next.extend_from_slice(&((head.len() + 12) as u32 + tgst_size).to_le_bytes());
        next.extend_from_slice(head);
        next.extend_from_slice(&on_disk(b"tgst"));
        next.extend_from_slice(&tgst_size.to_le_bytes());
        next.extend_from_slice(&tgst_size.to_le_bytes());
        next.extend_from_slice(leaves);
        next.extend_from_slice(&level);
        level = next;
    }

    let mut out = bytes[..bdat].to_vec();
    out.extend_from_slice(&on_disk(b"bdat"));
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(level.len() as u32).to_le_bytes());
    out.extend_from_slice(&level);
    out.extend_from_slice(&bytes[chunk_end(&bytes, bdat)..]);
    out
}

/// A few levels of a self-nesting block read: the construction is sound.
#[test]
fn a_block_nested_in_its_own_struct_reads_a_few_levels() {
    let (result, _) = read(&nested_blocks(3));
    let tag = result.expect("three levels");
    let depth = |tag: &TagFile| {
        let mut depth = 0;
        let mut current = tag.root().field("camera settings").and_then(|f| f.as_block()).and_then(|b| b.element(0));
        while let Some(element) = current {
            depth += 1;
            current = element.field("camera settings").and_then(|f| f.as_block()).and_then(|b| b.element(0));
        }
        depth
    };
    assert_eq!(depth(&tag), 2, "the root plus two nested levels below it");
}

/// Twenty thousand levels: each costs a few dozen bytes, and following them
/// all recursed until the stack overflowed.
#[test]
fn blocks_nested_past_any_real_tag_are_refused() {
    let (result, _) = read(&nested_blocks(20_000));
    assert!(matches!(result, Err(TagReadError::NestingTooDeep { .. })), "{:?}", result.err());
}
