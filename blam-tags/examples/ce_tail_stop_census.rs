//! Where does native-tail modeling actually stop, and how much is behind it?
//!
//! The tail dispatcher has 49 sites that rewind and decline to continue. Each is
//! a deliberate boundary, but as a list of 49 they say nothing about which ones
//! matter: one that fires on every `StaticMesh` is the frontier, one that has
//! never fired is a hypothetical.
//!
//! Reports per stopping class: how many exports it stops, and how many bytes are
//! left unmodeled behind it. That ordering is what says where Phase 4 work is
//! worth doing — and what says which of the 49 could simply be deleted.
//!
//! Run: `ce_tail_stop_census [usmap-path]`
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::IoStoreArchive;
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    ExportContext, PackageResolver, read_userdefined_struct_layout, walk_export,
};
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::package::ue_types::FPackageObjectIndexType;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::usmap::UsmapProperty;
use blam_tags::iostore::zen::FZenPackageHeader;

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
        .filter(|p| {
            p.extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("utoc"))
        })
        .filter(|p| {
            !p.file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("global.utoc"))
        })
        .collect();
    utocs.sort();

    let archives: Vec<IoStoreArchive> = utocs
        .iter()
        .filter_map(|u| IoStoreArchive::open(u).ok())
        .collect();

    let mut by_pkg: HashMap<String, (usize, String)> = HashMap::new();
    for (i, a) in archives.iter().enumerate() {
        for e in a.entries() {
            let lo = e.path.to_ascii_lowercase().replace('\\', "/");
            let Some(stem) = lo
                .strip_suffix(".uasset")
                .or_else(|| lo.strip_suffix(".umap"))
            else {
                continue;
            };
            let Some((prefix, rest)) = stem.split_once("/content/") else {
                continue;
            };
            let mount = match prefix.rsplit('/').next().unwrap_or("") {
                "meteorite" => "game",
                m => m,
            };
            by_pkg
                .entry(format!("/{mount}/{rest}"))
                .or_insert((i, e.path.clone()));
        }
    }
    let world = World {
        archives: &archives,
        by_pkg: &by_pkg,
        by_hash: &by_hash,
        usmap: &usmap,
    };

    // stopping class -> (exports stopped, bytes left unmodeled)
    let mut stops: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let (mut walked, mut complete) = (0u64, 0u64);
    let mut unmodeled_total = 0u64;
    // A declining arm is not the only way a tail goes unread. An arm that
    // returns "kept going" while leaving bytes behind reports no stop at all,
    // and the walk looks complete: `UMaterial` left four bytes unconsumed on
    // every one of 1,397 exports and this census called them all fully modeled.
    // Bytes consumed is the claim worth making, so it is measured separately.
    let mut short_by_class: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    // The bytes themselves, for the first example of each class -- a count says
    // an arm is short, the bytes say what it is short *of*.
    let mut short_sample: BTreeMap<String, String> = BTreeMap::new();
    let (mut to_the_end, mut leftover_total) = (0u64, 0u64);

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
            let Ok(payloads) = read_payloads(&h, &b) else {
                continue;
            };
            let names = h.name_map.copy_raw_names();
            // The bulk-data map is not optional context: `BodySetup` and friends
            // resolve their cooked payloads through it, so omitting it makes
            // them fail every time and attributes their whole tail to a "stop"
            // that is really a missing argument.
            let bulk: Vec<(i64, i64)> = h
                .bulk_data
                .iter()
                .map(|b| (b.serial_offset, b.serial_size))
                .collect();
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else {
                    continue;
                };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let layouts = RefCell::new(HashMap::new());
                let resolver = PkgResolver {
                    world: &world,
                    layouts: &layouts,
                    header: &h,
                    bytes: &b,
                    names: &names,
                };
                let ctx = ExportContext {
                    bulk_data: &bulk,
                    resolver: Some(&resolver),
                };
                let Ok(walk) =
                    walk_export(&payloads[i], &names, &usmap, short, ex.object_flags, &ctx)
                else {
                    continue;
                };
                walked += 1;
                let leftover = payloads[i].len().saturating_sub(walk.consumed);
                if leftover == 0 {
                    to_the_end += 1;
                } else if walk.stopped.is_none() {
                    // Only interesting when nothing declined -- a stop already
                    // accounts for its own remainder.
                    let e = short_by_class.entry(short.to_string()).or_default();
                    e.0 += 1;
                    e.1 += leftover as u64;
                    leftover_total += leftover as u64;
                    short_sample.entry(short.to_string()).or_insert_with(|| {
                        format!(
                            "{} left {leftover} of {} bytes; unread tail {:02x?}",
                            h.package_name(),
                            payloads[i].len(),
                            &payloads[i][walk.consumed..],
                        )
                    });
                }
                match walk.stopped {
                    None => complete += 1,
                    Some(stop) => {
                        let e = stops.entry(stop.class).or_default();
                        e.0 += 1;
                        e.1 += stop.remaining as u64;
                        unmodeled_total += stop.remaining as u64;
                    }
                }
            }
        }
    }

    println!("exports walked        {walked}");
    println!(
        "chain fully modeled   {complete} ({:.2}%)",
        100.0 * complete as f64 / walked.max(1) as f64
    );
    println!("stopped early         {}", walked - complete);
    println!(
        "bytes behind a stop   {unmodeled_total} ({:.2} GiB)",
        unmodeled_total as f64 / (1u64 << 30) as f64
    );
    println!();
    println!(
        "consumed to the end   {to_the_end} ({:.4}%)   <- the stronger claim",
        100.0 * to_the_end as f64 / walked.max(1) as f64
    );
    let short: u64 = short_by_class.values().map(|(n, _)| n).sum();
    println!(
        "silently short        {short} ({leftover_total} bytes across {} classes)",
        short_by_class.len()
    );

    if !short_by_class.is_empty() {
        println!("\nwalked without declining but left bytes behind:");
        let mut v: Vec<_> = short_by_class.iter().collect();
        v.sort_by_key(|(_, (_, b))| std::cmp::Reverse(*b));
        for (c, (n, b)) in v.iter().take(25) {
            println!("  {c:<44} {n:>10} exports {b:>12} bytes");
            if let Some(s) = short_sample.get(*c) {
                println!("      {s}");
            }
        }
    }

    let mut rows: Vec<_> = stops.iter().collect();
    rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    println!(
        "\n{:<40} {:>12} {:>16}",
        "stopping class", "exports", "unmodeled bytes"
    );
    for (class, (n, bytes)) in rows.iter().take(25) {
        println!("{class:<40} {n:>12} {bytes:>16}");
    }
    println!("\n{} distinct stopping classes", stops.len());
}
