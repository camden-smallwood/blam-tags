//! Full anatomy of a CE tag `.uasset`: every header section, with each import
//! resolved to `<imported package name>#<public export hash>` and the export
//! properties decoded against the usmap.
//!
//! Run: cargo run --release --features iostore --example ce_tag_pkg_anatomy -- <path-substring> [ClassName]

use std::io::Cursor;

use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::ue_types::{EIoStoreTocVersion, FPackageObjectIndex};
use blam_tags::iostore::unversioned::read_export_struct;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const USMAP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/assets/meteorite-5.5.4.usmap");
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn main() {
    let suffix = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "spartans-biped.uasset".into())
        .to_ascii_lowercase();
    let class_hint = std::env::args().nth(2);
    let usmap = Usmap::parse(&std::fs::read(USMAP).expect("usmap")).expect("parse usmap");

    let mut utocs: Vec<_> = std::fs::read_dir(PAKS)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("utoc")))
        .filter(|p| !p.file_name().is_some_and(|n| n.eq_ignore_ascii_case("global.utoc")))
        .collect();
    utocs.sort();

    for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let Some(rel) = a
            .entries()
            .iter()
            .find(|e| {
                e.path
                    .to_ascii_lowercase()
                    .replace('\\', "/")
                    .ends_with(&suffix)
            })
            .map(|e| e.path.clone())
        else {
            continue;
        };
        let ua = a.read(&rel).unwrap();
        let h = FZenPackageHeader::deserialize(&mut Cursor::new(&ua[..]), None, CV, HV, None).unwrap();
        println!("=== {rel}  ({} bytes in {})", ua.len(), u.display());
        println!("package name      : {}", h.package_name());
        println!("header_size       : {}", h.summary.header_size);
        println!("package_flags     : 0x{:x}", h.summary.package_flags);
        println!("is_unversioned    : {}", h.is_unversioned);
        println!("cooked_header_size: {}", h.summary.cooked_header_size);

        println!("\n-- name map ({}) --", h.name_map.copy_raw_names().len());
        for (i, n) in h.name_map.copy_raw_names().iter().enumerate() {
            println!("    [{i}] {n}");
        }

        println!("\n-- bulk data map ({}) --", h.bulk_data.len());
        for (i, b) in h.bulk_data.iter().enumerate() {
            println!("    [{i}] {b:?}");
        }

        println!(
            "\n-- imported packages ({} ids / {} names) --",
            h.imported_packages.len(),
            h.imported_package_names.len()
        );
        for (i, n) in h.imported_package_names.iter().enumerate() {
            let id = h.imported_packages.get(i).map(|p| p.0).unwrap_or(0);
            println!("    [{i:2}] {id:016x}  {n}");
        }

        println!(
            "\n-- imported public export hashes ({}) --",
            h.imported_public_export_hashes.len()
        );
        for (i, x) in h.imported_public_export_hashes.iter().enumerate() {
            println!("    [{i:2}] {x:016x}");
        }

        println!("\n-- import map ({}) --", h.import_map.len());
        for (i, im) in h.import_map.iter().enumerate() {
            let pi = -(i as i32) - 1;
            if let Some(r) = im.package_import() {
                let pkg = h
                    .imported_package_names
                    .get(r.imported_package_index as usize)
                    .cloned()
                    .unwrap_or_else(|| format!("<pkg {}>", r.imported_package_index));
                let hash = h
                    .imported_public_export_hashes
                    .get(r.imported_public_export_hash_index as usize)
                    .copied()
                    .unwrap_or(0);
                println!("    [{i:2}] (Object({pi:3})) {pkg}#{hash:016x}");
            } else {
                println!("    [{i:2}] (Object({pi:3})) {im:?}");
            }
        }

        println!("\n-- export map ({}) --", h.export_map.len());
        for (i, e) in h.export_map.iter().enumerate() {
            println!("    export[{i}] name={:?}", h.name_map.get(e.object_name).to_string());
            println!("        class    = {:?}", e.class_index);
            println!("        outer    = {:?}", e.outer_index);
            println!("        super    = {:?}", e.super_index);
            println!("        template = {:?}", e.template_index);
            println!("        flags    = 0x{:x}", e.object_flags);
            println!("        pub_hash = 0x{:016x}", e.public_export_hash);
            println!("        cooked_serial_offset = {}", e.cooked_serial_offset);
            println!("        cooked_serial_size   = {}", e.cooked_serial_size);
            println!("        filter_flags = {:?}", e.filter_flags);
        }

        println!("\n-- export bundle entries ({}) --", h.export_bundle_entries.len());
        for (i, b) in h.export_bundle_entries.iter().enumerate() {
            println!("    [{i}] {b:?}");
        }
        println!("\n-- dependency bundle headers ({}) --", h.dependency_bundle_headers.len());
        for (i, b) in h.dependency_bundle_headers.iter().enumerate() {
            println!("    [{i}] {b:?}");
        }
        println!("\n-- dependency bundle entries ({}) --", h.dependency_bundle_entries.len());
        for (i, b) in h.dependency_bundle_entries.iter().enumerate() {
            println!("    [{i}] {b:?}");
        }
        println!("\n-- shader map hashes ({}) --", h.shader_map_hashes.len());
        println!("-- cell import map ({}) / cell export map ({}) --",
            h.cell_import_map.len(), h.cell_export_map.len());

        let names = h.name_map.copy_raw_names();
        let start = h.summary.header_size as usize;
        for (i, e) in h.export_map.iter().enumerate() {
            let off = start + e.cooked_serial_offset as usize;
            let end = (off + e.cooked_serial_size as usize).min(ua.len());
            if off >= ua.len() {
                continue;
            }
            let body = &ua[off..end];
            println!("\n-- export[{i}] body ({} bytes) --", body.len());
            println!(
                "    {}",
                body.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            );
            let class = class_hint.clone().unwrap_or_else(|| guess_class(&rel));
            match read_export_struct(body, &names, &usmap, &class) {
                Ok(p) if p.is_empty() => println!("    decoded as {class}: (all defaults)"),
                Ok(p) => {
                    println!("    decoded as {class}:");
                    for (k, v) in p {
                        println!("        {k} = {v:?}");
                    }
                }
                Err(err) => println!("    decode as {class} failed: {err}"),
            }
        }

        // trailing bytes after last export = package trailer / hash
        let last_end = h
            .export_map
            .iter()
            .map(|e| start + (e.cooked_serial_offset + e.cooked_serial_size) as usize)
            .max()
            .unwrap_or(start);
        if last_end < ua.len() {
            println!(
                "\n-- trailer ({} bytes) --\n    {}",
                ua.len() - last_end,
                ua[last_end..].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
            );
        }
        return;
    }
    eprintln!("not found: {suffix}");
}

/// `foo-biped.uasset` -> `BlamBipedTagDataAsset`
fn guess_class(rel: &str) -> String {
    let stem = rel.rsplit('/').next().unwrap_or(rel);
    let stem = stem.strip_suffix(".uasset").unwrap_or(stem);
    let group = stem.rsplit('-').next().unwrap_or(stem);
    let mut out = String::from("Blam");
    for part in group.split('_') {
        let mut c = part.chars();
        if let Some(f) = c.next() {
            out.push(f.to_ascii_uppercase());
            out.push_str(c.as_str());
        }
    }
    out.push_str("TagDataAsset");
    out
}
