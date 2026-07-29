//! The cursor an export's bytes pass through, in either direction, and the
//! context it resolves references against.
//!
//! [`Ar`] is the seam: a layout is described once, against a trait that either
//! reads into a value or writes out of it, which is how UE's own
//! `FArchive& operator<<` works. The alternative — a `read_x` and a `write_x`
//! per layout — is two descriptions of one fact, and they drift.
//!
//! Values are passed as `&mut`: on load the callee fills them in, on save it
//! reads them out. A caller that wants to *write* therefore prepares the value
//! first and hands it over, rather than the archive returning anything.

use anyhow::{bail, Context, Result};
use std::sync::OnceLock;

use super::usmap::UsmapProperty;
use super::value::FName;

/// A bidirectional byte archive.
///
/// Implemented by [`Reader`] (loading) and [`Writer`] (saving). Every method
/// takes the value by `&mut` so one body serves both directions; branch on
/// [`Ar::is_loading`] only where the *shape* genuinely differs, such as sizing a
/// container from a count that is read on load and derived on save.
pub(super) trait Ar {
    fn u8(&mut self, v: &mut u8) -> Result<()>;
    fn u16(&mut self, v: &mut u16) -> Result<()>;
    fn i32(&mut self, v: &mut i32) -> Result<()>;
    fn u32(&mut self, v: &mut u32) -> Result<()>;
    fn u64(&mut self, v: &mut u64) -> Result<()>;
    fn f32(&mut self, v: &mut f32) -> Result<()>;
    fn f64(&mut self, v: &mut f64) -> Result<()>;
    /// An `FName` as the file stores it: name-map index then instance number.
    ///
    /// Note the writer needs no name map. The index travels with the value
    /// (see [`FName`]), which is the whole reason that type keeps the pair
    /// rather than the display string.
    fn fname(&mut self, v: &mut FName) -> Result<()>;
    /// An `FString`: a length then the characters, negative meaning UTF-16.
    fn fstring(&mut self, v: &mut String) -> Result<()>;
    /// Exactly `n` bytes, uninterpreted.
    fn raw(&mut self, v: &mut Vec<u8>, n: usize) -> Result<()>;
}

// An `is_loading` / `pos` pair belongs on this trait the moment a *single* body
// serves both directions and has to size a container from a count that is read
// on load and derived on save. Nothing does yet — `write_value` is still a
// mirror of `read_value` rather than one shared description — so they are left
// out rather than added speculatively.

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

/// The saving half of [`Ar`]: appends to a byte buffer.
///
/// Deliberately has no name map. An `FName` carries its own index, so a block
/// can be re-emitted without interning anything; a writer that *introduces* new
/// names is a package-level concern (growing `FNameMap`), not an export-level
/// one.
pub(super) struct Writer {
    pub(super) b: Vec<u8>,
}

impl Writer {
    pub(super) fn new() -> Self {
        Writer { b: Vec::new() }
    }
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.b
    }
}

impl Ar for Writer {
    fn u8(&mut self, v: &mut u8) -> Result<()> {
        self.b.push(*v);
        Ok(())
    }
    fn u16(&mut self, v: &mut u16) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn i32(&mut self, v: &mut i32) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn u32(&mut self, v: &mut u32) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn u64(&mut self, v: &mut u64) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn f32(&mut self, v: &mut f32) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn f64(&mut self, v: &mut f64) -> Result<()> {
        self.b.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn fname(&mut self, v: &mut FName) -> Result<()> {
        self.b.extend_from_slice(&v.index.to_le_bytes());
        self.b.extend_from_slice(&v.number.to_le_bytes());
        Ok(())
    }
    fn fstring(&mut self, v: &mut String) -> Result<()> {
        // Mirrors `FString::operator<<`: empty is a bare zero length; ASCII goes
        // out as bytes with a NUL; anything else as UTF-16 with a negated
        // character count. `Reader::fstring` stops at the first NUL, so a string
        // that survives a round trip must not contain one.
        if v.is_empty() {
            self.b.extend_from_slice(&0i32.to_le_bytes());
        } else if v.is_ascii() {
            self.b.extend_from_slice(&(v.len() as i32 + 1).to_le_bytes());
            self.b.extend_from_slice(v.as_bytes());
            self.b.push(0);
        } else {
            let chars: Vec<u16> = v.encode_utf16().collect();
            self.b.extend_from_slice(&(-(chars.len() as i32 + 1)).to_le_bytes());
            for c in chars {
                self.b.extend_from_slice(&c.to_le_bytes());
            }
            self.b.extend_from_slice(&0u16.to_le_bytes());
        }
        Ok(())
    }
    fn raw(&mut self, v: &mut Vec<u8>, n: usize) -> Result<()> {
        if v.len() != n {
            bail!("writer given {} bytes for a {n}-byte field", v.len());
        }
        self.b.extend_from_slice(v);
        Ok(())
    }
}

impl Ar for Reader<'_> {
    fn u8(&mut self, v: &mut u8) -> Result<()> {
        *v = Reader::u8(self)?;
        Ok(())
    }
    fn u16(&mut self, v: &mut u16) -> Result<()> {
        *v = Reader::u16(self)?;
        Ok(())
    }
    fn i32(&mut self, v: &mut i32) -> Result<()> {
        *v = Reader::i32(self)?;
        Ok(())
    }
    fn u32(&mut self, v: &mut u32) -> Result<()> {
        *v = Reader::u32(self)?;
        Ok(())
    }
    fn u64(&mut self, v: &mut u64) -> Result<()> {
        *v = Reader::u64(self)?;
        Ok(())
    }
    fn f32(&mut self, v: &mut f32) -> Result<()> {
        *v = Reader::f32(self)?;
        Ok(())
    }
    fn f64(&mut self, v: &mut f64) -> Result<()> {
        *v = Reader::f64(self)?;
        Ok(())
    }
    fn fname(&mut self, v: &mut FName) -> Result<()> {
        *v = Reader::fname(self)?;
        Ok(())
    }
    fn fstring(&mut self, v: &mut String) -> Result<()> {
        *v = Reader::fstring(self)?;
        Ok(())
    }
    fn raw(&mut self, v: &mut Vec<u8>, n: usize) -> Result<()> {
        *v = Reader::take(self, n)?.to_vec();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every primitive must survive write→read unchanged. This is the floor the
    /// whole write path stands on: if a scalar or a name cannot round-trip,
    /// nothing built above it can either.
    #[test]
    fn primitives_round_trip() {
        let mut w = Writer::new();
        w.u8(&mut 0xA5).unwrap();
        w.u16(&mut 0xBEEF).unwrap();
        w.i32(&mut -123_456).unwrap();
        w.u32(&mut 0xDEAD_BEEF).unwrap();
        w.u64(&mut 0x0123_4567_89AB_CDEF).unwrap();
        w.f32(&mut 0.5).unwrap();
        w.f64(&mut -2.25).unwrap();
        let bytes = w.into_bytes();
        assert_eq!(bytes.len(), 1 + 2 + 4 + 4 + 8 + 4 + 8);

        let mut r = Reader::new(&bytes, &[]);
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g) = (0u8, 0u16, 0i32, 0u32, 0u64, 0f32, 0f64);
        Ar::u8(&mut r, &mut a).unwrap();
        Ar::u16(&mut r, &mut b).unwrap();
        Ar::i32(&mut r, &mut c).unwrap();
        Ar::u32(&mut r, &mut d).unwrap();
        Ar::u64(&mut r, &mut e).unwrap();
        Ar::f32(&mut r, &mut f).unwrap();
        Ar::f64(&mut r, &mut g).unwrap();
        assert_eq!(
            (a, b, c, d, e, f, g),
            (0xA5, 0xBEEF, -123_456, 0xDEAD_BEEF, 0x0123_4567_89AB_CDEF, 0.5, -2.25)
        );
        assert_eq!(r.o, bytes.len(), "reader did not consume exactly what was written");
    }

    /// The writer needs no name map: an `FName` carries its own index, so the
    /// bytes come straight back even though the reader resolves them against a
    /// name table the writer never saw.
    #[test]
    fn fname_round_trips_without_a_name_map() {
        let mut w = Writer::new();
        w.fname(&mut FName::new(7, 5, "Rocket_4")).unwrap();
        let bytes = w.into_bytes();
        assert_eq!(bytes, [7, 0, 0, 0, 5, 0, 0, 0]);

        // Resolved against a table where index 7 is "Rocket": number 5 renders
        // as `_4`, and the identity is preserved regardless.
        let names: Vec<String> = (0..8).map(|i| format!("n{i}")).collect();
        let mut names = names;
        names[7] = "Rocket".to_string();
        let mut r = Reader::new(&bytes, &names);
        let mut back = FName::default();
        Ar::fname(&mut r, &mut back).unwrap();
        assert_eq!((back.index, back.number), (7, 5));
        assert_eq!(back.as_str(), "Rocket_4");
    }

    /// ASCII, empty and non-ASCII strings each take a different encoding path.
    #[test]
    fn fstring_round_trips() {
        for s in ["", "SK_Marine_Torso_01", "naïve", "日本語"] {
            let mut w = Writer::new();
            w.fstring(&mut s.to_string()).unwrap();
            let bytes = w.into_bytes();
            let mut r = Reader::new(&bytes, &[]);
            let mut back = String::new();
            Ar::fstring(&mut r, &mut back).unwrap();
            assert_eq!(back, s, "string did not survive a round trip");
            assert_eq!(r.o, bytes.len(), "wrong length consumed for {s:?}");
        }
    }

    /// A writer handed the wrong number of bytes for a fixed-size field is a
    /// caller bug, and must not silently emit a differently-sized record.
    #[test]
    fn raw_length_mismatch_is_an_error() {
        let mut w = Writer::new();
        assert!(w.raw(&mut vec![1, 2, 3], 4).is_err());
        assert!(w.raw(&mut vec![1, 2, 3, 4], 4).is_ok());
    }
}
