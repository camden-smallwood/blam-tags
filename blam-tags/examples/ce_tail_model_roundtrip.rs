//! Gate: does a *modeled* class tail reproduce the bytes it replaces?
//!
//! This is the property that makes Phase 4 safe to do incrementally. A tail
//! model is normally something you have to trust — it is a reading of engine
//! source against a binary blob, and a subtly wrong one produces plausible
//! values. Here the bytes are already known to be correct, because the export
//! round-trip copies them through verbatim. So converting a class from a
//! retained span into a model is checkable *against the span*: decode the tail,
//! re-emit it, and require the two to be identical.
//!
//! A class only appears here once it has a model, so the count grows as Phase 4
//! proceeds and never has to be taken on faith.
//!
//! Run: `ce_tail_model_roundtrip [usmap-path]`
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export, read_userdefined_struct_layout, roundtrip_tail, ExportContext, PackageResolver,
    TailContext,
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

    // class -> (seen, exact, failed, bytes modeled)
    let mut stats: BTreeMap<String, (u64, u64, u64, u64)> = BTreeMap::new();
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
            // Texture mips reference their payloads through the package's
            // bulk-data map, so the model needs it as context.
            let bulk: Vec<(i64, i64)> =
                h.bulk_data.iter().map(|x| (x.serial_offset, x.serial_size)).collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                // Deliberately *not* filtered by `MODELED_TAILS`: families are
                // dispatched by inheritance chain, so most modeled classes are
                // never named in that list. `roundtrip_tail` returning `None` is
                // the authority on "no model yet".
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                if parts.tail.is_empty() {
                    continue;
                }
                let s = stats.entry(short.to_string()).or_default();
                s.0 += 1;
                let Some(block) = parts.block.as_ref() else { continue };
                let layouts = RefCell::new(HashMap::new());
                let resolver = PkgResolver {
                    world: &world,
                    layouts: &layouts,
                    header: &h,
                    bytes: &b,
                    names: &names,
                };
                let ctx = TailContext {
                    bulk_data: &bulk,
                    origin: payloads[i].len() - parts.tail.len(),
                    usmap: &usmap,
                    resolver: Some(&resolver),
                };
                match roundtrip_tail(short, &parts.tail, &names, block, ctx) {
                    Some(Ok(out)) if out == parts.tail => {
                        s.1 += 1;
                        s.3 += parts.tail.len() as u64;
                    }
                    Some(Ok(out)) => {
                        s.2 += 1;
                        if samples.len() < 6 {
                            let at = out
                                .iter()
                                .zip(&parts.tail)
                                .position(|(x, y)| x != y)
                                .unwrap_or(out.len().min(parts.tail.len()));
                            samples.push(format!(
                                "{} :: {short}[{i}] {} bytes in, {} out, first difference at {at}\n    orig {:02x?}\n    ours {:02x?}",
                                h.package_name(),
                                parts.tail.len(),
                                out.len(),
                                &parts.tail[at.saturating_sub(4)..(at + 8).min(parts.tail.len())],
                                &out[at.saturating_sub(4)..(at + 8).min(out.len())],
                            ));
                        }
                    }
                    Some(Err(err)) => {
                        s.2 += 1;
                        if samples.len() < 6 {
                            samples.push(format!("{} :: {short}[{i}]: {err:#}", h.package_name()));
                        }
                    }
                    None => {}
                }
            }
        }
    }

    println!("{:<32} {:>12} {:>12} {:>10} {:>12}", "class", "tails", "exact", "failed", "bytes");
    let (mut t, mut e, mut f, mut by) = (0u64, 0u64, 0u64, 0u64);
    // A class with no model at all reports zero exact and zero failed. Counting
    // those in the denominator makes the pass rate look like a failure rate, so
    // they are reported as their own bucket.
    let mut unmodeled = 0u64;
    let mut unmodeled_classes = 0u64;
    for (class, (seen, exact, failed, bytes)) in &stats {
        if *exact == 0 && *failed == 0 {
            unmodeled += seen;
            unmodeled_classes += 1;
            continue;
        }
        println!("{class:<44} {seen:>10} {exact:>10} {failed:>8} {bytes:>13}");
        t += seen;
        e += exact;
        f += failed;
        by += bytes;
    }
    println!(
        "\n{e} of {t} modeled tails exact ({:.4}%), {f} failed, {by} bytes now regenerated rather than retained\nno model yet: {unmodeled} tails across {unmodeled_classes} classes",
        100.0 * e as f64 / t.max(1) as f64
    );
    for s in &samples {
        println!("\n{s}");
    }
    if e != t || t == 0 {
        std::process::exit(1);
    }
}
