//! The global container's **script object** table — the authoritative map from
//! an `FPackageObjectIndex` script-import hash back to the engine object it
//! names (`/Script/Engine.StaticMeshComponent`, …).
//!
//! Cooked packages refer to engine classes only by a 64-bit hash of the
//! object's path, so nothing in a package says what class an export *is*.
//! Reversing that hash by guessing candidate paths (e.g. from a UHT header
//! dump) only ever resolves the classes someone thought to try. The shipped
//! `global.utoc` carries the real table: every script object the game can load,
//! with its name, its outer, and its own global index.
//!
//! Layout of the `ScriptObjects` chunk (`EIoChunkType` 5, an otherwise all-zero
//! chunk id): a global `FNameMap` name batch, then a `TArray` of
//! `FScriptObjectEntry` — `FMappedName object_name`, and the `global_index`,
//! `outer_index` and `cdo_class_index` as `FPackageObjectIndex`es.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::name_map::{EMappedNameType, FMappedName, FNameMap};
use super::ser::{Readable, ReadExt};
use super::ue_types::FPackageObjectIndex;
use crate::iostore::IoStoreArchive;

/// `EIoChunkType::ScriptObjects`.
const CHUNK_TYPE_SCRIPT_OBJECTS: u8 = 5;

#[derive(Debug, Clone, Copy)]
pub struct ScriptObjectEntry {
    pub object_name: FMappedName,
    pub global_index: FPackageObjectIndex,
    pub outer_index: FPackageObjectIndex,
    pub cdo_class_index: FPackageObjectIndex,
}

/// The decoded table, with paths resolved for lookup by script-import hash.
#[derive(Debug, Clone, Default)]
pub struct ScriptObjects {
    entries: Vec<ScriptObjectEntry>,
    /// Script-import hash → fully qualified object path.
    by_hash: HashMap<u64, String>,
}

impl ScriptObjects {
    /// Read and resolve the table from a container that carries it (normally
    /// `global.utoc`).
    pub fn load(utoc: impl AsRef<Path>) -> Result<Self> {
        let archive = IoStoreArchive::open(utoc.as_ref())
            .with_context(|| format!("opening {}", utoc.as_ref().display()))?;
        // The script-object chunk's id is zero except for its type byte.
        let mut id = [0u8; 12];
        id[11] = CHUNK_TYPE_SCRIPT_OBJECTS;
        let index = archive
            .find_chunk(&crate::iostore::FIoChunkId(id))
            .context("container has no ScriptObjects chunk")?;
        Self::parse(&archive.read_chunk(index)?)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let mut cur = Cursor::new(bytes);
        let names = FNameMap::deserialize(&mut cur, EMappedNameType::Global)
            .context("global name map")?;
        let count: i32 = cur.de()?;
        if !(0..=10_000_000).contains(&count) {
            bail!("implausible script object count {count}");
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            entries.push(ScriptObjectEntry {
                object_name: FMappedName::de(&mut cur)?,
                global_index: FPackageObjectIndex::de(&mut cur)?,
                outer_index: FPackageObjectIndex::de(&mut cur)?,
                cdo_class_index: FPackageObjectIndex::de(&mut cur)?,
            });
        }

        // Index by global index so outers can be walked, then build each
        // object's full path.
        let by_index: HashMap<u64, usize> = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.global_index.raw_index(), i))
            .collect();
        let mut by_hash = HashMap::with_capacity(entries.len());
        for entry in &entries {
            if let Some(path) = resolve_path(entry, &entries, &by_index, &names, 0) {
                by_hash.insert(entry.global_index.raw_index(), path);
            }
        }
        Ok(Self { entries, by_hash })
    }

    /// The object path for a script-import hash, e.g.
    /// `/Script/Engine.StaticMeshComponent`.
    pub fn resolve(&self, hash: u64) -> Option<&str> {
        self.by_hash.get(&hash).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn entries(&self) -> &[ScriptObjectEntry] {
        &self.entries
    }

    /// How many resolved paths hash back to the index they were read from.
    ///
    /// The hash is computed from the path itself, so recomputing it is a
    /// self-check on the whole table: a path built with the wrong separator or
    /// a mis-walked outer chain will not round-trip.
    pub fn verified_count(&self) -> usize {
        self.by_hash
            .iter()
            .filter(|(hash, path)| {
                FPackageObjectIndex::create_script_import(path).raw_index() == **hash
            })
            .count()
    }
}

/// Build `outer.name` / `outer:name` by walking the outer chain to its package.
fn resolve_path(
    entry: &ScriptObjectEntry,
    entries: &[ScriptObjectEntry],
    by_index: &HashMap<u64, usize>,
    names: &FNameMap,
    depth: usize,
) -> Option<String> {
    if depth > 16 {
        return None;
    }
    let name = names.get(entry.object_name).into_owned();
    let Some(outer) = by_index.get(&entry.outer_index.raw_index()) else {
        // No outer: this is a root, i.e. the `/Script/Module` package itself.
        return Some(name);
    };
    let parent = resolve_path(&entries[*outer], entries, by_index, names, depth + 1)?;
    // A package's direct children are separated by `.`; anything deeper (a
    // function or property inside a class) by `:`.
    Some(if parent.contains('.') { format!("{parent}:{name}") } else { format!("{parent}.{name}") })
}
