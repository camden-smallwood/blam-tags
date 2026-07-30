//! Gate: `decode(encode(decode(x))) == decode(x)` for every export.
//!
//! This is the contract every model must meet, and the only one a lossy payload
//! can. Byte-identity is stronger and `ce_export_roundtrip` measures it, but BC
//! texture compression discards information by construction — decoding a mip and
//! re-compressing cannot reproduce the original blocks. What can always be
//! required is that the *data* survives: whatever could be read out originally
//! still reads out after writing.
//!
//! It is deliberately built before the models it will judge. Right now every
//! tail is a retained span, so this passes trivially — which is the point. It is
//! the harness that says whether each conversion preserved meaning, and a
//! conversion that breaks it is a model that lost something.
//!
//! Reports separately the exports that are byte-identical too, because a class
//! *without* a lossy payload should be, and one that quietly stops being is a
//! regression rather than an accepted cost.
//!
//! Run: `ce_semantic_roundtrip [usmap-path]`
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export_in, read_userdefined_struct_layout, write_export_in, ExportContext,
    PackageResolver,
};
use blam_tags::iostore::package::ue_types::FPackageObjectIndexType;
use blam_tags::iostore::usmap::UsmapProperty;
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// Everything mounted, shared by every package's resolver.
struct World<'a> {
    archives: &'a [IoStoreArchive],
    by_pkg: &'a HashMap<String, (usize, String)>,
    by_hash: &'a HashMap<u64, String>,
    usmap: &'a Usmap,
}

/// Resolves the struct references made by one package.
///
/// Supplying this is not an optional refinement of the measurement. A
/// `UDataTable` finds its row layout through it, and without one every data
/// table in the corpus reports its rows as an unmodeled tail — a property of
/// the harness, not of the codec. This census has now been wrong for exactly
/// that reason twice, once for the bulk-data map and once for this, so it now
/// runs the same resolver `ce_coverage_matrix` does rather than a reduced one.
struct PkgResolver<'a> {
    world: &'a World<'a>,
    layouts: &'a RefCell<HashMap<String, Option<Vec<UsmapProperty>>>>,
    header: &'a FZenPackageHeader,
    bytes: &'a [u8],
    names: &'a [String],
}

impl PkgResolver<'_> {
    fn layout_of_export(
        &self,
        ex: &blam_tags::iostore::zen::FExportMapEntry,
    ) -> Option<Vec<UsmapProperty>> {
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
            self.world.usmap,
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
            let (ai, exact) = self.world.by_pkg.get(&pkg.to_ascii_lowercase())?;
            let bytes = self.world.archives[*ai].read(exact).ok()?;
            let h = FZenPackageHeader::deserialize(&mut Cursor::new(&bytes), None, CV, HV, None)
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
                layouts: self.layouts,
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

fn main() {
    let usmap_path = std::env::args().nth(1).unwrap_or_else(|| {
        "/Users/camden/Downloads/5.5.4-1097863+++Meteorite+Rel-i343-Meteorite-2606-CU2-Meteorite.usmap".into()
    });
    let mut usmap = match std::fs::read(&usmap_path) {
        Ok(b) => Usmap::parse(&b).expect("parse usmap"),
        Err(_) => Usmap::meteorite().expect("bundled usmap"),
    };
    blam_tags::iostore::usmap::register_editor_plugin_classes(&mut usmap);

    let mut by_hash: HashMap<u64, String> = HashMap::new();
    let so = ScriptObjects::load(format!("{PAKS}/global.utoc")).expect("script objects");
    for e in so.entries() {
        if let Some(p) = so.resolve(e.global_index.raw_index()) {
            by_hash.insert(e.global_index.raw_index(), p.to_string());
        }
    }

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .expect("read Paks")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    let (mut total, mut semantic_ok, mut byte_ok, mut failed) = (0u64, 0u64, 0u64, 0u64);
    // Classes that survive semantically but are no longer byte-identical. Each
    // one has to be a class that genuinely contains a lossy codec.
    let mut lossy: BTreeMap<String, u64> = BTreeMap::new();
    let mut broke: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();

    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();
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
    let world = World { archives: &archives, by_pkg: &by_pkg, by_hash: &by_hash, usmap: &usmap };

    for a in &archives {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase();
            if !lo.ends_with(".uasset") && !lo.ends_with(".umap") {
                continue;
            }
            let Ok(b) = a.read(&e.path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            let layouts = RefCell::new(HashMap::new());
            let resolver = PkgResolver {
                world: &world,
                layouts: &layouts,
                header: &h,
                bytes: &b,
                names: &names,
            };
            let ctx = ExportContext { bulk_data: &bulk, resolver: Some(&resolver) };
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let Ok(first) = read_export_in(&payloads[i], &names, &usmap, short, ex.object_flags, &ctx)
                else {
                    continue;
                };
                total += 1;

                let Ok(bytes) = write_export_in(short, &first, &usmap, Some(&resolver)) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    continue;
                };
                let Ok(second) = read_export_in(&bytes, &names, &usmap, short, ex.object_flags, &ctx) else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: re-reading our own output failed",
                            h.package_name()
                        ));
                    }
                    continue;
                };

                if first.semantic_eq(&second) {
                    semantic_ok += 1;
                    if bytes == payloads[i] {
                        byte_ok += 1;
                    } else {
                        *lossy.entry(short.to_string()).or_default() += 1;
                        if samples.len() < 4 {
                            let at = bytes
                                .iter()
                                .zip(&payloads[i])
                                .position(|(x, y)| x != y)
                                .unwrap_or(bytes.len().min(payloads[i].len()));
                            let lo = at.saturating_sub(12);
                            samples.push(format!(
                                "{} :: {short}[{i}] value-stable, bytes differ\n    {} in, {} out, first difference at {at}\n    orig {:02x?}\n    ours {:02x?}",
                                h.package_name(),
                                payloads[i].len(),
                                bytes.len(),
                                &payloads[i][lo..(at + 12).min(payloads[i].len())],
                                &bytes[lo..(at + 12).min(bytes.len())],
                            ));
                        }
                    }
                } else {
                    failed += 1;
                    *broke.entry(short.to_string()).or_default() += 1;
                    if samples.len() < 8 {
                        samples.push(format!(
                            "{} :: {short}[{i}]: value did not survive the round trip",
                            h.package_name()
                        ));
                    }
                }
            }
        }
    }

    println!("exports examined      {total}");
    println!(
        "semantically stable   {semantic_ok} ({:.4}%)   <- the contract",
        100.0 * semantic_ok as f64 / total.max(1) as f64
    );
    println!(
        "  also byte-identical {byte_ok} ({:.4}%)",
        100.0 * byte_ok as f64 / total.max(1) as f64
    );
    println!("  value-stable only   {}", semantic_ok - byte_ok);
    println!("broken                {failed}");

    if !lossy.is_empty() {
        println!("\nsemantically stable but no longer byte-identical:");
        println!("  (each must be a class that genuinely contains a lossy codec)");
        let mut v: Vec<_> = lossy.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    if !broke.is_empty() {
        println!("\nbroken by class:");
        let mut v: Vec<_> = broke.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(20) {
            println!("  {n:>8}  {c}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
