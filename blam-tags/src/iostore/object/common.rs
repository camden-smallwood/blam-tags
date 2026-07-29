//! Small shared readers: counts, bulk arrays and container removal
//! prefixes. Used by property values, native structs and class tails alike.

use anyhow::{bail, Result};

use super::archive::Reader;
use super::value::PropValue;
use super::limits::{bounded, MAX_CONTAINER_ELEMENTS, MAX_ELEMENT_SIZE, MAX_NATIVE_COUNT};

/// A `TArray` written with `BulkSerialize`: the element size, the count, then
/// `count × size` bytes of blittable elements. Returns the element count.
pub(super) fn read_bulk_array(r: &mut Reader, what: &str) -> Result<usize> {
    let elem = r.i32()?;
    let elem = bounded(elem, MAX_ELEMENT_SIZE, &format!("{what} element size"), r.o - 4)? as i32;
    // Bound the count by the bytes actually left in the export rather than by a
    // flat ceiling. `FRawStaticIndexBuffer` stores its indices as *single-byte*
    // elements, so a 1024x1024 plane's index buffer is a legitimate count of
    // 25,165,824 — which a fixed cap rejects. Sizing against the remainder is
    // both correct for that case and tighter for every smaller one.
    let at = r.o;
    let n = r.i32()?;
    let remaining = r.b.len().saturating_sub(r.o);
    let bytes = usize::try_from(n).ok().and_then(|n| n.checked_mul(elem as usize));
    match bytes {
        Some(b) if b <= remaining => {
            r.take(b)?;
            Ok(n as usize)
        }
        _ => bail!("implausible {what} count {n} (elem {elem}, {remaining} left) @ {at}"),
    }
}

/// A natively-serialized array count, with a plausibility guard so a desync
/// fails loudly instead of allocating wildly.
/// An `FByteBulkData` whose payload the cook forced inline.
///
/// In a Zen package the bulk-data *header* is just an `int32` index into the
/// package's bulk-data map; the payload, when inlined, follows immediately.
/// Checking the mapped offset against the cursor is what distinguishes the two
/// — a payload that lives in the sibling `.ubulk` must be left alone.
pub(super) fn read_inline_bulk_data(r: &mut Reader, bulk_data: &[(i64, i64)], what: &str) -> Result<()> {
    let index = r.i32()?;
    let Some(&(offset, size)) = bulk_data.get(index.max(0) as usize) else {
        bail!("{what}: bulk data index {index} out of range");
    };
    if offset as usize == r.o {
        r.take(size.max(0) as usize)?;
    }
    Ok(())
}

/// The delta-serialization prefix shared by `TSet` and `TMap`: a count of
/// entries to remove, followed by that many keys/elements. `INDEX_NONE` means
/// the container is replaced wholesale and nothing follows.
/// Returns the removed entries, or `None` for `INDEX_NONE`. They used to be
/// read and dropped, which is invisible while the count is zero — as it is for
/// all but 5 of the 1,153,836 exports — and makes those 5 unwritable.
pub(super) fn read_container_removals(
    r: &mut Reader,
    what: &str,
    mut read_one: impl FnMut(&mut Reader) -> Result<PropValue>,
) -> Result<Option<Vec<PropValue>>> {
    let n = r.i32()?;
    if n == -1 {
        return Ok(None);
    }
    let n = bounded(n, MAX_CONTAINER_ELEMENTS, &format!("{what} removal"), r.o - 4)? as i32;
    let mut out = Vec::new();
    for _ in 0..n {
        out.push(read_one(r)?);
    }
    Ok(Some(out))
}

/// Wrap a container in [`PropValue::WithRemovals`] only when its removal prefix
/// actually carried something, so the overwhelmingly common shape is untouched.
pub(super) fn with_removals(removals: Option<Vec<PropValue>>, inner: PropValue) -> PropValue {
    match removals {
        Some(r) if r.is_empty() => inner,
        removals => PropValue::WithRemovals { removals, inner: Box::new(inner) },
    }
}

pub(super) fn native_count(r: &mut Reader, what: &str) -> Result<usize> {
    let n = r.i32()?;
    bounded(n, MAX_NATIVE_COUNT, what, r.o - 4)
}
