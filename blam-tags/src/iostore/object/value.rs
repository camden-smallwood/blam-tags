//! The values a decoded property block holds.
//!
//! Kept separate from the machinery that produces them so that the reader, the
//! writer and every typed decoder agree on one representation.

use std::collections::BTreeMap;
use std::ops::Deref;

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
    /// A nested reflected struct: property name → value.
    Struct(BTreeMap<String, PropValue>),
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
    pub fn as_struct(&self) -> Option<&BTreeMap<String, PropValue>> {
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
