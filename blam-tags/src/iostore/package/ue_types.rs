// Ported from trumank/retoc (MIT)
//! Minimal UE5 core types needed by the Zen package + container-header content
//! serialization ported from retoc. Only the small newtypes and enums that
//! `zen.rs` / `container_header.rs` reference are included here; the full
//! IoStore `Toc`/`FIoChunkId`/reader/writer machinery lives in this crate's own
//! `iostore` module and is intentionally NOT ported.

use anyhow::{Context, Result};
use std::fmt::{Display, Formatter};
use std::io::{Read, Write};
use std::str::FromStr;
use strum::FromRepr;

use super::ser::*;

/// Reuse this crate's validated CityHash64 (Google CityHash v1.1, the variant
/// UE uses) instead of pulling in the `cityhasher` crate.
pub(crate) use crate::iostore::writer::cityhash64;

pub(crate) fn align_u64(value: u64, alignment: u64) -> u64 {
    (value + alignment - 1) & !(alignment - 1)
}
pub(crate) fn align_usize(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

/// CityHash64 of the lowercased string encoded as UTF-16LE.
pub(crate) fn lower_utf16_cityhash(s: &str) -> u64 {
    let bytes = s.to_ascii_lowercase().encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    cityhash64(&bytes)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FPackageId(pub u64);
impl Readable for FPackageId {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self(s.de()?))
    }
}
impl Writeable for FPackageId {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.0)
    }
}
impl Display for FPackageId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl FPackageId {
    pub fn from_name(name: &str) -> Self {
        Self(lower_utf16_cityhash(name))
    }
}
impl FromStr for FPackageId {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(FPackageId(s.parse()?))
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct FIoContainerId(pub u64);
impl FIoContainerId {
    pub fn from_name(name: &str) -> Self {
        Self(lower_utf16_cityhash(name))
    }
}
impl Readable for FIoContainerId {
    fn de<S: Read>(stream: &mut S) -> Result<Self> {
        Ok(Self(stream.de()?))
    }
}
impl Writeable for FIoContainerId {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.0)
    }
}

#[derive(Debug, Copy, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FGuid {
    a: u32,
    b: u32,
    c: u32,
    d: u32,
}
impl Readable for FGuid {
    fn de<S: Read>(stream: &mut S) -> Result<Self> {
        Ok(Self {
            a: stream.de()?,
            b: stream.de()?,
            c: stream.de()?,
            d: stream.de()?,
        })
    }
}
impl Writeable for FGuid {
    fn ser<S: Write>(&self, stream: &mut S) -> Result<()> {
        stream.ser(&self.a)?;
        stream.ser(&self.b)?;
        stream.ser(&self.c)?;
        stream.ser(&self.d)?;
        Ok(())
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct FSHAHash([u8; 20]);
impl Readable for FSHAHash {
    fn de<S: Read>(stream: &mut S) -> Result<Self> {
        Ok(Self(stream.de()?))
    }
}
impl Writeable for FSHAHash {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.0)
    }
}
impl std::fmt::Debug for FSHAHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FSHAHash(")?;
        for b in self.0 {
            write!(f, "{:02X}", b)?;
        }
        write!(f, ")")
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, FromRepr)]
#[repr(u8)]
pub enum EIoStoreTocVersion {
    #[default]
    Invalid,
    Initial,
    DirectoryIndex,
    PartitionSize,
    PerfectHash,
    PerfectHashWithOverflow,
    OnDemandMetaData,
    RemovedOnDemandMetaData,
    ReplaceIoChunkHashWithIoHash,
}
impl Readable for EIoStoreTocVersion {
    fn de<S: Read>(stream: &mut S) -> Result<Self> {
        Self::from_repr(stream.de()?).context("invalid EIoStoreTocVersion value")
    }
}
impl Writeable for EIoStoreTocVersion {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&(*self as u8))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(C)] // Needed for sizeof to determine number of entries in package header
pub struct FPackageObjectIndex {
    type_and_id: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, FromRepr)]
pub enum FPackageObjectIndexType {
    Export,
    ScriptImport,
    PackageImport,
    Null,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FPackageImportReference {
    pub imported_package_index: u32,
    pub imported_public_export_hash_index: u32,
}

impl FPackageObjectIndex {
    const INDEX_BITS: u64 = 62;
    const INDEX_MASK: u64 = (1 << Self::INDEX_BITS) - 1;
    const TYPE_SHIFT: u64 = Self::INDEX_BITS;
    const INVALID_ID: u64 = !0;

    pub fn create_from_raw(raw: u64) -> Self {
        Self { type_and_id: raw }
    }
    pub fn create(kind: FPackageObjectIndexType, value: u64) -> Self {
        Self {
            type_and_id: ((kind as u64) << Self::TYPE_SHIFT) | value,
        }
    }
    pub fn create_null() -> Self {
        Self::create(FPackageObjectIndexType::Null, Self::INVALID_ID)
    }
    pub fn create_export(export_index: u32) -> Self {
        Self::create(FPackageObjectIndexType::Export, export_index as u64)
    }
    pub fn create_script_import(object_path: &str) -> Self {
        let import_hash = Self::generate_import_hash_from_object_path(object_path);
        Self::create(FPackageObjectIndexType::ScriptImport, import_hash)
    }
    pub fn create_package_import(import_ref: FPackageImportReference) -> Self {
        let import_value = import_ref.imported_public_export_hash_index as u64 | ((import_ref.imported_package_index as u64) << 32);
        Self::create(FPackageObjectIndexType::PackageImport, import_value)
    }
    pub fn create_script_import_from_verse_path(verse_path: &str) -> Self {
        let import_hash = Self::generate_import_hash_from_verse_path(verse_path);
        Self::create(FPackageObjectIndexType::ScriptImport, import_hash)
    }
    // Function to create a legacy UE4 zen package import from the full, lower-case name of the imported/exported object using / as a separator
    pub fn create_legacy_package_import_from_path(object_path: &str) -> Self {
        let import_hash = Self::generate_import_hash_from_object_path(object_path);
        Self::create(FPackageObjectIndexType::PackageImport, import_hash)
    }
    pub fn raw_index(self) -> u64 {
        self.type_and_id & Self::INDEX_MASK
    }
    pub fn kind(self) -> FPackageObjectIndexType {
        FPackageObjectIndexType::from_repr((self.type_and_id >> Self::TYPE_SHIFT) as usize).unwrap()
    }
    pub fn value(self) -> Option<u64> {
        (self.kind() != FPackageObjectIndexType::Null).then_some(self.type_and_id)
    }
    pub fn export(self) -> Option<u32> {
        (self.kind() == FPackageObjectIndexType::Export).then_some(self.type_and_id as u32)
    }
    pub fn package_import(self) -> Option<FPackageImportReference> {
        (self.kind() == FPackageObjectIndexType::PackageImport).then_some(FPackageImportReference {
            imported_package_index: ((self.type_and_id & FPackageObjectIndex::INDEX_MASK) >> 32) as u32,
            imported_public_export_hash_index: (self.type_and_id as u32),
        })
    }
    pub fn is_null(self) -> bool {
        self.kind() == FPackageObjectIndexType::Null
    }
    pub fn to_raw(self) -> u64 {
        self.type_and_id
    }

    fn generate_import_hash_from_object_path(object_path: &str) -> u64 {
        let lower_slash_path = object_path
            .chars()
            .map(|c| match c {
                ':' | '.' => '/',
                c => c.to_ascii_lowercase(),
            })
            .collect::<String>();
        let mut hash: u64 = cityhash64(&lower_slash_path.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>());
        hash &= !(3 << 62);
        hash
    }
    fn generate_import_hash_from_verse_path(verse_path: &str) -> u64 {
        let mut hash: u64 = cityhash64(verse_path.as_bytes());
        hash &= !(3 << 62);
        hash
    }
}
impl Readable for FPackageObjectIndex {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self { type_and_id: s.de()? })
    }
}
impl Display for FPackageObjectIndex {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_and_id)
    }
}
impl Writeable for FPackageObjectIndex {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.type_and_id)?;
        Ok(())
    }
}
impl std::fmt::Debug for FPackageObjectIndex {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.kind() {
            FPackageObjectIndexType::Export => write!(f, "FPackageObjectIndex::Export({:X?})", self.export().unwrap()),
            FPackageObjectIndexType::ScriptImport => write!(f, "FPackageObjectIndex::ScriptImport({:X?})", self.raw_index()),
            FPackageObjectIndexType::PackageImport => write!(f, "FPackageObjectIndex::PackageImport({:X?})", self.package_import().unwrap()),
            FPackageObjectIndexType::Null => write!(f, "FPackageObjectIndex::Null"),
        }
    }
}
