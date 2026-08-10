// Ported from trumank/retoc (MIT)
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::io::{Cursor, Write};
use std::{borrow::Cow, io::Read};
use strum::{Display, FromRepr};

use super::ser::*;
use super::ue_types::cityhash64;

const FNAME_HASH_ALGORITHM_ID: u64 = 0xC164_0000;

fn name_hash(name: &str) -> u64 {
    let lower = name.to_ascii_lowercase();
    if lower.is_ascii() {
        cityhash64(lower.as_bytes())
    } else {
        cityhash64(&lower.encode_utf16().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>())
    }
}

fn name_header(name: &str) -> [u8; 2] {
    let len = if name.is_ascii() { name.len() as i16 } else { name.encode_utf16().count() as i16 + i16::MIN };
    len.to_be_bytes()
}

/// Breaks down a combined FName string into a base name and a number. Number is 0 if there is no number
pub(crate) fn break_down_name_string<'a>(name: &'a str) -> (&'a str, i32) {
    let mut name_without_number: &'a str = name;
    let mut name_number: i32 = 0; // 0 means no number

    // Attempt to break down the composite name into the name part and the number part
    if let Some((left, right)) = name.rsplit_once('_') {
        // Right part needs to be parsed as a valid signed integer that is >= 0 and converts back to the same string
        // Last part is important for not touching names like: Rocket_04 - 04 should stay a part of the name, not a number, otherwise we would actually get Rocket_4 when deserializing!
        if let Ok(parsed_number) = right.parse::<i32>()
            && parsed_number >= 0
            && parsed_number.to_string() == right
        {
            name_without_number = left;
            name_number = parsed_number + 1; // stored as 1 more than the actual number
        }
    }
    (name_without_number, name_number)
}

pub fn read_name_batch<S: Read>(s: &mut S) -> Result<Vec<String>> {
    let num: u32 = s.de()?;
    if num == 0 {
        return Ok(vec![]);
    }
    let _num_string_bytes: u32 = s.de()?;
    let hash_version: u64 = s.de()?;
    // An assert here aborts on a corrupt name batch; this is a parse failure.
    if hash_version != FNAME_HASH_ALGORITHM_ID {
        bail!("unknown FName hash algorithm {hash_version:#x}");
    }

    let _hash_bytes: Vec<u8> = s.de_ctx(num as usize * 8)?;
    let lengths = read_array(num as usize, s, |s| Ok(i16::from_be_bytes(s.de()?)))?;
    let names: Vec<_> = lengths
        .iter()
        .map(|&l| {
            let l = if l < 0 { i16::MIN - l } else { l };
            read_string_data(l as i32, s)
        })
        .collect::<Result<_>>()?;
    Ok(names)
}

pub fn write_name_batch<S: Write, T: AsRef<str>>(s: &mut S, names: &[T]) -> Result<()> {
    fn name_byte_size(name: &str) -> u32 {
        if name.is_ascii() { name.len() as u32 } else { name.encode_utf16().count() as u32 * 2 }
    }

    s.ser(&(names.len() as u32))?;
    if names.is_empty() {
        return Ok(());
    }

    s.ser(&names.iter().map(|s| name_byte_size(s.as_ref())).sum::<u32>())?;
    s.ser(&FNAME_HASH_ALGORITHM_ID)?;

    for name in names {
        s.ser(&name_hash(name.as_ref()))?;
    }

    for name in names {
        s.ser(&name_header(name.as_ref()))?;
    }

    for name in names {
        let name = name.as_ref();
        if name.is_ascii() {
            s.write_all(name.as_bytes())?;
        } else {
            for c in name.encode_utf16() {
                s.ser(&c)?;
            }
        }
    }
    Ok(())
}

pub fn read_name_batch_parts(names_buffer: &[u8]) -> Result<Vec<String>> {
    let mut names = vec![];
    let mut s = Cursor::new(names_buffer);
    while s.position() < names_buffer.len() as u64 {
        let l = i16::from_be_bytes(s.de()?);
        let l = if l < 0 { i16::MIN - l } else { l };
        if l < 0 && s.position() & 1 != 0 {
            // UTF16 strings aligned to 2 bytes so read one byte to reach alignment
            s.de::<u8>()?;
        }
        names.push(read_string_data(l as i32, &mut s)?);
    }
    Ok(names)
}

pub fn write_name_batch_parts<T: AsRef<str>>(names: &[T]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut cur_names = Cursor::new(vec![]);
    let mut cur_hashes = Cursor::new(vec![]);

    cur_hashes.ser(&FNAME_HASH_ALGORITHM_ID)?;

    for name in names {
        let name = name.as_ref();
        cur_names.ser(&name_header(name))?;
        if name.is_ascii() {
            cur_names.write_all(name.as_bytes())?;
        } else {
            if cur_names.position() & 1 != 0 {
                cur_names.ser(&0u8)?;
            }
            for c in name.encode_utf16() {
                cur_names.ser(&c)?;
            }
        }
        cur_hashes.ser(&name_hash(name))?;
    }

    Ok((cur_names.into_inner(), cur_hashes.into_inner()))
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FNameMap {
    kind: EMappedNameType,
    names: Vec<String>,
    name_lookup: HashMap<String, usize>,
}
impl FNameMap {
    pub fn deserialize<S: Read>(s: &mut S, kind: EMappedNameType) -> Result<Self> {
        let names: Vec<String> = read_name_batch(s)?;
        Ok(Self::create_from_names(kind, names))
    }
    pub fn serialize<S: Write>(&self, s: &mut S) -> Result<()> {
        write_name_batch(s, &self.names)
    }
}

impl FNameMap {
    pub fn create(kind: EMappedNameType) -> Self {
        Self { kind, names: Vec::new(), name_lookup: HashMap::new() }
    }
    pub fn create_from_names(kind: EMappedNameType, names: Vec<String>) -> Self {
        let mut name_lookup: HashMap<String, usize> = HashMap::with_capacity(names.len());
        for (name_index, name) in names.iter().cloned().enumerate() {
            name_lookup.insert(name, name_index);
        }
        Self { kind, names, name_lookup }
    }
    pub fn get(&self, name: FMappedName) -> Cow<'_, str> {
        assert_eq!(name.kind(), self.kind, "Attempt to map name of the different kind in this name map Name Kind is {}, but name map kind is {}", name.kind(), self.kind);
        let n = &self.names[name.index() as usize];
        if name.number != 0 { format!("{n}_{}", name.number - 1).into() } else { n.into() }
    }

    pub fn store(&mut self, name: &str) -> FMappedName {
        let (name_without_number, name_number) = break_down_name_string(name);

        // Attempt to resolve the existing name through lookup
        if let Some(existing_index) = self.name_lookup.get(name_without_number) {
            return FMappedName::create((*existing_index) as u32, self.kind, name_number as u32);
        }

        // Create a new name and add it to the names list and to the name lookup
        let new_name_index = self.names.len();
        self.name_lookup.insert(name_without_number.to_string(), new_name_index);
        self.names.push(name_without_number.to_string());
        FMappedName::create(new_name_index as u32, self.kind, name_number as u32)
    }

    pub fn copy_raw_names(&self) -> Vec<String> {
        self.names.clone()
    }

    /// The raw name entries, in index order.
    ///
    /// Borrowed rather than cloned: [`copy_raw_names`](Self::copy_raw_names)
    /// copies the whole table, which an editor listing it per frame cannot
    /// afford.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The index `store` would resolve `name` to, without interning it.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let (base, _) = break_down_name_string(name);
        self.name_lookup.get(base).copied()
    }

    /// [`get`](Self::get) that reports a bad reference instead of panicking.
    ///
    /// `get` indexes the slice directly and asserts on the name kind, which is
    /// correct for a header this crate deserialized but not for one an editor is
    /// mutating: an out-of-range `FMappedName` there would take the host process
    /// down rather than surface as a validation failure.
    pub fn try_get(&self, name: FMappedName) -> Option<Cow<'_, str>> {
        if name.kind() != self.kind {
            return None;
        }
        let base = self.names.get(name.index() as usize)?;
        Some(if name.number != 0 {
            format!("{base}_{}", name.number - 1).into()
        } else {
            base.into()
        })
    }

    /// Rewrite the entry at `index`, in place.
    ///
    /// Every `FMappedName` is stored as an index, so this retargets every
    /// reference to that entry at once — the package name, an export's object
    /// name, and each `FName` inside an export's properties alike. That is the
    /// point of it, and also why it is not a general-purpose setter: callers are
    /// responsible for refreshing any *decoded* copy of the old text they hold,
    /// because a stale `FName` re-interned by string would silently fork a new
    /// entry rather than follow this one.
    ///
    /// Refuses rather than corrupts:
    ///
    /// * an out-of-range index;
    /// * an empty name;
    /// * a trailing `_N`, which [`store`](Self::store) would split into the
    ///   `FMappedName`'s number field. Stored raw it would round-trip as
    ///   `Foo_3_3` through [`get`](Self::get) for any reference carrying a
    ///   number of its own, so the two representations must not mix;
    /// * a name another entry already holds, which would leave one of them
    ///   unreachable from `name_lookup` — a later `store` of that text would
    ///   pick the other index, and the two would diverge on the next reopen.
    ///   The comparison is exact, matching `name_lookup`: `name_hash` lowercases
    ///   but case-differing entries legitimately coexist in shipped packages.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<()> {
        let Some(current) = self.names.get(index) else {
            bail!("name index {index} is out of range ({} names)", self.names.len());
        };
        if name.is_empty() {
            bail!("a name map entry cannot be empty");
        }
        let (base, _) = break_down_name_string(name);
        if base != name {
            bail!(
                "{name:?} ends in a number suffix, which is carried by each reference rather than \
                 by the name map; store the base name {base:?} instead"
            );
        }
        if let Some(&existing) = self.name_lookup.get(name)
            && existing != index
        {
            bail!("{name:?} is already name map entry {existing}");
        }
        if current == name {
            return Ok(());
        }
        let previous = std::mem::replace(&mut self.names[index], name.to_owned());
        // Only drop the old key if it still points here. A table deserialized
        // with duplicate entries keeps the last one in `name_lookup`, so the key
        // may belong to a different index.
        if self.name_lookup.get(&previous) == Some(&index) {
            self.name_lookup.remove(&previous);
        }
        self.name_lookup.insert(name.to_owned(), index);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Display, FromRepr)]
#[repr(u32)]
pub enum EMappedNameType {
    #[default]
    Package = 0,
    Container = 1,
    Global = 2,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FMappedName {
    index_and_type: u32,
    pub number: u32,
}
impl FMappedName {
    const INDEX_BITS: u32 = 30;
    const INDEX_MASK: u32 = (1 << Self::INDEX_BITS) - 1;
    const TYPE_MASK: u32 = !Self::INDEX_MASK;
    const TYPE_SHIFT: u32 = Self::INDEX_BITS;
    pub fn create(index: u32, kind: EMappedNameType, number: u32) -> Self {
        let shifted_type: u32 = (kind as u32) << Self::TYPE_SHIFT;
        let index_and_type: u32 = (index & Self::INDEX_MASK) | (shifted_type & Self::TYPE_MASK);
        FMappedName { index_and_type, number }
    }
    pub fn index(self) -> u32 {
        self.index_and_type & Self::INDEX_MASK
    }
    pub fn kind(self) -> EMappedNameType {
        let kind: u32 = (self.index_and_type & Self::TYPE_MASK) >> Self::TYPE_SHIFT;
        EMappedNameType::from_repr(kind).unwrap()
    }
}
impl Readable for FMappedName {
    fn de<S: Read>(s: &mut S) -> Result<Self> {
        Ok(Self { index_and_type: s.de()?, number: s.de()? })
    }
}
impl Writeable for FMappedName {
    fn ser<S: Write>(&self, stream: &mut S) -> Result<()> {
        stream.ser(&self.index_and_type)?;
        stream.ser(&self.number)?;
        Ok(())
    }
}

#[cfg(test)]
mod corpus {
    use crate::iostore::IoStoreArchive;
    use crate::iostore::container_header::EIoContainerHeaderVersion;
    use crate::iostore::package::builder::{read_payloads, write_package};
    use crate::iostore::package::zen::FZenPackageHeader;
    use crate::iostore::ue_types::EIoStoreTocVersion;
    use std::io::Cursor;
    use std::path::PathBuf;

    const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
    const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

    /// Renaming one name-map entry must survive a whole-package write and
    /// reopen, changing that entry and nothing else.
    ///
    /// The unit tests prove the table's own bookkeeping. This proves the part
    /// they cannot: that the change goes through `write_package`'s regenerated
    /// name batch and hash block on real cooked packages, and that every
    /// `FMappedName` the header carries still resolves afterwards. A rename that
    /// silently dropped or reordered entries would leave a package that parses
    /// and resolves the wrong strings.
    ///
    ///   CE_PAKS=/path/to/Meteorite/Content/Paks \
    ///     cargo test --features iostore name_map::corpus -- --ignored --nocapture
    #[test]
    #[ignore = "requires a Campaign Evolved install; set CE_PAKS"]
    fn a_renamed_entry_survives_a_package_round_trip_on_every_shipped_tag() {
        const PROBE: &str = "BaboonRenameProbe";

        let Ok(root) = std::env::var("CE_PAKS") else {
            panic!("set CE_PAKS to the game's Content/Paks");
        };
        let mut utocs: Vec<PathBuf> = std::fs::read_dir(&root)
            .expect("read paks dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();

        let (mut total, mut checked) = (0usize, 0usize);
        let mut failures: Vec<String> = Vec::new();

        for utoc in &utocs {
            let Ok(archive) = IoStoreArchive::open(utoc) else {
                continue;
            };
            for entry in archive.entries() {
                let lower = entry.path.to_ascii_lowercase().replace('\\', "/");
                if !lower.ends_with(".uasset") || !lower.contains("/content/tags/") {
                    continue;
                }
                let Ok(bytes) = archive.read(&entry.path) else {
                    continue;
                };
                let Ok(header) =
                    FZenPackageHeader::deserialize(&mut Cursor::new(&bytes[..]), None, CV, HV, None)
                else {
                    continue;
                };
                total += 1;

                let before = header.name_map.copy_raw_names();
                // The probe has to be absent, or the rename is correctly refused
                // as a duplicate rather than exercised.
                if before.is_empty() || before.iter().any(|name| name == PROBE) {
                    continue;
                }
                let Ok(payloads) = read_payloads(&header, &bytes) else {
                    continue;
                };

                let mut edited = header.clone();
                // The last entry: renaming index 0 would usually move the
                // package's own name, which is a different operation.
                let index = before.len() - 1;
                if let Err(error) = edited.name_map.rename(index, PROBE) {
                    failures.push(format!("{}: rename refused: {error}", entry.path));
                    continue;
                }

                let rebuilt = match write_package(&edited, &payloads, HV) {
                    Ok((rebuilt, _store)) => rebuilt,
                    Err(error) => {
                        failures.push(format!("{}: rewrite failed: {error}", entry.path));
                        continue;
                    }
                };
                let reopened = match FZenPackageHeader::deserialize(
                    &mut Cursor::new(&rebuilt[..]),
                    None,
                    CV,
                    HV,
                    None,
                ) {
                    Ok(reopened) => reopened,
                    Err(error) => {
                        failures.push(format!("{}: reopen failed: {error}", entry.path));
                        continue;
                    }
                };
                checked += 1;

                let mut expected = before.clone();
                expected[index] = PROBE.to_owned();
                let after = reopened.name_map.copy_raw_names();
                if after != expected && failures.len() < 10 {
                    failures.push(format!(
                        "{}: name map differs at or beyond entry {index}\n  before {:?}\n  after  {:?}",
                        entry.path,
                        &before[index.saturating_sub(1)..],
                        after.get(index.saturating_sub(1)..).unwrap_or_default(),
                    ));
                    continue;
                }
                // Every reference the header carries must still land inside the
                // table it was written against.
                if reopened.name_map.try_get(reopened.summary.name).is_none() && failures.len() < 10
                {
                    failures.push(format!("{}: package name no longer resolves", entry.path));
                    continue;
                }
                if let Some(bad) = reopened
                    .export_map
                    .iter()
                    .position(|export| reopened.name_map.try_get(export.object_name).is_none())
                    && failures.len() < 10
                {
                    failures.push(format!("{}: export {bad} name no longer resolves", entry.path));
                }
            }
        }

        println!("tag packages     {total}");
        println!("renames verified {checked}");
        for failure in &failures {
            println!("FAILURE {failure}");
        }
        assert!(total > 0, "no tag packages found");
        assert!(checked > 0, "no package was actually exercised");
        assert!(failures.is_empty(), "{} package(s) failed", failures.len());
    }
}

#[cfg(test)]
mod rename_tests {
    use super::*;

    fn map(names: &[&str]) -> FNameMap {
        FNameMap::create_from_names(
            EMappedNameType::Package,
            names.iter().map(|name| (*name).to_owned()).collect(),
        )
    }

    /// The whole point: references are indices, so one rewrite moves every one
    /// of them at once.
    #[test]
    fn a_rename_retargets_every_reference_to_that_entry() {
        let mut names = map(&["Warthog", "Material"]);
        let reference = FMappedName::create(0, EMappedNameType::Package, 0);
        let numbered = FMappedName::create(0, EMappedNameType::Package, 4);
        assert_eq!(names.get(reference), "Warthog");
        assert_eq!(names.get(numbered), "Warthog_3");

        names.rename(0, "Scorpion").unwrap();
        assert_eq!(names.get(reference), "Scorpion");
        assert_eq!(names.get(numbered), "Scorpion_3");
        assert_eq!(names.names(), ["Scorpion", "Material"]);
    }

    /// `store` must find the new text at the renamed index, and must not fork a
    /// second entry for it.
    #[test]
    fn the_lookup_follows_the_rename_in_both_directions() {
        let mut names = map(&["Warthog"]);
        names.rename(0, "Scorpion").unwrap();

        assert_eq!(names.index_of("Scorpion"), Some(0));
        assert_eq!(names.index_of("Warthog"), None);
        assert_eq!(names.store("Scorpion").index(), 0);
        assert_eq!(names.len(), 1, "storing the new text must not append");
        // The old text is now genuinely absent, so interning it is an append.
        assert_eq!(names.store("Warthog").index(), 1);
    }

    /// A trailing `_N` belongs to the reference, not the table. Stored raw, a
    /// reference carrying its own number would render `Foo_3_3`.
    #[test]
    fn a_numeric_suffix_is_refused_because_the_reference_carries_it() {
        let mut names = map(&["Warthog"]);
        let error = names.rename(0, "Scorpion_3").unwrap_err().to_string();
        assert!(error.contains("number suffix"), "{error}");
        assert_eq!(names.names(), ["Warthog"], "the refusal changed nothing");

        // `Rocket_04` is not a number suffix — it does not round-trip as one —
        // so it is a legitimate name.
        names.rename(0, "Rocket_04").unwrap();
        assert_eq!(names.names(), ["Rocket_04"]);
    }

    /// A duplicate key would leave one entry unreachable from `store`, and the
    /// two would then diverge on the next reopen.
    #[test]
    fn renaming_onto_another_entry_is_refused() {
        let mut names = map(&["Warthog", "Scorpion"]);
        let error = names.rename(0, "Scorpion").unwrap_err().to_string();
        assert!(error.contains("already name map entry 1"), "{error}");
        assert_eq!(names.names(), ["Warthog", "Scorpion"]);

        // Renaming an entry to what it already is stays a no-op rather than
        // tripping its own duplicate check.
        names.rename(0, "Warthog").unwrap();
        assert_eq!(names.names(), ["Warthog", "Scorpion"]);
    }

    /// Case-differing entries legitimately coexist in shipped packages, so the
    /// duplicate check matches `name_lookup` exactly rather than by hash.
    #[test]
    fn case_differing_entries_are_not_duplicates() {
        let mut names = map(&["Warthog", "Placeholder"]);
        names.rename(1, "warthog").unwrap();
        assert_eq!(names.names(), ["Warthog", "warthog"]);
        assert_eq!(names.index_of("Warthog"), Some(0));
        assert_eq!(names.index_of("warthog"), Some(1));
    }

    #[test]
    fn out_of_range_and_empty_are_refused() {
        let mut names = map(&["Warthog"]);
        assert!(names.rename(1, "Scorpion").is_err());
        assert!(names.rename(0, "").is_err());
        assert_eq!(names.names(), ["Warthog"]);
    }

    /// The hash block is regenerated from the names on write, so a rename costs
    /// nothing extra on the wire and reopens as itself.
    #[test]
    fn a_renamed_table_round_trips_through_the_name_batch() {
        let mut names = map(&["Warthog", "Material"]);
        names.rename(0, "Scorpion").unwrap();

        let mut buffer = Vec::new();
        names.serialize(&mut Cursor::new(&mut buffer)).unwrap();
        let reopened =
            FNameMap::deserialize(&mut Cursor::new(&buffer[..]), EMappedNameType::Package).unwrap();

        assert_eq!(reopened.names(), ["Scorpion", "Material"]);
        assert_eq!(reopened.index_of("Scorpion"), Some(0));
    }

    /// An editor mutating a header can produce a reference the table does not
    /// cover; that has to be reportable rather than fatal.
    #[test]
    fn try_get_reports_a_bad_reference_instead_of_panicking() {
        let names = map(&["Warthog"]);
        assert_eq!(names.try_get(FMappedName::create(0, EMappedNameType::Package, 0)).as_deref(), Some("Warthog"));
        assert!(names.try_get(FMappedName::create(7, EMappedNameType::Package, 0)).is_none());
        assert!(names.try_get(FMappedName::create(0, EMappedNameType::Container, 0)).is_none());
    }
}
