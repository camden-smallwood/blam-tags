// Ported from trumank/retoc (MIT)
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Seek;
use std::{
    collections::BTreeMap,
    io::{Cursor, Read, SeekFrom, Write},
    marker::PhantomData,
};
use strum::FromRepr;

use crate::iostore::package::name_map::{EMappedNameType, FMappedName, FNameMap, read_name_batch_parts, write_name_batch_parts};
use crate::iostore::package::ser::*;
use crate::iostore::package::ue_types::{FIoContainerId, FPackageId, FSHAHash};

#[derive(Debug, PartialEq)]
pub struct FIoContainerHeader {
    pub version: EIoContainerHeaderVersion,
    pub container_id: FIoContainerId,
    packages: StoreEntries,
    optional_segment_package_ids: Vec<FPackageId>,
    optional_segment_store_entries: Vec<u8>,
    redirect_name_map: FNameMap,
    localized_packages: Vec<FIoContainerHeaderLocalizedPackage>,
    package_redirects: Vec<FIoContainerHeaderPackageRedirect>,
    soft_package_references: Option<FIoContainerHeaderSoftPackageReferences>,
    // Legacy UE4 culture map (also known as localized package map) and package redirects (without source package name information)
    legacy_culture_package_map: FCulturePackageMap,
    legacy_package_redirects: Vec<LegacyContainerHeaderPackageRedirect>,
    // HashSet for IDs of the localized packages, since they only need to be added once
    localized_source_package_ids: HashSet<FPackageId>,
    // Package redirect lookup table, from source package ID to the redirected package ID
    package_redirect_lookup: HashMap<FPackageId, FPackageId>,
}
impl Readable for FIoContainerHeader {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Self::deserialize(s, None)
    }
}
impl FIoContainerHeader {
    pub fn deserialize<S: Read>(s: &mut S, version_override: Option<EIoContainerHeaderVersion>) -> Result<Self> {
        let signature: u32 = s.de()?;
        let version: EIoContainerHeaderVersion;
        let container_id;
        // if first 4 bytes are not MAGIC then header version must be Initial or earlier
        // if version override >= Initial, then first 4 bytes must be MAGIC, and version must be known
        if version_override.is_some_and(|v| v <= EIoContainerHeaderVersion::Initial) || signature != Self::MAGIC {
            version = version_override.unwrap_or(EIoContainerHeaderVersion::Initial);
            let mut id = [0; 8];
            id[0..4].copy_from_slice(&signature.to_le_bytes());
            id[4..8].copy_from_slice(&s.de::<[u8; 4]>()?);
            container_id = FIoContainerId(u64::from_le_bytes(id));
        } else {
            version = s.de()?;
            version_override.inspect(|v| assert_eq!(*v, version));
            container_id = s.de()?;
        }

        if version < EIoContainerHeaderVersion::OptionalSegmentPackages {
            let _package_count: u32 = s.de()?;
        }

        let mut new = Self::new(version, container_id);

        if version <= EIoContainerHeaderVersion::Initial {
            let names_buffer: Vec<u8> = s.de()?;
            let _name_hashes_buffer: Vec<u8> = s.de()?;
            let names = read_name_batch_parts(&names_buffer)?;

            // Create local name map for this container. This map should always be empty in legacy UE4 containers
            new.redirect_name_map = FNameMap::create_from_names(EMappedNameType::Container, names);
        }

        new.packages = StoreEntries::deserialize(s, version)?;

        if version > EIoContainerHeaderVersion::Initial {
            if version >= EIoContainerHeaderVersion::OptionalSegmentPackages {
                new.optional_segment_package_ids = s.de()?;
                new.optional_segment_store_entries = s.de()?;
            }

            new.redirect_name_map = FNameMap::deserialize(s, EMappedNameType::Container)?;
            new.localized_packages = s.de()?;
            new.package_redirects = s.de()?;

            // Populate Source Package IDs of localized packages from the list we just read
            new.localized_source_package_ids = new.package_redirects.iter().map(|x| x.source_package_id).collect();

            // Populate package redirects lookup from the package redirect list
            new.package_redirect_lookup.reserve(new.package_redirects.len());
            for redirect_entry in &new.package_redirects {
                new.package_redirect_lookup.insert(redirect_entry.source_package_id, redirect_entry.target_package_id);
            }
        } else {
            new.legacy_culture_package_map = s.de()?;
            new.legacy_package_redirects = s.de()?;

            // Populate package redirects lookup from the legacy package redirect list
            new.package_redirect_lookup.reserve(new.legacy_package_redirects.len());
            for redirect_entry in &new.legacy_package_redirects {
                new.package_redirect_lookup.insert(redirect_entry.source_package_id, redirect_entry.target_package_id);
            }
        }

        if version >= EIoContainerHeaderVersion::SoftPackageReferences {
            if version >= EIoContainerHeaderVersion::SoftPackageReferencesOffset {
                let soft_package_references_serial_info: FIoContainerHeaderSerialInfo = s.de()?;
                if soft_package_references_serial_info.size > 0 {
                    let has_soft_package_references: bool = s.de()?;
                    if has_soft_package_references {
                        new.soft_package_references = Some(s.de()?);
                    }
                }
            } else {
                let has_soft_package_references: bool = s.de()?;
                if has_soft_package_references {
                    new.soft_package_references = Some(s.de()?);
                }
            }
        }

        Ok(new)
    }
    pub fn serialize<S: Write + Seek>(&self, s: &mut S) -> Result<()> {
        if self.version > EIoContainerHeaderVersion::Initial {
            s.ser(&Self::MAGIC)?;
            s.ser(&self.version)?;
        }
        s.ser(&self.container_id)?;

        if self.version < EIoContainerHeaderVersion::OptionalSegmentPackages {
            s.ser(&(self.packages.0.len() as u32))?;
        }

        if self.version <= EIoContainerHeaderVersion::Initial {
            // Serialize container local name map. This map is generally empty in legacy UE4 containers because there are no fields that write to it
            let (names_buffer, name_hashes_buffer) = write_name_batch_parts(&self.redirect_name_map.copy_raw_names())?;
            s.ser(&names_buffer)?;
            s.ser(&name_hashes_buffer)?;
        }

        self.packages.serialize(s, self.version)?;

        if self.version > EIoContainerHeaderVersion::Initial {
            if self.version >= EIoContainerHeaderVersion::OptionalSegmentPackages {
                s.ser(&self.optional_segment_package_ids)?;
                s.ser(&self.optional_segment_store_entries)?;
            }

            self.redirect_name_map.serialize(s)?;
            s.ser(&self.localized_packages)?;
            s.ser(&self.package_redirects)?;
        } else {
            s.ser(&self.legacy_culture_package_map)?;
            s.ser(&self.legacy_package_redirects)?;
        }

        if self.version >= EIoContainerHeaderVersion::SoftPackageReferences {
            if self.version >= EIoContainerHeaderVersion::SoftPackageReferencesOffset {
                let serial_info_offset = s.stream_position()?;
                let mut soft_package_references_serial_info = FIoContainerHeaderSerialInfo::default();
                s.ser(&soft_package_references_serial_info)?;

                soft_package_references_serial_info.offset = s.stream_position()? as i64;
                s.ser(&self.soft_package_references.is_some())?;
                if let Some(soft_package_references) = &self.soft_package_references {
                    s.ser(soft_package_references)?;
                }
                soft_package_references_serial_info.size = s.stream_position()? as i64 - soft_package_references_serial_info.offset;

                let soft_package_references_end_offset = s.stream_position()?;
                s.seek(SeekFrom::Start(serial_info_offset))?;
                s.ser(&soft_package_references_serial_info)?;
                s.seek(SeekFrom::Start(soft_package_references_end_offset))?;
            } else {
                s.ser(&self.soft_package_references.is_some())?;
                if let Some(soft_package_references) = &self.soft_package_references {
                    s.ser(soft_package_references)?;
                }
            }
        }

        Ok(())
    }
}
impl FIoContainerHeader {
    const MAGIC: u32 = 0x496f436e;

    pub fn new(version: EIoContainerHeaderVersion, container_id: FIoContainerId) -> Self {
        Self {
            version,
            container_id,
            packages: StoreEntries::default(),
            optional_segment_package_ids: vec![],
            optional_segment_store_entries: vec![],
            redirect_name_map: FNameMap::default(),
            localized_packages: vec![],
            package_redirects: vec![],
            soft_package_references: None,
            legacy_culture_package_map: FCulturePackageMap::default(),
            legacy_package_redirects: vec![],
            localized_source_package_ids: HashSet::new(),
            package_redirect_lookup: HashMap::new(),
        }
    }

    pub fn add_package(&mut self, package_id: FPackageId, store_entry: StoreEntry) {
        self.packages.0.insert(package_id, store_entry);
    }

    pub fn add_localized_package(&mut self, package_culture: &str, source_package_name: &str, localized_package_id: FPackageId) -> Result<()> {
        let source_package_id = FPackageId::from_name(source_package_name);

        // New style localized packages do not track the localized package IDs, they only track the list of packages that are localized. Actual Package IDs for localized packages
        // are derived in runtime from package names. So we only need to create a single entry in the localized packages for each package
        if self.version > EIoContainerHeaderVersion::Initial {
            if !self.localized_source_package_ids.contains(&source_package_id) {
                let source_package_mapped_name = self.redirect_name_map.store(source_package_name);

                self.localized_source_package_ids.insert(source_package_id);
                self.localized_packages.push(FIoContainerHeaderLocalizedPackage {
                    source_package_id,
                    source_package_name: source_package_mapped_name,
                });
            }
        } else {
            // Old style localized packages. They track individual packages and their localized variants for each culture
            // Key in the culture package map is the culture name, values are mappings of source package ID to localized package ID
            let culture_localized_packages = self.legacy_culture_package_map.0.entry(package_culture.to_string()).or_default();
            culture_localized_packages.push((source_package_id, localized_package_id));
        }
        Ok(())
    }

    pub fn add_package_redirect(&mut self, source_package_name: &str, redirect_package_id: FPackageId) -> Result<()> {
        let source_package_id = FPackageId::from_name(source_package_name);

        // New style redirects track the package name as well as it's package ID
        if self.version > EIoContainerHeaderVersion::Initial {
            let source_package_name = self.redirect_name_map.store(source_package_name);

            self.package_redirects.push(FIoContainerHeaderPackageRedirect {
                source_package_id,
                source_package_name,
                target_package_id: redirect_package_id,
            });
            self.package_redirect_lookup.insert(source_package_id, redirect_package_id);
        } else {
            // Old style redirects only track bare source package ID and redirect package ID
            self.legacy_package_redirects.push(LegacyContainerHeaderPackageRedirect {
                source_package_id,
                target_package_id: redirect_package_id,
            });
            self.package_redirect_lookup.insert(source_package_id, redirect_package_id);
        }
        Ok(())
    }

    pub fn lookup_package_redirect(&self, source_package_id: FPackageId) -> Option<FPackageId> {
        self.package_redirect_lookup.get(&source_package_id).cloned()
    }

    pub fn get_store_entry(&self, package_id: FPackageId) -> Option<StoreEntry> {
        self.packages.get(package_id)
    }

    /// Drop a package's store entry, reporting whether one was there.
    ///
    /// Serialization rebuilds the count, the key list and every
    /// `offset_to_data_from_this` from the map, so removing a key is complete on
    /// its own — but only for the store. The members below are keyed by
    /// store-entry *ordinal* or by package id, and callers that remove a package
    /// have to check them separately.
    pub fn remove_package(&mut self, package_id: FPackageId) -> bool {
        self.packages.0.remove(&package_id).is_some()
    }

    /// Point every redirect that currently targets `from` at `to` instead.
    ///
    /// Returns how many moved. Without this a package could be renamed once and
    /// never again: renaming A to B installs a redirect targeting B, and the
    /// next rename would find B redirected-to and refuse rather than leave the
    /// first redirect dangling. Collapsing `A → B → C` to `A → C, B → C` keeps
    /// every name that ever pointed here resolving.
    pub fn retarget_package_redirect(&mut self, from: FPackageId, to: FPackageId) -> usize {
        let mut moved = 0;
        for redirect in &mut self.package_redirects {
            if redirect.target_package_id == from {
                redirect.target_package_id = to;
                moved += 1;
            }
        }
        for redirect in &mut self.legacy_package_redirects {
            if redirect.target_package_id == from {
                redirect.target_package_id = to;
                moved += 1;
            }
        }
        for target in self.package_redirect_lookup.values_mut() {
            if *target == from {
                *target = to;
            }
        }
        moved
    }

    /// Drop the redirect whose *source* is `source_package_id`, if any.
    ///
    /// Safe with respect to `redirect_name_map`: names are addressed by
    /// `FMappedName` index and the map is serialized whole, so an orphaned name
    /// costs bytes and nothing else. Removing it would renumber every index
    /// after it, which is the opposite of safe.
    pub fn remove_package_redirect(&mut self, source_package_id: FPackageId) -> bool {
        let before = self.package_redirects.len() + self.legacy_package_redirects.len();
        self.package_redirects
            .retain(|redirect| redirect.source_package_id != source_package_id);
        self.legacy_package_redirects
            .retain(|redirect| redirect.source_package_id != source_package_id);
        self.package_redirect_lookup.remove(&source_package_id);
        before != self.package_redirects.len() + self.legacy_package_redirects.len()
    }

    /// Whether the soft-package-reference block declares anything.
    ///
    /// Separate from [`has_soft_package_references`](Self::has_soft_package_references)
    /// because an empty block has no ordinals to desync: the refusal exists to
    /// protect a mapping, and a mapping of nothing is not one.
    pub fn soft_package_reference_count(&self) -> usize {
        self.soft_package_references
            .as_ref()
            .map(|refs| refs.package_indices.len())
            .unwrap_or_default()
    }

    /// Whether a soft-package-reference block is present.
    ///
    /// Its `package_indices` buffer is keyed by store-entry ordinal, and the
    /// store is a `BTreeMap` over `FPackageId` — so adding or removing any
    /// package renumbers ordinals that this block, round-tripped verbatim, does
    /// not follow. Mutating the store of such a container is refused rather than
    /// reasoned about.
    pub fn has_soft_package_references(&self) -> bool {
        self.soft_package_references.is_some()
    }

    /// Whether the optional segment declares any package. Its two parallel
    /// arrays have the same ordinal coupling as the soft references.
    pub fn has_optional_segment(&self) -> bool {
        !self.optional_segment_package_ids.is_empty()
    }

    /// Whether any redirect, new-style or legacy, points at `package_id`.
    /// Removing its target would leave the redirect resolving to nothing.
    pub fn redirects_to(&self, package_id: FPackageId) -> bool {
        self.package_redirects
            .iter()
            .any(|redirect| redirect.target_package_id == package_id)
            || self
                .legacy_package_redirects
                .iter()
                .any(|redirect| redirect.target_package_id == package_id)
            || self
                .package_redirect_lookup
                .values()
                .any(|target| *target == package_id)
    }

    /// Whether `package_id` is registered as a localized source package, in
    /// either the new list or the legacy culture map.
    pub fn is_localized_source(&self, package_id: FPackageId) -> bool {
        self.localized_source_package_ids.contains(&package_id)
            || self.legacy_culture_package_map.0.values().any(|packages| {
                packages
                    .iter()
                    .any(|(source, localized)| *source == package_id || *localized == package_id)
            })
    }
    pub fn package_ids(&self) -> std::iter::Copied<std::collections::btree_map::Keys<'_, FPackageId, StoreEntry>> {
        self.packages.0.keys().copied()
    }
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, FromRepr)]
#[repr(i32)]
pub enum EIoContainerHeaderVersion {
    PreInitial = -1,
    Initial = 0,
    LocalizedPackages = 1,
    OptionalSegmentPackages = 2,
    NoExportInfo = 3,
    SoftPackageReferences = 4,
    #[default]
    SoftPackageReferencesOffset = 5,
}
impl Readable for EIoContainerHeaderVersion {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        let value = s.de()?;
        Self::from_repr(value).with_context(|| format!("invalid EIoContainerHeaderVersion value: {value}"))
    }
}
impl Writeable for EIoContainerHeaderVersion {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&(*self as u32))
    }
}

#[derive(Debug, PartialEq)]
struct FIoContainerHeaderLocalizedPackage {
    source_package_id: FPackageId,
    source_package_name: FMappedName,
}
impl Readable for FIoContainerHeaderLocalizedPackage {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self {
            source_package_id: s.de()?,
            source_package_name: s.de()?,
        })
    }
}
impl Writeable for FIoContainerHeaderLocalizedPackage {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.source_package_id)?;
        s.ser(&self.source_package_name)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct FIoContainerHeaderPackageRedirect {
    source_package_id: FPackageId,
    target_package_id: FPackageId,
    source_package_name: FMappedName,
}
impl Readable for FIoContainerHeaderPackageRedirect {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self {
            source_package_id: s.de()?,
            target_package_id: s.de()?,
            source_package_name: s.de()?,
        })
    }
}
impl Writeable for FIoContainerHeaderPackageRedirect {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.source_package_id)?;
        s.ser(&self.target_package_id)?;
        s.ser(&self.source_package_name)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct FIoContainerHeaderSoftPackageReferences {
    package_ids: Vec<FPackageId>,
    package_indices: Vec<u8>,
}
impl Readable for FIoContainerHeaderSoftPackageReferences {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self { package_ids: s.de()?, package_indices: s.de()? })
    }
}
impl Writeable for FIoContainerHeaderSoftPackageReferences {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.package_ids)?;
        s.ser(&self.package_indices)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq, Default)]
struct FIoContainerHeaderSerialInfo {
    offset: i64,
    size: i64,
}
impl Readable for FIoContainerHeaderSerialInfo {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self { offset: s.de()?, size: s.de()? })
    }
}
impl Writeable for FIoContainerHeaderSerialInfo {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.offset)?;
        s.ser(&self.size)?;
        Ok(())
    }
}

// Used for UE4.27 package redirects that do not provide a source package name
#[derive(Debug, PartialEq)]
struct LegacyContainerHeaderPackageRedirect {
    source_package_id: FPackageId,
    target_package_id: FPackageId,
}
impl Readable for LegacyContainerHeaderPackageRedirect {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self { source_package_id: s.de()?, target_package_id: s.de()? })
    }
}
impl Writeable for LegacyContainerHeaderPackageRedirect {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.source_package_id)?;
        s.ser(&self.target_package_id)?;
        Ok(())
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct StoreEntry {
    // version == EIoContainerHeaderVersion::NoExportInfo
    pub export_bundles_size: u64,
    pub load_order: u32,

    // version < EIoContainerHeaderVersion::NoExportInfo
    pub export_count: i32,
    pub export_bundle_count: i32,

    pub imported_packages: Vec<FPackageId>,
    pub shader_map_hashes: Vec<FSHAHash>,
}

#[derive(Debug, Default, PartialEq)]
struct StoreEntries(BTreeMap<FPackageId, StoreEntry>);
impl StoreEntries {
    fn get(&self, package_id: FPackageId) -> Option<StoreEntry> {
        self.0.get(&package_id).cloned()
    }
    fn deserialize<S: Read>(s: &mut S, version: EIoContainerHeaderVersion) -> Result<Self> {
        let package_ids: Vec<FPackageId> = s.de()?;

        let buffer: Vec<u8> = s.de()?;
        let mut cur = Cursor::new(buffer);

        let (member_offset, entry_size) = match version {
            EIoContainerHeaderVersion::PreInitial => (8, 16),
            EIoContainerHeaderVersion::Initial => (24, 32),
            EIoContainerHeaderVersion::LocalizedPackages => (8, 24),
            EIoContainerHeaderVersion::OptionalSegmentPackages => (8, 24),
            EIoContainerHeaderVersion::NoExportInfo => (0, 16),
            EIoContainerHeaderVersion::SoftPackageReferences => (0, 16),
            EIoContainerHeaderVersion::SoftPackageReferencesOffset => (0, 16),
        };

        let entries = read_array(package_ids.len(), &mut cur, |s| FFilePackageStoreEntry::deserialize(s, version))?;

        let entries = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| -> Result<StoreEntry> {
                let offset = i * entry_size; // sizeof(FFilePackageStoreEntry)

                let mut new = StoreEntry {
                    export_bundles_size: entry.export_bundles_size,
                    load_order: entry.load_order,

                    export_count: entry.export_count,
                    export_bundle_count: entry.export_bundle_count,

                    ..Default::default()
                };

                let num = entry.imported_packages.array_num as usize;
                new.imported_packages = if num != 0 {
                    let offset = offset + member_offset + entry.imported_packages.offset_to_data_from_this as usize; // offset_of(FFilePackageStoreEntry::imported_packages)
                    cur.seek(SeekFrom::Start(offset as u64))?;
                    cur.de_ctx(num)?
                } else {
                    vec![]
                };

                if version > EIoContainerHeaderVersion::Initial {
                    let num = entry.shader_map_hashes.array_num as usize;
                    new.shader_map_hashes = if num != 0 {
                        let offset = offset + member_offset + entry.shader_map_hashes.offset_to_data_from_this as usize + 8; // offset_of(FFilePackageStoreEntry::shader_map_hashes)
                        cur.seek(SeekFrom::Start(offset as u64))?;
                        cur.de_ctx(num)?
                    } else {
                        vec![]
                    };
                }

                Ok(new)
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Self(BTreeMap::from_iter(package_ids.into_iter().zip(entries.into_iter()))))
    }
    fn serialize<S: Write>(&self, s: &mut S, version: EIoContainerHeaderVersion) -> Result<()> {
        s.ser(&(self.0.len() as u32))?;
        for package_id in self.0.keys() {
            s.ser(package_id)?;
        }

        let mut buffer: Vec<u8> = vec![];
        let mut cur = Cursor::new(&mut buffer);

        let (member_offset, entry_size) = match version {
            EIoContainerHeaderVersion::PreInitial => (8, 16),
            EIoContainerHeaderVersion::Initial => (24, 32),
            EIoContainerHeaderVersion::LocalizedPackages => (8, 24),
            EIoContainerHeaderVersion::OptionalSegmentPackages => (8, 24),
            EIoContainerHeaderVersion::NoExportInfo => (0, 16),
            EIoContainerHeaderVersion::SoftPackageReferences => (0, 16),
            EIoContainerHeaderVersion::SoftPackageReferencesOffset => (0, 16),
        };

        // calculate end of entries to start writing arrays
        let mut array_offset = self.0.len() * entry_size;

        for entry in self.0.values() {
            let mut ser_entry = FFilePackageStoreEntry {
                export_bundles_size: entry.export_bundles_size,
                load_order: entry.load_order,

                export_count: entry.export_count,
                export_bundle_count: entry.export_bundle_count,

                ..Default::default()
            };

            // save entry to calculate offsets and restore later
            let entry_offset = cur.position() as usize;

            // start writing arrays
            cur.set_position(array_offset as u64);

            if !entry.imported_packages.is_empty() {
                let offset = cur.position() as usize - entry_offset - member_offset;
                ser_entry.imported_packages.offset_to_data_from_this = offset as u32;
                ser_entry.imported_packages.array_num = entry.imported_packages.len() as u32;
                cur.ser_no_length(&entry.imported_packages)?;
            }
            if version > EIoContainerHeaderVersion::Initial && !entry.shader_map_hashes.is_empty() {
                let offset = cur.position() as usize - entry_offset - member_offset - 8;
                ser_entry.shader_map_hashes.offset_to_data_from_this = offset as u32;
                ser_entry.shader_map_hashes.array_num = entry.shader_map_hashes.len() as u32;
                cur.ser_no_length(&entry.shader_map_hashes)?;
            }

            // advance array_offset
            array_offset = cur.position() as usize;

            // reset cursor and write entry
            cur.set_position(entry_offset as u64);
            ser_entry.serialize(&mut cur, version)?;
        }

        s.ser::<Vec<u8>>(&buffer)?;
        Ok(())
    }
}

#[derive(Debug, Default)]
#[repr(C)]
struct TFilePackageStoreEntryCArrayView<T> {
    array_num: u32,
    offset_to_data_from_this: u32,
    _phantom: PhantomData<T>,
}
impl<T> Readable for TFilePackageStoreEntryCArrayView<T> {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self {
            array_num: s.de()?,
            offset_to_data_from_this: s.de()?,
            _phantom: Default::default(),
        })
    }
}
impl<T> Writeable for TFilePackageStoreEntryCArrayView<T> {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&self.array_num)?;
        s.ser(&self.offset_to_data_from_this)?;
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FFilePackageStoreEntry {
    // version == EIoContainerHeaderVersion::NoExportInfo
    export_bundles_size: u64,
    load_order: u32,

    // version < EIoContainerHeaderVersion::NoExportInfo
    export_count: i32,
    export_bundle_count: i32,

    imported_packages: TFilePackageStoreEntryCArrayView<FPackageId>,
    shader_map_hashes: TFilePackageStoreEntryCArrayView<FSHAHash>,
}
impl FFilePackageStoreEntry {
    fn deserialize<S: Read>(s: &mut S, version: EIoContainerHeaderVersion) -> Result<Self> {
        let mut entry = Self::default();

        if version == EIoContainerHeaderVersion::Initial {
            entry.export_bundles_size = s.de()?;
        }
        if version < EIoContainerHeaderVersion::NoExportInfo {
            entry.export_count = s.de()?;
            entry.export_bundle_count = s.de()?;
        }
        if version == EIoContainerHeaderVersion::Initial {
            entry.load_order = s.de()?;
            let _pad: u32 = s.de()?;
        }
        entry.imported_packages = s.de()?;
        if version > EIoContainerHeaderVersion::Initial {
            entry.shader_map_hashes = s.de()?;
        };
        Ok(entry)
    }
    fn serialize<S: Write>(&self, s: &mut S, version: EIoContainerHeaderVersion) -> Result<()> {
        if version == EIoContainerHeaderVersion::Initial {
            s.ser(&self.export_bundles_size)?;
        }
        if version < EIoContainerHeaderVersion::NoExportInfo {
            s.ser(&self.export_count)?;
            s.ser(&self.export_bundle_count)?;
        }
        if version == EIoContainerHeaderVersion::Initial {
            s.ser(&self.load_order)?;
            s.ser(&0u32)?;
        }
        s.ser(&self.imported_packages)?;
        if version > EIoContainerHeaderVersion::Initial {
            s.ser(&self.shader_map_hashes)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq)]
struct FCulturePackageMap(BTreeMap<String, Vec<(FPackageId, FPackageId)>>);
impl Readable for FCulturePackageMap {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        let culture_package_map_len: u32 = s.de()?;
        let mut culture_package_map = BTreeMap::new();
        for _ in 0..culture_package_map_len {
            let key: String = s.de()?;
            let value: Vec<(FPackageId, FPackageId)> = read_array(s.de::<u32>()? as usize, s, |s| Ok((s.de()?, s.de()?)))?;
            culture_package_map.insert(key, value);
        }
        Ok(Self(culture_package_map))
    }
}
impl Writeable for FCulturePackageMap {
    fn ser<S: Write>(&self, s: &mut S) -> Result<()> {
        s.ser(&(self.0.len() as u32))?;
        for (key, value) in &self.0 {
            s.ser(key)?;
            s.ser(&(value.len() as u32))?;
            for (a, b) in value {
                s.ser(a)?;
                s.ser(b)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod corpus {
    use super::*;
    use crate::iostore::IoStoreArchive;
    use std::io::Cursor;
    use std::path::PathBuf;

    /// `EIoChunkType::ContainerHeader`.
    const CHUNK_TYPE_CONTAINER_HEADER: u8 = 6;

    /// Report whether shipped containers carry the ordinal-keyed blocks that
    /// make adding or removing a package unsafe.
    ///
    /// A measurement, not an assertion about the game. It decides whether
    /// renaming a tag in place is possible on a shipped pak at all:
    /// `soft_package_references` and the optional segment are keyed by
    /// store-entry ordinal, and the store is a `BTreeMap` over `FPackageId`, so
    /// *adding* a package renumbers them exactly as removing one does.
    ///
    /// It matters twice over. `resolve_tag_deletion` refuses a container that
    /// carries them, but `duplicate_tag_in_place_impl` calls `add_package` with
    /// no such check -- so if shipped packs do carry a non-empty block, the
    /// duplicate path that already ships is unsound against them.
    ///
    ///   CE_PAKS=/path/to/Meteorite/Content/Paks \
    ///     cargo test --features iostore container::header::corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn report_ordinal_keyed_blocks_in_shipped_containers() {
        let Ok(root) = std::env::var("CE_PAKS") else {
            panic!("set CE_PAKS to the game's Content/Paks");
        };
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|path| !path.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();

        let (mut headers, mut soft, mut optional, mut locked) = (0usize, 0usize, 0usize, 0usize);
        for utoc in &utocs {
            let name = utoc.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            let Ok(archive) = IoStoreArchive::open(utoc) else {
                println!("{name:<44} could not be opened");
                continue;
            };
            let mut found = false;
            for index in 0..archive.chunk_count() as u32 {
                let Ok(id) = archive.chunk_id(index) else { continue };
                if id.chunk_type() != CHUNK_TYPE_CONTAINER_HEADER {
                    continue;
                }
                found = true;
                headers += 1;
                let Ok(bytes) = archive.read_chunk(index) else {
                    println!("{name:<44} header chunk unreadable");
                    continue;
                };
                match FIoContainerHeader::deserialize(&mut Cursor::new(&bytes[..]), None) {
                    Ok(header) => {
                        let refs = header.soft_package_reference_count();
                        let segment = header.optional_segment_package_ids.len();
                        soft += usize::from(refs > 0);
                        optional += usize::from(segment > 0);
                        locked += usize::from(refs > 0 || segment > 0);
                        println!(
                            "{name:<44} version {:?}  packages {}  soft refs {refs}  optional {segment}",
                            header.version,
                            header.package_ids().len(),
                        );
                    }
                    Err(error) => println!("{name:<44} header did not parse: {error}"),
                }
            }
            if !found {
                println!("{name:<44} carries no container header");
            }
        }

        println!("\ncontainer headers read      {headers}");
        println!("with soft package refs      {soft}");
        println!("with an optional segment    {optional}");
        println!("ordinal-locked              {locked}");
        if locked > 0 {
            println!(
                "\nIn-place rename is refused on those containers -- and the in-place duplicate\n\
                 path that already ships does NOT check for this before calling add_package."
            );
        } else {
            println!("\nNo container is ordinal-locked; in-place add and remove are both open.");
        }
        assert!(headers > 0, "no container header was found under CE_PAKS");
    }
}
