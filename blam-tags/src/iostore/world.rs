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
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;

use anyhow::{Context, Result};

use super::IoStoreArchive;
use super::container::pak::PakArchive;
use super::container_header::EIoContainerHeaderVersion;
use super::object::archive::PackageResolver;
use super::object::reflect::{read_userdefined_struct_layout, read_ustruct_layout};
use super::object::unversioned::ExportContext;
use super::package::ue_types::{FPackageObjectIndex, FPackageObjectIndexType};
use super::script_objects::ScriptObjects;
use super::ue_types::EIoStoreTocVersion;
use super::usmap::{Usmap, UsmapProperty};
use super::zen::{FExportMapEntry, FZenPackageHeader};

/// Container and header versions Campaign Evolved ships.
pub const CE_TOC_VERSION: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
/// See [`CE_TOC_VERSION`].
pub const CE_HEADER_VERSION: EIoContainerHeaderVersion =
    EIoContainerHeaderVersion::SoftPackageReferences;

fn collect_utocs(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_utocs(&path, out)?;
        } else if ty.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("utoc"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn collect_paks(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ty = entry.file_type()?;
        if ty.is_dir() {
            collect_paks(&path, out)?;
        } else if ty.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pak"))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn mount_key(path: &Path) -> (u32, u32, String) {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let patch = stem.ends_with("_p");
    let patch_version = if patch {
        stem.strip_suffix("_p")
            .and_then(|stem| stem.rsplit_once('_').map(|(_, tail)| tail))
            .and_then(|tail| tail.parse::<u32>().ok())
            .unwrap_or(1)
    } else {
        0
    };
    let score = if patch {
        4u32.saturating_add(100u32.saturating_mul(patch_version))
    } else {
        4
    };
    let chunk = stem
        .strip_prefix("pakchunk")
        .map(|tail| {
            tail.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
        })
        .filter(|digits| !digits.is_empty())
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or(u32::MAX);
    (
        score,
        chunk,
        path.to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase(),
    )
}

fn compare_mount_paths(a: &PathBuf, b: &PathBuf) -> Ordering {
    mount_key(a).cmp(&mount_key(b))
}

fn package_name_from_entry(path: &str) -> Option<(String, String)> {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let suffix = if lower.ends_with(".uasset") {
        ".uasset"
    } else if lower.ends_with(".umap") {
        ".umap"
    } else {
        return None;
    };
    let stem = &normalized[..normalized.len() - suffix.len()];
    let lower_stem = &lower[..lower.len() - suffix.len()];
    let content = lower_stem.find("/content/")?;
    let prefix = &stem[..content];
    let rest = &stem[content + "/content/".len()..];
    let project = prefix.rsplit('/').next().unwrap_or_default();
    let mount = if project.eq_ignore_ascii_case("meteorite") {
        "Game"
    } else {
        project
    };
    let package = format!("/{mount}/{rest}");
    Some((package.to_ascii_lowercase(), package))
}

/// One physical IoStore container in a mounted installation.
#[derive(Clone, Debug)]
pub struct WorldContainer {
    /// Stable index used by [`PackageProvider::container`].
    pub index: usize,
    /// Absolute path of the `.utoc`.
    pub path: PathBuf,
    /// Increasing mount order. A larger value wins a duplicate package path.
    pub read_order: u32,
    /// Whether this container's pathless directory index was reconstructed from
    /// its chunk ids, package headers and the other mounted containers.
    pub recovered_directory_index: bool,
    /// Number of package entries contributed by this container.
    pub package_count: usize,
}

/// One physical provider of a cooked package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageProvider {
    /// Index into [`World::containers`] and [`World::archives`].
    pub container: usize,
    /// Exact, case-preserving entry path inside the container.
    pub entry_path: String,
    /// This provider's increasing mount order. The largest value is active.
    pub read_order: u32,
}

/// A logical package and every mounted container that provides it.
#[derive(Clone, Debug)]
pub struct PackageRecord {
    /// Canonical Unreal package name, e.g. `/Game/Foo/Bar`.
    pub name: String,
    /// Providers in ascending mount order; the final entry is active.
    pub providers: Vec<PackageProvider>,
}

impl PackageRecord {
    /// The provider the game resolves after applying mount priority.
    pub fn active_provider(&self) -> Option<&PackageProvider> {
        self.providers.last()
    }
}

/// A non-fatal problem encountered while building a [`World`].
#[derive(Clone, Debug)]
pub struct WorldDiagnostic {
    pub path: PathBuf,
    pub message: String,
}

/// One legacy `.pak` container mounted beside the IoStore set.
#[derive(Clone, Debug)]
pub struct PakContainer {
    pub index: usize,
    pub path: PathBuf,
    pub read_order: u32,
    pub file_count: usize,
}

/// One physical provider of a file from a legacy `.pak`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PakProvider {
    pub container: usize,
    pub mounted_path: String,
    pub read_order: u32,
}

/// A logical legacy-pak file and all mounted providers for it.
#[derive(Clone, Debug)]
pub struct PakFileRecord {
    pub path: String,
    pub providers: Vec<PakProvider>,
}

impl PakFileRecord {
    pub fn active_provider(&self) -> Option<&PakProvider> {
        self.providers.last()
    }
}

/// Every `.utoc` in an installation, mounted, plus the reflection schema.
pub struct World {
    archives: Vec<IoStoreArchive>,
    containers: Vec<WorldContainer>,
    packages: Vec<PackageRecord>,
    /// Normalised package path (`/game/foo/bar`) -> [`Self::packages`] index.
    by_pkg: HashMap<String, usize>,
    /// Script-object hash -> full object path, for resolving class imports.
    by_hash: HashMap<u64, String>,
    usmap: Usmap,
    diagnostics: Vec<WorldDiagnostic>,
    pak_archives: Vec<Mutex<PakArchive>>,
    pak_containers: Vec<PakContainer>,
    pak_files: Vec<PakFileRecord>,
    by_pak_file: HashMap<String, usize>,
}

impl World {
    /// Mount every `.utoc` recursively beneath `paks_dir` (except
    /// `global.utoc`, which supplies the script objects) against `usmap`.
    ///
    /// Container and script-object failures are retained as diagnostics so a
    /// browser can present a partial mount. The call only fails when the root
    /// cannot be traversed or no IoStore container can be opened.
    pub fn open(paks_dir: impl AsRef<Path>, usmap: Usmap) -> Result<Self> {
        let paks_dir = paks_dir.as_ref();
        let mut paths = Vec::new();
        collect_utocs(paks_dir, &mut paths)
            .with_context(|| format!("read {}", paks_dir.display()))?;
        if paths.is_empty() {
            anyhow::bail!("no .utoc containers found beneath {}", paks_dir.display());
        }

        let global = paths
            .iter()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.eq_ignore_ascii_case("global.utoc"))
            })
            .cloned();
        let mut by_hash = HashMap::new();
        let mut diagnostics = Vec::new();
        match global.as_ref() {
            Some(path) => match ScriptObjects::load(path) {
                Ok(so) => {
                    for e in so.entries() {
                        if let Some(p) = so.resolve(e.global_index.raw_index()) {
                            by_hash.insert(e.global_index.raw_index(), p.to_string());
                        }
                    }
                }
                Err(error) => diagnostics.push(WorldDiagnostic {
                    path: path.clone(),
                    message: format!("could not load script objects: {error:#}"),
                }),
            },
            None => diagnostics.push(WorldDiagnostic {
                path: paks_dir.to_path_buf(),
                message: "global.utoc was not found; native class names may be unresolved"
                    .to_owned(),
            }),
        }

        paths.retain(|path| {
            !path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("global.utoc"))
        });
        paths.sort_by(compare_mount_paths);

        // Open all containers first. A pathless override needs the mounted base
        // set in hand before its chunk ids can be named.
        let mut opened: Vec<(PathBuf, Option<IoStoreArchive>)> = Vec::new();
        for path in paths {
            match IoStoreArchive::open(&path) {
                Ok(archive) => opened.push((path, Some(archive))),
                Err(error) => diagnostics.push(WorldDiagnostic {
                    path,
                    message: error.to_string(),
                }),
            }
        }
        if opened.is_empty() {
            anyhow::bail!(
                "no readable IoStore containers beneath {}",
                paks_dir.display()
            );
        }

        let needs_recovery: Vec<bool> = opened
            .iter()
            .map(|(_, archive)| {
                archive
                    .as_ref()
                    .is_some_and(|archive| archive.entries().is_empty())
            })
            .collect();
        for index in 0..opened.len() {
            if !needs_recovery[index] {
                continue;
            }
            let Some(mut archive) = opened[index].1.take() else {
                continue;
            };
            let bases: Vec<&IoStoreArchive> = opened
                .iter()
                .filter_map(|(_, archive)| archive.as_ref())
                .collect();
            let recovered = archive.recover_entries(&bases, Some("Meteorite/Content/"));
            if recovered == 0 {
                diagnostics.push(WorldDiagnostic {
                    path: opened[index].0.clone(),
                    message:
                        "container has no directory index and its chunk paths could not be recovered"
                            .to_owned(),
                });
            }
            opened[index].1 = Some(archive);
        }

        let mut archives = Vec::with_capacity(opened.len());
        let mut containers = Vec::with_capacity(opened.len());
        for (read_order, (path, archive)) in opened.into_iter().enumerate() {
            let Some(archive) = archive else {
                continue;
            };
            let index = archives.len();
            archives.push(archive);
            containers.push(WorldContainer {
                index,
                path,
                read_order: read_order as u32,
                recovered_directory_index: needs_recovery.get(read_order).copied().unwrap_or(false),
                package_count: 0,
            });
        }

        let mut provider_map: BTreeMap<String, (String, Vec<PackageProvider>)> = BTreeMap::new();
        for (container, archive) in archives.iter().enumerate() {
            let read_order = containers[container].read_order;
            for entry in archive.entries() {
                let Some((key, package)) = package_name_from_entry(&entry.path) else {
                    continue;
                };
                containers[container].package_count += 1;
                provider_map
                    .entry(key)
                    .or_insert_with(|| (package, Vec::new()))
                    .1
                    .push(PackageProvider {
                        container,
                        entry_path: entry.path.clone(),
                        read_order,
                    });
            }
        }

        let mut packages = Vec::with_capacity(provider_map.len());
        let mut by_pkg = HashMap::with_capacity(provider_map.len());
        for (key, (name, providers)) in provider_map {
            by_pkg.insert(key, packages.len());
            packages.push(PackageRecord { name, providers });
        }

        let mut pak_paths = Vec::new();
        collect_paks(paks_dir, &mut pak_paths)
            .with_context(|| format!("read {}", paks_dir.display()))?;
        pak_paths.sort_by(compare_mount_paths);
        let mut pak_archives = Vec::new();
        let mut pak_containers = Vec::new();
        let mut pak_provider_map: BTreeMap<String, (String, Vec<PakProvider>)> = BTreeMap::new();
        for (read_order, path) in pak_paths.into_iter().enumerate() {
            match PakArchive::open(&path) {
                Ok(archive) => {
                    let index = pak_archives.len();
                    for file in archive.files() {
                        let key = file.mounted_path.to_ascii_lowercase();
                        pak_provider_map
                            .entry(key)
                            .or_insert_with(|| (file.mounted_path.clone(), Vec::new()))
                            .1
                            .push(PakProvider {
                                container: index,
                                mounted_path: file.mounted_path.clone(),
                                read_order: read_order as u32,
                            });
                    }
                    pak_containers.push(PakContainer {
                        index,
                        path,
                        read_order: read_order as u32,
                        file_count: archive.files().len(),
                    });
                    pak_archives.push(Mutex::new(archive));
                }
                Err(error) => diagnostics.push(WorldDiagnostic {
                    path,
                    message: format!("could not open legacy pak: {error}"),
                }),
            }
        }
        let mut pak_files = Vec::with_capacity(pak_provider_map.len());
        let mut by_pak_file = HashMap::with_capacity(pak_provider_map.len());
        for (key, (path, providers)) in pak_provider_map {
            by_pak_file.insert(key, pak_files.len());
            pak_files.push(PakFileRecord { path, providers });
        }

        Ok(World {
            archives,
            containers,
            packages,
            by_pkg,
            by_hash,
            usmap,
            diagnostics,
            pak_archives,
            pak_containers,
            pak_files,
            by_pak_file,
        })
    }

    pub fn usmap(&self) -> &Usmap {
        &self.usmap
    }

    pub fn archives(&self) -> &[IoStoreArchive] {
        &self.archives
    }

    /// Physical containers in increasing mount order.
    pub fn containers(&self) -> &[WorldContainer] {
        &self.containers
    }

    /// Logical packages sorted by normalized package name.
    pub fn packages(&self) -> &[PackageRecord] {
        &self.packages
    }

    /// Non-fatal mount errors and compatibility omissions.
    pub fn diagnostics(&self) -> &[WorldDiagnostic] {
        &self.diagnostics
    }

    /// Successfully opened legacy `.pak` containers in increasing mount order.
    pub fn pak_containers(&self) -> &[PakContainer] {
        &self.pak_containers
    }

    /// Legacy-pak files sorted by normalized mounted path.
    pub fn pak_files(&self) -> &[PakFileRecord] {
        &self.pak_files
    }

    /// Resolve a mounted legacy-pak path case-insensitively.
    pub fn pak_file(&self, path: &str) -> Option<&PakFileRecord> {
        let index = *self
            .by_pak_file
            .get(&path.replace('\\', "/").to_ascii_lowercase())?;
        self.pak_files.get(index)
    }

    /// Read the active provider of a legacy-pak file.
    pub fn read_pak_file(&self, path: &str) -> Result<Vec<u8>> {
        let record = self
            .pak_file(path)
            .with_context(|| format!("legacy-pak file {path} is not mounted"))?;
        let provider = record
            .active_provider()
            .with_context(|| format!("legacy-pak file {path} has no providers"))?;
        self.read_pak_provider(provider)
    }

    /// Read one explicit provider of a legacy-pak file.
    pub fn read_pak_provider(&self, provider: &PakProvider) -> Result<Vec<u8>> {
        let archive = self
            .pak_archives
            .get(provider.container)
            .with_context(|| format!("legacy pak {} is not mounted", provider.container))?;
        archive
            .lock()
            .map_err(|_| anyhow::anyhow!("legacy pak lock is poisoned"))?
            .read(&provider.mounted_path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("read {}", provider.mounted_path))
    }

    /// Resolve a package name case-insensitively.
    pub fn package(&self, package: &str) -> Option<&PackageRecord> {
        let index = *self
            .by_pkg
            .get(&package.replace('\\', "/").to_ascii_lowercase())?;
        self.packages.get(index)
    }

    /// Read the active provider of a package.
    pub fn read_package(&self, package: &str) -> Result<Vec<u8>> {
        let record = self
            .package(package)
            .with_context(|| format!("package {package} is not mounted"))?;
        let provider = record
            .active_provider()
            .with_context(|| format!("package {package} has no providers"))?;
        self.read_provider(provider)
    }

    /// Read one explicit provider, including a shadowed version.
    pub fn read_provider(&self, provider: &PackageProvider) -> Result<Vec<u8>> {
        let archive = self
            .archives
            .get(provider.container)
            .with_context(|| format!("container {} is not mounted", provider.container))?;
        archive
            .read(&provider.entry_path)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("read {}", provider.entry_path))
    }

    /// The class path behind a script-object hash, e.g. an export's
    /// `class_index`. `None` for a hash this mount does not define.
    pub fn class_path(&self, hash: u64) -> Option<&str> {
        self.by_hash.get(&hash).map(String::as_str)
    }

    /// As [`World::class_path`], reduced to the bare class name the `.usmap`
    /// keys by.
    pub fn class_name(&self, hash: u64) -> Option<&str> {
        self.class_path(hash)
            .map(|p| p.rsplit('.').next().unwrap_or(p))
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
    pub fn class_key(
        &self,
        header: &FZenPackageHeader,
        class: FPackageObjectIndex,
    ) -> Option<String> {
        match class.kind() {
            FPackageObjectIndexType::ScriptImport => {
                Some(self.class_name(class.raw_index())?.to_string())
            }
            FPackageObjectIndexType::PackageImport => {
                let r = class.package_import()?;
                let pkg = header
                    .imported_package_names
                    .get(r.imported_package_index as usize)?;
                let hash = *header
                    .imported_public_export_hashes
                    .get(r.imported_public_export_hash_index as usize)?;
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
                let Ok(b) = self.archives[ai].read(&path) else {
                    continue;
                };
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
                let bulk: Vec<(i64, i64)> = h
                    .bulk_data
                    .iter()
                    .map(|x| (x.serial_offset, x.serial_size))
                    .collect();
                let ctx = ExportContext {
                    bulk_data: &bulk,
                    resolver: Some(&resolver),
                };
                for ex in &h.export_map {
                    let Some(class) = self.class_name(ex.class_index.raw_index()) else {
                        continue;
                    };
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
                    let Some(object) = names.get(ex.object_name.index() as usize) else {
                        continue;
                    };
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
            self.usmap
                .register_struct(&hash_key, super_name.clone(), props.clone());
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
        PkgResolver {
            world: self,
            layouts: Rc::default(),
            header,
            bytes,
            names,
        }
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
        let ctx = ExportContext {
            bulk_data: &[],
            resolver: Some(self),
        };
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
            FPackageObjectIndexType::ScriptImport => Some(
                self.world
                    .by_hash
                    .get(&oi.raw_index())?
                    .rsplit('.')
                    .next()?
                    .to_string(),
            ),
            // Another package's export, addressed by its public hash. The
            // package name alone would be ambiguous.
            FPackageObjectIndexType::PackageImport => {
                let r = oi.package_import()?;
                let pkg = self
                    .header
                    .imported_package_names
                    .get(r.imported_package_index as usize)?;
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
            let record = self.world.package(pkg)?;
            let provider = record.active_provider()?;
            let bytes = self.world.archives[provider.container]
                .read(&provider.entry_path)
                .ok()?;
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
                        names
                            .get(x.object_name.index() as usize)
                            .is_some_and(|n| n == object)
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
        self.layouts
            .borrow_mut()
            .insert(name.to_string(), out.clone());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_paths_keep_case_and_map_meteorite_to_game() {
        assert_eq!(
            package_name_from_entry("Meteorite/Content/UI/Data/MyTable.uasset"),
            Some((
                "/game/ui/data/mytable".to_owned(),
                "/Game/UI/Data/MyTable".to_owned()
            ))
        );
        assert_eq!(
            package_name_from_entry("Engine/Content/Maps/Probe.umap"),
            Some((
                "/engine/maps/probe".to_owned(),
                "/Engine/Maps/Probe".to_owned()
            ))
        );
        assert_eq!(package_name_from_entry("Meteorite/Content/Foo.ubulk"), None);
    }

    #[test]
    fn mount_order_puts_chunks_before_priority_patches() {
        let mut paths = vec![
            PathBuf::from("Paks/~mods/MyMod_2_P.utoc"),
            PathBuf::from("Paks/pakchunk10-WinGDK.utoc"),
            PathBuf::from("Paks/pakchunk2-WinGDK.utoc"),
            PathBuf::from("Paks/~mods/MyMod_P.utoc"),
        ];
        paths.sort_by(compare_mount_paths);
        let names: Vec<&str> = paths
            .iter()
            .filter_map(|path| path.file_name()?.to_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "pakchunk2-WinGDK.utoc",
                "pakchunk10-WinGDK.utoc",
                "MyMod_P.utoc",
                "MyMod_2_P.utoc"
            ]
        );
    }

    #[test]
    fn the_last_provider_is_active() {
        let record = PackageRecord {
            name: "/Game/Probe".to_owned(),
            providers: vec![
                PackageProvider {
                    container: 0,
                    entry_path: "base.uasset".to_owned(),
                    read_order: 0,
                },
                PackageProvider {
                    container: 1,
                    entry_path: "patch.uasset".to_owned(),
                    read_order: 1,
                },
            ],
        };
        assert_eq!(
            record.active_provider().map(|provider| provider.container),
            Some(1)
        );
    }

    #[test]
    fn the_last_legacy_pak_provider_is_active() {
        let record = PakFileRecord {
            path: "Meteorite/Content/WwiseAudio/probe.wem".to_owned(),
            providers: vec![
                PakProvider {
                    container: 0,
                    mounted_path: "Meteorite/Content/WwiseAudio/probe.wem".to_owned(),
                    read_order: 0,
                },
                PakProvider {
                    container: 2,
                    mounted_path: "Meteorite/Content/WwiseAudio/probe.wem".to_owned(),
                    read_order: 2,
                },
            ],
        };
        assert_eq!(
            record.active_provider().map(|provider| provider.container),
            Some(2)
        );
    }
}
