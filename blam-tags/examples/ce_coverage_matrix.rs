//! THE coverage measurement for the Campaign Evolved Unreal side.
//!
//! Every previous probe answered "does class X decode?" for a class picked by
//! hand. That can only ever report on the classes someone thought to ask about,
//! so it cannot answer "are we at 100%". This walks **every export of every
//! package** in the shipped paks, resolves each export's class, runs the
//! unversioned property reader against it, and tallies the result per class.
//!
//! For each class it reports:
//!   * exports seen, decoded, failed
//!   * where the property walk ended — `exact`/zero-`padded` means every byte of
//!     the export is accounted for; a non-zero `tail` means a natively
//!     serialized payload follows that this reader does not model
//!   * the most common failure message, so the next thing to fix is named
//!
//! and finishes with the two headline numbers: share of exports whose property
//! block decodes, and share whose bytes are *fully* accounted for.
//!
//! Run: ce_coverage_matrix [min-exports-to-list]
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex, FPackageObjectIndexType};
use blam_tags::iostore::unversioned::{
    read_export_with_trailer, read_userdefined_struct_layout, ExportContext, PackageResolver,
};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::usmap::{Usmap, UsmapProperty};
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const EXE: &str =
    "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Binaries/WinGDK/HaloCampaignEvolved.exe";
const UHT: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/UHTHeaderDump";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

#[derive(Default)]
struct Cov {
    seen: usize,
    decoded: usize,
    failed: usize,
    exact: usize,
    padded: usize,
    tail: usize,
    tail_bytes: u64,
    /// Whether the shipped executable registers this class at all.
    runtime: bool,
    /// Exports whose declared serial range lies outside the package chunk, so
    /// the reader never sees them.
    skipped: usize,
    errs: BTreeMap<String, usize>,
}

/// Everything mounted, shared by every package's resolver.
struct World<'a> {
    archives: &'a [IoStoreArchive],
    /// Package name (lower-case) → the container and exact path holding it.
    by_pkg: &'a HashMap<String, (usize, String)>,
    /// Script-import hash → `/Script/Module.Class`.
    by_hash: &'a HashMap<u64, String>,
    usmap: &'a Usmap,
}

/// Resolves the struct references made by one package.
///
/// A user-defined struct can hold another user-defined struct, in another
/// package again, so `struct_layout` re-enters through a fresh `PkgResolver`
/// for whichever package defines the nested type. Results are memoised because
/// a handful of lens-flare structs are referenced from many assets.
struct PkgResolver<'a> {
    world: &'a World<'a>,
    layouts: &'a RefCell<HashMap<String, Option<Vec<UsmapProperty>>>>,
    header: &'a FZenPackageHeader,
    bytes: &'a [u8],
    names: &'a [String],
}

impl PkgResolver<'_> {
    /// A name `struct_layout` can find a package-defined struct by: the
    /// defining package, then the object within it.
    fn qualified(pkg: &str, object: &str) -> String {
        format!("{pkg}.{object}")
    }

    fn layout_of_export(&self, ex: &blam_tags::iostore::zen::FExportMapEntry) -> Option<Vec<UsmapProperty>> {
        let off = self.header.summary.header_size as usize + ex.cooked_serial_offset as usize;
        let end = (off + ex.cooked_serial_size as usize).min(self.bytes.len());
        if off >= self.bytes.len() || off > end {
            return None;
        }
        let ctx = ExportContext { bulk_data: &[], resolver: Some(self) };
        read_userdefined_struct_layout(
            &self.bytes[off..end],
            self.names,
            self.world.usmap,
            ex.object_flags,
            &ctx,
        )
        .ok()
    }
}

impl PackageResolver for PkgResolver<'_> {
    fn struct_name(&self, package_index: i32) -> Option<String> {
        // Positive is an export of this very package (1-based); negative an
        // import; zero is null.
        if package_index > 0 {
            let ex = self.header.export_map.get(package_index as usize - 1)?;
            let object = self.names.get(ex.object_name.index() as usize)?;
            return Some(Self::qualified(&self.header.package_name(), object));
        }
        let oi = *self.header.import_map.get((-package_index - 1) as usize)?;
        match oi.kind() {
            // A native struct: the script-object table names it, the `.usmap`
            // has its layout.
            FPackageObjectIndexType::ScriptImport => {
                Some(self.world.by_hash.get(&oi.raw_index())?.rsplit('.').next()?.to_string())
            }
            // A struct cooked into another package. The import carries that
            // package's name and the hash of the exported object within it;
            // `struct_layout` turns the pair back into a layout.
            FPackageObjectIndexType::PackageImport => {
                let r = oi.package_import()?;
                let pkg = self.header.imported_package_names.get(r.imported_package_index as usize)?;
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
            let h =
                FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None).ok()?;
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
                layouts: self.layouts,
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

/// Collapse the varying parts of an error so the same defect groups together.
fn norm(e: &str) -> String {
    let mut out = String::new();
    let mut in_num = false;
    for c in e.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('N');
                in_num = true;
            }
        } else {
            in_num = false;
            out.push(c);
        }
    }
    out
}

fn main() {
    let min: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let show_class = std::env::var("BLAM_COV_SHOW").ok();
    let mut usmap = Usmap::parse(&std::fs::read(USMAP).unwrap()).unwrap();
    // Classes the shipped binary lacks entirely, so no dump of it can carry them.
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    // Which classes can the *shipped* game actually construct? UHT writes each
    // registered class's name into the binary, so a class name absent from the
    // executable is content the runtime could never load — cooked leftovers from
    // editor-only plugins. Coverage over those is meaningless; what matters is
    // coverage over the data the game uses.
    let exe_names: std::collections::HashSet<String> = match std::fs::read(EXE) {
        Ok(bytes) => {
            // UE stores most registered names as UTF-16 (`TEXT(...)`), so an
            // ASCII-only sweep misses them and wrongly brands live classes as
            // editor-only — `StaticMeshActor`, `DecalActor` and
            // `MaterialInstanceConstant` all appear *only* as UTF-16. Scan both.
            let mut out: std::collections::HashSet<String> = std::collections::HashSet::new();
            // The binary holds C++ names (`UNiagaraDataInterfaceCurve`) while the
            // package refers to the UObject name (`NiagaraDataInterfaceCurve`),
            // so record the token with its UE prefix stripped as well.
            fn add(out: &mut std::collections::HashSet<String>, cur: &[u8]) {
                if cur.len() < 3 {
                    return;
                }
                let t = String::from_utf8_lossy(cur).into_owned();
                if let Some(rest) = t.strip_prefix(['U', 'A', 'F', 'E']) {
                    if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                        out.insert(rest.to_string());
                    }
                }
                out.insert(t);
            }
            let mut cur: Vec<u8> = Vec::new();
            for &b in &bytes {
                if b.is_ascii_alphanumeric() || b == b'_' {
                    cur.push(b);
                } else {
                    add(&mut out, &cur);
                    cur.clear();
                }
            }
            add(&mut out, &cur);
            for start in 0..2usize {
                let mut cur: Vec<u8> = Vec::new();
                for pair in bytes[start.min(bytes.len())..].chunks_exact(2) {
                    if pair[1] == 0 && (pair[0].is_ascii_alphanumeric() || pair[0] == b'_') {
                        cur.push(pair[0]);
                    } else {
                        add(&mut out, &cur);
                        cur.clear();
                    }
                }
                add(&mut out, &cur);
            }
            out
        }
        Err(e) => {
            eprintln!("WARNING: cannot read exe ({e}); treating all classes as runtime");
            std::collections::HashSet::new()
        }
    };
    eprintln!("identifiers in executable: {}", exe_names.len());

    // Native class hash -> "/Script/Module.Class". The global container's
    // script-object table is authoritative and complete (every entry
    // round-trips through the import hash), so it replaces guessing candidate
    // paths from the UHT dump; the dump is kept only as a fallback.
    let mut by_hash: HashMap<u64, String> = HashMap::new();
    match ScriptObjects::load(format!("{PAKS}/global.utoc")) {
        Ok(so) => {
            for e in so.entries() {
                if let Some(p) = so.resolve(e.global_index.raw_index()) {
                    by_hash.insert(e.global_index.raw_index(), p.to_string());
                }
            }
            eprintln!("script objects resolved: {}", by_hash.len());
        }
        Err(e) => eprintln!("WARNING: no script-object table ({e:#}); falling back to UHT names"),
    }
    for m in std::fs::read_dir(UHT).unwrap().filter_map(|e| e.ok()) {
        if !m.path().is_dir() {
            continue;
        }
        let module = m.file_name().to_string_lossy().to_string();
        for sub in ["Public", "Private", "Classes"] {
            let Ok(rd) = std::fs::read_dir(format!("{UHT}/{module}/{sub}")) else { continue };
            for f in rd.filter_map(|e| e.ok()) {
                let n = f.file_name().to_string_lossy().to_string();
                let Some(stem) = n.strip_suffix(".h") else { continue };
                let path = format!("/Script/{module}.{stem}");
                by_hash
                    .entry(FPackageObjectIndex::create_script_import(&path).raw_index())
                    .or_insert(path);
                // A header often declares classes that do not share its file
                // name, so filename-only mapping leaves thousands of exports
                // unnameable. Also register every `class [MODULE_API] UFoo`
                // declaration, minus UE's U/A/F prefix.
                let Ok(text) = std::fs::read_to_string(f.path()) else { continue };
                for line in text.lines() {
                    let t = line.trim_start();
                    let Some(rest) = t.strip_prefix("class ") else { continue };
                    let mut word = rest.split_whitespace().next().unwrap_or("");
                    if word.ends_with("_API") {
                        word = rest.split_whitespace().nth(1).unwrap_or("");
                    }
                    let word = word.trim_end_matches(&[':', ';', '{'][..]);
                    if word.len() < 2 || !word.starts_with(['U', 'A', 'F']) {
                        continue;
                    }
                    let bare = &word[1..];
                    if !bare.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                        continue;
                    }
                    let p = format!("/Script/{module}.{bare}");
                    by_hash
                        .entry(FPackageObjectIndex::create_script_import(&p).raw_index())
                        .or_insert(p);
                }
            }
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    // Open every container up front and index its entries by path. A
    // `UDataTable`'s rows are laid out by a `RowStruct` that often lives in a
    // *different* package, so decoding one means reaching sideways into another
    // container — which needs all of them mounted at once. Mounting is just an
    // index parse (no decompression), so this is cheap.
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();
    // Index by *package name*, not by file path. A cooked path carries its UE
    // mount point: everything before `/content/` ends in the mount's name, and
    // `Meteorite` is this game's `/Game/`. Deriving `/Game/Foo` →
    // `Meteorite/Content/Foo.uasset` by hand looks right and silently misses
    // every plugin — the lens-flare structs live under
    // `Engine/Plugins/XGTG/XGTGLensFlares/Content/`, i.e. mount
    // `/XGTGLensFlares/`. Values keep the exact stored path, since IoStore
    // lookup is case-sensitive.
    let mut by_pkg: HashMap<String, (usize, String)> = HashMap::new();
    for (i, a) in archives.iter().enumerate() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase().replace('\\', "/");
            let Some(stem) = lo.strip_suffix(".uasset").or_else(|| lo.strip_suffix(".umap")) else {
                continue;
            };
            let Some((prefix, rest)) = stem.split_once("/content/") else { continue };
            let mount = match prefix.rsplit('/').next().unwrap_or("") {
                "meteorite" => "game",
                m => m,
            };
            by_pkg.entry(format!("/{mount}/{rest}")).or_insert((i, e.path.clone()));
        }
    }
    eprintln!("{} containers, {} packages indexed", archives.len(), by_pkg.len());

    let world = World { archives: &archives, by_pkg: &by_pkg, by_hash: &by_hash, usmap: &usmap };
    let layouts: RefCell<HashMap<String, Option<Vec<UsmapProperty>>>> = RefCell::new(HashMap::new());

    let mut cov: BTreeMap<String, Cov> = BTreeMap::new();
    let mut pkgs = 0usize;
    // `BLAM_COV_PKG=<substring>` restricts the sweep to matching packages. The
    // resolver-backed walk is the only view that decodes `UDataTable` and
    // friends correctly, but tracing it over 103k packages is unusable — this
    // makes `BLAM_UNVERSIONED_TRACE` tractable on a single asset.
    let only_pkg = std::env::var("BLAM_COV_PKG").ok().map(|s| s.to_ascii_lowercase());
    for a in &archives {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            if let Some(want) = &only_pkg {
                if !lo.contains(want.as_str()) {
                    continue;
                }
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            pkgs += 1;
            let names = h.name_map.copy_raw_names();
            // Bulk-data map entries, so tails that embed an inline payload can
            // look up its offset and size.
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|b| (b.serial_offset, b.serial_size)).collect();
            let resolver = PkgResolver {
                world: &world,
                layouts: &layouts,
                header: &h,
                bytes: &b,
                names: &names,
            };
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for ex in &h.export_map {
                // A Blueprint-generated class is defined by another package, so
                // there is no native schema to decode it against; count it
                // separately rather than pretending it is a decode failure.
                let class = match by_hash.get(&ex.class_index.raw_index()) {
                    Some(c) => c.clone(),
                    // A class that is not a script import is defined by content,
                    // not by the engine: either imported from another package or
                    // exported by this one (a Blueprint-generated class). Either
                    // way there is no native schema to decode it against, so it
                    // is a separate category rather than a decode failure.
                    None => match ex.class_index.kind() {
                        FPackageObjectIndexType::PackageImport => {
                            "<blueprint class from another package>".to_string()
                        }
                        FPackageObjectIndexType::Export => {
                            "<blueprint class defined in this package>".to_string()
                        }
                        FPackageObjectIndexType::Null => "<null class>".to_string(),
                        FPackageObjectIndexType::ScriptImport => {
                            "<unresolved script import>".to_string()
                        }
                    },
                };
                let c = cov.entry(class.clone()).or_default();
                c.seen += 1;
                if !class.starts_with("/Script/") {
                    continue;
                }
                let short = class.rsplit('.').next().unwrap();
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                // An export whose declared extent falls outside the chunk is
                // never attempted. Counting it as neither decoded nor failed
                // would quietly inflate the coverage percentage, so track it.
                if off >= b.len() || off > end {
                    c.skipped += 1;
                    continue;
                }
                let body = &b[off..end];
                // `BLAM_COV_SHOW=<Class>` narrates the exports of one class that
                // are still short. Unlike `ce_tail_probe` this runs with the
                // full package context, so it is the only view that is honest
                // about classes whose tail needs a resolver (`UDataTable`).
                let show = show_class.as_deref() == Some(short);
                match read_export_with_trailer(body, &names, &usmap, short, ex.object_flags, &ctx) {
                    Ok((_, used)) => {
                        c.decoded += 1;
                        // Normally only short exports are worth showing, but
                        // diagnosing a desync means comparing a broken export
                        // against a working one — `BLAM_COV_ALL=1` dumps every
                        // export of the class, not just the short ones.
                        let all = std::env::var("BLAM_COV_ALL").is_ok();
                        if show && (all || (used < body.len() && body[used..].iter().any(|&x| x != 0)))
                        {
                            println!(
                                "\n=== {} :: {} ({} bytes) — consumed {used}, {} left",
                                h.package_name(),
                                names.get(ex.object_name.index() as usize).map(String::as_str).unwrap_or("?"),
                                body.len(),
                                body.len() - used
                            );
                            // `BLAM_COV_FULL=1` dumps the export from byte zero
                            // instead of from where the walk stopped. A tail is
                            // only ever the *symptom*; when the desync is inside
                            // the property block the bytes that explain it sit
                            // before `used`, not after.
                            let from =
                                if std::env::var("BLAM_COV_FULL").is_ok() { 0 } else { used };
                            for (i, ch) in body[from..].chunks(16).take(16).enumerate() {
                                print!("  {:04x}: ", from + i * 16);
                                for x in ch {
                                    print!("{x:02x} ");
                                }
                                println!();
                            }
                            // For a `UStruct`-derived export the schema it wrote
                            // its own default instance against is the thing to
                            // look at, so show it beside the bytes.
                            if let Ok(fields) = read_userdefined_struct_layout(
                                body,
                                &names,
                                &usmap,
                                ex.object_flags,
                                &ctx,
                            ) {
                                for f in &fields {
                                    println!("    [{}] {} : {:?}", f.schema_index, f.name, f.ty);
                                }
                            }
                        }
                        // `read_export_with_trailer` already consumed UObject's
                        // trailer and each class's native tail, so whatever is
                        // left really is unmodeled payload.
                        let t = &body[used.min(body.len())..];
                        if t.is_empty() {
                            c.exact += 1;
                        } else if t.iter().all(|&x| x == 0) {
                            c.padded += 1;
                        } else {
                            c.tail += 1;
                            c.tail_bytes += t.len() as u64;
                        }
                    }
                    Err(e) => {
                        c.failed += 1;
                        if show {
                            println!("\n=== {} ({} bytes) — FAILED: {e:#}", h.package_name(), body.len());
                            if std::env::var("BLAM_COV_FULL").is_ok() {
                                for (i, ch) in body.chunks(16).take(48).enumerate() {
                                    print!("  {:04x}: ", i * 16);
                                    for x in ch {
                                        print!("{x:02x} ");
                                    }
                                    println!();
                                }
                            }
                        }
                        *c.errs.entry(norm(&format!("{e:#}"))).or_default() += 1;
                    }
                }
            }
        }
    }

    // Classify each class once. A UE name shows up in the binary inside longer
    // identifiers (`NiagaraScript` only ever appears within `NiagaraScriptSource`),
    // so an exact token match produces false "editor-only" verdicts — test for
    // containment instead.
    let class_names: Vec<String> = cov.keys().cloned().collect();
    for name in &class_names {
        let short = name.rsplit(['.', ':']).next().unwrap_or(name).to_string();
        let runtime = exe_names.is_empty()
            || !name.starts_with("/Script/")
            || exe_names.iter().any(|t| t.contains(&short));
        if let Some(c) = cov.get_mut(name) {
            c.runtime = runtime;
        }
    }

    let mut rows: Vec<_> = cov.iter().collect();
    rows.sort_by_key(|(_, c)| std::cmp::Reverse(c.seen));
    println!("{pkgs} packages\n");
    println!("{:<52} {:>8} {:>8} {:>8} {:>9} {:>8} {:>10}", "class", "exports", "decoded", "failed", "complete", "tail", "avg tail");
    for (name, c) in &rows {
        if c.seen < min {
            continue;
        }
        println!(
            "{:<52} {:>8} {:>8} {:>8} {:>9} {:>8} {:>10}",
            name.strip_prefix("/Script/").unwrap_or(name),
            c.seen,
            c.decoded,
            c.failed,
            c.exact + c.padded,
            c.tail,
            if c.tail > 0 { c.tail_bytes / c.tail as u64 } else { 0 }
        );
        if let Some((e, n)) = c.errs.iter().max_by_key(|(_, n)| **n) {
            println!("      └ {n}× {e}");
        }
    }

    let editor_only: Vec<_> =
        rows.iter().filter(|(n, c)| n.starts_with("/Script/") && !c.runtime).collect();
    let eo_exports: usize = editor_only.iter().map(|(_, c)| c.seen).sum();
    if !editor_only.is_empty() {
        println!("\neditor-only classes (absent from the shipped executable):");
        for (n, c) in &editor_only {
            println!("  {:>7}  {}", c.seen, n.strip_prefix("/Script/").unwrap_or(n));
        }
        println!("  = {eo_exports} exports the runtime could never construct");
    }

    let native: Vec<_> =
        rows.iter().filter(|(n, c)| n.starts_with("/Script/") && c.runtime).collect();
    let seen: usize = native.iter().map(|(_, c)| c.seen).sum();
    let skipped: usize = native.iter().map(|(_, c)| c.skipped).sum();
    let dec: usize = native.iter().map(|(_, c)| c.decoded).sum();
    let whole: usize = native.iter().map(|(_, c)| c.exact + c.padded).sum();
    let bp: usize = rows.iter().filter(|(n, _)| !n.starts_with("/Script/")).map(|(_, c)| c.seen).sum();
    println!("\nRUNTIME native-class exports   {seen}  (editor-only excluded: {eo_exports})");
    println!("  never attempted         {skipped} (serial range outside the chunk)");
    println!(
        "  decode failures         {}",
        seen - skipped - dec
    );
    println!("  property block decoded  {dec} ({:.4}%)", 100.0 * dec as f64 / seen as f64);
    println!("  every byte accounted    {whole} ({:.2}%)", 100.0 * whole as f64 / seen as f64);
    println!("blueprint/unknown-class exports {bp} (no native schema; decoded via their class package)");
}
