//! Gate: can every *export* in the shipped corpus be taken apart and put back
//! together byte-exactly?
//!
//! `ce_block_roundtrip` proves the property block. This proves the whole
//! serial range: block, `UObject` trailer, and the natively serialized tail
//! that follows. The tail is still bytes — modeling it is Phase 4 — so what
//! this actually measures is that the *decomposition* is exact and that nothing
//! is lost at the seams between the three parts.
//!
//! That makes it a weaker claim than the block gate on its own, and a
//! deliberately useful one: it is the harness every future tail model gets
//! checked against. Convert one class from a span to a model, re-run this, and
//! the count says whether the model is lossless against the bytes it replaced.
//!
//! Run: `ce_export_roundtrip [usmap-path]`
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export_in, read_userdefined_struct_layout, write_export_in, ExportContext,
    PackageResolver,
    Trailer,
};
use blam_tags::iostore::package::ue_types::FPackageObjectIndexType;
use blam_tags::iostore::usmap::UsmapProperty;
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
    let archives: Vec<IoStoreArchive> =
        utocs.iter().filter_map(|u| IoStoreArchive::open(u).ok()).collect();

    // A property bag names its members' struct types by object reference, and a
    // user-defined struct has no `.usmap` schema at all, so reading either needs
    // the whole mount rather than one package.
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

    let (mut total, mut same, mut unreadable, mut unwritable) = (0usize, 0usize, 0usize, 0usize);
    let mut differ_by_class: BTreeMap<String, usize> = BTreeMap::new();
    let mut trailers: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut tail_bytes = 0u64;
    let mut samples: Vec<String> = Vec::new();

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
            for ex in &h.export_map {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                let off = h.summary.header_size as usize + ex.cooked_serial_offset as usize;
                let end = (off + ex.cooked_serial_size as usize).min(b.len());
                if off >= b.len() || off > end {
                    continue;
                }
                let body = &b[off..end];
                let Ok(parts) = read_export_in(body, &names, &usmap, short, ex.object_flags, &ctx) else {
                    unreadable += 1;
                    continue;
                };
                total += 1;
                tail_bytes += parts.tail.len() as u64;
                *trailers
                    .entry(match parts.trailer {
                        Trailer::Absent => "absent",
                        Trailer::NoGuid => "no-guid",
                        Trailer::Guid(_) => "guid",
                    })
                    .or_default() += 1;
                match write_export_in(short, &parts, &usmap, Some(&resolver)) {
                    Ok(out) if out == body => same += 1,
                    Ok(out) => {
                        *differ_by_class.entry(short.to_string()).or_default() += 1;
                        if samples.len() < 8 {
                            let at = out
                                .iter()
                                .zip(body)
                                .position(|(x, y)| x != y)
                                .unwrap_or(out.len().min(body.len()));
                            samples.push(format!(
                                "{} :: {short}\n    {} bytes in, {} out, first difference at {at}",
                                h.package_name(),
                                body.len(),
                                out.len()
                            ));
                        }
                    }
                    Err(err) => {
                        unwritable += 1;
                        if samples.len() < 8 {
                            samples.push(format!("{short}: {:#}", err));
                        }
                    }
                }
            }
        }
    }

    println!("exports examined     {total}");
    println!("rebuilt exactly      {same} ({:.4}%)", 100.0 * same as f64 / total.max(1) as f64);
    println!("differ               {}", total - same - unwritable);
    println!("refused to write     {unwritable}");
    println!("unreadable (skipped) {unreadable}");
    println!("tail bytes retained  {tail_bytes} ({:.2} GiB, Phase 4)", tail_bytes as f64 / (1 << 30) as f64);
    println!("trailers             {trailers:?}");

    if !differ_by_class.is_empty() {
        println!("\ndiffering by class:");
        let mut v: Vec<_> = differ_by_class.iter().collect();
        v.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
        for (c, n) in v.iter().take(15) {
            println!("  {n:>7}  {c}");
        }
    }
    for s in &samples {
        println!("\n{s}");
    }
    if same != total {
        std::process::exit(1);
    }
}
