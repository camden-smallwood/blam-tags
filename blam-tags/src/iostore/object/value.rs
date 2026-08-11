//! The values a decoded property block holds.
//!
//! Kept separate from the machinery that produces them so that the reader, the
//! writer and every typed decoder agree on one representation.

use std::ops::Deref;

use super::hand_written::HandWritten;
use super::native::NativeStruct;
use std::sync::Arc;

/// An `FName`: an index into the package's name map plus an instance number.
///
/// Kept as the pair the file actually stores rather than as the display string,
/// because the two are not interconvertible. `FName(base = "Rocket", number = 5)`
/// and `FName(base = "Rocket_4", number = 0)` both render `"Rocket_4"`, and
/// `break_down_name_string` has to guess between them. 137 of the 663,971
/// distinct name-map entries in the shipped corpus are of the second kind
/// (`Shield_0`, `InstancedFoliageActor_25600_1_0`, …), so a writer that
/// round-tripped through the string would silently re-split them and grow the
/// name map.
///
/// `text` is the resolved display form, carried alongside so callers that only
/// want to read a name are unaffected — hence the [`Deref`] to `str`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FName {
    pub index: u32,
    pub number: u32,
    text: String,
}

impl FName {
    pub fn new(index: u32, number: u32, text: impl Into<String>) -> Self {
        FName {
            index,
            number,
            text: text.into(),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// `NAME_None` — what a zero-masked or default-constructed name resolves to.
    pub fn none() -> Self {
        FName {
            index: 0,
            number: 0,
            text: "None".to_string(),
        }
    }
}

// Ordered by display text first, so an `FName` sorts and groups the way a
// reader expects; the index and number break ties, which keeps `Ord` consistent
// with the derived `Eq` (two names that render alike but differ in identity must
// not compare `Equal`).
impl Ord for FName {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.text, self.index, self.number).cmp(&(&other.text, other.index, other.number))
    }
}
impl PartialOrd for FName {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Deref for FName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Display for FName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}
impl PartialEq<str> for FName {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}
impl PartialEq<&str> for FName {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

/// An `FString`, with the encoding the file actually used.
///
/// `FString::operator<<` picks the encoding from the *content*: a positive
/// length means ANSI/UTF-8 bytes, a negative one means UTF-16 with a negated
/// character count. A reader that keeps only the decoded text has thrown that
/// choice away, and a writer then has to guess it back — which is what this
/// used to do, by testing `is_ascii()`.
///
/// The guess happens to be right for every string Campaign Evolved ships, which
/// is why the block round-trip was already 100%. It is right by coincidence: an
/// all-ASCII string stored as UTF-16 re-emits as UTF-8 — the same value, and
/// different bytes. Recording the flag makes it right by construction, which is
/// what a codec reading third-party mod containers needs.
///
/// An empty string has **two** encodings and they both occur: a bare zero
/// length, or a length of one followed by the terminator. Both read back as
/// empty, so normalizing to the first changed 825 exports across every
/// text-bearing class — an `FText` with an empty namespace is the common case.
/// [`FStr::empty_has_terminator`] is which.
///
/// A declared length longer than the text plus its terminator keeps the excess
/// in [`FStr::trailing`]. UE stops at the first NUL and discards the rest, so
/// the *text* is unaffected — but the bytes are real, a writer that dropped them
/// would shorten the field, and nothing in the shipped corpus exercises it, so
/// the gate would never say.
#[derive(Debug, Clone, PartialEq, Eq, Default, PartialOrd, Ord, Hash)]
pub struct FStr {
    text: String,
    /// The file stored this as UTF-16 rather than as bytes.
    pub wide: bool,
    /// Only meaningful when the text is empty: the file wrote a terminator
    /// rather than a bare zero length.
    pub empty_has_terminator: bool,
    /// Bytes after the terminator, when the declared length ran past it.
    /// Code units, so two bytes each for a wide string.
    pub trailing: Vec<u8>,
}

impl FStr {
    pub fn new(text: impl Into<String>, wide: bool) -> Self {
        let text = text.into();
        // A non-empty string always has a terminator; the flag only decides the
        // empty case, so defaulting it this way keeps an authored string in the
        // canonical form.
        FStr {
            empty_has_terminator: !text.is_empty(),
            text,
            wide,
            trailing: Vec::new(),
        }
    }

    /// As [`FStr::new`], recording how an empty string was encoded.
    pub fn with_terminator(
        text: impl Into<String>,
        wide: bool,
        empty_has_terminator: bool,
    ) -> Self {
        FStr {
            text: text.into(),
            wide,
            empty_has_terminator,
            trailing: Vec::new(),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// Replace the user-visible text while preserving the encoding choice and
    /// any trailing bytes the original stream carried.
    ///
    /// Editors must not rebuild an existing `FString` through [`FStr::new`]:
    /// doing so normalizes UTF-16 strings and drops deliberately retained
    /// bytes even when the user only changed one character.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        if !self.text.is_empty() {
            self.empty_has_terminator = true;
        }
    }
    /// Whether this must go out as UTF-16: because the file did, or because the
    /// content cannot be represented as bytes. `FString::operator<<` chooses on
    /// exactly that second condition, so an authored string still gets the
    /// encoding the engine would have picked.
    pub fn is_wide(&self) -> bool {
        self.wide || !self.text.is_ascii()
    }
}

impl Deref for FStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Display for FStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}
impl From<String> for FStr {
    fn from(text: String) -> Self {
        FStr::new(text, false)
    }
}
impl From<&str> for FStr {
    fn from(text: &str) -> Self {
        FStr::new(text, false)
    }
}
impl PartialEq<str> for FStr {
    fn eq(&self, other: &str) -> bool {
        self.text == other
    }
}
impl PartialEq<&str> for FStr {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

/// One property block: the entries it holds, in the order the file stores
/// them, plus what is needed to put the block back.
///
/// This replaces the `BTreeMap<String, PropValue>` a struct used to decode to.
/// A map answers "what is this property's value", which is all a *reader* wants,
/// but it cannot answer "what did this block look like" — it has no order, no
/// schema slots, no zero-mask bits, and it silently merges a shadowed name. All
/// four are required to emit the block byte-exactly, so the map was a lossy
/// shape standing between the reader and any writer.
///
/// Reading is unaffected: [`PropertyBlock::get`] has the same signature as
/// `BTreeMap::get`, and `&PropertyBlock` iterates as `(&str, &PropValue)`.
#[derive(Debug, Clone, Default)]
pub struct PropertyBlock {
    /// In header order, which is schema order — not name order.
    pub entries: Vec<PropertyEntry>,
    pub layout: BlockLayout,
}

/// How a block's bytes are produced again.
///
/// There is only one way now. A `Native` variant used to sit beside this one,
/// holding a retained span for structs whose `Serialize` lives in engine code —
/// the arrangement where the decoded fields were a view and the bytes were the
/// truth. Every one of those is typed in [`super::hand_written`], so the
/// scaffolding is gone.
#[derive(Debug, Clone)]
pub enum BlockLayout {
    /// A cooked unversioned property block. The header is regenerated from the
    /// entries and the class's flattened schema length — proven byte-exact over
    /// the whole shipped corpus — so nothing about it is retained.
    Unversioned {
        /// The class's flattened property count. Load-bearing even when nothing
        /// is present: `Finalize` pops trailing skips only down to one, so an
        /// empty block still encodes `min(schema_len, 127)` of them.
        schema_len: u32,
        /// Empty leading fragments, which `FUnversionedHeaderBuilder` cannot
        /// emit but Campaign Evolved's tag wrappers all carry two of.
        leading_empty: u8,
    },
}

impl Default for BlockLayout {
    fn default() -> Self {
        BlockLayout::Unversioned {
            schema_len: 0,
            leading_empty: 0,
        }
    }
}

/// One property inside a [`PropertyBlock`].
#[derive(Debug, Clone)]
pub struct PropertyEntry {
    pub name: Arc<str>,
    pub value: PropValue,
    /// Where this entry sat in the class's flattened schema, or `None` for a
    /// a block built from a bare field map, which has no schema to index into.
    pub slot: Option<SchemaSlot>,
}

/// An entry's position in the flattened schema the fragment stream indexes by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchemaSlot {
    pub index: u32,
    /// Which slot of a static array this is (`0` for a plain property). A
    /// `UPROPERTY` declared `Thing[N]` occupies N consecutive schema indices,
    /// each independently present.
    pub array_index: u8,
    /// The value serialized no bytes and is its type's zero. Kept because it is
    /// not re-derivable: `CanSerializeAsZero` decides whether a zero value *may*
    /// be masked, so a zero that was written out longhand and one that was
    /// masked are the same value and different bytes.
    pub zero_masked: bool,
}

impl PropertyBlock {
    /// The value of the named property, or `None`. Same shape as
    /// `BTreeMap::get`, which is how every existing reader keeps working.
    ///
    /// A static array's slots share one name; this returns the first, matching
    /// what the map did for the `array_dim == 1` case that is all any current
    /// caller reads. Use [`PropertyBlock::slots`] to see every slot.
    pub fn get(&self, name: &str) -> Option<&PropValue> {
        self.entries
            .iter()
            .find(|e| &*e.name == name)
            .map(|e| &e.value)
    }

    /// Every entry sharing `name`, in schema order — the static-array case.
    pub fn slots<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a PropertyEntry> {
        self.entries.iter().filter(move |e| &*e.name == name)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PropValue)> {
        self.entries.iter().map(|e| (&*e.name, &e.value))
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| &*e.name)
    }
    pub fn values(&self) -> impl Iterator<Item = &PropValue> {
        self.entries.iter().map(|e| &e.value)
    }

    /// The class's flattened schema length, for an unversioned block.
    pub fn schema_len(&self) -> Option<u32> {
        match self.layout {
            BlockLayout::Unversioned { schema_len, .. } => Some(schema_len),
        }
    }
}

/// Adopt a hand-assembled field map as a block.
///
/// Entries adopted this way have no [`SchemaSlot`], so the block is not
/// writable through the schema path. Nothing produces one any more — it remains
/// only as a convenience for building a block in tests.
impl From<std::collections::BTreeMap<String, PropValue>> for PropertyBlock {
    fn from(m: std::collections::BTreeMap<String, PropValue>) -> Self {
        PropertyBlock {
            entries: m
                .into_iter()
                .map(|(k, value)| PropertyEntry {
                    name: Arc::from(k.as_str()),
                    value,
                    slot: None,
                })
                .collect(),
            layout: BlockLayout::default(),
        }
    }
}

/// Consuming a block yields `(name, value)`, so it can be folded straight into
/// a map — which is what the hand-written decoders that flatten a nested block
/// into their own fields do.
impl IntoIterator for PropertyBlock {
    type Item = (String, PropValue);
    type IntoIter = std::vec::IntoIter<(String, PropValue)>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries
            .into_iter()
            .map(|e| (e.name.to_string(), e.value))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl<'a> IntoIterator for &'a PropertyBlock {
    type Item = (&'a str, &'a PropValue);
    type IntoIter = std::vec::IntoIter<(&'a str, &'a PropValue)>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter().collect::<Vec<_>>().into_iter()
    }
}

/// A decoded property value. Only the shapes this reader needs are modeled;
/// everything else is consumed for correct positioning and discarded.
#[derive(Debug, Clone)]
pub enum PropValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Name(FName),
    Str(FStr),
    /// An `FPackageIndex` (import if negative, export if positive).
    Object(i32),
    /// An `FSoftObjectPath`: `(PackageName, AssetName, SubPath)`.
    SoftObject(SoftObjectPath),
    Array(Vec<PropValue>),
    /// A `TSet`. Distinct from [`PropValue::Array`] because the *value* has to
    /// say which container it came from: they serialize alike but a consumer
    /// deciding whether duplicates are meaningful, or an editor offering "add
    /// element", needs to know without re-reading the schema.
    Set(Vec<PropValue>),
    /// A `TMap`, preserving insertion order.
    Map(Vec<(PropValue, PropValue)>),
    /// A `TSet`/`TMap` whose delta-serialization prefix was **not** empty.
    ///
    /// Both open with a count of entries the loader should remove before
    /// applying the ones that follow, and that count is followed by that many
    /// keys/elements. It is empty for all but **5 exports of the 1,153,836** in
    /// the shipped corpus, so this wraps the container rather than widening
    /// `Map`/`Set` — which a dozen call sites destructure positionally — and the
    /// common shape stays exactly what it was.
    ///
    /// `removals: None` is `INDEX_NONE`, "replace the container wholesale",
    /// which carries no elements. Measured: zero occurrences, but it is a
    /// different instruction from "remove nothing" and is kept distinct rather
    /// than flattened to an empty list.
    WithRemovals {
        removals: Option<Vec<PropValue>>,
        inner: Box<PropValue>,
    },
    /// A nested struct — reflected or hand-written, see [`BlockLayout`].
    Struct(PropertyBlock),
    /// A fixed-size natively-serialized struct, decoded — see [`NativeStruct`].
    Native(NativeStruct),
    /// A struct whose `Serialize` lives in engine code, decoded into typed
    /// fields and written back from them — see [`HandWritten`].
    HandWritten(HandWritten),
    /// `FScriptDelegate`: the bound object and the function it names.
    Delegate {
        object: i32,
        function: FName,
    },
    /// `FMulticastScriptDelegate`: the invocation list.
    MulticastDelegate(Vec<(i32, FName)>),
    /// `FFieldPath`: the property path and the object it is rooted at.
    FieldPath {
        path: Vec<FName>,
        owner: i32,
    },
    /// A `TOptional` that is not set. Distinct from a set-but-empty value, and
    /// from [`PropValue::Raw`] — no bytes follow the four-byte "is set" flag.
    Unset,
    /// Bytes consumed but deliberately not modeled.
    ///
    /// There is no "we saw something and dropped it" case: anything this reader
    /// declines to interpret keeps the exact bytes it consumed, so a writer can
    /// put them back and a round trip stays byte-exact. Replaces the old
    /// `Opaque`, which lost 17,611 values across the shipped corpus.
    Raw(Vec<u8>),
}

impl PropValue {
    /// Visit every [`FName`] this value carries, recursively — see
    /// [`PropertyBlock::visit_names_mut`].
    ///
    /// The match is exhaustive on purpose. A missed name is invisible: the file
    /// still round-trips, because the index is what is written, and the damage
    /// only appears later when someone edits the stale field and forks a name.
    /// Listing the name-free variants explicitly makes a new one a compile
    /// error instead.
    pub fn visit_names_mut(&mut self, f: &mut dyn FnMut(&mut FName)) {
        match self {
            PropValue::Name(name) => f(name),
            PropValue::SoftObject(path) => {
                f(&mut path.package);
                f(&mut path.asset);
            }
            PropValue::Array(values) | PropValue::Set(values) => {
                for value in values {
                    value.visit_names_mut(f);
                }
            }
            PropValue::Map(pairs) => {
                for (key, value) in pairs {
                    key.visit_names_mut(f);
                    value.visit_names_mut(f);
                }
            }
            PropValue::WithRemovals { removals, inner } => {
                if let Some(removals) = removals {
                    for value in removals {
                        value.visit_names_mut(f);
                    }
                }
                inner.visit_names_mut(f);
            }
            PropValue::Struct(block) => block.visit_names_mut(f),
            PropValue::HandWritten(value) => value.visit_names_mut(f),
            PropValue::Delegate { function, .. } => f(function),
            PropValue::MulticastDelegate(bindings) => {
                for (_, function) in bindings {
                    f(function);
                }
            }
            PropValue::FieldPath { path, .. } => {
                for segment in path {
                    f(segment);
                }
            }
            // No name map reference. `Native` is listed here on evidence: every
            // `NativeStruct` variant is numeric, and the module names no `FName`
            // at all.
            PropValue::Bool(_)
            | PropValue::Int(_)
            | PropValue::Float(_)
            | PropValue::Str(_)
            | PropValue::Object(_)
            | PropValue::Native(_)
            | PropValue::Unset
            | PropValue::Raw(_) => {}
        }
    }
}

impl PropertyBlock {
    /// Visit every [`FName`] reachable from this block, in place.
    ///
    /// An `FName` serializes as its index and number, so renaming a name-map
    /// entry already retargets every reference to it on disk. What it does *not*
    /// do is update the resolved `text` a decoded value carries — and a stale
    /// one is not merely cosmetic, because interning is by string: editing that
    /// field afterwards would fork a fresh entry rather than follow the rename.
    /// This is how a caller refreshes them.
    ///
    /// Property *names* are not visited. A cooked block is
    /// [`BlockLayout::Unversioned`], so its names come from the class's schema
    /// rather than from the package's name map.
    pub fn visit_names_mut(&mut self, f: &mut dyn FnMut(&mut FName)) {
        for entry in &mut self.entries {
            entry.value.visit_names_mut(f);
        }
    }

    /// Equality by *value*, for the round-trip contract
    /// `decode(encode(decode(x))) == decode(x)`.
    ///
    /// Not `PartialEq`: floats are compared by their **bits**, because a round
    /// trip has to give back the number that was there — `-0.0` and `0.0` are
    /// different values here, and two `NaN`s with the same payload are the same
    /// value. Derived equality gets both of those backwards.
    pub fn semantic_eq(&self, other: &PropertyBlock) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        if !self.layout.semantic_eq(&other.layout) {
            return false;
        }
        self.entries
            .iter()
            .zip(&other.entries)
            .all(|(a, b)| a.name == b.name && a.slot == b.slot && a.value.semantic_eq(&b.value))
    }
}

impl BlockLayout {
    pub fn semantic_eq(&self, other: &BlockLayout) -> bool {
        match (self, other) {
            (
                BlockLayout::Unversioned {
                    schema_len: a,
                    leading_empty: b,
                },
                BlockLayout::Unversioned {
                    schema_len: c,
                    leading_empty: d,
                },
            ) => a == c && b == d,
        }
    }
}

impl PropValue {
    /// Look through a [`PropValue::WithRemovals`] wrapper to the container
    /// itself.
    ///
    /// Every accessor below goes through this, so a reader never has to know
    /// that a container carried a removal prefix — which is the point of
    /// wrapping rather than widening: the 5 exports that have one read exactly
    /// like the 1,153,831 that do not.
    pub fn unwrapped(&self) -> &PropValue {
        match self {
            PropValue::WithRemovals { inner, .. } => inner.unwrapped(),
            other => other,
        }
    }

    /// See [`PropertyBlock::semantic_eq`].
    pub fn semantic_eq(&self, other: &PropValue) -> bool {
        use PropValue::*;
        match (self, other) {
            (Bool(a), Bool(b)) => a == b,
            (Int(a), Int(b)) => a == b,
            // By bits: a round trip must return the same number, and `-0.0` is
            // not `0.0` on the wire.
            (Float(a), Float(b)) => a.to_bits() == b.to_bits(),
            (Name(a), Name(b)) => a == b,
            (Str(a), Str(b)) => a == b && a.wide == b.wide,
            (Object(a), Object(b)) => a == b,
            (SoftObject(a), SoftObject(b)) => {
                a.package == b.package && a.asset == b.asset && a.sub_path == b.sub_path
            }
            (Array(a), Array(b)) | (Set(a), Set(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.semantic_eq(y))
            }
            (Map(a), Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ak, av), (bk, bv))| ak.semantic_eq(bk) && av.semantic_eq(bv))
            }
            (
                WithRemovals {
                    removals: ar,
                    inner: ai,
                },
                WithRemovals {
                    removals: br,
                    inner: bi,
                },
            ) => {
                let removals_eq = match (ar, br) {
                    (Some(x), Some(y)) => {
                        x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.semantic_eq(q))
                    }
                    (None, None) => true,
                    _ => false,
                };
                removals_eq && ai.semantic_eq(bi)
            }
            (Struct(a), Struct(b)) => a.semantic_eq(b),
            (Native(a), Native(b)) => a.semantic_eq(b),
            (HandWritten(a), HandWritten(b)) => a.semantic_eq(b),
            (
                Delegate {
                    object: ao,
                    function: af,
                },
                Delegate {
                    object: bo,
                    function: bf,
                },
            ) => ao == bo && af == bf,
            (MulticastDelegate(a), MulticastDelegate(b)) => a == b,
            (
                FieldPath {
                    path: ap,
                    owner: ao,
                },
                FieldPath {
                    path: bp,
                    owner: bo,
                },
            ) => ap == bp && ao == bo,
            (Unset, Unset) => true,
            (Raw(a), Raw(b)) => a == b,
            _ => false,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Name(n) => Some(n.as_str()),
            PropValue::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(PropValue, PropValue)]> {
        match self.unwrapped() {
            PropValue::Map(m) => Some(m),
            _ => None,
        }
    }
    /// A `TArray`'s elements. A `TSet` is *not* an array — ask [`Self::as_set`].
    pub fn as_array(&self) -> Option<&[PropValue]> {
        match self.unwrapped() {
            PropValue::Array(a) => Some(a),
            _ => None,
        }
    }
    /// A `TSet`'s elements.
    pub fn as_set(&self) -> Option<&[PropValue]> {
        match self.unwrapped() {
            PropValue::Set(a) => Some(a),
            _ => None,
        }
    }
    /// The elements of either a `TArray` or a `TSet`, for callers that only
    /// care that it is a sequence.
    pub fn as_sequence(&self) -> Option<&[PropValue]> {
        match self.unwrapped() {
            PropValue::Array(a) | PropValue::Set(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_struct(&self) -> Option<&PropertyBlock> {
        match self {
            PropValue::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_soft_object(&self) -> Option<&SoftObjectPath> {
        match self {
            PropValue::SoftObject(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_native(&self) -> Option<&NativeStruct> {
        match self {
            PropValue::Native(n) => Some(n),
            _ => None,
        }
    }
}

/// A component-relative transform (`FTransform`) attached to a bone: UE5
/// large-world-coordinate `double`s, so `FQuat`=4×f64 and `FVector`=3×f64.
#[derive(Debug, Clone, Copy)]
pub struct MeshTransform {
    /// `(x, y, z, w)` quaternion.
    pub rotation: [f32; 4],
    pub translation: [f32; 3],
    pub scale: [f32; 3],
}

impl Default for MeshTransform {
    fn default() -> Self {
        Self {
            rotation: [0.0, 0.0, 0.0, 1.0],
            translation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl MeshTransform {
    pub fn is_identity(&self) -> bool {
        self.translation == [0.0; 3]
            && self.rotation == [0.0, 0.0, 0.0, 1.0]
            && self.scale == [1.0; 3]
    }

    /// Decode from a reflected `FTransform` struct value (`Rotation`/
    /// `Translation`/`Scale3D` as native `FQuat`/`FVector` blobs).
    /// Decode from a reflected `FTransform` struct value.
    ///
    /// Its three fields are fixed-size native structs, so this is now field
    /// access rather than the hand-rolled byte arithmetic it used to be.
    pub(crate) fn from_prop(v: &PropValue) -> Option<MeshTransform> {
        let s = v.as_struct()?;
        let mut t = MeshTransform::default();
        if let Some(NativeStruct::Vec4d(r)) = s.get("Rotation").and_then(PropValue::as_native) {
            t.rotation = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
        }
        if let Some(NativeStruct::Vec3d(v)) = s.get("Translation").and_then(PropValue::as_native) {
            t.translation = [v[0] as f32, v[1] as f32, v[2] as f32];
        }
        if let Some(NativeStruct::Vec3d(v)) = s.get("Scale3D").and_then(PropValue::as_native) {
            t.scale = [v[0] as f32, v[1] as f32, v[2] as f32];
        }
        Some(t)
    }
}

/// An `FSoftObjectPath` — a `TopLevelAssetPath` plus optional sub-path.
#[derive(Debug, Clone, Default)]
pub struct SoftObjectPath {
    /// Full package name, e.g. `/Game/Characters/Marine/.../SK_Marine_Torso_01`.
    pub package: FName,
    /// Object name within the package, e.g. `SK_Marine_Torso_01`.
    pub asset: FName,
    /// Read through `fstring`, so it carries its encoding like any other
    /// `FString` — see [`FStr`].
    pub sub_path: FStr,
}

impl SoftObjectPath {
    pub fn is_empty(&self) -> bool {
        self.package.as_str().is_empty() && self.asset.as_str().is_empty()
    }
}

#[cfg(test)]
mod visit_names_tests {
    use super::*;
    use crate::iostore::object::hand_written::{HandWritten, LocatorFragment};

    fn block(values: Vec<PropValue>) -> PropertyBlock {
        PropertyBlock {
            entries: values
                .into_iter()
                .enumerate()
                .map(|(i, value)| PropertyEntry {
                    name: format!("Field{i}").into(),
                    value,
                    slot: None,
                })
                .collect(),
            layout: BlockLayout::Unversioned {
                schema_len: 0,
                leading_empty: 0,
            },
        }
    }

    fn name(text: &str) -> FName {
        FName::new(0, 0, text)
    }

    fn seen(block: &mut PropertyBlock) -> Vec<String> {
        let mut found = Vec::new();
        block.visit_names_mut(&mut |name| found.push(name.to_string()));
        found.sort();
        found
    }

    /// Every nesting shape a name can hide behind, in one block. A variant that
    /// stops being visited is invisible on disk — the index is what is written —
    /// and only shows up later as a forked name-map entry.
    #[test]
    fn every_nested_shape_is_reached() {
        let mut subject = block(vec![
            PropValue::Name(name("Direct")),
            PropValue::SoftObject(SoftObjectPath {
                package: name("SoftPackage"),
                asset: name("SoftAsset"),
                sub_path: FStr::from("sub"),
            }),
            PropValue::Array(vec![PropValue::Name(name("InArray"))]),
            PropValue::Set(vec![PropValue::Name(name("InSet"))]),
            PropValue::Map(vec![(
                PropValue::Name(name("MapKey")),
                PropValue::Name(name("MapValue")),
            )]),
            PropValue::WithRemovals {
                removals: Some(vec![PropValue::Name(name("Removed"))]),
                inner: Box::new(PropValue::Name(name("Kept"))),
            },
            PropValue::Struct(block(vec![PropValue::Name(name("InStruct"))])),
            PropValue::Delegate {
                object: 0,
                function: name("DelegateFn"),
            },
            PropValue::MulticastDelegate(vec![(0, name("MulticastFn"))]),
            PropValue::FieldPath {
                path: vec![name("PathSegment")],
                owner: 0,
            },
            PropValue::HandWritten(HandWritten::LocatorFragment(LocatorFragment {
                fragment_type: name("FragmentType"),
                payload: Some(block(vec![PropValue::Name(name("InFragment"))])),
            })),
        ]);

        assert_eq!(
            seen(&mut subject),
            [
                "DelegateFn",
                "Direct",
                "FragmentType",
                "InArray",
                "InFragment",
                "InSet",
                "InStruct",
                "Kept",
                "MapKey",
                "MapValue",
                "MulticastFn",
                "PathSegment",
                "Removed",
                "SoftAsset",
                "SoftPackage",
            ]
        );
    }

    /// The visitor mutates in place, which is the whole reason it exists: a
    /// rename retargets the index on disk but leaves the resolved text stale,
    /// and interning is by string.
    #[test]
    fn the_visitor_can_rewrite_the_resolved_text() {
        let mut subject = block(vec![
            PropValue::Name(FName::new(4, 0, "Warthog")),
            PropValue::Array(vec![PropValue::Name(FName::new(4, 3, "Warthog_2"))]),
            PropValue::Name(FName::new(9, 0, "Untouched")),
        ]);

        subject.visit_names_mut(&mut |name| {
            if name.index == 4 {
                let text = if name.number != 0 {
                    format!("Scorpion_{}", name.number - 1)
                } else {
                    "Scorpion".to_owned()
                };
                *name = FName::new(name.index, name.number, text);
            }
        });

        assert_eq!(seen(&mut subject), ["Scorpion", "Scorpion_2", "Untouched"]);
    }

    /// Property names come from the class schema, not the package name map, so
    /// renaming a name-map entry must not touch them.
    #[test]
    fn property_names_are_not_name_map_references() {
        let mut subject = block(vec![PropValue::Int(1)]);
        assert!(seen(&mut subject).is_empty());
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// The display string is not a faithful identity. `base "Rocket" + number 5`
    /// and `base "Rocket_4" + number 0` both render `"Rocket_4"`, so a writer
    /// that round-tripped through the string would pick whichever
    /// `break_down_name_string` guessed. 137 name-map entries in the shipped
    /// corpus are of the second kind, which is why the pair is kept.
    #[test]
    fn same_text_can_mean_two_different_names() {
        let via_number = FName::new(12, 5, "Rocket_4");
        let literal = FName::new(99, 0, "Rocket_4");
        assert_eq!(
            via_number.as_str(),
            literal.as_str(),
            "they render identically"
        );
        assert_ne!(via_number, literal, "but they are not the same name");
    }

    /// Existing callers that only read a name keep working through `Deref`.
    #[test]
    fn reads_like_a_string() {
        let n = FName::new(3, 0, "Shield_0");
        assert_eq!(&*n, "Shield_0");
        assert!(n.starts_with("Shield"));
        assert_eq!(n.rsplit('_').next(), Some("0"));
        assert_eq!(n, "Shield_0");
        assert_eq!(format!("{n}"), "Shield_0");
        assert_eq!(PropValue::Name(n).as_str(), Some("Shield_0"));
    }

    #[test]
    fn none_is_index_zero() {
        let n = FName::none();
        assert_eq!((n.index, n.number), (0, 0));
        assert_eq!(n.as_str(), "None");
    }
}
