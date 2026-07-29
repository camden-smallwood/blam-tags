//! What is actually inside an `UInstancedStaticMeshComponent` tail?
//!
//! The tail is 435 MiB across 121,013 exports — 8.91% of all retained tail
//! bytes, and the largest population that is not behind a compression codec. Its
//! walker reads two flags and up to four bulk arrays, but a *skipper* never has
//! to know what an element is, so nothing yet records the element sizes a typed
//! model has to commit to.
//!
//! Censuses the discriminator before the dispatch gets built: how the two flags
//! combine, and the element size of every bulk array under each.
//!
//! Run: `ce_ismc_probe [usmap-path]`
use std::collections::{BTreeMap, HashMap};
use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::read_export;
use blam_tags::iostore::package::builder::read_payloads;
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

/// The classes whose chain ends in `UInstancedStaticMeshComponent`.
const CLASSES: &[&str] = &[
    "InstancedStaticMeshComponent",
    "FoliageInstancedStaticMeshComponent",
    "HLODInstancedStaticMeshComponent",
];

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

    // (cooked, has_skip_serialization_data, cooked-render-data present) -> count
    let mut shapes: BTreeMap<(bool, bool, bool), u64> = BTreeMap::new();
    // (which array, element size) -> (arrays, total elements)
    let mut elems: BTreeMap<(&str, i32), (u64, u64)> = BTreeMap::new();
    let mut total = 0u64;
    let mut short_tail = 0u64;

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
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
                if !CLASSES.contains(&short) {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, short, ex.object_flags)
                else {
                    continue;
                };
                total += 1;
                // The ISMC part is the *end* of the tail; everything before it
                // belongs to the base classes. Walk from the back instead of
                // re-deriving the chain: the last 8 bytes before the arrays are
                // the two flags, so scan forward for a consistent parse.
                let t = &parts.tail;
                let Some(off) = ismc_start(t) else {
                    short_tail += 1;
                    continue;
                };
                let mut p = off;
                let cooked = rd_u32(t, &mut p) != 0;
                let has_skip = rd_u32(t, &mut p) != 0;
                let mut render = false;
                if has_skip {
                    for what in ["PerInstanceSMData", "PerInstanceSMCustomData"] {
                        let (sz, n) = rd_bulk(t, &mut p);
                        let e = elems.entry((what, sz)).or_default();
                        e.0 += 1;
                        e.1 += n as u64;
                    }
                }
                if cooked {
                    render = rd_u32(t, &mut p) != 0;
                    if render {
                        for what in ["render instance", "render custom"] {
                            let (sz, n) = rd_bulk(t, &mut p);
                            let e = elems.entry((what, sz)).or_default();
                            e.0 += 1;
                            e.1 += n as u64;
                        }
                    }
                }
                *shapes.entry((cooked, has_skip, render)).or_default() += 1;
            }
        }
    }

    println!("ISMC-family exports    {total}");
    println!("tail too short to scan {short_tail}");
    println!("\n{:<8} {:<10} {:<12} {:>12}", "cooked", "hasSkip", "renderData", "exports");
    for ((c, s, r), n) in &shapes {
        println!("{c:<8} {s:<10} {r:<12} {n:>12}");
    }
    println!("\n{:<22} {:>10} {:>12} {:>16}", "array", "elem size", "arrays", "elements");
    for ((what, sz), (arrays, n)) in &elems {
        println!("{what:<22} {sz:>10} {arrays:>12} {n:>16}");
    }
}

/// The ISMC tail sits at the end; find where its two flags start by requiring
/// the whole remainder to parse consistently from there.
fn ismc_start(t: &[u8]) -> Option<usize> {
    (0..t.len().saturating_sub(7)).find(|&off| parses_to_end(t, off))
}

fn parses_to_end(t: &[u8], off: usize) -> bool {
    let mut p = off;
    if p + 8 > t.len() {
        return false;
    }
    let cooked = rd_u32(t, &mut p) != 0;
    let has_skip = rd_u32(t, &mut p) != 0;
    if has_skip {
        for _ in 0..2 {
            if !skip_bulk(t, &mut p) {
                return false;
            }
        }
    }
    if cooked {
        if p + 4 > t.len() {
            return false;
        }
        if rd_u32(t, &mut p) != 0 {
            for _ in 0..2 {
                if !skip_bulk(t, &mut p) {
                    return false;
                }
            }
        }
    }
    p == t.len()
}

fn rd_u32(t: &[u8], p: &mut usize) -> u32 {
    let v = u32::from_le_bytes(t[*p..*p + 4].try_into().unwrap());
    *p += 4;
    v
}

fn rd_bulk(t: &[u8], p: &mut usize) -> (i32, i32) {
    let sz = i32::from_le_bytes(t[*p..*p + 4].try_into().unwrap());
    let n = i32::from_le_bytes(t[*p + 4..*p + 8].try_into().unwrap());
    *p += 8 + (sz.max(0) as usize) * (n.max(0) as usize);
    (sz, n)
}

fn skip_bulk(t: &[u8], p: &mut usize) -> bool {
    if *p + 8 > t.len() {
        return false;
    }
    let sz = i32::from_le_bytes(t[*p..*p + 4].try_into().unwrap());
    let n = i32::from_le_bytes(t[*p + 4..*p + 8].try_into().unwrap());
    if !(0..=4096).contains(&sz) || n < 0 {
        return false;
    }
    let bytes = (sz as usize).checked_mul(n as usize).unwrap_or(usize::MAX);
    match p.checked_add(8 + bytes) {
        Some(q) if q <= t.len() => {
            *p = q;
            true
        }
        _ => false,
    }
}
