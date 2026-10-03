//! Text and audio readers given a count or size far larger than their input.
//! Each must fail without first reserving room for what the input claims.
//!
//! A counting global allocator records the largest single allocation each
//! parse asks for. That is what this checks, not whether the allocation
//! happens to fail: on a system that overcommits (macOS), reserving 100 GB
//! succeeds lazily and the parse goes on to fail cleanly, while on one that
//! does not (Linux, by default) the same request aborts the process.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

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

/// Run `f`, returning its result and the largest single allocation it made.
fn largest_allocation<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|largest| largest.set(0));
    let result = f();
    (result, LARGEST.with(Cell::get))
}

const LIMIT: usize = 1 << 20;

#[test]
fn an_ass_count_larger_than_the_file_reserves_nothing_for_it() {
    use blam_tags::ass_parse::{AssParseError, parse};

    let header = "7\n\"tool\" \"1.0\" \"user\" \"machine\"\n";
    let mesh = "1\n\"MESH\" \"\" \"\"\n";
    for (src, missing) in [
        (format!("{header}2000000000\n"), "a material name"),
        (format!("{header}0\n2000000000\n"), "an object class"),
        (format!("{header}0\n{mesh}2000000000\n"), "a vertex position"),
        (format!("{header}0\n{mesh}1\n0 0 0 0 0 1 0 0 0 2000000000\n"), "a node index"),
        (format!("{header}0\n{mesh}1\n0 0 0 0 0 1 0 0 0 0 2000000000\n"), "a uv u"),
        (format!("{header}0\n{mesh}0\n2000000000\n"), "a triangle material"),
        (format!("{header}0\n0\n2000000000\n"), "an instance object index"),
    ] {
        let (result, largest) = largest_allocation(|| parse(&src));
        assert!(
            matches!(result, Err(AssParseError::Truncated { what }) if what == missing),
            "{src:?}: {:?}",
            result.err()
        );
        assert!(largest < LIMIT, "{src:?} asked for {largest} bytes");
    }
}

/// The JMS reader had this guard already; it is the control here.
#[test]
fn a_jms_count_larger_than_the_file_reserves_nothing_for_it() {
    let src = ";### VERSION ###\n8213\n;### NODES ###\n2000000000\n";
    let (result, largest) = largest_allocation(|| blam_tags::jms::JmsFile::parse(src));
    assert!(result.is_err());
    assert!(largest < LIMIT, "asked for {largest} bytes");
}

#[cfg(feature = "audio")]
mod audio {
    use super::{LIMIT, largest_allocation};
    use std::path::PathBuf;

    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("blam-tags-absurd-counts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// An FSB5 header: magic, version, subsound count, sample-header and
    /// name region sizes, data size, format, and zeros to 60 bytes.
    fn fsb5(subsounds: u32, headers: u32, names: u32, body: &[u8]) -> Vec<u8> {
        let mut out = b"FSB5".to_vec();
        for word in [1, subsounds, headers, names, 0, 15] {
            out.extend_from_slice(&u32::to_le_bytes(word));
        }
        out.resize(60, 0);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn an_fsb5_bank_claiming_more_than_it_holds_reserves_nothing_for_it() {
        use blam_tags::audio::fsb5::Fsb5;
        for (name, bytes) in [
            ("headers.fsb", fsb5(1, u32::MAX, 0, &[0; 8])),
            ("names.fsb", fsb5(0, 0, u32::MAX, &[])),
            ("subsounds.fsb", fsb5(u32::MAX, 8, 0, &[0; 8])),
        ] {
            let path = scratch(name, &bytes);
            let (result, largest) = largest_allocation(|| Fsb5::open(&path));
            let _ = std::fs::remove_file(&path);
            assert!(result.is_err(), "{name} opened");
            assert!(largest < LIMIT, "{name} asked for {largest} bytes");
        }
        // One subsound with an 8-byte sample header and no data opens.
        let path = scratch("one.fsb", &fsb5(1, 8, 0, &[0; 8]));
        let result = Fsb5::open(&path);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result.expect("one subsound").subsounds.len(), 1);
    }

    /// An AKPK header: magic, header length, version, then the language,
    /// bank and stream section sizes.
    fn akpk(header_len: u32, sizes: [u32; 3], body: &[u8]) -> Vec<u8> {
        let mut out = b"AKPK".to_vec();
        for word in [header_len, 1, sizes[0], sizes[1], sizes[2], 0] {
            out.extend_from_slice(&u32::to_le_bytes(word));
        }
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn a_wwise_package_claiming_more_than_it_holds_reserves_nothing_for_it() {
        use blam_tags::audio::wwise::Pck;
        // Sections larger than the file.
        let path = scratch("sections.pck", &akpk(u32::MAX, [0, u32::MAX / 2, 0], &[]));
        let (result, largest) = largest_allocation(|| Pck::open(&path));
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
        assert!(largest < LIMIT, "sections asked for {largest} bytes");

        // A bank table counting four billion entries in four bytes.
        let body = [0u32.to_le_bytes(), u32::MAX.to_le_bytes()].concat();
        let path = scratch("count.pck", &akpk(24, [4, 4, 0], &body));
        let (result, largest) = largest_allocation(|| Pck::open(&path));
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
        assert!(largest < LIMIT, "file table asked for {largest} bytes");

        // An entry read past the end of the file.
        let path = scratch("range.pck", &akpk(8, [4, 4, 0], &[0; 8]));
        let pck = Pck::open(&path).expect("an empty package");
        let (result, largest) = largest_allocation(|| pck.read_range(0, u32::MAX as u64 * 16));
        let fits = pck.read_range(0, 8);
        let _ = std::fs::remove_file(&path);
        assert!(result.is_err());
        assert!(largest < LIMIT, "read_range asked for {largest} bytes");
        assert_eq!(fits.expect("in range").len(), 8);
    }
}
