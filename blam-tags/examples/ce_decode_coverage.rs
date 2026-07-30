//! Gate: what share of an export's bytes sit behind a *typed model* rather than
//! a byte blob?
//!
//! This is the number Level 2 moves. `ce_export_roundtrip` says the bytes come
//! back; it says nothing about whether anything understood them, and a codec
//! that round-trips 4.77 GiB of `Vec<u8>` scores 100% on it while modeling
//! nothing.
//!
//! Four populations are untyped today, and they are not all in the plan:
//!
//!  * **`Export.tail`** — the class tails, retained spans.
//!  * **`BlockLayout::Native`** — hand-written structs, decoded into fields but
//!    written from their span.
//!  * **`PropValue::Raw`** — values the reader declines to interpret.
//!  * **`NativeStruct::Opaque`** — a fixed-size native struct whose size is
//!    known but whose fields are not modeled yet. The rest of that population
//!    is typed as of work item A2, which is what took this gate off its
//!    starting number.
//!
//! Run: `ce_decode_coverage [usmap-path]`
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{
    read_export, read_userdefined_struct_layout, roundtrip_tail, ExportContext, PackageResolver,
    PropValue, PropertyBlock, TailContext,
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

#[derive(Default)]
struct Untyped {
    tail: u64,
    native_struct_span: u64,
    fixed_native: u64,
    /// Payloads inside an otherwise-typed hand-written struct — currently only
    /// `FInstancedPropertyBag`, whose values are laid out by the bag's own
    /// descriptors and which nothing in the corpus ships enough of to model.
    hand_written: u64,
    raw: u64,
}

fn walk(v: &PropValue, u: &mut Untyped, by_struct: &mut BTreeMap<String, u64>, depth: usize) {
    if depth > 24 {
        return;
    }
    match v {
        // Typed now (work item A2) — only an unmodeled `Opaque` still counts.
        PropValue::Native(n) => u.fixed_native += n.untyped_bytes() as u64,
        // Typed as of work item A; nothing left untyped inside one.
        PropValue::HandWritten(h) => u.hand_written += h.untyped_bytes() as u64,
        PropValue::Raw(b) => u.raw += b.len() as u64,
        PropValue::Struct(block) => {
            for (_, inner) in block.iter() {
                walk(inner, u, by_struct, depth + 1);
            }
        }
        PropValue::Array(items) => items.iter().for_each(|x| walk(x, u, by_struct, depth + 1)),
        PropValue::Map(m) => m.iter().for_each(|(k, val)| {
            walk(k, u, by_struct, depth + 1);
            walk(val, u, by_struct, depth + 1);
        }),
        PropValue::WithRemovals { removals, inner } => {
            if let Some(r) = removals {
                r.iter().for_each(|x| walk(x, u, by_struct, depth + 1));
            }
            walk(inner, u, by_struct, depth + 1);
        }
        _ => {}
    }
}

fn walk_block(b: &PropertyBlock, u: &mut Untyped, by_struct: &mut BTreeMap<String, u64>) {
    for (_, v) in b.iter() {
        walk(v, u, by_struct, 0);
    }
}

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

    let mut u = Untyped::default();
    let mut by_struct: BTreeMap<String, u64> = BTreeMap::new();
    let mut tail_by_class: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_bytes = 0u64;
    let mut modeled_tail = 0u64;

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
            for (i, ex) in h.export_map.iter().enumerate() {
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class);
                if usmap.flattened_properties(short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                total_bytes += payloads[i].len() as u64;
                // A tail with a model is *not* a blob. This gate predates
                // `tail_models` and counted every tail as one, which reported
                // 12.72% typed while 4.77 GiB of it had been converted.
                if !parts.tail.is_empty() {
                    let empty = Default::default();
                    let block = parts.block.as_ref().unwrap_or(&empty);
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
                    let ctx = TailContext {
                        bulk_data: &bulk,
                        origin: payloads[i].len() - parts.tail.len(),
                        usmap: &usmap,
                        resolver: Some(&resolver),
                    };
                    match roundtrip_tail(short, &parts.tail, &names, block, ctx) {
                        Some(Ok(_)) => modeled_tail += parts.tail.len() as u64,
                        // Either no model, or a model that needs context this
                        // gate does not supply — reported, not hidden.
                        _ => {
                            u.tail += parts.tail.len() as u64;
                            *tail_by_class.entry(short.to_string()).or_default() +=
                                parts.tail.len() as u64;
                        }
                    }
                }
                if let Some(block) = parts.block.as_ref() {
                    walk_block(block, &mut u, &mut by_struct);
                }
            }
        }
    }

    let untyped = u.tail + u.native_struct_span + u.fixed_native + u.hand_written + u.raw;
    let typed = total_bytes.saturating_sub(untyped);

    println!("export bytes total     {total_bytes:>14}");
    println!(
        "behind a typed model   {typed:>14}  ({:.4}%)   <- the number Level 2 moves",
        100.0 * typed as f64 / total_bytes.max(1) as f64
    );
    println!("still a byte blob      {untyped:>14}  ({:.4}%)", 100.0 * untyped as f64 / total_bytes.max(1) as f64);
    println!();
    println!("  class tails          {:>14}  ({} classes with no model here)", u.tail, tail_by_class.len());
    println!();
    println!("  modeled tail bytes   {modeled_tail:>14}");
    println!(
        "  NOTE: bytes *inside* a modeled tail that are still `Vec<u8>` — Nanite pages, Chaos"
    );
    println!(
        "        geometry, shader bytecode, block-compressed mips, `TArray<uint8>` — are counted"
    );
    println!("        as modeled here. They are leaf data with no interior UE exposes either.");
    println!("  hand-written structs {:>14}  ({} structs)", u.native_struct_span, by_struct.len());
    println!("  unmodeled natives    {:>14}  (NativeStruct::Opaque)", u.fixed_native);
    println!("  property-bag payload {:>14}  (laid out by its own descriptors)", u.hand_written);
    println!("  unmodeled (Raw)      {:>14}", u.raw);
    if !tail_by_class.is_empty() {
        println!("\nclasses whose tail this gate could not model:");
        let mut v: Vec<_> = tail_by_class.iter().collect();
        v.sort_by_key(|(_, b)| std::cmp::Reverse(**b));
        for (c, b) in v {
            println!("  {b:>12}  {c}");
        }
    }
}
