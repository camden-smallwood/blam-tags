//! One place for the bounds this reader enforces, and why each is what it is.
//!
//! These are not arbitrary safety valves. A cooked stream is read as a sequence
//! of counts followed by that many things, so a count read at the wrong offset
//! is the *first* symptom of a desync — and the difference between a clear error
//! and a multi-gigabyte allocation. They were previously seven separate literals
//! across five files, which made "is this count plausible?" a question with five
//! different answers.
//!
//! This module also matters because `iostore` now reads third-party mod
//! containers, so every count in it is attacker-controlled.

use anyhow::{bail, Result};

/// Elements in a `TArray`/`TSet`/`TMap` inside a property block.
///
/// Property-block containers are gameplay data, not geometry; the largest in the
/// shipped corpus is far below this.
pub(super) const MAX_CONTAINER_ELEMENTS: i32 = 1_000_000;

/// Elements in a natively serialized tail's array.
///
/// Deliberately looser than [`MAX_CONTAINER_ELEMENTS`]: tails carry real
/// geometry, and a `FRawStaticIndexBuffer` stores single-byte elements, so a
/// 1024×1024 plane's index buffer is a legitimate count of 25,165,824. Where the
/// remaining bytes are known, prefer bounding by those instead — see
/// [`super::common::read_bulk_array`], which is both tighter and correct for
/// that case.
pub(super) const MAX_NATIVE_COUNT: i32 = 10_000_000;

/// Stride of one element in a bulk array, in bytes.
pub(super) const MAX_ELEMENT_SIZE: i32 = 4096;

/// Properties in a recovered `FField` chain.
pub(super) const MAX_FIELD_COUNT: i32 = 4096;

/// Nesting depth for reflected structs and recursive native readers.
///
/// UE's own property graph is far shallower; this only has to stop a cyclic or
/// desynced stream from recursing until the stack gives out.
pub(super) const MAX_DEPTH: usize = 32;

/// Cap on speculative `Vec::with_capacity`.
///
/// A count is validated before it is *used*, but reserving for it beforehand
/// would let a bogus one allocate gigabytes before the check runs. Reserving a
/// bounded amount and letting the vector grow costs nothing on real data, where
/// counts are small.
pub(super) const PREALLOC_CAP: usize = 4096;

/// Validate a count read from the stream and widen it to `usize`.
///
/// `what` names the thing being counted and `at` is where the count was read,
/// because a bad count is nearly always a symptom of a desync *earlier* in the
/// stream — the offset is what makes it diagnosable.
pub(super) fn bounded(n: i32, max: i32, what: &str, at: usize) -> Result<usize> {
    if !(0..=max).contains(&n) {
        bail!("implausible {what} count {n} (max {max}) @ {at}");
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::super::archive::Reader;
    use super::*;

    /// `take` used to compute `self.o + n`, which wraps on a hostile count and
    /// can turn a read-past-end into an in-range slice. Only reachable from a
    /// malformed container, which is exactly what this module now reads.
    #[test]
    fn a_huge_take_errors_instead_of_wrapping() {
        let bytes = [0u8; 8];
        let mut r = Reader::new(&bytes, &[]);
        r.o = 4;
        assert!(r.take(usize::MAX).is_err(), "an overflowing take must not succeed");
        assert!(r.take(usize::MAX - 3).is_err());
        assert!(r.take(4).is_ok(), "a legitimate read still works");
    }

    /// The bound names what was being counted and where, because a bad count is
    /// almost always the symptom of a desync earlier in the stream.
    #[test]
    fn bounded_reports_what_and_where() {
        let e = bounded(-1, MAX_CONTAINER_ELEMENTS, "array", 0x40).unwrap_err().to_string();
        assert!(e.contains("array") && e.contains("64"), "unhelpful message: {e}");
        assert_eq!(bounded(7, MAX_CONTAINER_ELEMENTS, "array", 0).unwrap(), 7);
        assert!(bounded(MAX_CONTAINER_ELEMENTS + 1, MAX_CONTAINER_ELEMENTS, "array", 0).is_err());
    }
}
