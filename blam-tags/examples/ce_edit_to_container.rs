//! End to end: edit a property in a real Campaign Evolved package, write a real
//! mod container, then reopen that container and read the change back.
//!
//! `ce_edit_roundtrip` proves an edit survives a package rebuild *in memory*.
//! This is the last link: the rebuilt package has to go into a `.utoc`/`.ucas`/
//! `.pak` triplet the game will actually mount, and come back out of it intact.
//! Everything between the two — chunk ids, compression, the container's package
//! store entry, the `.utoc` index — is machinery no earlier gate touches.
//!
//! It writes into a temporary directory and reads it back; it never modifies the
//! game install. What it cannot prove is that the game *loads* it — that is a
//! human running the build. It proves everything up to that point.
//!
//! Run: `ce_edit_to_container [package-path] [usmap-path]`
use std::collections::HashMap;
use std::io::Cursor;

use blam_tags::iostore::container::writer::{write_package_mod_container, PackageOverride};
use blam_tags::iostore::container_header::EIoContainerHeaderVersion;
use blam_tags::iostore::object::unversioned::{read_export, write_export, PropValue};
use blam_tags::iostore::package::builder::{read_payloads, write_package};
use blam_tags::iostore::script_objects::ScriptObjects;
use blam_tags::iostore::ue_types::EIoStoreTocVersion;
use blam_tags::iostore::usmap::Usmap;
use blam_tags::iostore::zen::FZenPackageHeader;
use blam_tags::iostore::IoStoreArchive;

const PAKS: &str = "/Users/camden/Halo/halo-campaign-evolved_pc/Meteorite/Content/Paks";
const CV: EIoStoreTocVersion = EIoStoreTocVersion::ReplaceIoChunkHashWithIoHash;
const HV: EIoContainerHeaderVersion = EIoContainerHeaderVersion::SoftPackageReferences;

const NEW_STRING: &str = "BlamEditToContainer_ProbeValue";

fn main() {
    let want_package = std::env::args().nth(1);
    let usmap_path = std::env::args().nth(2).unwrap_or_else(|| {
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

    // Find a package with an editable string property.
    let mut chosen: Option<(IoStoreArchive, String, usize, String, String)> = None;
    'outer: for u in &utocs {
        let Ok(a) = IoStoreArchive::open(u) else { continue };
        let paths: Vec<String> = a
            .entries()
            .iter()
            .map(|e| e.path.clone())
            .filter(|p| p.to_ascii_lowercase().ends_with(".uasset"))
            .collect();
        for path in paths {
            if let Some(want) = &want_package {
                if !path.contains(want.as_str()) {
                    continue;
                }
            }
            let Ok(b) = a.read(&path) else { continue };
            let Ok(h) = FZenPackageHeader::deserialize(&mut Cursor::new(&b), None, CV, HV, None)
            else {
                continue;
            };
            // Prefer a package with neighbours, so the check that they survive
            // an export changing size is not vacuous.
            if want_package.is_none() && h.export_map.len() < 4 {
                continue;
            }
            let Ok(payloads) = read_payloads(&h, &b) else { continue };
            let names = h.name_map.copy_raw_names();
            for i in 0..h.export_map.len() {
                let ex = &h.export_map[i];
                let Some(class) = by_hash.get(&ex.class_index.raw_index()) else { continue };
                let short = class.rsplit('.').next().unwrap_or(class).to_string();
                if usmap.flattened_properties(&short).is_none() {
                    continue;
                }
                let Ok(parts) = read_export(&payloads[i], &names, &usmap, &short, ex.object_flags)
                else {
                    continue;
                };
                let Some(block) = parts.properties() else { continue };
                if let Some(entry) = block
                    .entries
                    .iter()
                    .find(|e| matches!(&e.value, PropValue::Str(s) if s != NEW_STRING))
                {
                    chosen = Some((a, path.clone(), i, short, entry.name.to_string()));
                    break 'outer;
                }
            }
        }
    }

    let Some((archive, path, idx, class, prop)) = chosen else {
        eprintln!("no package with an editable string property found");
        std::process::exit(2);
    };

    let original = archive.read(&path).expect("read package");
    let header = FZenPackageHeader::deserialize(&mut Cursor::new(&original), None, CV, HV, None)
        .expect("parse header");
    let mut payloads = read_payloads(&header, &original).expect("payloads");
    let names = header.name_map.copy_raw_names();
    let was = {
        let parts = read_export(
            &payloads[idx],
            &names,
            &usmap,
            &class,
            header.export_map[idx].object_flags,
        )
        .expect("decode");
        parts
            .properties()
            .and_then(|b| b.get(&prop))
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    };

    println!("package  {path}");
    println!("export   {idx} ({class})");
    println!("property {prop}");
    println!("  was    {was:?}");
    println!("  now    {NEW_STRING:?}");

    // Edit, rebuild.
    let mut parts = read_export(
        &payloads[idx],
        &names,
        &usmap,
        &class,
        header.export_map[idx].object_flags,
    )
    .expect("decode");
    let block = parts.properties_mut().expect("block");
    block
        .entries
        .iter_mut()
        .find(|e| &*e.name == prop.as_str())
        .expect("property")
        .value = PropValue::Str(NEW_STRING.into());
    let before_len = payloads[idx].len();
    payloads[idx] = write_export(&class, &parts, &usmap).expect("re-encode export");
    println!(
        "\nexport size {before_len} -> {} ({:+})",
        payloads[idx].len(),
        payloads[idx].len() as i64 - before_len as i64
    );

    let (rebuilt, store) = write_package(&header, &payloads, HV).expect("rebuild package");
    println!("package size {} -> {}", original.len(), rebuilt.len());

    // Write a real mod container triplet.
    let dir = std::env::temp_dir().join("blam_edit_to_container");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    // `_P` is what makes the game treat it as a higher-priority overlay.
    let out = dir.join("zzz_blam_edit_P.utoc");
    write_package_mod_container(
        &[PackageOverride {
            archive: &archive,
            uasset_path: &path,
            bytes: rebuilt.clone(),
            store,
        }],
        &out,
    )
    .expect("write mod container");

    let triplet: Vec<String> = std::fs::read_dir(&dir)
        .expect("list")
        .filter_map(|e| e.ok())
        .map(|e| {
            format!(
                "{} ({} bytes)",
                e.file_name().to_string_lossy(),
                e.metadata().map(|m| m.len()).unwrap_or(0)
            )
        })
        .collect();
    println!("\nwrote {}", dir.display());
    for t in &triplet {
        println!("  {t}");
    }

    // Reopen the container we just wrote and read the change back out of it.
    //
    // By chunk *id*, not by path: an override container carries no directory
    // index — that is deliberate, since it overrides chunks the base container
    // already names — so a path lookup finds nothing. The id is the identity
    // that matters, and it is the same one the base container uses.
    let mod_archive = IoStoreArchive::open(&out).expect("reopen mod container");
    let chunk_id = archive.chunk_id_for(&path).expect("chunk id");
    let index = mod_archive
        .find_chunk(&chunk_id)
        .expect("the override container should contain the chunk it overrides");
    let from_container = mod_archive.read_chunk(index).expect("read package from mod container");
    if from_container != rebuilt {
        eprintln!("\nFAIL: bytes read back from the container differ from what was written");
        std::process::exit(1);
    }
    let h2 = FZenPackageHeader::deserialize(&mut Cursor::new(&from_container), None, CV, HV, None)
        .expect("parse rebuilt header");
    let payloads2 = read_payloads(&h2, &from_container).expect("payloads");
    let names2 = h2.name_map.copy_raw_names();
    let parts2 = read_export(
        &payloads2[idx],
        &names2,
        &usmap,
        &class,
        h2.export_map[idx].object_flags,
    )
    .expect("decode from container");
    let got = parts2
        .properties()
        .and_then(|b| b.get(&prop))
        .and_then(|v| v.as_str().map(str::to_string));

    if got.as_deref() != Some(NEW_STRING) {
        eprintln!("\nFAIL: expected {NEW_STRING:?} from the container, got {got:?}");
        std::process::exit(1);
    }

    // And nothing else moved.
    let mut neighbours = 0;
    for i in 0..payloads.len() {
        if i != idx {
            assert_eq!(payloads2[i], payloads[i], "export {i} changed");
            neighbours += 1;
        }
    }

    println!("\nreopened the container and read back {:?}", got.unwrap());
    println!(
        "{neighbours} other exports byte-identical ({} of them moved)",
        payloads.len().saturating_sub(idx + 1)
    );
    println!("\nOK — edit survived package rebuild, container write, and container read.");
}
