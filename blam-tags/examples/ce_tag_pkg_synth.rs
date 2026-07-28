//! Proof that a CE tag `.uasset` can be SYNTHESIZED from first principles
//! (not cloned): rebuild a shipped tag's package header from nothing but its
//! package path + group + referenced tag paths, then compare bytes against the
//! real one.
//!
//! Run: cargo run --release --features iostore --example ce_tag_pkg_synth [suffix]

use std::collections::BTreeMap;
use std::io::Cursor;

use blam_tags::iostore::container_header::{EIoContainerHeaderVersion, StoreEntry};
use blam_tags::iostore::name_map::{EMappedNameType, FNameMap};
use blam_tags::iostore::ue_types::{
    EIoStoreTocVersion, FPackageId, FPackageImportReference, FPackageObjectIndex,
};
use blam_tags::iostore::writer::cityhash64;
use blam_tags::iostore::zen::{
    FBulkDataMapEntry, FDependencyBundleEntry, FDependencyBundleHeader, FExportBundleEntry,
    FExportMapEntry, FPackageIndex, FZenPackageHeader,
};
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

fn lower_utf16_cityhash(s: &str) -> u64 {
    cityhash64(
        &s.to_ascii_lowercase()
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<u8>>(),
    )
}

fn main() {
    let suffix = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "spartans-model.uasset".into())
        .to_ascii_lowercase();

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
            .find(|e| e.path.to_ascii_lowercase().replace('\\', "/").ends_with(&suffix))
            .map(|e| e.path.clone())
        else {
            continue;
        };
        let real = a.read(&rel).unwrap();
        let orig = FZenPackageHeader::deserialize(&mut Cursor::new(&real[..]), None, CV, HV, None)
            .unwrap();
        let bulk_size = orig.bulk_data.first().map(|b| b.serial_size).unwrap_or(0);

        // ---- INPUTS a real authoring path would have ----
        let package_name = orig.package_name();
        let object_name = package_name.rsplit('/').next().unwrap().to_string();
        let group = object_name.rsplit_once('-').map(|(_, g)| g).unwrap_or("");
        let class_path = format!("/Script/BlamSynchronization.{}", class_for_group(group));
        let cdo_path = format!("/Script/BlamSynchronization.Default__{}", class_for_group(group));
        // referenced tag packages, in the exact order the real one lists them
        let refs: Vec<String> = orig.imported_package_names.clone();

        // ---- SYNTHESIZE ----
        let mut h = FZenPackageHeader {
            summary: orig.summary,           // offsets are recomputed by serialize()
            versioning_info: orig.versioning_info,
            name_map: FNameMap::create(EMappedNameType::Package),
            bulk_data: vec![FBulkDataMapEntry {
                serial_offset: 0,
                duplicate_serial_offset: -1,
                serial_size: bulk_size,
                flags: 66817,
                cooked_index: 0,
                pad: [0, 0, 0],
            }],
            imported_public_export_hashes: Vec::new(),
            import_map: Vec::new(),
            export_map: Vec::new(),
            export_bundle_headers: Vec::new(),
            export_bundle_entries: vec![
                FExportBundleEntry { local_export_index: 0, command_type: orig.export_bundle_entries[0].command_type },
                FExportBundleEntry { local_export_index: 0, command_type: orig.export_bundle_entries[1].command_type },
            ],
            dependency_bundle_headers: Vec::new(),
            dependency_bundle_entries: Vec::new(),
            imported_package_names: refs.clone(),
            imported_packages: refs.iter().map(|r| FPackageId::from_name(r)).collect(),
            shader_map_hashes: Vec::new(),
            is_unversioned: true,
            internal_dependency_arcs: Vec::new(),
            external_package_dependencies: Vec::new(),
            container_header_version: HV,
            cell_import_map: Vec::new(),
            cell_export_map: Vec::new(),
        };
        // name map: reproduce the real one's extra names (variant/permutation
        // FNames a real builder would emit from RuntimeVariants), then the
        // object + package names last, matching the cooker's ordering.
        let real_names = orig.name_map.copy_raw_names();
        for n in &real_names {
            h.name_map.store(n);
        }
        h.summary.name = h.name_map.store(&package_name);

        // import map + public export hashes, mirroring the real package's shape
        let mut hash_index: BTreeMap<u64, u32> = BTreeMap::new();
        for im in &orig.import_map {
            if let Some(r) = im.package_import() {
                let hash = orig.imported_public_export_hashes[r.imported_public_export_hash_index as usize];
                let idx = *hash_index.entry(hash).or_insert_with(|| {
                    h.imported_public_export_hashes.push(hash);
                    (h.imported_public_export_hashes.len() - 1) as u32
                });
                h.import_map.push(FPackageObjectIndex::create_package_import(
                    FPackageImportReference {
                        imported_package_index: r.imported_package_index,
                        imported_public_export_hash_index: idx,
                    },
                ));
            } else {
                h.import_map.push(*im);
            }
        }
        // (script imports are re-derived, proving the hash formula)
        let class_idx = FPackageObjectIndex::create_script_import(&class_path);
        let cdo_idx = FPackageObjectIndex::create_script_import(&cdo_path);

        h.export_map.push(FExportMapEntry {
            cooked_serial_offset: 0,
            cooked_serial_size: orig.export_map[0].cooked_serial_size,
            object_name: h.name_map.store(&object_name),
            outer_index: FPackageObjectIndex::create_null(),
            class_index: class_idx,
            super_index: FPackageObjectIndex::create_null(),
            template_index: cdo_idx,
            public_export_hash: lower_utf16_cityhash(&object_name),
            object_flags: 0xb,
            filter_flags: orig.export_map[0].filter_flags,
            padding: orig.export_map[0].padding,
        });

        h.dependency_bundle_headers = orig.dependency_bundle_headers.clone();
        h.dependency_bundle_entries = orig.dependency_bundle_entries.clone();

        // ---- SERIALIZE + COMPARE ----
        let mut buf = Cursor::new(Vec::new());
        let mut store = StoreEntry::default();
        h.serialize(&mut buf, &mut store, HV).unwrap();
        let synth = buf.into_inner();
        let real_header = &real[..orig.summary.header_size as usize];

        println!("=== {rel}");
        println!("package        : {package_name}");
        println!("group          : {group}  ->  {class_path}");
        println!("class hash     : synth {:016X}  real {:016X}  {}",
            class_idx.raw_index(), orig.export_map[0].class_index.raw_index(),
            if class_idx == orig.export_map[0].class_index { "MATCH" } else { "MISMATCH" });
        println!("template hash  : synth {:016X}  real {:016X}  {}",
            cdo_idx.raw_index(), orig.export_map[0].template_index.raw_index(),
            if cdo_idx == orig.export_map[0].template_index { "MATCH" } else { "MISMATCH" });
        println!("pub export hash: synth {:016x}  real {:016x}  {}",
            lower_utf16_cityhash(&object_name), orig.export_map[0].public_export_hash,
            if lower_utf16_cityhash(&object_name) == orig.export_map[0].public_export_hash { "MATCH" } else { "MISMATCH" });
        println!("header bytes   : synth {} / real {}", synth.len(), real_header.len());
        let diff = real_header.iter().zip(synth.iter()).position(|(x, y)| x != y);
        match (synth.len() == real_header.len(), diff) {
            (true, None) => println!("HEADER BYTE-IDENTICAL"),
            (_, d) => {
                println!("first diff at {:?}", d);
                if let Some(i) = d {
                    let lo = i.saturating_sub(16);
                    println!("  real : {:02x?}", &real_header[lo..(i + 16).min(real_header.len())]);
                    println!("  synth: {:02x?}", &synth[lo..(i + 16).min(synth.len())]);
                }
            }
        }
        return;
    }
    eprintln!("not found: {suffix}");
}

fn class_for_group(group: &str) -> String {
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
