//! Corruption tests: feed deliberately malformed tag bytes to
//! [`TagFile::read_from_bytes`] and assert that each failure surfaces
//! as a specific [`TagReadError`] variant rather than a panic.
//!
//! Strategy: build a minimal valid tag in memory from the
//! `stereo_system` schema (the smallest definition in the tree —
//! single block, single struct, single u32 field), serialise it, then
//! mutate specific bytes per test.

use blam_tags::{TagFile, TagReadError};

const SCHEMA: &str = "../definitions/halo3_mcc/stereo_system.json";

/// Build a clean valid tag and return its serialised bytes.
fn baseline_bytes() -> Vec<u8> {
    let tag = TagFile::new(SCHEMA).expect("build stereo_system tag from schema");
    tag.write_to_bytes().expect("serialise tag")
}

/// Source-literal → on-disk byte order. The lib stores 4-byte
/// signatures as BE-packed u32s (`from_be_bytes`) and writes them
/// little-endian (`to_le_bytes`), which reverses the bytes on disk.
fn on_disk(sig: &[u8; 4]) -> [u8; 4] {
    [sig[3], sig[2], sig[1], sig[0]]
}

/// Locate the first occurrence of a source-literal signature on disk.
fn find_sig(bytes: &[u8], sig: &[u8; 4]) -> usize {
    let needle = on_disk(sig);
    bytes.windows(4).position(|w| w == needle).unwrap_or_else(|| {
        panic!(
            "signature {:?} (on-disk {:?}) not found",
            std::str::from_utf8(sig).unwrap_or("?"),
            std::str::from_utf8(&needle).unwrap_or("?"),
        )
    })
}

#[test]
fn baseline_roundtrips_clean() {
    let bytes = baseline_bytes();
    let _tag = TagFile::read_from_bytes(&bytes).expect("baseline must parse cleanly");
}

#[test]
fn truncated_tag_surfaces_io_error() {
    let bytes = baseline_bytes();
    // Lop off the back half — read_exact will hit EOF inside the
    // payload.
    let truncated = &bytes[..bytes.len() / 2];
    let err = TagFile::read_from_bytes(truncated).expect_err("truncated tag must error");
    // A chunk whose declared size runs past the cut is caught before its
    // bytes are read, as SizeExceedsInput.
    assert!(
        matches!(
            err,
            TagReadError::Io(_) | TagReadError::ChunkSizeMismatch { .. } | TagReadError::SizeExceedsInput { .. }
        ),
        "expected Io, ChunkSizeMismatch or SizeExceedsInput, got {err:?}",
    );
}

#[test]
fn bad_blam_signature() {
    let mut bytes = baseline_bytes();
    let pos = find_sig(&bytes, b"BLAM");
    bytes[pos..pos + 4].copy_from_slice(&on_disk(b"XXXX"));
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad BLAM must error");
    let TagReadError::BadChunkSignature { expected, got, .. } = err else {
        panic!("expected BadChunkSignature, got {err:?}");
    };
    assert_eq!(expected, *b"BLAM");
    assert_eq!(got, *b"XXXX");
}

#[test]
fn bad_outer_tag_signature() {
    let mut bytes = baseline_bytes();
    let pos = find_sig(&bytes, b"tag!");
    bytes[pos..pos + 4].copy_from_slice(&on_disk(b"yyyy"));
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad tag! must error");
    let TagReadError::BadChunkSignature { expected, got, .. } = err else {
        panic!("expected BadChunkSignature, got {err:?}");
    };
    assert_eq!(expected, *b"tag!");
    assert_eq!(got, *b"yyyy");
}

#[test]
fn bad_blay_signature() {
    let mut bytes = baseline_bytes();
    let pos = find_sig(&bytes, b"blay");
    bytes[pos..pos + 4].copy_from_slice(&on_disk(b"zzzz"));
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad blay must error");
    let TagReadError::BadChunkSignature { expected, got, .. } = err else {
        panic!("expected BadChunkSignature, got {err:?}");
    };
    assert_eq!(expected, *b"blay");
    assert_eq!(got, *b"zzzz");
}

#[test]
fn bad_blay_chunk_version() {
    let mut bytes = baseline_bytes();
    // blay chunk-header version sits 4 bytes after the signature
    // (sig + version + size little-endian u32s).
    let pos = find_sig(&bytes, b"blay");
    bytes[pos + 4..pos + 8].copy_from_slice(&99u32.to_le_bytes());
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad blay version must error");
    let TagReadError::BadChunkVersion { chunk, version } = err else {
        panic!("expected BadChunkVersion, got {err:?}");
    };
    assert_eq!(chunk, "blay");
    assert_eq!(version, 99);
}

#[test]
fn unsupported_layout_payload_version() {
    let mut bytes = baseline_bytes();
    // The blay payload starts 12 bytes after the chunk header. The
    // 24-byte payload header is `root_data_size (4) + guid (16) +
    // version (4)`, so the payload `version` field sits 12 + 4 + 16
    // = 32 bytes after the start of the blay chunk-header signature.
    let pos = find_sig(&bytes, b"blay");
    let version_offset = pos + 12 + 4 + 16;
    bytes[version_offset..version_offset + 4].copy_from_slice(&7u32.to_le_bytes());
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad payload version must error");
    let TagReadError::UnsupportedLayoutVersion(v) = err else {
        panic!("expected UnsupportedLayoutVersion, got {err:?}");
    };
    assert_eq!(v, 7);
}

#[test]
fn count_mismatch_on_str_chunk() {
    let mut bytes = baseline_bytes();
    // Find str* chunk and bump its declared size so it disagrees
    // with the count in the layout header. str* size lives 8 bytes
    // after the signature.
    let pos = find_sig(&bytes, b"str*");
    let size_offset = pos + 8;
    let original_size = u32::from_le_bytes(bytes[size_offset..size_offset + 4].try_into().unwrap());
    let bogus = original_size.wrapping_add(123);
    bytes[size_offset..size_offset + 4].copy_from_slice(&bogus.to_le_bytes());
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad str* size must error");
    // CountMismatch is what the str* size check produces (it's
    // compared against header.string_data_size, treated as a count).
    assert!(
        matches!(
            err,
            TagReadError::CountMismatch { chunk: "str*", .. }
                | TagReadError::Io(_)
                | TagReadError::ChunkSizeMismatch { .. }
        ),
        "expected CountMismatch / Io / ChunkSizeMismatch, got {err:?}",
    );
}

#[test]
fn bad_tgly_chunk_signature() {
    let mut bytes = baseline_bytes();
    let pos = find_sig(&bytes, b"tgly");
    bytes[pos..pos + 4].copy_from_slice(&on_disk(b"qqqq"));
    let err = TagFile::read_from_bytes(&bytes).expect_err("bad tgly must error");
    let TagReadError::BadChunkSignature { expected, got, .. } = err else {
        panic!("expected BadChunkSignature, got {err:?}");
    };
    assert_eq!(expected, *b"tgly");
    assert_eq!(got, *b"qqqq");
}

#[test]
fn unknown_top_level_chunk_signature() {
    // Append a fake unknown chunk after the tag stream to trigger the
    // unhandled-signature path in TagFile::read.
    let mut bytes = baseline_bytes();
    // Chunk header: 4-byte sig + 4-byte version + 4-byte size = 12 bytes.
    bytes.extend_from_slice(&on_disk(b"junk"));
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let err = TagFile::read_from_bytes(&bytes).expect_err("unknown chunk must error");
    let TagReadError::UnknownSubChunkSignature { context, signature } = err else {
        panic!("expected UnknownSubChunkSignature, got {err:?}");
    };
    assert_eq!(context, "tag-file top-level");
    assert_eq!(signature, *b"junk");
}

#[test]
fn trailing_garbage_after_streams() {
    // Add bytes after the last stream to trip the chunk-size
    // mismatch path. Trailing bytes form the start of a 12-byte
    // chunk header; if we add fewer than 12 bytes, the
    // `read_u32_le` for the lookahead signature will hit EOF first.
    let mut bytes = baseline_bytes();
    bytes.extend_from_slice(&[0xCC; 4]);
    let err = TagFile::read_from_bytes(&bytes).expect_err("trailing garbage must error");
    // Could surface as either Io (EOF on signature lookahead) or
    // UnknownSubChunkSignature (if the 4 random bytes happen to
    // form a parseable u32 unmatched by want/info/assd).
    assert!(
        matches!(
            err,
            TagReadError::Io(_) | TagReadError::UnknownSubChunkSignature { .. }
        ),
        "expected Io or UnknownSubChunkSignature, got {err:?}",
    );
}
