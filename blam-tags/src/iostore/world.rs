//! A mounted IoStore installation, and the schema resolver it can answer with.
//!
//! Decoding a package is not a function of that package alone. A `UDataTable`
//! finds its row layout through an object reference into *another* package; an
//! `FInstancedPropertyBag` names its members' struct types the same way; a
//! `UUserDefinedStruct` has no `.usmap` schema anywhere and must be read out of
//! the package that declares it. Every one of those needs the whole mount.
//!
//! This lived as a copied block in seven measurement harnesses, and the copying
//! was not harmless: a harness that ran without a resolver, or with a reduced
//! one, measured itself rather than the codec, and did so undetected three
//! separate times — data tables reported as unmodeled tails, a stop census with
//! no resolver at all, and edit gates that skipped every export needing one.
//! Having exactly one implementation is the fix; it is also the thing an
//! external caller opening a Campaign Evolved install needs first.
use std::cell::RefCell;
use std::rc::Rc;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};

use super::container_header::EIoContainerHeaderVersion;
use super::object::archive::PackageResolver;
use super::object::reflect::read_userdefined_struct_layout;
use super::object::unversioned::ExportContext;
use super::package::ue_types::FPackageObjectIndexType;
use super::script_objects::ScriptObjects;
use super::ue_types::EIoStoreTocVersion;
use super::usmap::{Usmap, UsmapProperty};
use super::zen::{FExportMapEntry, FZenPackageHeader};
use super::IoStoreArchive;

/// Container and header versions Campaign Evolved ships.
pub const CE_TOC_VERSION: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
/// See [`CE_TOC_VERSION`].
pub const CE_HEADER_VERSION: EIoContainerHeaderVersion =
    EIoContainerHeaderVersion::SoftPackageReferences;

/// Every `.utoc` in an installation, mounted, plus the reflection schema.
pub struct World {
    archives: Vec<IoStoreArchive>,
    /// Normalised package path (`/game/foo/bar`) -> (archive index, exact entry).
    by_pkg: HashMap<String, (usize, String)>,
    /// Script-object hash -> full object path, for resolving class imports.
    by_hash: HashMap<u64, String>,
    usmap: Usmap,
}

impl World {
    /// Mount every `.utoc` in `paks_dir` (except `global.utoc`, which supplies
    /// the script objects) against `usmap`.
    pub fn open(paks_dir: impl AsRef<Path>, usmap: Usmap) -> Result<Self> {
        let paks_dir = paks_dir.as_ref();
        let global = paks_dir.join("global.utoc");
        let so = ScriptObjects::load(&global)
            .with_context(|| format!("load script objects from {}", global.display()))?;
        let mut by_hash = HashMap::new();
        for e in so.entries() {
            if let Some(p) = so.resolve(e.global_index.raw_index()) {
                by_hash.insert(e.global_index.raw_index(), p.to_string());
            }
        }

        let mut utocs: Vec<_> = std::fs::read_dir(paks_dir)
            .with_context(|| format!("read {}", paks_dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
            .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
            .collect();
        utocs.sort();
        let archives: Vec<IoStoreArchive> =
            utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

        let mut by_pkg: HashMap<String, (usize, String)> = HashMap::new();
        for (i, a) in archives.iter().enumerate() {
            for e in a.entries() {
                let lo = e.path.to_ascii_lowercase().replace('\\', "/");
                let Some(stem) = lo.strip_suffix(".uasset").or_else(|| lo.strip_suffix(".umap"))
                else {
                    continue;
                };
                let Some((prefix, rest)) = stem.split_once("/content/") else { continue };
                // `Meteorite` is the project, so its content mounts at `/Game`.
                let mount = match prefix.rsplit('/').next().unwrap_or("") {
                    "meteorite" => "game",
                    m => m,
                };
                by_pkg.entry(format!("/{mount}/{rest}")).or_insert((i, e.path.clone()));
            }
        }
        Ok(World { archives, by_pkg, by_hash, usmap })
    }

    pub fn usmap(&self) -> &Usmap {
        &self.usmap
    }

    pub fn archives(&self) -> &[IoStoreArchive] {
        &self.archives
    }

    /// The class path behind a script-object hash, e.g. an export's
    /// `class_index`. `None` for a hash this mount does not define.
    pub fn class_path(&self, hash: u64) -> Option<&str> {
        self.by_hash.get(&hash).map(String::as_str)
    }

    /// As [`World::class_path`], reduced to the bare class name the `.usmap`
    /// keys by.
    pub fn class_name(&self, hash: u64) -> Option<&str> {
        self.class_path(hash).map(|p| p.rsplit('.').next().unwrap_or(p))
    }

    /// A resolver scoped to one package. Cheap; make one per package.
    pub fn resolver<'a>(
        &'a self,
        header: &'a FZenPackageHeader,
        bytes: &'a [u8],
        names: &'a [String],
    ) -> PkgResolver<'a> {
        PkgResolver { world: self, layouts: Rc::default(), header, bytes, names }
    }
}

type LayoutCache = RefCell<HashMap<String, Option<Vec<UsmapProperty>>>>;

/// Resolves the struct references made by one package, against a whole [`World`].
pub struct PkgResolver<'a> {
    world: &'a World,
    /// Memo, and the cycle guard: a name claims its slot before recursing.
    /// Shared with the resolvers this one spawns for other packages, so a chain
    /// of user-defined structs across packages is read once.
    layouts: Rc<LayoutCache>,
    header: &'a FZenPackageHeader,
    bytes: &'a [u8],
    names: &'a [String],
}

impl PkgResolver<'_> {
    /// Read a `UUserDefinedStruct` export's declared field layout.
    fn layout_of_export(&self, ex: &FExportMapEntry) -> Option<Vec<UsmapProperty>> {
        let off = self.header.summary.header_size as usize + ex.cooked_serial_offset as usize;
        let end = (off + ex.cooked_serial_size as usize).min(self.bytes.len());
        if off >= self.bytes.len() || off > end {
            return None;
        }
        let ctx = ExportContext { bulk_data: &[], resolver: Some(self) };
        read_userdefined_struct_layout(
            &self.bytes[off..end],
            self.names,
            &self.world.usmap,
            ex.object_flags,
            &ctx,
        )
        .ok()
    }
}

impl PackageResolver for PkgResolver<'_> {
    fn struct_name(&self, package_index: i32) -> Option<String> {
        if package_index > 0 {
            let ex = self.header.export_map.get(package_index as usize - 1)?;
            let object = self.names.get(ex.object_name.index() as usize)?;
            return Some(format!("{}.{object}", self.header.package_name()));
        }
        let oi = *self.header.import_map.get((-package_index - 1) as usize)?;
        match oi.kind() {
            FPackageObjectIndexType::ScriptImport => {
                Some(self.world.by_hash.get(&oi.raw_index())?.rsplit('.').next()?.to_string())
            }
            // Another package's export, addressed by its public hash. The
            // package name alone would be ambiguous.
            FPackageObjectIndexType::PackageImport => {
                let r = oi.package_import()?;
                let pkg =
                    self.header.imported_package_names.get(r.imported_package_index as usize)?;
                let hash = *self
                    .header
                    .imported_public_export_hashes
                    .get(r.imported_public_export_hash_index as usize)?;
                Some(format!("{pkg}#{hash:016x}"))
            }
            _ => None,
        }
    }

    fn struct_layout(&self, name: &str) -> Option<Vec<UsmapProperty>> {
        if let Some(hit) = self.layouts.borrow().get(name) {
            return hit.clone();
        }
        // Guard against a reference cycle: claim the slot before recursing.
        self.layouts.borrow_mut().insert(name.to_string(), None);

        let (pkg, want) = match name.split_once('#') {
            Some((pkg, hash)) => (pkg, Some(u64::from_str_radix(hash, 16).ok()?)),
            None => (name.rsplit_once('.')?.0, None),
        };
        let out = (|| {
            let (ai, exact) = self.world.by_pkg.get(&pkg.to_ascii_lowercase())?;
            let bytes = self.world.archives[*ai].read(exact).ok()?;
            let h = FZenPackageHeader::deserialize(
                &mut Cursor::new(&bytes),
                None,
                CE_TOC_VERSION,
                CE_HEADER_VERSION,
                None,
            )
            .ok()?;
            let names = h.name_map.copy_raw_names();
            let ex = match want {
                Some(hash) => h.export_map.iter().find(|x| x.public_export_hash == hash)?,
                None => {
                    let object = name.rsplit_once('.')?.1;
                    h.export_map.iter().find(|x| {
                        names.get(x.object_name.index() as usize).is_some_and(|n| n == object)
                    })?
                }
            };
            let inner = PkgResolver {
                world: self.world,
                layouts: Rc::clone(&self.layouts),
                header: &h,
                bytes: &bytes,
                names: &names,
            };
            inner.layout_of_export(ex)
        })();
        self.layouts.borrow_mut().insert(name.to_string(), out.clone());
        out
    }
}
