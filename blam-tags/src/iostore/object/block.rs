//! The `FUnversionedHeader` codec and the walk over one property block.
//!
//! A cooked property block is a run of `skip`/`value` fragments plus an optional
//! zero mask, then the present values back to back. `FUnversionedHeaderBuilder`
//! (UnversionedPropertySerialization.cpp:795) is fully deterministic, so this
//! block can be *regenerated* rather than retained — verified byte-exact against
//! every export in the shipped corpus.

use anyhow::{bail, Context, Result};
use std::sync::Arc;

use super::archive::{trace_enabled, Ar, Reader};
use super::usmap::{PropertyType, Usmap, UsmapProperty};
use super::limits::MAX_DEPTH;
use super::value::{BlockLayout, FName, PropValue, PropertyBlock, PropertyEntry, SchemaSlot};
use super::native_bool::{bool_is_known, is_native_bool};
use super::property::{can_serialize_as_zero, read_value, should_save_as_zero, write_value};
use super::native::NativeStruct;
use super::structs::native_struct_size;

/// One `FUnversionedHeader` fragment (`FFragment`,
/// UnversionedPropertySerialization.cpp:621).
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Fragment {
    skip: u8,
    has_zeroes: bool,
    value_num: u8,
    is_last: bool,
}

impl Fragment {
    const SKIP_MAX: u8 = 127;
    const VALUE_MAX: u8 = 127;

    fn unpack(p: u16) -> Self {
        Fragment {
            skip: (p & 0x7f) as u8,
            has_zeroes: (p & 0x80) != 0,
            is_last: (p & 0x100) != 0,
            value_num: (p >> 9) as u8,
        }
    }

    fn pack(self) -> u16 {
        self.skip as u16
            | if self.has_zeroes { 0x0080 } else { 0 }
            | ((self.value_num as u16) << 9)
            | if self.is_last { 0x0100 } else { 0 }
    }
}

/// What a property block's header says, in the form a writer can replay.
///
/// `present` is the schema indices carrying a value, each paired with whether
/// that value is non-zero; a zero-masked property serializes no bytes at all.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Header {
    pub present: Vec<(usize, bool)>,
    /// Empty `(skip 0, value 0)` fragments before the first real one.
    ///
    /// `FUnversionedHeaderBuilder` cannot emit these, and nothing Epic cooked
    /// has them — but every one of Campaign Evolved's 12,161 `/Game/Tags`
    /// wrappers begins with exactly two, so i343's tag tool writes its own
    /// headers. UE's loader skips them (`FIterator::Skip` walks past
    /// `ValueNum == 0`), so they are inert; carrying the count is what lets us
    /// reproduce those packages byte-for-byte rather than merely equivalently.
    pub leading_empty: u8,
}

/// `FUnversionedHeaderBuilder` (UnversionedPropertySerialization.cpp:795),
/// ported literally so a header can be *regenerated* from the property set
/// rather than retained as bytes.
#[derive(Debug)]
struct HeaderBuilder {
    fragments: Vec<Fragment>,
    zero_mask: Vec<bool>,
}

impl HeaderBuilder {
    fn new() -> Self {
        HeaderBuilder { fragments: vec![Fragment::default()], zero_mask: Vec::new() }
    }

    /// Drop the zero-mask bits of a fragment that turned out to have no zeroes.
    /// Called when *closing* a fragment, so it always applies to the last one.
    fn trim_zero_mask(&mut self) {
        let last = *self.fragments.last().expect("never empty");
        if !last.has_zeroes {
            let keep = self.zero_mask.len() - last.value_num as usize;
            self.zero_mask.truncate(keep);
        }
    }

    fn include(&mut self, is_zero: bool) {
        if self.fragments.last().expect("never empty").value_num == Fragment::VALUE_MAX {
            self.trim_zero_mask();
            self.fragments.push(Fragment::default());
        }
        let last = self.fragments.last_mut().expect("never empty");
        last.value_num += 1;
        last.has_zeroes |= is_zero;
        self.zero_mask.push(is_zero);
    }

    fn exclude(&mut self) {
        let last = *self.fragments.last().expect("never empty");
        if last.value_num != 0 || last.skip == Fragment::SKIP_MAX {
            self.trim_zero_mask();
            self.fragments.push(Fragment::default());
        }
        self.fragments.last_mut().expect("never empty").skip += 1;
    }

    /// Trailing value-less fragments are dropped — but only down to one, which
    /// is why a block with nothing present still encodes `min(schema_len, 127)`
    /// skips rather than nothing at all.
    fn finalize(&mut self) {
        self.trim_zero_mask();
        while self.fragments.len() > 1 && self.fragments.last().expect("never empty").value_num == 0
        {
            self.fragments.pop();
        }
        self.fragments.last_mut().expect("never empty").is_last = true;
    }

    /// `FUnversionedHeader::Save` + `SaveZeroMaskData`.
    fn save(&self, out: &mut Vec<u8>) {
        for f in &self.fragments {
            out.extend_from_slice(&f.pack().to_le_bytes());
        }
        let bits = self.zero_mask.len();
        if bits == 0 {
            return;
        }
        let mut words = vec![0u32; bits.div_ceil(32)];
        for (i, &b) in self.zero_mask.iter().enumerate() {
            if b {
                words[i / 32] |= 1 << (i % 32);
            }
        }
        // The last word is masked to the bits in use; the width of the mask as a
        // whole is chosen by bit count, not by word count.
        let last = u32::MAX >> ((32 - bits % 32) % 32);
        if bits <= 8 {
            out.push((words[0] & last) as u8);
        } else if bits <= 16 {
            out.extend_from_slice(&((words[0] & last) as u16).to_le_bytes());
        } else {
            let n = words.len();
            for w in &words[..n - 1] {
                out.extend_from_slice(&w.to_le_bytes());
            }
            out.extend_from_slice(&(words[n - 1] & last).to_le_bytes());
        }
    }
}

/// Emit the `FUnversionedHeader` bytes for a property block.
///
/// `schema_len` is the class's flattened property count. UE walks the *whole*
/// schema calling Include/Exclude per property, and [`HeaderBuilder::finalize`]
/// pops trailing skips only down to one — so the length is load-bearing for
/// blocks with nothing present, and stopping at the last present index instead
/// produces the wrong bytes for them.
pub(super) fn write_header_inner(header: &Header, schema_len: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..header.leading_empty {
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    let mut b = HeaderBuilder::new();
    let mut it = header.present.iter().peekable();
    for i in 0..schema_len {
        match it.peek() {
            Some(&&(idx, non_zero)) if idx == i => {
                b.include(!non_zero);
                it.next();
            }
            _ => b.exclude(),
        }
    }
    b.finalize();
    b.save(&mut out);
    out
}

/// Emit a whole property block: its header, then its present values.
///
/// The header is *derived* from the entries rather than retained — each entry's
/// [`SchemaSlot`] says where it sat and whether it was zero-masked, which is
/// exactly the two things `FUnversionedHeaderBuilder` consumes. A zero-masked
/// entry contributes a header bit and no bytes.
///
/// `flat` is the class's flattened schema, which supplies each value's declared
/// type. It has to be passed in rather than looked up here because a block does
/// not name its own class: the caller reaches a nested one *through* the
/// property whose type named it.
pub(super) fn write_block(
    ar: &mut impl Ar,
    block: &PropertyBlock,
    flat: &[(&UsmapProperty, u8, &str)],
    usmap: &Usmap,
) -> Result<()> {
    let BlockLayout::Unversioned { schema_len, leading_empty } = block.layout;

    // The block records the schema length it was read against, and the caller
    // resolved the schema independently. If those disagree the header would be
    // silently wrong — a different `.usmap` than the data was cooked with, or a
    // block attached to the wrong property. Say so instead.
    if schema_len as usize != flat.len() {
        bail!(
            "block was read against a {schema_len}-property schema but the one supplied has {}",
            flat.len()
        );
    }

    // Masking is decided per entry by *both* halves of what the engine knows:
    // whether this property may be masked at all, and whether its value is
    // currently zero.
    //
    // Replaying the file's bit alone is wrong — edit a masked property to a
    // non-zero value and the edit is silently discarded, since a masked entry
    // writes no bytes. Deriving it alone is also wrong, and measurably so
    // (`ce_zero_mask_census`): 629,385 of 6,304,759 entries would be masked
    // that the cooker wrote longhand, 627,396 of them `Bool`, because
    // `CanSerializeAsZero` gates bools on `IsNativeBool()` and the `.usmap`
    // cannot tell a real `bool` from a bitfield `uint8 bFoo:1`. Masking a
    // bitfield is not merely different, it is unsafe: `LoadZero` memzeroes the
    // property's storage and would clear the sibling bits sharing that byte.
    //
    // So the file supplies maskability — it is a static fact about the property,
    // and a cooked block that wrote a zero longhand has *proven* that property
    // is not maskable — and the value supplies zero-ness. The census measured
    // zero entries where the cooker masked something we would not, so this is
    // never too permissive.
    //
    // Consequence, deliberately accepted: editing an unmasked property to zero
    // writes it longhand where the cooker might have masked it. That loads to
    // the identical value, and for a bitfield it is the only safe encoding.
    let mut plan = Vec::with_capacity(block.entries.len());
    for e in &block.entries {
        let slot = e.slot.with_context(|| {
            format!("property {} has no schema slot, so its block cannot be written", e.name)
        })?;
        let (prop, _, owner) = *flat.get(slot.index as usize).with_context(|| {
            format!("schema index {} beyond {} properties", slot.index, flat.len())
        })?;
        // Maskability comes from two different kinds of evidence.
        //
        // For a `bool` it is now a *derived* fact: `native_bool` records which
        // declarations are real `bool`s and which are `: 1` bitfields, scraped
        // from the game's own UHT headers, and `ce_native_bool_check` measured
        // it against all 1,412,525 bool entries in the corpus with zero
        // disagreements in either direction. Deriving it is strictly better than
        // replaying the file's bit, because a *newly inserted* property has no
        // bit to replay — it would otherwise always write longhand.
        //
        // Everything else still has to replay it. `CanSerializeAsZero` gates
        // non-bool, non-struct properties on `CPF_ZeroConstructor |
        // CPF_NoDestructor`, and the `.usmap` carries neither flag, so the file
        // remains the only evidence: a cooked block that wrote a zero longhand
        // has proven that property is not maskable. `ce_zero_mask_census`
        // measures the residue this still covers for us -- 1,795 `Object` and
        // 194 `Name` entries, across 3 declarations.
        // A bool whose declaration the dump *knows* derives its maskability from
        // the table. One it has never seen — a field of a Blueprint-defined
        // struct, say, which exists in no C++ header — falls back to replaying
        // the file's bit, which is what every type did before the table existed.
        // Answering "not native" for an unknown declaration is wrong in the
        // expensive direction: it stops masking something the cooker masked, and
        // that rewrites the fragment header.
        let maskable = match &prop.ty {
            PropertyType::Bool if bool_is_known(owner, &prop.name) => {
                is_native_bool(owner, &prop.name)
            }
            ty => slot.zero_masked && can_serialize_as_zero(ty),
        };
        let is_zero = maskable && should_save_as_zero(&prop.ty, &e.value, usmap);
        plan.push((e, &prop.ty, slot.index as usize, is_zero));
    }
    let present: Vec<(usize, bool)> =
        plan.iter().map(|(_, _, idx, is_zero)| (*idx, !*is_zero)).collect();
    // `HeaderBuilder` walks the schema in order, so the entries must be in it.
    if present.windows(2).any(|w| w[0].0 >= w[1].0) {
        bail!("property block entries are not in ascending schema order");
    }
    let header = write_header_inner(&Header { present, leading_empty }, schema_len as usize);
    let n = header.len();
    ar.raw(&mut header.clone(), n)?;

    for (e, ty, _, is_zero) in plan {
        if is_zero {
            continue; // The mask bit *is* the encoding; no bytes follow.
        }
        write_value(ar, ty, &e.value, false, usmap)
            .with_context(|| format!("writing property {}", e.name))?;
    }
    Ok(())
}

/// Emit a whole property block for a named class — header and values.
///
/// The write counterpart to
/// [`read_export_struct_len`](super::export::read_export_struct_len), and the
/// gate `ce_block_roundtrip` measures: for an unmodified block this must
/// reproduce the exact bytes it was read from.
pub fn emit_block(class: &str, block: &PropertyBlock, usmap: &Usmap) -> Result<Vec<u8>> {
    emit_block_in(class, block, usmap, None)
}

/// As [`emit_block`], with the package context a nested layout may need.
pub fn emit_block_in(
    class: &str,
    block: &PropertyBlock,
    usmap: &Usmap,
    resolver: Option<&dyn super::archive::PackageResolver>,
) -> Result<Vec<u8>> {
    let mut w = super::archive::Writer::with_resolver(resolver);
    let flat = flattened_schema(class, usmap)?;
    write_block(&mut w, block, &flat, usmap)?;
    Ok(w.into_bytes())
}

/// Read an `FUnversionedHeader`, returning `(present_schema_indices, ...)`
/// where each present index is paired with whether its value is non-zero (a
/// zero-masked property serializes no bytes — it is the zero value).
pub(super) fn read_header(r: &mut Reader) -> Result<Header> {
    let mut frags = Vec::new();
    let mut zero_mask_num = 0usize;
    let mut leading_empty = 0u8;
    let mut seen_non_empty = false;
    loop {
        let packed = r.u16()?;
        let frag = Fragment::unpack(packed);
        // A wholly empty fragment before any real one is CE tag-wrapper
        // padding (see `Header::leading_empty`). `is_last` terminates the
        // header, so such a fragment is never a lead-in even when it is bare.
        if packed == 0 && !seen_non_empty {
            leading_empty = leading_empty.saturating_add(1);
        } else {
            seen_non_empty = true;
        }
        if frag.has_zeroes {
            zero_mask_num += frag.value_num as usize;
        }
        let last = frag.is_last;
        frags.push(frag);
        if last {
            break;
        }
    }
    // Zero mask: one bit per value in has-zeroes fragments.
    let mut zero_mask = Vec::with_capacity(zero_mask_num);
    if zero_mask_num > 0 {
        let (words, word_bits): (Vec<u32>, usize) = if zero_mask_num <= 8 {
            (vec![r.u8()? as u32], 8)
        } else if zero_mask_num <= 16 {
            (vec![r.u16()? as u32], 16)
        } else {
            let n = zero_mask_num.div_ceil(32);
            let mut w = Vec::with_capacity(n);
            for _ in 0..n {
                w.push(r.u32()?);
            }
            (w, 32)
        };
        for i in 0..zero_mask_num {
            zero_mask.push((words[i / word_bits] >> (i % word_bits)) & 1 == 1);
        }
    }

    let mut present = Vec::new();
    let mut schema_it = 0usize;
    let mut zi = 0usize;
    for frag in &frags {
        schema_it += frag.skip as usize;
        for _ in 0..frag.value_num {
            let non_zero = if frag.has_zeroes {
                let nz = !zero_mask[zi];
                zi += 1;
                nz
            } else {
                true
            };
            present.push((schema_it, non_zero));
            schema_it += 1;
        }
    }
    Ok(Header { present, leading_empty })
}


/// Read a full reflected struct/class instance (its unversioned property
/// block) named `class`, returning present property name→value.
pub(super) fn read_struct(r: &mut Reader, class: &str, usmap: &Usmap, depth: usize) -> Result<PropertyBlock> {
    if depth > MAX_DEPTH {
        bail!("unversioned struct nesting too deep at {class}");
    }
    // A `UUserDefinedStruct` used as a property type is in no `.usmap` under
    // any name; its layout has to come back out of the package that defines it.
    if usmap.get(class).is_none() {
        if let Some(fields) = r.resolver.and_then(|p| p.struct_layout(class)) {
            // A recovered `FField` chain occupies `array_dim` slots too, the
            // same as a `.usmap` one.
            // A user-defined struct declares every field itself, so it is its
            // own owner for the purposes of the native-bool lookup.
            let schema: Vec<(&UsmapProperty, u8, &str)> = fields
                .iter()
                .flat_map(|f| (0..f.array_dim.max(1)).map(move |i| (f, i, class)))
                .collect();
            return read_struct_with_schema(r, class, &schema, usmap, depth);
        }
    }
    let flat = flattened_schema(class, usmap)?;
    read_struct_with_schema(r, class, &flat, usmap, depth)
}

/// The flattened property schema the unversioned fragment stream indexes by,
/// each slot paired with its index within a static array.
pub(super) fn flattened_schema<'u>(
    class: &str,
    usmap: &'u Usmap,
) -> Result<Vec<(&'u UsmapProperty, u8, &'u str)>> {
    // Campaign Evolved ships `Blam*TagDataAsset` classes that appear in neither
    // the `.usmap` nor the UHT dump (`BlamFrameEventListTagDataAsset` alone
    // covers 130 exports). They add no properties of their own over the shared
    // base, so decoding them against `BlamTagDataAssetBase` recovers the whole
    // property block rather than failing outright.
    usmap
        .flattened_owned_slots(class)
        .or_else(|| {
            (class.starts_with("Blam") && class.ends_with("TagDataAsset"))
                .then(|| usmap.flattened_owned_slots("BlamTagDataAssetBase"))
                .flatten()
        })
        .with_context(|| format!("no .usmap schema for struct {class}"))
}

/// Walk one unversioned property block against an explicit schema.
///
/// Split out of [`read_struct`] because not every schema comes from the
/// `.usmap`: a `UUserDefinedStruct`'s default instance and a `UDataTable`'s
/// rows are indexed by a property list recovered from *package* bytes.
/// `label` only names the block in errors and traces.
pub(super) fn read_struct_with_schema(
    r: &mut Reader,
    label: &str,
    flat: &[(&UsmapProperty, u8, &str)],
    usmap: &Usmap,
    depth: usize,
) -> Result<PropertyBlock> {
    let class = label;
    let header_start = r.o;
    let header = read_header(r)?;
    if trace_enabled() {
        eprintln!(
            "{:indent$}{class} @ {header_start} (header {} bytes, present {:?})",
            "",
            r.o - header_start,
            header.present,
            indent = depth * 2
        );
    }
    let mut entries = Vec::with_capacity(header.present.len());
    for (idx, non_zero) in header.present {
        let (prop, slot, _) = *flat
            .get(idx)
            .with_context(|| format!("{class}: present schema index {idx} beyond {} props", flat.len()))?;
        let start = r.o;
        let value = if non_zero {
            read_value(r, &prop.ty, usmap, depth, false)?
        } else {
            // Zero-masked: the property is its zero value, no bytes consumed.
            zero_value(&prop.ty)
        };
        if trace_enabled() {
            eprintln!(
                "{:indent$}  [{idx}] {}{} : {:?} @ {start}..{}{}",
                "",
                prop.name,
                if prop.array_dim > 1 { format!("[{slot}]") } else { String::new() },
                prop.ty,
                r.o,
                if non_zero { "" } else { " (zero-masked)" },
                indent = depth * 2
            );
        }
        // Each present schema index is its own entry, so a static array's slots
        // and two properties that shadow a name stay distinct instead of one
        // overwriting the other. Slots the block never mentions produce no
        // entry at all — absent, rather than given an invented value.
        entries.push(PropertyEntry {
            name: Arc::from(prop.name.as_str()),
            value,
            slot: Some(SchemaSlot {
                index: idx as u32,
                array_index: slot,
                zero_masked: !non_zero,
            }),
        });
    }
    Ok(PropertyBlock {
        entries,
        layout: BlockLayout::Unversioned {
            schema_len: flat.len() as u32,
            leading_empty: header.leading_empty,
        },
    })
}

/// The implicit value of a zero-masked property, which serialized no bytes.
///
/// `LoadZero` (UnversionedPropertySerialization.cpp:122) memzeroes the
/// property's storage, so the value is whatever all-zero bytes mean for that
/// type — a real value, not an absence. Returning "unknown" here threw away
/// **1,501,709** values across the shipped corpus (1,280,279 enums and 221,430
/// names), and did so invisibly, because the value being lost is the default
/// one.
///
/// Only types `CanSerializeAsZero` accepts can appear here at all: it demands
/// `CPF_ZeroConstructor | CPF_NoDestructor`, or `STRUCT_Atomic` for a struct.
/// Measured over the whole corpus, the zero-masked population is exactly enums,
/// names and twelve small atomic structs — never a string, array, map, set,
/// text, delegate, soft object or field path, all of which have destructors.
pub(super) fn zero_value(ty: &PropertyType) -> PropValue {
    match ty {
        PropertyType::Bool => PropValue::Bool(false),
        PropertyType::Int
        | PropertyType::Int8
        | PropertyType::Int16
        | PropertyType::Int64
        | PropertyType::UInt16
        | PropertyType::UInt32
        | PropertyType::UInt64
        | PropertyType::Byte { enum_name: None } => PropValue::Int(0),
        PropertyType::Float | PropertyType::Double => PropValue::Float(0.0),
        PropertyType::Object
        | PropertyType::Interface
        | PropertyType::WeakObject
        | PropertyType::LazyObject => PropValue::Object(0),
        // An `FName` of index 0 is `NAME_None`, whatever the package name map
        // holds at that slot — the zero is the engine's, not the package's.
        PropertyType::Name => PropValue::Name(FName::none()),
        // A zero enum is its underlying integer's zero. A `TEnumAsByte` naming
        // an enum lands here too, via `FByteProperty`.
        PropertyType::Enum { inner, .. } => zero_value(inner),
        PropertyType::Byte { enum_name: Some(_) } => PropValue::Int(0),
        // An atomic struct's zero is that many zero bytes, which keeps
        // `MeshTransform::from_prop` and friends working on defaulted values
        // instead of seeing an opaque hole.
        PropertyType::Struct(name) => match native_struct_size(name) {
            Some(size) => NativeStruct::decode(name, &vec![0u8; size])
                .map(PropValue::Native)
                .unwrap_or(PropValue::Raw(vec![0; size])),
            None => PropValue::Struct(PropertyBlock::default()),
        },
        PropertyType::Optional(_) => PropValue::Unset,
        // Everything else is not zero-maskable per `CanSerializeAsZero`, so
        // reaching this arm means either a schema we have misread or an engine
        // change. An empty byte span is the honest answer: no bytes were
        // serialized, and we decline to invent a value.
        _ => PropValue::Raw(Vec::new()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip a header through the reader and the builder.
    fn roundtrip(present: &[(usize, bool)], schema_len: usize, leading_empty: u8) -> Vec<u8> {
        let h = Header { present: present.to_vec(), leading_empty };
        let bytes = emit_header(&h, schema_len);
        let mut r = Reader::new(&bytes, &[]);
        let back = read_header(&mut r).expect("re-read the header we just wrote");
        assert_eq!(back, h, "header did not survive a write/read round trip");
        assert_eq!(r.o, bytes.len(), "reader did not consume the whole header");
        bytes
    }

    /// `Finalize` pops trailing value-less fragments only *down to one*, so a
    /// block with nothing present still encodes `min(schema_len, 127)` skips.
    /// Emitting a bare `is_last` fragment instead is the obvious reading and is
    /// wrong for 62,606 exports in the shipped corpus.
    #[test]
    fn empty_block_still_encodes_its_schema_length() {
        assert_eq!(roundtrip(&[], 16, 0), 0x0110u16.to_le_bytes());
        assert_eq!(roundtrip(&[], 46, 0), 0x012eu16.to_le_bytes());
        // Capped at SkipMax, because every fragment past the first is popped.
        assert_eq!(roundtrip(&[], 300, 0), 0x017fu16.to_le_bytes());
        // A class with no properties at all is a bare terminator.
        assert_eq!(roundtrip(&[], 0, 0), 0x0100u16.to_le_bytes());
    }

    /// Campaign Evolved's tag wrappers open with two empty fragments that
    /// `FUnversionedHeaderBuilder` cannot produce. Carrying the count is what
    /// makes those 12,161 packages reproducible byte-for-byte.
    #[test]
    fn leading_empty_fragments_are_preserved() {
        let bytes = roundtrip(&[(0, true)], 8, 2);
        assert_eq!(bytes, [0x00, 0x00, 0x00, 0x00, 0x00, 0x03]);
    }

    /// Trailing skips *after* a value vanish, because the pop loop reaches them.
    #[test]
    fn trailing_skips_after_a_value_are_dropped() {
        let with = emit_header(&Header { present: vec![(0, true)], leading_empty: 0 }, 64);
        let without = emit_header(&Header { present: vec![(0, true)], leading_empty: 0 }, 1);
        assert_eq!(with, without, "trailing skips should not reach the output");
    }

    /// A fragment holds at most 127 skips and 127 values, so crossing either
    /// boundary must open a new one without disturbing the zero mask.
    #[test]
    fn fragment_boundaries_round_trip() {
        roundtrip(&[(200, true)], 201, 0);
        let many: Vec<(usize, bool)> = (0..300).map(|i| (i, true)).collect();
        roundtrip(&many, 300, 0);
        let alternating: Vec<(usize, bool)> = (0..300).map(|i| (i, i % 3 != 0)).collect();
        roundtrip(&alternating, 300, 0);
    }

    /// The zero mask is only written for fragments that actually carry a zero,
    /// and its width is chosen by bit count: u8 to 8 bits, u16 to 16, then u32
    /// words. Each boundary is a place to be off by one.
    #[test]
    fn zero_mask_widths() {
        // Index 0 is always the zero one, so every case really does carry a
        // mask; with no zeroes at all the mask is omitted entirely (below).
        for n in [1usize, 8, 9, 16, 17, 32, 33, 64, 65] {
            let present: Vec<(usize, bool)> = (0..n).map(|i| (i, i % 2 == 1)).collect();
            let bytes = roundtrip(&present, n, 0);
            let mask_bytes = bytes.len() - 2; // one fragment
            let expect = if n <= 8 { 1 } else if n <= 16 { 2 } else { 4 * n.div_ceil(32) };
            assert_eq!(mask_bytes, expect, "wrong zero-mask width for {n} values");
        }
        // All non-zero: no mask at all.
        let present: Vec<(usize, bool)> = (0..40).map(|i| (i, true)).collect();
        assert_eq!(roundtrip(&present, 40, 0).len(), 2);
    }
    use super::super::export::{read_export_struct, read_export_struct_len};
    use crate::iostore::usmap::UsmapProperty;

    /// Build a two-property schema — a `TSet` followed by an `int32` — and
    /// decode a hand-written stream through it.
    ///
    /// A `TSet` serializes like a `TMap`: `NumElementsToRemove` *then* `Num`.
    /// Reading it as a bare `TArray` consumes only one of the two, so the
    /// trailing `int32` is read four bytes early. That failure is silent —
    /// the shifted bytes still decode as a perfectly plausible integer — which
    /// is why it needs a test rather than an eyeball.
    #[test]
    fn set_consumes_its_remove_count_so_later_properties_stay_aligned() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SetAlignmentProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "Tags".to_string(),
                    ty: PropertyType::Set(Box::new(PropertyType::Int)),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "Value".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        // One fragment: skip 0, two values present, none zero, is-last.
        // (ValueNum occupies bits 9+, the is-last flag is bit 8.)
        let fragment: u16 = (2 << 9) | 0x0100;
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&0i32.to_le_bytes()); // NumElementsToRemove
        export.extend_from_slice(&0i32.to_le_bytes()); // Num (empty set)
        export.extend_from_slice(&0x1122_3344i32.to_le_bytes()); // Value

        let props = read_export_struct(&export, &[], &usmap, "SetAlignmentProbe")
            .expect("decode probe struct");

        assert!(matches!(props.get("Tags"), Some(PropValue::Array(v)) if v.is_empty()));
        assert!(
            matches!(props.get("Value"), Some(PropValue::Int(0x1122_3344))),
            "int after a set was read from the wrong offset: {:?}",
            props.get("Value")
        );
    }

    /// `FPerPlatformFloat` writes `bool bCooked` before its `Default`, and
    /// `FArchive` writes a bool as **four** bytes — so the cooked struct is
    /// eight bytes, not four.
    ///
    /// Reading it as a bare `float` leaves the stream four bytes short, and the
    /// property after it still decodes as a perfectly plausible number, so the
    /// damage only surfaces much later (or never). This single size was what
    /// blocked `SkeletalMesh` and `StaticMesh` entirely: every LOD's
    /// `ScreenSize` is a `PerPlatformFloat`.
    #[test]
    fn per_platform_float_consumes_its_cooked_flag() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "PerPlatformAlignmentProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "ScreenSize".to_string(),
                    ty: PropertyType::Struct("PerPlatformFloat".to_string()),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "LODHysteresis".to_string(),
                    ty: PropertyType::Float,
                },
            ],
        );

        let mut export = Vec::new();
        let fragment: u16 = (2 << 9) | 0x0100; // skip 0, two values, is-last
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&1i32.to_le_bytes()); // bCooked
        export.extend_from_slice(&1.0f32.to_le_bytes()); // Default
        export.extend_from_slice(&0.02f32.to_le_bytes()); // LODHysteresis

        let (props, used) =
            read_export_struct_len(&export, &[], &usmap, "PerPlatformAlignmentProbe")
                .expect("decode probe struct");

        assert_eq!(used, export.len(), "walk did not consume the whole block");
        assert!(
            matches!(props.get("LODHysteresis"), Some(PropValue::Float(v)) if (*v - 0.02).abs() < 1e-6),
            "float after a PerPlatformFloat was misaligned: {:?}",
            props.get("LODHysteresis")
        );
    }

    /// A `UPROPERTY` declared `Thing[N]` occupies N consecutive schema slots,
    /// each independently present. Keying the output on property name alone
    /// collapses all N into whichever came last — silently, because the result
    /// is a perfectly plausible single value. 76 classes and 42,172 exports in
    /// the shipped corpus have at least one.
    #[test]
    fn static_array_slots_do_not_overwrite_each_other() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "StaticArrayProbe",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 4,
                    name: "Tints".to_string(),
                    ty: PropertyType::Int,
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "After".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        // Five values present (four array slots then `After`), none zero.
        export.extend_from_slice(&(((5u16) << 9) | 0x0100).to_le_bytes());
        for v in [11i32, 22, 33, 44, 0x5EED] {
            export.extend_from_slice(&v.to_le_bytes());
        }

        let (props, used) =
            read_export_struct_len(&export, &[], &usmap, "StaticArrayProbe").expect("decode");
        assert_eq!(used, export.len(), "walk did not consume the whole block");
        // Each slot is its own entry, tagged with which slot it is, so none can
        // overwrite a sibling and the block stays replayable.
        let got: Vec<(u8, i64)> = props
            .slots("Tints")
            .map(|e| match (e.slot.expect("schema slot"), &e.value) {
                (slot, PropValue::Int(n)) => (slot.array_index, *n),
                (_, other) => panic!("expected ints in the static array, got {other:?}"),
            })
            .collect();
        assert_eq!(got, vec![(0, 11), (1, 22), (2, 33), (3, 44)]);
        assert!(matches!(props.get("After"), Some(PropValue::Int(0x5EED))));
    }

    /// Slots the block never mentions produce no entry at all. Absent is not
    /// the same as zero, and inventing a placeholder for one would put a value
    /// into the block that the file never had.
    #[test]
    fn absent_static_array_slots_stay_unset() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SparseArrayProbe",
            None,
            vec![UsmapProperty {
                schema_index: 0,
                array_dim: 3,
                name: "Slots".to_string(),
                ty: PropertyType::Int,
            }],
        );
        let mut export = Vec::new();
        // Skip slot 0, then one value for slot 1, and nothing for slot 2.
        export.extend_from_slice(&(((1u16) << 9) | 0x0100 | 1).to_le_bytes());
        export.extend_from_slice(&7i32.to_le_bytes());
        let props = read_export_struct(&export, &[], &usmap, "SparseArrayProbe").expect("decode");
        let got: Vec<(u8, i64)> = props
            .slots("Slots")
            .map(|e| match (e.slot.expect("schema slot"), &e.value) {
                (slot, PropValue::Int(n)) => (slot.array_index, *n),
                (_, other) => panic!("expected an int, got {other:?}"),
            })
            .collect();
        assert_eq!(got, vec![(1, 7)], "only the slot the block mentions exists");
    }

    /// Editing a zero-masked property to a non-zero value must start emitting
    /// bytes for it.
    ///
    /// This is the failure that replaying the file's mask bit produces, and it
    /// is silent and total: a masked entry writes *no bytes*, so the edit is
    /// discarded and the property loads back as zero. The round-trip gate cannot
    /// see it — every block it checks is unmodified — which is why it needs a
    /// test rather than a corpus run.
    #[test]
    fn editing_a_masked_property_stops_masking_it() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "MaskedEditProbe",
            None,
            vec![UsmapProperty {
                schema_index: 0,
                array_dim: 1,
                name: "Value".to_string(),
                ty: PropertyType::Int,
            }],
        );
        // One value present, zero-masked: no value bytes follow the header.
        let export = [0x80u8, 0x03, 0x01];
        let (mut block, used) =
            read_export_struct_len(&export, &[], &usmap, "MaskedEditProbe").expect("decode");
        assert_eq!(used, export.len());
        assert!(block.entries[0].slot.expect("slot").zero_masked);
        assert!(matches!(block.entries[0].value, PropValue::Int(0)));

        // Unmodified, it must reproduce the original bytes exactly.
        assert_eq!(emit_block("MaskedEditProbe", &block, &usmap).expect("emit"), export);

        // Edited, the mask must drop and the value must appear.
        block.entries[0].value = PropValue::Int(7);
        let out = emit_block("MaskedEditProbe", &block, &usmap).expect("emit edited");
        let (back, _) =
            read_export_struct_len(&out, &[], &usmap, "MaskedEditProbe").expect("re-read");
        assert!(
            matches!(back.entries[0].value, PropValue::Int(7)),
            "the edit was discarded by the zero mask: {:?}",
            back.entries[0].value
        );
        assert!(!back.entries[0].slot.expect("slot").zero_masked);
    }

    /// A non-empty set must still leave the stream aligned.
    #[test]
    fn non_empty_set_stays_aligned() {
        let mut usmap = Usmap::meteorite().expect("bundled usmap");
        usmap.register_struct(
            "SetAlignmentProbe2",
            None,
            vec![
                UsmapProperty {
                    schema_index: 0,
                    array_dim: 1,
                    name: "Tags".to_string(),
                    ty: PropertyType::Set(Box::new(PropertyType::Int)),
                },
                UsmapProperty {
                    schema_index: 1,
                    array_dim: 1,
                    name: "Value".to_string(),
                    ty: PropertyType::Int,
                },
            ],
        );

        let mut export = Vec::new();
        let fragment: u16 = (2 << 9) | 0x0100;
        export.extend_from_slice(&fragment.to_le_bytes());
        export.extend_from_slice(&0i32.to_le_bytes()); // NumElementsToRemove
        export.extend_from_slice(&2i32.to_le_bytes()); // Num
        export.extend_from_slice(&7i32.to_le_bytes());
        export.extend_from_slice(&9i32.to_le_bytes());
        export.extend_from_slice(&0x0BAD_F00Di32.to_le_bytes());

        let props = read_export_struct(&export, &[], &usmap, "SetAlignmentProbe2")
            .expect("decode probe struct");

        match props.get("Tags") {
            Some(PropValue::Array(v)) => {
                let ints: Vec<i64> = v
                    .iter()
                    .map(|e| match e {
                        PropValue::Int(n) => *n,
                        other => panic!("expected ints in the set, got {other:?}"),
                    })
                    .collect();
                assert_eq!(ints, vec![7, 9]);
            }
            other => panic!("expected a 2-element set, got {other:?}"),
        }
        assert!(
            matches!(props.get("Value"), Some(PropValue::Int(0x0BAD_F00D))),
            "int after a non-empty set was misaligned: {:?}",
            props.get("Value")
        );
    }
}

/// Parse an `FUnversionedHeader` from the start of an export's bytes, returning
/// it and how many bytes it occupied.
///
/// The byte-slice form, so callers outside this layer need not know about the
/// cursor. Note the header is the very first thing in every export that has a
/// property block, so `bytes` is normally the export itself.
pub fn parse_header(bytes: &[u8]) -> Result<(Header, usize)> {
    let mut r = Reader::new(bytes, &[]);
    let h = read_header(&mut r)?;
    Ok((h, r.o))
}

/// Emit the `FUnversionedHeader` bytes for `header` against a class whose
/// flattened schema has `schema_len` properties.
///
/// Inverse of [`parse_header`]: `emit_header(parse_header(x).0, len) == x` for
/// every export in the shipped corpus (see `ce_header_roundtrip`).
pub fn emit_header(header: &Header, schema_len: usize) -> Vec<u8> {
    write_header_inner(header, schema_len)
}

#[cfg(test)]
mod zero_value_tests {
    use super::*;

    /// A zero-masked property is a *value*, not an absence. Returning an opaque
    /// placeholder for the non-scalar cases discarded 1,501,709 values across
    /// the shipped corpus — invisibly, because what goes missing is the default.
    #[test]
    fn zero_masked_names_and_enums_keep_their_value() {
        assert!(
            matches!(zero_value(&PropertyType::Name), PropValue::Name(n) if n == "None"),
            "a zero FName is NAME_None, not an unknown"
        );
        let e = PropertyType::Enum {
            inner: Box::new(PropertyType::Byte { enum_name: None }),
            enum_name: "EFoo".to_string(),
        };
        assert!(matches!(zero_value(&e), PropValue::Int(0)));
        assert!(matches!(
            zero_value(&PropertyType::Byte { enum_name: Some("EFoo".into()) }),
            PropValue::Int(0)
        ));
    }

    /// An atomic struct's zero is that many zero bytes, so decoders that read a
    /// native blob (transforms, GUIDs) still work on a defaulted value.
    #[test]
    fn zero_masked_atomic_structs_are_zero_bytes() {
        match zero_value(&PropertyType::Struct("Guid".to_string())) {
            PropValue::Native(n) => {
                assert_eq!(n, crate::iostore::object::native::NativeStruct::Guid([0; 4]))
            }
            other => panic!("expected 16 zero bytes for a zero FGuid, got {other:?}"),
        }
        match zero_value(&PropertyType::Struct("Vector".to_string())) {
            PropValue::Native(n) => {
                assert_eq!(n, crate::iostore::object::native::NativeStruct::Vec3d([0.0; 3]))
            }
            other => panic!("expected a 24-byte zero FVector, got {other:?}"),
        }
    }

    /// Types `CanSerializeAsZero` rejects should never be zero-masked; if one
    /// appears we say so with an empty span rather than inventing a value.
    #[test]
    fn non_zeroable_types_do_not_get_invented_values() {
        assert!(matches!(zero_value(&PropertyType::Str), PropValue::Raw(b) if b.is_empty()));
        assert!(matches!(
            zero_value(&PropertyType::Array(Box::new(PropertyType::Int))),
            PropValue::Raw(b) if b.is_empty()
        ));
    }
}
