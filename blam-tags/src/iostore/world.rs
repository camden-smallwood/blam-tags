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
use super::object::reflect::{read_userdefined_struct_layout, read_ustruct_layout};
use super::object::unversioned::ExportContext;
use super::package::ue_types::{FPackageObjectIndex, FPackageObjectIndexType};
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

    /// The `.usmap` key for an export's class.
    ///
    /// An export names its class with an `FPackageObjectIndex`, and only the
    /// `ScriptImport` case is a native class the `.usmap` knows by name. A
    /// Blueprint-generated class is a *`PackageImport`* — another package's
    /// export, addressed by public hash — or, when the class lives in the same
    /// package, an `Export`. Every gate resolved only the first and dropped the
    /// rest: 89,762 of the corpus's 1,243,749 exports, never read or counted.
    ///
    /// The synthetic `pkg#hash` and `pkg.object` forms match what
    /// [`PkgResolver::struct_name`] produces, so a class registered under one
    /// is found through the other.
    pub fn class_key(&self, header: &FZenPackageHeader, class: FPackageObjectIndex) -> Option<String> {
        match class.kind() {
            FPackageObjectIndexType::ScriptImport => {
                Some(self.class_name(class.raw_index())?.to_string())
            }
            FPackageObjectIndexType::PackageImport => {
                let r = class.package_import()?;
                let pkg = header.imported_package_names.get(r.imported_package_index as usize)?;
                let hash =
                    *header.imported_public_export_hashes.get(r.imported_public_export_hash_index as usize)?;
                Some(format!("{pkg}#{hash:016x}"))
            }
            FPackageObjectIndexType::Export => {
                let ex = header.export_map.get(class.raw_index() as usize)?;
                let names = header.name_map.copy_raw_names();
                let object = names.get(ex.object_name.index() as usize)?;
                Some(format!("{}.{object}", header.package_name()))
            }
            _ => None,
        }
    }

    /// Recover every Blueprint-generated class in the mount and register it in
    /// the `.usmap`, so exports of those classes decode like any other.
    ///
    /// A `UBlueprintGeneratedClass` export declares the properties the
    /// Blueprint adds; the rest of its flattened schema is its parent's, which
    /// `Usmap`'s super chain supplies once the parent is registered too — the
    /// parent may itself be another generated class, which is why every one is
    /// registered before any is used.
    ///
    /// Each class is registered under **both** keys [`World::class_key`] can
    /// produce, because an importer addresses it by public hash while the
    /// declaring package names it by object.
    ///
    /// Returns `(classes registered, exports that failed to yield a layout)`.
    pub fn register_generated_classes(&mut self) -> (usize, usize) {
        let mut found: Vec<(String, String, Option<String>, Vec<UsmapProperty>)> = Vec::new();
        let mut failed = 0usize;
        for ai in 0..self.archives.len() {
            let entries: Vec<String> = self.archives[ai]
                .entries()
                .iter()
                .map(|e| e.path.clone())
                .filter(|p| {
                    let lo = p.to_ascii_lowercase();
                    lo.ends_with(".uasset") || lo.ends_with(".umap")
                })
                .collect();
            for path in entries {
                let Ok(b) = self.archives[ai].read(&path) else { continue };
                let Ok(h) = FZenPackageHeader::deserialize(
                    &mut Cursor::new(&b),
                    None,
                    CE_TOC_VERSION,
                    CE_HEADER_VERSION,
                    None,
                ) else {
                    continue;
                };
                let names = h.name_map.copy_raw_names();
                let resolver = self.resolver(&h, &b, &names);
                let bulk: Vec<(i64, i64)> =
                    h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
                let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
                for ex in &h.export_map {
                    let Some(class) = self.class_name(ex.class_index.raw_index()) else { continue };
                    // Any export that *is* a class serializes `UStruct`'s
                    // prefix, so any of them can yield a layout. Testing the
                    // name for `GeneratedClass` looked equivalent and was not:
                    // it missed `RigVMMemoryStorageGeneratorClass`, whose two
                    // exports account for 8,273 of the 83,641 bytes that were
                    // still untyped. Ask the schema whether it derives from
                    // `UClass` instead of pattern-matching its name.
                    if !self.derives_from(class, "Class") {
                        continue;
                    }
                    let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                    let end = (off + ex.cooked_serial_size as usize).min(b.len());
                    if off >= b.len() || off > end {
                        continue;
                    }
                    let Some(object) = names.get(ex.object_name.index() as usize) else { continue };
                    match read_ustruct_layout(
                        &b[off..end],
                        &names,
                        &self.usmap,
                        class,
                        ex.object_flags,
                        &ctx,
                    ) {
                        Ok((super_index, props)) => found.push((
                            format!("{}#{:016x}", h.package_name(), ex.public_export_hash),
                            format!("{}.{object}", h.package_name()),
                            resolver.struct_name(super_index),
                            props,
                        )),
                        Err(_) => failed += 1,
                    }
                }
            }
        }
        let n = found.len();
        for (hash_key, path_key, super_name, props) in found {
            self.usmap.register_struct(&hash_key, super_name.clone(), props.clone());
            self.usmap.register_struct(&path_key, super_name, props);
        }
        (n, failed)
    }

    /// Whether `class` is `base` or has it in its `.usmap` super chain.
    pub fn derives_from(&self, class: &str, base: &str) -> bool {
        let mut cur = class;
        for _ in 0..64 {
            if cur == base {
                return true;
            }
            match self.usmap.get(cur).and_then(|s| s.super_name.as_deref()) {
                Some(s) => cur = s,
                None => return false,
            }
        }
        false
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
