use anyhow::{Context, Result, ensure};
use std::ops::Range;

pub(crate) fn range(offset: usize, size: usize, length: usize) -> Result<Range<usize>> {
    let end = offset.checked_add(size).context("byte range overflow")?;
    ensure!(
        end <= length,
        "byte range {offset:#x}..{end:#x} exceeds {length:#x} bytes"
    );
    Ok(offset..end)
}
pub(crate) fn bytes<const N: usize>(data: &[u8], offset: usize) -> Result<[u8; N]> {
    Ok(data[range(offset, N, data.len())?].try_into().unwrap())
}
pub(crate) fn u32_at(data: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, offset)?))
}
pub(crate) fn i32_at(data: &[u8], offset: usize) -> Result<i32> {
    Ok(i32::from_le_bytes(bytes(data, offset)?))
}
pub(crate) fn count(data: &[u8], offset: usize) -> Result<usize> {
    usize::try_from(i32_at(data, offset)?).context("negative table count")
}
pub(crate) fn u64_at(data: &[u8], offset: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, offset)?))
}
pub(crate) fn table_end(start: usize, count: usize, stride: usize, length: usize) -> Result<usize> {
    let size = count.checked_mul(stride).context("table size overflow")?;
    Ok(range(start, size, length)?.end)
}
