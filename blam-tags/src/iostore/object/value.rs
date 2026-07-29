//! The values a decoded property block holds.
//!
//! Kept separate from the machinery that produces them so that the reader, the
//! writer and every typed decoder agree on one representation.

use std::ops::Deref;
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
        FName { index, number, text: text.into() }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// `NAME_None` — what a zero-masked or default-constructed name resolves to.
    pub fn none() -> Self {
        FName { index: 0, number: 0, text: "None".to_string() }
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
/// The distinction is load-bearing and used to be invisible: `PropValue::Struct`
/// meant *either* a cooked unversioned block *or* a map some hand-written
/// decoder assembled, and those go back to bytes by completely different rules.
/// Conflating them is why writing any struct at all had to be refused.
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
    /// A struct with a hand-written `Serialize`, whose layout lives in code
    /// rather than in a schema. The decoded fields are for readers; the bytes
    /// are what goes back out, so the round trip is exact before the layout is
    /// modeled. Converting one to a real writer is then verifiable against the
    /// span it replaces.
    Native { name: Arc<str>, bytes: Vec<u8> },
}

impl Default for BlockLayout {
    fn default() -> Self {
        BlockLayout::Unversioned { schema_len: 0, leading_empty: 0 }
    }
}

/// One property inside a [`PropertyBlock`].
#[derive(Debug, Clone)]
pub struct PropertyEntry {
    pub name: Arc<str>,
    pub value: PropValue,
    /// Where this entry sat in the class's flattened schema, or `None` for a
    /// [`BlockLayout::Native`] block, which has no schema to index into.
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
        self.entries.iter().find(|e| &*e.name == name).map(|e| &e.value)
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
            BlockLayout::Native { .. } => None,
        }
    }
}

/// Adopt a hand-assembled field map as a block.
///
/// The hand-written struct decoders name their fields themselves rather than
/// walking a schema, so their entries have no [`SchemaSlot`]. Such a block is
/// only writable once [`BlockLayout::Native`] bytes are attached to it, which
/// [`read_native_variable_struct`](super::structs::read_native_variable_struct)
/// does centrally for all of them.
impl From<std::collections::BTreeMap<String, PropValue>> for PropertyBlock {
    fn from(m: std::collections::BTreeMap<String, PropValue>) -> Self {
        PropertyBlock {
            entries: m
                .into_iter()
                .map(|(k, value)| PropertyEntry { name: Arc::from(k.as_str()), value, slot: None })
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
    Str(String),
    /// An `FPackageIndex` (import if negative, export if positive).
    Object(i32),
    /// An `FSoftObjectPath`: `(PackageName, AssetName, SubPath)`.
    SoftObject(SoftObjectPath),
    Array(Vec<PropValue>),
    /// A `TMap`, preserving insertion order.
    Map(Vec<(PropValue, PropValue)>),
    /// A nested struct — reflected or hand-written, see [`BlockLayout`].
    Struct(PropertyBlock),
    /// A natively-serialized struct's raw bytes (e.g. `FVector`/`FQuat`), kept
    /// so transforms can be decoded on demand.
    Native(Vec<u8>),
    /// `FScriptDelegate`: the bound object and the function it names.
    Delegate { object: i32, function: FName },
    /// `FMulticastScriptDelegate`: the invocation list.
    MulticastDelegate(Vec<(i32, FName)>),
    /// `FFieldPath`: the property path and the object it is rooted at.
    FieldPath { path: Vec<FName>, owner: i32 },
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
    pub fn as_str(&self) -> Option<&str> {
        match self {
            PropValue::Name(n) => Some(n.as_str()),
            PropValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_map(&self) -> Option<&[(PropValue, PropValue)]> {
        match self {
            PropValue::Map(m) => Some(m),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[PropValue]> {
        match self {
            PropValue::Array(a) => Some(a),
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
    pub fn as_native(&self) -> Option<&[u8]> {
        match self {
            PropValue::Native(b) => Some(b),
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
    pub(crate) fn from_prop(v: &PropValue) -> Option<MeshTransform> {
        let s = v.as_struct()?;
        let f64s = |name: &str, n: usize| -> Option<Vec<f64>> {
            let b = s.get(name)?.as_native()?;
            if b.len() < n * 8 {
                return None;
            }
            Some(
                (0..n)
                    .map(|i| f64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().unwrap()))
                    .collect(),
            )
        };
        let mut t = MeshTransform::default();
        if let Some(r) = f64s("Rotation", 4) {
            t.rotation = [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32];
        }
        if let Some(tr) = f64s("Translation", 3) {
            t.translation = [tr[0] as f32, tr[1] as f32, tr[2] as f32];
        }
        if let Some(sc) = f64s("Scale3D", 3) {
            t.scale = [sc[0] as f32, sc[1] as f32, sc[2] as f32];
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
    pub sub_path: String,
}

impl SoftObjectPath {
    pub fn is_empty(&self) -> bool {
        self.package.as_str().is_empty() && self.asset.as_str().is_empty()
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
        assert_eq!(via_number.as_str(), literal.as_str(), "they render identically");
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
