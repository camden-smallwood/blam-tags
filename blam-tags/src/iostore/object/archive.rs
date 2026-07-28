//! The cursor an export's bytes pass through, and the context it resolves
//! references against.

use anyhow::{Context, Result};
use std::sync::OnceLock;

use super::usmap::UsmapProperty;
use super::value::FName;

/// Little-endian byte-cursor over an export's serial data.
pub(super) struct Reader<'a> {
    pub(super) b: &'a [u8],
    pub(super) o: usize,
    pub(super) names: &'a [String],
    /// The `FField` chain this export defined, if it is `UStruct`-derived.
    ///
    /// `UUserDefinedStruct` writes a default instance of *itself* after its
    /// `UStruct` body, so the schema its property block indexes by is not in
    /// the `.usmap` at all — it is the chain that was just parsed a few bytes
    /// earlier. Stashing it here is what lets a later class in the same chain
    /// walk that block.
    pub(super) struct_fields: Option<Vec<UsmapProperty>>,
    /// Resolves references out of this package (see [`ExportContext`]).
    pub(super) resolver: Option<&'a dyn PackageResolver>,
}

impl<'a> Reader<'a> {
    pub(super) fn new(b: &'a [u8], names: &'a [String]) -> Self {
        Reader { b, o: 0, names, struct_fields: None, resolver: None }
    }
    pub(super) fn with_ctx(b: &'a [u8], names: &'a [String], ctx: &ExportContext<'a>) -> Self {
        Reader { resolver: ctx.resolver, ..Reader::new(b, names) }
    }
    /// The bytes consumed since `start`. Used to keep an unmodeled value's
    /// exact bytes rather than dropping them (see `PropValue::Raw`).
    pub(super) fn since(&self, start: usize) -> Vec<u8> {
        self.b[start.min(self.o)..self.o].to_vec()
    }
    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let s = self
            .b
            .get(self.o..self.o.checked_add(n).unwrap_or(usize::MAX))
            .with_context(|| format!("unversioned read past end (+{n} @ {})", self.o))?;
        self.o += n;
        Ok(s)
    }
    pub(super) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub(super) fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub(super) fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub(super) fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    /// An `FName` with its identity intact: the `i32` name-map index and `i32`
    /// instance number, plus the text they resolve to. Use this wherever the
    /// value is kept; [`Reader::name`] is for callers that only compare or
    /// print it.
    pub(super) fn fname(&mut self) -> Result<FName> {
        let at = self.o;
        let idx = self.i32()?;
        let number = self.i32()?;
        let base = usize::try_from(idx)
            .ok()
            .and_then(|i| self.names.get(i))
            .with_context(|| format!("FName index {idx} out of range (@ {at})"))?;
        let text = if number > 0 { format!("{base}_{}", number - 1) } else { base.clone() };
        Ok(FName::new(idx as u32, number as u32, text))
    }

    /// An `FName` flattened to its display string. A non-zero number appends
    /// `_{number-1}`, per UE convention — which is lossy, hence [`Reader::fname`]
    /// for anything that is stored rather than merely inspected.
    pub(super) fn name(&mut self) -> Result<String> {
        let idx = self.i32()?;
        let number = self.i32()?;
        let base = usize::try_from(idx)
            .ok()
            .and_then(|i| self.names.get(i))
            .with_context(|| format!("FName index {idx} out of range (@ {})", self.o - 8))?;
        Ok(if number > 0 {
            format!("{base}_{}", number - 1)
        } else {
            base.clone()
        })
    }
    /// `FString`: `i32 len`; positive = UTF-8 (NUL-terminated), negative =
    /// UTF-16 (len is negated char count).
    pub(super) fn fstring(&mut self) -> Result<String> {
        let n = self.i32()?;
        if n == 0 {
            return Ok(String::new());
        }
        if n > 0 {
            let bytes = self.take(n as usize)?;
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
        } else {
            let chars = (-n) as usize;
            let bytes = self.take(chars * 2)?;
            let u16s: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .take_while(|&c| c != 0)
                .collect();
            Ok(String::from_utf16_lossy(&u16s))
        }
    }
}

/// Whether to narrate *why* a class's native tail stopped (`BLAM_TAIL_WHY`).
///
/// Read once. It used to be probed from the environment on per-element paths,
/// which is a syscall inside a decode loop.
pub(super) fn tail_why() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BLAM_TAIL_WHY").is_ok())
}

/// Whether to narrate the walk to stderr (`BLAM_UNVERSIONED_TRACE=1`).
///
/// A desync in this reader is silent by construction — misread bytes still
/// decode as plausible values, and the failure only surfaces much later as an
/// implausible array count. The only reliable way to diagnose one is to watch
/// each property's byte range against the raw export, so that view is built in
/// rather than reconstructed by hand each time.
pub(super) fn trace_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("BLAM_UNVERSIONED_TRACE").is_ok_and(|v| !v.is_empty() && v != "0")
    })
}

/// Resolves the struct references a cooked export makes to things outside
/// itself.
///
/// Two kinds of reference need this. `UDataTable::RowStruct` is an
/// `FPackageIndex` naming the struct its rows are laid out by, and every
/// `FStructProperty` in a recovered `FField` chain stores only an
/// `FPackageIndex` for its type. Either may point at a native struct (a script
/// import, so the `.usmap` has it) or at a `UUserDefinedStruct` cooked into
/// another package entirely — and only the caller holds the import map and the
/// other containers.
///
/// Splitting it in two is what lets the two halves compose: [`struct_name`]
/// depends on the package currently being read, [`struct_layout`] does not, so
/// a struct that nests another user-defined struct resolves by recursion
/// instead of needing a schema table threaded through the reader.
///
/// [`struct_name`]: PackageResolver::struct_name
/// [`struct_layout`]: PackageResolver::struct_layout
pub trait PackageResolver {
    /// The name of the struct an `FPackageIndex` from *this* package points
    /// at. Native structs should come back under their `.usmap` name; anything
    /// else under a name `struct_layout` can find it by.
    fn struct_name(&self, package_index: i32) -> Option<String>;

    /// The property layout of a struct that is not in the `.usmap`, by the
    /// name [`struct_name`](PackageResolver::struct_name) returned. Called only
    /// after a `.usmap` lookup misses.
    fn struct_layout(&self, _name: &str) -> Option<Vec<UsmapProperty>> {
        None
    }
}

/// Package-level context an export's native tail needs but its own bytes
/// cannot supply.
#[derive(Default, Clone, Copy)]
pub struct ExportContext<'a> {
    /// The package's bulk-data map as `(serial_offset, serial_size)`, so tails
    /// with an inline bulk payload can find it.
    pub bulk_data: &'a [(i64, i64)],
    /// Resolves references out of this package. Without one, `UDataTable` rows
    /// and struct-typed user-defined fields are reported as unmodeled tails
    /// rather than guessed at.
    pub resolver: Option<&'a dyn PackageResolver>,
}

impl<'a> ExportContext<'a> {
    /// Context carrying only a bulk-data map — enough for every class whose
    /// tail is self-describing.
    pub fn new(bulk_data: &'a [(i64, i64)]) -> Self {
        ExportContext { bulk_data, resolver: None }
    }
}
