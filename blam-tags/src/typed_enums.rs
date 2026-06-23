//! Typed, schema-name-resolved enum & flags wrappers.
//!
//! # Why this exists
//!
//! Tag enum/flags fields store a raw integer. Historically the walkers
//! decoded that integer *positionally* (`from_int(2) => Water`), which
//! interprets the wire value against ONE fixed ordering and ignores the
//! schema embedded in the tag. Halo's option/flag lists were reordered
//! across development, so the same wire value can mean different options
//! in tags authored under different schema revisions — a silent
//! mis-decode (the `cook torrance` / riverworld cube-validation class of
//! bug).
//!
//! The fix: resolve by NAME. The field decoder already resolves the
//! embedded option/bit names ([`TagFieldData::CharEnum`] etc. carry
//! `name` / `names`). We map those names onto a canonical Rust enum `T`
//! whose variants + discriminants come from the authoritative
//! `definitions/halo3_mcc/*.json` (`enums_flags`). The stored value is
//! re-expressed in `T`'s canonical bit order, so consumers compare
//! against typed variants and are immune to wire drift.
//!
//! # Authoring a `T`
//!
//! `T` is a fieldless enum. Variant discriminants are the canonical
//! bit/option indices from the JSON `options` list. Per-variant
//! `#[strum(serialize = "...")]` carries the exact schema name. All
//! behaviour is derived — no hand-written methods:
//!
//! ```ignore
//! #[derive(Clone, Copy, PartialEq, Eq, Debug,
//!          num_derive::FromPrimitive, num_derive::ToPrimitive,
//!          strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
//! #[strum(ascii_case_insensitive)]
//! #[repr(i8)]
//! pub enum BeamProfileShape {
//!     #[strum(serialize = "aligned ribbon")] AlignedRibbon = 0,
//!     #[strum(serialize = "cross")]          Cross = 1,
//!     #[strum(serialize = "n-gon")]          NGon = 2,
//! }
//! ```
//!
//! Single-choice enum type names must NOT end in `Enum` — the schema
//! strings (`beam_profile_shape_enum`) are definition *variable* names,
//! not type names. A `Flags` suffix on flag types is fine and avoids
//! colliding with the matching definition struct (e.g.
//! `light_definition_flags` -> `LightDefinitionFlags`, distinct from
//! `LightDefinition`).

use std::fmt;
use std::marker::PhantomData;

use num_traits::{FromPrimitive, ToPrimitive};
use strum::VariantArray;

// ---------------------------------------------------------------------------
// SchemaEnum — blanket-implemented for any T built from the derives above.
// ---------------------------------------------------------------------------

/// A fieldless enum whose variants map to schema option/bit names and to
/// canonical integer indices. Blanket-implemented; you never write this
/// by hand — deriving `FromPrimitive`, `ToPrimitive`, `EnumString`,
/// `IntoStaticStr`, and `VariantArray` is enough.
pub trait SchemaEnum: Copy + 'static + fmt::Debug {
    /// Resolve a schema option/bit name to a variant. Tries an exact
    /// (case-insensitive) match first, then a normalized match that
    /// folds away spaces / dashes / underscores / case so cosmetic
    /// embedded-vs-JSON spelling differences still resolve.
    fn from_schema_name(name: &str) -> Option<Self>;
    /// The canonical schema name of this variant.
    fn schema_name(self) -> &'static str;
    /// Canonical bit/option index (the variant discriminant).
    fn to_index(self) -> u32;
    /// Variant for a canonical bit/option index, if any.
    fn from_index(index: u32) -> Option<Self>;
    /// All variants (for diagnostics).
    fn variants() -> &'static [Self];
}

/// Schema-name special characters, per `Bungie.Tags.TagFieldNameInfo`.
/// The canonical field name is the substring BEFORE the first of these:
/// `#` description, `&` display-name, `:` units, `!` hidden,
/// `*` read-only, `^` block-element label. (`/` is path-normalized to
/// `\` in the canonical name but isn't a terminator; it folds away as
/// non-alphanumeric regardless.)
const NAME_SPECIALS: [char; 6] = ['#', '&', ':', '!', '*', '^'];

/// Fold a schema name to a comparison key.
///
/// Mirrors `TagFieldNameInfo`: take the text before the first special
/// character (the metadata after it — tooltip / display name / units —
/// is UI-only, carried in the source schema → our JSON, but NOT in tag
/// files), then drop everything non-alphanumeric and lowercase so
/// cosmetic spacing/dash/underscore/case differences also fold away.
/// `"allow shadows and gels#CPU lights..."`, `"shader flags*"`,
/// `"type^"`, `"N-Gon"` fold to `"allowshadowsandgels"`,
/// `"shaderflags"`, `"type"`, `"ngon"`.
fn fold(name: &str) -> String {
    // Strip Bungie field-name markers first, then any `{alternate name}`
    // annotation. Enum option strings in the source schema list old /
    // display alternates after the canonical name as `primary{alternate}`
    // (e.g. `system age{emitter age}`); only the primary is canonical, and
    // a tag may embed either form, so cut at the first `{` on both sides.
    let base = name.split(NAME_SPECIALS).next().unwrap_or(name);
    let base = base.split('{').next().unwrap_or(base);
    base.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl<T> SchemaEnum for T
where
    T: Copy
        + 'static
        + fmt::Debug
        + std::str::FromStr
        + Into<&'static str>
        + ToPrimitive
        + FromPrimitive
        + VariantArray,
{
    fn from_schema_name(name: &str) -> Option<Self> {
        if let Ok(v) = name.parse::<T>() {
            return Some(v);
        }
        let key = fold(name);
        T::VARIANTS
            .iter()
            .copied()
            .find(|v| fold((*v).into()) == key)
    }

    fn schema_name(self) -> &'static str {
        self.into()
    }

    fn to_index(self) -> u32 {
        // Discriminants are small non-negative bit/option indices.
        self.to_u32()
            .expect("SchemaEnum discriminant must fit in u32")
    }

    fn from_index(index: u32) -> Option<Self> {
        T::from_u32(index)
    }

    fn variants() -> &'static [Self] {
        T::VARIANTS
    }
}

// ---------------------------------------------------------------------------
// TagInt — the storage integer widths a tag enum/flags field can use.
// ---------------------------------------------------------------------------

/// The raw storage integer type backing an enum/flags field
/// (`U` in `Enum<T, U>` / `Flags<T, U>`).
pub trait TagInt: Copy + 'static + fmt::Debug + Eq {
    fn from_i128(v: i128) -> Self;
    fn to_u64(self) -> u64;
    /// Width in bits (8/16/32) — bounds the flag-bit scan.
    const BITS: u32;
    const ZERO: Self;
}

macro_rules! impl_tag_int {
    ($($t:ty),*) => {$(
        impl TagInt for $t {
            #[inline] fn from_i128(v: i128) -> Self { v as $t }
            #[inline] fn to_u64(self) -> u64 { self as u64 }
            const BITS: u32 = <$t>::BITS;
            const ZERO: Self = 0;
        }
    )*};
}
impl_tag_int!(i8, i16, i32, u8, u16, u32);

// ---------------------------------------------------------------------------
// Enum<T, U> — a single-choice enum field.
// ---------------------------------------------------------------------------

/// A decoded single-choice enum field: the resolved canonical variant
/// `T`, plus the raw storage value `U` as authored.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Enum<T, U> {
    variant: T,
    raw: U,
}

impl<T: SchemaEnum, U: TagInt> Enum<T, U> {
    /// The resolved canonical variant.
    #[inline]
    pub fn get(self) -> T {
        self.variant
    }
    /// Set the variant (e.g. for synthesized / overridden values).
    #[inline]
    pub fn set(&mut self, variant: T) {
        self.variant = variant;
        self.raw = U::from_i128(variant.to_index() as i128);
    }
    /// The canonical schema name of the resolved variant.
    #[inline]
    pub fn name(self) -> &'static str {
        self.variant.schema_name()
    }

    /// Build directly from a variant (for synthesized/overridden values).
    pub fn from_variant(variant: T) -> Self {
        Enum {
            variant,
            raw: U::from_i128(variant.to_index() as i128),
        }
    }

    /// The raw storage value as authored in the tag. `pub(crate)` —
    /// callers operate on the typed variant, not the integer.
    #[inline]
    pub(crate) fn raw(self) -> U {
        self.raw
    }

    /// Resolve a raw value + its embedded name into a typed `Enum`.
    /// `name` is the embedded schema option name (`None` if the decoder
    /// couldn't resolve it). Panics on an unresolved value — a tag whose
    /// enum value has no embedded name, or a name with no matching `T`
    /// variant, is a real decode error we want surfaced, not buried.
    pub(crate) fn resolve(field: &str, raw: U, name: Option<&str>) -> Self {
        let variant = match name {
            Some(n) => T::from_schema_name(n).unwrap_or_else(|| {
                panic!(
                    "enum field {field:?}: schema name {n:?} (raw {raw:?}) \
                     has no matching {} variant; known: {:?}",
                    std::any::type_name::<T>(),
                    T::variants()
                        .iter()
                        .map(|v| v.schema_name())
                        .collect::<Vec<_>>(),
                )
            }),
            None => panic!(
                "enum field {field:?}: raw value {raw:?} has no embedded \
                 schema name (out of range for the tag's option list); \
                 cannot resolve to {}",
                std::any::type_name::<T>()
            ),
        };
        Enum { variant, raw }
    }
}

impl<T: SchemaEnum, U: TagInt> fmt::Debug for Enum<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.variant)
    }
}

/// `Display` = the resolved variant's canonical schema name (the by-name
/// representation — no raw bits). Lets diagnostics print enum fields
/// directly with `{}`.
impl<T: SchemaEnum, U: TagInt> fmt::Display for Enum<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.variant.schema_name())
    }
}

/// `Default` for explicit fallback construction only — NOT a decode
/// path (decode always goes through [`Enum::resolve`], which panics on
/// an unresolved value). Requires `T: Default` (mark a `#[default]`
/// variant). Lets walker structs keep `#[derive(Default)]`.
impl<T: SchemaEnum + Default, U: TagInt> Default for Enum<T, U> {
    fn default() -> Self {
        Enum::from_variant(T::default())
    }
}

impl<T: SchemaEnum, U: TagInt> PartialEq<T> for Enum<T, U>
where
    T: PartialEq,
{
    fn eq(&self, other: &T) -> bool {
        self.variant == *other
    }
}

// ---------------------------------------------------------------------------
// Flags<T, U> — a bitfield. Stored as the resolved set of canonical
// variants plus the raw authored value (so unknown/over-range bits are
// preserved for round-tripping).
// ---------------------------------------------------------------------------

/// A decoded flags field: the set of canonical variants whose bits are
/// set, re-expressed in `T`'s canonical bit order. The raw authored
/// value is retained for round-tripping / diagnostics.
#[derive(Clone)]
pub struct Flags<T, U> {
    set: Vec<T>,
    raw: U,
    _u: PhantomData<U>,
}

impl<T: SchemaEnum + PartialEq, U: TagInt> Flags<T, U> {
    /// The set flags, in canonical order.
    pub fn get(&self) -> Vec<T> {
        self.set.clone()
    }
    /// Is `flag` set?
    #[inline]
    pub fn contains(&self, flag: T) -> bool {
        self.set.iter().any(|f| *f == flag)
    }
    /// Are ALL of `flags` set?
    pub fn test(&self, flags: &[T]) -> bool {
        flags.iter().all(|f| self.contains(*f))
    }
    /// Are ANY of `flags` set?
    pub fn test_any(&self, flags: &[T]) -> bool {
        flags.iter().any(|f| self.contains(*f))
    }
    /// Set or clear a flag.
    pub fn set(&mut self, flag: T, on: bool) {
        let present = self.contains(flag);
        if on && !present {
            self.set.push(flag);
            self.set.sort_by_key(|f| f.to_index());
        } else if !on && present {
            self.set.retain(|f| *f != flag);
        }
    }
    /// Build directly from a set of flags (synthesized values).
    pub fn from_slice(flags: &[T]) -> Self {
        let mut f = Self::default();
        for &flag in flags {
            f.set(flag, true);
        }
        f.raw = U::from_i128(f.canonical_bits() as i128);
        f
    }
    /// Iterate the set flags (canonical order).
    pub fn iter(&self) -> impl Iterator<Item = T> + '_ {
        self.set.iter().copied()
    }
    /// Schema names of the set flags.
    pub fn names(&self) -> Vec<&'static str> {
        self.set.iter().map(|f| f.schema_name()).collect()
    }
    /// No flags set.
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// The raw authored value. `pub(crate)` — callers operate on the
    /// typed flag set, not the integer.
    #[inline]
    pub(crate) fn raw(&self) -> U {
        self.raw
    }
    /// Canonical bit pattern (bits at `T` discriminants). `pub(crate)` —
    /// internal/serialization use only.
    pub(crate) fn canonical_bits(&self) -> u64 {
        self.set.iter().fold(0u64, |acc, f| acc | (1u64 << f.to_index()))
    }

    /// Resolve a raw value + its embedded set-bit names into a typed
    /// `Flags`. `names` are the embedded `(bit, name)` pairs of the SET
    /// bits (from the decoder). Panics if any set, named bit fails to
    /// map to a `T` variant. A set bit with NO embedded name (past the
    /// schema's string list) is preserved in `raw` but cannot be typed —
    /// that is tolerated (runtime/over-range bits), unlike enums.
    pub(crate) fn resolve(_field: &str, raw: U, names: &[(u32, String)]) -> Self {
        // Tolerate set bits whose schema name has no matching variant — newer
        // or runtime-only flags past this walker's enum. They stay in `raw`
        // (nothing lost) but can't be typed; skip them rather than panicking,
        // mirroring how unnamed over-range bits are already handled.
        let set = names
            .iter()
            .filter_map(|(_bit, n)| T::from_schema_name(n))
            .collect();
        Flags {
            set,
            raw,
            _u: PhantomData,
        }
    }
}

impl<T: SchemaEnum + PartialEq, U: TagInt> Default for Flags<T, U> {
    fn default() -> Self {
        Flags {
            set: Vec::new(),
            raw: U::ZERO,
            _u: PhantomData,
        }
    }
}

impl<T: SchemaEnum + PartialEq, U: TagInt> PartialEq for Flags<T, U> {
    fn eq(&self, other: &Self) -> bool {
        self.canonical_bits() == other.canonical_bits()
    }
}

impl<T: SchemaEnum + PartialEq, U: TagInt> fmt::Debug for Flags<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.set.iter()).finish()
    }
}

/// Hex display forwards to the retained raw authored value — for the
/// round-tripping / diagnostics use the struct doc-comment calls out
/// (`Debug` already prints the resolved variant names). Lets dump tools
/// keep `{:x}` / `{:#x}` on a flags field.
impl<T, U: fmt::LowerHex> fmt::LowerHex for Flags<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.raw, f)
    }
}
impl<T, U: fmt::UpperHex> fmt::UpperHex for Flags<T, U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::UpperHex::fmt(&self.raw, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Debug,
             num_derive::FromPrimitive, num_derive::ToPrimitive,
             strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
    #[strum(ascii_case_insensitive)]
    #[repr(i8)]
    enum BeamProfileShape {
        #[strum(serialize = "aligned ribbon")] AlignedRibbon = 0,
        #[strum(serialize = "cross")]          Cross = 1,
        #[strum(serialize = "n-gon")]          NGon = 2,
    }

    #[derive(Clone, Copy, PartialEq, Eq, Debug,
             num_derive::FromPrimitive, num_derive::ToPrimitive,
             strum::EnumString, strum::IntoStaticStr, strum::VariantArray)]
    #[strum(ascii_case_insensitive)]
    #[repr(u16)]
    enum ContentFlags {
        #[strum(serialize = "double-sided")] DoubleSided = 0,
        #[strum(serialize = "additive")]     Additive = 1,
        #[strum(serialize = "subtractive")]  Subtractive = 2,
    }

    #[test]
    fn enum_resolves_by_name_and_normalized() {
        // exact
        assert_eq!(BeamProfileShape::from_schema_name("cross"), Some(BeamProfileShape::Cross));
        // case-insensitive (strum) + dash/space/underscore folding (our fold)
        assert_eq!(BeamProfileShape::from_schema_name("N-Gon"), Some(BeamProfileShape::NGon));
        assert_eq!(BeamProfileShape::from_schema_name("n gon"), Some(BeamProfileShape::NGon));
        assert_eq!(BeamProfileShape::from_schema_name("aligned_ribbon"), Some(BeamProfileShape::AlignedRibbon));
        // schema markers (#desc &display :units ! * ^) are stripped before matching
        assert_eq!(ContentFlags::from_schema_name("double-sided#renders both faces"), Some(ContentFlags::DoubleSided));
        assert_eq!(ContentFlags::from_schema_name("additive*"), Some(ContentFlags::Additive));
        assert_eq!(ContentFlags::from_schema_name("subtractive^"), Some(ContentFlags::Subtractive));
        // `{alternate name}` annotations are stripped before matching, so a
        // tag embedding either the primary or the full `primary{alt}` form
        // resolves to the same variant.
        assert_eq!(ContentFlags::from_schema_name("additive{old additive}"), Some(ContentFlags::Additive));
        assert_eq!(BeamProfileShape::from_schema_name("bogus"), None);
        // index <-> variant
        assert_eq!(BeamProfileShape::Cross.to_index(), 1);
        assert_eq!(<BeamProfileShape as SchemaEnum>::from_index(2), Some(BeamProfileShape::NGon));
    }

    #[test]
    fn enum_wrapper_resolves_and_keeps_raw() {
        let e: Enum<BeamProfileShape, i8> =
            Enum::resolve("profile shape", 2, Some("n-gon"));
        assert_eq!(e.get(), BeamProfileShape::NGon);
        assert_eq!(e.raw(), 2i8);
        assert_eq!(e.name(), "n-gon");
        assert!(e == BeamProfileShape::NGon);
    }

    #[test]
    #[should_panic(expected = "no matching")]
    fn enum_panics_on_unknown_name() {
        let _: Enum<BeamProfileShape, i8> = Enum::resolve("profile shape", 9, Some("zorp"));
    }

    #[test]
    fn flags_resolve_by_name_and_repack() {
        // embedded order: say the tag put "additive" at bit 5, "double-sided" at bit 0.
        let names = vec![(0u32, "double-sided".to_string()), (5u32, "additive".to_string())];
        let f: Flags<ContentFlags, u16> = Flags::resolve("flags", 0b100001, &names);
        // typed queries — no raw integer in sight
        assert_eq!(f.get(), vec![ContentFlags::DoubleSided, ContentFlags::Additive]);
        assert!(f.contains(ContentFlags::DoubleSided));
        assert!(f.test(&[ContentFlags::DoubleSided, ContentFlags::Additive]));
        assert!(!f.test(&[ContentFlags::DoubleSided, ContentFlags::Subtractive]));
        assert!(f.test_any(&[ContentFlags::Subtractive, ContentFlags::Additive]));
        assert!(!f.test_any(&[ContentFlags::Subtractive]));
        assert_eq!(f.names(), vec!["double-sided", "additive"]);
        // canonical re-pack uses T discriminants (0 and 1), NOT the embedded bit 5.
        assert_eq!(f.canonical_bits(), 0b11);
        assert_eq!(f.raw(), 0b100001u16); // raw preserves the authored pattern
    }

    #[test]
    fn flags_and_enum_mutation() {
        let mut f: Flags<ContentFlags, u16> = Flags::from_slice(&[ContentFlags::Additive]);
        assert!(f.contains(ContentFlags::Additive));
        f.set(ContentFlags::Subtractive, true);
        f.set(ContentFlags::Additive, false);
        assert_eq!(f.get(), vec![ContentFlags::Subtractive]);
        assert_eq!(f.canonical_bits(), 0b100);

        let mut e: Enum<BeamProfileShape, i8> = Enum::from_variant(BeamProfileShape::Cross);
        assert_eq!(e.get(), BeamProfileShape::Cross);
        e.set(BeamProfileShape::NGon);
        assert_eq!(e.get(), BeamProfileShape::NGon);
        assert_eq!(e.name(), "n-gon");
    }

    #[test]
    fn flags_empty_is_default() {
        let f: Flags<ContentFlags, u16> = Flags::resolve("flags", 0, &[]);
        assert!(f.is_empty());
        assert_eq!(f, Flags::<ContentFlags, u16>::default());
    }
}
