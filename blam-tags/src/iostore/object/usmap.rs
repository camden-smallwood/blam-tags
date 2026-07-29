//! `.usmap` reader — the UE5 reflection schema (class/struct property
//! layouts) needed to walk cooked objects' *unversioned* property blocks.
//!
//! Cooked UE5 IoStore packages serialize `UObject` properties without tags
//! (no per-property name/type/size): just a fragment bitstream over the
//! class's property *schema*, then bare values. To skip or read that block
//! you need the schema, which lives in a `.usmap` mappings file (dumped
//! from the engine's reflection data). This module parses that file.
//!
//! Format = Epic's `.usmap`, version 4 (`ExplicitEnumValues`), validated
//! byte-exact against the Halo: Campaign Evolved (UE 5.5.4) mappings:
//! header (`u16` magic `0x30C4`, `u8` version, optional package-versioning
//! block, `u8` compression, `u32` comp/decomp sizes) + a Zstd/Oodle/None
//! payload holding: a name table, an enum table, and a struct/schema table.
//! See [`Usmap::parse`].

use std::collections::HashMap;
use std::io::{Cursor, Read};

use anyhow::{bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};

/// The Campaign Evolved (UE 5.5.4) mappings, bundled so CE mesh extraction
/// needs no external `.usmap`. Build-specific — the emitter regenerates it
/// from the user's own exe for other/patched builds.
pub const METEORITE_USMAP: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap"));

/// `EPropertyType` — the on-disk property-kind byte, in engine order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyType {
    Byte { enum_name: Option<String> },
    Bool,
    Int,
    Float,
    Object,
    Name,
    Delegate,
    Double,
    Array(Box<PropertyType>),
    Struct(String),
    Str,
    Text,
    Interface,
    MulticastDelegate,
    WeakObject,
    LazyObject,
    AssetObject,
    SoftObject,
    UInt64,
    UInt32,
    UInt16,
    Int64,
    Int16,
    Int8,
    Map(Box<PropertyType>, Box<PropertyType>),
    Set(Box<PropertyType>),
    Enum { inner: Box<PropertyType>, enum_name: String },
    FieldPath,
    Optional(Box<PropertyType>),
    Utf8Str,
    AnsiStr,
    /// A kind byte this reader doesn't model (forward-compat).
    Unknown(u8),
}

/// One serializable property in a struct/class schema.
#[derive(Debug, Clone)]
pub struct UsmapProperty {
    /// Index of this property within the flattened class schema (the value
    /// the unversioned-property fragment stream indexes by).
    pub schema_index: u16,
    /// Static array dimension (`1` for a plain property).
    pub array_dim: u8,
    pub name: String,
    pub ty: PropertyType,
}

/// A struct or class schema: its own serializable properties plus a link
/// to its super (base) so callers can flatten the inheritance chain.
#[derive(Debug, Clone)]
pub struct UsmapStruct {
    pub name: String,
    /// Base struct/class name, or `None` at the root of the chain.
    pub super_name: Option<String>,
    /// Total property count across the chain as recorded by the dumper
    /// (`PropertyCount`); the flattened schema length.
    pub prop_count: u16,
    /// This struct's *own* serializable properties (not inherited).
    pub properties: Vec<UsmapProperty>,
}

/// A parsed enum: name + `(value, name)` pairs (`ExplicitEnumValues`).
#[derive(Debug, Clone)]
pub struct UsmapEnum {
    pub name: String,
    pub values: Vec<(u64, String)>,
}

/// A parsed `.usmap`: the engine's reflection schema.
#[derive(Debug)]
pub struct Usmap {
    pub names: Vec<String>,
    pub enums: Vec<UsmapEnum>,
    pub structs: Vec<UsmapStruct>,
    by_name: HashMap<String, usize>,
}

// EUsmapVersion
const V_PACKAGE_VERSIONING: u8 = 1;
const V_EXPLICIT_ENUM_VALUES: u8 = 4;

// EUsmapCompressionMethod
const COMP_NONE: u8 = 0;
const COMP_OODLE: u8 = 1;
const COMP_BROTLI: u8 = 2;
const COMP_ZSTD: u8 = 3;

impl Usmap {
    /// Parse the bundled Campaign Evolved (UE 5.5.4) mappings.
    pub fn meteorite() -> Result<Self> {
        Self::parse(METEORITE_USMAP)
    }

    /// Look up a struct/class schema by name.
    pub fn get(&self, name: &str) -> Option<&UsmapStruct> {
        self.by_name.get(name).map(|&i| &self.structs[i])
    }

    /// Register (or replace) a schema at runtime. Blueprint-generated structs
    /// (e.g. a DataTable's `S_MetaHumanHeads` row struct) are absent from the
    /// native-reflection `.usmap`, so their layout must be recovered from the
    /// `UUserDefinedStruct` export and injected here before `read_struct` can
    /// decode instances of them. `properties` must already carry their
    /// serialization-order `schema_index` (0-based, derived-first).
    pub fn register_struct(
        &mut self,
        name: &str,
        super_name: Option<String>,
        properties: Vec<UsmapProperty>,
    ) {
        let prop_count = properties.len() as u16;
        let st = UsmapStruct { name: name.to_string(), super_name, prop_count, properties };
        if let Some(&i) = self.by_name.get(name) {
            self.structs[i] = st;
        } else {
            self.by_name.insert(name.to_string(), self.structs.len());
            self.structs.push(st);
        }
    }

    /// Flatten a class's full property schema across the super chain, in
    /// unversioned-serialization order. This mirrors the engine's
    /// `UStruct::PropertyLink` iteration, which visits the *most-derived*
    /// class's own properties first and then chains up to the base — so the
    /// flattened list is derived→base, and that is the order the fragment
    /// stream's schema index refers to.
    ///
    /// (Validated byte-perfect against a cooked `BlamMeshSynchronizationComponent`
    /// export: present fragments `{2,5,7}` land on `MeshSynchronizationDataAsset`,
    /// `AnimationClass`, `RuntimeRegions` only under derived-first ordering.)
    /// A **static array** property (`T Foo[N]`, `array_dim = N`) occupies `N`
    /// consecutive schema slots, one per element, each independently present or
    /// absent in the fragment stream. So it is emitted `array_dim` times.
    /// Ignoring this mis-indexes every property after the array and desyncs the
    /// reader — `MaterialInstance::PhysicalMaterialMap` is `array_dim = 8`,
    /// which is why `Parent` sits at schema index 9 and why every
    /// `MaterialInstanceConstant` failed to decode. Expanding by `array_dim`
    /// makes position == `schema_index` (rebased per struct) for all 10,647
    /// classes in the shipped `.usmap`. Tag classes are all `array_dim = 1`,
    /// which is why they decoded correctly regardless.
    pub fn flattened_properties(&self, name: &str) -> Option<Vec<&UsmapProperty>> {
        Some(self.flattened_slots(name)?.into_iter().map(|(p, _)| p).collect())
    }

    /// As [`Self::flattened_properties`], but pairing each slot with its index
    /// *within* a static array (`0` for a plain property).
    ///
    /// A `UPROPERTY` declared `Thing[8]` occupies eight consecutive schema
    /// slots, each independently present or absent in the fragment stream. The
    /// index is what distinguishes them; without it, a reader keyed on property
    /// name collapses all eight into whichever came last — which is exactly what
    /// happened to `MaterialInstance::PhysicalMaterialMap[8]` and the 90 other
    /// static-array properties the mappings declare.
    pub fn flattened_slots(&self, name: &str) -> Option<Vec<(&UsmapProperty, u8)>> {
        Some(self.flattened_owned_slots(name)?.into_iter().map(|(p, i, _)| (p, i)).collect())
    }

    /// As [`Self::flattened_slots`], but also naming the struct that *declares*
    /// each slot.
    ///
    /// Flattening deliberately loses this, and for reading it does not matter.
    /// It matters for any fact that is a property of the declaration rather than
    /// of the value — `bool` versus bitfield `uint8 b:1` being the one that
    /// forced this, since a flattened name alone is ambiguous: 133 bool property
    /// names in the engine headers are declared both ways by different structs.
    pub fn flattened_owned_slots(&self, name: &str) -> Option<Vec<(&UsmapProperty, u8, &str)>> {
        // Walk from the most-derived class up to the root, emitting each
        // struct's own properties as we go (derived→base).
        let mut out: Vec<(&UsmapProperty, u8, &str)> = Vec::new();
        let mut cur = self.get(name)?;
        loop {
            let owner: &str = &cur.name;
            // Each struct's own properties, in schema-index order, each
            // occupying `array_dim` slots.
            let mut own: Vec<&UsmapProperty> = cur.properties.iter().collect();
            own.sort_by_key(|p| p.schema_index);
            for p in own {
                for i in 0..p.array_dim.max(1) {
                    out.push((p, i, owner));
                }
            }
            match cur.super_name.as_deref().and_then(|s| self.get(s)) {
                Some(sup) => cur = sup,
                None => break,
            }
        }
        Some(out)
    }

    /// Parse a `.usmap` from its raw (still-compressed) file bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut c = Cursor::new(bytes);
        let magic = c.read_u16::<LittleEndian>()?;
        if magic != 0x30C4 {
            bail!("bad .usmap magic {magic:#06x} (expected 0x30C4)");
        }
        let version = c.read_u8()?;
        if version >= V_PACKAGE_VERSIONING {
            let has_versioning = c.read_i32::<LittleEndian>()?;
            if has_versioning != 0 {
                let _ue4 = c.read_i32::<LittleEndian>()?;
                let _ue5 = c.read_i32::<LittleEndian>()?;
                // FCustomVersionContainer: i32 count + count*(FGuid(16) + i32)
                let cv = c.read_i32::<LittleEndian>()?;
                for _ in 0..cv.max(0) {
                    let mut guid = [0u8; 16];
                    c.read_exact(&mut guid)?;
                    let _ver = c.read_i32::<LittleEndian>()?;
                }
                let _netcl = c.read_u32::<LittleEndian>()?;
            }
        }
        let method = c.read_u8()?;
        let comp_size = c.read_u32::<LittleEndian>()? as usize;
        let decomp_size = c.read_u32::<LittleEndian>()? as usize;
        let start = c.position() as usize;
        let payload = bytes
            .get(start..start + comp_size)
            .context("truncated .usmap: compressed payload shorter than header claims")?;

        let body = decompress(method, payload, decomp_size)?;
        if body.len() != decomp_size {
            bail!(
                "usmap decompressed size mismatch: got {}, header says {decomp_size}",
                body.len()
            );
        }
        Self::parse_body(&body, version)
    }

    fn parse_body(body: &[u8], version: u8) -> Result<Self> {
        let mut c = Cursor::new(body);

        // Names
        let n = c.read_u32::<LittleEndian>()? as usize;
        let mut names = Vec::with_capacity(n);
        for _ in 0..n {
            let len = c.read_u16::<LittleEndian>()? as usize; // LongFName
            let mut buf = vec![0u8; len];
            c.read_exact(&mut buf)?;
            names.push(String::from_utf8_lossy(&buf).into_owned());
        }
        let name_of = |idx: i32, names: &[String]| -> Option<String> {
            usize::try_from(idx).ok().and_then(|i| names.get(i).cloned())
        };

        // Enums
        let ec = c.read_u32::<LittleEndian>()? as usize;
        let mut enums = Vec::with_capacity(ec);
        for _ in 0..ec {
            let name = name_of(c.read_i32::<LittleEndian>()?, &names).unwrap_or_default();
            let vc = c.read_u16::<LittleEndian>()? as usize; // LargeEnums
            let mut values = Vec::with_capacity(vc);
            for _ in 0..vc {
                let value = if version >= V_EXPLICIT_ENUM_VALUES {
                    c.read_u64::<LittleEndian>()?
                } else {
                    values.len() as u64
                };
                let vn = name_of(c.read_i32::<LittleEndian>()?, &names).unwrap_or_default();
                values.push((value, vn));
            }
            enums.push(UsmapEnum { name, values });
        }

        // Structs
        let sc = c.read_u32::<LittleEndian>()? as usize;
        let mut structs = Vec::with_capacity(sc);
        let mut by_name = HashMap::with_capacity(sc);
        for _ in 0..sc {
            let name = name_of(c.read_i32::<LittleEndian>()?, &names).unwrap_or_default();
            let super_name = name_of(c.read_i32::<LittleEndian>()?, &names);
            let prop_count = c.read_u16::<LittleEndian>()?;
            let ser_count = c.read_u16::<LittleEndian>()? as usize;
            let mut properties = Vec::with_capacity(ser_count);
            for _ in 0..ser_count {
                let schema_index = c.read_u16::<LittleEndian>()?;
                let array_dim = c.read_u8()?;
                let pname = name_of(c.read_i32::<LittleEndian>()?, &names).unwrap_or_default();
                let ty = read_property_type(&mut c, &names)?;
                properties.push(UsmapProperty { schema_index, array_dim, name: pname, ty });
            }
            by_name.insert(name.clone(), structs.len());
            structs.push(UsmapStruct { name, super_name, prop_count, properties });
        }

        let consumed = c.position() as usize;
        if consumed != body.len() {
            bail!("usmap body under/over-read: consumed {consumed} of {}", body.len());
        }
        Ok(Self { names, enums, structs, by_name })
    }
}

fn decompress(method: u8, payload: &[u8], decomp_size: usize) -> Result<Vec<u8>> {
    match method {
        COMP_NONE => Ok(payload.to_vec()),
        COMP_ZSTD => {
            let mut dec = ruzstd::StreamingDecoder::new(payload)
                .map_err(|e| anyhow::anyhow!("zstd init: {e}"))?;
            let mut out = Vec::with_capacity(decomp_size);
            dec.read_to_end(&mut out).map_err(|e| anyhow::anyhow!("zstd decode: {e}"))?;
            Ok(out)
        }
        COMP_OODLE => bail!("Oodle-compressed .usmap not yet supported"),
        COMP_BROTLI => bail!("Brotli-compressed .usmap not yet supported"),
        other => bail!("unknown .usmap compression method {other}"),
    }
}

fn read_property_type(c: &mut Cursor<&[u8]>, names: &[String]) -> Result<PropertyType> {
    let name_of = |idx: i32| -> Option<String> {
        usize::try_from(idx).ok().and_then(|i| names.get(i).cloned())
    };
    let t = c.read_u8()?;
    Ok(match t {
        // ByteProperty carries no trailing data in this format; an
        // enum-typed byte is encoded as `EnumProperty<ByteProperty>`.
        0 => PropertyType::Byte { enum_name: None },
        1 => PropertyType::Bool,
        2 => PropertyType::Int,
        3 => PropertyType::Float,
        4 => PropertyType::Object,
        5 => PropertyType::Name,
        6 => PropertyType::Delegate,
        7 => PropertyType::Double,
        8 => PropertyType::Array(Box::new(read_property_type(c, names)?)),
        9 => PropertyType::Struct(name_of(c.read_i32::<LittleEndian>()?).unwrap_or_default()),
        10 => PropertyType::Str,
        11 => PropertyType::Text,
        12 => PropertyType::Interface,
        13 => PropertyType::MulticastDelegate,
        14 => PropertyType::WeakObject,
        15 => PropertyType::LazyObject,
        16 => PropertyType::AssetObject,
        17 => PropertyType::SoftObject,
        18 => PropertyType::UInt64,
        19 => PropertyType::UInt32,
        20 => PropertyType::UInt16,
        21 => PropertyType::Int64,
        22 => PropertyType::Int16,
        23 => PropertyType::Int8,
        24 => {
            let k = read_property_type(c, names)?;
            let v = read_property_type(c, names)?;
            PropertyType::Map(Box::new(k), Box::new(v))
        }
        25 => PropertyType::Set(Box::new(read_property_type(c, names)?)),
        26 => {
            let inner = read_property_type(c, names)?;
            let enum_name = name_of(c.read_i32::<LittleEndian>()?).unwrap_or_default();
            PropertyType::Enum { inner: Box::new(inner), enum_name }
        }
        27 => PropertyType::FieldPath,
        28 => PropertyType::Optional(Box::new(read_property_type(c, names)?)),
        29 => PropertyType::Utf8Str,
        30 => PropertyType::AnsiStr,
        other => PropertyType::Unknown(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_meteorite_usmap() {
        let m = Usmap::meteorite().expect("parse bundled usmap");
        // Validated byte-exact against the Python reference parser.
        assert_eq!(m.names.len(), 50346, "name count");
        assert_eq!(m.enums.len(), 2407, "enum count");
        assert_eq!(m.structs.len(), 12679, "struct count");

        let sk = m.get("SkeletalMesh").expect("SkeletalMesh schema");
        assert_eq!(sk.super_name.as_deref(), Some("SkinnedAsset"));
        assert_eq!(sk.properties.len(), 33);
        assert_eq!(sk.properties[0].name, "Skeleton");
        assert!(matches!(sk.properties[0].ty, PropertyType::Object));
        // Materials: ArrayProperty<StructProperty<SkeletalMaterial>>
        let mats = sk.properties.iter().find(|p| p.name == "Materials").unwrap();
        match &mats.ty {
            PropertyType::Array(inner) => {
                assert!(matches!(inner.as_ref(), PropertyType::Struct(n) if n == "SkeletalMaterial"));
            }
            other => panic!("Materials type = {other:?}"),
        }

        // Flattened schema walks the super chain past SkinnedAsset.
        let flat = m.flattened_properties("SkeletalMesh").expect("flatten");
        assert!(flat.len() > sk.properties.len(), "flattened includes ancestors");
    }
}

/// Schemas for classes the shipped game references but that no `.usmap` dumped
/// from it can contain.
///
/// A `.usmap` is a snapshot of a *running* process's reflection data. Campaign
/// Evolved's cooked packages carry exports of editor-plugin classes
/// (`WorldPartitionHLODUtilities`, `PerformanceOverlayTool`) because the cooker
/// ran in the editor with those plugins loaded — their names are even in the
/// shipped `global.utoc` script-object table. But the shipped executable does
/// not contain those modules at all (their class names do not appear in it), so
/// the reflection data is not there to dump and the runtime could not construct
/// them either.
///
/// These are stock Unreal plugins, so their layouts are recovered from the
/// engine's own definitions and registered here. Each is corpus-checked: the
/// export must decode *and* account for every byte.
pub fn register_editor_plugin_classes(usmap: &mut Usmap) {
    let obj = |index: u16, name: &str, ty: PropertyType| UsmapProperty {
        schema_index: index,
        array_dim: 1,
        name: name.to_string(),
        ty,
    };

    // `UHLODBuilderMeshMergeSettings : UHLODBuilderSettings` — the merge
    // settings struct then an optional override material. Confirmed against
    // `HLODLayer_Merged`, whose 45-byte export is a 2-byte header (properties 0
    // and 1 present), a 35-byte `FMeshMergingSettings` block, the material as
    // `fa ff ff ff` (import -6), and the four-byte object trailer.
    usmap.register_struct(
        "HLODBuilderMeshMergeSettings",
        Some("HLODBuilderSettings".to_string()),
        vec![
            obj(0, "MeshMergeSettings", PropertyType::Struct("MeshMergingSettings".to_string())),
            obj(1, "HLODMaterial", PropertyType::Object),
        ],
    );

    // `UHLODBuilderInstancingSettings : UHLODBuilderSettings`.
    usmap.register_struct(
        "HLODBuilderInstancingSettings",
        Some("HLODBuilderSettings".to_string()),
        vec![obj(0, "bDisallowNanite", PropertyType::Bool)],
    );
}
