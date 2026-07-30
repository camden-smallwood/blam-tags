//! A whole export: the property block, `UObject`'s trailer, and each class in
//! the inheritance chain's natively serialized tail.

use anyhow::{bail, Context, Result};

use super::archive::{tail_why, ExportContext, Reader};
use super::block::read_struct;
use super::tails::{read_class_native_tail, read_rig_hierarchy, read_rigvm};
use super::usmap::Usmap;
use super::value::PropertyBlock;

/// Decode a cooked object export's unversioned property block for a known
/// native `class` (present in the `.usmap`), returning present property
/// name→value. General entry point for simple UObject exports (e.g.
/// `SkeletalMeshSocket`) whose serial data is just their reflected properties.
pub fn read_export_struct(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<PropertyBlock> {
    read_export_struct_len(export, names, usmap, class).map(|(props, _)| props)
}

/// As [`read_export_struct`], but also returning how many bytes the property
/// block consumed.
///
/// This is the corpus gate for this reader. "Decoded without error" is a weak
/// claim — a desynced walk keeps reading plausible values and only trips much
/// later, or not at all. For a class whose export is *nothing but* its property
/// block, `consumed == export.len()` (modulo zero padding) is a real check that
/// every byte was accounted for; for classes with a native tail (a mesh's render
/// data, a texture's platform data) it says where that tail begins.
pub fn read_export_struct_len(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
) -> Result<(PropertyBlock, usize)> {
    let mut r = Reader::new(export, names);
    let props = read_struct(&mut r, class, usmap, 0)?;
    Ok((props, r.o))
}

/// Where the native-tail walk stopped short of modeling the whole export.
///
/// The tail dispatcher has 49 sites that rewind and decline to continue —
/// `MaterialInterface`'s partly-modeled cached expressions, `Material`'s inline
/// shader maps, and so on. Each one is a deliberate boundary, but until now the
/// only trace was a line on stderr behind `BLAM_TAIL_WHY`, so a caller learned
/// that a tail existed and nothing about *why* modeling stopped there.
///
/// This is that boundary as data: the class that declined, where in the export
/// it happened, and how much was left. The reason itself is the comment at the
/// site — the class and offset are what locate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailStop {
    /// The class in the inheritance chain whose tail reader declined.
    pub class: String,
    /// Offset into the export where the walk stopped.
    pub at: usize,
    /// Bytes after that point, left as an unmodeled span.
    pub remaining: usize,
}

/// A chain walk's outcome: what it consumed, and where it gave up if it did.
#[derive(Debug, Clone)]
pub struct TailWalk {
    pub block: PropertyBlock,
    /// Bytes consumed by block + trailer + every tail that was modeled.
    pub consumed: usize,
    /// `None` when the whole chain was walked to the end.
    pub stopped: Option<TailStop>,
}

pub fn read_export_with_trailer(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<(PropertyBlock, usize)> {
    let walk = walk_export(export, names, usmap, class, object_flags, ctx)?;
    Ok((walk.block, walk.consumed))
}

/// As [`read_export_with_trailer`], but reporting where the tail walk stopped.
pub fn walk_export(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<TailWalk> {
    let mut r = Reader::with_ctx(export, names, ctx);
    // A handful of classes override `Serialize` and deliberately do **not**
    // call `Super::Serialize` on the load path, so the export carries no
    // property block, no `UObject` GUID trailer and no inherited tails — it
    // begins immediately with the class's own data. `URigVM::Serialize`
    // (RigVM.cpp:109) only calls up for reference collection and memory
    // counting; loading goes straight to `Load`.
    if class == "RigVM" || class == "RigHierarchy" {
        if class == "RigHierarchy" {
            let props = PropertyBlock::default();
            let at = r.o;
            if read_rig_hierarchy(&mut r).is_err() {
                r.o = at;
            }
            return Ok(TailWalk { block: props, consumed: r.o, stopped: None });
        }
        let props = PropertyBlock::default();
        read_rigvm(&mut r, usmap)?;
        return Ok(TailWalk { block: props, consumed: r.o, stopped: None });
    }
    let props = read_struct(&mut r, class, usmap, 0)?;
    if export.len() >= r.o + 4 {
        let at = r.o;
        match r.u32()? {
            0 => {}
            1 => {
                r.take(16)?;
            }
            // Not a boolean, so this export does not follow the trailer model
            // (its property walk stopped early, or the class serializes
            // something else here). Rewind and leave the rest as an unmodeled
            // tail rather than failing an otherwise good property decode.
            // Not a boolean, so this export does not follow the trailer model
            // — the walk stops here and everything from `at` is unmodeled.
            _ => {
                r.o = at;
                return Ok(TailWalk {
                    block: props,
                    consumed: r.o,
                    stopped: Some(TailStop {
                        class: "UObject".to_string(),
                        at: r.o,
                        remaining: export.len() - r.o,
                    }),
                });
            }
        }
    }
    // Walk to the root of the chain, then replay it base → derived.
    let mut chain = Vec::new();
    let mut cur = Some(class.to_string());
    while let Some(c) = cur {
        if chain.len() > 64 {
            break;
        }
        cur = usmap.get(&c).and_then(|s| s.super_name.clone());
        chain.push(c);
    }
    chain.reverse();
    let why = tail_why();
    let mut stopped = None;
    for c in &chain {
        let at = r.o;
        let keep_going = read_class_native_tail(&mut r, c, &props, usmap, ctx, object_flags)
            .with_context(|| format!("native tail of {c} (in {class})"))?;
        if why {
            eprintln!(
                "  chain: {c} {at}..{}{}",
                r.o,
                if keep_going { "" } else { "  <- STOPPED" }
            );
        }
        if !keep_going {
            stopped = Some(TailStop {
                class: c.clone(),
                at: r.o,
                remaining: export.len().saturating_sub(r.o),
            });
            break;
        }
    }
    Ok(TailWalk { block: props, consumed: r.o, stopped })
}

/// The `UObject` trailer written after the property block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Trailer {
    /// Not written at all: a class-default object, or the export ended before
    /// the four-byte flag. Also what a *malformed* flag decodes to — see
    /// [`read_export`] — in which case those bytes stay in the tail.
    #[default]
    Absent,
    /// `hasGuid == 0`.
    NoGuid,
    /// `hasGuid == 1`, followed by the GUID.
    Guid([u8; 16]),
}

/// One export, decomposed into the parts a writer needs.
///
/// The tail — everything each class in the inheritance chain natively
/// serializes after the reflected properties — is kept as bytes. That is 71% of
/// the corpus and 4.88 GiB, and modeling it is Phase 4. Keeping it verbatim
/// makes the export round trip exact *now*, and gives every future tail model
/// something to be checked against: convert one class, and the gate proves the
/// model is lossless against the span it replaces.
#[derive(Debug, Clone, Default)]
pub struct Export {
    pub block: ExportBlock,
    pub trailer: Trailer,
    pub tail: Vec<u8>,
}

/// An export's property region, and *why* it looks the way it does.
///
/// The three cases are genuinely different claims and were worth separating: an
/// absent block because the format says so is not the same as an absent block
/// because the schema is missing, and collapsing them into one `Option` is how
/// a gate ends up filtering the second away and reporting full coverage.
#[derive(Debug, Clone, Default)]
pub enum ExportBlock {
    /// Decoded against the `.usmap`. Every export in the shipped corpus but 18.
    Reflected(PropertyBlock),
    /// The class's `Serialize` never calls `Super`, so there is no property
    /// block to decode at all (`URigVM`, `URigHierarchy`; see
    /// [`NO_PROPERTY_BLOCK`]). A positive statement about the format.
    #[default]
    NotSerialized,
    /// The declaring module ships no reflection data anywhere, so the values
    /// cannot be typed. See [`UnreflectedBlock`].
    Unreflected(UnreflectedBlock),
}

/// The property region of an export whose class has no reflection data.
///
/// Campaign Evolved cooks six assets from `/Script/XGTGPerformanceOverlayTool`
/// — a Microsoft performance-overlay plugin — but ships none of its code. Its
/// three classes (`PerfToolTextBox`, `PerfToolFontSettings`,
/// `PerformanceOverlayToolCookShim`, 19 exports) appear in no `.usmap`, no UHT
/// header dump, not in UE 5.5.4, and — searched byte-wise in both UTF-8 and
/// UTF-16 — in neither shipping executable. `ce_schema_fit` then tried every
/// one of the 1,724 known schemas against the data and found none that decodes
/// `PerfToolTextBox` and consumes its value region, which rules out the
/// "subclass that adds no properties of its own" case that makes
/// `Blam*TagDataAsset` decodable. The types are not recoverable from anything
/// shipped.
///
/// What *is* recoverable is decoded here, because the unversioned header is
/// schema-independent: which flattened indices carry a value, which are
/// zero-masked, and the schema length the header walks. That is enough to
/// re-emit the header rather than retain it, and enough for a caller to say
/// "index 6 is set" — just not what index 6 *is*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnreflectedBlock {
    /// Fully decoded: present slots and their zero-mask bits.
    pub header: super::block::Header,
    /// The flattened schema length the header walks, which is what re-emits it.
    pub schema_len: u16,
    /// Everything after the header. Not split per property — without types
    /// there is no way to know where one value ends and the next begins, so
    /// this necessarily swallows the trailer and tail as well.
    pub rest: Vec<u8>,
}

impl Export {
    /// Equality by *value* — the round-trip contract
    /// `decode(encode(decode(x))) == decode(x)`.
    ///
    /// The bar every model must clear, and the only one a lossy payload can:
    /// whatever could be read out originally must still read out after writing,
    /// whether or not the bytes match.
    ///
    /// While a tail is a retained span its bytes *are* its value, so this is
    /// byte comparison for it today and becomes a real value comparison as each
    /// arm is modeled.
    pub fn semantic_eq(&self, other: &Export) -> bool {
        use ExportBlock::*;
        let block_eq = match (&self.block, &other.block) {
            (Reflected(a), Reflected(b)) => a.semantic_eq(b),
            (NotSerialized, NotSerialized) => true,
            (Unreflected(a), Unreflected(b)) => a == b,
            _ => false,
        };
        block_eq && self.trailer == other.trailer && self.tail == other.tail
    }

    /// The decoded property block, or `None` in either case where there is not
    /// one. Callers that only want to read properties want this; callers that
    /// need to distinguish *why* should match [`ExportBlock`] directly.
    pub fn properties(&self) -> Option<&PropertyBlock> {
        match &self.block {
            ExportBlock::Reflected(b) => Some(b),
            _ => None,
        }
    }

    /// Mutable counterpart of [`Export::properties`].
    pub fn properties_mut(&mut self) -> Option<&mut PropertyBlock> {
        match &mut self.block {
            ExportBlock::Reflected(b) => Some(b),
            _ => None,
        }
    }
}

/// Split an export into property block, `UObject` trailer and unmodeled tail.
pub fn read_export(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
) -> Result<Export> {
    read_export_in(export, names, usmap, class, object_flags, &ExportContext::new(&[]))
}

/// As [`read_export`], but with the package context.
///
/// Some property values cannot be decoded without it. A `UUserDefinedStruct`
/// used as a property type has no `.usmap` schema, and an `FInstancedPropertyBag`
/// names its members' struct types by *object reference* — both need the
/// package to resolve a layout. Without a resolver those exports fail to read,
/// which is the honest outcome: the data is there, the schema is elsewhere.
pub fn read_export_in(
    export: &[u8],
    names: &[String],
    usmap: &Usmap,
    class: &str,
    object_flags: u32,
    ctx: &ExportContext<'_>,
) -> Result<Export> {
    if NO_PROPERTY_BLOCK.contains(&class) {
        return Ok(Export {
            block: ExportBlock::NotSerialized,
            trailer: Trailer::Absent,
            tail: export.to_vec(),
        });
    }
    if !super::block::has_schema(class, usmap) {
        let (header, used, schema_len) = super::block::parse_header_walked(export)?;
        return Ok(Export {
            block: ExportBlock::Unreflected(UnreflectedBlock {
                header,
                schema_len: u16::try_from(schema_len)
                    .with_context(|| format!("{class}: schema length {schema_len} overflows u16"))?,
                rest: export[used..].to_vec(),
            }),
            trailer: Trailer::Absent,
            tail: Vec::new(),
        });
    }
    let mut r = Reader::with_ctx(export, names, ctx);
    let block = read_struct(&mut r, class, usmap, 0)?;
    let mut trailer = Trailer::Absent;
    if export.len() >= r.o + 4 {
        let at = r.o;
        match r.u32()? {
            0 => trailer = Trailer::NoGuid,
            1 => trailer = Trailer::Guid(r.take(16)?.try_into().expect("16 bytes")),
            // Not a boolean, so this export does not follow the trailer model.
            // Rewind and let the bytes belong to the tail, which keeps the
            // round trip exact rather than failing a good property decode.
            _ => r.o = at,
        }
    }
    Ok(Export { block: ExportBlock::Reflected(block), trailer, tail: export[r.o..].to_vec() })
}

/// Reassemble the bytes [`read_export`] took apart.
///
/// Inverse by construction, and measured as such: `ce_export_roundtrip` runs it
/// over every export in the shipped corpus.
pub fn write_export(class: &str, ex: &Export, usmap: &Usmap) -> Result<Vec<u8>> {
    write_export_in(class, ex, usmap, None)
}

/// As [`write_export`], with the package context. Regenerating a nested block's
/// header needs the same layout the reader used, and for a property bag or a
/// user-defined struct that layout lives in another package.
pub fn write_export_in(
    class: &str,
    ex: &Export,
    usmap: &Usmap,
    resolver: Option<&dyn super::archive::PackageResolver>,
) -> Result<Vec<u8>> {
    let mut out = match &ex.block {
        ExportBlock::Reflected(b) => super::block::emit_block_in(class, b, usmap, resolver)?,
        ExportBlock::NotSerialized => Vec::new(),
        // The header is regenerated from the decoded fragments like any other,
        // not replayed: `schema_len` is what makes that possible without a
        // schema. Only the value region is carried verbatim, because nothing
        // shipped says how to type it.
        ExportBlock::Unreflected(u) => {
            let mut b = super::block::emit_header(&u.header, u.schema_len as usize);
            b.extend_from_slice(&u.rest);
            b
        }
    };
    match &ex.trailer {
        Trailer::Absent => {}
        Trailer::NoGuid => out.extend_from_slice(&0u32.to_le_bytes()),
        Trailer::Guid(g) => {
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(g);
        }
    }
    out.extend_from_slice(&ex.tail);
    Ok(out)
}

/// Classes whose `Serialize` never calls `Super`, so they have no property
/// block, no `UObject` trailer and no inherited tails.
pub const NO_PROPERTY_BLOCK: &[&str] = &["RigVM", "RigHierarchy"];

/// The `UObject` trailer **every** export writes after its property block: a
/// four-byte `hasGuid` and, when set, the 16-byte GUID.
///
/// Class-default objects included — `FLazyObjectPtr::PossiblySerializeObjectGuid`
/// (Obj.cpp:148) has no `RF_ClassDefaultObject` branch. `object_flags` is kept
/// in the signature because callers pass it and the distinction is worth being
/// explicit about having checked.
pub(super) fn read_uobject_trailer(r: &mut Reader, _object_flags: u32) -> Result<()> {
    if r.b.len() < r.o + 4 {
        return Ok(());
    }
    match r.u32()? {
        0 => Ok(()),
        1 => r.take(16).map(|_| ()),
        other => bail!("UObject hasGuid is {other}, not a bool (@ {})", r.o - 4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::iostore::usmap::{PropertyType, UsmapProperty};

    /// A one-`int32` class, so an export is a two-byte header, four bytes of
    /// value, the trailer, then whatever the class serializes natively.
    fn probe() -> Usmap {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "ExportProbe",
            None,
            vec![UsmapProperty {
                schema_index: 0,
                array_dim: 1,
                name: "Value".to_string(),
                ty: PropertyType::Int,
            }],
        );
        usmap
    }

    fn body(trailer: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x03]; // one value present, is-last
        v.extend_from_slice(&0x1234_5678i32.to_le_bytes());
        v.extend_from_slice(trailer);
        v.extend_from_slice(tail);
        v
    }

    /// Each trailer shape must survive, and the tail must come back untouched.
    #[test]
    fn export_decomposition_round_trips() {
        let usmap = probe();
        let tail: Vec<u8> = (0u8..32).collect();
        for (trailer_bytes, expect) in [
            (vec![0u8, 0, 0, 0], Trailer::NoGuid),
            (
                [1u8, 0, 0, 0].iter().copied().chain(0u8..16).collect::<Vec<u8>>(),
                Trailer::Guid(core::array::from_fn(|i| i as u8)),
            ),
        ] {
            let raw = body(&trailer_bytes, &tail);
            let ex = read_export(&raw, &[], &usmap, "ExportProbe", 0).expect("read");
            assert_eq!(ex.trailer, expect);
            assert_eq!(ex.tail, tail, "tail was not preserved");
            assert_eq!(write_export("ExportProbe", &ex, &usmap).expect("write"), raw);
        }
    }

    /// A flag that is not a boolean means this export does not follow the
    /// trailer model. Those bytes must stay in the tail rather than being
    /// consumed as a trailer, or they would be dropped on write.
    #[test]
    fn a_malformed_trailer_flag_stays_in_the_tail() {
        let usmap = probe();
        let raw = body(&[0x99, 0x88, 0x77, 0x66], &[1, 2, 3]);
        let ex = read_export(&raw, &[], &usmap, "ExportProbe", 0).expect("read");
        assert_eq!(ex.trailer, Trailer::Absent);
        assert_eq!(ex.tail, vec![0x99, 0x88, 0x77, 0x66, 1, 2, 3]);
        assert_eq!(write_export("ExportProbe", &ex, &usmap).expect("write"), raw);
    }

    /// A class-default object writes the trailer like anything else.
    ///
    /// This test asserted the opposite for most of the project, and was wrong:
    /// `UObject::Serialize` calls `FLazyObjectPtr::PossiblySerializeObjectGuid`
    /// (Obj.cpp:148) unconditionally on save, and that writes
    /// `TryEnterField(TEXT("Guid"), Guid.IsValid())` — the four-byte presence
    /// bool — for every object (LazyObjectPtr.cpp). The only `RF_ClassDefaultObject`
    /// branch nearby is sparse class data, and it is `IsTransacting()`-only.
    ///
    /// It stayed wrong because it is hand-built and no *reachable* CDO had a
    /// tail arm to trip over the four phantom bytes. Once Blueprint-class
    /// exports became reachable, 970 CDO tails failed at "+4 @ 4"; consuming
    /// the trailer took that to 1.
    #[test]
    fn a_class_default_object_still_writes_a_trailer() {
        const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;
        let usmap = probe();
        let raw = body(&[], &[0, 0, 0, 0, 7]);
        let ex = read_export(&raw, &[], &usmap, "ExportProbe", RF_CLASS_DEFAULT_OBJECT)
            .expect("read");
        assert_eq!(ex.trailer, Trailer::NoGuid);
        assert_eq!(ex.tail, vec![7]);
        assert_eq!(write_export("ExportProbe", &ex, &usmap).expect("write"), raw);
    }

    /// `URigVM`/`URigHierarchy` never call `Super::Serialize`, so they have no
    /// block to emit a header for — writing one would corrupt them.
    #[test]
    fn a_class_without_a_property_block_is_all_tail() {
        let usmap = probe();
        let raw: Vec<u8> = (0u8..24).collect();
        let ex = read_export(&raw, &[], &usmap, "RigVM", 0).expect("read");
        assert!(
            matches!(ex.block, ExportBlock::NotSerialized),
            "RigVM must not decode a property block"
        );
        assert_eq!(ex.tail, raw);
        assert_eq!(write_export("RigVM", &ex, &usmap).expect("write"), raw);
    }

    /// A class with no reflection data anywhere still decodes its header and
    /// still round-trips — and stays *distinguishable* from a class that has no
    /// block by design. Conflating the two is what let a gate filter the
    /// `XGTGPerformanceOverlayTool` exports away and report full coverage.
    ///
    /// The bytes are `PerformanceOverlayToolCookShim`'s real payload: a header
    /// of `skip 1, value 0, last`, then four trailing bytes. Nothing is present,
    /// so no types are needed to read it — and `skip 1` pins the flattened
    /// schema length at exactly 1, which is what re-emits those bytes.
    #[test]
    fn a_class_with_no_reflection_data_decodes_its_header_and_round_trips() {
        let usmap = probe();
        let raw = vec![0x01, 0x01, 0x00, 0x00, 0x00, 0x00];
        let ex = read_export(&raw, &[], &usmap, "PerformanceOverlayToolCookShim", 0)
            .expect("read");
        let ExportBlock::Unreflected(un) = &ex.block else {
            panic!("expected an unreflected block, got {:?}", ex.block);
        };
        assert!(un.header.present.is_empty(), "the header declares no values");
        assert_eq!(un.schema_len, 1, "`skip 1` is only writable by a 1-property schema");
        assert_eq!(un.rest, vec![0, 0, 0, 0]);
        assert!(ex.properties().is_none(), "there is no typed block to hand out");
        assert_eq!(
            write_export("PerformanceOverlayToolCookShim", &ex, &usmap).expect("write"),
            raw,
            "the header is regenerated from the decoded fragments, not replayed"
        );
    }

    /// The two block-less cases must not compare equal to each other, or a
    /// semantic round-trip would accept turning one into the other.
    #[test]
    fn an_unreflected_block_is_not_a_missing_block() {
        let usmap = probe();
        let raw = vec![0x01, 0x01, 0x00, 0x00, 0x00, 0x00];
        let unreflected =
            read_export(&raw, &[], &usmap, "PerformanceOverlayToolCookShim", 0).expect("read");
        let none = read_export(&raw, &[], &usmap, "RigVM", 0).expect("read");
        assert!(!unreflected.semantic_eq(&none));
        assert!(!none.semantic_eq(&unreflected));
    }
}
